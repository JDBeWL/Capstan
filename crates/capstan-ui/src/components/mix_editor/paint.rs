//! 过渡编辑器与设置页共用的自绘工具，绘制能量包络、节拍网格与自动化曲线。

use capstan_analysis::{TrackAnalysis, WAVEFORM_BIN_SECS};
use capstan_audio::{Automation, CurvePreset};

use crate::platform::*;
use crate::theme;

/// 轨道 B 与低频曲线的辅色（青薄荷），与强调色形成清晰区分。
pub const TRACK_B_HEX: u32 = 0x00c888;
/// 三频彩色波形图例色（低频暖珊瑚、中频翠绿、高频天青、人声段琥珀）。
pub const BAND_LOW_HEX: u32 = 0xf2573d;
pub const BAND_MID_HEX: u32 = 0x2ec78f;
pub const BAND_HIGH_HEX: u32 = 0x42adf5;
pub const VOCAL_MARK_HEX: u32 = 0xf59e0b;

/// 漂移超限或裁剪提示色，复用主题危险色。
pub fn track_b_color() -> Rgba {
    rgb(TRACK_B_HEX)
}

/// 在时间轴或曲线区绘制实时试听播放头竖线与顶部指示帽。
pub fn paint_playhead(window: &mut Window, x: f32, y: f32, h: f32) {
    if h <= 0.0 {
        return;
    }
    let color = theme::text();
    paint_rect(window, x - 1.0, y, 2.0, h, color.opacity(0.92));
    paint_rect(window, x - 4.0, y, 8.0, 4.0, color);
}

/// 填充矩形（基于 `Path` 构造，兼容全部 GPUI 渲染管线）。
pub fn paint_rect(window: &mut Window, x: f32, y: f32, w: f32, h: f32, color: Rgba) {
    if w <= 0.0 || h <= 0.0 {
        return;
    }
    let mut path = Path::new(point(px(x), px(y)));
    path.line_to(point(px(x + w), px(y)));
    path.line_to(point(px(x + w), px(y + h)));
    path.line_to(point(px(x), px(y + h)));
    window.paint_path(path, color);
}

/// 批量绘制折线带。将全部线段拼入单个 `Path` 一次提交，避免每段单独一次 draw call。
pub fn paint_polyline(window: &mut Window, pts: &[(f32, f32)], thickness: f32, color: Rgba) {
    if pts.len() < 2 || thickness <= 0.0 {
        return;
    }
    let half = thickness * 0.5;
    let mut path: Option<Path<Pixels>> = None;

    for seg in pts.windows(2) {
        let (x0, y0) = seg[0];
        let (x1, y1) = seg[1];
        let dx = x1 - x0;
        let dy = y1 - y0;
        let len = dx.hypot(dy);
        if len <= 1e-4 {
            continue;
        }
        let nx = -dy / len * half;
        let ny = dx / len * half;
        let p0 = point(px(x0 + nx), px(y0 + ny));
        let p1 = point(px(x1 + nx), px(y1 + ny));
        let p2 = point(px(x1 - nx), px(y1 - ny));
        let p3 = point(px(x0 - nx), px(y0 - ny));

        if let Some(existing) = path.as_mut() {
            existing.move_to(p0);
            existing.line_to(p1);
            existing.line_to(p2);
            existing.line_to(p3);
        } else {
            let mut created = Path::new(p0);
            created.line_to(p1);
            created.line_to(p2);
            created.line_to(p3);
            path = Some(created);
        }
    }

    if let Some(path) = path {
        window.paint_path(path, color);
    }
}

