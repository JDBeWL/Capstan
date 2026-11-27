//! 混音计划与其编译产物。音频内核只认帧号与求值器，不感知 BPM 或曲目结构。

use std::path::PathBuf;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::curve::{Automation, CompiledAutomation};
use super::tempo::{MAX_TEMPO_RATIO, MIN_TEMPO_RATIO, TempoMode};

/// 自动混音选出的过渡策略类型。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum MixKind {
    /// 速度匹配，对拍长过渡并可叠加低频交换。
    BeatMatched,
    /// 速度略偏，在小节线上做 1–2 小节短淡化。
    BarCrossfade,
    /// 速度差过大或节奏置信度低，退回按时间交叉淡化。
    Crossfade,
    /// 在小节线处直接短淡切入。
    Cut,
    /// 专辑连续曲目或无重叠场景，保持零修改无缝直连。
    #[default]
    Gapless,
}

impl MixKind {
    pub const fn label(self) -> &'static str {
        match self {
            Self::BeatMatched => "对拍混音",
            Self::BarCrossfade => "小节淡化",
            Self::Crossfade => "交叉淡化",
            Self::Cut => "节拍硬切",
            Self::Gapless => "无缝直连",
        }
    }
}

/// 过渡的音乐风格配方，决定音量、三段均衡、扫频滤镜与回声散尾的组合形态。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum MixStyle {
    /// 融合，即对拍长过渡，配合低频交换与柔和中高频错峰。
    #[default]
    Blend,
    /// 推升，即前曲高通扫频拉起张力，后曲低通渐开并在落拍处全频接管。
    Rise,
    /// 柔和淡化，即平滑等功率淡化并辅以轻低通退远，适合抒情或节奏不稳定曲目。
    Fade,
    /// 回声切，即前曲高通收束并触发节拍同步回声散尾，适合大速差或调性反差衔接。
    Wash,
    /// 用户手工微调后的自定义过渡。
    Custom,
}

impl MixStyle {
    /// 可由规划器或编辑器直接套用的四种内置配方。
    pub const RECIPES: [Self; 4] = [Self::Blend, Self::Rise, Self::Fade, Self::Wash];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Blend => "融合",
            Self::Rise => "推升",
            Self::Fade => "淡化",
            Self::Wash => "回声切",
            Self::Custom => "自定义",
        }
    }
}

/// 两条曲目之间的一次完整过渡计划。时间坐标均为各曲目的原始媒体时间。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct MixPlan {
    pub kind: MixKind,
    /// 当前计划所对应的过渡配方。
    pub style: MixStyle,
    /// 当前曲（A）的出点起点，B 在此时刻开始出声。
    pub out_start: Duration,
    /// 当前曲（A）的出点终点，此时刻丢弃 A；早于 A 总时长即裁剪尾奏。
    pub out_end: Duration,
    /// 下一首（B）的入点，B 从该时刻起播。
    pub in_start: Duration,
    /// A 的音量自动化曲线（`t ∈ [0, 1] → 线性增益`）。
    pub a_gain: Automation,
    /// B 的音量自动化曲线（`t ∈ [0, 1] → 线性增益`）。
    pub b_gain: Automation,
    /// A 的低搁架 EQ 曲线（150 Hz，dB）；空表示不启用。
    pub a_low_db: Automation,
    /// B 的低搁架 EQ 曲线（150 Hz，dB）；空表示不启用。
    pub b_low_db: Automation,
    /// A 的中频 EQ 曲线（1 kHz，dB）；空表示不启用。
    pub a_mid_db: Automation,
    /// B 的中频 EQ 曲线（1 kHz，dB）；空表示不启用。
    pub b_mid_db: Automation,
    /// A 的高搁架 EQ 曲线（6 kHz，dB）；空表示不启用。
    pub a_high_db: Automation,
    /// B 的高搁架 EQ 曲线（6 kHz，dB）；空表示不启用。
    pub b_high_db: Automation,
    /// A 的双极点扫频滤镜曲线（`-1.0` 深低通，`0.0` 直通，`+1.0` 深高通）；空表示不启用。
    pub a_filter: Automation,
    /// B 的双极点扫频滤镜曲线（`-1.0` 深低通，`0.0` 直通，`+1.0` 深高通）；空表示不启用。
    pub b_filter: Automation,
    /// A 的节拍回声发送量曲线（`0.0..=1.0`）；空表示关闭回声散尾。
    pub a_echo_send: Automation,
    /// 节拍回声延迟时长（秒），常用 A 曲的 1/2 拍或 3/4 拍。
    pub echo_delay_secs: f32,
    /// 节拍回声反馈比（`0.0..=0.82`）。
    pub echo_feedback: f32,
    /// 过渡期 B 轨对拍变速模式。
    pub tempo_mode: TempoMode,
    /// 重叠期 B 轨速度倍率（`bpm_a / eff_b_bpm`）；0 或 1 表示原速不变。
    pub b_tempo_ratio: f32,
    /// 重叠结束后 B 轨平滑滑回 `1.0×` 原速所需时长。
    pub b_tempo_release: Duration,
    /// 为匹配响度施加在 B 上的初始增益修正（dB），过渡后平滑回归 0 dB。
    pub b_trim_db: f32,
    /// 过渡结束后 `b_trim_db` 回归 0 dB 所需时长。
    pub b_trim_release: Duration,
}

