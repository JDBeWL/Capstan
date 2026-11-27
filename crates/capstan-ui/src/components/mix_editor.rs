//! 可视化过渡编辑器。展示上下双轨能量包络与节拍网格，支持拖拽出点与入点吸附网格、
//! 编辑音量与低频自动化曲线控制点、实时试听，以及按曲目对持久化覆盖。

pub mod paint;

use std::sync::Arc;
use std::time::{Duration, Instant};

use capstan_analysis::{
    PlanContext, StylePreference, TrackAnalysis, apply_style_recipe, plan as plan_mix,
};
use capstan_audio::{Automation, CurvePreset, MixKind, MixPlan, MixStyle, TempoMode};
use capstan_core::{Track, TrackId, format_duration};

use crate::components::SettingsEntity;
use crate::entities::{AnalysisEntity, AnalysisEvent, AnalysisPriority, PlaybackEntity};
use crate::icon::{Glyph, icon};
use crate::mix_overrides::MixOverridesEntity;
use crate::platform::*;
use crate::theme;

pub use paint::curve_preview;

/// 编辑器向根视图发出的语义通知。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MixEditorEvent {
    Closed,
    Saved,
    Reset,
}

/// 曲线编辑器当前激活的通道，包含音量、三段均衡、双极点扫频滤镜或回声发送。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CurveChannel {
    Gain,
    LowEq,
    MidEq,
    HighEq,
    Filter,
    Echo,
}

impl CurveChannel {
    const ALL: [Self; 6] = [
        Self::Gain,
        Self::LowEq,
        Self::MidEq,
        Self::HighEq,
        Self::Filter,
        Self::Echo,
    ];

    const fn id(self) -> &'static str {
        match self {
            Self::Gain => "mix-tab-gain",
            Self::LowEq => "mix-tab-low",
            Self::MidEq => "mix-tab-mid",
            Self::HighEq => "mix-tab-high",
            Self::Filter => "mix-tab-filter",
            Self::Echo => "mix-tab-echo",
        }
    }

    const fn label(self) -> &'static str {
        match self {
            Self::Gain => "音量",
            Self::LowEq => "低频 150Hz",
            Self::MidEq => "中频 1kHz",
            Self::HighEq => "高频 6kHz",
            Self::Filter => "扫频滤镜",
            Self::Echo => "回声发送",
        }
    }

    const fn range(self) -> (f32, f32) {
        match self {
            Self::Gain | Self::Echo => (0.0, 1.0),
            Self::LowEq | Self::MidEq | Self::HighEq => (-24.0, 6.0),
            Self::Filter => (-1.0, 1.0),
        }
    }

    const fn is_single_lane(self) -> bool {
        matches!(self, Self::Echo)
    }

    fn is_active_in(self, plan: &MixPlan) -> bool {
        match self {
            Self::Gain => true,
            Self::LowEq => !plan.a_low_db.is_empty() || !plan.b_low_db.is_empty(),
            Self::MidEq => !plan.a_mid_db.is_empty() || !plan.b_mid_db.is_empty(),
            Self::HighEq => !plan.a_high_db.is_empty() || !plan.b_high_db.is_empty(),
            Self::Filter => !plan.a_filter.is_empty() || !plan.b_filter.is_empty(),
            Self::Echo => {
                !plan.a_echo_send.is_empty()
                    && plan.echo_delay_secs >= 0.08
                    && plan.echo_feedback > 0.01
            }
        }
    }
}

/// 哪一条曲目的曲线。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Lane {
    A,
    B,
}

/// 正在进行中的鼠标拖拽目标。
#[derive(Clone, Copy, Debug)]
enum DragTarget {
    OutStart,
    OutEnd,
    TrackBShift {
        anchor_x: f32,
        initial_in_start: f32,
    },
    CurvePoint {
        lane: Lane,
        index: usize,
    },
}

const TIMELINE_H: f32 = 168.0;
const CURVE_H: f32 = 164.0;
const CURVE_PAD_X: f32 = 14.0;
const CURVE_PAD_Y: f32 = 14.0;
/// 曲线控制点命中半径（像素）。
const POINT_HIT_RADIUS: f32 = 10.0;
/// 时间轴前后各额外留出的上下文窗口（秒）。
const VIEW_MARGIN_SECS: f32 = 10.0;

pub struct MixEditor {
    analysis: Entity<AnalysisEntity>,
    settings: Entity<SettingsEntity>,
    overrides: Entity<MixOverridesEntity>,
    playback: Entity<PlaybackEntity>,
    pair: Option<(Track, Track)>,
    analysis_a: Option<Arc<TrackAnalysis>>,
    analysis_b: Option<Arc<TrackAnalysis>>,
    plan: Option<MixPlan>,
    preset: CurvePreset,
    channel: CurveChannel,
    /// 双击新增控制点时作用在哪一条曲目的曲线上。
    active_lane: Lane,
    drag: Option<DragTarget>,
    timeline_bounds: Option<Bounds<Pixels>>,
    curve_bounds: Option<Bounds<Pixels>>,
    /// 记录最近一次在曲线空白处点击的时间与坐标，用于识别双击新增控制点。
    last_curve_click: Option<(Instant, f32, f32)>,
    _subs: Vec<Subscription>,
}

impl EventEmitter<MixEditorEvent> for MixEditor {}

impl MixEditor {
    pub fn new(
        analysis: Entity<AnalysisEntity>,
        settings: Entity<SettingsEntity>,
        overrides: Entity<MixOverridesEntity>,
        playback: Entity<PlaybackEntity>,
        cx: &mut Context<Self>,
    ) -> Self {
        let on_analysis = cx.subscribe(
            &analysis,
            |this, _, event: &AnalysisEvent, cx| match event {
                AnalysisEvent::Ready(id) => this.on_analysis_ready(*id, cx),
                AnalysisEvent::Hydrated(_) => this.on_analysis_hydrated(cx),
                AnalysisEvent::Failed(_) => {}
            },
        );
        // 仅在弹窗打开时响应播放进度推进，驱动时间轴与曲线区的实时试听播放头。
        let on_playback = cx.observe(&playback, |this, _, cx| {
            if this.is_open() {
                cx.notify();
            }
        });
        Self {
            analysis,
            settings,
            overrides,
            playback,
            pair: None,
            analysis_a: None,
            analysis_b: None,
            plan: None,
            preset: CurvePreset::EqualPower,
            channel: CurveChannel::Gain,
            active_lane: Lane::A,
            drag: None,
            timeline_bounds: None,
            curve_bounds: None,
            last_curve_click: None,
            _subs: vec![on_analysis, on_playback],
        }
    }

    pub fn is_open(&self) -> bool {
        self.pair.is_some()
    }

    /// 打开指定两首曲目的过渡编辑器，若尚未分析则立即投递高优先级分析任务。
    pub fn open(&mut self, a: Track, b: Track, cx: &mut Context<Self>) {
        self.analysis.update(cx, |analysis, _| {
            analysis.request(&a, AnalysisPriority::High);
            analysis.request(&b, AnalysisPriority::High);
        });
        let (an_a, an_b) = {
            let analysis = self.analysis.read(cx);
            (analysis.get(a.id), analysis.get(b.id))
        };
        self.pair = Some((a, b));
        self.analysis_a = an_a;
        self.analysis_b = an_b;
        self.drag = None;
        self.rebuild_plan(cx);
        cx.notify();
    }

    pub fn close(&mut self, cx: &mut Context<Self>) {
        if self.pair.take().is_some() {
            self.drag = None;
            cx.emit(MixEditorEvent::Closed);
            cx.notify();
        }
    }

    fn on_analysis_ready(&mut self, id: TrackId, cx: &mut Context<Self>) {
        let Some((a, b)) = self.pair.as_ref() else {
            return;
        };
        if a.id != id && b.id != id {
            return;
        }
        let analysis = self.analysis.read(cx);
        if a.id == id {
            self.analysis_a = analysis.get(a.id);
        }
        if b.id == id {
            self.analysis_b = analysis.get(b.id);
        }
        if self.plan.is_none() {
            self.rebuild_plan(cx);
        }
        cx.notify();
    }

