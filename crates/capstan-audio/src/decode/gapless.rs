//! 播放链内的无缝换轨。当前段与预载下一段共享同一条 `Source`，段边界不插静音、不重置增益。
//! 下一段置于可替换槽位，队列顺序变化时只换槽位，不重放当前段，也不会播出过期曲目。

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rodio::{ChannelCount, SampleRate, Source};

/// 累计样本数每 256 个同步一次原子量，约 3 ms @44.1 kHz 立体声。
/// 逐样本回写代价过高，段边界强制同步一次，故误差只影响进度条。
const POSITION_STORE_INTERVAL: u64 = 256;

/// 预载的一段音源。已按输出设备的采样率与声道数对齐。
pub struct Segment {
    source: Box<dyn Source<Item = f32> + Send>,
}

impl Segment {
    pub fn new(source: Box<dyn Source<Item = f32> + Send>) -> Self {
        Self { source }
    }
}

/// 下一段的可替换槽位。控制线程写入，音频回调在段边界取出。
pub type NextSlot = Arc<Mutex<Option<Segment>>>;

pub fn new_slot() -> NextSlot {
    Arc::new(Mutex::new(None))
}

/// 段边界与位置计数，单位是交错后的 f32 样本。控制线程据此计算进度并判断换段。
#[derive(Debug, Default)]
pub struct GaplessState {
    segment_seq: AtomicU64,
    segment_start: AtomicU64,
    samples: AtomicU64,
}

impl GaplessState {
    /// 已跨过的段边界数。0 表示仍在第一段，每跨一次加一。
    pub fn segment_seq(&self) -> u64 {
        self.segment_seq.load(Ordering::Acquire)
    }

    /// 当前段起点对应的累计样本数。
    pub fn segment_start(&self) -> u64 {
        self.segment_start.load(Ordering::Acquire)
    }

    /// 当前段已经输出的时长。
    pub fn segment_position(&self, sample_rate: u32, channels: u16) -> Duration {
        let per_second = u64::from(sample_rate) * u64::from(channels.max(1));
        if per_second == 0 {
            return Duration::ZERO;
        }
        let played = self
            .samples
            .load(Ordering::Relaxed)
            .saturating_sub(self.segment_start.load(Ordering::Acquire));
        Duration::from_secs_f64(played as f64 / per_second as f64)
    }
}

/// 当前段与预载下一段串联成一条连续音源。
pub struct GaplessSource {
    current: Box<dyn Source<Item = f32> + Send>,
    next: NextSlot,
    state: Arc<GaplessState>,
    sample_rate: SampleRate,
    channels: ChannelCount,
    /// 尚未同步进 `state.samples` 的样本数。
    pending: u64,
}

impl GaplessSource {
    pub fn new(
        current: Box<dyn Source<Item = f32> + Send>,
        next: NextSlot,
        state: Arc<GaplessState>,
        sample_rate: SampleRate,
        channels: ChannelCount,
    ) -> Self {
        Self {
            current,
            next,
            state,
            sample_rate,
            channels,
            pending: 0,
        }
    }

    fn flush_counts(&mut self) {
        if self.pending > 0 {
            self.state
                .samples
                .fetch_add(self.pending, Ordering::Relaxed);
            self.pending = 0;
        }
    }

    /// 当前段读完，尝试接上预载段。没有下一段时返回 `None`，整条链结束。
    fn advance_segment(&mut self) -> Option<()> {
        let segment = self.next.lock().ok()?.take()?;
        // 必须记录边界落点，新段进度才能从 0 重新起算。
        self.flush_counts();
        let start = self.state.samples.load(Ordering::Relaxed);
        self.state.segment_start.store(start, Ordering::Release);
        self.state.segment_seq.fetch_add(1, Ordering::AcqRel);
        self.current = segment.source;
        Some(())
    }
}

impl Iterator for GaplessSource {
    type Item = f32;

    #[inline]
    fn next(&mut self) -> Option<f32> {
        loop {
            if let Some(sample) = self.current.next() {
                self.pending += 1;
                if self.pending >= POSITION_STORE_INTERVAL {
                    self.flush_counts();
                }
                return Some(sample);
            }
            // 当前段结束；接不上下一段时整条链结束。
            self.advance_segment()?;
        }
    }
}

