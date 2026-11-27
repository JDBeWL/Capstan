//! 自动混音规划器。综合 BPM 关系、节拍置信度与稳定性、能量走势、响度差、调性与专辑连续性，
//! 产出音频层可直接执行的 [`MixPlan`]。

use std::time::Duration;

use capstan_audio::{Automation, ControlPoint, CurvePreset, MixKind, MixPlan, MixStyle, TempoMode};
use serde::{Deserialize, Serialize};

use crate::model::TrackAnalysis;

/// 不变速对拍时允许的端到端节拍漂移预算（锚定在重叠中点，单侧最大误差为本值）。
const DRIFT_BUDGET_SECS: f32 = 0.040;
/// 允许进入对拍与小节对齐的最大相对速度偏差（8%）。
const MAX_TEMPO_DELTA: f32 = 0.08;
/// 微变速与保音高拉伸的分界线（±3%）。
const VARISPEED_MAX_DELTA: f32 = 0.03;
/// 低于此偏差（0.2%）时不启动变速处理，保持零开销直通。
const TEMPO_SYNC_DEADBAND: f32 = 0.002;
/// 参与对拍的最低速度置信度与速度稳定度门槛。
const MIN_CONFIDENCE: f32 = 0.40;
const MIN_STABILITY: f32 = 0.60;
/// 低频交换切低幅度（dB）。
const LOW_SWAP_CUT_DB: f32 = -24.0;
/// 响度匹配的最大补偿幅度（±4 dB）。
const MAX_LOUDNESS_TRIM_DB: f32 = 4.0;

/// 自动混音的过渡风格偏好。选择 `Auto` 时由规划器结合曲目结构自动挑选配方。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum StylePreference {
    #[default]
    Auto,
    Blend,
    Rise,
    Fade,
    Wash,
}

impl StylePreference {
    pub const ALL: [Self; 5] = [Self::Auto, Self::Blend, Self::Rise, Self::Fade, Self::Wash];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Auto => "自动",
            Self::Blend => "融合",
            Self::Rise => "推升",
            Self::Fade => "淡化",
            Self::Wash => "回声切",
        }
    }

    pub const fn fixed_style(self) -> Option<MixStyle> {
        match self {
            Self::Auto => None,
            Self::Blend => Some(MixStyle::Blend),
            Self::Rise => Some(MixStyle::Rise),
            Self::Fade => Some(MixStyle::Fade),
            Self::Wash => Some(MixStyle::Wash),
        }
    }
}

/// 允许提前离开当前曲目的裁剪程度。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum TrimLevel {
    /// 只裁掉尾部静音与尾奏低能区，不早于 `outro_start`。
    Off,
    /// 最多提前 1 个乐句，且不超过 30 秒。
    #[default]
    Light,
    /// 最多提前 60 秒。
    Medium,
    /// 可在最后 40% 时长内的任意乐句边界切出。
    Aggressive,
}

impl TrimLevel {
    pub const ALL: [Self; 4] = [Self::Off, Self::Light, Self::Medium, Self::Aggressive];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Off => "不裁剪",
            Self::Light => "轻度",
            Self::Medium => "中度",
            Self::Aggressive => "积极",
        }
    }
}

/// 过渡长度偏好。对拍时按小节计，非对拍交叉淡化时退回对应秒数。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum TransitionLength {
    Short,
    #[default]
    Medium,
    Long,
}

impl TransitionLength {
    pub const ALL: [Self; 3] = [Self::Short, Self::Medium, Self::Long];

    pub const fn bars(self) -> usize {
        match self {
            Self::Short => 4,
            Self::Medium => 8,
            Self::Long => 16,
        }
    }

    pub const fn fallback_secs(self) -> f32 {
        match self {
            Self::Short => 4.0,
            Self::Medium => 6.0,
            Self::Long => 10.0,
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::Short => "短（4 小节）",
            Self::Medium => "中（8 小节）",
            Self::Long => "长（16 小节）",
        }
    }
}

/// 持久化于 `settings.json` 的自动混音偏好。`enabled` 默认关闭，保持原有无缝直连行为。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AutoMixPrefs {
    pub enabled: bool,
    pub style: StylePreference,
    pub trim: TrimLevel,
    pub length: TransitionLength,
    pub curve: CurvePreset,
    pub beat_match: bool,
    pub tempo_sync: bool,
    pub low_swap: bool,
    pub loudness_match: bool,
    pub respect_album_continuity: bool,
    pub background_analyze: bool,
}

impl Default for AutoMixPrefs {
    fn default() -> Self {
        Self {
            enabled: false,
            style: StylePreference::Auto,
            trim: TrimLevel::Light,
            length: TransitionLength::Medium,
            curve: CurvePreset::EqualPower,
            beat_match: true,
            tempo_sync: true,
            low_swap: true,
            loudness_match: true,
            respect_album_continuity: true,
            background_analyze: false,
        }
    }
}

/// 规划本次过渡时的实时上下文。
#[derive(Clone, Copy, Debug)]
pub struct PlanContext {
    pub current_position: Duration,
    /// 两首是否属于同一张专辑的相邻音轨。
    pub same_album_consecutive: bool,
    /// 输出后端要求的最小提前量（共享 3 秒，独占 4 秒）。
    pub min_lead: Duration,
}

impl Default for PlanContext {
    fn default() -> Self {
        Self {
            current_position: Duration::ZERO,
            same_album_consecutive: false,
            min_lead: Duration::from_secs(3),
        }
    }
}

