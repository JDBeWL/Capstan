//! 工业质感的自绘件，含切角、三色条、斜线纹与等高线。
//! 切角 = 可点击。在块的右下角盖一块底色三角，画在背景之后、内容之下。
//! 调用方必须给右侧留出 `cut + 一段固定值` 的内边距，文字才不会压进切角。

use std::cell::RefCell;
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

/// 启动首帧的三色条。从左往右铺满 600ms，只播一次。
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

/// 左上角切角，给底部面板与对话框这类从下或从上推进来的块用。
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

/// 两角各一片极淡的等高线，不闭合。几何由 [`crate::common::contour`] 生成，
/// 坡度叠加涟漪，只留横穿整片的开放线。
pub fn contour_fill() -> impl IntoElement {
    canvas(
        |_bounds, _window, _cx| (),
        move |bounds, (), window, _cx| {
            let (x, y) = (f32::from(bounds.origin.x), f32::from(bounds.origin.y));
            let scale = window.scale_factor();
            let (w, h) = (f32::from(bounds.size.width), f32::from(bounds.size.height));
            if w < 200.0 || h < 150.0 {
                return;
            }
            let ink = theme::text();
            // 右上。页头右侧那条空带，线从上边垂下来。
            let band_w = (w * 0.46).clamp(240.0, 460.0);
            let band_h = (h * 0.20).clamp(96.0, 148.0);
            paint_patch(
                window,
                ink.opacity(CONTOUR_INK),
                (x + w - band_w, y),
                (band_w, band_h),
                CONTOUR_SEED_TOP,
                scale,
            );
            // 左下。贴着左边与下边的一小片。它压在曲目上，比页头那条带淡一档。
            let corner_w = (w * 0.32).clamp(200.0, 340.0);
            let corner_h = (h * 0.26).clamp(120.0, 200.0);
            paint_patch(
                window,
                ink.opacity(CONTOUR_INK * 0.7),
                (x, y + h - corner_h),
                (corner_w, corner_h),
                CONTOUR_SEED_BOTTOM,
                scale,
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
/// 淡出用的指数。压过线性，片缘那几档掉得更快，线不会被片的矩形边掐断。
const CONTOUR_FALLOFF: f32 = 1.5;
/// 两片各用一个种子；换了就是另一片地形。
const CONTOUR_SEED_TOP: u32 = 20_261_013;
const CONTOUR_SEED_BOTTOM: u32 = 7_741;

/// 画一片等高线。第一次遇到这组尺寸、缩放与墨色时烘成位图，之后每帧只贴一次图。
/// 逐帧重跑路径光栅化约需 1.7ms，是这一屏最贵的绘制项。
fn paint_patch(
    window: &mut Window,
    ink: Rgba,
    origin: (f32, f32),
    extent: (f32, f32),
    seed: u32,
    scale: f32,
) {
    let Some(image) = patch_image(window, extent, seed, scale, ink) else {
        return;
    };
    let bounds = Bounds {
        origin: point(px(origin.0), px(origin.1)),
        size: gpui::size(px(extent.0), px(extent.1)),
    };
    window
        .paint_image(bounds, bounds, Corners::default(), image, 0, false)
        .ok();
}

/// 贴图缓存的键，由尺寸、种子、墨色与缩放组成；任一项变了才重烘。
type PatchKey = (i32, i32, u32, u32, u16);

thread_local! {
    static PATCH_IMAGES: RefCell<Vec<(PatchKey, std::sync::Arc<RenderImage>)>> =
        const { RefCell::new(Vec::new()) };
}

/// 保留最近几份。拖窗口时会连续产生好几个尺寸，留一份会一直重烘。
const PATCH_CACHE_LEN: usize = 8;

fn patch_image(
    window: &mut Window,
    extent: (f32, f32),
    seed: u32,
    scale: f32,
    ink: Rgba,
) -> Option<std::sync::Arc<RenderImage>> {
    let key: PatchKey = (
        extent.0.round() as i32,
        extent.1.round() as i32,
        seed,
        u32::from(ink),
        (scale * 100.0).round() as u16,
    );
    let hit = PATCH_IMAGES.with(|cache| {
        cache
            .borrow()
            .iter()
            .find(|(cached, _)| *cached == key)
            .map(|(_, image)| std::sync::Arc::clone(image))
    });
    if hit.is_some() {
        return hit;
    }

    let patched = crate::common::contour::patch(extent.0, extent.1, seed);
    let image = std::sync::Arc::new(RenderImage::new([bake(&patched, extent, scale, ink)?]));
    let evicted = PATCH_IMAGES.with(|cache| {
        let mut cache = cache.borrow_mut();
        cache.push((key, std::sync::Arc::clone(&image)));
        if cache.len() > PATCH_CACHE_LEN {
            Some(cache.remove(0).1)
        } else {
            None
        }
    });
    // 淘汰的贴图须从图集释放，否则连续拖窗口会累积无人使用的贴图。
    if let Some(old) = evicted {
        window.drop_image(old).ok();
    }
    Some(image)
}

/// 把一片等高线烘成 BGRA 位图，线宽、淡出档位、墨色都提前算进像素里。
/// 同一档内取最大覆盖，相邻线段在接头处才不会叠深；档与档之间是同一墨色的叠加，
/// 因此只需累一条 alpha 通道。
fn bake(
    patch: &crate::common::contour::Patch,
    extent: (f32, f32),
    scale: f32,
    ink: Rgba,
) -> Option<image::Frame> {
    let width = ((extent.0 * scale).ceil() as u32).max(1);
    let height = ((extent.1 * scale).ceil() as u32).max(1);
    if width > 4096 || height > 4096 {
        return None;
    }
    let len = (width * height) as usize;
    let mut cover = vec![0u8; len];
    let mut alpha = vec![0u8; len];
    let steps = usize::from(crate::common::contour::FADE_STEPS);

    for bucket in 1..=steps {
        for heavy in [false, true] {
            cover.fill(0);
            let half = if heavy { CONTOUR_W * 1.5 } else { CONTOUR_W } * scale * 0.5;
            let mut painted = false;
            for run in &patch.runs {
                if usize::from(run.weight) != bucket || run.heavy != heavy {
                    continue;
                }
                painted = true;
                for pair in run.points.windows(2) {
                    stamp(&mut cover, width, height, pair[0], pair[1], half, scale);
                }
            }
            if !painted {
                continue;
            }
            // 片心那一档用满墨色，往片缘一档比一档淡，最后一档几乎看不见。
            let strength = bucket as f32 / steps as f32;
            composite(&mut alpha, &cover, ink.a * strength.powf(CONTOUR_FALLOFF));
        }
    }

    let (r, g, b) = (
        (ink.r * 255.0).round() as u8,
        (ink.g * 255.0).round() as u8,
        (ink.b * 255.0).round() as u8,
    );
    let mut pixels = vec![0u8; len * 4];
    for (index, slot) in pixels.as_chunks_mut::<4>().0.iter_mut().enumerate() {
        // RenderImage 要 BGRA，通道顺序在这里就换好。
        slot[0] = b;
        slot[1] = g;
        slot[2] = r;
        slot[3] = alpha[index];
    }
    image::RgbaImage::from_raw(width, height, pixels).map(image::Frame::new)
}

/// 把一层的覆盖度并进 alpha 通道。同色叠加只需累 alpha，不必逐像素合成颜色。
fn composite(alpha: &mut [u8], cover: &[u8], level: f32) {
    for (dst, &cov) in alpha.iter_mut().zip(cover.iter()) {
        if cov == 0 {
            continue;
        }
        // level 已经把「档位强度 × 墨色 alpha」算进来，cover 是这一层的覆盖度。
        let src = (level * f32::from(cov) / 255.0 * 255.0)
            .round()
            .clamp(0.0, 255.0) as u32;
        let below = u32::from(*dst);
        // src over dst，a = a1 + a2(1 - a1)
        *dst = (src + below * (255 - src) / 255).min(255) as u8;
    }
}

/// 4×4 次采样铺一条线段。用线段自身的局部坐标判定内外，等价于一个方头矩形，
/// 比逐点做多边形内外测试便宜。
const SUBSAMPLES: u32 = 4;

fn stamp(
    cover: &mut [u8],
    width: u32,
    height: u32,
    a: (f32, f32),
    b: (f32, f32),
    half: f32,
    scale: f32,
) {
    let (x1, y1) = (a.0 * scale, a.1 * scale);
    let (x2, y2) = (b.0 * scale, b.1 * scale);
    let (dx, dy) = (x2 - x1, y2 - y1);
    let len_sq = dx * dx + dy * dy;
    if len_sq <= f32::EPSILON {
        return;
    }
    let len = len_sq.sqrt();
    let reach = (len + half).ceil();
    let min_x = (x1.min(x2) - reach).max(0.0) as u32;
    let min_y = (y1.min(y2) - reach).max(0.0) as u32;
    let max_x = (x1.max(x2) + reach).min((width - 1) as f32) as u32;
    let max_y = (y1.max(y2) + reach).min((height - 1) as f32) as u32;

    let inv = 1.0 / (SUBSAMPLES * SUBSAMPLES) as f32;
    for py in min_y..=max_y {
        for px in min_x..=max_x {
            let mut inside = 0u32;
            for sy in 0..SUBSAMPLES {
                for sx in 0..SUBSAMPLES {
                    let fx = px as f32 + (sx as f32 + 0.5) / SUBSAMPLES as f32;
                    let fy = py as f32 + (sy as f32 + 0.5) / SUBSAMPLES as f32;
                    let (rx, ry) = (fx - x1, fy - y1);
                    let along = (rx * dx + ry * dy) / len_sq;
                    let across = (rx * -dy + ry * dx) / len;
                    if (0.0..=1.0).contains(&along) && across.abs() <= half {
                        inside += 1;
                    }
                }
            }
            if inside > 0 {
                let cov = (inside as f32 * inv * 255.0).round() as u8;
                let slot = &mut cover[(py * width + px) as usize];
                *slot = (*slot).max(cov);
            }
        }
    }
}

/// 页面背景。两角各一片等高线，不含渐变与投影，也不叠别的纹样。
/// 纸面只有一层薄墨，避免压过标题与列表。
pub fn page_backdrop() -> impl IntoElement {
    div().absolute().inset_0().child(contour_fill())
}

/// 三色条必须是一条窄带，不能变成色块。
const _: () = assert!(crate::theme::TRI_H <= 4.0);
