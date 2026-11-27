//! 色度提取与 Krumhansl–Schmuckler 调性估计，映射至 Camelot 轮编码。

use crate::model::{KeyInfo, MusicalMode};
use crate::stft::{ANALYSIS_RATE, FFT_SIZE, SPECTRUM_BINS};

/// Krumhansl–Schmuckler 大调与小调轮廓（以 C 为 0）。
const MAJOR_PROFILE: [f32; 12] = [
    6.35, 2.23, 3.48, 2.33, 4.38, 4.09, 2.52, 5.19, 2.39, 3.66, 2.29, 2.88,
];
const MINOR_PROFILE: [f32; 12] = [
    6.33, 2.68, 3.52, 5.38, 2.60, 3.53, 2.54, 4.75, 3.98, 2.69, 3.34, 3.17,
];

/// 主音序号（0=C..11=B）到大调（B）与小调（A）Camelot 编号的映射。
const MAJOR_CAMELOT: [u8; 12] = [8, 3, 10, 5, 12, 7, 2, 9, 4, 11, 6, 1];
const MINOR_CAMELOT: [u8; 12] = [5, 12, 7, 2, 9, 4, 11, 6, 1, 8, 3, 10];

/// 流式色度累加器。同时保存低频采样（约每秒 2 帧）的色度轨迹，供乐句边界新奇度使用。
pub struct ChromaCollector {
    /// 每个频箱对应的半音类（0..12），有效频率范围之外为 `None`。
    bin_pitch: Vec<Option<u8>>,
    global: [f64; 12],
    /// 按固定步长降采样后的色度帧（约 12 帧合并 1 帧，即 ~7 Hz）。
    frames: Vec<[f32; 12]>,
    acc: [f32; 12],
    acc_count: usize,
}

/// 多少个 STFT 帧合并为一帧色度轨迹。
pub const CHROMA_STRIDE: usize = 12;

impl ChromaCollector {
    pub fn new() -> Self {
        let bin_hz = ANALYSIS_RATE as f32 / FFT_SIZE as f32;
        let bin_pitch = (0..SPECTRUM_BINS)
            .map(|bin| {
                let hz = bin as f32 * bin_hz;
                // 65 Hz（C2）到 2100 Hz（C7），覆盖基音与主要泛音区。
                if !(65.0..=2_100.0).contains(&hz) {
                    return None;
                }
                let midi = (12.0 * (hz / 440.0).log2() + 69.0).round() as i32;
                Some(midi.rem_euclid(12) as u8)
            })
            .collect();

        Self {
            bin_pitch,
            global: [0.0; 12],
            frames: Vec::new(),
            acc: [0.0; 12],
            acc_count: 0,
        }
    }

    pub fn push_frame(&mut self, magnitudes: &[f32]) {
        for (bin, &mag) in magnitudes.iter().enumerate().take(SPECTRUM_BINS) {
            if let Some(pc) = self.bin_pitch[bin] {
                let power = mag * mag;
                let idx = usize::from(pc);
                self.global[idx] += f64::from(power);
                self.acc[idx] += power;
            }
        }
        self.acc_count += 1;
        if self.acc_count >= CHROMA_STRIDE {
            self.flush_acc();
        }
    }

    fn flush_acc(&mut self) {
        if self.acc_count == 0 {
            return;
        }
        let sum: f32 = self.acc.iter().sum();
        let mut frame = self.acc;
        if sum > 1e-9 {
            for v in &mut frame {
                *v /= sum;
            }
        }
        self.frames.push(frame);
        self.acc = [0.0; 12];
        self.acc_count = 0;
    }

    /// 结束采集，返回 `(调性估计, 降采样色度轨迹)`。
    pub fn finish(mut self) -> (Option<KeyInfo>, Vec<[f32; 12]>) {
        self.flush_acc();
        let total: f64 = self.global.iter().sum();
        if total <= 1e-9 {
            return (None, self.frames);
        }
        let chroma: [f32; 12] = std::array::from_fn(|i| (self.global[i] / total) as f32);
        let key = estimate_key(&chroma);
        (key, self.frames)
    }
}

impl Default for ChromaCollector {
    fn default() -> Self {
        Self::new()
    }
}

fn estimate_key(chroma: &[f32; 12]) -> Option<KeyInfo> {
    let mut best = (0u8, MusicalMode::Major, f32::NEG_INFINITY);
    let mut second = f32::NEG_INFINITY;

    for tonic in 0..12u8 {
        for (mode, profile) in [
            (MusicalMode::Major, &MAJOR_PROFILE),
            (MusicalMode::Minor, &MINOR_PROFILE),
        ] {
            let corr = pearson_rotated(chroma, profile, usize::from(tonic));
            if corr > best.2 {
                second = best.2;
                best = (tonic, mode, corr);
            } else if corr > second {
                second = corr;
            }
        }
    }

    if best.2 <= 0.15 {
        return None;
    }

    let margin = (best.2 - second).max(0.0);
    let confidence =
        (0.5 * best.2.clamp(0.0, 1.0) + 0.5 * (margin * 4.0).clamp(0.0, 1.0)).clamp(0.0, 1.0);
    let (camelot_num, camelot_letter) = match best.1 {
        MusicalMode::Major => (MAJOR_CAMELOT[usize::from(best.0)], 'B'),
        MusicalMode::Minor => (MINOR_CAMELOT[usize::from(best.0)], 'A'),
    };

    Some(KeyInfo {
        tonic: best.0,
        mode: best.1,
        camelot_num,
        camelot_letter,
        confidence,
    })
}

fn pearson_rotated(chroma: &[f32; 12], profile: &[f32; 12], tonic: usize) -> f32 {
    let mean_c: f32 = chroma.iter().sum::<f32>() / 12.0;
    let mean_p: f32 = profile.iter().sum::<f32>() / 12.0;

    let mut num = 0.0_f32;
    let mut den_c = 0.0_f32;
    let mut den_p = 0.0_f32;

    for i in 0..12 {
        let dc = chroma[(i + tonic) % 12] - mean_c;
        let dp = profile[i] - mean_p;
        num += dc * dp;
        den_c += dc * dc;
        den_p += dp * dp;
    }
    let den = (den_c * den_p).sqrt();
    if den <= 1e-9 { 0.0 } else { num / den }
}
