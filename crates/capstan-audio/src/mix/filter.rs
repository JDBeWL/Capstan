//! 混音过渡滤波器，包含三段 DJ 均衡（低、中、高）与 Cytomic TPT 状态变量扫频滤镜。

use std::f32::consts::{FRAC_1_SQRT_2, PI, TAU};

/// 低频交换的分频点，覆盖底鼓与次低音主能量区。
pub const LOW_SHELF_HZ: f32 = 150.0;
/// 中频均衡中心频率，覆盖人声与主旋律核心频段。
pub const MID_PEAK_HZ: f32 = 1_000.0;
/// 高频搁架分频点，覆盖踩镲与空气感频段。
pub const HIGH_SHELF_HZ: f32 = 6_000.0;

/// 扫频滤镜双极点控制量的柔和干湿过渡宽度，消除跨零爆音。
const SWEEP_FADE_ZONE: f32 = 0.04;
/// 扫频滤镜品质因数，略带音乐性共鸣而不刺耳。
const SWEEP_Q: f32 = 0.85;
const SWEEP_LPF_MIN_HZ: f32 = 180.0;
const SWEEP_LPF_MAX_HZ: f32 = 18_000.0;
const SWEEP_HPF_MIN_HZ: f32 = 30.0;
const SWEEP_HPF_MAX_HZ: f32 = 8_000.0;

/// 通用双二阶滤波器内核（Direct Form II Transposed）。
#[derive(Clone, Copy, Debug)]
struct Biquad {
    b0: f32,
    b1: f32,
    b2: f32,
    a1: f32,
    a2: f32,
    z1: f32,
    z2: f32,
}

impl Biquad {
    const fn passthrough() -> Self {
        Self {
            b0: 1.0,
            b1: 0.0,
            b2: 0.0,
            a1: 0.0,
            a2: 0.0,
            z1: 0.0,
            z2: 0.0,
        }
    }

    fn set_passthrough(&mut self) {
        self.b0 = 1.0;
        self.b1 = 0.0;
        self.b2 = 0.0;
        self.a1 = 0.0;
        self.a2 = 0.0;
    }

    fn set_normalized(&mut self, b0: f32, b1: f32, b2: f32, a0: f32, a1: f32, a2: f32) {
        let inv = 1.0 / a0;
        self.b0 = b0 * inv;
        self.b1 = b1 * inv;
        self.b2 = b2 * inv;
        self.a1 = a1 * inv;
        self.a2 = a2 * inv;
    }

    #[inline]
    fn process(&mut self, x: f32) -> f32 {
        let y = self.b0 * x + self.z1;
        self.z1 = self.b1 * x - self.a1 * y + self.z2;
        self.z2 = self.b2 * x - self.a2 * y;
        y
    }
}

/// 单声道低搁架滤波器（150 Hz，Q = 0.707）。
#[derive(Clone, Copy, Debug)]
pub struct LowShelf {
    sample_rate: f32,
    gain_db: f32,
    bq: Biquad,
}

impl LowShelf {
    pub fn new(sample_rate: u32) -> Self {
        Self {
            sample_rate: sample_rate.max(8_000) as f32,
            gain_db: 0.0,
            bq: Biquad::passthrough(),
        }
    }

    /// 更新低搁架增益（单位 dB，夹到 -36..=12）。变化量小于 0.01 dB 时跳过重算。
    pub fn set_gain_db(&mut self, gain_db: f32) {
        let clamped = gain_db.clamp(-36.0, 12.0);
        if (clamped - self.gain_db).abs() < 0.01 && self.bq.b0 != 0.0 {
            return;
        }
        self.gain_db = clamped;
        if clamped.abs() < 0.01 {
            self.bq.set_passthrough();
            return;
        }

        let a = 10.0_f32.powf(clamped / 40.0);
        let hz = LOW_SHELF_HZ.min(self.sample_rate * 0.42);
        let w0 = TAU * hz / self.sample_rate;
        let cos_w0 = w0.cos();
        let sin_w0 = w0.sin();
        let alpha = sin_w0 / (2.0 * FRAC_1_SQRT_2);
        let two_sqrt_a_alpha = 2.0 * a.sqrt() * alpha;

        let b0 = a * ((a + 1.0) - (a - 1.0) * cos_w0 + two_sqrt_a_alpha);
        let b1 = 2.0 * a * ((a - 1.0) - (a + 1.0) * cos_w0);
        let b2 = a * ((a + 1.0) - (a - 1.0) * cos_w0 - two_sqrt_a_alpha);
        let a0 = (a + 1.0) + (a - 1.0) * cos_w0 + two_sqrt_a_alpha;
        let a1 = -2.0 * ((a - 1.0) + (a + 1.0) * cos_w0);
        let a2 = (a + 1.0) + (a - 1.0) * cos_w0 - two_sqrt_a_alpha;

        self.bq.set_normalized(b0, b1, b2, a0, a1, a2);
    }

