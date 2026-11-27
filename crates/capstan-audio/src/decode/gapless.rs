//! 播放链内的无缝换轨与双轨自动化混音。
//! 下一段未携带混音计划时做零修改无缝直连；携带 [`CompiledMix`] 时在目标帧处
//! 进入双轨叠加，按块执行音量与低搁架自动化并在过渡结束时裁剪旧段与释放响度补偿。

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rodio::{ChannelCount, SampleRate, Source};

use crate::mix::{
    CompiledMix, EchoOutBank, SoftLimiter, SweepFilterBank, TempoAdapter, TempoMode,
    ThreeBandEqBank, TrimRamp,
};

/// 累计样本数每 256 个同步一次原子量，约 3 ms @44.1 kHz 立体声。
/// 逐样本回写代价过高，段边界强制同步一次，故误差只影响进度条。
const POSITION_STORE_INTERVAL: u64 = 256;

/// 自动化控制块长度，单位帧。块内对增益逐帧线性插值以消除拉链噪声，滤波系数按块刷新。
const CONTROL_BLOCK_FRAMES: u64 = 64;

/// 单段模式下轮询下一段槽位的最大帧间隔，约 1.3 ms @48 kHz。
const SLOT_POLL_FRAMES: u64 = 64;

/// 预载的一段音源。已按输出设备的采样率与声道数对齐。
pub struct Segment {
    source: Box<dyn Source<Item = f32> + Send>,
    mix: Option<CompiledMix>,
}

impl Segment {
    /// 纯无缝直连段，不附加混音计划。
    pub fn new(source: Box<dyn Source<Item = f32> + Send>) -> Self {
        Self { source, mix: None }
    }

    /// 携带已编译混音计划的预载段。
    pub fn with_mix(source: Box<dyn Source<Item = f32> + Send>, mix: CompiledMix) -> Self {
        Self {
            source,
            mix: Some(mix),
        }
    }
}

/// 下一段的可替换槽位。控制线程写入，音频回调在触发帧或段边界取出。
pub type NextSlot = Arc<Mutex<Option<Segment>>>;

pub fn new_slot() -> NextSlot {
    Arc::new(Mutex::new(None))
}

/// 一次换段边界记录，单位为输出格式的交错样本数。供存在环缓冲延迟的后端按已消费位置推进。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SegmentBoundary {
    pub seq: u64,
    pub start_sample: u64,
    pub mix_end_sample: u64,
}

/// 段边界、位置计数与混音标志，单位是交错后的 f32 样本。控制线程据此计算进度并判断换段。
#[derive(Debug, Default)]
pub struct GaplessState {
    segment_seq: AtomicU64,
    segment_start: AtomicU64,
    samples: AtomicU64,
    mixing: AtomicBool,
    boundaries: Mutex<VecDeque<SegmentBoundary>>,
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

