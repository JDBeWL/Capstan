//! 曲目结构分析，涵盖首尾静音、逐拍电平、三频彩色波形、人声存在度、乐句边界与前奏及尾奏范围。

use crate::key::CHROMA_STRIDE;
use crate::stft::{FRAME_RATE, center_frame_time};

/// 静音判定阈值，即 -60 dBFS。
const SILENCE_RMS: f32 = 1e-3;
/// 三频彩色波形包络的时间分辨率（秒）。100 ms 一格，4 分钟曲目约 7 KB。
pub const WAVEFORM_BIN_SECS: f32 = 0.1;
/// 人声存在度归一化底线，防止纯器乐曲目的底噪被拉伸为满格人声。
const VOCAL_NORM_FLOOR: f32 = 0.28;
/// 判定明显人声段的小节阈值（0..=1）。
const VOCAL_ACTIVE_THRESHOLD: f32 = 0.45;

pub struct StructureSummary {
    pub bar_energy: Vec<[u8; 3]>,
    pub bar_vocal: Vec<u8>,
    pub beat_rms: Vec<u8>,
    pub waveform_rgb: Vec<[u8; 3]>,
    pub phrase_bars: u8,
    pub phrase_offset_bars: u16,
    pub lead_silence: f32,
    pub tail_silence: f32,
    pub intro_end: f32,
    pub outro_start: f32,
    pub vocal_start: f32,
    pub vocal_end: f32,
}

#[allow(clippy::too_many_arguments)]
pub fn analyze(
    duration: f32,
    frame_rms: &[f32],
    band_energy: &[[f32; 3]],
    frame_vocal: &[f32],
    center_ratio_100ms: &[f32],
    chroma_frames: &[[f32; 12]],
    beats: &[f32],
    downbeat_phase: u8,
) -> StructureSummary {
    let (lead_silence, tail_silence) = detect_silence(duration, frame_rms);
    let beat_rms = compute_beat_rms(beats, duration, frame_rms);
    let waveform_rgb = compute_waveform_rgb(duration, band_energy);
    let (bar_energy, bar_chroma, bar_times) =
        compute_bars(beats, downbeat_phase, duration, band_energy, chroma_frames);
    let (phrase_bars, phrase_offset_bars) = detect_phrases(&bar_energy, &bar_chroma);
    let (intro_end, outro_start) = detect_intro_outro(
        duration,
        lead_silence,
        tail_silence,
        &bar_energy,
        &bar_times,
        phrase_bars,
        phrase_offset_bars,
    );
    let (bar_vocal, vocal_start, vocal_end) = compute_bar_vocal(
        beats,
        downbeat_phase,
        duration,
        frame_vocal,
        center_ratio_100ms,
        intro_end,
        outro_start,
    );

    StructureSummary {
        bar_energy,
        bar_vocal,
        beat_rms,
        waveform_rgb,
        phrase_bars,
        phrase_offset_bars,
        lead_silence,
        tail_silence,
        intro_end,
        outro_start,
        vocal_start,
        vocal_end,
    }
}

/// 按 100 ms 步长提取三频（低、中、高）彩色波形包络，各频带独立归一化至 0..=255。
fn compute_waveform_rgb(duration: f32, band_energy: &[[f32; 3]]) -> Vec<[u8; 3]> {
    if band_energy.is_empty() || duration <= 0.0 {
        return Vec::new();
    }
    let bins = ((duration / WAVEFORM_BIN_SECS).ceil() as usize).max(1);
    let mut raw = Vec::with_capacity(bins);
    for idx in 0..bins {
        let t0 = idx as f32 * WAVEFORM_BIN_SECS;
        let t1 = (t0 + WAVEFORM_BIN_SECS).min(duration);
        let f0 = time_to_frame(t0, band_energy.len());
        let f1 = time_to_frame(t1, band_energy.len())
            .max(f0 + 1)
            .min(band_energy.len());
        let mut peak = [0.0_f32; 3];
        for frame in &band_energy[f0..f1] {
            peak[0] = peak[0].max(frame[0]);
            peak[1] = peak[1].max(frame[1]);
            peak[2] = peak[2].max(frame[2]);
        }
        raw.push(peak);
    }

    let scales: [f32; 3] = std::array::from_fn(|b| {
        let vals: Vec<f32> = raw.iter().map(|e| e[b]).collect();
        percentile(&vals, 0.95).max(1e-6)
    });
    raw.into_iter()
        .map(|e| {
            [
                ((e[0] / scales[0]).clamp(0.0, 1.0) * 255.0).round() as u8,
                ((e[1] / scales[1]).clamp(0.0, 1.0) * 255.0).round() as u8,
                ((e[2] / scales[2]).clamp(0.0, 1.0) * 255.0).round() as u8,
            ]
        })
        .collect()
}