/// 为当前曲（A）与下一首（B）规划自动混音方案。
pub fn plan(
    a: &TrackAnalysis,
    b: &TrackAnalysis,
    prefs: &AutoMixPrefs,
    override_plan: Option<&MixPlan>,
    ctx: PlanContext,
) -> MixPlan {
    if let Some(custom) = override_plan {
        return clamp_override(custom, a, b, ctx);
    }

    // 同专辑连续音轨且连接处无静音（如现场专辑与概念专辑连轨），保持无缝直连。
    if prefs.respect_album_continuity
        && ctx.same_album_consecutive
        && a.tail_silence < 0.25
        && b.lead_silence < 0.25
    {
        return gapless_plan(a);
    }

    let min_out_lead = ctx.current_position.as_secs_f32() + ctx.min_lead.as_secs_f32();
    let audible_end_a = (a.duration - a.tail_silence).clamp(0.0, a.duration);
    if audible_end_a <= min_out_lead + 0.5 || b.duration < 2.0 {
        return gapless_plan(a);
    }

    // 评估节奏匹配与漂移预算。
    let confident = prefs.beat_match
        && a.bpm_confidence >= MIN_CONFIDENCE
        && b.bpm_confidence >= MIN_CONFIDENCE
        && a.tempo_stability >= MIN_STABILITY
        && b.tempo_stability >= MIN_STABILITY
        && a.beats.len() >= 8
        && b.beats.len() >= 8;

    let (eff_b_bpm, tempo_delta, drift_max_bars) = if confident {
        tempo_compatibility(a.bpm, b.bpm)
    } else {
        (b.bpm, 1.0, 0)
    };
    // 开启变速对拍时，±8% 内不再受 40 ms 不变速漂移预算截断，可解锁完整的 4/8/16 小节锁拍。
    let can_tempo_sync = prefs.tempo_sync && confident && tempo_delta <= MAX_TEMPO_DELTA;
    let n_max_bars = if can_tempo_sync {
        prefs.length.bars().max(16)
    } else {
        drift_max_bars
    };

    let mut kind = if !confident {
        MixKind::Crossfade
    } else if tempo_delta <= MAX_TEMPO_DELTA && n_max_bars >= 2 {
        MixKind::BeatMatched
    } else if tempo_delta <= MAX_TEMPO_DELTA && n_max_bars >= 1 {
        MixKind::BarCrossfade
    } else {
        // 速度差过大时，若 A 出点能量高且 B 入点起拍有力，采用小节线硬切；否则普通交叉淡化。
        let a_bar = a
            .bar_at(a.outro_start)
            .unwrap_or(a.bar_count().saturating_sub(1));
        let b_bar = b.bar_at(b.lead_silence + 0.1).unwrap_or(0);
        if a.bar_total_energy(a_bar) >= 0.60 && b.bar_total_energy(b_bar) >= 0.55 {
            MixKind::Cut
        } else {
            MixKind::Crossfade
        }
    };

    // 确定初步重叠时长；回声切需要 4 拍留出散尾空间。
    let bar_a = a.bar_period();
    let mut overlap_secs = match kind {
        MixKind::BeatMatched => {
            let bars = prefs.length.bars().min(n_max_bars).max(2);
            bars as f32 * bar_a
        }
        MixKind::BarCrossfade => {
            let bars = n_max_bars.clamp(1, 2);
            bars as f32 * bar_a
        }
        MixKind::Cut => (a.beat_period() * 4.0).clamp(2.2, 4.5),
        MixKind::Crossfade => prefs.length.fallback_secs(),
        MixKind::Gapless => return gapless_plan(a),
    };
    if prefs.style == StylePreference::Wash {
        overlap_secs = overlap_secs.min((a.beat_period() * 8.0).clamp(2.4, 5.0));
    }

    // 限制重叠不超过任意一首有效时长的一半，避免短曲整首都在混音。
    let max_overlap = ((audible_end_a - min_out_lead).max(0.5))
        .min((b.duration - b.lead_silence).max(0.5) * 0.45);
    if overlap_secs > max_overlap {
        if matches!(kind, MixKind::BeatMatched | MixKind::BarCrossfade) {
            let fit_bars = (max_overlap / bar_a).floor() as usize;
            if fit_bars >= 2 && kind == MixKind::BeatMatched {
                overlap_secs = fit_bars as f32 * bar_a;
            } else if fit_bars >= 1 {
                kind = MixKind::BarCrossfade;
                overlap_secs = fit_bars as f32 * bar_a;
            } else {
                kind = MixKind::Crossfade;
                overlap_secs = max_overlap.min(prefs.length.fallback_secs());
            }
        } else {
            overlap_secs = max_overlap;
        }
    }

    // 根据速度关系确定变速对拍参数，以便联合评估 B 的实际媒体重叠时长。
    let (tempo_mode, b_tempo_ratio, b_tempo_release) = if can_tempo_sync
        && matches!(kind, MixKind::BeatMatched | MixKind::BarCrossfade)
        && eff_b_bpm > 1.0
        && a.bpm > 1.0
    {
        let ratio = (a.bpm / eff_b_bpm).clamp(0.88, 1.12);
        let diff = (ratio - 1.0).abs();
        if diff < TEMPO_SYNC_DEADBAND {
            (TempoMode::Off, 1.0, Duration::ZERO)
        } else if diff <= VARISPEED_MAX_DELTA {
            let release = (diff * 60.0).clamp(1.5, 4.0);
            (
                TempoMode::Varispeed,
                ratio,
                Duration::from_secs_f32(release),
            )
        } else {
            let release = (diff * 60.0).clamp(2.0, 5.0);
            (
                TempoMode::PitchPreserving,
                ratio,
                Duration::from_secs_f32(release),
            )
        }
    } else {
        (TempoMode::Off, 1.0, Duration::ZERO)
    };

    // 联合搜索 A 出点与 B 入点，优先避开双曲人声重叠并让 B 人声落于交接处。
    let (out_start, mut in_start) = pick_transition_pair(
        a,
        b,
        prefs.trim,
        kind,
        overlap_secs,
        b_tempo_ratio,
        min_out_lead,
        audible_end_a,
    );
    let out_end = (out_start + overlap_secs).min(a.duration);
    let actual_overlap = (out_end - out_start).max(0.0);
    if actual_overlap < 0.2 {
        return gapless_plan(a);
    }

    // 变速对拍时 B 的消费时长按 `b_tempo_ratio` 折算，入点直接锁死首拍无需中点漂移居中。
    let b_media_overlap = actual_overlap * b_tempo_ratio;
    if tempo_mode == TempoMode::Off
        && kind == MixKind::BeatMatched
        && eff_b_bpm > 1.0
        && a.bpm > 1.0
    {
        let p_a = 60.0 / a.bpm;
        let p_b = 60.0 / eff_b_bpm;
        let beats_in_overlap = (actual_overlap / p_a).round();
        // 把两首的节拍对齐锚定在重叠中点（两首音量相当处），两端分摊一半漂移。
        let center_shift = 0.5 * beats_in_overlap * (p_a - p_b);
        in_start = (in_start - center_shift).clamp(0.0, (b.duration - actual_overlap).max(0.0));
    }

    let vocal_clash =
        measure_vocal_clash(a, out_start, actual_overlap, b, in_start, b_media_overlap);

    // 选择风格配方并生成音量、三段均衡、扫频滤镜、回声与响度补偿。
    let (b_trim_db, b_trim_release) =
        if prefs.loudness_match && a.loudness_lufs > -60.0 && b.loudness_lufs > -60.0 {
            let diff = (a.loudness_lufs - b.loudness_lufs)
                .clamp(-MAX_LOUDNESS_TRIM_DB, MAX_LOUDNESS_TRIM_DB);
            let release_secs = (a.bar_period() * 2.0).clamp(2.0, 8.0);
            (diff, Duration::from_secs_f32(release_secs))
        } else {
            (0.0, Duration::ZERO)
        };

    let style = resolve_style(
        a,
        b,
        kind,
        tempo_delta,
        out_start,
        in_start,
        b_media_overlap,
        vocal_clash,
        prefs,
    );

    let mut built = MixPlan {
        kind,
        style,
        out_start: Duration::from_secs_f32(out_start),
        out_end: Duration::from_secs_f32(out_end),
        in_start: Duration::from_secs_f32(in_start),
        tempo_mode,
        b_tempo_ratio,
        b_tempo_release,
        b_trim_db,
        b_trim_release,
        ..MixPlan::default()
    };
    apply_style_recipe(&mut built, style, a, b, prefs);
    built
}