    /// 是否正处于双轨叠加过渡期间。
    pub fn is_mixing(&self) -> bool {
        self.mixing.load(Ordering::Acquire)
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

    /// 取出首个 `start_sample <= played_samples` 的边界记录；尚未到达时返回 `None`。
    pub fn pop_boundary_at(&self, played_samples: u64) -> Option<SegmentBoundary> {
        let mut queue = self.boundaries.lock().ok()?;
        let ready = queue
            .front()
            .is_some_and(|b| b.start_sample <= played_samples);
        if ready { queue.pop_front() } else { None }
    }

    fn record_boundary(&self, boundary: SegmentBoundary) {
        if let Ok(mut queue) = self.boundaries.lock() {
            queue.push_back(boundary);
        }
    }
}

/// 音源封装。常规播放走无额外开销的直出分支，过渡期变速对拍与回速期临时包一层 [`TempoAdapter`]。
enum TrackSource {
    Direct(Box<dyn Source<Item = f32> + Send>),
    Stretched(Box<TempoAdapter>),
    Empty,
}

impl TrackSource {
    fn next_frame(&mut self, dst: &mut [f32]) -> bool {
        match self {
            Self::Direct(src) => {
                let Some(first) = src.next() else {
                    return false;
                };
                dst[0] = first;
                for slot in &mut dst[1..] {
                    *slot = src.next().unwrap_or(0.0);
                }
                true
            }
            Self::Stretched(adapter) => adapter.next_frame(dst),
            Self::Empty => false,
        }
    }
}

struct OverlapState {
    next: TrackSource,
    mix: CompiledMix,
    frame: u64,
    a_exhausted: bool,
    block_left: u64,
    a_gain: f32,
    a_step: f32,
    b_gain: f32,
    b_step: f32,
    a_echo_send: f32,
    a_echo_step: f32,
    echo_tail_fade_frames: u64,
    a_eq: Option<ThreeBandEqBank>,
    b_eq: Option<ThreeBandEqBank>,
    a_filter: Option<SweepFilterBank>,
    b_filter: Option<SweepFilterBank>,
    a_echo: Option<EchoOutBank>,
    limiter: SoftLimiter,
    a_frame_buf: Vec<f32>,
    b_frame_buf: Vec<f32>,
    /// 当前帧交错声道缓冲，用于零延迟立体声联动限幅。
    frame_buf: Vec<f32>,
}

impl OverlapState {
    fn new(
        next_raw: Box<dyn Source<Item = f32> + Send>,
        mix: CompiledMix,
        sample_rate: u32,
        channels: usize,
    ) -> Self {
        let ch = channels.max(1);
        let next = if mix.tempo_mode == TempoMode::Off {
            TrackSource::Direct(next_raw)
        } else {
            TrackSource::Stretched(Box::new(TempoAdapter::new(
                next_raw,
                sample_rate,
                ch,
                mix.tempo_mode,
                mix.b_tempo_ratio,
                mix.b_tempo_release_frames,
            )))
        };

        let a_eq = (mix.a_low_db.is_some() || mix.a_mid_db.is_some() || mix.a_high_db.is_some())
            .then(|| {
                ThreeBandEqBank::new(
                    sample_rate,
                    ch,
                    mix.a_low_db.is_some(),
                    mix.a_mid_db.is_some(),
                    mix.a_high_db.is_some(),
                )
            });
        let b_eq = (mix.b_low_db.is_some() || mix.b_mid_db.is_some() || mix.b_high_db.is_some())
            .then(|| {
                ThreeBandEqBank::new(
                    sample_rate,
                    ch,
                    mix.b_low_db.is_some(),
                    mix.b_mid_db.is_some(),
                    mix.b_high_db.is_some(),
                )
            });
        let a_filter = mix
            .a_filter
            .as_ref()
            .map(|_| SweepFilterBank::new(sample_rate, ch));
        let b_filter = mix
            .b_filter
            .as_ref()
            .map(|_| SweepFilterBank::new(sample_rate, ch));
        let a_echo = mix
            .a_echo_send
            .as_ref()
            .map(|_| EchoOutBank::new(sample_rate, ch, mix.echo_delay_secs, mix.echo_feedback));
        let echo_tail_fade_frames = ((0.030 * f64::from(sample_rate)).round() as u64)
            .min(mix.length_frames / 4)
            .max(1);

        Self {
            next,
            mix,
            frame: 0,
            a_exhausted: false,
            block_left: 0,
            a_gain: 1.0,
            a_step: 0.0,
            b_gain: 0.0,
            b_step: 0.0,
            a_echo_send: 0.0,
            a_echo_step: 0.0,
            echo_tail_fade_frames,
            a_eq,
            b_eq,
            a_filter,
            b_filter,
            a_echo,
            limiter: SoftLimiter::new(sample_rate),
            a_frame_buf: vec![0.0; ch],
            b_frame_buf: vec![0.0; ch],
            frame_buf: vec![0.0; ch],
        }
    }