/// 结合人声频带谐波度与立体声中置占比，计算逐小节人声存在度及起止时刻。
fn compute_bar_vocal(
    beats: &[f32],
    downbeat_phase: u8,
    duration: f32,
    frame_vocal: &[f32],
    center_ratio_100ms: &[f32],
    intro_end: f32,
    outro_start: f32,
) -> (Vec<u8>, f32, f32) {
    let phase = usize::from(downbeat_phase % 4);
    if phase >= beats.len() || frame_vocal.is_empty() {
        return (Vec::new(), intro_end, outro_start);
    }

    let bar_starts: Vec<usize> = (phase..beats.len()).step_by(4).collect();
    let mut raw = Vec::with_capacity(bar_starts.len());
    let mut bar_spans = Vec::with_capacity(bar_starts.len());

    for (idx, &beat_i) in bar_starts.iter().enumerate() {
        let t0 = beats[beat_i];
        let t1 = bar_starts
            .get(idx + 1)
            .map_or(duration, |&next_b| beats[next_b])
            .max(t0);
        bar_spans.push((t0, t1));

        let f0 = time_to_frame(t0, frame_vocal.len());
        let f1 = time_to_frame(t1, frame_vocal.len())
            .max(f0 + 1)
            .min(frame_vocal.len());
        let vocal_mean = frame_vocal[f0..f1].iter().sum::<f32>() / (f1 - f0) as f32;

        let center_mean = if center_ratio_100ms.is_empty() {
            1.0
        } else {
            let c0 = ((t0 / WAVEFORM_BIN_SECS).floor() as usize).min(center_ratio_100ms.len() - 1);
            let c1 = ((t1 / WAVEFORM_BIN_SECS).ceil() as usize)
                .max(c0 + 1)
                .min(center_ratio_100ms.len());
            center_ratio_100ms[c0..c1].iter().sum::<f32>() / (c1 - c0) as f32
        };

        raw.push(vocal_mean * (0.40 + 0.60 * center_mean));
    }

    let scale = percentile(&raw, 0.95).max(VOCAL_NORM_FLOOR);
    let norm: Vec<f32> = raw
        .into_iter()
        .map(|v| (v / scale).clamp(0.0, 1.0))
        .collect();
    let quantized: Vec<u8> = norm.iter().map(|&v| (v * 255.0).round() as u8).collect();

    let start_cap = duration * 0.55;
    let vocal_start = bar_spans
        .iter()
        .enumerate()
        .find(|&(i, &(t0, _))| {
            t0 <= start_cap && window_mean(&norm, i, 2) >= VOCAL_ACTIVE_THRESHOLD
        })
        .map_or(intro_end, |(_, &(t0, _))| t0);

    let end_floor = duration * 0.45;
    let vocal_end = bar_spans
        .iter()
        .enumerate()
        .rev()
        .find(|&(i, &(_, t1))| t1 >= end_floor && norm[i] >= VOCAL_ACTIVE_THRESHOLD)
        .map_or(outro_start, |(_, &(_, t1))| t1)
        .max(vocal_start);

    (quantized, vocal_start, vocal_end)
}

/// 扫描首尾静音，误差在一帧以内（约 11.6 ms）。
fn detect_silence(duration: f32, frame_rms: &[f32]) -> (f32, f32) {
    if frame_rms.is_empty() || duration <= 0.0 {
        return (0.0, 0.0);
    }
    let first = frame_rms.iter().position(|&r| r >= SILENCE_RMS);
    let last = frame_rms.iter().rposition(|&r| r >= SILENCE_RMS);

    let (Some(first), Some(last)) = (first, last) else {
        return (duration, 0.0);
    };

    let lead = if first == 0 {
        0.0
    } else {
        center_frame_time(first).clamp(0.0, duration)
    };
    let tail_end = center_frame_time(last).clamp(0.0, duration);
    let tail = if last + 1 >= frame_rms.len() {
        0.0
    } else {
        (duration - tail_end).max(0.0)
    };
    (lead, tail)
}

/// 统计每一拍内的平均 RMS，按第 95 百分位归一化并量化到 0..=255。
fn compute_beat_rms(beats: &[f32], duration: f32, frame_rms: &[f32]) -> Vec<u8> {
    if beats.is_empty() || frame_rms.is_empty() {
        return Vec::new();
    }
    let mut raw = Vec::with_capacity(beats.len());
    for i in 0..beats.len() {
        let t0 = beats[i];
        let t1 = beats.get(i + 1).copied().unwrap_or(duration).max(t0);
        let f0 = time_to_frame(t0, frame_rms.len());
        let f1 = time_to_frame(t1, frame_rms.len())
            .max(f0 + 1)
            .min(frame_rms.len());
        let mean = frame_rms[f0..f1].iter().sum::<f32>() / (f1 - f0) as f32;
        raw.push(mean);
    }

    let scale = percentile(&raw, 0.95).max(1e-6);
    raw.into_iter()
        .map(|v| ((v / scale).clamp(0.0, 1.0) * 255.0).round() as u8)
        .collect()
}