    #[inline]
    pub fn process(&mut self, x: f32) -> f32 {
        self.bq.process(x)
    }
}

/// 按声道数组织的低搁架滤波器组，保证左右声道独立维护延迟状态。
#[derive(Clone, Debug)]
pub struct LowShelfBank {
    channels: Vec<LowShelf>,
}

impl LowShelfBank {
    pub fn new(sample_rate: u32, channel_count: usize) -> Self {
        let n = channel_count.max(1);
        Self {
            channels: (0..n).map(|_| LowShelf::new(sample_rate)).collect(),
        }
    }

    pub fn set_gain_db(&mut self, gain_db: f32) {
        for filter in &mut self.channels {
            filter.set_gain_db(gain_db);
        }
    }

    #[inline]
    pub fn process_channel(&mut self, channel: usize, sample: f32) -> f32 {
        self.channels
            .get_mut(channel)
            .map_or(sample, |f| f.process(sample))
    }
}

/// 单声道中频钟形均衡器（1 kHz，Q = 0.707）。
#[derive(Clone, Copy, Debug)]
struct MidPeak {
    sample_rate: f32,
    gain_db: f32,
    bq: Biquad,
}

impl MidPeak {
    fn new(sample_rate: u32) -> Self {
        Self {
            sample_rate: sample_rate.max(8_000) as f32,
            gain_db: 0.0,
            bq: Biquad::passthrough(),
        }
    }

    fn set_gain_db(&mut self, gain_db: f32) {
        let clamped = gain_db.clamp(-24.0, 12.0);
        if (clamped - self.gain_db).abs() < 0.01 && self.bq.b0 != 0.0 {
            return;
        }
        self.gain_db = clamped;
        if clamped.abs() < 0.01 {
            self.bq.set_passthrough();
            return;
        }

        let a = 10.0_f32.powf(clamped / 40.0);
        let hz = MID_PEAK_HZ.min(self.sample_rate * 0.42);
        let w0 = TAU * hz / self.sample_rate;
        let cos_w0 = w0.cos();
        let sin_w0 = w0.sin();
        let alpha = sin_w0 / (2.0 * FRAC_1_SQRT_2);

        let b0 = 1.0 + alpha * a;
        let b1 = -2.0 * cos_w0;
        let b2 = 1.0 - alpha * a;
        let a0 = 1.0 + alpha / a;
        let a1 = -2.0 * cos_w0;
        let a2 = 1.0 - alpha / a;

        self.bq.set_normalized(b0, b1, b2, a0, a1, a2);
    }

    #[inline]
    fn process(&mut self, x: f32) -> f32 {
        self.bq.process(x)
    }
}

/// 单声道高搁架均衡器（6 kHz，Q = 0.707）。
#[derive(Clone, Copy, Debug)]
struct HighShelf {
    sample_rate: f32,
    gain_db: f32,
    bq: Biquad,
}

impl HighShelf {
    fn new(sample_rate: u32) -> Self {
        Self {
            sample_rate: sample_rate.max(8_000) as f32,
            gain_db: 0.0,
            bq: Biquad::passthrough(),
        }
    }