    fn prepare_block(&mut self) {
        let remaining = self.mix.length_frames.saturating_sub(self.frame);
        let block = CONTROL_BLOCK_FRAMES.min(remaining).max(1);
        let total = self.mix.length_frames.max(1) as f32;
        let t0 = (self.frame as f32 / total).clamp(0.0, 1.0);
        let t1 = ((self.frame + block) as f32 / total).clamp(0.0, 1.0);
        let t_mid = f32::midpoint(t0, t1);

        let ga0 = self.mix.a_gain.eval(t0).clamp(0.0, 2.0);
        let ga1 = self.mix.a_gain.eval(t1).clamp(0.0, 2.0);
        let gb0 = self.mix.b_gain.eval(t0).clamp(0.0, 2.0);
        let gb1 = self.mix.b_gain.eval(t1).clamp(0.0, 2.0);

        self.a_gain = ga0;
        self.a_step = (ga1 - ga0) / block as f32;
        self.b_gain = gb0;
        self.b_step = (gb1 - gb0) / block as f32;

        if let Some(auto) = self.mix.a_echo_send.as_ref() {
            let s0 = auto.eval(t0).clamp(0.0, 1.0);
            let s1 = auto.eval(t1).clamp(0.0, 1.0);
            self.a_echo_send = s0;
            self.a_echo_step = (s1 - s0) / block as f32;
        } else {
            self.a_echo_send = 0.0;
            self.a_echo_step = 0.0;
        }

        if let Some(eq) = self.a_eq.as_mut() {
            if let Some(auto) = self.mix.a_low_db.as_ref() {
                eq.set_low_db(auto.eval(t_mid));
            }
            if let Some(auto) = self.mix.a_mid_db.as_ref() {
                eq.set_mid_db(auto.eval(t_mid));
            }
            if let Some(auto) = self.mix.a_high_db.as_ref() {
                eq.set_high_db(auto.eval(t_mid));
            }
        }
        if let Some(eq) = self.b_eq.as_mut() {
            if let Some(auto) = self.mix.b_low_db.as_ref() {
                eq.set_low_db(auto.eval(t_mid));
            }
            if let Some(auto) = self.mix.b_mid_db.as_ref() {
                eq.set_mid_db(auto.eval(t_mid));
            }
            if let Some(auto) = self.mix.b_high_db.as_ref() {
                eq.set_high_db(auto.eval(t_mid));
            }
        }
        if let (Some(flt), Some(auto)) = (self.a_filter.as_mut(), self.mix.a_filter.as_ref()) {
            flt.set_amount(auto.eval(t_mid));
        }
        if let (Some(flt), Some(auto)) = (self.b_filter.as_mut(), self.mix.b_filter.as_ref()) {
            flt.set_amount(auto.eval(t_mid));
        }

        self.block_left = block;
    }

