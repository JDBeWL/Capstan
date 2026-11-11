//! 导入流程，开对话框 → 记下曲库根 → 起扫描线程 → 分批读标签 → 入队。
//! 扫描在后台线程跑，主线程每 `SCAN_FLUSH_INTERVAL` 收一批、每拍只 `notify` 一次。
//! 补扫走同一条路，只是先按库里已有的 id 筛一遍，已有的曲目不再读标签。

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Sender, TryRecvError};
use std::time::Duration;

use capstan_core::{
    FileStamp, ScanContext, Track, TrackId, artist_key, collect_audio_paths_with,
    scan_into_tracks_with, sibling_index,
};

use crate::platform::*;

use super::{LibraryEntity, LibraryEvent};

/// 扫描过程中每批之间回主线程的间隔。
const SCAN_FLUSH_INTERVAL: Duration = Duration::from_millis(80);
/// 扫描期间分组与检索缓存的最短失效间隔。每批失效意味着整库重分组与小写化，
/// 48k 曲单批的派生开销可达 50 ms；节流后派生视图最多滞后一个间隔，结束强制失效兜底。
pub(super) const IMPORT_INVALIDATE_INTERVAL: Duration = Duration::from_millis(500);
/// 每读多少首回传一批。
const SCAN_CHUNK: usize = 64;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ScanProgress {
    /// 已经读完标签的曲目数。
    pub found: usize,
    /// 这一轮真正要读的曲目数，即进度分母。`0` 表示目录还没走完、分母还不明。
    /// 不能用 `total_files`。补扫时磁盘上有 1200 首、库里已有 1195 首，
    /// 分母写 1200 会被读成「还剩 1195 首没处理」。
    pub total: usize,
}

/// 后台线程回主线程的三条消息。
enum ScanTick {
    /// 目录走完了，一共这么多音频文件（含已经在库的）。总数先报，界面才知道规模。
    Found(usize),
    /// 筛掉库里已有的之后，这一轮实际要读的份数，即进度分母。
    ToRead(usize),
    Tracks(Vec<Track>),
}

/// 一次扫描的意图。
struct ScanRequest {
    /// 要扫的路径。目录递归展开，文件按单首收。
    inputs: Vec<PathBuf>,
    /// 只读库里没有的文件。补扫用，已经入库的不再读一遍标签。
    fresh_only: bool,
    /// 不看文件指纹，每一首都重读。只由「重读全部标签」置位。
    /// 文件没动但标签被外部编辑器改过时，只有这条路能把改动带回来。
    force: bool,
    /// 启动补扫，没有新增就不打扰用户。
    quiet: bool,
}

impl LibraryEntity {
    pub fn set_recursive(&mut self, recursive: bool, cx: &mut Context<Self>) {
        if self.recursive != recursive {
            self.recursive = recursive;
            cx.notify();
        }
    }

    pub fn recursive(&self) -> bool {
        self.recursive
    }

    pub fn scanning(&self) -> Option<ScanProgress> {
        self.scanning
    }

    /// 已记住的音乐文件夹。设置页要列出来，重扫从这里出发。
    pub fn roots(&self) -> &[PathBuf] {
        self.roots.items()
    }

