//! 流式短时傅里叶变换（STFT）。复用内部缓冲区，逐帧回调幅度谱与未加窗均方根。

use std::f32::consts::TAU;
use std::sync::Arc;

use rustfft::num_complex::Complex;
use rustfft::{Fft, FftPlanner};

/// 分析使用的统一采样率，单位 Hz。
pub const ANALYSIS_RATE: u32 = 22_050;
/// STFT 窗长。1024 点 @22.05 kHz 约 46.4 ms，兼顾低频分辨率与瞬态定位。
pub const FFT_SIZE: usize = 1024;
/// STFT 步长。256 点 @22.05 kHz 约 11.6 ms，帧率约 86.13 Hz。
pub const HOP_SIZE: usize = 256;
/// 有效正频率箱数（含直流与奈奎斯特）。
pub const SPECTRUM_BINS: usize = FFT_SIZE / 2 + 1;
/// 每秒 STFT 帧数。
pub const FRAME_RATE: f32 = ANALYSIS_RATE as f32 / HOP_SIZE as f32;

/// 第 `frame` 帧在瞬态检测中的有效时间戳，单位秒。
/// 经对数压缩的谱通量在窗右缘刚触及脉冲时跃升最大，故按右缘偏半步标定时间。
pub fn onset_frame_time(frame: usize) -> f32 {
    let sample = frame * HOP_SIZE + FFT_SIZE - HOP_SIZE / 2;
    sample as f32 / ANALYSIS_RATE as f32
}

/// 第 `frame` 帧对应的窗中心时刻，单位秒。用于静音与能量定位。
pub fn center_frame_time(frame: usize) -> f32 {
    let sample = frame * HOP_SIZE + FFT_SIZE / 2;
    sample as f32 / ANALYSIS_RATE as f32
}

pub struct StreamingStft {
    fft: Arc<dyn Fft<f32>>,
    window: Vec<f32>,
    ring: Vec<f32>,
    filled: usize,
    spectrum: Vec<Complex<f32>>,
    scratch: Vec<Complex<f32>>,
    magnitudes: Vec<f32>,
}

impl StreamingStft {
    pub fn new() -> Self {
        let mut planner = FftPlanner::<f32>::new();
        let fft = planner.plan_fft_forward(FFT_SIZE);
        let scratch_len = fft.get_inplace_scratch_len();

        let window: Vec<f32> = (0..FFT_SIZE)
            .map(|i| 0.5 * (1.0 - (TAU * i as f32 / FFT_SIZE as f32).cos()))
            .collect();

        Self {
            fft,
            window,
            ring: vec![0.0; FFT_SIZE],
            filled: 0,
            spectrum: vec![Complex::new(0.0, 0.0); FFT_SIZE],
            scratch: vec![Complex::new(0.0, 0.0); scratch_len],
            magnitudes: vec![0.0; SPECTRUM_BINS],
        }
    }

    /// 推送一批单声道 22.05 kHz 样本；每凑满一帧即触发一次 `on_frame(幅度谱, 帧均方根)`。
    pub fn push(&mut self, samples: &[f32], mut on_frame: impl FnMut(&[f32], f32)) {
        let mut cursor = 0;
        while cursor < samples.len() {
            let need = FFT_SIZE - self.filled;
            let take = need.min(samples.len() - cursor);
            self.ring[self.filled..self.filled + take]
                .copy_from_slice(&samples[cursor..cursor + take]);
            self.filled += take;
            cursor += take;

            if self.filled == FFT_SIZE {
                self.emit(&mut on_frame);
                self.ring.copy_within(HOP_SIZE..FFT_SIZE, 0);
                self.filled = FFT_SIZE - HOP_SIZE;
            }
        }
    }

    fn emit(&mut self, on_frame: &mut impl FnMut(&[f32], f32)) {
        let mut sq_sum = 0.0_f32;
        for i in 0..FFT_SIZE {
            let s = self.ring[i];
            sq_sum += s * s;
            self.spectrum[i] = Complex::new(s * self.window[i], 0.0);
        }
        let rms = (sq_sum / FFT_SIZE as f32).sqrt();

        self.fft
            .process_with_scratch(&mut self.spectrum, &mut self.scratch);

        for (bin, dst) in self.magnitudes.iter_mut().enumerate() {
            let c = self.spectrum[bin];
            *dst = c.re.hypot(c.im);
        }
        on_frame(&self.magnitudes, rms);
    }
}

impl Default for StreamingStft {
    fn default() -> Self {
        Self::new()
    }
}