    fn on_analysis_hydrated(&mut self, cx: &mut Context<Self>) {
        let Some((a, b)) = self.pair.as_ref() else {
            return;
        };
        let analysis = self.analysis.read(cx);
        let mut updated = false;
        if self.analysis_a.is_none() {
            self.analysis_a = analysis.get(a.id);
            updated |= self.analysis_a.is_some();
        }
        if self.analysis_b.is_none() {
            self.analysis_b = analysis.get(b.id);
            updated |= self.analysis_b.is_some();
        }
        if updated {
            if self.plan.is_none() {
                self.rebuild_plan(cx);
            }
            cx.notify();
        }
    }

    fn rebuild_plan(&mut self, cx: &App) {
        let Some((a, b)) = self.pair.as_ref() else {
            self.plan = None;
            return;
        };
        let (Some(an_a), Some(an_b)) = (self.analysis_a.as_ref(), self.analysis_b.as_ref()) else {
            self.plan = None;
            return;
        };
        let prefs = self.settings.read(cx).settings().auto_mix;
        let existing = self.overrides.read(cx).get(a.id, b.id).cloned();
        let ctx = PlanContext {
            current_position: Duration::ZERO,
            same_album_consecutive: same_album_consecutive(a, b),
            min_lead: Duration::ZERO,
        };
        let mut built = plan_mix(an_a, an_b, &prefs, existing.as_ref(), ctx);
        // 编辑器打开无缝计划时默认给出一段可调淡化起点，方便用户直接微调。
        if built.kind == MixKind::Gapless && existing.is_none() {
            let dur_a = an_a.duration.max(1.0);
            let overlap = prefs.length.fallback_secs().min(dur_a * 0.4).max(2.0);
            let out_end = (dur_a - an_a.tail_silence).max(overlap);
            let out_start = (out_end - overlap).max(0.0);
            built = MixPlan {
                kind: MixKind::Crossfade,
                style: MixStyle::Fade,
                out_start: Duration::from_secs_f32(out_start),
                out_end: Duration::from_secs_f32(out_end),
                in_start: Duration::from_secs_f32(an_b.lead_silence),
                ..MixPlan::default()
            };
            apply_style_recipe(&mut built, MixStyle::Fade, an_a, an_b, &prefs);
        }
        self.preset =
            if existing.is_some() || !matches!(built.style, MixStyle::Blend | MixStyle::Fade) {
                CurvePreset::Custom
            } else {
                prefs.curve
            };
        self.plan = Some(built);
    }

    /// 估算不变速对拍模式下两首在当前重叠时长上的单侧最大节拍漂移（毫秒）。
    fn drift_ms(&self) -> Option<f32> {
        let (Some(a), Some(b), Some(plan)) = (
            self.analysis_a.as_ref(),
            self.analysis_b.as_ref(),
            self.plan.as_ref(),
        ) else {
            return None;
        };
        if plan.kind != MixKind::BeatMatched
            || plan.tempo_mode != TempoMode::Off
            || a.bpm <= 1.0
            || b.bpm <= 1.0
        {
            return None;
        }
        let best_b = [b.bpm, b.bpm * 2.0, b.bpm * 0.5]
            .into_iter()
            .min_by(|x, y| (x - a.bpm).abs().total_cmp(&(y - a.bpm).abs()))
            .unwrap_or(b.bpm);
        let p_a = 60.0 / a.bpm;
        let p_b = 60.0 / best_b;
        let beats = plan.overlap_duration().as_secs_f32() / p_a;
        Some(0.5 * beats * (p_a - p_b).abs() * 1000.0)
    }

    /// 计算时间轴显示范围 `(A 的起始秒数, 可视总跨度秒数)`。
    fn timeline_window(&self) -> (f32, f32) {
        let Some(plan) = self.plan.as_ref() else {
            return (0.0, 30.0);
        };
        let out_start = plan.out_start.as_secs_f32();
        let out_end = plan.out_end.as_secs_f32().max(out_start + 0.5);
        let start = (out_start - VIEW_MARGIN_SECS).max(0.0);
        let end = out_end + VIEW_MARGIN_SECS;
        (start, (end - start).max(12.0))
    }

    fn apply_auto_recipe(&mut self, cx: &mut Context<Self>) {
        let (Some((a, b)), Some(an_a), Some(an_b)) = (
            self.pair.as_ref(),
            self.analysis_a.as_ref(),
            self.analysis_b.as_ref(),
        ) else {
            return;
        };
        let mut prefs = self.settings.read(cx).settings().auto_mix;
        prefs.style = StylePreference::Auto;
        let ctx = PlanContext {
            current_position: Duration::ZERO,
            same_album_consecutive: same_album_consecutive(a, b),
            min_lead: Duration::ZERO,
        };
        let mut built = plan_mix(an_a, an_b, &prefs, None, ctx);
        if built.kind == MixKind::Gapless {
            let dur_a = an_a.duration.max(1.0);
            let overlap = prefs.length.fallback_secs().min(dur_a * 0.4).max(2.0);
            let out_end = (dur_a - an_a.tail_silence).max(overlap);
            let out_start = (out_end - overlap).max(0.0);
            built = MixPlan {
                kind: MixKind::Crossfade,
                style: MixStyle::Fade,
                out_start: Duration::from_secs_f32(out_start),
                out_end: Duration::from_secs_f32(out_end),
                in_start: Duration::from_secs_f32(an_b.lead_silence),
                ..MixPlan::default()
            };
            apply_style_recipe(&mut built, MixStyle::Fade, an_a, an_b, &prefs);
        }
        self.preset = if matches!(built.style, MixStyle::Blend | MixStyle::Fade) {
            prefs.curve
        } else {
            CurvePreset::Custom
        };
        self.plan = Some(built);
        cx.notify();
    }

    fn apply_style(&mut self, style: MixStyle, cx: &mut Context<Self>) {
        let (Some(an_a), Some(an_b)) = (self.analysis_a.as_ref(), self.analysis_b.as_ref()) else {
            return;
        };
        let prefs = self.settings.read(cx).settings().auto_mix;
        if let Some(plan) = self.plan.as_mut() {
            apply_style_recipe(plan, style, an_a, an_b, &prefs);
            self.preset = if matches!(style, MixStyle::Blend | MixStyle::Fade) {
                prefs.curve
            } else {
                CurvePreset::Custom
            };
            cx.notify();
        }
    }

    fn apply_preset(&mut self, preset: CurvePreset, cx: &mut Context<Self>) {
        if let Some(plan) = self.plan.as_mut() {
            plan.a_gain = Automation::preset_out(preset);
            plan.b_gain = Automation::preset_in(preset);
            plan.style = MixStyle::Custom;
            self.preset = preset;
            cx.notify();
        }
    }

    fn apply_low_swap(&mut self, enable: bool, cx: &mut Context<Self>) {
        if let Some(plan) = self.plan.as_mut() {
            if enable {
                plan.a_low_db = Automation::low_swap_out(0.5, 0.06, -24.0);
                plan.b_low_db = Automation::low_swap_in(0.5, 0.06, -24.0);
            } else {
                plan.a_low_db = Automation::empty();
                plan.b_low_db = Automation::empty();
            }
            plan.style = MixStyle::Custom;
            cx.notify();
        }
    }

    fn apply_mid_handover(&mut self, enable: bool, cx: &mut Context<Self>) {
        if let Some(plan) = self.plan.as_mut() {
            if enable {
                plan.a_mid_db = Automation::ramp(0.35, 0.0, 0.78, -8.0);
                plan.b_mid_db = Automation::ramp(0.18, -6.0, 0.58, 0.0);
            } else {
                plan.a_mid_db = Automation::empty();
                plan.b_mid_db = Automation::empty();
            }
            plan.style = MixStyle::Custom;
            cx.notify();
        }
    }

    fn apply_high_handover(&mut self, enable: bool, cx: &mut Context<Self>) {
        if let Some(plan) = self.plan.as_mut() {
            if enable {
                plan.a_high_db = Automation::ramp(0.55, 0.0, 0.92, -6.0);
                plan.b_high_db = Automation::ramp(0.12, -5.0, 0.48, 0.0);
            } else {
                plan.a_high_db = Automation::empty();
                plan.b_high_db = Automation::empty();
            }
            plan.style = MixStyle::Custom;
            cx.notify();
        }
    }