/// 计算每个 4 拍小节的三频带能量、色度均值与起止时刻。
fn compute_bars(
    beats: &[f32],
    downbeat_phase: u8,
    duration: f32,
    band_energy: &[[f32; 3]],
    chroma_frames: &[[f32; 12]],
) -> (Vec<[u8; 3]>, Vec<[f32; 12]>, Vec<f32>) {
    let phase = usize::from(downbeat_phase % 4);
    if phase >= beats.len() || band_energy.is_empty() {
        return (Vec::new(), Vec::new(), Vec::new());
    }

    let bar_starts: Vec<usize> = (phase..beats.len()).step_by(4).collect();
    let mut raw_energy: Vec<[f32; 3]> = Vec::with_capacity(bar_starts.len());
    let mut bar_chroma: Vec<[f32; 12]> = Vec::with_capacity(bar_starts.len());
    let mut bar_times: Vec<f32> = Vec::with_capacity(bar_starts.len());

    for (idx, &beat_i) in bar_starts.iter().enumerate() {
        let t0 = beats[beat_i];
        let t1 = bar_starts
            .get(idx + 1)
            .map_or(duration, |&next_b| beats[next_b])
            .max(t0);
        bar_times.push(t0);

        let f0 = time_to_frame(t0, band_energy.len());
        let f1 = time_to_frame(t1, band_energy.len())
            .max(f0 + 1)
            .min(band_energy.len());
        let inv = 1.0 / (f1 - f0) as f32;
        let mut e = [0.0_f32; 3];
        for frame in &band_energy[f0..f1] {
            e[0] += frame[0];
            e[1] += frame[1];
            e[2] += frame[2];
        }
        raw_energy.push([e[0] * inv, e[1] * inv, e[2] * inv]);

        let c0 = (f0 / CHROMA_STRIDE).min(chroma_frames.len());
        let c1 = (f1.div_ceil(CHROMA_STRIDE))
            .max(c0 + 1)
            .min(chroma_frames.len());
        let mut ch = [0.0_f32; 12];
        if c0 < c1 {
            let cinv = 1.0 / (c1 - c0) as f32;
            for cf in &chroma_frames[c0..c1] {
                for (dst, &src) in ch.iter_mut().zip(cf.iter()) {
                    *dst += src * cinv;
                }
            }
        }
        bar_chroma.push(ch);
    }

    // 三个频带分别按各自 95 百分位归一化，确保高低频在同一量化尺度上可比。
    let scales: [f32; 3] = std::array::from_fn(|b| {
        let vals: Vec<f32> = raw_energy.iter().map(|e| e[b]).collect();
        percentile(&vals, 0.95).max(1e-6)
    });

    let quantized = raw_energy
        .into_iter()
        .map(|e| {
            [
                ((e[0] / scales[0]).clamp(0.0, 1.0) * 255.0).round() as u8,
                ((e[1] / scales[1]).clamp(0.0, 1.0) * 255.0).round() as u8,
                ((e[2] / scales[2]).clamp(0.0, 1.0) * 255.0).round() as u8,
            ]
        })
        .collect();

    (quantized, bar_chroma, bar_times)
}

/// 计算小节边界的新奇度，在 8 小节与 16 小节两种周期及全部相位中挑出边界最鲜明的一组。
fn detect_phrases(bar_energy: &[[u8; 3]], bar_chroma: &[[f32; 12]]) -> (u8, u16) {
    let n = bar_energy.len();
    if n < 12 {
        return (8, 0);
    }

    let mut novelty = vec![0.0_f32; n];
    for b in 2..n.saturating_sub(2) {
        let win = 4.min(b).min(n - b);
        let prev_e = mean_bar_energy(&bar_energy[b - win..b]);
        let next_e = mean_bar_energy(&bar_energy[b..b + win]);
        let edist = ((next_e[0] - prev_e[0]).powi(2)
            + (next_e[1] - prev_e[1]).powi(2)
            + (next_e[2] - prev_e[2]).powi(2))
        .sqrt();

        let prev_c = mean_bar_chroma(&bar_chroma[b - win..b]);
        let next_c = mean_bar_chroma(&bar_chroma[b..b + win]);
        let cdist: f32 = prev_c
            .iter()
            .zip(next_c.iter())
            .map(|(a, b)| (a - b).powi(2))
            .sum::<f32>()
            .sqrt();

        novelty[b] = edist + 0.6 * cdist;
    }

    let mut best = (8u8, 0u16, f32::NEG_INFINITY);
    let periods: &[usize] = if n >= 32 { &[8, 16] } else { &[8] };

    for &period in periods {
        for offset in 0..period {
            let mut sum = 0.0_f32;
            let mut count = 0usize;
            let mut b = offset;
            while b < n {
                sum += novelty[b];
                count += 1;
                b += period;
            }
            let avg = if count > 0 { sum / count as f32 } else { 0.0 };
            // 16 小节边界数减半，略微加权避免样本更少造成虚高。
            let score = if period == 16 { avg * 0.96 } else { avg };
            if score > best.2 {
                best = (period as u8, offset as u16, score);
            }
        }
    }

    (best.0, best.1)
}