    /// 计算一整帧的双轨 EQ、扫频滤镜、回声散尾、叠加与软限幅；若 B 轨提前耗尽返回 `false`。
    fn render_frame(&mut self, current: &mut TrackSource) -> bool {
        if self.block_left == 0 {
            self.prepare_block();
        }
        if !self.next.next_frame(&mut self.b_frame_buf) {
            return false;
        }
        if self.a_exhausted || !current.next_frame(&mut self.a_frame_buf) {
            self.a_exhausted = true;
            self.a_frame_buf.fill(0.0);
        }

        let rem = self.mix.length_frames.saturating_sub(self.frame);
        let tail_fade = if rem < self.echo_tail_fade_frames {
            rem as f32 / self.echo_tail_fade_frames as f32
        } else {
            1.0
        };
        let b_trim = self.mix.b_trim.hold_gain();
        let channels = self.frame_buf.len();

        for ch in 0..channels {
            let a_raw = self.a_frame_buf[ch];
            let b_raw = self.b_frame_buf[ch];

            let a_eq = match self.a_eq.as_mut() {
                Some(eq) => eq.process_channel(ch, a_raw),
                None => a_raw,
            };
            let a_flt = match self.a_filter.as_mut() {
                Some(flt) => flt.process_channel(ch, a_eq),
                None => a_eq,
            };
            let echo_wet = match self.a_echo.as_mut() {
                Some(echo) => echo.process_channel(ch, a_flt, self.a_echo_send, tail_fade),
                None => 0.0,
            };

            let b_eq = match self.b_eq.as_mut() {
                Some(eq) => eq.process_channel(ch, b_raw),
                None => b_raw,
            };
            let b_flt = match self.b_filter.as_mut() {
                Some(flt) => flt.process_channel(ch, b_eq),
                None => b_eq,
            };

            self.frame_buf[ch] = a_flt * self.a_gain + echo_wet + b_flt * self.b_gain * b_trim;
        }

        self.limiter.process_frame(&mut self.frame_buf);
        self.a_gain += self.a_step;
        self.b_gain += self.b_step;
        self.a_echo_send += self.a_echo_step;
        self.block_left = self.block_left.saturating_sub(1);
        true
    }
}

/// 当前段与预载下一段串联或叠加成一条连续音源。
pub struct GaplessSource {
    current: TrackSource,
    next: NextSlot,
    state: Arc<GaplessState>,
    sample_rate: SampleRate,
    channels: ChannelCount,
    channel_count: usize,
    channel_index: usize,
    /// 当前段起点以来已消费的原始媒体帧数，用于精确命中 `CompiledMix::start_frame`。
    segment_frames: u64,
    /// 下一次检查 `next` 槽位是否到达触发帧的帧号。
    next_poll_frame: u64,
    /// 双轨叠加态；非重叠期间为 `None`。
    overlap: Option<OverlapState>,
    /// 过渡结束后仍需平滑释放的响度补偿；`None` 时零开销直通。
    active_trim: Option<TrimRamp>,
    current_trim_gain: f32,
    /// 单段回速期使用的单帧缓冲。
    single_frame_buf: Vec<f32>,
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
        let channel_count = usize::from(channels.get()).max(1);
        Self {
            current: TrackSource::Direct(current),
            next,
            state,
            sample_rate,
            channels,
            channel_count,
            channel_index: 0,
            segment_frames: 0,
            next_poll_frame: 0,
            overlap: None,
            active_trim: None,
            current_trim_gain: 1.0,
            single_frame_buf: vec![0.0; channel_count],
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

    /// 当回速适配器排空完毕时立即解包回原始直出流，同步已拉取的精确媒体帧数。
    fn unwrap_current_if_ready(&mut self) {
        let ready = matches!(
            &self.current,
            TrackSource::Stretched(adapter) if adapter.is_ready_to_unwrap()
        );
        if !ready {
            return;
        }
        if let TrackSource::Stretched(adapter) =
            std::mem::replace(&mut self.current, TrackSource::Empty)
        {
            self.segment_frames = adapter.source_frames_pulled();
            self.current = TrackSource::Direct(adapter.into_inner());
        }
    }

    /// 在帧边界轮询槽位；若存在带混音计划的预载段且已到达 `start_frame`，进入双轨叠加态。
    fn maybe_enter_overlap(&mut self) {
        if self.segment_frames < self.next_poll_frame {
            return;
        }
        let Ok(mut guard) = self.next.try_lock() else {
            self.next_poll_frame = self.segment_frames + SLOT_POLL_FRAMES;
            return;
        };
        let Some(segment) = guard.as_ref() else {
            self.next_poll_frame = self.segment_frames + SLOT_POLL_FRAMES;
            return;
        };
        let Some(mix) = segment.mix.as_ref() else {
            self.next_poll_frame = self.segment_frames + SLOT_POLL_FRAMES;
            return;
        };

        if self.segment_frames < mix.start_frame {
            self.next_poll_frame = mix.start_frame.min(self.segment_frames + SLOT_POLL_FRAMES);
            return;
        }

        let Some(segment) = guard.take() else {
            return;
        };
        drop(guard);

        let Some(mix) = segment.mix else {
            return;
        };
        self.start_overlap(segment.source, mix);
    }

    fn start_overlap(&mut self, next_source: Box<dyn Source<Item = f32> + Send>, mix: CompiledMix) {
        self.flush_counts();
        let start = self.state.samples.load(Ordering::Relaxed);
        let mix_samples = mix.length_frames * self.channel_count as u64;
        self.state.segment_start.store(start, Ordering::Release);
        let seq = self.state.segment_seq.fetch_add(1, Ordering::AcqRel) + 1;
        self.state.record_boundary(SegmentBoundary {
            seq,
            start_sample: start,
            mix_end_sample: start + mix_samples,
        });

        self.segment_frames = 0;
        self.active_trim = None;
        self.current_trim_gain = 1.0;

        if mix.length_frames == 0 {
            // 零长度硬切，立即丢弃旧段并切到新段。
            self.current = TrackSource::Direct(next_source);
            self.state.mixing.store(false, Ordering::Release);
            if mix.b_trim.is_active() {
                self.active_trim = Some(mix.b_trim);
            }
            self.next_poll_frame = 0;
            return;
        }

        self.state.mixing.store(true, Ordering::Release);
        self.overlap = Some(OverlapState::new(
            next_source,
            mix,
            self.sample_rate.get(),
            self.channel_count,
        ));
    }

    /// 当前段读完，尝试接上预载段。没有下一段时返回 `None`，整条链结束。
    fn advance_segment(&mut self) -> Option<()> {
        let segment = self.next.lock().ok()?.take()?;
        self.flush_counts();
        let start = self.state.samples.load(Ordering::Relaxed);
        self.state.segment_start.store(start, Ordering::Release);
        let seq = self.state.segment_seq.fetch_add(1, Ordering::AcqRel) + 1;
        self.state.record_boundary(SegmentBoundary {
            seq,
            start_sample: start,
            mix_end_sample: start,
        });
        self.state.mixing.store(false, Ordering::Release);
        self.current = TrackSource::Direct(segment.source);
        self.segment_frames = 0;
        self.channel_index = 0;
        self.next_poll_frame = 0;
        self.active_trim = segment.mix.map(|m| m.b_trim).filter(TrimRamp::is_active);
        self.current_trim_gain = 1.0;
        Some(())
    }

    fn finish_overlap(&mut self) {
        if let Some(mut overlap) = self.overlap.take() {
            if let TrackSource::Stretched(ref mut adapter) = overlap.next {
                adapter.begin_release();
                self.segment_frames = adapter.media_frames();
            }
            self.current = overlap.next;
            self.unwrap_current_if_ready();
            self.state.mixing.store(false, Ordering::Release);
            self.active_trim = overlap.mix.b_trim.is_active().then_some(overlap.mix.b_trim);
            self.current_trim_gain = 1.0;
            self.next_poll_frame = self.segment_frames;
        }
    }

    fn next_overlap_sample(&mut self) -> Option<f32> {
        let ch = self.channel_index;
        if ch == 0 {
            let ok = self
                .overlap
                .as_mut()
                .is_some_and(|ov| ov.render_frame(&mut self.current));
            if !ok {
                self.finish_overlap();
                return None;
            }
        }

        let overlap = self.overlap.as_mut()?;
        let sample = overlap.frame_buf.get(ch).copied().unwrap_or(0.0);

        self.channel_index += 1;
        let mut ended = false;
        if self.channel_index >= self.channel_count {
            self.channel_index = 0;
            if let TrackSource::Stretched(ref adapter) = overlap.next {
                self.segment_frames = adapter.media_frames();
            } else {
                self.segment_frames += 1;
            }
            overlap.frame += 1;
            if overlap.frame >= overlap.mix.length_frames {
                ended = true;
            }
        }
        if ended {
            self.finish_overlap();
        }

        self.pending += 1;
        if self.pending >= POSITION_STORE_INTERVAL {
            self.flush_counts();
        }
        Some(sample)
    }
}

impl Iterator for GaplessSource {
    type Item = f32;