    pub fn open_dialog(&mut self, cx: &mut Context<Self>) {
        let receiver = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: true,
            multiple: true,
            // 这个串是确定按钮的文案（Windows 走 `SetOkButtonLabel`）。
            // 写长了整颗按钮被撑满一行；Windows 的文件夹选择器本来也只能一次选一个。
            prompt: Some(SharedString::from("导入")),
        });

        cx.spawn(async move |this, cx| {
            // 外层 Result 是「对话框能不能开」，内层是「用户选了什么」
            let Ok(result) = receiver.await else {
                return;
            };
            match result {
                Ok(Some(paths)) if !paths.is_empty() => {
                    this.update(cx, |this, cx| this.import(paths, cx)).ok();
                }
                Ok(_) => {}
                Err(err) => {
                    let message = format!("打开文件对话框失败：{err}");
                    this.update(cx, |_, cx| cx.emit(LibraryEvent::Failed(message)))
                        .ok();
                }
            }
        })
        .detach();
    }

    /// 导入一批路径（对话框、拖放、命令行）。目录同时记录为曲库根，后续补扫从这里出发。
    pub fn import(&mut self, inputs: Vec<PathBuf>, cx: &mut Context<Self>) {
        if inputs.is_empty() {
            return;
        }
        self.remember_roots(&inputs);
        self.start_scan(
            ScanRequest {
                inputs,
                fresh_only: false,
                force: false,
                quiet: false,
            },
            cx,
        );
    }

    /// 重读全部曲目的标签。与 [`Self::rescan`] 不同。后者只补入新文件，已在库的曲目不动；
    /// 这里不看指纹，每首重新解析容器。标签在 Capstan 之外被修改（其它编辑器、脚本、重新下载）时，只有这里能把改动带回来。
    /// 手改过的字段不受影响，见 [`capstan_core::Track::keep_manual`]。
    pub fn refresh_tags(&mut self, cx: &mut Context<Self>) {
        // 首屏还在后台读盘时曲库是空的，这时候刷新等于什么都没做。
        if self.loading {
            return;
        }
        let paths: Vec<PathBuf> = self
            .playlist
            .tracks()
            .iter()
            .map(|track| track.path.clone())
            .collect();
        if paths.is_empty() {
            return;
        }
        self.start_scan(
            ScanRequest {
                inputs: paths,
                fresh_only: false,
                force: true,
                quiet: false,
            },
            cx,
        );
    }

    /// 重新扫描全部曲库根。只读新文件的标签，已在库的原样留着，
    /// 所以重扫一个大目录也只遍历文件系统，不会重读整个曲库。
    pub fn rescan(&mut self, cx: &mut Context<Self>) {
        self.rescan_with(false, cx);
    }

    /// 启动补扫。上一次退出之后新增的曲目在这里补进来，没有新增就不弹提示。
    pub fn rescan_quietly(&mut self, cx: &mut Context<Self>) {
        self.rescan_with(true, cx);
    }

    /// 移除一个曲库根。只停止跟踪，不动已经入库的曲目。
    /// 顺带删曲目是另一回事，不能藏在一个「移除文件夹」里。
    pub fn forget_root(&mut self, path: &Path, cx: &mut Context<Self>) {
        if self.roots.remove(path) {
            crate::roots_store::save(&self.roots);
            cx.notify();
        }
    }

    fn rescan_with(&mut self, quiet: bool, cx: &mut Context<Self>) {
        if self.roots.is_empty() {
            if !quiet {
                cx.emit(LibraryEvent::Failed(
                    "还没有添加音乐文件夹：先导入一个文件夹".to_owned(),
                ));
            }
            return;
        }
        self.start_scan(
            ScanRequest {
                inputs: self.roots.items().to_vec(),
                fresh_only: true,
                force: false,
                quiet,
            },
            cx,
        );
    }

    /// 目录入册为曲库根；单个文件不入册，它只是「这一首」。
    fn remember_roots(&mut self, inputs: &[PathBuf]) {
        let mut changed = false;
        for input in inputs {
            if input.is_dir() && self.roots.add(input.clone()) {
                changed = true;
            }
        }
        if changed {
            crate::roots_store::save(&self.roots);
        }
    }

    /// 起一轮扫描。同一时刻只有一轮，新任务到来时先停止旧任务。
    // `_scan` 只写不读，作用是在 Drop 时取消扫描。
    #[allow(clippy::used_underscore_binding)]
    fn start_scan(&mut self, request: ScanRequest, cx: &mut Context<Self>) {
        // 旧任务的接收端马上会被换掉，先设置停止标志，避免读取无效标签。
        if let Some(cancel) = self.scan_cancel.take() {
            cancel.store(true, Ordering::Relaxed);
        }

        self.scanning = Some(ScanProgress::default());
        cx.notify();

        // 已在库的曲目连同文件指纹一并取走。补扫据此判定「无需读取」，整目录重导据此判定
        // 「文件未变更则不再读取标签」。指纹与 id 同出一源，一次遍历即可完成。
        let cached: HashMap<TrackId, FileStamp> = self
            .playlist
            .tracks()
            .iter()
            .map(|track| (track.id, track.stamp))
            .collect();
        // 补扫要知道「哪些已经在库里」。这份快照在后台线程里用，所以带走一份 id 集合。
        let known: HashSet<TrackId> = if request.fresh_only {
            cached.keys().copied().collect()
        } else {
            HashSet::new()
        };
        // 「重读全部标签」故意不带指纹，带了就等于什么都没做。
        let cached = (!request.force).then_some(cached);

        // 库里已有的艺术家名单。文件名推断拿它定方向。首次导入时是空的，只能靠目录结构。
        let artists = known_artists(self.playlist.tracks());

        let (tx, rx) = mpsc::channel::<ScanTick>();
        // `background_executor()` 返回借用，要 clone 出所有权值才能带进 'static 任务。
        let executor = cx.background_executor().clone();
        // 后台任务拿不到实体，开关按值带进去。
        let recursive = self.recursive;
        let cancel = Arc::new(AtomicBool::new(false));
        self.scan_cancel = Some(Arc::clone(&cancel));

        // 展开目录 → 读标签 → 分批发送；`tx` drop 后的 `Disconnected` 就是扫完了。
        // 用专用 OS 线程而不是 `executor.spawn`。整段阻塞、没有一个 `await`，会长时间占住共享池里的一个 worker。
        let scan = std::thread::Builder::new()
            .name("mp-scan".to_owned())
            .spawn(move || {
                run_scan(
                    request.inputs,
                    recursive,
                    known,
                    artists,
                    cached,
                    cancel,
                    tx,
                );
            });

        if let Err(err) = scan {
            // 线程起不来（资源耗尽）。必须清掉 `scanning`，否则界面永远停在「正在扫描」；还要发 `Failed`，否则点了导入毫无反应。
            let message = format!("无法启动扫描线程：{err}");
            log::error!("{message}");
            self.scanning = None;
            cx.emit(LibraryEvent::Failed(message));
            cx.notify();
            return;
        }

        let quiet = request.quiet;
        let refresh = request.force;
        let task = cx.spawn(async move |this, cx| {
            // 三个计数各有用处。`scanned` 是扫到的文件数（含已在库的），`added` 是新入库的，
            // `refreshed` 是重读过标签的。只报 `added` 的话，空文件夹和「全都已在库」都是 0。
            let mut scanned = 0usize;
            let mut added = 0usize;
            let mut refreshed = 0usize;
            let mut to_read: Option<usize> = None;
            loop {
                executor.timer(SCAN_FLUSH_INTERVAL).await;

                let mut batch = Vec::new();
                let mut done = false;
                loop {
                    match rx.try_recv() {
                        Ok(ScanTick::Found(total)) => scanned = total,
                        Ok(ScanTick::ToRead(count)) => to_read = Some(count),
                        Ok(ScanTick::Tracks(tracks)) => batch.extend(tracks),
                        Err(TryRecvError::Empty) => break,
                        Err(TryRecvError::Disconnected) => {
                            done = true;
                            break;
                        }
                    }
                }

                if batch.is_empty() && to_read.is_none() && !done {
                    continue;
                }

                let alive = this
                    .update(cx, |this, cx| {
                        // 分母要尽早确定，界面才知道当前读取进度。
                        if let Some(count) = to_read.take()
                            && let Some(progress) = this.scanning.as_mut()
                        {
                            progress.total = count;
                        }
                        if !batch.is_empty() {
                            let read = batch.len();
                            refreshed += read;
                            let batch_added = this.playlist.push(batch);
                            added += batch_added;
                            if let Some(progress) = this.scanning.as_mut() {
                                progress.found += read;
                            }
                            // 重导只刷新标签时也要失效分组与检索字段缓存，否则搜索还按旧标题匹配。
                            // 带过滤条件时新曲目也要按条件进投影，否则导入时列表看起来没有变化。
                            // 两者都按 IMPORT_INVALIDATE_INTERVAL 节流，因为每批都做即整库二次成本。
                            if this.import_invalidate_at.elapsed() >= IMPORT_INVALIDATE_INTERVAL {
                                this.invalidate();
                                this.import_invalidate_at = std::time::Instant::now();
                                if this.is_filtering() {
                                    this.recompute_visible(cx);
                                }
                            }
                        }
                        if done {
                            // 节流的兜底。结束时必须让分组、检索与派生视图看到最终集合。
                            if added > 0 || refreshed > 0 {
                                this.invalidate();
                            }
                            this.scanning = None;
                            // 有新增才写盘。整库重写一遍不便宜，启动补扫天天跑。
                            if added > 0 || !quiet {
                                this.persist();
                            }
                            // 进度条消失是唯一的结束信号，空文件夹时更是全程无反馈，所以这里必须发事件。
                            if refresh {
                                // 用户主动点的一次刷新，文案不该说「已在曲库里」
                                cx.emit(LibraryEvent::Refreshed { count: refreshed });
                            } else {
                                cx.emit(LibraryEvent::Imported {
                                    scanned,
                                    added,
                                    refreshed,
                                    quiet,
                                });
                            }
                        }
                        cx.notify();
                    })
                    .is_ok();

                if !alive || done {
                    break;
                }
            }
        });

        self._scan = Some(task);
    }
}

