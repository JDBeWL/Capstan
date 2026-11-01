//! 工业质感的自绘件：切角、三色条、斜线纹、等高线。
//! 切角 = 可点击：在块的右下角盖一块底色三角，画在背景之后、内容之下。
//! 调用方必须给右侧留出 `cut + 一段固定值` 的内边距，文字才不会压进切角。

use std::time::Duration;

use crate::platform::*;
use crate::theme;

/// 三色条的品红与薄荷绿。中间一格永远跟着强调色。
const TRI_MAGENTA: u32 = 0xff00f0;
const TRI_MINT: u32 = 0x00ffa2;

/// 3px 三色条。只放在页面最顶上（以及详情标题下方）。
pub fn tri_strip() -> impl IntoElement {
    div()
        .flex_none()
        .h(px(theme::TRI_H))
        .w_full()
        .flex()
        .child(div().flex_1().h_full().bg(rgb(TRI_MAGENTA)))
        .child(div().flex_1().h_full().bg(theme::accent()))
        .child(div().flex_1().h_full().bg(rgb(TRI_MINT)))
}

/// 启动首帧的三色条：从左往右铺满 600ms，只播一次。
pub fn tri_strip_sweep() -> impl IntoElement {
    div()
        .flex_none()
        .h(px(theme::TRI_H))
        .w_full()
        .overflow_hidden()
        .with_animation(
            "startup-tri-sweep",
            Animation::new(Duration::from_millis(crate::common::anim::MOTION_SWEEP_MS))
                .with_easing(theme::ease_std()),
            |el, t| {
                el.child(
                    div()
                        .h_full()
                        .w(relative(t))
                        .flex()
                        .child(div().flex_1().h_full().bg(rgb(TRI_MAGENTA)))
                        .child(div().flex_1().h_full().bg(theme::accent()))
                        .child(div().flex_1().h_full().bg(rgb(TRI_MINT))),
                )
            },
        )
}

/// 右下角切角。`backdrop` 是块所在容器的底色，用来盖掉三角区域。
/// 作为块的首个子元素使用；绝对定位，不参与布局。
pub fn cut_br(size: f32, backdrop: Rgba) -> impl IntoElement {
    canvas(
        |_bounds, _window, _cx| (),
        move |bounds, (), window, _cx| {
            let (x, y) = (f32::from(bounds.origin.x), f32::from(bounds.origin.y));
            let (w, h) = (f32::from(bounds.size.width), f32::from(bounds.size.height));
            let mut path = Path::new(point(px(x), px(y + h)));
            path.line_to(point(px(x + w), px(y)));
            path.line_to(point(px(x + w), px(y + h)));
            window.paint_path(path, backdrop);
        },
    )
    .absolute()
    .bottom_0()
    .right_0()
    .w(px(size))
    .h(px(size))
}

/// 左上角切角，给底部面板 / 对话框这类从下或从上推进来的块用。
pub fn cut_tl(size: f32, backdrop: Rgba) -> impl IntoElement {
    canvas(
        |_bounds, _window, _cx| (),
        move |bounds, (), window, _cx| {
            let (x, y) = (f32::from(bounds.origin.x), f32::from(bounds.origin.y));
            let (w, h) = (f32::from(bounds.size.width), f32::from(bounds.size.height));
            let mut path = Path::new(point(px(x), px(y)));
            path.line_to(point(px(x + w), px(y)));
            path.line_to(point(px(x), px(y + h)));
            window.paint_path(path, backdrop);
        },
    )
    .absolute()
    .top_0()
    .left_0()
    .w(px(size))
    .h(px(size))
}