    fn apply_filter_preset(&mut self, mode: i8, cx: &mut Context<Self>) {
        if let Some(plan) = self.plan.as_mut() {
            match mode {
                1 => {
                    plan.a_filter = Automation::ramp(0.15, 0.0, 0.86, 0.62);
                    plan.b_filter = Automation::ramp(0.0, -0.38, 0.78, 0.0);
                }
                -1 => {
                    plan.a_filter = Automation::ramp(0.35, 0.0, 0.95, -0.34);
                    plan.b_filter = Automation::empty();
                }
                _ => {
                    plan.a_filter = Automation::empty();
                    plan.b_filter = Automation::empty();
                }
            }
            plan.style = MixStyle::Custom;
            cx.notify();
        }
    }

    fn apply_echo_beats(&mut self, beat_mult: Option<f32>, cx: &mut Context<Self>) {
        let beat_a = self.analysis_a.as_ref().map_or(0.5, |an| an.beat_period());
        if let Some(plan) = self.plan.as_mut() {
            if let Some(mult) = beat_mult {
                plan.echo_delay_secs = (beat_a * mult).clamp(0.12, 1.0);
                if plan.echo_feedback <= 0.05 {
                    plan.echo_feedback = 0.56;
                }
                if plan.a_echo_send.is_empty() {
                    plan.a_echo_send = Automation::pulse(0.02, 0.12, 0.28, 0.38, 0.72);
                }
            } else {
                plan.a_echo_send = Automation::empty();
                plan.echo_delay_secs = 0.0;
                plan.echo_feedback = 0.0;
            }
            plan.style = MixStyle::Custom;
            cx.notify();
        }
    }

    fn preview_play(&self, cx: &mut Context<Self>) {
        let (Some((a, b)), Some(plan)) = (self.pair.as_ref(), self.plan.as_ref()) else {
            return;
        };
        let start = plan.out_start.saturating_sub(Duration::from_secs(5));
        let track_a = a.clone();
        let to_path = b.path.clone();
        let plan = plan.clone();
        self.playback.update(cx, |playback, cx| {
            playback.preview_mix(track_a, start, to_path, plan, cx);
        });
    }

    /// 微调 B 轨入点（正值向后移入更晚内容，负值提前），按变速倍率约束在合法媒体范围内。
    fn nudge_in_start(&mut self, delta_secs: f32, cx: &mut Context<Self>) {
        let (Some(an_b), Some(plan)) = (self.analysis_b.as_ref(), self.plan.as_mut()) else {
            return;
        };
        let b_ratio = plan.effective_tempo_ratio();
        let max_in = (an_b.duration - plan.overlap_duration().as_secs_f32() * b_ratio).max(0.0);
        let next = (plan.in_start.as_secs_f32() + delta_secs).clamp(0.0, max_in);
        plan.in_start = Duration::from_secs_f32(next);
        cx.notify();
    }

    /// 将当前播放位置折算为时间轴坐标（A 轨秒数），仅当播放曲目为 A 或 B 时返回 `Some`。
    fn playhead_time(&self, cx: &App) -> Option<f32> {
        let (Some((a, b)), Some(plan)) = (self.pair.as_ref(), self.plan.as_ref()) else {
            return None;
        };
        let playback = self.playback.read(cx);
        let current_id = playback.current()?.id;
        let pos = playback.position().as_secs_f32();
        if !playback.is_playing() && pos <= 0.0 {
            return None;
        }
        if current_id == a.id {
            Some(pos)
        } else if current_id == b.id {
            let out_start = plan.out_start.as_secs_f32();
            let out_end = plan.out_end.as_secs_f32();
            let in_start = plan.in_start.as_secs_f32();
            let b_ratio = plan.effective_tempo_ratio().max(0.1);
            let rel_b = pos - in_start;
            let b_overlap_media = (out_end - out_start).max(0.0) * b_ratio;
            if rel_b <= b_overlap_media {
                Some(out_start + rel_b / b_ratio)
            } else {
                Some(out_end + (rel_b - b_overlap_media))
            }
        } else {
            None
        }
    }

    fn save_override(&mut self, cx: &mut Context<Self>) {
        let (Some((a, b)), Some(plan)) = (self.pair.as_ref(), self.plan.as_ref()) else {
            return;
        };
        let (a_id, b_id, plan) = (a.id, b.id, plan.clone());
        self.overrides.update(cx, |store, cx| {
            store.set(a_id, b_id, plan, cx);
        });
        cx.emit(MixEditorEvent::Saved);
        cx.notify();
    }

    fn reset_override(&mut self, cx: &mut Context<Self>) {
        let Some((a, b)) = self.pair.as_ref() else {
            return;
        };
        let (a_id, b_id) = (a.id, b.id);
        self.overrides.update(cx, |store, cx| {
            store.remove(a_id, b_id, cx);
        });
        self.rebuild_plan(cx);
        cx.emit(MixEditorEvent::Reset);
        cx.notify();
    }

    fn handle_timeline_down(&mut self, event: &MouseDownEvent, cx: &mut Context<Self>) {
        let (Some(bounds), Some(plan)) = (self.timeline_bounds, self.plan.as_ref()) else {
            return;
        };
        let x0 = f32::from(bounds.origin.x);
        let y0 = f32::from(bounds.origin.y);
        let w = f32::from(bounds.size.width).max(1.0);
        let h = f32::from(bounds.size.height).max(1.0);
        let mx = f32::from(event.position.x);
        let my = f32::from(event.position.y);

        let (view_start, span) = self.timeline_window();
        let out_start_x = x0 + (plan.out_start.as_secs_f32() - view_start) / span * w;
        let out_end_x = x0 + (plan.out_end.as_secs_f32() - view_start) / span * w;

        if (mx - out_start_x).abs() <= 12.0 {
            self.drag = Some(DragTarget::OutStart);
        } else if (mx - out_end_x).abs() <= 12.0 {
            self.drag = Some(DragTarget::OutEnd);
        } else if my >= y0 + h * 0.5 {
            self.drag = Some(DragTarget::TrackBShift {
                anchor_x: mx,
                initial_in_start: plan.in_start.as_secs_f32(),
            });
        }
        cx.notify();
    }

    fn handle_timeline_move(&mut self, event: &MouseMoveEvent, cx: &mut Context<Self>) {
        let Some(drag) = self.drag else {
            return;
        };
        let (Some(bounds), Some(an_a), Some(an_b)) = (
            self.timeline_bounds,
            self.analysis_a.as_ref(),
            self.analysis_b.as_ref(),
        ) else {
            return;
        };
        let (view_start, span) = self.timeline_window();
        let x0 = f32::from(bounds.origin.x);
        let w = f32::from(bounds.size.width).max(1.0);
        let mx = f32::from(event.position.x);
        let snap = !event.modifiers.shift;

        let Some(plan) = self.plan.as_mut() else {
            return;
        };

        match drag {
            DragTarget::OutStart => {
                let raw = (view_start + (mx - x0) / w * span).clamp(0.0, an_a.duration);
                let snapped = if snap {
                    an_a.snap_to_grid(raw, false)
                } else {
                    raw
                };
                let max_start = (plan.out_end.as_secs_f32() - 0.5).max(0.0);
                plan.out_start = Duration::from_secs_f32(snapped.min(max_start));
            }
            DragTarget::OutEnd => {
                let raw = (view_start + (mx - x0) / w * span).clamp(0.0, an_a.duration);
                let snapped = if snap {
                    an_a.snap_to_grid(raw, false)
                } else {
                    raw
                };
                let min_end = (plan.out_start.as_secs_f32() + 0.5).min(an_a.duration);
                plan.out_end = Duration::from_secs_f32(snapped.max(min_end));
            }
            DragTarget::TrackBShift {
                anchor_x,
                initial_in_start,
            } => {
                // 向左拖 B 轨 = 把 B 更靠后的内容拉到出点处，即增大 `in_start`。
                let b_ratio = plan.effective_tempo_ratio();
                let dt = -(mx - anchor_x) / w * span * b_ratio;
                let max_in =
                    (an_b.duration - plan.overlap_duration().as_secs_f32() * b_ratio).max(0.0);
                let raw = (initial_in_start + dt).clamp(0.0, max_in);
                let snapped = if snap {
                    an_b.snap_to_grid(raw, false).min(max_in)
                } else {
                    raw
                };
                plan.in_start = Duration::from_secs_f32(snapped);
            }
            DragTarget::CurvePoint { .. } => return,
        }
        cx.notify();
    }