    #[inline]
    fn next(&mut self) -> Option<f32> {
        loop {
            if self.overlap.is_some() {
                if let Some(sample) = self.next_overlap_sample() {
                    return Some(sample);
                }
                continue;
            }

            if self.channel_index == 0 {
                self.maybe_enter_overlap();
                if self.overlap.is_some() {
                    continue;
                }
                if let Some(trim) = self.active_trim.as_mut() {
                    self.current_trim_gain = trim.next_release_gain();
                    if !trim.is_active() {
                        self.active_trim = None;
                    }
                } else {
                    self.current_trim_gain = 1.0;
                }
            }

            match &mut self.current {
                TrackSource::Direct(src) => {
                    if let Some(raw) = src.next() {
                        let sample = if (self.current_trim_gain - 1.0).abs() < 1e-6 {
                            raw
                        } else {
                            raw * self.current_trim_gain
                        };
                        self.channel_index += 1;
                        if self.channel_index >= self.channel_count {
                            self.channel_index = 0;
                            self.segment_frames += 1;
                        }
                        self.pending += 1;
                        if self.pending >= POSITION_STORE_INTERVAL {
                            self.flush_counts();
                        }
                        return Some(sample);
                    }
                }
                TrackSource::Stretched(adapter) => {
                    let ch = self.channel_index;
                    if ch == 0 {
                        if !adapter.next_frame(&mut self.single_frame_buf) {
                            self.unwrap_current_if_ready();
                            // 解包后可能切回直出流，进入下一轮继续读取；若已耗尽则换段。
                            if matches!(self.current, TrackSource::Direct(_)) {
                                continue;
                            }
                            self.advance_segment()?;
                            continue;
                        }
                        self.segment_frames = adapter.media_frames();
                    }
                    let raw = self.single_frame_buf.get(ch).copied().unwrap_or(0.0);
                    let sample = if (self.current_trim_gain - 1.0).abs() < 1e-6 {
                        raw
                    } else {
                        raw * self.current_trim_gain
                    };
                    self.channel_index += 1;
                    if self.channel_index >= self.channel_count {
                        self.channel_index = 0;
                        self.unwrap_current_if_ready();
                    }
                    self.pending += 1;
                    if self.pending >= POSITION_STORE_INTERVAL {
                        self.flush_counts();
                    }
                    return Some(sample);
                }
                TrackSource::Empty => {}
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
    use crate::mix::{Automation, CurvePreset, MixKind, MixPlan};
    use rodio::buffer::SamplesBuffer;
    use std::num::NonZero;
    use std::sync::atomic::AtomicBool;

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

        let _: Vec<f32> = source.by_ref().take(1024).collect();
        assert_eq!(
            state.segment_position(48_000, 2),
            Duration::from_secs_f64(1024.0 / 96_000.0),
            "换段后进度要从新段起点算"
        );
    }

    /// 验证双轨叠加；起点前全为 A，叠加期间逐帧吻合 `A·gA + B·gB`，终点后丢弃 A 且全为 B。
    #[test]
    fn mix_overlaps_two_sources_follows_curves_and_drops_trimmed_a() {
        struct TrackDrop {
            left: usize,
            value: f32,
            dropped: Arc<AtomicBool>,
        }
        impl Iterator for TrackDrop {
            type Item = f32;
            fn next(&mut self) -> Option<f32> {
                if self.left == 0 {
                    None
                } else {
                    self.left -= 1;
                    Some(self.value)
                }
            }
        }
        impl Drop for TrackDrop {
            fn drop(&mut self) {
                self.dropped.store(true, Ordering::SeqCst);
            }
        }
        impl Source for TrackDrop {
            fn current_span_len(&self) -> Option<usize> {
                None
            }
            fn channels(&self) -> ChannelCount {
                channels()
            }
            fn sample_rate(&self) -> SampleRate {
                rate()
            }
            fn total_duration(&self) -> Option<Duration> {
                None
            }
        }

        let a_dropped = Arc::new(AtomicBool::new(false));
        // 采样率 1000 Hz 立体声方便逐帧核算，A 共 500 帧（被裁掉最后 200 帧），B 共 400 帧。
        let sr = NonZero::new(1000).unwrap();
        let src_a = TrackDrop {
            left: 500 * 2,
            value: 0.5,
            dropped: Arc::clone(&a_dropped),
        };
        let src_b = Box::new(SamplesBuffer::new(channels(), sr, vec![0.25_f32; 400 * 2]));

        let plan = MixPlan {
            kind: MixKind::Crossfade,
            out_start: Duration::from_millis(100), // 100 帧
            out_end: Duration::from_millis(300),   // 200 帧重叠
            in_start: Duration::ZERO,
            a_gain: Automation::preset_out(CurvePreset::Linear),
            b_gain: Automation::preset_in(CurvePreset::Linear),
            ..MixPlan::default()
        };
        let compiled = CompiledMix::compile(&plan, Duration::ZERO, 1000).unwrap();
        assert_eq!(compiled.start_frame, 100);
        assert_eq!(compiled.length_frames, 200);

        let slot = new_slot();
        *slot.lock().unwrap() = Some(Segment::with_mix(src_b, compiled));
        let state = Arc::new(GaplessState::default());

        let mut source =
            GaplessSource::new(Box::new(src_a), slot, Arc::clone(&state), sr, channels());

        // 前 100 帧（200 样本）全为 A = 0.5，尚未跨段。
        let pre: Vec<f32> = source.by_ref().take(100 * 2).collect();
        assert!(pre.iter().all(|&s| (s - 0.5).abs() < 1e-6));
        assert_eq!(state.segment_seq(), 0);
        assert!(!state.is_mixing());

        // 中间 200 帧（400 样本）处于重叠期。
        let first_mix = source.next().unwrap();
        assert_eq!(state.segment_seq(), 1, "过渡开始即应上报段边界");
        assert_eq!(state.segment_start(), 200);
        assert!(state.is_mixing(), "过渡期间 mixing 标志应为真");

        let mut mid = Vec::with_capacity(400);
        mid.push(first_mix);
        mid.extend(source.by_ref().take(399));

        // 逐帧检查与线性混合公式 0.5*(1-t) + 0.25*t 一致。
        for frame in 0..200 {
            let progress = frame as f32 / 200.0;
            let expected = 0.5 * (1.0 - progress) + 0.25 * progress;
            let left = mid[frame * 2];
            let right = mid[frame * 2 + 1];
            assert!(
                (left - expected).abs() < 1e-3 && (right - expected).abs() < 1e-3,
                "第 {frame} 帧：期望 {expected:.4}，实际 ({left:.4}, {right:.4})"
            );
        }

        // 重叠结束，A 应当已被丢弃（裁剪了剩余 200 帧），此后全为 B = 0.25。
        assert!(!state.is_mixing(), "过渡结束后 mixing 标志应复原");
        assert!(
            a_dropped.load(Ordering::SeqCst),
            "过渡结束必须丢弃被裁剪的 A"
        );

        let tail: Vec<f32> = source.collect();
        assert_eq!(tail.len(), 200 * 2, "B 剩余 200 帧应当完整播出");
        assert!(tail.iter().all(|&s| (s - 0.25).abs() < 1e-6));
    }

    #[test]
    fn early_exhaustion_of_a_during_overlap_continues_b_cleanly() {
        let sr = NonZero::new(1000).unwrap();
        // A 只有 150 帧，但计划在 100..300 帧重叠，A 会在重叠一半时耗尽。
        let a = Box::new(SamplesBuffer::new(channels(), sr, vec![0.5_f32; 150 * 2]));
        let b = Box::new(SamplesBuffer::new(channels(), sr, vec![0.4_f32; 300 * 2]));
        let plan = MixPlan {
            kind: MixKind::Crossfade,
            out_start: Duration::from_millis(100),
            out_end: Duration::from_millis(300),
            in_start: Duration::ZERO,
            a_gain: Automation::preset_out(CurvePreset::Linear),
            b_gain: Automation::preset_in(CurvePreset::Linear),
            ..MixPlan::default()
        };
        let compiled = CompiledMix::compile(&plan, Duration::ZERO, 1000).unwrap();

        let slot = new_slot();
        *slot.lock().unwrap() = Some(Segment::with_mix(b, compiled));
        let state = Arc::new(GaplessState::default());
        let source = GaplessSource::new(a, slot, state, sr, channels());

        let out: Vec<f32> = source.collect();
        // 100 帧纯 A + 300 帧 B（其中前 200 帧为重叠期） = 400 帧。
        assert_eq!(out.len(), 400 * 2);
        // 最后 100 帧全为 B = 0.4。
        assert!(out[300 * 2..].iter().all(|&s| (s - 0.4).abs() < 1e-6));
    }

    #[test]
    fn echo_out_keeps_ringing_after_dry_a_cuts_and_soft_limiter_prevents_clipping() {
        let sr = NonZero::new(8_000).unwrap();
        let frames = 4_000_usize; // 0.5 秒
        // A 轨输出 1 kHz 正弦波，B 轨为静音，验证干声切零后回声仍在重叠尾段延续。
        let mut samples_a = Vec::with_capacity(frames * 2);
        for i in 0..frames {
            let v = (std::f32::consts::TAU * 1_000.0 * i as f32 / 8_000.0).sin() * 0.8;
            samples_a.push(v);
            samples_a.push(v);
        }
        let a = Box::new(SamplesBuffer::new(channels(), sr, samples_a));
        let b = Box::new(SamplesBuffer::new(
            channels(),
            sr,
            vec![0.0_f32; frames * 2],
        ));

        let plan = MixPlan {
            kind: MixKind::Cut,
            out_start: Duration::ZERO,
            out_end: Duration::from_millis(400),
            in_start: Duration::ZERO,
            a_gain: Automation::ramp(0.15, 1.0, 0.25, 0.0),
            b_gain: Automation::flat(1.0),
            a_echo_send: Automation::pulse(0.0, 0.05, 0.20, 0.25, 0.9),
            echo_delay_secs: 0.10,
            echo_feedback: 0.60,
            ..MixPlan::default()
        };
        let compiled = CompiledMix::compile(&plan, Duration::ZERO, 8_000).unwrap();
        let slot = new_slot();
        *slot.lock().unwrap() = Some(Segment::with_mix(b, compiled));
        let source = GaplessSource::new(a, slot, Arc::new(GaplessState::default()), sr, channels());
        let out: Vec<f32> = source.collect();

        // t ∈ [0.35, 0.80] 区间 A 干声已归零且 B 为静音，此时只剩回声散尾，峰值应显著非零。
        let tail_peak = out[1_200 * 2..2_600 * 2]
            .iter()
            .fold(0.0_f32, |acc, &s| acc.max(s.abs()));
        assert!(
            tail_peak > 0.1,
            "干声切零后应能听到回声散尾，实测 {tail_peak}"
        );

        // 验证两首满幅轨叠加时软限幅把峰值压在 1.0 以内，避免削波。
        let hot_a = Box::new(SamplesBuffer::new(channels(), sr, vec![0.9_f32; 800 * 2]));
        let hot_b = Box::new(SamplesBuffer::new(channels(), sr, vec![0.9_f32; 800 * 2]));
        let hot_plan = MixPlan {
            kind: MixKind::Crossfade,
            out_start: Duration::ZERO,
            out_end: Duration::from_millis(100),
            in_start: Duration::ZERO,
            a_gain: Automation::flat(1.0),
            b_gain: Automation::flat(1.0),
            ..MixPlan::default()
        };
        let hot_compiled = CompiledMix::compile(&hot_plan, Duration::ZERO, 8_000).unwrap();
        let hot_slot = new_slot();
        *hot_slot.lock().unwrap() = Some(Segment::with_mix(hot_b, hot_compiled));
        let hot_source = GaplessSource::new(
            hot_a,
            hot_slot,
            Arc::new(GaplessState::default()),
            sr,
            channels(),
        );
        let hot_peak = hot_source.fold(0.0_f32, |acc, s| acc.max(s.abs()));
        assert!(
            hot_peak <= 0.99,
            "双轨满幅叠加必须被软限幅压在 1.0 以内，实测 {hot_peak}"
        );
    }

    #[test]
    fn tempo_stretched_overlap_releases_back_to_direct_b_and_finishes_cleanly() {
        let sr = NonZero::new(8_000).unwrap();
        // A 共 0.5 秒（4000 帧），B 共 1.0 秒（8000 帧）；0.1s..0.4s 重叠，
        // B 以 0.94× 拉伸并在结束后 0.2s 内滑回原速。
        let a = Box::new(SamplesBuffer::new(channels(), sr, vec![0.3_f32; 4_000 * 2]));
        let mut samples_b = Vec::with_capacity(8_000 * 2);
        for i in 0..8_000 {
            let v = (std::f32::consts::TAU * 440.0 * i as f32 / 8_000.0).sin() * 0.5;
            samples_b.push(v);
            samples_b.push(v);
        }
        let b = Box::new(SamplesBuffer::new(channels(), sr, samples_b));

        let plan = MixPlan {
            kind: MixKind::BeatMatched,
            out_start: Duration::from_millis(100),
            out_end: Duration::from_millis(400),
            in_start: Duration::ZERO,
            a_gain: Automation::preset_out(CurvePreset::EqualPower),
            b_gain: Automation::preset_in(CurvePreset::EqualPower),
            tempo_mode: TempoMode::PitchPreserving,
            b_tempo_ratio: 0.94,
            b_tempo_release: Duration::from_millis(200),
            ..MixPlan::default()
        };
        let compiled = CompiledMix::compile(&plan, Duration::ZERO, 8_000).unwrap();
        let slot = new_slot();
        *slot.lock().unwrap() = Some(Segment::with_mix(b, compiled));
        let state = Arc::new(GaplessState::default());
        let mut source = GaplessSource::new(a, slot, Arc::clone(&state), sr, channels());

        let out: Vec<f32> = source.by_ref().collect();
        assert_eq!(state.segment_seq(), 1, "应当正常跨段一次");
        assert!(!state.is_mixing(), "播完后混音标志必须复位");
        assert!(
            matches!(source.current, TrackSource::Direct(_)),
            "回速结束后必须解包回零开销直出音源"
        );
        // 0.1s A + ~1.02s 拉伸后的 B ≈ 9000 帧（18000 样本）。
        let expected_samples = 18_000_usize;
        assert!(
            out.len().abs_diff(expected_samples) < 1_600,
            "带拉伸与回速的总样本数应接近 {expected_samples}，实测 {}",
            out.len()
        );
        assert!(out.iter().all(|s| s.is_finite() && s.abs() <= 1.0));
    }
}
