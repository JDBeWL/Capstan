//! 速度估计（BPM）、置信度与滑窗速度稳定性。
//! 利用 FFT 求瞬态包络自相关，配合谐波梳、对数高斯速度先验与标签先验消除八度歧义。

use rustfft::FftPlanner;
use rustfft::num_complex::Complex;

use crate::stft::FRAME_RATE;

/// 允许估计的速度区间，单位 BPM。
pub const MIN_BPM: f32 = 60.0;
pub const MAX_BPM: f32 = 200.0;

/// 速度先验的中心与宽度（八度）。中心略高于 120 令 174 略优于 87、90 略优于 180。
const PRIOR_CENTER_BPM: f32 = 125.0;
const PRIOR_SIGMA_OCTAVES: f32 = 1.1;

/// 局部稳定性分析的滑窗与步长，单位秒。
const WINDOW_SECS: f32 = 8.0;
const WINDOW_HOP_SECS: f32 = 4.0;

/// 速度估计结果。
#[derive(Clone, Debug)]
pub struct TempoEstimate {
    pub bpm: f32,
    pub confidence: f32,
    pub stability: f32,
    /// 前三个候选速度及其得分，供诊断示例打印。
    pub candidates: Vec<(f32, f32)>,
}

/// 估计全曲速度、置信度与速度稳定性。
pub fn estimate(onset: &[f32], bpm_tag: Option<f32>) -> TempoEstimate {
    if onset.len() < 128 {
        return TempoEstimate {
            bpm: 120.0,
            confidence: 0.0,
            stability: 0.0,
            candidates: Vec::new(),
        };
    }

    // 轻度对数压缩，避免 4/4 重音拍把四倍周期抬高到压过真实节拍周期。
    let compressed: Vec<f32> = onset
        .iter()
        .map(|&v| (1.0 + 2.0 * v.max(0.0)).ln())
        .collect();
    let max_lag = ((60.0 * FRAME_RATE / MIN_BPM) * 8.5).ceil() as usize;
    let acf = autocorrelation(&compressed, max_lag.min(compressed.len() / 2));

    let (candidates, raw_peak, median_score) = search_candidates(&acf, bpm_tag);
    let best_bpm = candidates.first().map_or(120.0, |&(bpm, _)| bpm);
    let refined_bpm = refine_bpm(&acf, best_bpm);

    // 置信度看基频与谐波自相关强度，以及最高峰相对中位背景的突出程度。
    let base_lag = 60.0 * FRAME_RATE / refined_bpm;
    let comb_norm = (comb_score(&acf, base_lag) / 1.6).clamp(0.0, 1.0);
    let background = median_score.max(0.0);
    let peak_above = (raw_peak - background).max(0.0);
    let contrast = (peak_above / (raw_peak.max(0.0) + 0.20)).clamp(0.0, 1.0);
    let confidence = (0.60 * comb_norm + 0.40 * contrast).clamp(0.0, 1.0);

    let stability = estimate_stability(&compressed, refined_bpm, confidence);

    TempoEstimate {
        bpm: refined_bpm,
        confidence,
        stability,
        candidates,
    }
}

