//! 流式 ITU-R BS.1770-4 积分响度计。
//! 在原生采样率与声道上逐块运行 K 计权与 400 ms 重叠门限，内存与曲目长度无关。

use std::f64::consts::PI;

/// 两级 K 计权 biquad（Direct Form II Transposed）。
#[derive(Clone, Copy, Debug)]
struct Biquad {
    b0: f64,
    b1: f64,
    b2: f64,
    a1: f64,
    a2: f64,
    z1: f64,
    z2: f64,
}

impl Biquad {
    fn new(b0: f64, b1: f64, b2: f64, a1: f64, a2: f64) -> Self {
        Self {
            b0,
            b1,
            b2,
            a1,
            a2,
            z1: 0.0,
            z2: 0.0,
        }
    }

    /// Stage 1：头模声学高搁架预滤波（+4 dB，转折约 1682 Hz）。
    fn pre_filter(sample_rate: f64) -> Self {
        let db = 3.999_843_853_973_347;
        let f0 = 1_681.974_450_955_533;
        let q = 0.707_175_236_955_419_6;
        let k = (PI * f0 / sample_rate).tan();
        let vh = 10.0_f64.powf(db / 20.0);
        let vb = vh.powf(0.499_666_774_154_541_6);
        let a0 = 1.0 + k / q + k * k;
        let b0 = (vh + vb * k / q + k * k) / a0;
        let b1 = 2.0 * (k * k - vh) / a0;
        let b2 = (vh - vb * k / q + k * k) / a0;
        let a1 = 2.0 * (k * k - 1.0) / a0;
        let a2 = (1.0 - k / q + k * k) / a0;
        Self::new(b0, b1, b2, a1, a2)
    }

    /// Stage 2：RLB 高通滤波（约 38 Hz）。
    fn rlb_filter(sample_rate: f64) -> Self {
        let f0 = 38.135_470_876_024_44;
        let q = 0.500_327_037_323_877_3;
        let k = (PI * f0 / sample_rate).tan();
        let a0 = 1.0 + k / q + k * k;
        let b0 = 1.0 / a0;
        let b1 = -2.0 / a0;
        let b2 = 1.0 / a0;
        let a1 = 2.0 * (k * k - 1.0) / a0;
        let a2 = (1.0 - k / q + k * k) / a0;
        Self::new(b0, b1, b2, a1, a2)
    }

    #[inline]
    fn process(&mut self, x: f64) -> f64 {
        let y = self.b0 * x + self.z1;
        self.z1 = self.b1 * x - self.a1 * y + self.z2;
        self.z2 = self.b2 * x - self.a2 * y;
        y
    }
}

/// 每个 100 ms 步长保存各声道的平方和，四格合并为一个 400 ms 门限块。
pub struct LoudnessMeter {
    channels: usize,
    weights: Vec<f64>,
    stage1: Vec<Biquad>,
    stage2: Vec<Biquad>,
    step_frames: usize,
    in_step: usize,
    step_sq: Vec<f64>,
    /// 按声道分别存最近 4 个 100 ms 步长的平方和，用于无分配滑动 400 ms 块。
    history: Vec<[f64; 4]>,
    ring_pos: usize,
    steps_seen: usize,
    /// 每个 400 ms 块的加权均方能量 `sum_i G_i * z_i`。
    blocks: Vec<f64>,
}

impl LoudnessMeter {
    pub fn new(sample_rate: u32, channels: u16) -> Self {
        let fs = f64::from(sample_rate.max(8_000));
        let ch = usize::from(channels.max(1));
        let weights = channel_weights(ch);
        let stage1 = (0..ch).map(|_| Biquad::pre_filter(fs)).collect();
        let stage2 = (0..ch).map(|_| Biquad::rlb_filter(fs)).collect();
        let step_frames = ((fs * 0.1).round() as usize).max(1);
        Self {
            channels: ch,
            weights,
            stage1,
            stage2,
            step_frames,
            in_step: 0,
            step_sq: vec![0.0; ch],
            history: vec![[0.0; 4]; ch],
            ring_pos: 0,
            steps_seen: 0,
            blocks: Vec::new(),
        }
    }

    /// 推送一帧交错样本。长度不足声道数时忽略。
    #[inline]
    pub fn push_frame(&mut self, frame: &[f32]) {
        if frame.len() < self.channels {
            return;
        }
        for (ch, &sample) in frame.iter().take(self.channels).enumerate() {
            let s1 = self.stage1[ch].process(f64::from(sample));
            let s2 = self.stage2[ch].process(s1);
            self.step_sq[ch] += s2 * s2;
        }
        self.in_step += 1;
        if self.in_step >= self.step_frames {
            self.commit_step();
        }
    }

    fn commit_step(&mut self) {
        let slot = self.ring_pos;
        for ch in 0..self.channels {
            self.history[ch][slot] = self.step_sq[ch];
            self.step_sq[ch] = 0.0;
        }
        self.ring_pos = (self.ring_pos + 1) % 4;
        self.steps_seen += 1;
        self.in_step = 0;

        if self.steps_seen >= 4 {
            let block_frames = (self.step_frames * 4) as f64;
            let mut weighted_energy = 0.0;
            for ch in 0..self.channels {
                let sum_sq: f64 = self.history[ch].iter().sum();
                weighted_energy += self.weights[ch] * (sum_sq / block_frames);
            }
            self.blocks.push(weighted_energy);
        }
    }

    /// 计算全曲积分响度（LUFS）。纯静音或短于 400 ms 时退回 `-70.0`。
    pub fn finish(&self) -> f32 {
        // 绝对门限为 -70 LUFS 对应的线性均方能量。
        let abs_thresh = 10.0_f64.powf((-70.0 + 0.691) / 10.0);
        let mut abs_sum = 0.0;
        let mut abs_count = 0usize;
        for &energy in &self.blocks {
            if energy > abs_thresh {
                abs_sum += energy;
                abs_count += 1;
            }
        }
        if abs_count == 0 {
            return -70.0;
        }
        let ungated_mean = abs_sum / abs_count as f64;
        // 相对门限为无门限响度再降 10 LU，即线性能量乘 0.1。
        let rel_thresh = ungated_mean * 0.1;

        let mut rel_sum = 0.0;
        let mut rel_count = 0usize;
        for &energy in &self.blocks {
            if energy > abs_thresh && energy > rel_thresh {
                rel_sum += energy;
                rel_count += 1;
            }
        }
        if rel_count == 0 {
            return -70.0;
        }
        let gated_mean = rel_sum / rel_count as f64;
        (-0.691 + 10.0 * gated_mean.log10()) as f32
    }
}

/// BS.1770 声道权重为前三声道 1.0，5.1 的 LFE 声道 0.0，环绕声道 1.41。
fn channel_weights(channels: usize) -> Vec<f64> {
    (0..channels)
        .map(|idx| match (channels, idx) {
            (_, 0..=2) => 1.0,
            (6..=8, 3) => 0.0,
            _ => 1.41,
        })
        .collect()
}
