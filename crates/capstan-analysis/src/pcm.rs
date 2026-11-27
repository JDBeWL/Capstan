//! 流式 PCM 供给，完成同步解码、下混单声道、重采样到 22.05 kHz，并同步送入响度计。

use std::path::Path;

use audioadapter_buffers::direct::InterleavedSlice;
use capstan_audio::TrackDecoder;
use rubato::{
    Async, FixedAsync, Resampler, SincInterpolationParameters, SincInterpolationType,
    WindowFunction,
};

use crate::error::{AnalysisError, Result};
use crate::loudness::LoudnessMeter;
use crate::model::Cancel;
use crate::stft::ANALYSIS_RATE;

/// 每次解码与检查取消令牌的帧块大小。
const DECODE_CHUNK_FRAMES: usize = 2048;

/// 分析重采样使用的轻量 sinc 参数；仅服务于 STFT 特征提取，无需回放级 256 抽头。
const SINC_LEN: usize = 64;
const OVERSAMPLING: usize = 32;
const CUTOFF: f32 = 0.90;

/// 解码后产出的流式元数据。
#[derive(Clone, Debug)]
pub struct StreamSummary {
    pub duration: f32,
    pub loudness_lufs: f32,
    /// 每 100 ms 一格的立体声中置能量比 `Mid² / (Mid² + Side²)`；单声道素材恒为 1.0。
    pub center_ratio_100ms: Vec<f32>,
}

/// 打开音频文件并流式送出 22.05 kHz 单声道样本块，同时计算原始采样率下的 BS.1770 响度。
pub fn stream_file(
    path: &Path,
    cancel: &Cancel,
    on_mono_22k: impl FnMut(&[f32]),
) -> Result<StreamSummary> {
    let mut decoder = TrackDecoder::open(path)?;
    decoder.prefill()?;
    let sample_rate = decoder.sample_rate();
    let channels = decoder.channels().max(1);
    stream_interleaved(sample_rate, channels, decoder, cancel, on_mono_22k)
}

/// 从任意交错 f32 迭代器流式产出 22.05 kHz 单声道块并计算响度。
pub fn stream_interleaved<I>(
    sample_rate: u32,
    channels: u16,
    mut samples: I,
    cancel: &Cancel,
    mut on_mono_22k: impl FnMut(&[f32]),
) -> Result<StreamSummary>
where
    I: Iterator<Item = f32>,
{
    let ch = usize::from(channels.max(1));
    let inv_ch = 1.0 / ch as f32;
    let mut loudness = LoudnessMeter::new(sample_rate, channels);

    let mut resampler = if sample_rate == ANALYSIS_RATE {
        None
    } else {
        Some(MonoResampler::new(sample_rate, ANALYSIS_RATE)?)
    };

    let mut frame_buf = vec![0.0_f32; ch];
    let mut mono_buf = Vec::with_capacity(DECODE_CHUNK_FRAMES);
    let mut total_frames: u64 = 0;

    let bin_frames = (sample_rate / 10).max(1) as usize;
    let mut center_ratio_100ms = Vec::new();
    let mut bin_count = 0usize;
    let mut mid_sq = 0.0_f64;
    let mut side_sq = 0.0_f64;

    loop {
        if cancel.is_cancelled() {
            return Err(AnalysisError::Cancelled);
        }
        mono_buf.clear();

        let mut eof = false;
        for _ in 0..DECODE_CHUNK_FRAMES {
            let mut sum = 0.0_f32;
            let mut read = 0usize;
            for slot in &mut frame_buf {
                if let Some(sample) = samples.next() {
                    *slot = sample;
                    sum += sample;
                    read += 1;
                } else {
                    eof = true;
                    break;
                }
            }
            if read < ch {
                eof = true;
                break;
            }
            loudness.push_frame(&frame_buf);
            if ch >= 2 {
                let mid = 0.5 * f64::from(frame_buf[0] + frame_buf[1]);
                let side = 0.5 * f64::from(frame_buf[0] - frame_buf[1]);
                mid_sq += mid * mid;
                side_sq += side * side;
            }
            bin_count += 1;
            if bin_count >= bin_frames {
                center_ratio_100ms.push(finish_center_bin(ch, mid_sq, side_sq));
                bin_count = 0;
                mid_sq = 0.0;
                side_sq = 0.0;
            }
            mono_buf.push(sum * inv_ch);
            total_frames += 1;
        }

        if !mono_buf.is_empty() {
            match resampler.as_mut() {
                Some(r) => r.push(&mono_buf, &mut on_mono_22k)?,
                None => on_mono_22k(&mono_buf),
            }
        }

        if eof {
            break;
        }
    }

    if bin_count > 0 {
        center_ratio_100ms.push(finish_center_bin(ch, mid_sq, side_sq));
    }

    if let Some(r) = resampler.as_mut() {
        r.finish(&mut on_mono_22k)?;
    }

    let duration = total_frames as f32 / sample_rate.max(1) as f32;
    Ok(StreamSummary {
        duration,
        loudness_lufs: loudness.finish(),
        center_ratio_100ms,
    })
}