/// 根据曲目能量走势、速度差、调性兼容度与人声冲突度，决定本次过渡的风格配方。
#[allow(clippy::too_many_arguments)]
fn resolve_style(
    a: &TrackAnalysis,
    b: &TrackAnalysis,
    kind: MixKind,
    tempo_delta: f32,
    out_start: f32,
    in_start: f32,
    overlap: f32,
    vocal_clash: f32,
    prefs: &AutoMixPrefs,
) -> MixStyle {
    if let Some(fixed) = prefs.style.fixed_style() {
        return fixed;
    }
    if kind == MixKind::Cut {
        return MixStyle::Wash;
    }

    let a_bar = a
        .bar_at(out_start)
        .unwrap_or(a.bar_count().saturating_sub(1));
    let b_bar = b.bar_at(in_start).unwrap_or(0);
    let b_after_bar = b.bar_at(in_start + overlap).unwrap_or(b_bar);

    let a_energy = a.bar_total_energy(a_bar);
    let b_in_energy = b.bar_total_energy(b_bar);
    let b_after_energy = b.bar_total_energy(b_after_bar);
    let key_compat = match (a.key, b.key) {
        (Some(ka), Some(kb)) => ka.compatibility(&kb),
        _ => 0.65,
    };

    if kind == MixKind::Crossfade {
        // 大速差或调性反差且双曲能量较高时，改用回声切避免长时间打架。
        if a_energy >= 0.55 && b_in_energy >= 0.45 && (key_compat <= 0.25 || tempo_delta > 0.15) {
            return MixStyle::Wash;
        }
        return MixStyle::Fade;
    }

    // 双曲明显人声重叠加调性冲突时，改用回声切速战速决防止抢唱打架。
    if vocal_clash >= 0.45 && key_compat <= 0.35 {
        return MixStyle::Wash;
    }

    // 对拍或小节淡化时，若 B 入点随后进入高能落点，使用推升配方拉起张力。
    if b_after_energy - b_in_energy >= 0.16 && b_after_energy >= 0.55 {
        return MixStyle::Rise;
    }

    // 调性显著冲突且双曲中频都很满时，用回声切替代长重叠防止和声打架。
    if key_compat <= 0.20 && a.bar_mid_energy(a_bar) >= 0.55 && b.bar_mid_energy(b_bar) >= 0.55 {
        return MixStyle::Wash;
    }

    MixStyle::Blend
}

