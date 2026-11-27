//! 三频带对数压缩谱通量与逐帧频带能量提取。

use crate::stft::SPECTRUM_BINS;

/// 对数压缩系数。弱音与强音瞬态都保持可见，避免极强单音压死其余节拍。
const LOG_GAMMA: f32 = 100.0;

/// 低频带（底鼓与贝斯，约 22–151 Hz）。
const LOW_START: usize = 1;
const LOW_END: usize = 8;
/// 中频带（军鼓、人声与主旋律，约 172–2002 Hz）。
const MID_END: usize = 94;
/// 人声核心频带（约 301–3424 Hz，@22.05 kHz / 1024 FFT）。
const VOCAL_START: usize = 14;
const VOCAL_END: usize = 159;

/// 局部均值减去的半窗长，约 ±116 ms。
const LOCAL_MEAN_RADIUS: usize = 10;

/// 逐帧提取的瞬态与能量序列。
pub struct OnsetFeatures {
    /// 三频带加权并去局部均值后的瞬态包络。
    pub onset: Vec<f32>,
    /// 低频瞬态包络，用于小节线重音判定。
    pub low_onset: Vec<f32>,
    /// 每帧低、中、高三频带的线性均方根能量。
    pub band_energy: Vec<[f32; 3]>,
    /// 每帧未加窗时域 RMS。
    pub frame_rms: Vec<f32>,
    /// 每帧人声频带占比与谐波度综合得分（0..=1）。
    pub frame_vocal: Vec<f32>,
}

/// 逐帧累加器，流式消费 STFT 幅度谱，最终产出 [`OnsetFeatures`]。
pub struct OnsetCollector {
    prev_log: Vec<f32>,
    raw_combined: Vec<f32>,
    raw_low: Vec<f32>,
    band_energy: Vec<[f32; 3]>,
    frame_rms: Vec<f32>,
    frame_vocal: Vec<f32>,
}

impl OnsetCollector {
    pub fn new() -> Self {
        Self {
            prev_log: vec![0.0; SPECTRUM_BINS],
            raw_combined: Vec::new(),
            raw_low: Vec::new(),
            band_energy: Vec::new(),
            frame_rms: Vec::new(),
            frame_vocal: Vec::new(),
        }
    }

    pub fn push_frame(&mut self, magnitudes: &[f32], rms: f32) {
        let mut low_flux = 0.0_f32;
        let mut mid_flux = 0.0_f32;
        let mut high_flux = 0.0_f32;

        let mut low_e = 0.0_f32;
        let mut mid_e = 0.0_f32;
        let mut high_e = 0.0_f32;
        let mut vocal_e = 0.0_f32;
        let mut vocal_log_sum = 0.0_f32;
        let mut total_e = 0.0_f32;

        for (bin, &mag) in magnitudes.iter().enumerate().take(SPECTRUM_BINS) {
            let compressed = (1.0 + LOG_GAMMA * mag).ln();
            let diff = (compressed - self.prev_log[bin]).max(0.0);
            self.prev_log[bin] = compressed;
            let power = mag * mag;

            if bin >= LOW_START {
                total_e += power;
            }
            if (VOCAL_START..VOCAL_END).contains(&bin) {
                vocal_e += power;
                vocal_log_sum += (power + 1e-12).ln();
            }

            if (LOW_START..LOW_END).contains(&bin) {
                low_flux += diff;
                low_e += power;
            } else if (LOW_END..MID_END).contains(&bin) {
                mid_flux += diff;
                mid_e += power;
            } else if bin >= MID_END {
                high_flux += diff;
                high_e += power;
            }
        }

        let low_bins = (LOW_END - LOW_START) as f32;
        let mid_bins = (MID_END - LOW_END) as f32;
        let high_bins = (SPECTRUM_BINS - MID_END) as f32;
        let vocal_bins = (VOCAL_END - VOCAL_START) as f32;

        let low_mean = low_flux / low_bins;
        let mid_mean = mid_flux / mid_bins;
        let high_mean = high_flux / high_bins;

        let vocal_mean_p = vocal_e / vocal_bins;
        let flatness = if vocal_mean_p > 1e-10 {
            ((vocal_log_sum / vocal_bins).exp() / vocal_mean_p).clamp(0.0, 1.0)
        } else {
            1.0
        };
        let tonality = (1.0 - flatness).clamp(0.0, 1.0);
        let vocal_share = (vocal_e / (total_e + 1e-12)).clamp(0.0, 1.0);

        self.raw_low.push(low_mean);
        self.raw_combined
            .push(0.50 * low_mean + 0.30 * mid_mean + 0.20 * high_mean);
        self.band_energy.push([
            (low_e / low_bins).sqrt(),
            (mid_e / mid_bins).sqrt(),
            (high_e / high_bins).sqrt(),
        ]);
        self.frame_rms.push(rms);
        self.frame_vocal.push((vocal_share * tonality).sqrt());
    }

    pub fn finish(self) -> OnsetFeatures {
        let onset = subtract_local_mean(&self.raw_combined, LOCAL_MEAN_RADIUS);
        let low_onset = subtract_local_mean(&self.raw_low, LOCAL_MEAN_RADIUS);
        OnsetFeatures {
            onset,
            low_onset,
            band_energy: self.band_energy,
            frame_rms: self.frame_rms,
            frame_vocal: self.frame_vocal,
        }
    }
}

impl Default for OnsetCollector {
    fn default() -> Self {
        Self::new()
    }
}

/// 减去对称滑窗均值并半波整流，压低持续稳态音对瞬态的抬升。
fn subtract_local_mean(values: &[f32], radius: usize) -> Vec<f32> {
    let n = values.len();
    if n == 0 {
        return Vec::new();
    }
    let mut prefix = Vec::with_capacity(n + 1);
    prefix.push(0.0_f64);
    for &v in values {
        let last = prefix[prefix.len() - 1];
        prefix.push(last + f64::from(v));
    }

    let mut out = Vec::with_capacity(n);
    for (i, &v) in values.iter().enumerate() {
        let lo = i.saturating_sub(radius);
        let hi = (i + radius + 1).min(n);
        let mean = ((prefix[hi] - prefix[lo]) / (hi - lo) as f64) as f32;
        out.push((v - mean).max(0.0));
    }
    out
}