/// 在 60–200 BPM 范围内按 0.25 BPM 网格扫描谐波梳得分，返回前三个局部极大值候选。
fn search_candidates(acf: &[f32], bpm_tag: Option<f32>) -> (Vec<(f32, f32)>, f32, f32) {
    let steps = ((MAX_BPM - MIN_BPM) * 4.0) as usize + 1;
    let mut scores = Vec::with_capacity(steps);

    for i in 0..steps {
        let bpm = MIN_BPM + i as f32 * 0.25;
        let period = 60.0 * FRAME_RATE / bpm;
        let comb = comb_score(acf, period);
        let prior = tempo_prior(bpm);
        let tag_boost = tag_prior(bpm, bpm_tag);
        scores.push((bpm, comb * prior * tag_boost));
    }

    let mut sorted_vals: Vec<f32> = scores.iter().map(|&(_, s)| s).collect();
    sorted_vals.sort_by(f32::total_cmp);
    let median = sorted_vals
        .get(sorted_vals.len() / 2)
        .copied()
        .unwrap_or(0.0);

    let mut peaks: Vec<(f32, f32)> = Vec::new();
    for i in 1..scores.len().saturating_sub(1) {
        let s = scores[i].1;
        if s >= scores[i - 1].1 && s > scores[i + 1].1 {
            peaks.push(scores[i]);
        }
    }
    if peaks.is_empty()
        && let Some(&best) = scores.iter().max_by(|a, b| a.1.total_cmp(&b.1))
    {
        peaks.push(best);
    }

    peaks.sort_by(|a, b| b.1.total_cmp(&a.1));

    // 去重，与已选候选偏差在 3% 以内的邻峰不重复列出。
    let mut top: Vec<(f32, f32)> = Vec::new();
    for (bpm, score) in peaks {
        let near = top
            .iter()
            .any(|&(kept, _)| ((bpm - kept) / kept).abs() < 0.03);
        if !near {
            top.push((bpm, score));
            if top.len() >= 3 {
                break;
            }
        }
    }

    let best_score = top.first().map_or(0.0, |&(_, s)| s);
    (top, best_score, median)
}

/// 围绕粗候选在 ±1.5 BPM 内按 0.02 BPM 步长精修，优先利用 1×、2×、4×、8× 周期。
fn refine_bpm(acf: &[f32], coarse: f32) -> f32 {
    let lo = (coarse - 1.5).max(MIN_BPM);
    let hi = (coarse + 1.5).min(MAX_BPM);
    let mut best_bpm = coarse;
    let mut best_val = f32::NEG_INFINITY;

    let mut bpm = lo;
    while bpm <= hi + 1e-4 {
        let period = 60.0 * FRAME_RATE / bpm;
        let val = comb_score(acf, period);
        if val > best_val {
            best_val = val;
            best_bpm = bpm;
        }
        bpm += 0.02;
    }
    best_bpm
}

/// 多阶谐波梳得分。同时计入 1×、2×、3×、4×、8× 周期，使真节拍与四拍小节周期相互增强。
fn comb_score(acf: &[f32], period: f32) -> f32 {
    let c1 = sample_acf(acf, period);
    let c2 = sample_acf(acf, period * 2.0);
    let c3 = sample_acf(acf, period * 3.0);
    let c4 = sample_acf(acf, period * 4.0);
    let c8 = sample_acf(acf, period * 8.0);
    1.0 * c1 + 0.55 * c2 + 0.25 * c3 + 0.35 * c4 + 0.20 * c8
}

/// 线性插值读取非整数滞后的自相关值。
fn sample_acf(acf: &[f32], lag: f32) -> f32 {
    if lag < 1.0 || lag >= (acf.len().saturating_sub(1)) as f32 {
        return 0.0;
    }
    let idx = lag.floor() as usize;
    let frac = lag - idx as f32;
    let a = acf[idx];
    let b = acf.get(idx + 1).copied().unwrap_or(a);
    a + (b - a) * frac
}

/// 对数高斯速度先验。
fn tempo_prior(bpm: f32) -> f32 {
    let octaves = (bpm / PRIOR_CENTER_BPM).log2();
    (-0.5 * (octaves / PRIOR_SIGMA_OCTAVES).powi(2)).exp()
}

/// 标签 BPM 先验。直命中（±4%）给予强增益，倍频或半频命中给予温和增益。
fn tag_prior(bpm: f32, tag: Option<f32>) -> f32 {
    let Some(t) = tag else {
        return 1.0;
    };
    if !t.is_finite() || t <= 0.0 {
        return 1.0;
    }
    let rel = |target: f32| ((bpm - target) / target).abs();
    if rel(t) <= 0.04 {
        1.75
    } else if rel(t * 2.0) <= 0.04 || rel(t * 0.5) <= 0.04 {
        1.10
    } else {
        1.0
    }
}