    fn set_gain_db(&mut self, gain_db: f32) {
        let clamped = gain_db.clamp(-24.0, 12.0);
        if (clamped - self.gain_db).abs() < 0.01 && self.bq.b0 != 0.0 {
            return;
        }
        self.gain_db = clamped;
        if clamped.abs() < 0.01 {
            self.bq.set_passthrough();
            return;
        }

        let a = 10.0_f32.powf(clamped / 40.0);
        let hz = HIGH_SHELF_HZ.min(self.sample_rate * 0.42);
        let w0 = TAU * hz / self.sample_rate;
        let cos_w0 = w0.cos();
        let sin_w0 = w0.sin();
        let alpha = sin_w0 / (2.0 * FRAC_1_SQRT_2);
        let two_sqrt_a_alpha = 2.0 * a.sqrt() * alpha;

        let b0 = a * ((a + 1.0) + (a - 1.0) * cos_w0 + two_sqrt_a_alpha);
        let b1 = -2.0 * a * ((a - 1.0) + (a + 1.0) * cos_w0);
        let b2 = a * ((a + 1.0) + (a - 1.0) * cos_w0 - two_sqrt_a_alpha);
        let a0 = (a + 1.0) - (a - 1.0) * cos_w0 + two_sqrt_a_alpha;
        let a1 = 2.0 * ((a - 1.0) - (a + 1.0) * cos_w0);
        let a2 = (a + 1.0) - (a - 1.0) * cos_w0 - two_sqrt_a_alpha;

        self.bq.set_normalized(b0, b1, b2, a0, a1, a2);
    }

    #[inline]
    fn process(&mut self, x: f32) -> f32 {
        self.bq.process(x)
    }
}

/// 三段按需启用的多声道均衡器组。未绑定的频段不分配也不参与计算。
#[derive(Clone, Debug)]
pub struct ThreeBandEqBank {
    low: Option<LowShelfBank>,
    mid: Option<Vec<MidPeak>>,
    high: Option<Vec<HighShelf>>,
}

impl ThreeBandEqBank {
    pub fn new(
        sample_rate: u32,
        channel_count: usize,
        has_low: bool,
        has_mid: bool,
        has_high: bool,
    ) -> Self {
        let n = channel_count.max(1);
        Self {
            low: has_low.then(|| LowShelfBank::new(sample_rate, n)),
            mid: has_mid.then(|| (0..n).map(|_| MidPeak::new(sample_rate)).collect()),
            high: has_high.then(|| (0..n).map(|_| HighShelf::new(sample_rate)).collect()),
        }
    }

    pub fn set_low_db(&mut self, db: f32) {
        if let Some(low) = self.low.as_mut() {
            low.set_gain_db(db);
        }
    }

    pub fn set_mid_db(&mut self, db: f32) {
        if let Some(mid) = self.mid.as_mut() {
            for f in mid {
                f.set_gain_db(db);
            }
        }
    }

    pub fn set_high_db(&mut self, db: f32) {
        if let Some(high) = self.high.as_mut() {
            for f in high {
                f.set_gain_db(db);
            }
        }
    }

    #[inline]
    pub fn process_channel(&mut self, channel: usize, mut sample: f32) -> f32 {
        if let Some(low) = self.low.as_mut() {
            sample = low.process_channel(channel, sample);
        }
        if let Some(mid) = self.mid.as_mut()
            && let Some(f) = mid.get_mut(channel)
        {
            sample = f.process(sample);
        }
        if let Some(high) = self.high.as_mut()
            && let Some(f) = high.get_mut(channel)
        {
            sample = f.process(sample);
        }
        sample
    }
}

/// 单声道双极点 DJ 扫频滤镜，采用 Cytomic TPT SVF 拓扑以消除动态调制爆音。
#[derive(Clone, Copy, Debug)]
struct SweepFilter {
    sample_rate: f32,
    amount: f32,
    wet: f32,
    is_hpf: bool,
    a1: f32,
    a2: f32,
    a3: f32,
    k: f32,
    ic1eq: f32,
    ic2eq: f32,
}

impl SweepFilter {
    fn new(sample_rate: u32) -> Self {
        Self {
            sample_rate: sample_rate.max(8_000) as f32,
            amount: 0.0,
            wet: 0.0,
            is_hpf: false,
            a1: 1.0,
            a2: 0.0,
            a3: 0.0,
            k: 1.0 / SWEEP_Q,
            ic1eq: 0.0,
            ic2eq: 0.0,
        }
    }