fn finish_center_bin(channels: usize, mid_sq: f64, side_sq: f64) -> f32 {
    if channels < 2 {
        return 1.0;
    }
    let total = mid_sq + side_sq;
    if total > 1e-12 {
        (mid_sq / total) as f32
    } else {
        0.0
    }
}

/// 单声道流式重采样器，将输入块重采样为固定 22.05 kHz 输出。
struct MonoResampler {
    resampler: Async<f32>,
    chunk: usize,
    ratio: f64,
    pending: Vec<f32>,
    in_buf: Vec<f32>,
    out: Vec<f32>,
    fed_frames: usize,
    out_frames: usize,
    flush_chunks: usize,
}

impl MonoResampler {
    fn new(source_rate: u32, target_rate: u32) -> Result<Self> {
        let chunk = match source_rate {
            0..=32_000 => 512,
            32_001..=64_000 => 1024,
            _ => 2048,
        };
        let params = SincInterpolationParameters::new(SINC_LEN, WindowFunction::BlackmanHarris2)
            .f_cutoff(CUTOFF)
            .interpolation(SincInterpolationType::Linear)
            .oversampling_factor(OVERSAMPLING);
        let ratio = f64::from(target_rate) / f64::from(source_rate.max(1));
        let resampler = Async::<f32>::new_sinc(ratio, 2.0, &params, chunk, 1, FixedAsync::Input)
            .map_err(|err| AnalysisError::Resampler(err.to_string()))?;
        let out_max = resampler.output_frames_max();
        let latency_in = (resampler.output_delay() as f64 / ratio).ceil() as usize;
        let flush_chunks = latency_in.div_ceil(chunk) + 1;

        Ok(Self {
            resampler,
            chunk,
            ratio,
            pending: Vec::with_capacity(chunk * 2),
            in_buf: vec![0.0_f32; chunk],
            out: vec![0.0_f32; out_max],
            fed_frames: 0,
            out_frames: 0,
            flush_chunks,
        })
    }

    fn push(&mut self, samples: &[f32], on_out: &mut impl FnMut(&[f32])) -> Result<()> {
        self.pending.extend_from_slice(samples);
        while self.pending.len() >= self.chunk {
            self.in_buf.copy_from_slice(&self.pending[..self.chunk]);
            self.pending.drain(..self.chunk);
            self.fed_frames += self.chunk;
            let written = self.process_in_buf()?;
            if written > 0 {
                self.out_frames += written;
                on_out(&self.out[..written]);
            }
        }
        Ok(())
    }

    fn finish(&mut self, on_out: &mut impl FnMut(&[f32])) -> Result<()> {
        if !self.pending.is_empty() {
            let valid = self.pending.len();
            self.fed_frames += valid;
            self.in_buf[..valid].copy_from_slice(&self.pending);
            self.in_buf[valid..].fill(0.0);
            self.pending.clear();
            let written = self.process_in_buf()?;
            self.emit_clamped(written, on_out);
        }

        self.in_buf.fill(0.0);
        for _ in 0..self.flush_chunks {
            let written = self.process_in_buf()?;
            if !self.emit_clamped(written, on_out) {
                break;
            }
        }
        Ok(())
    }

    fn emit_clamped(&mut self, written: usize, on_out: &mut impl FnMut(&[f32])) -> bool {
        let total = (self.fed_frames as f64 * self.ratio).ceil() as usize;
        let keep = total.saturating_sub(self.out_frames).min(written);
        if keep == 0 {
            return false;
        }
        self.out_frames += keep;
        on_out(&self.out[..keep]);
        true
    }

    fn process_in_buf(&mut self) -> Result<usize> {
        let input = InterleavedSlice::new(&self.in_buf, 1, self.chunk)
            .map_err(|err| AnalysisError::Resampler(err.to_string()))?;
        let out_len = self.out.len();
        let mut output = InterleavedSlice::new_mut(&mut self.out, 1, out_len)
            .map_err(|err| AnalysisError::Resampler(err.to_string()))?;
        let (_, written) = self
            .resampler
            .process_into_buffer(&input, &mut output, None)
            .map_err(|err| AnalysisError::Resampler(err.to_string()))?;
        Ok(written)
    }
}
