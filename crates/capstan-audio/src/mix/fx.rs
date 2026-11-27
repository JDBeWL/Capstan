//! 过渡期专用效果器，包含节拍同步回声散尾与总线软限幅器。

use std::f32::consts::TAU;

/// 回声反馈环内的高通截止频率，滤掉底鼓堆积以免与下一首低频冲突。
const ECHO_HPF_HZ: f32 = 280.0;
/// 回声反馈环内的低通截止频率，使每次回声自然变暗退远。
const ECHO_LPF_HZ: f32 = 5_500.0;
/// 软限幅器起控门限，低于此电平保持逐样本直通。
const LIMITER_THRESHOLD: f32 = 0.92;
/// 软限幅器渐近天花板，防止双轨叠加峰值削波。
const LIMITER_CEILING: f32 = 0.985;
/// 软限幅器释放时间常数，单位秒。
const LIMITER_RELEASE_SECS: f32 = 0.050;

/// 单声道节拍同步反馈延迟线，内嵌高通与低通塑形。
#[derive(Clone, Debug)]
struct EchoChannel {
    buf: Vec<f32>,
    cursor: usize,
    hp_lp: f32,
    lp_state: f32,
}

impl EchoChannel {
    fn new(delay_frames: usize) -> Self {
        Self {
            buf: vec![0.0; delay_frames.max(1)],
            cursor: 0,
            hp_lp: 0.0,
            lp_state: 0.0,
        }
    }

    #[inline]
    fn process(
        &mut self,
        dry: f32,
        send: f32,
        feedback: f32,
        hp_alpha: f32,
        lp_alpha: f32,
        tail_fade: f32,
    ) -> f32 {
        let delayed = self.buf[self.cursor];
        self.hp_lp += hp_alpha * (delayed - self.hp_lp);
        let hp = delayed - self.hp_lp;
        self.lp_state += lp_alpha * (hp - self.lp_state);
        let shaped = self.lp_state;

        self.buf[self.cursor] = dry * send + shaped * feedback;
        self.cursor += 1;
        if self.cursor >= self.buf.len() {
            self.cursor = 0;
        }
        shaped * tail_fade
    }
}

/// 多声道节拍同步回声散尾器，声道间独立缓冲以保留立体声像。
#[derive(Clone, Debug)]
pub struct EchoOutBank {
    channels: Vec<EchoChannel>,
    feedback: f32,
    hp_alpha: f32,
    lp_alpha: f32,
}

impl EchoOutBank {
    pub fn new(sample_rate: u32, channel_count: usize, delay_secs: f32, feedback: f32) -> Self {
        let sr = sample_rate.max(8_000) as f32;
        let delay_frames = ((delay_secs.clamp(0.08, 1.2) * sr).round() as usize).max(1);
        let hp_hz = ECHO_HPF_HZ.min(sr * 0.40);
        let lp_hz = ECHO_LPF_HZ.min(sr * 0.42);
        let hp_alpha = 1.0 - (-TAU * hp_hz / sr).exp();
        let lp_alpha = 1.0 - (-TAU * lp_hz / sr).exp();
        let n = channel_count.max(1);

        Self {
            channels: (0..n).map(|_| EchoChannel::new(delay_frames)).collect(),
            feedback: feedback.clamp(0.0, 0.82),
            hp_alpha,
            lp_alpha,
        }
    }

    /// 输入该声道的 A 轨干声与发送量，返回叠加到总线的回声湿声。
    #[inline]
    pub fn process_channel(&mut self, channel: usize, dry: f32, send: f32, tail_fade: f32) -> f32 {
        let Some(line) = self.channels.get_mut(channel) else {
            return 0.0;
        };
        line.process(
            dry,
            send.clamp(0.0, 1.0),
            self.feedback,
            self.hp_alpha,
            self.lp_alpha,
            tail_fade.clamp(0.0, 1.0),
        )
    }
}