    /// 设置双极点控制量。`-1.0` 为深低通，`0.0` 为无损直通，`+1.0` 为深高通。
    fn set_amount(&mut self, amount: f32) {
        let clamped = amount.clamp(-1.0, 1.0);
        if (clamped - self.amount).abs() < 1e-4 {
            return;
        }
        self.amount = clamped;
        let mag = clamped.abs();
        if mag < 1e-4 {
            self.wet = 0.0;
            return;
        }

        self.wet = (mag / SWEEP_FADE_ZONE).clamp(0.0, 1.0);
        self.is_hpf = clamped > 0.0;

        let fc = if self.is_hpf {
            let max_hz = SWEEP_HPF_MAX_HZ.min(self.sample_rate * 0.40);
            SWEEP_HPF_MIN_HZ * (max_hz / SWEEP_HPF_MIN_HZ).powf(mag)
        } else {
            let max_hz = SWEEP_LPF_MAX_HZ.min(self.sample_rate * 0.42);
            max_hz * (SWEEP_LPF_MIN_HZ / max_hz).powf(mag)
        };
        let fc = fc.clamp(20.0, self.sample_rate * 0.44);

        let g = (PI * fc / self.sample_rate).tan();
        let k = self.k;
        self.a1 = 1.0 / (1.0 + g * (g + k));
        self.a2 = g * self.a1;
        self.a3 = g * self.a2;
    }

    #[inline]
    fn process(&mut self, x: f32) -> f32 {
        if self.wet <= 0.0 {
            return x;
        }
        let v3 = x - self.ic2eq;
        let v1 = self.a1 * self.ic1eq + self.a2 * v3;
        let v2 = self.ic2eq + self.a2 * self.ic1eq + self.a3 * v3;
        self.ic1eq = 2.0 * v1 - self.ic1eq;
        self.ic2eq = 2.0 * v2 - self.ic2eq;

        let filtered = if self.is_hpf {
            x - self.k * v1 - v2
        } else {
            v2
        };
        if self.wet >= 1.0 {
            filtered
        } else {
            x + self.wet * (filtered - x)
        }
    }
}

/// 多声道双极点扫频滤镜组。
#[derive(Clone, Debug)]
pub struct SweepFilterBank {
    channels: Vec<SweepFilter>,
}

impl SweepFilterBank {
    pub fn new(sample_rate: u32, channel_count: usize) -> Self {
        let n = channel_count.max(1);
        Self {
            channels: (0..n).map(|_| SweepFilter::new(sample_rate)).collect(),
        }
    }

    pub fn set_amount(&mut self, amount: f32) {
        for f in &mut self.channels {
            f.set_amount(amount);
        }
    }