/// 绘制单条曲目轨道的三频彩色波形（缺失时回退到逐拍能量条）、人声区标尺与节拍、小节及乐句网格。
/// `time_origin` 为时间轴左端对应的该曲媒体时间（秒），`span` 为可视时间跨度（秒）。
#[allow(clippy::too_many_arguments)]
pub fn paint_track_lane(
    window: &mut Window,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    analysis: &TrackAnalysis,
    time_origin: f32,
    span: f32,
    bar_color: Rgba,
) {
    if w <= 1.0 || h <= 1.0 || span <= 0.1 {
        return;
    }
    let to_x = |t: f32| x + (t - time_origin) / span * w;
    let mid_y = y + h * 0.5;

    if analysis.waveform_rgb.is_empty() {
        // 旧缓存缺少 100 ms 彩色波形时回退到逐拍对称能量条。
        let beats = &analysis.beats;
        for (idx, &t0) in beats.iter().enumerate() {
            let t1 = beats
                .get(idx + 1)
                .copied()
                .unwrap_or(analysis.duration)
                .max(t0 + 0.05);
            if t1 < time_origin || t0 > time_origin + span {
                continue;
            }
            let rms = analysis
                .beat_rms
                .get(idx)
                .map_or(0.35, |&v| f32::from(v) / 255.0);
            let x0 = to_x(t0).clamp(x, x + w);
            let x1 = to_x(t1).clamp(x, x + w);
            let bw = (x1 - x0 - 1.0).max(1.0);
            let half_h = (rms * h * 0.44).max(2.0);
            paint_rect(
                window,
                x0,
                mid_y - half_h,
                bw,
                half_h * 2.0,
                bar_color.opacity(0.45),
            );
        }
    } else {
        // 100 ms 三频彩色波形，按可视格数聚合成最多约 160 柱，兼顾瞬态清晰度与绘制开销。
        let bins = &analysis.waveform_rgb;
        let b0 = ((time_origin / WAVEFORM_BIN_SECS).floor().max(0.0) as usize).min(bins.len());
        let b1 =
            (((time_origin + span) / WAVEFORM_BIN_SECS).ceil().max(0.0) as usize).min(bins.len());
        let step = ((b1.saturating_sub(b0)) / 160).max(1);
        let mut idx = b0;
        while idx < b1 {
            let end = (idx + step).min(b1);
            let mut peak = [0u8; 3];
            for &[lo, mid, hi] in &bins[idx..end] {
                peak[0] = peak[0].max(lo);
                peak[1] = peak[1].max(mid);
                peak[2] = peak[2].max(hi);
            }
            let lo = f32::from(peak[0]) / 255.0;
            let mid = f32::from(peak[1]) / 255.0;
            let hi = f32::from(peak[2]) / 255.0;
            let amp = (lo * 0.45 + mid * 0.35 + hi * 0.20).max(lo.max(mid).max(hi) * 0.85);
            if amp > 0.015 {
                let t0 = idx as f32 * WAVEFORM_BIN_SECS;
                let t1 = end as f32 * WAVEFORM_BIN_SECS;
                let x0 = to_x(t0).clamp(x, x + w);
                let x1 = to_x(t1).clamp(x, x + w);
                let bw = (x1 - x0 - 0.6).max(1.2);
                let half_h = (amp * h * 0.42).max(1.5);
                paint_rect(
                    window,
                    x0,
                    mid_y - half_h,
                    bw,
                    half_h * 2.0,
                    rgb_band_color(lo, mid, hi),
                );
            }
            idx = end;
        }
    }

    // 轨道顶边绘制人声活跃小节条，直观展示 A 人声收尾与 B 人声切入位置。
    if !analysis.bar_vocal.is_empty() {
        let vocal_color = rgb(VOCAL_MARK_HEX);
        let bar_count = analysis.bar_count();
        for bar in 0..bar_count {
            let level = analysis.bar_vocal_level(bar);
            if level < 0.45 {
                continue;
            }
            let Some(t0) = analysis.bar_time(bar) else {
                continue;
            };
            let t1 = analysis
                .bar_time(bar + 1)
                .unwrap_or(analysis.duration)
                .max(t0 + 0.1);
            if t1 < time_origin || t0 > time_origin + span {
                continue;
            }
            let x0 = to_x(t0).clamp(x, x + w);
            let x1 = to_x(t1).clamp(x, x + w);
            if x1 > x0 + 0.5 {
                paint_rect(
                    window,
                    x0,
                    y,
                    x1 - x0,
                    3.0,
                    vocal_color.opacity((0.35 + 0.55 * level).min(0.95)),
                );
            }
        }
    }

    // 节拍网格（细）、小节线（中）与乐句边界（粗）。
    let beats = &analysis.beats;
    let phase = usize::from(analysis.downbeat_phase % 4);
    let phrase_step = usize::from(analysis.phrase_bars.max(1));
    let phrase_offset = usize::from(analysis.phrase_offset_bars);
    let grid_color = theme::text_faint();

    for (idx, &t) in beats.iter().enumerate() {
        if t < time_origin || t > time_origin + span {
            continue;
        }
        let gx = to_x(t);
        let is_bar = idx >= phase && (idx - phase).is_multiple_of(4);
        let bar_idx = idx.saturating_sub(phase) / 4;
        let is_phrase = is_bar
            && bar_idx >= phrase_offset
            && (bar_idx - phrase_offset).is_multiple_of(phrase_step);

        if is_phrase {
            paint_rect(window, gx - 1.0, y, 2.0, h, theme::text().opacity(0.45));
        } else if is_bar {
            paint_rect(window, gx, y, 1.0, h, grid_color.opacity(0.35));
        } else {
            paint_rect(
                window,
                gx,
                y + h * 0.65,
                1.0,
                h * 0.35,
                grid_color.opacity(0.18),
            );
        }
    }
}