    fn curve_range(&self) -> (f32, f32) {
        self.channel.range()
    }

    fn curve_mut(&mut self, lane: Lane) -> Option<&mut Automation> {
        let plan = self.plan.as_mut()?;
        Some(match (self.channel, lane) {
            (CurveChannel::Gain, Lane::A) => &mut plan.a_gain,
            (CurveChannel::Gain, Lane::B) => &mut plan.b_gain,
            (CurveChannel::LowEq, Lane::A) => &mut plan.a_low_db,
            (CurveChannel::LowEq, Lane::B) => &mut plan.b_low_db,
            (CurveChannel::MidEq, Lane::A) => &mut plan.a_mid_db,
            (CurveChannel::MidEq, Lane::B) => &mut plan.b_mid_db,
            (CurveChannel::HighEq, Lane::A) => &mut plan.a_high_db,
            (CurveChannel::HighEq, Lane::B) => &mut plan.b_high_db,
            (CurveChannel::Filter, Lane::A) => &mut plan.a_filter,
            (CurveChannel::Filter, Lane::B) => &mut plan.b_filter,
            (CurveChannel::Echo, Lane::A) => &mut plan.a_echo_send,
            (CurveChannel::Echo, Lane::B) => return None,
        })
    }

    fn curve_ref(&self, lane: Lane) -> Option<&Automation> {
        let plan = self.plan.as_ref()?;
        Some(match (self.channel, lane) {
            (CurveChannel::Gain, Lane::A) => &plan.a_gain,
            (CurveChannel::Gain, Lane::B) => &plan.b_gain,
            (CurveChannel::LowEq, Lane::A) => &plan.a_low_db,
            (CurveChannel::LowEq, Lane::B) => &plan.b_low_db,
            (CurveChannel::MidEq, Lane::A) => &plan.a_mid_db,
            (CurveChannel::MidEq, Lane::B) => &plan.b_mid_db,
            (CurveChannel::HighEq, Lane::A) => &plan.a_high_db,
            (CurveChannel::HighEq, Lane::B) => &plan.b_high_db,
            (CurveChannel::Filter, Lane::A) => &plan.a_filter,
            (CurveChannel::Filter, Lane::B) => &plan.b_filter,
            (CurveChannel::Echo, Lane::A) => &plan.a_echo_send,
            (CurveChannel::Echo, Lane::B) => return None,
        })
    }

    fn curve_coords(&self, bounds: Bounds<Pixels>, mx: f32, my: f32) -> (f32, f32) {
        let left = f32::from(bounds.origin.x) + CURVE_PAD_X;
        let top = f32::from(bounds.origin.y) + CURVE_PAD_Y;
        let width = (f32::from(bounds.size.width) - 2.0 * CURVE_PAD_X).max(1.0);
        let height = (f32::from(bounds.size.height) - 2.0 * CURVE_PAD_Y).max(1.0);
        let (min_v, max_v) = self.curve_range();
        let frac = ((mx - left) / width).clamp(0.0, 1.0);
        let norm_y = (1.0 - (my - top) / height).clamp(0.0, 1.0);
        let value = min_v + norm_y * (max_v - min_v);
        (frac, value)
    }

    fn hit_curve_point(&self, bounds: Bounds<Pixels>, mx: f32, my: f32) -> Option<(Lane, usize)> {
        let x = f32::from(bounds.origin.x) + CURVE_PAD_X;
        let y = f32::from(bounds.origin.y) + CURVE_PAD_Y;
        let w = (f32::from(bounds.size.width) - 2.0 * CURVE_PAD_X).max(1.0);
        let h = (f32::from(bounds.size.height) - 2.0 * CURVE_PAD_Y).max(1.0);
        let (min_v, max_v) = self.curve_range();
        let range = (max_v - min_v).max(1e-5);

        let lanes: &[Lane] = if self.channel.is_single_lane() {
            &[Lane::A]
        } else if self.active_lane == Lane::A {
            &[Lane::A, Lane::B]
        } else {
            &[Lane::B, Lane::A]
        };

        let mut best: Option<(Lane, usize, f32)> = None;
        for &lane in lanes {
            let Some(curve) = self.curve_ref(lane) else {
                continue;
            };
            for (idx, pt) in curve.points().iter().enumerate() {
                let px_x = x + pt.t * w;
                let px_y = y + (1.0 - (pt.value - min_v) / range) * h;
                let dist = (mx - px_x).hypot(my - px_y);
                if dist <= POINT_HIT_RADIUS && best.is_none_or(|(_, _, prev)| dist < prev) {
                    best = Some((lane, idx, dist));
                }
            }
        }
        best.map(|(lane, idx, _)| (lane, idx))
    }

    fn handle_curve_left_down(&mut self, event: &MouseDownEvent, cx: &mut Context<Self>) {
        let Some(bounds) = self.curve_bounds else {
            return;
        };
        let mx = f32::from(event.position.x);
        let my = f32::from(event.position.y);

        if let Some((lane, index)) = self.hit_curve_point(bounds, mx, my) {
            self.active_lane = lane;
            self.drag = Some(DragTarget::CurvePoint { lane, index });
            self.last_curve_click = None;
            cx.notify();
            return;
        }

        // 双击空白处时，在当前激活轨道曲线上新增一个控制点并立即进入拖拽。
        let now = Instant::now();
        let is_double = self.last_curve_click.is_some_and(|(prev, lx, ly)| {
            now.duration_since(prev) <= Duration::from_millis(380)
                && (mx - lx).hypot(my - ly) <= 12.0
        });
        if is_double {
            self.last_curve_click = None;
            let (t, value) = self.curve_coords(bounds, mx, my);
            let (min_v, max_v) = self.curve_range();
            let lane = if self.channel.is_single_lane() {
                Lane::A
            } else {
                self.active_lane
            };
            let channel = self.channel;
            let beat_a = self.analysis_a.as_ref().map_or(0.5, |an| an.beat_period());
            if channel == CurveChannel::Echo
                && let Some(plan) = self.plan.as_mut()
            {
                if plan.echo_delay_secs < 0.08 {
                    plan.echo_delay_secs = (beat_a * 0.75).clamp(0.18, 0.75);
                }
                if plan.echo_feedback <= 0.05 {
                    plan.echo_feedback = 0.56;
                }
            }
            if let Some(curve) = self.curve_mut(lane) {
                if curve.is_empty() && channel != CurveChannel::Gain {
                    *curve = Automation::flat(0.0);
                }
                let index = curve.insert_point(t, value, min_v, max_v);
                if let Some(plan) = self.plan.as_mut() {
                    plan.style = MixStyle::Custom;
                }
                if channel == CurveChannel::Gain {
                    self.preset = CurvePreset::Custom;
                }
                self.drag = Some(DragTarget::CurvePoint { lane, index });
                cx.notify();
            }
        } else {
            self.last_curve_click = Some((now, mx, my));
        }
    }

    fn handle_curve_right_down(&mut self, event: &MouseDownEvent, cx: &mut Context<Self>) {
        let Some(bounds) = self.curve_bounds else {
            return;
        };
        let mx = f32::from(event.position.x);
        let my = f32::from(event.position.y);
        let channel = self.channel;
        if let Some((lane, index)) = self.hit_curve_point(bounds, mx, my)
            && let Some(curve) = self.curve_mut(lane)
            && curve.remove_point(index)
        {
            if let Some(plan) = self.plan.as_mut() {
                plan.style = MixStyle::Custom;
            }
            if channel == CurveChannel::Gain {
                self.preset = CurvePreset::Custom;
            }
            cx.notify();
        }
    }