/// 45° 斜线纹（/ 方向），铺满所在容器。线宽 1.5px、周期 6.5px。
/// 容器要 `relative`；作为背景层时放在第一个子元素。
pub fn hatch_fill(color: Rgba) -> impl IntoElement {
    canvas(
        |_bounds, _window, _cx| (),
        move |bounds, (), window, _cx| {
            let (x, y) = (f32::from(bounds.origin.x), f32::from(bounds.origin.y));
            let (w, h) = (f32::from(bounds.size.width), f32::from(bounds.size.height));
            let step = 9.2;
            let line = 1.5;
            let mut path = Path::new(point(px(x), px(y)));
            let mut start = -h;
            while start < w {
                let x0 = start;
                path.move_to(point(px(x + x0), px(y + h)));
                path.line_to(point(px(x + x0 + line), px(y + h)));
                path.line_to(point(px(x + x0 + h + line), px(y)));
                path.line_to(point(px(x + x0 + h), px(y)));
                start += step;
            }
            window.paint_path(path, color);
        },
    )
    .absolute()
    .size_full()
}

/// 两角各一小片极淡的等高线，不闭合、像水流。铺满所在容器。
pub fn contour_fill() -> impl IntoElement {
    canvas(
        |_bounds, _window, _cx| (),
        move |bounds, (), window, _cx| {
            let (x, y) = (f32::from(bounds.origin.x), f32::from(bounds.origin.y));
            let (w, h) = (f32::from(bounds.size.width), f32::from(bounds.size.height));
            if w < 40.0 || h < 40.0 {
                return;
            }
            let color = theme::text().opacity(0.07);
            let mut path = Path::new(point(px(x), px(y)));
            // 右上角：三条向下弯的水流线，向边缘渐隐由透明度很低的整体色负责。
            for (offset, amp) in [(0.0, 10.0), (12.0, 18.0), (24.0, 10.0)] {
                let pts = curve_points(
                    (w * 0.56, -12.0 + offset),
                    (w * 0.94, h * 0.18 + offset),
                    amp,
                );
                push_ribbon(&mut path, &pts, 1.2, x, y);
            }
            // 左下角：两小片。
            for (offset, amp) in [(0.0, -8.0), (14.0, -14.0)] {
                let pts = curve_points(
                    (-12.0 + offset, h * 0.74),
                    (w * 0.22 + offset, h * 0.94),
                    amp,
                );
                push_ribbon(&mut path, &pts, 1.2, x, y);
            }
            window.paint_path(path, color);
        },
    )
    .absolute()
    .size_full()
}

fn curve_points(from: (f32, f32), to: (f32, f32), amp: f32) -> Vec<(f32, f32)> {
    let mut pts = Vec::with_capacity(25);
    for i in 0..=24 {
        let t = i as f32 / 24.0;
        let x = from.0 + (to.0 - from.0) * t;
        let y = from.1 + (to.1 - from.1) * t + amp * (t * std::f32::consts::PI).sin();
        pts.push((x, y));
    }
    pts
}

/// 把折线加宽成一条细带，追加进同一个 `Path`。
fn push_ribbon(path: &mut Path<Pixels>, pts: &[(f32, f32)], width: f32, ox: f32, oy: f32) {
    for pair in pts.windows(2) {
        let ((x1, y1), (x2, y2)) = (pair[0], pair[1]);
        let (dx, dy) = (x2 - x1, y2 - y1);
        let len = (dx * dx + dy * dy).sqrt().max(0.001);
        let (nx, ny) = (-dy / len * width * 0.5, dx / len * width * 0.5);
        path.move_to(point(px(ox + x1 + nx), px(oy + y1 + ny)));
        path.line_to(point(px(ox + x2 + nx), px(oy + y2 + ny)));
        path.line_to(point(px(ox + x2 - nx), px(oy + y2 - ny)));
        path.line_to(point(px(ox + x1 - nx), px(oy + y1 - ny)));
    }
}

/// 页面背景：两角等高线 + 右上角一条极淡的斜线纹带。不含渐变与投影。
pub fn page_backdrop() -> impl IntoElement {
    div().absolute().inset_0().child(contour_fill()).child(
        div()
            .absolute()
            .top_0()
            .right_0()
            .w(px(300.))
            .h(px(104.))
            .child(hatch_fill(theme::text().opacity(0.045))),
    )
}

/// 三色条必须是一条窄带，不能变成色块。
const _: () = assert!(crate::theme::TRI_H <= 4.0);