/// 8 秒滑窗估计局部速度并按变异系数衡量稳定性。
fn estimate_stability(signal: &[f32], global_bpm: f32, fallback: f32) -> f32 {
    let win = (WINDOW_SECS * FRAME_RATE).round() as usize;
    let hop = (WINDOW_HOP_SECS * FRAME_RATE).round() as usize;
    if signal.len() < win + hop {
        return fallback;
    }

    let max_lag = ((60.0 * FRAME_RATE / MIN_BPM) * 4.5).ceil() as usize;
    let mut local_bpms = Vec::new();

    let mut start = 0;
    while start + win <= signal.len() {
        let slice = &signal[start..start + win];
        let energy: f32 = slice.iter().map(|v| v * v).sum::<f32>() / slice.len() as f32;
        if energy > 1e-4 {
            let acf = autocorrelation(slice, max_lag.min(slice.len() / 2));
            let (cands, _, _) = search_candidates(&acf, None);
            if let Some(&(mut bpm, _)) = cands.first() {
                // 折算到与全局速度最接近的八度，避免单个窗口八度跳变误伤稳定性。
                let candidates = [bpm, bpm * 2.0, bpm * 0.5];
                bpm = candidates
                    .into_iter()
                    .min_by(|a, b| (a - global_bpm).abs().total_cmp(&(b - global_bpm).abs()))
                    .unwrap_or(bpm);
                local_bpms.push(bpm);
            }
        }
        start += hop;
    }

    if local_bpms.len() < 2 {
        return fallback;
    }

    let mean = local_bpms.iter().sum::<f32>() / local_bpms.len() as f32;
    if mean <= 1.0 {
        return 0.0;
    }
    let var = local_bpms.iter().map(|&b| (b - mean).powi(2)).sum::<f32>() / local_bpms.len() as f32;
    let cv = var.sqrt() / mean;
    // 变异系数 0 表示完全稳速（1.0）；≥ 6%（如明显渐快或渐慢）降至接近 0。
    (1.0 - cv / 0.06).clamp(0.0, 1.0)
}

/// 经 FFT 计算去均值后的归一化自相关，返回 `0..=max_lag`。
fn autocorrelation(signal: &[f32], max_lag: usize) -> Vec<f32> {
    let n = signal.len();
    if n < 4 || max_lag == 0 {
        return vec![0.0; max_lag + 1];
    }

    let mean = signal.iter().copied().map(f64::from).sum::<f64>() / n as f64;
    let fft_len = (n * 2).next_power_of_two();
    let mut buf: Vec<Complex<f32>> = Vec::with_capacity(fft_len);
    for &s in signal {
        buf.push(Complex::new((f64::from(s) - mean) as f32, 0.0));
    }
    buf.resize(fft_len, Complex::new(0.0, 0.0));

    let mut planner = FftPlanner::<f32>::new();
    let forward = planner.plan_fft_forward(fft_len);
    let inverse = planner.plan_fft_inverse(fft_len);
    forward.process(&mut buf);
    for c in &mut buf {
        let mag2 = c.re * c.re + c.im * c.im;
        *c = Complex::new(mag2, 0.0);
    }
    inverse.process(&mut buf);

    let zero = buf[0].re;
    if zero <= 1e-9 {
        return vec![0.0; max_lag + 1];
    }
    let inv_zero = 1.0 / zero;

    (0..=max_lag)
        .map(|lag| {
            // 无偏缩放，避免长滞后因重叠帧数减少而被系统性压低。
            let scale = n as f32 / (n - lag).max(1) as f32;
            (buf[lag].re * inv_zero * scale).clamp(-1.0, 1.0)
        })
        .collect()
}
