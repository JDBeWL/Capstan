//! 过渡期变速对拍引擎。±3% 内采用 4 点三次插值微变速，±3%–±12% 采用立体声联动 WSOLA 保音高拉伸。
//! 过渡结束后平滑滑回原速并无缝解包为原始音频流，正常播放期间零额外开销。

use std::collections::VecDeque;
use std::f32::consts::TAU;

use rodio::Source;
use serde::{Deserialize, Serialize};

/// 允许的最小与最大速度倍率（±12%）。
pub const MIN_TEMPO_RATIO: f32 = 0.88;
pub const MAX_TEMPO_RATIO: f32 = 1.12;
/// 退出微变速前对齐小数相位的过渡帧数（约 1.3 ms @48 kHz）。
const VARISPEED_SETTLE_STEPS: u32 = 64;

/// 过渡期对拍的变速模式。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum TempoMode {
    /// 不变速，保持原速播放。
    #[default]
    Off,
    /// 四点三次插值微变速，适合 ±3% 以内，瞬态最锐利且无相位涂抹。
    Varispeed,
    /// 立体声联动 WSOLA 保音高拉伸，适合 ±3%–±10% 速度差，不变调且无漂移。
    PitchPreserving,
}

impl TempoMode {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Off => "原速",
            Self::Varispeed => "微变速锁定",
            Self::PitchPreserving => "保音高拉伸",
        }
    }
}

/// 挂在 B 轨前端的过渡变速适配器。重叠期锁在目标倍率，重叠结束后平滑滑回 `1.0×` 并归还原始音源。
pub struct TempoAdapter {
    source: Box<dyn Source<Item = f32> + Send>,
    hold_ratio: f64,
    release_frames: u64,
    release_elapsed: u64,
    releasing: bool,
    engine: TempoEngine,
}

enum TempoEngine {
    Varispeed(VarispeedCore),
    Wsola(WsolaCore),
}

impl TempoAdapter {
    /// 按指定模式与采样率构造变速适配器。`ratio` 为 `input_frames / output_frames`（小于 1 表示放慢）。
    pub fn new(
        mut source: Box<dyn Source<Item = f32> + Send>,
        sample_rate: u32,
        channels: usize,
        mode: TempoMode,
        ratio: f32,
        release_frames: u64,
    ) -> Self {
        let ch = channels.max(1);
        let hold_ratio = f64::from(ratio.clamp(MIN_TEMPO_RATIO, MAX_TEMPO_RATIO));
        let engine = match mode {
            TempoMode::PitchPreserving => TempoEngine::Wsola(WsolaCore::new(sample_rate, ch)),
            TempoMode::Varispeed | TempoMode::Off => {
                TempoEngine::Varispeed(VarispeedCore::new(&mut *source, ch))
            }
        };
        Self {
            source,
            hold_ratio,
            release_frames,
            release_elapsed: 0,
            releasing: false,
            engine,
        }
    }

    /// 通知适配器重叠期已结束，开始在 `release_frames` 内平滑滑回 `1.0×`。
    pub fn begin_release(&mut self) {
        self.releasing = true;
    }

    /// 是否已完成回速并排空内部缓冲，可安全调用 [`Self::into_inner`] 切回零开销直出。
    pub fn is_ready_to_unwrap(&self) -> bool {
        match &self.engine {
            TempoEngine::Varispeed(v) => v.done,
            TempoEngine::Wsola(w) => matches!(w.phase, WsolaPhase::Done),
        }
    }

    /// 归还底层原始音源，其下一帧恰好与刚输出的最后一帧无损衔接。
    pub fn into_inner(self) -> Box<dyn Source<Item = f32> + Send> {
        self.source
    }

    /// 当前已播放到的原始媒体帧位置，供上层校准下一段混音的精确节拍触发帧。
    pub fn media_frames(&self) -> u64 {
        match &self.engine {
            TempoEngine::Varispeed(v) => v.cursor_frame,
            TempoEngine::Wsola(w) => w.media_frames(),
        }
    }

    /// 已从底层原始音源拉取的总帧数。解包回直出时用于对齐媒体帧计数。
    pub fn source_frames_pulled(&self) -> u64 {
        match &self.engine {
            TempoEngine::Varispeed(v) => v.source_pulled,
            TempoEngine::Wsola(w) => w.source_pulled,
        }
    }

