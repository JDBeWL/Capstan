//! 后台曲目分析实体。管理专用分析线程、三级优先级队列与磁盘缓存。
//! 结果落地后发出 [`AnalysisEvent::Ready`]，不随播放进度轮询重绘。

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, TryRecvError};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use capstan_analysis::{AnalysisCache, Cancel, TrackAnalysis, analyze_file};
use capstan_core::{FileStamp, Track, TrackId};

use crate::persist::data_dir;
use crate::platform::*;

/// 轮询分析结果的间隔。只在队列有返回时才发出事件，空转零重绘。
const POLL_INTERVAL: Duration = Duration::from_millis(100);

/// 任务优先级。数字越小越先执行，顺序为当前曲 > 接下来几首 > 后台整库扫描。
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum AnalysisPriority {
    High = 0,
    Medium = 1,
    Low = 2,
}

#[derive(Clone, Debug, PartialEq)]
pub enum AnalysisEvent {
    Ready(TrackId),
    /// 启动时从磁盘缓存批量恢复完成，附带已就绪的 `(TrackId, bpm)`。
    Hydrated(Vec<(TrackId, f32)>),
    Failed(TrackId),
}

struct Job {
    id: TrackId,
    path: PathBuf,
    stamp: FileStamp,
    bpm_tag: Option<f32>,
    priority: AnalysisPriority,
}

struct WorkerOutcome {
    id: TrackId,
    result: Option<Arc<TrackAnalysis>>,
}

struct SharedQueue {
    jobs: Mutex<VecDeque<Job>>,
    queued_ids: Mutex<HashSet<TrackId>>,
    wake: Condvar,
    shutdown: AtomicBool,
    background_enabled: AtomicBool,
    cancel_current: Cancel,
}

impl SharedQueue {
    fn new() -> Self {
        Self {
            jobs: Mutex::new(VecDeque::new()),
            queued_ids: Mutex::new(HashSet::new()),
            wake: Condvar::new(),
            shutdown: AtomicBool::new(false),
            background_enabled: AtomicBool::new(false),
            cancel_current: Cancel::new(),
        }
    }

    fn enqueue(&self, job: Job) {
        let Ok(mut jobs) = self.jobs.lock() else {
            return;
        };
        let Ok(mut ids) = self.queued_ids.lock() else {
            return;
        };
        if ids.contains(&job.id) {
            // 已在队列里时若新请求优先级更高，直接提升并重排。
            if let Some(existing) = jobs.iter_mut().find(|item| item.id == job.id)
                && job.priority < existing.priority
            {
                existing.priority = job.priority;
                let mut vec: Vec<Job> = jobs.drain(..).collect();
                vec.sort_by_key(|item| item.priority);
                jobs.extend(vec);
            }
            return;
        }
        ids.insert(job.id);
        let pos = jobs
            .iter()
            .position(|existing| job.priority < existing.priority)
            .unwrap_or(jobs.len());
        jobs.insert(pos, job);
        self.wake.notify_one();
    }

    fn pop(&self) -> Option<Job> {
        let mut jobs = self.jobs.lock().ok()?;
        loop {
            if self.shutdown.load(Ordering::Acquire) {
                return None;
            }
            if let Some(front) = jobs.front() {
                if front.priority == AnalysisPriority::Low
                    && !self.background_enabled.load(Ordering::Acquire)
                {
                    // 后台整库扫描关闭时跳过低优先级任务，但仍响应高与中优先级任务。
                    let drop_id = front.id;
                    jobs.pop_front();
                    if let Ok(mut ids) = self.queued_ids.lock() {
                        ids.remove(&drop_id);
                    }
                    continue;
                }
                let job = jobs.pop_front()?;
                if let Ok(mut ids) = self.queued_ids.lock() {
                    ids.remove(&job.id);
                }
                return Some(job);
            }
            jobs = self.wake.wait(jobs).ok()?;
        }
    }