    fn handle_curve_move(&mut self, event: &MouseMoveEvent, cx: &mut Context<Self>) {
        let Some(DragTarget::CurvePoint { lane, index }) = self.drag else {
            return;
        };
        let Some(bounds) = self.curve_bounds else {
            return;
        };
        let mx = f32::from(event.position.x);
        let my = f32::from(event.position.y);
        let (t, value) = self.curve_coords(bounds, mx, my);
        let (min_v, max_v) = self.curve_range();
        let channel = self.channel;
        if let Some(curve) = self.curve_mut(lane) {
            curve.move_point(index, t, value, min_v, max_v);
            if let Some(plan) = self.plan.as_mut() {
                plan.style = MixStyle::Custom;
            }
            if channel == CurveChannel::Gain {
                self.preset = CurvePreset::Custom;
            }
            cx.notify();
        }
    }

    fn finish_drag(&mut self, cx: &mut Context<Self>) {
        if self.drag.take().is_some() {
            cx.notify();
        }
    }
}

/// 判定两首曲目是否同属一张专辑的连续音轨。
pub fn same_album_consecutive(a: &Track, b: &Track) -> bool {
    let (Some(alb_a), Some(alb_b)) = (a.album.as_deref(), b.album.as_deref()) else {
        return false;
    };
    if alb_a.trim().is_empty() || !alb_a.eq_ignore_ascii_case(alb_b) {
        return false;
    }
    match (a.track_no, b.track_no) {
        (Some(na), Some(nb)) => nb == na + 1,
        _ => false,
    }
}

impl Render for MixEditor {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some((track_a, track_b)) = self.pair.clone() else {
            return div().into_any_element();
        };

        let has_override = self.overrides.read(cx).contains(track_a.id, track_b.id);
        let drift_ms = self.drift_ms();
        let drift_warning = drift_ms.is_some_and(|ms| ms > 40.0);

        let header = div()
            .w_full()
            .flex()
            .items_center()
            .justify_between()
            .gap(px(theme::GAP_MD))
            .pb(px(theme::GAP_MD))
            .border_b_1()
            .border_color(theme::border())
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .child(crate::common::section::eyebrow("Mix Editor / 过渡编辑"))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(theme::GAP_SM))
                            .text_size(px(theme::FONT_SUBTITLE))
                            .font_weight(theme::WEIGHT_TITLE)
                            .text_color(theme::text())
                            .child(format!("{}  ⇄  {}", track_a.title, track_b.title)),
                    ),
            )
            .child(
                div()
                    .id("mix-editor-close")
                    .size(px(30.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .bg(theme::elevated())
                    .cursor_pointer()
                    .hover(|s| s.bg(theme::border()))
                    .on_click(cx.listener(|this, _, _, cx| this.close(cx)))
                    .child(icon(Glyph::Cross, 15., theme::text())),
            );

        let body: AnyElement = if let (Some(an_a), Some(an_b), Some(plan)) = (
            self.analysis_a.as_ref().map(Arc::clone),
            self.analysis_b.as_ref().map(Arc::clone),
            self.plan.clone(),
        ) {
            let playhead_t = self.playhead_time(cx);
            let playhead_frac = playhead_t.and_then(|t| {
                let os = plan.out_start.as_secs_f32();
                let ov = plan.overlap_duration().as_secs_f32();
                (ov > 0.05 && t >= os && t <= os + ov).then(|| (t - os) / ov)
            });
            let meta_bar = render_meta_bar(&an_a, &an_b, &plan, drift_ms, has_override);
            let timeline = self.render_timeline(an_a, an_b, &plan, drift_warning, playhead_t, cx);
            let curve_area = self.render_curve_panel(&plan, playhead_frac, cx);
            let footer = Self::render_footer(has_override, cx);

            div()
                .w_full()
                .flex()
                .flex_col()
                .gap(px(theme::GAP_MD))
                .child(meta_bar)
                .child(timeline)
                .child(curve_area)
                .child(footer)
                .into_any_element()
        } else {
            div()
                .w_full()
                .h(px(320.))
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap(px(theme::GAP_SM))
                .child(
                    div()
                        .text_size(px(theme::FONT_BODY))
                        .text_color(theme::text())
                        .child("正在分析两首曲目的 BPM、节拍网格与能量结构…"),
                )
                .child(
                    div()
                        .text_size(px(theme::FONT_CAPTION))
                        .text_color(theme::text_muted())
                        .child("分析完成后会自动填入推荐的混音切点与曲线"),
                )
                .into_any_element()
        };

        // 模态遮罩，点击外围可关闭。
        div()
            .id("mix-editor-backdrop")
            .absolute()
            .inset_0()
            .bg(theme::scrim())
            .flex()
            .items_center()
            .justify_center()
            .p(px(theme::GAP_XL))
            .occlude()
            .on_click(cx.listener(|this, _, _, cx| this.close(cx)))
            .child(
                div()
                    .id("mix-editor-dialog")
                    .w(px(840.))
                    .max_w_full()
                    .bg(theme::panel())
                    .border_1()
                    .border_color(theme::border())
                    .p(px(theme::GAP_LG))
                    .flex()
                    .flex_col()
                    .gap(px(theme::GAP_MD))
                    // 阻断内部点击冒泡到遮罩，避免编辑时误关闭弹窗。
                    .on_click(|_, _, cx| cx.stop_propagation())
                    .child(header)
                    .child(body),
            )
            .into_any_element()
    }
}