/// 后台线程。展开目录 → 挑出库里没有的 → 逐首读标签，一批一批发回去。
/// 参数按值传入。这是线程入口，所有参数都由 `start_scan` 提供。
#[allow(clippy::needless_pass_by_value)]
fn run_scan(
    inputs: Vec<PathBuf>,
    recursive: bool,
    known: HashSet<TrackId>,
    artists: HashSet<String>,
    cached: Option<HashMap<TrackId, FileStamp>>,
    cancel: Arc<AtomicBool>,
    tx: Sender<ScanTick>,
) {
    let files = collect_audio_paths_with(&inputs, recursive);
    // 总数先报。界面据此知道这次扫到了多少，也才知道「一首新的都没有」是正常结果。
    if tx.send(ScanTick::Found(files.len())).is_err() {
        return;
    }

    // 重复段统计对全部发现的文件算一次，含已知曲目，因此预切片与补扫只传新增文件
    // 都不会割裂同目录信号。纯字符串统计，不碰磁盘。
    let siblings = sibling_index(&files);
    let fresh = fresh_paths(files, &known);
    // 进度分母。用真正要读的份数，磁盘上找到多少首由 `Found` 报给结果汇总，两者不是一回事。
    if tx.send(ScanTick::ToRead(fresh.len())).is_err() {
        return;
    }
    let context = ScanContext {
        known_artists: Some(&artists),
        cached: cached.as_ref(),
        siblings: Some(&siblings),
    };

    // 分片读，每片之间检查停止标志；换扫描目标时，剩余的标签不必再读完。
    for slice in fresh.chunks(SCAN_CHUNK) {
        if cancel.load(Ordering::Relaxed) {
            return;
        }
        scan_into_tracks_with(slice.to_vec(), SCAN_CHUNK, context, |chunk| {
            let _ = tx.send(ScanTick::Tracks(chunk));
        });
    }
}