/// 零延迟立体声联动软限幅器，保护双轨叠加总线不削波。
#[derive(Clone, Copy, Debug)]
pub struct SoftLimiter {
    gain: f32,
    release_coeff: f32,
}

impl SoftLimiter {
    pub fn new(sample_rate: u32) -> Self {
        let sr = sample_rate.max(8_000) as f32;
        let release_coeff = 1.0 - (-1.0 / (LIMITER_RELEASE_SECS * sr)).exp();
        Self {
            gain: 1.0,
            release_coeff,
        }
    }

    /// 对一整帧交错声道样本施加相同增益，保证声像不偏移。
    #[inline]
    pub fn process_frame(&mut self, frame: &mut [f32]) {
        let peak = frame.iter().fold(0.0_f32, |acc, &s| acc.max(s.abs()));
        let target = if peak > LIMITER_THRESHOLD {
            let span = LIMITER_CEILING - LIMITER_THRESHOLD;
            let over = peak - LIMITER_THRESHOLD;
            let out_peak = LIMITER_THRESHOLD + span * (over / span).tanh();
            out_peak / peak
        } else {
            1.0
        };

        if target < self.gain {
            self.gain = target;
        } else if self.gain < 1.0 {
            self.gain += self.release_coeff * (target - self.gain);
            if target >= 1.0 && (1.0 - self.gain) < 1e-6 {
                self.gain = 1.0;
            }
        }

        if self.gain < 1.0 {
            for sample in frame {
                *sample *= self.gain;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn echo_out_produces_decaying_beat_synced_repeats_after_dry_stops() {
        let sr = 10_000_u32;
        // 0.1 秒延迟对应 1000 帧，反馈 0.55。
        let mut echo = EchoOutBank::new(sr, 2, 0.1, 0.55);

        // 先送入 20 帧 1 kHz 短脉冲作为干声，随后彻底静音。
        let mut wet = Vec::with_capacity(4_000);
        for i in 0..4_000 {
            let dry = if i < 20 {
                (TAU * 1_000.0 * i as f32 / sr as f32).sin()
            } else {
                0.0
            };
            let send = if i < 20 { 1.0 } else { 0.0 };
            wet.push(echo.process_channel(0, dry, send, 1.0));
        }

        let peak_in = |start: usize, end: usize| -> f32 {
            wet[start..end]
                .iter()
                .fold(0.0_f32, |acc, &s| acc.max(s.abs()))
        };
        let p0 = peak_in(0, 900);
        let p1 = peak_in(980, 1_060);
        let p2 = peak_in(1_980, 2_060);
        let p3 = peak_in(2_980, 3_060);

        assert!(p0 < 1e-5, "首拍到达前不应有回声输出，实测 {p0}");
        assert!(p1 > 0.3, "第 1 次节拍回声应当清晰可闻，实测 {p1}");
        assert!(
            p2 > 0.1 && p2 < p1,
            "第 2 次回声应当平滑衰减：p1={p1}, p2={p2}"
        );
        assert!(p3 < p2, "第 3 次回声应当继续衰减：p2={p2}, p3={p3}");
    }

    #[test]
    fn soft_limiter_is_bit_exact_below_threshold_and_caps_peaks_below_ceiling() {
        let mut limiter = SoftLimiter::new(48_000);
        let mut quiet = [0.5_f32, -0.8_f32];
        limiter.process_frame(&mut quiet);
        assert_eq!(quiet, [0.5, -0.8], "门限以下必须逐样本无损直通");

        let mut hot = [1.8_f32, -0.9_f32];
        limiter.process_frame(&mut hot);
        assert!(
            hot[0] <= LIMITER_CEILING + 1e-6 && hot[0] > LIMITER_THRESHOLD,
            "峰值应压缩到天花板以内，实测 {}",
            hot[0]
        );
        // 左右声道等比缩放，比值保持 2:-1 不偏移声像。
        assert!(
            (hot[0] / hot[1].abs() - 2.0).abs() < 1e-5,
            "立体声联动缩放不得改变声道比"
        );
    }
}