impl MixEditor {
    fn render_timeline(
        &self,
        an_a: Arc<TrackAnalysis>,
        an_b: Arc<TrackAnalysis>,
        plan: &MixPlan,
        drift_warning: bool,
        playhead_t: Option<f32>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (view_start, span) = self.timeline_window();
        let out_start = plan.out_start.as_secs_f32();
        let out_end = plan.out_end.as_secs_f32();
        let in_start = plan.in_start.as_secs_f32();
        let b_ratio = plan.effective_tempo_ratio();
        let b_span = span * b_ratio;
        let beat_b = an_b.beat_period();
        // B 轨按变速倍率折算媒体跨度，使 B 的 `in_start` 对齐 A 的 `out_start` 且节拍网格一一重合。
        let b_origin = in_start - (out_start - view_start) * b_ratio;

        let weak = cx.entity().downgrade();
        let capture = move |bounds: Bounds<Pixels>, _window: &mut Window, cx: &mut App| {
            weak.update(cx, |this, _| this.timeline_bounds = Some(bounds))
                .ok();
        };

        let nudge_strip = div()
            .w_full()
            .flex()
            .items_center()
            .justify_between()
            .gap(px(theme::GAP_SM))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(theme::GAP_XS))
                    .child(
                        div()
                            .pr(px(theme::GAP_XS))
                            .text_size(px(theme::FONT_CAPTION))
                            .text_color(theme::text_muted())
                            .child("微调 B 入点"),
                    )
                    .child(
                        tab_chip("mix-nudge-b-m1", "−1 拍", false).on_click(cx.listener(
                            move |this, _, _, cx| {
                                this.nudge_in_start(-beat_b, cx);
                            },
                        )),
                    )
                    .child(
                        tab_chip("mix-nudge-b-m20", "−20 ms", false).on_click(cx.listener(
                            |this, _, _, cx| {
                                this.nudge_in_start(-0.02, cx);
                            },
                        )),
                    )
                    .child(
                        tab_chip("mix-nudge-b-p20", "+20 ms", false).on_click(cx.listener(
                            |this, _, _, cx| {
                                this.nudge_in_start(0.02, cx);
                            },
                        )),
                    )
                    .child(
                        tab_chip("mix-nudge-b-p1", "+1 拍", false).on_click(cx.listener(
                            move |this, _, _, cx| {
                                this.nudge_in_start(beat_b, cx);
                            },
                        )),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(theme::GAP_SM))
                    .child(legend_dot(paint::BAND_LOW_HEX, "低频"))
                    .child(legend_dot(paint::BAND_MID_HEX, "中频"))
                    .child(legend_dot(paint::BAND_HIGH_HEX, "高频"))
                    .child(legend_dot(paint::VOCAL_MARK_HEX, "人声段")),
            );

        div()
            .w_full()
            .flex()
            .flex_col()
            .gap(px(theme::GAP_XS))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .text_size(px(theme::FONT_CAPTION))
                    .text_color(theme::text_muted())
                    .child("双轨时间轴（拖出点 / 终点线吸附小节，拖下轨平移入点；按住 Shift 自由微调）")
                    .child(format!(
                        "A 出点 {} → {} │ B 入点 {}",
                        format_duration(plan.out_start),
                        format_duration(plan.out_end),
                        format_duration(plan.in_start),
                    )),
            )
            .child(
                div()
                    .id("mix-timeline")
                    .w_full()
                    .h(px(TIMELINE_H))
                    .bg(theme::window_base())
                    .border_1()
                    .border_color(theme::border())
                    .cursor_pointer()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, event: &MouseDownEvent, _, cx| {
                            this.handle_timeline_down(event, cx);
                        }),
                    )
                    .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _, cx| {
                        this.handle_timeline_move(event, cx);
                    }))
                    .on_mouse_up(
                        MouseButton::Left,
                        cx.listener(|this, _, _, cx| this.finish_drag(cx)),
                    )
                    .on_mouse_up_out(
                        MouseButton::Left,
                        cx.listener(|this, _, _, cx| this.finish_drag(cx)),
                    )
                    .child(
                        canvas(capture, move |bounds, (), window, _cx| {
                            let x = f32::from(bounds.origin.x);
                            let y = f32::from(bounds.origin.y);
                            let w = f32::from(bounds.size.width).max(1.0);
                            let h = f32::from(bounds.size.height).max(1.0);
                            let half_h = h * 0.5;
                            let to_x = |t: f32| x + (t - view_start) / span * w;

                            // 轨道 A（上半）与轨道 B（下半）的彩色波形、人声标记与节拍网格。
                            paint::paint_track_lane(
                                window,
                                x,
                                y + 4.0,
                                w,
                                half_h - 8.0,
                                &an_a,
                                view_start,
                                span,
                                theme::text(),
                            );
                            paint::paint_track_lane(
                                window,
                                x,
                                y + half_h + 4.0,
                                w,
                                half_h - 8.0,
                                &an_b,
                                b_origin,
                                b_span,
                                paint::track_b_color(),
                            );

                            // 中间分隔线。
                            paint::paint_rect(window, x, y + half_h, w, 1.0, theme::border());

                            // 重叠区间高亮；节拍漂移超限时染警示色。
                            let ox0 = to_x(out_start).clamp(x, x + w);
                            let ox1 = to_x(out_end).clamp(x, x + w);
                            let zone_color = if drift_warning {
                                theme::danger().opacity(0.24)
                            } else {
                                theme::accent().opacity(0.18)
                            };
                            paint::paint_rect(window, ox0, y, (ox1 - ox0).max(0.0), h, zone_color);

                            // A 在 `out_end` 之后被裁剪掉的片段压暗。
                            if ox1 < x + w {
                                paint::paint_rect(
                                    window,
                                    ox1,
                                    y,
                                    x + w - ox1,
                                    half_h,
                                    theme::scrim().opacity(0.35),
                                );
                            }

                            // B 在 `out_start` 之前的未播放前导压暗。
                            if ox0 > x {
                                paint::paint_rect(
                                    window,
                                    x,
                                    y + half_h,
                                    ox0 - x,
                                    half_h,
                                    theme::scrim().opacity(0.35),
                                );
                            }

                            // 出点与终点拖拽手柄竖线。
                            paint::paint_rect(window, ox0 - 1.5, y, 3.0, h, theme::accent());
                            paint::paint_rect(
                                window,
                                ox1 - 1.5,
                                y,
                                3.0,
                                h,
                                paint::track_b_color(),
                            );

                            // 试听实时播放头。
                            if let Some(pt) = playhead_t
                                && pt >= view_start
                                && pt <= view_start + span
                            {
                                paint::paint_playhead(window, to_x(pt), y, h);
                            }
                        })
                        .size_full(),
                    ),
            )
            .child(nudge_strip)
            .into_any_element()
    }

    fn render_curve_panel(
        &self,
        plan: &MixPlan,
        playhead_frac: Option<f32>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let channel = self.channel;
        let active_lane = if channel.is_single_lane() {
            Lane::A
        } else {
            self.active_lane
        };
        let (min_v, max_v) = self.curve_range();
        let fallback_zero = |curve: &Automation| {
            if curve.is_empty() {
                Automation::flat(0.0)
            } else {
                curve.clone()
            }
        };
        let (curve_a, curve_b): (Automation, Option<Automation>) = match channel {
            CurveChannel::Gain => (plan.a_gain.clone(), Some(plan.b_gain.clone())),
            CurveChannel::LowEq => (
                fallback_zero(&plan.a_low_db),
                Some(fallback_zero(&plan.b_low_db)),
            ),
            CurveChannel::MidEq => (
                fallback_zero(&plan.a_mid_db),
                Some(fallback_zero(&plan.b_mid_db)),
            ),
            CurveChannel::HighEq => (
                fallback_zero(&plan.a_high_db),
                Some(fallback_zero(&plan.b_high_db)),
            ),
            CurveChannel::Filter => (
                fallback_zero(&plan.a_filter),
                Some(fallback_zero(&plan.b_filter)),
            ),
            CurveChannel::Echo => (fallback_zero(&plan.a_echo_send), None),
        };

        let weak = cx.entity().downgrade();
        let capture = move |bounds: Bounds<Pixels>, _window: &mut Window, cx: &mut App| {
            weak.update(cx, |this, _| this.curve_bounds = Some(bounds))
                .ok();
        };

        // 第一行用于过渡风格配方一键切换与当前编辑轨道选择。
        let mut recipe_row = div()
            .flex()
            .items_center()
            .gap(px(theme::GAP_XS))
            .child(
                div()
                    .pr(px(theme::GAP_XS))
                    .text_size(px(theme::FONT_CAPTION))
                    .text_color(theme::text_muted())
                    .child("配方"),
            )
            .child(
                tab_chip("mix-style-auto", "自动", false).on_click(cx.listener(
                    |this, _, _, cx| {
                        this.apply_auto_recipe(cx);
                    },
                )),
            );
        for style in MixStyle::RECIPES {
            recipe_row = recipe_row.child(
                tab_chip(
                    SharedString::from(format!("mix-style-{style:?}")),
                    style.label(),
                    plan.style == style,
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.apply_style(style, cx);
                })),
            );
        }

        let lane_row = if channel.is_single_lane() {
            div()
                .text_size(px(theme::FONT_CAPTION))
                .text_color(theme::text_muted())
                .child("仅作用于前曲 A")
        } else {
            div()
                .flex()
                .items_center()
                .gap(px(theme::GAP_XS))
                .child(
                    tab_chip("mix-lane-a", "编辑 A", active_lane == Lane::A).on_click(cx.listener(
                        |this, _, _, cx| {
                            this.active_lane = Lane::A;
                            cx.notify();
                        },
                    )),
                )
                .child(
                    tab_chip("mix-lane-b", "编辑 B", active_lane == Lane::B).on_click(cx.listener(
                        |this, _, _, cx| {
                            this.active_lane = Lane::B;
                            cx.notify();
                        },
                    )),
                )
        };

        let top_bar = div()
            .w_full()
            .flex()
            .items_center()
            .justify_between()
            .gap(px(theme::GAP_SM))
            .child(recipe_row)
            .child(lane_row);

        // 第二行用于六通道曲线切换与当前通道的快捷预设按钮。
        let mut chan_tabs = div().flex().items_center().gap(px(theme::GAP_XS));
        for ch in CurveChannel::ALL {
            let active_in_plan = ch != CurveChannel::Gain && ch.is_active_in(plan);
            let label = if active_in_plan {
                format!("{} ·", ch.label())
            } else {
                ch.label().to_owned()
            };
            chan_tabs = chan_tabs.child(tab_chip(ch.id(), &label, channel == ch).on_click(
                cx.listener(move |this, _, _, cx| {
                    this.channel = ch;
                    if ch.is_single_lane() {
                        this.active_lane = Lane::A;
                    }
                    cx.notify();
                }),
            ));
        }

        let quick_tools: AnyElement =
            match channel {
                CurveChannel::Gain => {
                    let mut presets = div().flex().items_center().gap(px(theme::GAP_XS));
                    for preset in CurvePreset::BUILTINS {
                        presets = presets.child(
                            tab_chip(
                                SharedString::from(format!("mix-preset-{preset:?}")),
                                preset.label(),
                                self.preset == preset,
                            )
                            .on_click(cx.listener(
                                move |this, _, _, cx| {
                                    this.apply_preset(preset, cx);
                                },
                            )),
                        );
                    }
                    presets.into_any_element()
                }
                CurveChannel::LowEq => {
                    let has_low = CurveChannel::LowEq.is_active_in(plan);
                    div()
                        .flex()
                        .items_center()
                        .gap(px(theme::GAP_XS))
                        .child(
                            tab_chip("mix-low-swap", "低频交换", has_low).on_click(cx.listener(
                                |this, _, _, cx| {
                                    this.apply_low_swap(true, cx);
                                },
                            )),
                        )
                        .child(tab_chip("mix-low-clear", "平直 0 dB", !has_low).on_click(
                            cx.listener(|this, _, _, cx| {
                                this.apply_low_swap(false, cx);
                            }),
                        ))
                        .into_any_element()
                }
                CurveChannel::MidEq => {
                    let has_mid = CurveChannel::MidEq.is_active_in(plan);
                    div()
                        .flex()
                        .items_center()
                        .gap(px(theme::GAP_XS))
                        .child(tab_chip("mix-mid-handover", "中频错峰", has_mid).on_click(
                            cx.listener(|this, _, _, cx| {
                                this.apply_mid_handover(true, cx);
                            }),
                        ))
                        .child(tab_chip("mix-mid-clear", "平直 0 dB", !has_mid).on_click(
                            cx.listener(|this, _, _, cx| {
                                this.apply_mid_handover(false, cx);
                            }),
                        ))
                        .into_any_element()
                }
                CurveChannel::HighEq => {
                    let has_high = CurveChannel::HighEq.is_active_in(plan);
                    div()
                        .flex()
                        .items_center()
                        .gap(px(theme::GAP_XS))
                        .child(
                            tab_chip("mix-high-handover", "高频交接", has_high).on_click(
                                cx.listener(|this, _, _, cx| {
                                    this.apply_high_handover(true, cx);
                                }),
                            ),
                        )
                        .child(tab_chip("mix-high-clear", "平直 0 dB", !has_high).on_click(
                            cx.listener(|this, _, _, cx| {
                                this.apply_high_handover(false, cx);
                            }),
                        ))
                        .into_any_element()
                }
                CurveChannel::Filter => {
                    let has_flt = CurveChannel::Filter.is_active_in(plan);
                    div()
                        .flex()
                        .items_center()
                        .gap(px(theme::GAP_XS))
                        .child(
                            tab_chip("mix-flt-rise", "高通推升", false).on_click(cx.listener(
                                |this, _, _, cx| {
                                    this.apply_filter_preset(1, cx);
                                },
                            )),
                        )
                        .child(
                            tab_chip("mix-flt-lpf", "低通退远", false).on_click(cx.listener(
                                |this, _, _, cx| {
                                    this.apply_filter_preset(-1, cx);
                                },
                            )),
                        )
                        .child(tab_chip("mix-flt-clear", "直通关闭", !has_flt).on_click(
                            cx.listener(|this, _, _, cx| {
                                this.apply_filter_preset(0, cx);
                            }),
                        ))
                        .into_any_element()
                }
                CurveChannel::Echo => {
                    let has_echo = CurveChannel::Echo.is_active_in(plan);
                    let beat_a = self.analysis_a.as_ref().map_or(0.5, |an| an.beat_period());
                    let cur_mult = if has_echo && beat_a > 0.05 {
                        plan.echo_delay_secs / beat_a
                    } else {
                        0.0
                    };
                    div()
                        .flex()
                        .items_center()
                        .gap(px(theme::GAP_XS))
                        .child(
                            tab_chip(
                                "mix-echo-half",
                                "½ 拍",
                                has_echo && (cur_mult - 0.5).abs() < 0.12,
                            )
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.apply_echo_beats(Some(0.5), cx);
                            })),
                        )
                        .child(
                            tab_chip(
                                "mix-echo-3q",
                                "¾ 拍",
                                has_echo && (cur_mult - 0.75).abs() < 0.12,
                            )
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.apply_echo_beats(Some(0.75), cx);
                            })),
                        )
                        .child(
                            tab_chip(
                                "mix-echo-1b",
                                "1 拍",
                                has_echo && (cur_mult - 1.0).abs() < 0.15,
                            )
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.apply_echo_beats(Some(1.0), cx);
                            })),
                        )
                        .child(
                            tab_chip("mix-echo-off", "关闭", !has_echo).on_click(cx.listener(
                                |this, _, _, cx| {
                                    this.apply_echo_beats(None, cx);
                                },
                            )),
                        )
                        .into_any_element()
                }
            };

        let toolbar = div()
            .w_full()
            .flex()
            .items_center()
            .justify_between()
            .gap(px(theme::GAP_SM))
            .child(chan_tabs)
            .child(quick_tools);

        let caption = match channel {
            CurveChannel::Gain => {
                "音量曲线（0%–100%）：拖拽控制点调整，双击空白处新增，右键移除（黑/白线 = A 轨，青线 = B 轨）"
                    .to_owned()
            }
            CurveChannel::LowEq | CurveChannel::MidEq | CurveChannel::HighEq => {
                "均衡曲线（-24 dB 至 +6 dB，实线基准为 0 dB）：拖拽控制点调整，双击新增，右键移除"
                    .to_owned()
            }
            CurveChannel::Filter => {
                "扫频滤镜（中线 0 为直通，向上为高通 HPF 变薄，向下为低通 LPF 闷暗）：拖拽控制点、双击新增、右键移除"
                    .to_owned()
            }
            CurveChannel::Echo => {
                if CurveChannel::Echo.is_active_in(plan) {
                    format!(
                        "A 轨节拍回声发送（延迟 {:.0} ms，反馈 {:.0}%）：干声切落后回声继续在 B 轨上方渐弱",
                        plan.echo_delay_secs * 1000.0,
                        plan.echo_feedback * 100.0,
                    )
                } else {
                    "回声发送已关闭：点击右侧节拍按钮或双击画布启用 A 轨节拍同步 Echo Out"
                        .to_owned()
                }
            }
        };

        // 为均衡（0 dB）与扫频滤镜（0.0 直通）提供显眼的基准水平线。
        let ref_zero_y = match channel {
            CurveChannel::LowEq
            | CurveChannel::MidEq
            | CurveChannel::HighEq
            | CurveChannel::Filter => Some(0.0_f32),
            CurveChannel::Gain | CurveChannel::Echo => None,
        };

        div()
            .w_full()
            .flex()
            .flex_col()
            .gap(px(theme::GAP_XS))
            .child(top_bar)
            .child(toolbar)
            .child(
                div()
                    .id("mix-curve-canvas")
                    .w_full()
                    .h(px(CURVE_H))
                    .bg(theme::window_base())
                    .border_1()
                    .border_color(theme::border())
                    .cursor_pointer()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, event: &MouseDownEvent, _, cx| {
                            this.handle_curve_left_down(event, cx);
                        }),
                    )
                    .on_mouse_down(
                        MouseButton::Right,
                        cx.listener(|this, event: &MouseDownEvent, _, cx| {
                            this.handle_curve_right_down(event, cx);
                        }),
                    )
                    .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _, cx| {
                        this.handle_curve_move(event, cx);
                    }))
                    .on_mouse_up(
                        MouseButton::Left,
                        cx.listener(|this, _, _, cx| this.finish_drag(cx)),
                    )
                    .on_mouse_up_out(
                        MouseButton::Left,
                        cx.listener(|this, _, _, cx| this.finish_drag(cx)),
                    )
                    .child(
                        canvas(capture, move |bounds, (), window, _cx| {
                            let left = f32::from(bounds.origin.x) + CURVE_PAD_X;
                            let top = f32::from(bounds.origin.y) + CURVE_PAD_Y;
                            let width = (f32::from(bounds.size.width) - 2.0 * CURVE_PAD_X).max(1.0);
                            let height =
                                (f32::from(bounds.size.height) - 2.0 * CURVE_PAD_Y).max(1.0);
                            let range = (max_v - min_v).max(1e-5);

                            // 背景 4×4 参考网格。
                            for step in 0..=4 {
                                let frac = step as f32 / 4.0;
                                paint::paint_rect(
                                    window,
                                    left,
                                    top + frac * height,
                                    width,
                                    1.0,
                                    theme::border_soft(),
                                );
                                paint::paint_rect(
                                    window,
                                    left + frac * width,
                                    top,
                                    1.0,
                                    height,
                                    theme::border_soft(),
                                );
                            }

                            if let Some(zero_v) = ref_zero_y {
                                let ny = ((zero_v - min_v) / range).clamp(0.0, 1.0);
                                let zy = top + (1.0 - ny) * height;
                                paint::paint_rect(
                                    window,
                                    left,
                                    zy,
                                    width,
                                    1.5,
                                    theme::text_faint().opacity(0.65),
                                );
                            }

                            let draw_curve =
                                |window: &mut Window,
                                 auto: &Automation,
                                 color: Rgba,
                                 bold: bool| {
                                    let compiled = auto.compile();
                                    let samples = 64usize;
                                    let pts: Vec<(f32, f32)> = (0..=samples)
                                        .map(|idx| {
                                            let frac = idx as f32 / samples as f32;
                                            let val = compiled.eval(frac).clamp(min_v, max_v);
                                            let ny = (val - min_v) / range;
                                            (left + frac * width, top + (1.0 - ny) * height)
                                        })
                                        .collect();
                                    paint::paint_polyline(
                                        window,
                                        &pts,
                                        if bold { 2.5 } else { 1.6 },
                                        color,
                                    );
                                    let sz = if bold { 8.0 } else { 6.0 };
                                    for pt in auto.points() {
                                        let px_x = left + pt.t * width;
                                        let ny = ((pt.value - min_v) / range).clamp(0.0, 1.0);
                                        let px_y = top + (1.0 - ny) * height;
                                        paint::paint_rect(
                                            window,
                                            px_x - sz * 0.5,
                                            px_y - sz * 0.5,
                                            sz,
                                            sz,
                                            color,
                                        );
                                    }
                                };

                            draw_curve(window, &curve_a, theme::text(), active_lane == Lane::A);
                            if let Some(ref cb) = curve_b {
                                draw_curve(
                                    window,
                                    cb,
                                    paint::track_b_color(),
                                    active_lane == Lane::B,
                                );
                            }
                            if let Some(frac) = playhead_frac {
                                paint::paint_playhead(window, left + frac * width, top, height);
                            }
                        })
                        .size_full(),
                    ),
            )
            .child(
                div()
                    .text_size(px(theme::FONT_MICRO))
                    .text_color(theme::text_faint())
                    .child(caption),
            )
            .into_any_element()
    }

    fn render_footer(has_override: bool, cx: &mut Context<Self>) -> AnyElement {
        div()
            .w_full()
            .pt(px(theme::GAP_SM))
            .border_t_1()
            .border_color(theme::border())
            .flex()
            .items_center()
            .justify_between()
            .gap(px(theme::GAP_SM))
            .child(
                action_btn("mix-preview", "▶ 试听过渡", true).on_click(cx.listener(
                    |this, _, _, cx| {
                        this.preview_play(cx);
                    },
                )),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(theme::GAP_SM))
                    .when(has_override, |row| {
                        row.child(
                            action_btn("mix-reset", "恢复自动", false).on_click(cx.listener(
                                |this, _, _, cx| {
                                    this.reset_override(cx);
                                },
                            )),
                        )
                    })
                    .child(
                        action_btn("mix-save", "保存为此对歌曲", true).on_click(cx.listener(
                            |this, _, _, cx| {
                                this.save_override(cx);
                            },
                        )),
                    ),
            )
            .into_any_element()
    }
}