/// 定位前奏结束与尾奏开始。按小节能量中位数的比例判定，并落到最近的乐句边界上。
fn detect_intro_outro(
    duration: f32,
    lead_silence: f32,
    tail_silence: f32,
    bar_energy: &[[u8; 3]],
    bar_times: &[f32],
    phrase_bars: u8,
    phrase_offset_bars: u16,
) -> (f32, f32) {
    let audible_end = (duration - tail_silence).max(lead_silence);
    if bar_energy.is_empty() || bar_times.is_empty() {
        return (lead_silence, audible_end);
    }

    let totals: Vec<f32> = bar_energy
        .iter()
        .map(|&[lo, mid, hi]| (f32::from(lo) + f32::from(mid) + f32::from(hi)) / (3.0 * 255.0))
        .collect();
    let median = percentile(&totals, 0.50);

    let step = usize::from(phrase_bars.max(1));
    let offset = usize::from(phrase_offset_bars);
    let phrase_idx: Vec<usize> = (offset..bar_times.len()).step_by(step).collect();

    // 前奏结束取第一个使后续能量持续达到中位数 65% 的乐句边界，最晚不超过前 35%。
    let intro_cap = duration * 0.35;
    let intro_end = phrase_idx
        .iter()
        .copied()
        .find(|&b| {
            let t = bar_times[b];
            t >= lead_silence && t <= intro_cap && window_mean(&totals, b, 4) >= median * 0.65
        })
        .map_or(lead_silence, |b| bar_times[b]);

    // 尾奏开始取从末尾往回找最后一个能量维持在中位数 70% 以上的段之后那个乐句边界。
    let outro_floor = duration * 0.60;
    let outro_start = phrase_idx
        .iter()
        .rev()
        .copied()
        .find(|&b| {
            let t = bar_times[b];
            t >= outro_floor && t < audible_end && window_mean(&totals, b, 4) < median * 0.70
        })
        .or_else(|| {
            // 能量全程偏高时退回最接近末尾的那个乐句边界。
            phrase_idx
                .iter()
                .rev()
                .copied()
                .find(|&b| bar_times[b] >= outro_floor && bar_times[b] < audible_end - 4.0)
        })
        .map_or(audible_end, |b| bar_times[b]);

    (intro_end, outro_start.max(intro_end))
}

fn window_mean(values: &[f32], start: usize, len: usize) -> f32 {
    let end = (start + len).min(values.len());
    if start >= end {
        return 0.0;
    }
    values[start..end].iter().sum::<f32>() / (end - start) as f32
}

fn mean_bar_energy(slice: &[[u8; 3]]) -> [f32; 3] {
    if slice.is_empty() {
        return [0.0; 3];
    }
    let inv = 1.0 / (slice.len() as f32 * 255.0);
    let mut acc = [0.0_f32; 3];
    for &[a, b, c] in slice {
        acc[0] += f32::from(a);
        acc[1] += f32::from(b);
        acc[2] += f32::from(c);
    }
    [acc[0] * inv, acc[1] * inv, acc[2] * inv]
}

fn mean_bar_chroma(slice: &[[f32; 12]]) -> [f32; 12] {
    if slice.is_empty() {
        return [0.0; 12];
    }
    let inv = 1.0 / slice.len() as f32;
    let mut acc = [0.0_f32; 12];
    for frame in slice {
        for (dst, &src) in acc.iter_mut().zip(frame.iter()) {
            *dst += src * inv;
        }
    }
    acc
}

fn time_to_frame(time: f32, len: usize) -> usize {
    if time <= 0.0 || len == 0 {
        return 0;
    }
    ((time * FRAME_RATE).round() as usize).min(len - 1)
}

fn percentile(values: &[f32], q: f32) -> f32 {
    if values.is_empty() {
        return 0.0;
    }
    let mut sorted = values.to_vec();
    sorted.sort_by(f32::total_cmp);
    let idx = ((sorted.len() - 1) as f32 * q.clamp(0.0, 1.0)).round() as usize;
    sorted[idx]
}