    fn clear_low_priority(&self) {
        if let (Ok(mut jobs), Ok(mut ids)) = (self.jobs.lock(), self.queued_ids.lock()) {
            jobs.retain(|job| {
                let keep = job.priority != AnalysisPriority::Low;
                if !keep {
                    ids.remove(&job.id);
                }
                keep
            });
        }
    }
}

pub struct AnalysisEntity {
    results: HashMap<TrackId, Arc<TrackAnalysis>>,
    failed: HashSet<TrackId>,
    queue: Arc<SharedQueue>,
    worker: Option<JoinHandle<()>>,
    _pump: Task<()>,
    _hydrate: Option<Task<()>>,
}

impl EventEmitter<AnalysisEvent> for AnalysisEntity {}

impl AnalysisEntity {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let queue = Arc::new(SharedQueue::new());
        let (tx, rx) = mpsc::channel::<WorkerOutcome>();

        let cache = data_dir().map(|dir| AnalysisCache::new(dir.join("analysis")));
        let worker_queue = Arc::clone(&queue);
        let worker = thread::Builder::new()
            .name("capstan-analysis".to_owned())
            .spawn(move || run_worker(&worker_queue, cache.as_ref(), &tx))
            .ok();

        let executor = cx.background_executor().clone();
        let pump = cx.spawn(async move |this, cx| {
            loop {
                executor.timer(POLL_INTERVAL).await;
                let mut batch = Vec::new();
                let mut disconnected = false;
                loop {
                    match rx.try_recv() {
                        Ok(item) => batch.push(item),
                        Err(TryRecvError::Empty) => break,
                        Err(TryRecvError::Disconnected) => {
                            disconnected = true;
                            break;
                        }
                    }
                }
                if batch.is_empty() {
                    if disconnected {
                        break;
                    }
                    continue;
                }
                let alive = this
                    .update(cx, |this, cx| {
                        for item in batch {
                            if let Some(analysis) = item.result {
                                this.failed.remove(&item.id);
                                this.results.insert(item.id, analysis);
                                cx.emit(AnalysisEvent::Ready(item.id));
                            } else {
                                this.failed.insert(item.id);
                                cx.emit(AnalysisEvent::Failed(item.id));
                            }
                        }
                        cx.notify();
                    })
                    .is_ok();
                if !alive || disconnected {
                    break;
                }
            }
        });

        Self {
            results: HashMap::new(),
            failed: HashSet::new(),
            queue,
            worker,
            _pump: pump,
            _hydrate: None,
        }
    }

    /// 取内存缓存中的分析结果；未就绪时返回 `None`。
    pub fn get(&self, id: TrackId) -> Option<Arc<TrackAnalysis>> {
        self.results.get(&id).map(Arc::clone)
    }

    /// 是否已经持有该曲目的内存分析结果。
    pub fn is_analyzed(&self, id: TrackId) -> bool {
        self.results.contains_key(&id)
    }

    /// 取内存缓存中的调性估计结果；未分析或无调性时返回 `None`。
    pub fn key(&self, id: TrackId) -> Option<capstan_analysis::KeyInfo> {
        self.results.get(&id).and_then(|analysis| analysis.key)
    }

    /// 取内存缓存中的 Camelot 调性短码（如 `"8B"`）；未就绪时返回 `None`。
    pub fn camelot(&self, id: TrackId) -> Option<String> {
        self.key(id).map(|info| info.camelot_code())
    }

    /// 从磁盘缓存批量恢复已分析曲目，不触发任何音频解码，用于启动后即刻填充 BPM 与智能混排数据。
    #[allow(clippy::used_underscore_binding)]
    pub fn hydrate_cached(&mut self, tracks: Vec<(TrackId, FileStamp)>, cx: &mut Context<Self>) {
        let executor = cx.background_executor().clone();
        self._hydrate = Some(cx.spawn(async move |this, cx| {
            let loaded = executor
                .spawn(async move { load_cached_batch(tracks) })
                .await;
            let _ = this.update(cx, |this, cx| {
                let mut bpms = Vec::with_capacity(loaded.len());
                for (id, analysis) in loaded {
                    bpms.push((id, analysis.bpm));
                    this.failed.remove(&id);
                    this.results.entry(id).or_insert(analysis);
                }
                let changed = !bpms.is_empty();
                cx.emit(AnalysisEvent::Hydrated(bpms));
                if changed {
                    cx.notify();
                }
            });
        }));
    }

    /// 请求分析一首曲目；已有内存结果或此前失败过时跳过。
    pub fn request(&self, track: &Track, priority: AnalysisPriority) {
        if !track.is_playable()
            || self.results.contains_key(&track.id)
            || self.failed.contains(&track.id)
        {
            return;
        }
        self.queue.enqueue(Job {
            id: track.id,
            path: track.path.clone(),
            stamp: track.stamp,
            bpm_tag: track.bpm_tag,
            priority,
        });
    }

    /// 批量投递后台分析任务。
    pub fn request_batch(&self, tracks: &[Track], priority: AnalysisPriority) {
        for track in tracks {
            self.request(track, priority);
        }
    }

    /// 控制是否允许消费低优先级的整库后台扫描任务。
    pub fn set_background_enabled(&self, enabled: bool) {
        self.queue
            .background_enabled
            .store(enabled, Ordering::Release);
        if enabled {
            self.queue.wake.notify_one();
        } else {
            self.queue.clear_low_priority();
        }
    }

    pub fn analyzed_count(&self) -> usize {
        self.results.len()
    }
}