fn rgb_band_color(lo: f32, mid: f32, hi: f32) -> Rgba {
    let sum = (lo + mid + hi).max(1e-4);
    let r = ((lo * 0.95 + mid * 0.18 + hi * 0.26) / sum).clamp(0.0, 1.0);
    let g = ((lo * 0.34 + mid * 0.78 + hi * 0.68) / sum).clamp(0.0, 1.0);
    let b = ((lo * 0.24 + mid * 0.56 + hi * 0.98) / sum).clamp(0.0, 1.0);
    let hex = ((r * 255.0).round() as u32) << 16
        | ((g * 255.0).round() as u32) << 8
        | ((b * 255.0).round() as u32);
    rgb(hex).opacity(0.74)
}

/// 绘制设置页用的紧凑曲线预设预览图（双曲淡出与淡入）。
pub fn curve_preview(preset: CurvePreset) -> impl IntoElement {
    let out_curve = Automation::preset_out(preset).compile();
    let in_curve = Automation::preset_in(preset).compile();

    div()
        .flex_none()
        .w(px(120.))
        .h(px(36.))
        .bg(theme::elevated())
        .border_1()
        .border_color(theme::border())
        .child(
            canvas(
                |_bounds, _window, _cx| (),
                move |bounds, (), window, _cx| {
                    let left = f32::from(bounds.origin.x) + 6.0;
                    let top = f32::from(bounds.origin.y) + 5.0;
                    let width = (f32::from(bounds.size.width) - 12.0).max(1.0);
                    let height = (f32::from(bounds.size.height) - 10.0).max(1.0);

                    let samples = 32usize;
                    let pts_a: Vec<(f32, f32)> = (0..=samples)
                        .map(|idx| {
                            let frac = idx as f32 / samples as f32;
                            let val = out_curve.eval(frac).clamp(0.0, 1.0);
                            (left + frac * width, top + (1.0 - val) * height)
                        })
                        .collect();
                    let pts_b: Vec<(f32, f32)> = (0..=samples)
                        .map(|idx| {
                            let frac = idx as f32 / samples as f32;
                            let val = in_curve.eval(frac).clamp(0.0, 1.0);
                            (left + frac * width, top + (1.0 - val) * height)
                        })
                        .collect();

                    paint_polyline(window, &pts_a, 2.0, theme::text());
                    paint_polyline(window, &pts_b, 2.0, track_b_color());
                },
            )
            .size_full(),
        )
}