fn render_meta_bar(
    an_a: &TrackAnalysis,
    an_b: &TrackAnalysis,
    plan: &MixPlan,
    drift_ms: Option<f32>,
    has_override: bool,
) -> AnyElement {
    let overlap_s = plan.overlap_duration().as_secs_f32();
    let bars = (overlap_s / an_a.bar_period()).round();
    let key_a = an_a
        .key
        .map_or_else(|| "未知调性".to_owned(), |k| k.camelot_code());
    let key_b = an_b
        .key
        .map_or_else(|| "未知调性".to_owned(), |k| k.camelot_code());

    let mut row = div()
        .w_full()
        .flex()
        .flex_wrap()
        .items_center()
        .gap(px(theme::GAP_SM))
        .child(info_chip(
            &format!("{} · {}", plan.kind.label(), plan.style.label()),
            true,
        ))
        .child(info_chip(
            &format!("{:.1} → {:.1} BPM", an_a.bpm, an_b.bpm),
            false,
        ))
        .child(info_chip(&format!("{key_a} → {key_b}"), false))
        .child(info_chip(
            &format!("重叠 {overlap_s:.1}s（约 {bars:.0} 小节）"),
            false,
        ));

    if plan.tempo_mode != TempoMode::Off {
        let ratio = plan.effective_tempo_ratio();
        row = row.child(info_chip(
            &format!("{} ×{ratio:.3}", plan.tempo_mode.label()),
            false,
        ));
    }

    if let Some(ms) = drift_ms
        && ms > 40.0
    {
        row = row.child(
            div()
                .px(px(theme::GAP_SM))
                .py(px(2.))
                .bg(theme::danger())
                .text_color(theme::text_on_danger())
                .text_size(px(theme::FONT_CAPTION))
                .child(format!("节拍漂移 ±{ms:.0} ms（超出建议预算）")),
        );
    }

    if has_override {
        row = row.child(info_chip("已自定义", false));
    }

    row.into_any_element()
}