impl Drop for AnalysisEntity {
    fn drop(&mut self) {
        self.queue.shutdown.store(true, Ordering::Release);
        self.queue.cancel_current.cancel();
        self.queue.wake.notify_all();
        if let Some(handle) = self.worker.take() {
            let _ = handle.join();
        }
    }
}

fn run_worker(
    queue: &SharedQueue,
    cache: Option<&AnalysisCache>,
    tx: &mpsc::Sender<WorkerOutcome>,
) {
    while let Some(job) = queue.pop() {
        if queue.shutdown.load(Ordering::Acquire) {
            break;
        }

        if let Some(cache) = cache
            && let Some(hit) = cache.load(job.id, job.stamp)
        {
            let _ = tx.send(WorkerOutcome {
                id: job.id,
                result: Some(Arc::new(hit)),
            });
            continue;
        }

        match analyze_file(&job.path, job.bpm_tag, &queue.cancel_current) {
            Ok(analysis) => {
                if let Some(cache) = cache
                    && let Err(err) = cache.save(job.id, &analysis)
                {
                    log::warn!("保存曲目分析缓存失败：{err}");
                }
                let _ = tx.send(WorkerOutcome {
                    id: job.id,
                    result: Some(Arc::new(analysis)),
                });
            }
            Err(capstan_analysis::AnalysisError::Cancelled) => break,
            Err(err) => {
                log::debug!("分析曲目 {} 失败：{err}", job.path.display());
                let _ = tx.send(WorkerOutcome {
                    id: job.id,
                    result: None,
                });
            }
        }
    }
}

/// 扫描磁盘缓存目录，只读取确实存在且文件指纹匹配的 JSON 缓存，避免对未分析文件发起解码。
fn load_cached_batch(tracks: Vec<(TrackId, FileStamp)>) -> Vec<(TrackId, Arc<TrackAnalysis>)> {
    let Some(dir) = data_dir().map(|base| base.join("analysis")) else {
        return Vec::new();
    };
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let stamp_by_id: HashMap<TrackId, FileStamp> = tracks.into_iter().collect();
    let cache = AnalysisCache::new(dir);
    let mut loaded = Vec::new();

    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name_str) = name.to_str() else {
            continue;
        };
        let Some(stem) = name_str.strip_suffix(".json") else {
            continue;
        };
        let Ok(raw_id) = u64::from_str_radix(stem, 16) else {
            continue;
        };
        let id = TrackId(raw_id);
        let Some(&stamp) = stamp_by_id.get(&id) else {
            continue;
        };
        if let Some(analysis) = cache.load(id, stamp) {
            loaded.push((id, Arc::new(analysis)));
        }
    }
    loaded
}