impl MixPlan {
    pub fn overlap_duration(&self) -> Duration {
        self.out_end.saturating_sub(self.out_start)
    }

    /// 返回生效的 B 轨速度倍率；未启用变速或值无效时返回 `1.0`。
    pub fn effective_tempo_ratio(&self) -> f32 {
        if self.tempo_mode == TempoMode::Off
            || !self.b_tempo_ratio.is_finite()
            || self.b_tempo_ratio <= 0.0
        {
            return 1.0;
        }
        let clamped = self.b_tempo_ratio.clamp(MIN_TEMPO_RATIO, MAX_TEMPO_RATIO);
        if (clamped - 1.0).abs() < 0.001 {
            1.0
        } else {
            clamped
        }
    }

    /// 计划内容的稳定指纹，供上层判断预载计划是否变化。
    pub fn fingerprint(&self) -> u64 {
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        let mut mix = |bytes: &[u8]| {
            for &b in bytes {
                h ^= u64::from(b);
                h = h.wrapping_mul(0x0000_0100_0000_01b3);
            }
        };
        mix(&[self.kind as u8, self.style as u8, self.tempo_mode as u8]);
        mix(&(self.out_start.as_nanos() as u64).to_le_bytes());
        mix(&(self.out_end.as_nanos() as u64).to_le_bytes());
        mix(&(self.in_start.as_nanos() as u64).to_le_bytes());
        mix(&self.echo_delay_secs.to_bits().to_le_bytes());
        mix(&self.echo_feedback.to_bits().to_le_bytes());
        mix(&self.b_tempo_ratio.to_bits().to_le_bytes());
        mix(&(self.b_tempo_release.as_nanos() as u64).to_le_bytes());
        mix(&self.b_trim_db.to_bits().to_le_bytes());
        mix(&(self.b_trim_release.as_nanos() as u64).to_le_bytes());
        for curve in [
            &self.a_gain,
            &self.b_gain,
            &self.a_low_db,
            &self.b_low_db,
            &self.a_mid_db,
            &self.b_mid_db,
            &self.a_high_db,
            &self.b_high_db,
            &self.a_filter,
            &self.b_filter,
            &self.a_echo_send,
        ] {
            mix(&(curve.points().len() as u64).to_le_bytes());
            for p in curve.points() {
                mix(&p.t.to_bits().to_le_bytes());
                mix(&p.value.to_bits().to_le_bytes());
            }
        }
        h
    }
}

/// 预载下一首的载荷，含曲目路径与可选混音计划。`mix` 为 `None` 时执行纯无缝换轨。
#[derive(Clone, Debug, PartialEq)]
pub struct NextTrack {
    pub path: PathBuf,
    pub mix: Option<MixPlan>,
}

impl NextTrack {
    pub fn gapless(path: PathBuf) -> Self {
        Self { path, mix: None }
    }

    pub fn mixed(path: PathBuf, mix: MixPlan) -> Self {
        Self {
            path,
            mix: Some(mix),
        }
    }
}

/// B 曲目的响度补偿渐变。过渡期间保持 `initial_gain`，过渡结束后在 `release_frames` 内线性回到 1.0。
#[derive(Clone, Copy, Debug)]
pub struct TrimRamp {
    initial_gain: f32,
    release_frames: u64,
    elapsed_frames: u64,
}

impl TrimRamp {
    pub fn from_db(db: f32, release: Duration, sample_rate: u32) -> Self {
        let clamped = db.clamp(-12.0, 12.0);
        let initial_gain = if clamped.abs() < 0.05 {
            1.0
        } else {
            10.0_f32.powf(clamped / 20.0)
        };
        let release_frames = (release.as_secs_f64() * f64::from(sample_rate)).round() as u64;
        Self {
            initial_gain,
            release_frames,
            elapsed_frames: 0,
        }
    }