impl Source for GaplessSource {
    fn current_span_len(&self) -> Option<usize> {
        None
    }

    fn channels(&self) -> ChannelCount {
        self.channels
    }

    fn sample_rate(&self) -> SampleRate {
        self.sample_rate
    }

    fn total_duration(&self) -> Option<Duration> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rodio::buffer::SamplesBuffer;
    use std::num::NonZero;

    fn rate() -> SampleRate {
        NonZero::new(48_000).unwrap()
    }

    fn channels() -> ChannelCount {
        NonZero::new(2).unwrap()
    }

    fn constant(value: f32, samples: usize) -> Box<dyn Source<Item = f32> + Send> {
        Box::new(SamplesBuffer::new(channels(), rate(), vec![value; samples]))
    }

    fn chain(
        current: Box<dyn Source<Item = f32> + Send>,
        slot: NextSlot,
        state: Arc<GaplessState>,
    ) -> GaplessSource {
        GaplessSource::new(current, slot, state, rate(), channels())
    }

    #[test]
    fn segments_join_without_inserted_samples() {
        let slot = new_slot();
        let state = Arc::new(GaplessState::default());
        *slot.lock().unwrap() = Some(Segment::new(constant(0.5, 1024)));

        let mut source = chain(constant(0.25, 1024), Arc::clone(&slot), Arc::clone(&state));
        let out: Vec<f32> = source.by_ref().take(2048).collect();

        assert_eq!(out.len(), 2048, "两段应当完整衔接");
        assert!(out[..1024].iter().all(|s| *s == 0.25));
        assert!(out[1024..].iter().all(|s| *s == 0.5), "边界不得插入零样本");
        assert_eq!(state.segment_seq(), 1, "跨过一次边界");
        assert_eq!(
            state.segment_start(),
            1024,
            "第二段从累计第 1024 个样本起算"
        );
    }

    #[test]
    fn preloaded_segment_can_be_replaced_before_the_boundary() {
        let slot = new_slot();
        let state = Arc::new(GaplessState::default());
        *slot.lock().unwrap() = Some(Segment::new(constant(0.25, 1024)));

        let mut source = chain(constant(0.5, 1024), Arc::clone(&slot), Arc::clone(&state));
        assert!(source.by_ref().take(512).all(|s| s == 0.5));

        // 队列顺序变化时，槽位里的旧下一段尚未开始，直接替换。
        *slot.lock().unwrap() = Some(Segment::new(constant(0.75, 1024)));
        let out: Vec<f32> = source.by_ref().take(1536).collect();

        assert!(out[..512].iter().all(|s| *s == 0.5), "当前段不能被改写");
        assert!(
            out[512..].iter().all(|s| *s == 0.75),
            "边界之后应当播替换后的下一段"
        );
    }

    #[test]
    fn no_next_segment_ends_the_chain() {
        let state = Arc::new(GaplessState::default());
        let mut source = chain(constant(0.5, 1024), new_slot(), Arc::clone(&state));

        let out: Vec<f32> = source.by_ref().collect();
        assert_eq!(out.len(), 1024);
        assert_eq!(state.segment_seq(), 0, "没跨段");
        assert_eq!(state.segment_start(), 0);
    }

    #[test]
    fn segment_position_restarts_at_each_boundary() {
        let slot = new_slot();
        *slot.lock().unwrap() = Some(Segment::new(constant(0.5, 4096)));
        let state = Arc::new(GaplessState::default());
        let mut source = chain(constant(0.25, 1024), slot, Arc::clone(&state));

        let first: Vec<f32> = source.by_ref().take(1024).collect();
        assert_eq!(first.len(), 1024);
        assert_eq!(
            state.segment_position(48_000, 2),
            Duration::from_secs_f64(1024.0 / 96_000.0),
            "第一段的已播时长"
        );

        // 第二段再走 1024 个样本，进度应当从 0 重新起算。
        let _: Vec<f32> = source.by_ref().take(1024).collect();
        assert_eq!(
            state.segment_position(48_000, 2),
            Duration::from_secs_f64(1024.0 / 96_000.0),
            "换段后进度要从新段起点算"
        );
    }
}
