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

/// 两角各一片极淡的等高线，不闭合、像水流。铺满所在容器。
/// 几何由 [`crate::common::contour`] 生成：坡度加涟漪上的等值线，只留横穿整片的开放线。
/// 两片各贴一条边、各朝一边淡出，所以看上去没有轮廓，也没有一条线是直挺挺划过去的。
pub fn contour_fill() -> impl IntoElement {
    canvas(
        |_bounds, _window, _cx| (),
        move |bounds, (), window, _cx| {
            let (x, y) = (f32::from(bounds.origin.x), f32::from(bounds.origin.y));
            let (w, h) = (f32::from(bounds.size.width), f32::from(bounds.size.height));
            if w < 200.0 || h < 150.0 {
                return;
            }
            let ink = theme::text();
            // 右上：页头右侧那条空带，线从上边垂下来。
            let band_w = (w * 0.46).clamp(240.0, 460.0);
            let band_h = (h * 0.20).clamp(96.0, 148.0);
            paint_patch(
                window,
                ink.opacity(CONTOUR_INK),
                (x + w - band_w, y),
                (band_w, band_h),
                CONTOUR_SEED_TOP,
            );
            // 左下：贴着左边与下边的一小片。它压在曲目上，比页头那条带淡一档。
            let corner_w = (w * 0.32).clamp(200.0, 340.0);
            let corner_h = (h * 0.26).clamp(120.0, 200.0);
            paint_patch(
                window,
                ink.opacity(CONTOUR_INK * 0.7),
                (x, y + h - corner_h),
                (corner_w, corner_h),
                CONTOUR_SEED_BOTTOM,
            );
        },
    )
    .absolute()
    .size_full()
}

/// 一整片等高线的墨色。只做底色质感，深色主题里同样是一层薄墨。
const CONTOUR_INK: f32 = 0.085;
/// 线宽。比 1px 细线略粗一点，缩放到高分屏才不会掉成虚线。
const CONTOUR_W: f32 = 1.2;
/// 淡出用的指数：压过线性，片缘那几档掉得更快，线不会被片的矩形边掐断。
const CONTOUR_FALLOFF: f32 = 1.5;
/// 两片各用一个种子；换了就是另一片地形。
const CONTOUR_SEED_TOP: u32 = 20_261_013;
const CONTOUR_SEED_BOTTOM: u32 = 7_741;

fn paint_patch(window: &mut Window, ink: Rgba, origin: (f32, f32), size: (f32, f32), seed: u32) {
    let patch = crate::common::contour::patch(size.0, size.1, seed);
    let steps = usize::from(crate::common::contour::FADE_STEPS);
    // 每一档两条：普通线一条、计曲线一条，计曲线要粗一点只能分开填。
    let lanes = steps * 2;
    let mut paths: Vec<Path<Pixels>> = (0..lanes)
        .map(|_| Path::new(point(px(0.), px(0.))))
        .collect();
    let mut used = vec![false; lanes];
    for run in &patch.runs {
        let bucket = usize::from(run.weight.max(1) - 1).min(steps - 1);
        let index = bucket * 2 + usize::from(run.heavy);
        let width = if run.heavy {
            CONTOUR_W * 1.5
        } else {
            CONTOUR_W
        };
        emit_run(&mut paths[index], run, origin, width);
        used[index] = true;
    }
    for (index, path) in paths.into_iter().enumerate() {
        if !used[index] {
            continue;
        }
        // 片心那一档用满墨色，往片缘一档比一档淡，最后一档几乎看不见。
        let strength = (index / 2 + 1) as f32 / steps as f32;
        window.paint_path(path, ink.opacity(strength.powf(CONTOUR_FALLOFF)));
    }
}

/// 一条等值线画成一根细带：`paint_path` 是填充不是描边，
/// 直接填一条折线会被首尾连起来，变成一大片墨。
fn emit_run(
    path: &mut Path<Pixels>,
    run: &crate::common::contour::Run,
    origin: (f32, f32),
    width: f32,
) {
    let at = |(x, y): (f32, f32)| point(px(origin.0 + x), px(origin.1 + y));
    for pair in run.points.windows(2) {
        let ((x1, y1), (x2, y2)) = (pair[0], pair[1]);
        let (dx, dy) = (x2 - x1, y2 - y1);
        let len = dx.hypot(dy).max(0.001);
        let (nx, ny) = (-dy / len * width * 0.5, dx / len * width * 0.5);
        path.move_to(at((x1 + nx, y1 + ny)));
        path.line_to(at((x2 + nx, y2 + ny)));
        path.line_to(at((x2 - nx, y2 - ny)));
        path.line_to(at((x1 - nx, y1 - ny)));
    }
}

/// 页面背景：两角各一片等高线。不含渐变与投影，也不叠别的纹样 ——
/// 纸面上只有一层薄墨，标题、列表和按钮读起来才不吵。
pub fn page_backdrop() -> impl IntoElement {
    div().absolute().inset_0().child(contour_fill())
}

/// 三色条必须是一条窄带，不能变成色块。
const _: () = assert!(crate::theme::TRI_H <= 4.0);
