//! 动态规划节拍跟踪（Ellis 2007）、亚帧峰值细化与 4/4 小节线相位判定。

use crate::stft::{FRAME_RATE, onset_frame_time};

/// 节拍间隔偏离惩罚权重。与归一化到单位标准差的瞬态包络配套。
const DP_ALPHA: f32 = 100.0;

/// 跟踪全曲节拍并返回 `(节拍时刻序列, 小节线相位 0..4)`。
pub fn track_beats(
    onset: &[f32],
    low_onset: &[f32],
    band_energy: &[[f32; 3]],
    bpm: f32,
) -> (Vec<f32>, u8) {
    let n = onset.len();
    if n < 16 || !bpm.is_finite() || bpm <= 1.0 {
        return (Vec::new(), 0);
    }

    let period = 60.0 * FRAME_RATE / bpm;
    let normalized = normalize_onset(onset);
    let smoothed = gaussian_smooth(&normalized, (0.03 * period).clamp(0.8, 2.5));

    let min_step = ((0.5 * period).round() as usize).max(2);
    let max_step = ((2.0 * period).round() as usize).max(min_step + 1);

    // 预计算间隔惩罚表，内层循环免去重复 `ln()`。
    let penalties: Vec<f32> = (0..=max_step)
        .map(|dt| {
            if dt == 0 {
                f32::NEG_INFINITY
            } else {
                let ratio = (dt as f32 / period).ln();
                -DP_ALPHA * ratio * ratio
            }
        })
        .collect();

    let mut score = vec![0.0_f32; n];
    let mut back = vec![usize::MAX; n];

    for t in 0..n {
        let local = smoothed[t];
        let mut best_prev = f32::NEG_INFINITY;
        let mut best_idx = usize::MAX;

        if t >= min_step {
            let lo = t.saturating_sub(max_step);
            let hi = t - min_step;
            for tau in lo..=hi {
                let candidate = score[tau] + penalties[t - tau];
                if candidate > best_prev {
                    best_prev = candidate;
                    best_idx = tau;
                }
            }
        }

        if best_idx != usize::MAX && best_prev > 0.0 {
            score[t] = local + best_prev;
            back[t] = best_idx;
        } else {
            score[t] = local;
        }
    }

    // 回溯终点落在末尾两个拍长窗口内得分最高处。
    let tail_window = ((2.0 * period).ceil() as usize).min(n);
    let mut cur = (n - tail_window..n)
        .max_by(|&a, &b| score[a].total_cmp(&score[b]))
        .unwrap_or(n - 1);

    let mut frames = Vec::new();
    while cur != usize::MAX {
        frames.push(cur);
        cur = back[cur];
    }
    frames.reverse();

    let beats: Vec<f32> = frames.iter().map(|&f| refine_beat_time(onset, f)).collect();
    let downbeat_phase = detect_downbeat_phase(&frames, low_onset, band_energy);

    (beats, downbeat_phase)
}

/// 抛物线插值将离散帧索引细化到亚帧级时间戳。
fn refine_beat_time(onset: &[f32], frame: usize) -> f32 {
    let base = onset_frame_time(frame);
    if frame == 0 || frame + 1 >= onset.len() {
        return base;
    }
    let y0 = onset[frame - 1];
    let y1 = onset[frame];
    let y2 = onset[frame + 1];
    let denom = y0 - 2.0 * y1 + y2;
    let delta = if denom.abs() > 1e-6 && y1 >= y0 && y1 >= y2 {
        (0.5 * (y0 - y2) / denom).clamp(-0.5, 0.5)
    } else {
        0.0
    };
    (base + delta / FRAME_RATE).max(0.0)
}

/// 按 4/4 拍比较 4 种相位；小节第一拍通常兼具低频重音与和声及能量跃迁。
fn detect_downbeat_phase(frames: &[usize], low_onset: &[f32], band_energy: &[[f32; 3]]) -> u8 {
    if frames.len() < 4 {
        return 0;
    }

    let mut best_phase = 0u8;
    let mut best_score = f32::NEG_INFINITY;

    for phase in 0..4usize {
        let mut sum = 0.0_f32;
        let mut count = 0usize;
        let mut idx = phase;
        while idx < frames.len() {
            let f = frames[idx];
            // 取节拍 ±1 帧内的低频峰值，吸收轻微相位抖动。
            let lo = f.saturating_sub(1);
            let hi = (f + 1).min(low_onset.len().saturating_sub(1));
            let low_peak = low_onset[lo..=hi].iter().copied().fold(0.0_f32, f32::max);

            // 拍后一小段与拍前一小段的频带能量差分，反映和声与配器换段。
            let before = mean_band(band_energy, f.saturating_sub(8), f);
            let after = mean_band(band_energy, f, (f + 8).min(band_energy.len()));
            let novelty = (after[0] - before[0]).max(0.0)
                + (after[1] - before[1]).max(0.0)
                + (after[2] - before[2]).max(0.0);

            sum += low_peak * 1.4 + novelty * 0.6;
            count += 1;
            idx += 4;
        }
        let avg = if count > 0 { sum / count as f32 } else { 0.0 };
        if avg > best_score {
            best_score = avg;
            best_phase = phase as u8;
        }
    }

    best_phase
}

fn mean_band(band_energy: &[[f32; 3]], start: usize, end: usize) -> [f32; 3] {
    if start >= end || end > band_energy.len() {
        return [0.0; 3];
    }
    let mut acc = [0.0_f32; 3];
    for e in &band_energy[start..end] {
        acc[0] += e[0];
        acc[1] += e[1];
        acc[2] += e[2];
    }
    let inv = 1.0 / (end - start) as f32;
    [acc[0] * inv, acc[1] * inv, acc[2] * inv]
}

fn normalize_onset(onset: &[f32]) -> Vec<f32> {
    let n = onset.len() as f32;
    let mean = onset.iter().sum::<f32>() / n;
    let var = onset.iter().map(|&v| (v - mean).powi(2)).sum::<f32>() / n;
    let std = var.sqrt();
    if std <= 1e-7 {
        return onset.to_vec();
    }
    onset.iter().map(|&v| v / std).collect()
}

fn gaussian_smooth(values: &[f32], sigma: f32) -> Vec<f32> {
    let radius = (sigma * 3.0).ceil() as usize;
    if radius == 0 || values.is_empty() {
        return values.to_vec();
    }
    let mut kernel = Vec::with_capacity(radius * 2 + 1);
    let mut norm = 0.0_f32;
    for j in 0..=(radius * 2) {
        let dist = j as f32 - radius as f32;
        let w = (-0.5 * (dist / sigma).powi(2)).exp();
        kernel.push(w);
        norm += w;
    }
    for w in &mut kernel {
        *w /= norm;
    }

    let n = values.len();
    let mut out = vec![0.0_f32; n];
    for (i, dst) in out.iter_mut().enumerate() {
        let mut acc = 0.0_f32;
        let start = i.saturating_sub(radius);
        let end = (i + radius + 1).min(n);
        for (pos, &val) in values.iter().enumerate().take(end).skip(start) {
            let k = pos + radius - i;
            acc += val * kernel[k];
        }
        *dst = acc;
    }
    out
}