/// 将指定风格配方写入已有 `MixPlan` 的音量、三段均衡、扫频滤镜与回声字段。
pub fn apply_style_recipe(
    plan: &mut MixPlan,
    style: MixStyle,
    a: &TrackAnalysis,
    b: &TrackAnalysis,
    prefs: &AutoMixPrefs,
) {
    plan.style = style;
    if style == MixStyle::Custom {
        return;
    }

    plan.a_low_db = Automation::empty();
    plan.b_low_db = Automation::empty();
    plan.a_mid_db = Automation::empty();
    plan.b_mid_db = Automation::empty();
    plan.a_high_db = Automation::empty();
    plan.b_high_db = Automation::empty();
    plan.a_filter = Automation::empty();
    plan.b_filter = Automation::empty();
    plan.a_echo_send = Automation::empty();
    plan.echo_delay_secs = 0.0;
    plan.echo_feedback = 0.0;

    let overlap = plan.overlap_duration().as_secs_f32().max(0.2);
    let b_overlap = overlap * plan.effective_tempo_ratio();
    let vocal_clash = measure_vocal_clash(
        a,
        plan.out_start.as_secs_f32(),
        overlap,
        b,
        plan.in_start.as_secs_f32(),
        b_overlap,
    );
    let allow_low = prefs.low_swap && overlap >= 1.0;

    match style {
        MixStyle::Blend => {
            plan.a_gain = Automation::preset_out(prefs.curve);
            plan.b_gain = Automation::preset_in(prefs.curve);
            if allow_low {
                let half_w = match plan.kind {
                    MixKind::BeatMatched => (0.5 * a.beat_period() / overlap).clamp(0.03, 0.12),
                    MixKind::BarCrossfade => (a.beat_period() / overlap).clamp(0.06, 0.16),
                    _ => (1.2 / overlap).clamp(0.08, 0.22),
                };
                plan.a_low_db = Automation::low_swap_out(0.5, half_w, LOW_SWAP_CUT_DB);
                plan.b_low_db = Automation::low_swap_in(0.5, half_w, LOW_SWAP_CUT_DB);
            }
            if overlap >= 2.0 {
                // 人声重叠明显时加深并提前衰减 A 中频，给 B 主唱让出清晰频段。
                let (mid_start, mid_end, mid_cut) = if vocal_clash >= 0.30 {
                    (0.24, 0.70, -11.5)
                } else {
                    (0.35, 0.78, -8.0)
                };
                plan.a_mid_db = Automation::ramp(mid_start, 0.0, mid_end, mid_cut);
                plan.b_mid_db = Automation::ramp(0.18, -6.0, 0.58, 0.0);
                plan.a_high_db = Automation::ramp(0.55, 0.0, 0.92, -6.0);
                plan.b_high_db = Automation::ramp(0.12, -5.0, 0.48, 0.0);
            }
        }
        MixStyle::Rise => {
            plan.a_gain = Automation::new(vec![
                ControlPoint::new(0.0, 1.0),
                ControlPoint::new(0.62, 0.92),
                ControlPoint::new(0.82, 0.45),
                ControlPoint::new(0.94, 0.0),
                ControlPoint::new(1.0, 0.0),
            ]);
            plan.b_gain = Automation::new(vec![
                ControlPoint::new(0.0, 0.0),
                ControlPoint::new(0.25, 0.35),
                ControlPoint::new(0.72, 0.78),
                ControlPoint::new(0.88, 1.0),
                ControlPoint::new(1.0, 1.0),
            ]);
            if allow_low {
                let half_w = (0.5 * a.beat_period() / overlap).clamp(0.03, 0.10);
                plan.a_low_db = Automation::low_swap_out(0.72, half_w, LOW_SWAP_CUT_DB);
                plan.b_low_db = Automation::low_swap_in(0.72, half_w, LOW_SWAP_CUT_DB);
            }
            let mid_cut = if vocal_clash >= 0.30 { -11.0 } else { -8.0 };
            plan.a_mid_db = Automation::ramp(0.45, 0.0, 0.86, mid_cut);
            plan.a_filter = Automation::ramp(0.15, 0.0, 0.86, 0.62);
            plan.b_filter = Automation::ramp(0.0, -0.38, 0.78, 0.0);
        }
        MixStyle::Fade => {
            plan.a_gain = Automation::preset_out(prefs.curve);
            plan.b_gain = Automation::preset_in(prefs.curve);
            if allow_low {
                let half_w = (1.2 / overlap).clamp(0.10, 0.22);
                plan.a_low_db = Automation::low_swap_out(0.5, half_w, LOW_SWAP_CUT_DB);
                plan.b_low_db = Automation::low_swap_in(0.5, half_w, LOW_SWAP_CUT_DB);
            }
            if vocal_clash >= 0.30 && overlap >= 2.0 {
                plan.a_mid_db = Automation::ramp(0.28, 0.0, 0.76, -10.0);
            }
            plan.a_filter = Automation::ramp(0.35, 0.0, 0.95, -0.34);
        }
        MixStyle::Wash => {
            plan.a_gain = Automation::new(vec![
                ControlPoint::new(0.0, 1.0),
                ControlPoint::new(0.18, 0.92),
                ControlPoint::new(0.34, 0.0),
                ControlPoint::new(1.0, 0.0),
            ]);
            plan.b_gain = Automation::new(vec![
                ControlPoint::new(0.0, 0.0),
                ControlPoint::new(0.12, 0.20),
                ControlPoint::new(0.36, 1.0),
                ControlPoint::new(1.0, 1.0),
            ]);
            if allow_low {
                plan.a_low_db = Automation::low_swap_out(0.22, 0.08, LOW_SWAP_CUT_DB);
                plan.b_low_db = Automation::low_swap_in(0.22, 0.08, -18.0);
            }
            plan.a_filter = Automation::ramp(0.05, 0.0, 0.36, 0.48);
            plan.a_echo_send = Automation::pulse(0.02, 0.12, 0.28, 0.38, 0.72);
            plan.echo_delay_secs = (a.beat_period() * 0.75).clamp(0.18, 0.75);
            plan.echo_feedback = 0.56;
        }
        MixStyle::Custom => {}
    }
}

/// 计算 B 在 `{1×, 2×, 0.5×}` 倍率中与 A 最接近的有效速度、相对偏差与允许的最大对拍小节数。
fn tempo_compatibility(bpm_a: f32, bpm_b: f32) -> (f32, f32, usize) {
    if bpm_a <= 1.0 || bpm_b <= 1.0 {
        return (bpm_b, 1.0, 0);
    }
    let mut best_b = bpm_b;
    let mut best_delta = f32::INFINITY;
    for mult in [1.0_f32, 2.0, 0.5] {
        let cand = bpm_b * mult;
        let delta = ((bpm_a - cand) / bpm_a).abs();
        if delta < best_delta {
            best_delta = delta;
            best_b = cand;
        }
    }

    let p_a = 60.0 / bpm_a;
    let p_b = 60.0 / best_b;
    let dp = (p_a - p_b).abs();
    let max_beats = if dp < 1e-5 {
        128
    } else {
        ((2.0 * DRIFT_BUDGET_SECS) / dp).floor() as usize
    };
    (best_b, best_delta, max_beats / 4)
}