    /// 生成下一帧交错样本写入 `out`，长度必须等于声道数；底层音源耗尽时返回 `false`。
    pub fn next_frame(&mut self, out: &mut [f32]) -> bool {
        let (ratio, finish_now) = self.current_ratio_and_finish();
        if self.releasing && self.release_elapsed < self.release_frames {
            self.release_elapsed += 1;
        }
        match &mut self.engine {
            TempoEngine::Varispeed(v) => v.next_frame(&mut *self.source, ratio, finish_now, out),
            TempoEngine::Wsola(w) => w.next_frame(&mut *self.source, ratio, finish_now, out),
        }
    }

    fn current_ratio_and_finish(&self) -> (f64, bool) {
        if !self.releasing {
            return (self.hold_ratio, false);
        }
        if self.release_frames == 0 || self.release_elapsed >= self.release_frames {
            return (1.0, true);
        }
        let t = self.release_elapsed as f64 / self.release_frames as f64;
        let ratio = self.hold_ratio + (1.0 - self.hold_ratio) * t;
        (ratio, false)
    }
}

/// 从交错音源拉取完整一帧；首声道为 `None` 时返回 `false`，尾残帧以 0 补齐声道。
fn pull_frame(source: &mut dyn Source<Item = f32>, dst: &mut [f32]) -> bool {
    let Some(first) = source.next() else {
        return false;
    };
    dst[0] = first;
    for slot in &mut dst[1..] {
        *slot = source.next().unwrap_or(0.0);
    }
    true
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum VariPhase {
    Running,
    Settling { steps_left: u32, step_ratio: f64 },
    Draining,
}

/// 4 点 Catmull-Rom 三次插值流式微变速内核。
struct VarispeedCore {
    channels: usize,
    h: [Vec<f32>; 4],
    frac: f64,
    cursor_frame: u64,
    source_pulled: u64,
    remaining_valid: usize,
    source_eof: bool,
    phase: VariPhase,
    done: bool,
}

impl VarispeedCore {
    fn new(source: &mut dyn Source<Item = f32>, channels: usize) -> Self {
        let mut h = [
            vec![0.0; channels],
            vec![0.0; channels],
            vec![0.0; channels],
            vec![0.0; channels],
        ];
        let mut source_pulled = 0_u64;
        let mut remaining_valid = 0_usize;
        let mut source_eof = false;

        if pull_frame(source, &mut h[1]) {
            let first = h[1].clone();
            h[0].copy_from_slice(&first);
            source_pulled = 1;
            remaining_valid = 1;
            if pull_frame(source, &mut h[2]) {
                source_pulled = 2;
                remaining_valid = 2;
                if pull_frame(source, &mut h[3]) {
                    source_pulled = 3;
                    remaining_valid = 3;
                } else {
                    let prev = h[2].clone();
                    h[3].copy_from_slice(&prev);
                    source_eof = true;
                }
            } else {
                h[2].copy_from_slice(&first);
                h[3].copy_from_slice(&first);
                source_eof = true;
            }
        } else {
            source_eof = true;
        }

        Self {
            channels,
            h,
            frac: 0.0,
            cursor_frame: 0,
            source_pulled,
            remaining_valid,
            source_eof,
            phase: VariPhase::Running,
            done: false,
        }
    }

    fn shift_pull(&mut self, source: &mut dyn Source<Item = f32>) {
        self.h.rotate_left(1);
        self.cursor_frame += 1;
        if !self.source_eof && pull_frame(source, &mut self.h[3]) {
            self.source_pulled += 1;
        } else {
            self.source_eof = true;
            let prev = self.h[2].clone();
            self.h[3].copy_from_slice(&prev);
            self.remaining_valid = self.remaining_valid.saturating_sub(1);
        }
    }

    fn next_frame(
        &mut self,
        source: &mut dyn Source<Item = f32>,
        ratio: f64,
        finish_now: bool,
        out: &mut [f32],
    ) -> bool {
        if self.remaining_valid == 0 {
            self.done = true;
            return false;
        }

        if finish_now && self.phase == VariPhase::Running {
            if self.frac < 1e-4 {
                self.frac = 0.0;
                self.phase = VariPhase::Draining;
            } else {
                let steps = VARISPEED_SETTLE_STEPS;
                let step_ratio = if self.frac >= 0.5 {
                    1.0 + (1.0 - self.frac) / f64::from(steps)
                } else {
                    1.0 - self.frac / f64::from(steps)
                };
                self.phase = VariPhase::Settling {
                    steps_left: steps,
                    step_ratio,
                };
            }
        }

        if self.phase == VariPhase::Draining {
            out[..self.channels].copy_from_slice(&self.h[1]);
            self.cursor_frame += 1;
            self.remaining_valid = self.remaining_valid.saturating_sub(1);
            if self.remaining_valid == 0 {
                self.done = true;
            } else {
                self.h.rotate_left(1);
            }
            return true;
        }

        let t = self.frac as f32;
        for (ch, slot) in out.iter_mut().enumerate().take(self.channels) {
            let y0 = self.h[0][ch];
            let y1 = self.h[1][ch];
            let y2 = self.h[2][ch];
            let y3 = self.h[3][ch];
            let a0 = -0.5 * y0 + 1.5 * y1 - 1.5 * y2 + 0.5 * y3;
            let a1 = y0 - 2.5 * y1 + 2.0 * y2 - 0.5 * y3;
            let a2 = -0.5 * y0 + 0.5 * y2;
            let a3 = y1;
            *slot = ((a0 * t + a1) * t + a2) * t + a3;
        }

        let effective_ratio = match self.phase {
            VariPhase::Running => ratio,
            VariPhase::Settling { step_ratio, .. } => step_ratio,
            VariPhase::Draining => 1.0,
        };

        self.frac += effective_ratio;
        while self.frac >= 1.0 && self.remaining_valid > 0 {
            self.frac -= 1.0;
            self.shift_pull(source);
        }

        if let VariPhase::Settling {
            steps_left,
            step_ratio,
        } = self.phase
        {
            let next_left = steps_left.saturating_sub(1);
            if next_left == 0 {
                // 对齐收尾时若小数位刚好紧贴 1.0，再推进一帧落回整数边界。
                if self.frac >= 0.5 && self.remaining_valid > 0 {
                    self.shift_pull(source);
                }
                self.frac = 0.0;
                self.phase = VariPhase::Draining;
                if self.remaining_valid == 0 {
                    self.done = true;
                }
            } else {
                self.phase = VariPhase::Settling {
                    steps_left: next_left,
                    step_ratio,
                };
            }
        }

        true
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WsolaPhase {
    Running,
    Draining { next_frame: u64 },
    Done,
}

/// 50% 重叠 Hann 窗立体声联动 WSOLA 时间拉伸内核。
struct WsolaCore {
    channels: usize,
    hop_s: usize,
    win: usize,
    search_radius: usize,
    window: Vec<f32>,
    in_buf: VecDeque<f32>,
    buf_start: u64,
    source_pulled: u64,
    source_eof: bool,
    tail_buf: Vec<f32>,
    out_buf: Vec<f32>,
    out_cursor: usize,
    out_len: usize,
    first_block: bool,
    natural_next: u64,
    analysis_pos: f64,
    phase: WsolaPhase,
    scratch_frame: Vec<f32>,
}

impl WsolaCore {
    fn new(sample_rate: u32, channels: usize) -> Self {
        let sr = sample_rate.max(8_000) as f32;
        let hop_s = ((sr * 0.020).round() as usize).max(64);
        let win = hop_s * 2;
        let search_radius = (hop_s / 4).max(16);
        let window: Vec<f32> = (0..win)
            .map(|i| 0.5 * (1.0 - (TAU * i as f32 / win as f32).cos()))
            .collect();

        Self {
            channels,
            hop_s,
            win,
            search_radius,
            window,
            in_buf: VecDeque::with_capacity(win * 4 * channels),
            buf_start: 0,
            source_pulled: 0,
            source_eof: false,
            tail_buf: vec![0.0; hop_s * channels],
            out_buf: vec![0.0; hop_s * channels],
            out_cursor: 0,
            out_len: 0,
            first_block: true,
            natural_next: 0,
            analysis_pos: 0.0,
            phase: WsolaPhase::Running,
            scratch_frame: vec![0.0; channels],
        }
    }

    fn media_frames(&self) -> u64 {
        match self.phase {
            WsolaPhase::Draining { next_frame } => next_frame,
            WsolaPhase::Done => self.source_pulled,
            WsolaPhase::Running => {
                let block_offset = self.out_cursor as u64;
                (self.analysis_pos.round().max(0.0) as u64)
                    .saturating_sub(self.hop_s as u64)
                    .saturating_add(block_offset)
            }
        }
    }

    fn ensure_frames(&mut self, source: &mut dyn Source<Item = f32>, end_frame: u64) {
        while !self.source_eof && self.source_pulled < end_frame {
            if pull_frame(source, &mut self.scratch_frame) {
                self.in_buf.extend(self.scratch_frame.iter().copied());
                self.source_pulled += 1;
            } else {
                self.source_eof = true;
            }
        }
    }

    #[inline]
    fn sample_at(&self, frame: u64, ch: usize) -> f32 {
        if frame < self.buf_start {
            return 0.0;
        }
        let rel = (frame - self.buf_start) as usize;
        self.in_buf
            .get(rel * self.channels + ch)
            .copied()
            .unwrap_or(0.0)
    }

    #[inline]
    fn mono_at(&self, frame: u64) -> f32 {
        if frame < self.buf_start {
            return 0.0;
        }
        let base = (frame - self.buf_start) as usize * self.channels;
        if base + self.channels > self.in_buf.len() {
            return 0.0;
        }
        let mut sum = 0.0_f32;
        for ch in 0..self.channels {
            sum += self.in_buf[base + ch];
        }
        sum / self.channels as f32
    }

    fn discard_before(&mut self, keep_from: u64) {
        if keep_from <= self.buf_start {
            return;
        }
        let avail_frames = self.in_buf.len() / self.channels;
        let drop_frames = ((keep_from - self.buf_start) as usize).min(avail_frames);
        if drop_frames > 0 {
            self.in_buf.drain(..drop_frames * self.channels);
            self.buf_start += drop_frames as u64;
        }
    }

    /// 在 `[cand_min, cand_max]` 中寻找与自然续接模板相关性最高的起点帧，以声道和单声道搜索保证立体声相位锁定。
    fn best_candidate(&self, cand_min: u64, cand_max: u64) -> u64 {
        let step = (self.hop_s / 64).max(1);
        let mut ref_energy = 0.0_f32;
        let mut k = 0;
        while k < self.hop_s {
            let r = self.mono_at(self.natural_next + k as u64);
            ref_energy += r * r;
            k += step;
        }
        if ref_energy < 1e-10 {
            return cand_min + (cand_max - cand_min) / 2;
        }

        let mut best_frame = cand_min;
        let mut best_score = f32::NEG_INFINITY;

        for cand in cand_min..=cand_max {
            let mut dot = 0.0_f32;
            let mut cand_energy = 0.0_f32;
            let mut i = 0;
            while i < self.hop_s {
                let r = self.mono_at(self.natural_next + i as u64);
                let c = self.mono_at(cand + i as u64);
                dot += r * c;
                cand_energy += c * c;
                i += step;
            }
            let score = dot / (ref_energy * cand_energy + 1e-12).sqrt();
            if score > best_score {
                best_score = score;
                best_frame = cand;
            }
        }
        best_frame
    }

    fn synthesize_block(
        &mut self,
        source: &mut dyn Source<Item = f32>,
        ratio: f64,
        finish_now: bool,
    ) {
        if self.first_block {
            self.ensure_frames(source, self.win as u64);
            let avail = self.source_pulled as usize;
            if avail == 0 {
                self.out_len = 0;
                self.phase = WsolaPhase::Done;
                return;
            }
            let emit = avail.min(self.hop_s);
            for i in 0..emit {
                for ch in 0..self.channels {
                    self.out_buf[i * self.channels + ch] = self.sample_at(i as u64, ch);
                }
            }
            for i in 0..self.hop_s {
                let w = self.window[self.hop_s + i];
                for ch in 0..self.channels {
                    self.tail_buf[i * self.channels + ch] =
                        self.sample_at((self.hop_s + i) as u64, ch) * w;
                }
            }
            self.natural_next = self.hop_s as u64;
            self.analysis_pos = (self.hop_s as f64) * ratio;
            self.first_block = false;
            self.out_len = emit;
            self.out_cursor = 0;
            if finish_now {
                self.phase = WsolaPhase::Draining {
                    next_frame: emit as u64,
                };
            }
            return;
        }

        if finish_now {
            // 收尾块选在自然续接点；前后半 Hann 窗之和恒为 1.0，精确无损切回原流。
            let end_needed = self.natural_next + self.hop_s as u64;
            self.ensure_frames(source, end_needed);
            let emit = self
                .source_pulled
                .saturating_sub(self.natural_next)
                .min(self.hop_s as u64) as usize;
            for i in 0..emit {
                let w = self.window[i];
                for ch in 0..self.channels {
                    let idx = i * self.channels + ch;
                    self.out_buf[idx] =
                        self.tail_buf[idx] + self.sample_at(self.natural_next + i as u64, ch) * w;
                }
            }
            self.out_len = emit;
            self.out_cursor = 0;
            self.phase = WsolaPhase::Draining {
                next_frame: self.natural_next + emit as u64,
            };
            return;
        }

        let ideal = self.analysis_pos.round().max(0.0) as u64;
        let cand_min = ideal
            .saturating_sub(self.search_radius as u64)
            .max(self.buf_start);
        let cand_max = ideal + self.search_radius as u64;
        let need_end = (cand_max + self.win as u64).max(self.natural_next + self.hop_s as u64);
        self.ensure_frames(source, need_end);

        if ideal >= self.source_pulled && self.natural_next >= self.source_pulled {
            self.out_len = 0;
            self.phase = WsolaPhase::Done;
            return;
        }

        let safe_max = cand_max
            .min(self.source_pulled.saturating_sub(self.hop_s as u64))
            .max(cand_min);
        let best = if (ratio - 1.0).abs() < 1e-4
            && self.natural_next >= cand_min
            && self.natural_next <= safe_max
        {
            self.natural_next
        } else {
            self.best_candidate(cand_min, safe_max)
        };

        let emit = self
            .source_pulled
            .saturating_sub(best)
            .min(self.hop_s as u64) as usize;
        for i in 0..emit {
            let w = self.window[i];
            for ch in 0..self.channels {
                let idx = i * self.channels + ch;
                self.out_buf[idx] = self.tail_buf[idx] + self.sample_at(best + i as u64, ch) * w;
            }
        }
        for i in 0..self.hop_s {
            let w = self.window[self.hop_s + i];
            for ch in 0..self.channels {
                let idx = i * self.channels + ch;
                self.tail_buf[idx] = self.sample_at(best + self.hop_s as u64 + i as u64, ch) * w;
            }
        }

        self.natural_next = best + self.hop_s as u64;
        self.analysis_pos += (self.hop_s as f64) * ratio;
        let next_floor = self.analysis_pos.floor().max(0.0) as u64;
        let keep_from = self
            .natural_next
            .min(next_floor)
            .saturating_sub(self.search_radius as u64 + 16);
        self.discard_before(keep_from);

        self.out_len = emit;
        self.out_cursor = 0;
    }

    fn next_frame(
        &mut self,
        source: &mut dyn Source<Item = f32>,
        ratio: f64,
        finish_now: bool,
        out: &mut [f32],
    ) -> bool {
        if self.out_cursor < self.out_len {
            let base = self.out_cursor * self.channels;
            out[..self.channels].copy_from_slice(&self.out_buf[base..base + self.channels]);
            self.out_cursor += 1;
            return true;
        }

        match self.phase {
            WsolaPhase::Done => false,
            WsolaPhase::Draining { next_frame } => {
                if next_frame >= self.source_pulled {
                    self.phase = WsolaPhase::Done;
                    return false;
                }
                for (ch, slot) in out[..self.channels].iter_mut().enumerate() {
                    *slot = self.sample_at(next_frame, ch);
                }
                let next = next_frame + 1;
                if next >= self.source_pulled {
                    self.phase = WsolaPhase::Done;
                } else {
                    self.phase = WsolaPhase::Draining { next_frame: next };
                }
                true
            }
            WsolaPhase::Running => {
                self.synthesize_block(source, ratio, finish_now);
                if self.out_cursor < self.out_len {
                    let base = self.out_cursor * self.channels;
                    out[..self.channels].copy_from_slice(&self.out_buf[base..base + self.channels]);
                    self.out_cursor += 1;
                    true
                } else {
                    false
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rodio::buffer::SamplesBuffer;
    use std::num::NonZero;

    fn sine_source(sr: u32, hz: f32, frames: usize) -> Box<dyn Source<Item = f32> + Send> {
        let mut samples = Vec::with_capacity(frames * 2);
        for i in 0..frames {
            let s = (TAU * hz * i as f32 / sr as f32).sin() * 0.6;
            samples.push(s);
            samples.push(s * 0.5);
        }
        Box::new(SamplesBuffer::new(
            NonZero::new(2).unwrap(),
            NonZero::new(sr).unwrap(),
            samples,
        ))
    }

    /// 统计过零频率，用于检验保音高拉伸是否保持原音高。
    fn estimate_hz(mono: &[f32], sr: u32) -> f32 {
        let mut crossings = 0usize;
        for w in mono.windows(2) {
            if w[0] <= 0.0 && w[1] > 0.0 {
                crossings += 1;
            }
        }
        crossings as f32 * sr as f32 / mono.len().max(1) as f32
    }

    #[test]
    fn varispeed_stretches_smoothly_and_unwraps_without_discontinuity() {
        let sr = 16_000_u32;
        let source = sine_source(sr, 440.0, 32_000);
        let mut adapter = TempoAdapter::new(source, sr, 2, TempoMode::Varispeed, 0.98, 2_000);

        let mut left = Vec::new();
        let mut frame = [0.0_f32; 2];
        for _ in 0..4_000 {
            assert!(adapter.next_frame(&mut frame));
            left.push(frame[0]);
        }
        adapter.begin_release();
        while !adapter.is_ready_to_unwrap() {
            assert!(adapter.next_frame(&mut frame));
            left.push(frame[0]);
        }

        let boundary = left.len();
        let mut raw = adapter.into_inner();
        for _ in 0..512 {
            let l = raw.next().unwrap();
            let _r = raw.next().unwrap();
            left.push(l);
        }

        // 检查全程与解包接缝处不存在台阶爆音（440 Hz @16kHz 最大邻样本差约 0.11）。
        let max_jump = left[boundary.saturating_sub(16)..boundary + 16]
            .windows(2)
            .fold(0.0_f32, |acc, w| acc.max((w[1] - w[0]).abs()));
        assert!(max_jump < 0.16, "微变速解包边界出现爆音台阶：{max_jump:.4}");
    }

    #[test]
    fn wsola_preserves_pitch_at_8_percent_slowdown_and_unwraps_cleanly() {
        let sr = 24_000_u32;
        let input_frames = 48_000_usize; // 2 秒
        let source = sine_source(sr, 440.0, input_frames);
        let mut adapter = TempoAdapter::new(source, sr, 2, TempoMode::PitchPreserving, 0.92, 4_000);

        let mut left = Vec::new();
        let mut right = Vec::new();
        let mut frame = [0.0_f32; 2];

        // 先在 0.92× 稳定运行 1 秒（24000 输出帧），检验音高保持 440 Hz 且左右声道比严格 2:1。
        for _ in 0..24_000 {
            assert!(adapter.next_frame(&mut frame));
            left.push(frame[0]);
            right.push(frame[1]);
        }
        let stretched_hz = estimate_hz(&left[2_000..22_000], sr);
        assert!(
            (stretched_hz - 440.0).abs() < 8.0,
            "WSOLA 8% 降速应保持 440 Hz 原音高，实测 {stretched_hz:.1} Hz"
        );
        let max_lr_err = left
            .iter()
            .zip(right.iter())
            .fold(0.0_f32, |acc, (&l, &r)| acc.max((l * 0.5 - r).abs()));
        assert!(
            max_lr_err < 1e-4,
            "立体声联动 WSOLA 不得破坏声道相位与声像比，误差 {max_lr_err}"
        );

        // 触发回速并等待无缝解包回原始流。
        adapter.begin_release();
        while !adapter.is_ready_to_unwrap() {
            if !adapter.next_frame(&mut frame) {
                break;
            }
            left.push(frame[0]);
        }
        assert!(adapter.is_ready_to_unwrap(), "WSOLA 应在回速结束后解包");

        let boundary = left.len();
        let mut raw = adapter.into_inner();
        for _ in 0..256 {
            if let (Some(l), Some(_r)) = (raw.next(), raw.next()) {
                left.push(l);
            }
        }
        let max_jump = left[boundary.saturating_sub(16)..boundary + 16]
            .windows(2)
            .fold(0.0_f32, |acc, w| acc.max((w[1] - w[0]).abs()));
        assert!(max_jump < 0.12, "WSOLA 解包边界出现爆音台阶：{max_jump:.4}");
    }

    #[test]
    fn wsola_identity_ratio_is_transparent() {
        let sr = 16_000_u32;
        let frames = 8_000_usize;
        let source = sine_source(sr, 330.0, frames);
        let mut adapter = TempoAdapter::new(source, sr, 2, TempoMode::PitchPreserving, 1.0, 0);

        let mut frame = [0.0_f32; 2];
        let mut max_err = 0.0_f32;
        for i in 0..4_000 {
            assert!(adapter.next_frame(&mut frame));
            let expected = (TAU * 330.0 * i as f32 / sr as f32).sin() * 0.6;
            max_err = max_err.max((frame[0] - expected).abs());
        }
        assert!(
            max_err < 1e-4,
            "1.0× 倍率下 WSOLA 应近乎无损，误差 {max_err}"
        );
    }
}