    #[inline]
    pub fn process_channel(&mut self, channel: usize, sample: f32) -> f32 {
        self.channels
            .get_mut(channel)
            .map_or(sample, |f| f.process(sample))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn measure_db(mut run: impl FnMut(f32) -> f32, rate: u32, hz: f32) -> f32 {
        let total = rate as usize;
        let settle = total / 2;
        let mut sq = 0.0_f64;
        for i in 0..total {
            let x = (TAU * hz * i as f32 / rate as f32).sin();
            let y = run(x);
            if i >= settle {
                sq += f64::from(y) * f64::from(y);
            }
        }
        let rms = (sq / (total - settle) as f64).sqrt();
        let in_rms = std::f64::consts::FRAC_1_SQRT_2;
        (20.0 * (rms / in_rms).log10()) as f32
    }

    #[test]
    fn zero_db_is_exact_passthrough() {
        let mut filter = LowShelf::new(48_000);
        for i in 0..512 {
            let x = (TAU * 440.0 * i as f32 / 48_000.0).sin();
            assert!((filter.process(x) - x).abs() < 1e-6);
        }
    }

    #[test]
    fn minus_24db_attenuates_50hz_and_leaves_5khz_intact() {
        let mut lo_filter = LowShelf::new(48_000);
        lo_filter.set_gain_db(-24.0);
        let low_db = measure_db(|x| lo_filter.process(x), 48_000, 50.0);
        assert!(
            low_db <= -20.0,
            "50 Hz 衰减不足：期望 ≤ -20 dB，实测 {low_db:.2} dB"
        );

        let mut hi_filter = LowShelf::new(48_000);
        hi_filter.set_gain_db(-24.0);
        let high_db = measure_db(|x| hi_filter.process(x), 48_000, 5_000.0);
        assert!(
            high_db.abs() <= 0.5,
            "5 kHz 应基本直通（±0.5 dB），实测 {high_db:.2} dB"
        );
    }

    #[test]
    fn three_band_eq_isolates_mid_and_high_bands() {
        let mut mid_eq = ThreeBandEqBank::new(48_000, 2, false, true, false);
        mid_eq.set_mid_db(-12.0);
        let mid_cut_db = measure_db(|x| mid_eq.process_channel(0, x), 48_000, 1_000.0);
        let sub_pass_db = measure_db(|x| mid_eq.process_channel(1, x), 48_000, 50.0);
        assert!(
            mid_cut_db <= -10.5,
            "1 kHz 中频应大幅衰减，实测 {mid_cut_db:.2} dB"
        );
        assert!(
            sub_pass_db.abs() <= 1.0,
            "50 Hz 不应受中频影响，实测 {sub_pass_db:.2} dB"
        );

        let mut hi_eq = ThreeBandEqBank::new(48_000, 2, false, false, true);
        hi_eq.set_high_db(-18.0);
        let treble_cut_db = measure_db(|x| hi_eq.process_channel(0, x), 48_000, 12_000.0);
        let bass_pass_db = measure_db(|x| hi_eq.process_channel(1, x), 48_000, 100.0);
        assert!(
            treble_cut_db <= -13.0,
            "12 kHz 高频应显著衰减，实测 {treble_cut_db:.2} dB"
        );
        assert!(
            bass_pass_db.abs() <= 0.5,
            "100 Hz 不应受高频搁架影响，实测 {bass_pass_db:.2} dB"
        );
    }

    #[test]
    fn sweep_filter_lpf_hpf_and_rapid_modulation_are_stable() {
        let mut sweep = SweepFilterBank::new(48_000, 2);
        for i in 0..256 {
            let x = (TAU * 440.0 * i as f32 / 48_000.0).sin();
            assert_eq!(sweep.process_channel(0, x), x, "零位必须无损直通");
        }

        // LPF 深扫（-0.75，截止约 570 Hz）；8 kHz 应大幅衰减，100 Hz 保留。
        sweep.set_amount(-0.75);
        let lpf_hi = measure_db(|x| sweep.process_channel(0, x), 48_000, 8_000.0);
        let lpf_lo = measure_db(|x| sweep.process_channel(1, x), 48_000, 100.0);
        assert!(
            lpf_hi <= -30.0,
            "LPF 下 8 kHz 应重度衰减，实测 {lpf_hi:.2} dB"
        );
        assert!(
            lpf_lo.abs() <= 1.5,
            "LPF 下 100 Hz 应通过，实测 {lpf_lo:.2} dB"
        );

        // HPF 深扫（+0.75，截止约 2 kHz）；80 Hz 应大幅衰减，8 kHz 保留。
        let mut hp_sweep = SweepFilterBank::new(48_000, 2);
        hp_sweep.set_amount(0.75);
        let hpf_lo = measure_db(|x| hp_sweep.process_channel(0, x), 48_000, 80.0);
        let hpf_hi = measure_db(|x| hp_sweep.process_channel(1, x), 48_000, 8_000.0);
        assert!(
            hpf_lo <= -35.0,
            "HPF 下 80 Hz 应重度衰减，实测 {hpf_lo:.2} dB"
        );
        assert!(
            hpf_hi.abs() <= 1.5,
            "HPF 下 8 kHz 应通过，实测 {hpf_hi:.2} dB"
        );

        // 剧烈往返扫频不得产生非有限值或增益爆炸。
        let mut mod_sweep = SweepFilterBank::new(48_000, 1);
        for i in 0..4_800 {
            if i % 64 == 0 {
                let amount = ((i as f32 / 4_800.0) * TAU * 4.0).sin();
                mod_sweep.set_amount(amount);
            }
            let x = (TAU * 330.0 * i as f32 / 48_000.0).sin();
            let y = mod_sweep.process_channel(0, x);
            assert!(y.is_finite() && y.abs() < 4.0, "快速扫频发散：y={y}");
        }
    }
}