/// 联合评估候选 `(out_start, in_start)` 组合，平衡能量回落、乐句完整、人声交接与人声冲突惩罚。
#[allow(clippy::too_many_arguments)]
fn pick_transition_pair(
    a: &TrackAnalysis,
    b: &TrackAnalysis,
    trim: TrimLevel,
    kind: MixKind,
    overlap_secs: f32,
    b_tempo_ratio: f32,
    min_lead_time: f32,
    audible_end: f32,
) -> (f32, f32) {
    let phrase_dur = f32::from(a.phrase_bars.max(4)) * a.bar_period();
    let trim_floor = match trim {
        TrimLevel::Off => a.outro_start.min(audible_end),
        TrimLevel::Light => {
            (audible_end - phrase_dur.max(overlap_secs).min(30.0)).min(a.outro_start)
        }
        TrimLevel::Medium => (audible_end - 60.0).min(a.outro_start),
        TrimLevel::Aggressive => (a.duration * 0.60).min(a.outro_start),
    };
    let lower = trim_floor.max(min_lead_time).clamp(0.0, audible_end);
    // 优先在能完整容纳 `overlap_secs` 的区间内找网格，避免长过渡被曲尾过早截断。
    let full_upper = (a.duration - overlap_secs + 0.75).clamp(lower, audible_end);
    let fallback_upper = (audible_end - overlap_secs * 0.5).max(lower);
    let grid_aligned = matches!(
        kind,
        MixKind::BeatMatched | MixKind::BarCrossfade | MixKind::Cut
    );

    let collect_out = |hi: f32| -> Vec<f32> {
        let mut list: Vec<f32> = a
            .phrase_boundaries()
            .into_iter()
            .filter(|&t| t >= lower && t <= hi)
            .collect();
        if list.is_empty() && grid_aligned {
            list = a
                .downbeats()
                .into_iter()
                .filter(|&t| t >= lower && t <= hi)
                .collect();
        }
        // A 的人声结束点若落在合法区间内，作为额外候选纳入评估。
        if a.vocal_end > 0.0 {
            let vocal_cand = if grid_aligned {
                a.snap_to_grid(a.vocal_end, false)
            } else {
                a.vocal_end
            };
            if vocal_cand >= lower
                && vocal_cand <= hi
                && list.iter().all(|&t| (t - vocal_cand).abs() > 0.25)
            {
                list.push(vocal_cand);
            }
        }
        list
    };

    let mut out_candidates = collect_out(full_upper);
    if out_candidates.is_empty() && fallback_upper > full_upper {
        out_candidates = collect_out(fallback_upper);
    }
    if out_candidates.is_empty() {
        out_candidates.push((audible_end - overlap_secs).max(lower).min(audible_end));
    }

    let key_compat = match (a.key, b.key) {
        (Some(ka), Some(kb)) => ka.compatibility(&kb),
        _ => 0.5,
    };
    let span = (audible_end - lower).max(1.0);
    let base_entry = if grid_aligned {
        b.next_downbeat(b.lead_silence).unwrap_or(b.lead_silence)
    } else {
        b.lead_silence
    };

    let mut best_pair = (
        out_candidates[0],
        base_entry.clamp(0.0, (b.duration - overlap_secs * b_tempo_ratio).max(0.0)),
    );
    let mut best_score = f32::NEG_INFINITY;

    for &out_t in &out_candidates {
        let out_end = (out_t + overlap_secs).min(a.duration);
        let act_ov = (out_end - out_t).max(0.2);
        let b_ov = act_ov * b_tempo_ratio;
        let in_candidates = collect_in_candidates(b, grid_aligned, base_entry, b_ov);

        let bar = a.bar_at(out_t).unwrap_or(0);
        let prev_e = bar
            .checked_sub(1)
            .map_or_else(|| a.bar_total_energy(bar), |p| a.bar_total_energy(p));
        let cur_e = a.bar_total_energy(bar);
        let energy_drop = (prev_e - cur_e).max(0.0);
        let closeness = 1.0 - ((audible_end - out_t) / span).clamp(0.0, 1.0);
        let a_vocal_end_bonus = if a.vocal_end > 0.0 && out_t >= a.vocal_end - 0.5 {
            0.22
        } else {
            0.0
        };
        let a_overlap_vocal = mean_vocal_in_range(a, out_t, act_ov);

        for in_t in in_candidates {
            let b_end = in_t + b_ov;
            let clash = measure_vocal_clash(a, out_t, act_ov, b, in_t, b_ov);

            let b_handoff_bonus = if b.vocal_start > base_entry + 1.0 {
                if (b_end - b.vocal_start).abs() <= b.bar_period() * 1.1 {
                    0.25
                } else if b_end < b.vocal_start {
                    0.10
                } else {
                    0.0
                }
            } else {
                0.0
            };

            let b_intro_align_bonus = if b.intro_end > base_entry + b_ov + 1.0
                && (b_end - b.intro_end).abs() <= b.bar_period() * 1.1
            {
                0.12
            } else {
                0.0
            };

            let score = 0.30 * energy_drop
                + 0.25 * closeness
                + 0.12 * a.bpm_confidence
                + 0.12 * key_compat
                + a_vocal_end_bonus
                + b_handoff_bonus
                + b_intro_align_bonus
                - 0.22 * a_overlap_vocal
                - 0.65 * clash;

            if score > best_score {
                best_score = score;
                best_pair = (out_t, in_t);
            }
        }
    }

    best_pair
}

fn collect_in_candidates(
    b: &TrackAnalysis,
    grid_aligned: bool,
    base_entry: f32,
    b_media_overlap: f32,
) -> Vec<f32> {
    let max_in = (b.duration - b_media_overlap).max(0.0);
    let mut out = vec![base_entry.clamp(0.0, max_in)];

    let mut try_push = |target_end: f32| {
        if target_end <= base_entry + b_media_overlap + 1.0 {
            return;
        }
        let desired = (target_end - b_media_overlap).max(base_entry);
        let aligned = if grid_aligned {
            b.snap_to_grid(desired, false)
        } else {
            desired
        };
        if aligned >= b.lead_silence
            && aligned + b_media_overlap <= b.duration + 0.05
            && out.iter().all(|&t| (t - aligned).abs() > 0.25)
        {
            out.push(aligned.clamp(0.0, max_in));
        }
    };

    try_push(b.intro_end);
    try_push(b.vocal_start);
    out
}

/// 沿重叠区间中心加权采样双曲人声存在度的乘积，返回 0..=1 的同步人声冲突度。
fn measure_vocal_clash(
    a: &TrackAnalysis,
    out_start: f32,
    a_overlap: f32,
    b: &TrackAnalysis,
    in_start: f32,
    b_overlap: f32,
) -> f32 {
    const SAMPLES: usize = 8;
    let mut num = 0.0_f32;
    let mut den = 0.0_f32;
    for k in 0..SAMPLES {
        let u = (k as f32 + 0.5) / SAMPLES as f32;
        let weight = (1.0 - (2.0 * u - 1.0).abs()).max(0.25);
        let va = a.vocal_at_time(out_start + u * a_overlap);
        let vb = b.vocal_at_time(in_start + u * b_overlap);
        num += weight * va * vb;
        den += weight;
    }
    if den > 0.0 {
        (num / den).clamp(0.0, 1.0)
    } else {
        0.0
    }
}

fn mean_vocal_in_range(track: &TrackAnalysis, start: f32, span: f32) -> f32 {
    const SAMPLES: usize = 6;
    let sum: f32 = (0..SAMPLES)
        .map(|k| {
            let u = (k as f32 + 0.5) / SAMPLES as f32;
            track.vocal_at_time(start + u * span)
        })
        .sum();
    (sum / SAMPLES as f32).clamp(0.0, 1.0)
}