/// 库里出现过的艺术家名，折成可比较的形式。多人的字段按分隔符拆开。
/// `周杰伦/费玉清` 里的两位都算认识，否则这类合作曲目的文件名推不出来。
fn known_artists(tracks: &[Track]) -> HashSet<String> {
    tracks
        .iter()
        .filter_map(|track| track.artist.as_deref())
        .flat_map(|artist| artist.split(['/', ',']))
        .map(artist_key)
        .filter(|key| !key.is_empty())
        .collect()
}

/// 挑出库里还没有的文件。`known` 为空表示这次不筛（整目录导入要刷新已有曲目的标签）。
fn fresh_paths(files: Vec<PathBuf>, known: &HashSet<TrackId>) -> Vec<PathBuf> {
    if known.is_empty() {
        return files;
    }
    files
        .into_iter()
        .filter(|path| !known.contains(&TrackId::from_path(path)))
        .collect()
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::path::{Path, PathBuf};

    use capstan_core::TrackId;

    use super::fresh_paths;

    fn paths(items: &[&str]) -> Vec<PathBuf> {
        items.iter().map(PathBuf::from).collect()
    }

    fn known(items: &[&str]) -> HashSet<TrackId> {
        items
            .iter()
            .map(|path| TrackId::from_path(Path::new(path)))
            .collect()
    }

    /// 「新增的歌会不会被扫到」全靠这一层筛。在库的要排除，不在库的要保留。
    #[test]
    fn 补扫只挑库里没有的() {
        let found = fresh_paths(
            paths(&["/m/a.flac", "/m/b.flac", "/m/c.flac"]),
            &known(&["/m/a.flac", "/m/c.flac"]),
        );
        assert_eq!(found, paths(&["/m/b.flac"]));
    }

    /// 整目录导入不筛。已经在库的曲目要重读标签，`push` 会按 id 替换旧条目。
    #[test]
    fn 没有已知集合时原样返回() {
        let files = paths(&["/m/a.flac", "/m/b.flac"]);
        assert_eq!(fresh_paths(files.clone(), &HashSet::new()), files);
    }
}