fn info_chip(label: &str, accented: bool) -> AnyElement {
    div()
        .px(px(theme::GAP_SM))
        .py(px(2.))
        .bg(if accented {
            theme::accent()
        } else {
            theme::elevated()
        })
        .text_color(if accented {
            theme::text_on_accent()
        } else {
            theme::text()
        })
        .font(theme::field_font())
        .text_size(px(theme::FONT_CAPTION))
        .child(label.to_owned())
        .into_any_element()
}

fn legend_dot(hex: u32, label: &'static str) -> impl IntoElement {
    div()
        .flex()
        .items_center()
        .gap(px(4.))
        .text_size(px(theme::FONT_MICRO))
        .text_color(theme::text_muted())
        .child(div().size(px(7.)).rounded(px(2.)).bg(rgb(hex)))
        .child(label)
}

fn tab_chip(id: impl Into<ElementId>, label: &str, active: bool) -> Stateful<Div> {
    div()
        .id(id)
        .px(px(theme::GAP_SM))
        .py(px(3.))
        .bg(if active {
            theme::control()
        } else {
            theme::elevated()
        })
        .text_color(if active {
            theme::text_on_control()
        } else {
            theme::text_muted()
        })
        .text_size(px(theme::FONT_CAPTION))
        .font_weight(theme::WEIGHT_BUTTON)
        .cursor_pointer()
        .child(label.to_owned())
}

fn action_btn(id: &'static str, label: &str, primary: bool) -> Stateful<Div> {
    div()
        .id(id)
        .px(px(theme::GAP_MD))
        .py(px(6.))
        .bg(if primary {
            theme::control()
        } else {
            theme::elevated()
        })
        .text_color(if primary {
            theme::text_on_control()
        } else {
            theme::text()
        })
        .text_size(px(theme::FONT_CAPTION))
        .font_weight(theme::WEIGHT_BUTTON)
        .cursor_pointer()
        .hover(move |s| {
            if primary {
                s.bg(theme::accent()).text_color(theme::text_on_accent())
            } else {
                s.bg(theme::border())
            }
        })
        .child(label.to_owned())
}