fn clamp_override(
    custom: &MixPlan,
    a: &TrackAnalysis,
    b: &TrackAnalysis,
    ctx: PlanContext,
) -> MixPlan {
    let min_lead = ctx.current_position.saturating_add(ctx.min_lead);
    if custom.kind != MixKind::Gapless && custom.out_start < min_lead {
        return gapless_plan(a);
    }
    let dur_a = Duration::from_secs_f32(a.duration.max(0.0));
    let dur_b = Duration::from_secs_f32(b.duration.max(0.0));
    let mut out = custom.clone();
    out.out_start = out.out_start.min(dur_a);
    out.out_end = out.out_end.clamp(out.out_start, dur_a);
    out.in_start = out.in_start.min(dur_b);
    out
}

/// 构造无重叠的无缝直连计划，音频层据此保持零修改衔接。
pub fn gapless_plan(a: &TrackAnalysis) -> MixPlan {
    let end = Duration::from_secs_f32(a.duration.max(0.0));
    MixPlan {
        kind: MixKind::Gapless,
        style: MixStyle::Fade,
        out_start: end,
        out_end: end,
        in_start: Duration::ZERO,
        a_gain: Automation::flat(1.0),
        b_gain: Automation::flat(1.0),
        ..MixPlan::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ANALYSIS_VERSION, KeyInfo, MusicalMode};
    use capstan_core::FileStamp;

    fn mock_track(bpm: f32, duration: f32, lufs: f32, tail_silence: f32) -> TrackAnalysis {
        let period = 60.0 / bpm;
        let mut beats = Vec::new();
        let mut t = 0.0_f32;
        while t < duration {
            beats.push(t);
            t += period;
        }
        let bar_count = beats.len().div_ceil(4);
        let bar_energy = (0..bar_count)
            .map(|b| {
                // 尾奏最后 16 小节能量回落。
                if b + 16 >= bar_count {
                    [100, 90, 90]
                } else {
                    [200, 180, 170]
                }
            })
            .collect();
        let outro_bar = bar_count.saturating_sub(16);
        let outro_start = beats.get(outro_bar * 4).copied().unwrap_or(duration * 0.8);

        TrackAnalysis {
            version: ANALYSIS_VERSION,
            stamp: FileStamp::default(),
            duration,
            bpm,
            bpm_confidence: 0.9,
            tempo_stability: 0.95,
            beats,
            downbeat_phase: 0,
            phrase_bars: 8,
            phrase_offset_bars: 0,
            bar_energy,
            bar_vocal: Vec::new(),
            beat_rms: vec![180; 64],
            waveform_rgb: Vec::new(),
            loudness_lufs: lufs,
            lead_silence: 0.0,
            tail_silence,
            intro_end: 0.0,
            outro_start,
            vocal_start: 0.0,
            vocal_end: outro_start,
            key: Some(KeyInfo {
                tonic: 0,
                mode: MusicalMode::Major,
                camelot_num: 8,
                camelot_letter: 'B',
                confidence: 0.8,
            }),
        }
    }

    #[test]
    fn matching_tempo_produces_beat_matched_at_preferred_length() {
        let a = mock_track(128.0, 200.0, -9.0, 0.5);
        let b = mock_track(128.0, 200.0, -11.0, 0.5);
        let prefs = AutoMixPrefs {
            enabled: true,
            length: TransitionLength::Medium,
            ..AutoMixPrefs::default()
        };

        let result = plan(&a, &b, &prefs, None, PlanContext::default());
        assert_eq!(result.kind, MixKind::BeatMatched);
        let expected_overlap = 8.0 * a.bar_period();
        assert!(
            (result.overlap_duration().as_secs_f32() - expected_overlap).abs() < 0.05,
            "同速 8 小节预期约 {expected_overlap:.2} s，实测 {:?}",
            result.overlap_duration()
        );
        assert!(
            !result.a_low_db.is_empty() && !result.b_low_db.is_empty(),
            "双曲低频强时应生成低频交换曲线"
        );
        assert!(
            (result.b_trim_db - 2.0).abs() < 0.01,
            "响度差 +2 dB 应写入 b_trim_db"
        );
    }

    #[test]
    fn slight_tempo_mismatch_shrinks_overlap_when_tempo_sync_disabled() {
        let a = mock_track(128.0, 200.0, -10.0, 0.5);
        let b = mock_track(130.0, 200.0, -10.0, 0.5);
        let prefs = AutoMixPrefs {
            enabled: true,
            tempo_sync: false,
            length: TransitionLength::Long,
            ..AutoMixPrefs::default()
        };

        let result = plan(&a, &b, &prefs, None, PlanContext::default());
        assert_eq!(result.kind, MixKind::BeatMatched);
        assert_eq!(result.tempo_mode, TempoMode::Off);
        // 128 vs 130 BPM 每拍差约 7.21 ms，关闭变速时 40ms 预算收敛到 2 小节。
        let bars = (result.overlap_duration().as_secs_f32() / a.bar_period()).round() as usize;
        assert_eq!(
            bars, 2,
            "128/130 关闭变速时漂移预算应把长度收敛到 2 小节，实测 {bars}"
        );
    }

    #[test]
    fn tempo_sync_uses_varispeed_within_3_percent_and_keeps_full_overlap() {
        let a = mock_track(128.0, 200.0, -10.0, 0.5);
        let b = mock_track(130.0, 200.0, -10.0, 0.5);
        let prefs = AutoMixPrefs {
            enabled: true,
            tempo_sync: true,
            length: TransitionLength::Long,
            ..AutoMixPrefs::default()
        };

        let result = plan(&a, &b, &prefs, None, PlanContext::default());
        assert_eq!(result.kind, MixKind::BeatMatched);
        assert_eq!(result.tempo_mode, TempoMode::Varispeed);
        let bars = (result.overlap_duration().as_secs_f32() / a.bar_period()).round() as usize;
        assert_eq!(
            bars, 16,
            "开启变速对拍后应保留完整的 16 小节重叠，实测 {bars}"
        );
        assert!((result.b_tempo_ratio - 128.0 / 130.0).abs() < 1e-4);
        assert!(result.b_tempo_release >= Duration::from_millis(1500));
    }

    #[test]
    fn tempo_sync_uses_pitch_preserving_wsola_for_medium_tempo_gap() {
        let a = mock_track(120.0, 200.0, -10.0, 0.5);
        let b = mock_track(128.0, 200.0, -10.0, 0.5);
        let prefs = AutoMixPrefs {
            enabled: true,
            tempo_sync: true,
            length: TransitionLength::Medium,
            ..AutoMixPrefs::default()
        };

        let result = plan(&a, &b, &prefs, None, PlanContext::default());
        assert_eq!(result.kind, MixKind::BeatMatched);
        assert_eq!(result.tempo_mode, TempoMode::PitchPreserving);
        let bars = (result.overlap_duration().as_secs_f32() / a.bar_period()).round() as usize;
        assert_eq!(
            bars, 8,
            "120 vs 128 BPM 应解锁完整 8 小节保音高对拍，实测 {bars}"
        );
        assert!((result.b_tempo_ratio - 120.0 / 128.0).abs() < 1e-4);
        assert!(result.b_tempo_release >= Duration::from_secs(2));
    }

    #[test]
    fn tempo_sync_handles_half_and_double_time_matches() {
        let a = mock_track(70.0, 200.0, -10.0, 0.5);
        let b = mock_track(136.0, 200.0, -10.0, 0.5);
        let prefs = AutoMixPrefs {
            enabled: true,
            tempo_sync: true,
            length: TransitionLength::Short,
            ..AutoMixPrefs::default()
        };

        let result = plan(&a, &b, &prefs, None, PlanContext::default());
        assert_eq!(result.kind, MixKind::BeatMatched);
        // 136 BPM 半速等效 68 BPM，与 70 BPM 相差 2.94%，命中微变速。
        assert_eq!(result.tempo_mode, TempoMode::Varispeed);
        assert!((result.b_tempo_ratio - 70.0 / 68.0).abs() < 1e-4);
    }

    #[test]
    fn large_tempo_gap_falls_back_to_crossfade_or_cut() {
        let a = mock_track(90.0, 200.0, -10.0, 0.5);
        let b = mock_track(128.0, 200.0, -10.0, 0.5);
        let prefs = AutoMixPrefs {
            enabled: true,
            ..AutoMixPrefs::default()
        };

        let result = plan(&a, &b, &prefs, None, PlanContext::default());
        assert!(
            matches!(result.kind, MixKind::Crossfade | MixKind::Cut),
            "大速差不得走对拍，实测 {:?}",
            result.kind
        );
    }

    #[test]
    fn consecutive_album_tracks_without_silence_stay_gapless() {
        let a = mock_track(128.0, 200.0, -10.0, 0.0);
        let b = mock_track(128.0, 200.0, -10.0, 0.0);
        let prefs = AutoMixPrefs {
            enabled: true,
            respect_album_continuity: true,
            ..AutoMixPrefs::default()
        };
        let ctx = PlanContext {
            same_album_consecutive: true,
            ..PlanContext::default()
        };

        let result = plan(&a, &b, &prefs, None, ctx);
        assert_eq!(result.kind, MixKind::Gapless);
    }

    #[test]
    fn trim_off_never_exits_before_outro_and_respects_min_lead() {
        let a = mock_track(128.0, 200.0, -10.0, 0.5);
        let b = mock_track(128.0, 200.0, -10.0, 0.5);
        let prefs = AutoMixPrefs {
            enabled: true,
            trim: TrimLevel::Off,
            ..AutoMixPrefs::default()
        };
        let ctx = PlanContext {
            current_position: Duration::from_secs(175),
            same_album_consecutive: false,
            min_lead: Duration::from_secs(4),
        };

        let result = plan(&a, &b, &prefs, None, ctx);
        assert!(
            result.out_start.as_secs_f32() >= a.outro_start - 0.01,
            "TrimLevel::Off 不得早于 outro_start ({:.2})，实测 {:?}",
            a.outro_start,
            result.out_start
        );
        assert!(
            result.out_start >= Duration::from_secs(179),
            "出点必须满足 MIN_LEAD 约束，实测 {:?}",
            result.out_start
        );
        // 验证出点落在 A 的小节线上。
        let downbeats = a.downbeats();
        let on_bar = downbeats
            .iter()
            .any(|&t| (t - result.out_start.as_secs_f32()).abs() < 0.02);
        assert!(on_bar, "对拍出点应当落在 A 的小节线上");
    }

    #[test]
    fn low_swap_applies_actively_on_crossfade_and_quiet_intro_outro() {
        let mut a = mock_track(92.0, 200.0, -10.0, 0.5);
        let mut b = mock_track(128.0, 200.0, -10.0, 0.5);
        // 压低首尾能量，确保走普通交叉淡化且起止小节低频很弱。
        for bar in &mut a.bar_energy {
            bar[0] = 20;
            bar[1] = 40;
            bar[2] = 40;
        }
        for bar in &mut b.bar_energy {
            bar[0] = 20;
            bar[1] = 40;
            bar[2] = 40;
        }
        let prefs = AutoMixPrefs {
            enabled: true,
            low_swap: true,
            ..AutoMixPrefs::default()
        };

        let result = plan(&a, &b, &prefs, None, PlanContext::default());
        assert_eq!(result.kind, MixKind::Crossfade);
        assert!(
            !result.a_low_db.is_empty() && !result.b_low_db.is_empty(),
            "开启低频交换后，普通交叉淡化也应主动生成低频交换曲线"
        );
        assert!(result.a_low_db.eval(0.0).abs() < 0.01);
        assert!(result.a_low_db.eval(1.0) <= -20.0);
        assert!(result.b_low_db.eval(0.0) <= -20.0);
        assert!(result.b_low_db.eval(1.0).abs() < 0.01);
    }

    #[test]
    fn low_swap_disabled_leaves_low_eq_curves_empty() {
        let a = mock_track(128.0, 200.0, -10.0, 0.5);
        let b = mock_track(128.0, 200.0, -10.0, 0.5);
        let prefs = AutoMixPrefs {
            enabled: true,
            low_swap: false,
            ..AutoMixPrefs::default()
        };

        let result = plan(&a, &b, &prefs, None, PlanContext::default());
        assert_eq!(result.kind, MixKind::BeatMatched);
        assert!(result.a_low_db.is_empty() && result.b_low_db.is_empty());
    }

    #[test]
    fn blend_recipe_populates_mid_and_high_eq_handover_curves() {
        let a = mock_track(128.0, 200.0, -10.0, 0.5);
        let b = mock_track(128.0, 200.0, -10.0, 0.5);
        let prefs = AutoMixPrefs {
            enabled: true,
            ..AutoMixPrefs::default()
        };

        let result = plan(&a, &b, &prefs, None, PlanContext::default());
        assert_eq!(result.style, MixStyle::Blend);
        assert!(!result.a_mid_db.is_empty() && !result.b_mid_db.is_empty());
        assert!(!result.a_high_db.is_empty() && !result.b_high_db.is_empty());
        assert!(result.a_mid_db.eval(1.0) <= -6.0);
        assert!(result.b_mid_db.eval(0.0) <= -4.0);
        assert!(result.a_echo_send.is_empty());
    }

    #[test]
    fn auto_selects_rise_when_incoming_track_builds_into_drop() {
        let a = mock_track(128.0, 200.0, -10.0, 0.5);
        let mut b = mock_track(128.0, 200.0, -10.0, 0.5);
        // B 的前 8 小节能量偏低，第 8 小节后进入高能副歌。
        b.bar_energy[..8].fill([60, 70, 65]);
        let prefs = AutoMixPrefs {
            enabled: true,
            ..AutoMixPrefs::default()
        };

        let result = plan(&a, &b, &prefs, None, PlanContext::default());
        assert_eq!(result.kind, MixKind::BeatMatched);
        assert_eq!(result.style, MixStyle::Rise);
        assert!(
            result.a_filter.eval(0.86) >= 0.5,
            "推升配方应对 A 施加高通扫频"
        );
        assert!(
            result.b_filter.eval(0.0) <= -0.3,
            "推升配方应对 B 施加低通渐开"
        );
    }

    #[test]
    fn auto_selects_wash_with_echo_on_high_energy_tempo_jump() {
        let mut a = mock_track(92.0, 200.0, -10.0, 0.5);
        let b = mock_track(128.0, 200.0, -10.0, 0.5);
        // 保持尾奏高能量，触发硬切并进入回声切配方。
        a.bar_energy.fill([200, 190, 185]);
        let prefs = AutoMixPrefs {
            enabled: true,
            ..AutoMixPrefs::default()
        };

        let result = plan(&a, &b, &prefs, None, PlanContext::default());
        assert_eq!(result.style, MixStyle::Wash);
        assert!(!result.a_echo_send.is_empty(), "回声切必须配置回声发送曲线");
        assert!(result.echo_delay_secs >= 0.18 && result.echo_feedback > 0.4);
        assert!(result.a_filter.eval(0.36) > 0.3);
    }

    #[test]
    fn explicit_style_preference_overrides_auto_selection() {
        let a = mock_track(128.0, 200.0, -10.0, 0.5);
        let b = mock_track(128.0, 200.0, -10.0, 0.5);
        let prefs = AutoMixPrefs {
            enabled: true,
            style: StylePreference::Rise,
            ..AutoMixPrefs::default()
        };

        let result = plan(&a, &b, &prefs, None, PlanContext::default());
        assert_eq!(result.style, MixStyle::Rise);
        assert!(!result.a_filter.is_empty());
    }

    #[test]
    fn joint_scoring_avoids_vocal_clash_and_lands_before_b_vocal_entry() {
        let mut a = mock_track(128.0, 200.0, -10.0, 0.5);
        let mut b = mock_track(128.0, 200.0, -10.0, 0.5);
        let bar_secs_a = a.bar_period();
        let bar_secs_b = b.bar_period();
        let count_a = a.bar_count();
        let count_b = b.bar_count();

        // A 在倒数第 24..12 小节仍在唱副歌，最后 12 小节为纯器乐尾奏。
        let mut vocal_a = vec![220u8; count_a];
        for v in vocal_a.iter_mut().skip(count_a.saturating_sub(12)) {
            *v = 10;
        }
        a.bar_vocal = vocal_a;
        a.vocal_end = a
            .bar_time(count_a.saturating_sub(12))
            .unwrap_or(a.outro_start);

        // B 前 16 小节为器乐前奏，第 16 小节进入主唱。
        let mut vocal_b = vec![230u8; count_b];
        for v in vocal_b.iter_mut().take(16) {
            *v = 10;
        }
        b.bar_vocal = vocal_b;
        b.vocal_start = b.bar_time(16).unwrap_or(16.0 * bar_secs_b);

        let prefs = AutoMixPrefs {
            enabled: true,
            trim: TrimLevel::Medium,
            length: TransitionLength::Medium,
            ..AutoMixPrefs::default()
        };
        let result = plan(&a, &b, &prefs, None, PlanContext::default());
        assert_eq!(result.kind, MixKind::BeatMatched);
        assert!(
            result.out_start.as_secs_f32() >= a.vocal_end - bar_secs_a * 0.6,
            "出点应避开 A 的人声段，vocal_end={:.2}，实测 {:?}",
            a.vocal_end,
            result.out_start
        );
        let b_end = result.in_start.as_secs_f32() + result.overlap_duration().as_secs_f32();
        assert!(
            (b_end - b.vocal_start).abs() <= bar_secs_b * 1.2,
            "B 入点应反推使主唱起点 ({:.2}) 落于过渡结束处 ({b_end:.2})",
            b.vocal_start
        );
    }

    #[test]
    fn heavy_vocal_clash_with_key_mismatch_prefers_wash() {
        let mut a = mock_track(128.0, 200.0, -10.0, 0.5);
        let mut b = mock_track(128.0, 200.0, -10.0, 0.5);
        // 双曲全程高人声，且调性完全不兼容（8B 对 2B）。
        a.bar_vocal = vec![240; a.bar_count()];
        b.bar_vocal = vec![240; b.bar_count()];
        a.vocal_end = a.duration;
        b.vocal_start = 0.0;
        b.key = Some(KeyInfo {
            tonic: 6,
            mode: MusicalMode::Major,
            camelot_num: 2,
            camelot_letter: 'B',
            confidence: 0.9,
        });

        let prefs = AutoMixPrefs {
            enabled: true,
            ..AutoMixPrefs::default()
        };
        let result = plan(&a, &b, &prefs, None, PlanContext::default());
        assert_eq!(
            result.style,
            MixStyle::Wash,
            "重度人声冲突叠加调性反差时应自动选择回声切"
        );
    }
}