    pub fn is_active(&self) -> bool {
        (self.initial_gain - 1.0).abs() >= 1e-4 && self.elapsed_frames < self.release_frames
    }

    /// 过渡期内的固定响度补偿倍率。
    #[inline]
    pub fn hold_gain(&self) -> f32 {
        self.initial_gain
    }

    /// 过渡结束后的逐帧释放倍率。到达 `release_frames` 后恒返回 1.0。
    #[inline]
    pub fn next_release_gain(&mut self) -> f32 {
        if (self.initial_gain - 1.0).abs() < 1e-4 || self.release_frames == 0 {
            self.elapsed_frames = self.release_frames;
            return 1.0;
        }
        if self.elapsed_frames >= self.release_frames {
            return 1.0;
        }
        let t = self.elapsed_frames as f32 / self.release_frames as f32;
        self.elapsed_frames += 1;
        self.initial_gain + (1.0 - self.initial_gain) * t
    }
}

/// 编译为设备帧号的混音计划，音频回调只做整数比较与零分配查表。
#[derive(Clone, Debug)]
pub struct CompiledMix {
    /// 相对当前段起点的触发帧号。
    pub start_frame: u64,
    /// 重叠长度，单位帧。
    pub length_frames: u64,
    pub a_gain: CompiledAutomation,
    pub b_gain: CompiledAutomation,
    pub a_low_db: Option<CompiledAutomation>,
    pub b_low_db: Option<CompiledAutomation>,
    pub a_mid_db: Option<CompiledAutomation>,
    pub b_mid_db: Option<CompiledAutomation>,
    pub a_high_db: Option<CompiledAutomation>,
    pub b_high_db: Option<CompiledAutomation>,
    pub a_filter: Option<CompiledAutomation>,
    pub b_filter: Option<CompiledAutomation>,
    pub a_echo_send: Option<CompiledAutomation>,
    pub echo_delay_secs: f32,
    pub echo_feedback: f32,
    pub tempo_mode: TempoMode,
    pub b_tempo_ratio: f32,
    pub b_tempo_release_frames: u64,
    pub b_trim: TrimRamp,
}

impl CompiledMix {
    /// 按当前段起点 `base` 与输出采样率编译计划。`Gapless` 返回 `None`。
    pub fn compile(plan: &MixPlan, base: Duration, sample_rate: u32) -> Option<Self> {
        if plan.kind == MixKind::Gapless {
            return None;
        }
        let rate = f64::from(sample_rate.max(1));
        let start_offset = plan.out_start.saturating_sub(base);
        let start_frame = (start_offset.as_secs_f64() * rate).round() as u64;
        let length_frames = (plan.overlap_duration().as_secs_f64() * rate).round() as u64;

        let opt = |auto: &Automation| (!auto.is_empty()).then(|| auto.compile());
        let a_echo_send = if !plan.a_echo_send.is_empty()
            && plan.echo_delay_secs >= 0.08
            && plan.echo_feedback > 0.01
        {
            Some(plan.a_echo_send.compile())
        } else {
            None
        };

        let ratio = plan.effective_tempo_ratio();
        let (tempo_mode, b_tempo_ratio, b_tempo_release_frames) =
            if plan.tempo_mode != TempoMode::Off && (ratio - 1.0).abs() >= 0.001 {
                let release_frames = (plan.b_tempo_release.as_secs_f64() * rate).round() as u64;
                (plan.tempo_mode, ratio, release_frames)
            } else {
                (TempoMode::Off, 1.0, 0)
            };

        Some(Self {
            start_frame,
            length_frames,
            a_gain: plan.a_gain.compile(),
            b_gain: plan.b_gain.compile(),
            a_low_db: opt(&plan.a_low_db),
            b_low_db: opt(&plan.b_low_db),
            a_mid_db: opt(&plan.a_mid_db),
            b_mid_db: opt(&plan.b_mid_db),
            a_high_db: opt(&plan.a_high_db),
            b_high_db: opt(&plan.b_high_db),
            a_filter: opt(&plan.a_filter),
            b_filter: opt(&plan.b_filter),
            a_echo_send,
            echo_delay_secs: plan.echo_delay_secs,
            echo_feedback: plan.echo_feedback,
            tempo_mode,
            b_tempo_ratio,
            b_tempo_release_frames,
            b_trim: TrimRamp::from_db(plan.b_trim_db, plan.b_trim_release, sample_rate),
        })
    }
}
