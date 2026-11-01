//! 一次性入场与未知进度。短、硬、机械，时长和曲线都取自 `theme`。
//! gpui 的 `Animation` 没有 delay，错开用一段不改样式的先行动画拼出来。

use std::time::Duration;

use crate::platform::*;
use crate::theme;

/// 列表行错开，38ms 一行。
pub const STAGGER_ROW_MS: u64 = 38;
/// 换内容后等用户停手再错开入场；这段时间用来合并连续输入。
pub const STAGGER_DEBOUNCE_MS: u64 = 180;
/// 一次入场只照顾首屏这些行；滚出去再回来不重播。
pub const STAGGER_ROWS: usize = 12;
/// 错开窗口，超过它就不再把新挂载的行算进这次入场。
pub const STAGGER_WINDOW_MS: u64 = 900;
/// 大块内容错开，100ms 一块。
pub const STAGGER_BLOCK_MS: u64 = 100;
/// 首屏数据比骨架晚 120ms 开始。
pub const DATA_DELAY_MS: u64 = 120;
/// 逐行亮起与表面出现。
pub const MOTION_COME_MS: u64 = 320;
/// 封面解码完成后的淡入。
pub const MOTION_COVER_MS: u64 = 180;
/// 未知进度条跑完一圈。
pub const MOTION_LOADER_MS: u64 = 1200;
/// 字标推入，慢起硬停。
pub const MOTION_DRIVE_MS: u64 = 620;
/// 字标第二行相对第一行的错开。
pub const WORDMARK_STAGGER_MS: u64 = 140;
/// 首屏三色条扫过。
pub const MOTION_SWEEP_MS: u64 = 600;
/// 读数乱码落定。
pub const MOTION_SCRAMBLE_MS: u64 = 600;
/// 悬停时读数重新落定。
pub const MOTION_SCRAMBLE_HOVER_MS: u64 = 360;
/// 分段控件与页签的墨块滑动。
pub const MOTION_SEG_MS: u64 = 280;
/// 导航当前块滑动，与换页同档。
pub const MOTION_NAV_MS: u64 = 360;

/// 出现。透明 + 从左侧滑到位，snap 曲线。
pub fn enter<E>(
    id: impl Into<ElementId>,
    delay_ms: u64,
    duration_ms: u64,
    offset: f32,
    el: E,
) -> AnimationElement<E>
where
    E: Styled + IntoElement + 'static,
{
    reveal(
        el,
        id.into(),
        delay_ms,
        duration_ms,
        offset,
        theme::ease_snap(),
    )
}

/// 换页。用页面色盖板从右往左退开，前缘留一道墨线；慢起硬停。
/// GPUI 这一支没有 clip-path，盖板擦除是快门的近似。
pub fn page_enter<E>(id: impl Into<ElementId>, el: E) -> AnimationElement<E>
where
    E: Styled + ParentElement + IntoElement + 'static,
{
    el.with_animation(
        id,
        Animation::new(Duration::from_millis(theme::MOTION_PAGE_MS))
            .with_easing(theme::ease_mech()),
        |el, t| {
            let revealed = t.clamp(0.0, 1.0);
            el.relative().child(
                canvas(
                    |_bounds, _window, _cx| (),
                    move |bounds, (), window, _cx| {
                        if revealed >= 1.0 {
                            return;
                        }
                        let x = f32::from(bounds.origin.x);
                        let y = f32::from(bounds.origin.y);
                        let w = f32::from(bounds.size.width);
                        let h = f32::from(bounds.size.height);
                        if w <= 0.0 || h <= 0.0 {
                            return;
                        }
                        let cover_w = w * (1.0 - revealed);
                        let cover_x = x + w - cover_w;
                        let mut cover = Path::new(point(px(cover_x), px(y)));
                        cover.line_to(point(px(x + w), px(y)));
                        cover.line_to(point(px(x + w), px(y + h)));
                        cover.line_to(point(px(cover_x), px(y + h)));
                        window.paint_path(cover, theme::bg());

                        let edge = 2.0;
                        let mut line = Path::new(point(px(cover_x), px(y)));
                        line.line_to(point(px(cover_x + edge), px(y)));
                        line.line_to(point(px(cover_x + edge), px(y + h)));
                        line.line_to(point(px(cover_x), px(y + h)));
                        window.paint_path(line, theme::text().opacity(0.85));
                    },
                )
                .absolute()
                .inset_0(),
            )
        },
    )
}

/// 字标推入。透明 + 从左侧滑到位，慢起硬停。
pub fn drive<E>(id: impl Into<ElementId>, delay_ms: u64, el: E) -> AnimationElement<E>
where
    E: Styled + IntoElement + 'static,
{
    reveal(
        el,
        id.into(),
        delay_ms,
        MOTION_DRIVE_MS,
        16.0,
        theme::ease_mech(),
    )
}

/// 分段控件的当前块位置，索引之间按 280ms mech 插值。
pub fn segment_slide(
    id: impl Into<ElementId>,
    index: usize,
    window: &mut Window,
    cx: &mut App,
) -> f32 {
    slide(id, index, MOTION_SEG_MS, window, cx)
}

/// 导航的当前块位置。与换页同档，360ms mech。
pub fn nav_slide(id: impl Into<ElementId>, index: usize, window: &mut Window, cx: &mut App) -> f32 {
    slide(id, index, MOTION_NAV_MS, window, cx)
}

fn slide(
    id: impl Into<ElementId>,
    index: usize,
    duration_ms: u64,
    window: &mut Window,
    cx: &mut App,
) -> f32 {
    motion::transition(
        (id.into(), SharedString::from("segment")),
        index as f32,
        motion::Transition::new(Duration::from_millis(duration_ms)).ease(theme::ease_mech()),
        window,
        cx,
    )
}

/// 乱码落定。progress 从 0 到 1，数字逐位从左往右锁定，未锁定的位持续翻滚。
pub fn scramble(text: &str, progress: f32) -> String {
    if progress >= 1.0 {
        return text.to_owned();
    }
    let chars: Vec<char> = text.chars().collect();
    let locked = (progress * chars.len() as f32).floor() as usize;
    let tick = (progress * 60.0) as u32;
    chars
        .iter()
        .enumerate()
        .map(|(index, ch)| {
            if !ch.is_ascii_digit() || index < locked {
                return *ch;
            }
            let salt = (index as u32)
                .wrapping_mul(0x9E37_79B9)
                .wrapping_add(tick.wrapping_mul(0x85EB_CA6B));
            char::from_digit((salt >> 16) % 10, 10).unwrap_or('0')
        })
        .collect()
}

/// 数据入场 + 乱码落定。延迟期间只占位不显示，之后按 duration 线性锁定。
/// `build` 每帧拿到 0→1 的进度，用 [`scramble`] 生成当前文字。
pub fn scramble_content<E, C>(
    id: impl Into<ElementId>,
    delay_ms: u64,
    duration_ms: u64,
    el: E,
    build: impl Fn(f32) -> C + 'static,
) -> AnimationElement<E>
where
    E: Styled + ParentElement + IntoElement + 'static,
    C: IntoElement + 'static,
{
    let mut animations = Vec::with_capacity(2);
    if delay_ms > 0 {
        animations.push(Animation::new(Duration::from_millis(delay_ms)));
    }
    animations.push(Animation::new(Duration::from_millis(duration_ms)));

    el.with_animations(id, animations, move |el, ix, t| {
        let waiting = delay_ms > 0 && ix == 0;
        let progress = if waiting { 0.0 } else { t };
        el.opacity(if waiting { 0.0 } else { 1.0 })
            .child(build(progress))
    })
}

fn reveal<E>(
    el: E,
    id: ElementId,
    delay_ms: u64,
    duration_ms: u64,
    offset: f32,
    easing: impl Fn(f32) -> f32 + 'static,
) -> AnimationElement<E>
where
    E: Styled + IntoElement + 'static,
{
    let mut animations = Vec::with_capacity(2);
    if delay_ms > 0 {
        animations.push(Animation::new(Duration::from_millis(delay_ms)));
    }
    animations.push(Animation::new(Duration::from_millis(duration_ms)).with_easing(easing));

    el.with_animations(id, animations, move |el, ix, t| {
        // 延迟那一段停在起始状态；最后一段才真正动。
        let shown = if ix == 0 && delay_ms > 0 { 0.0 } else { t };
        el.opacity(shown)
            .relative()
            .left(px(-offset * (1.0 - shown)))
    })
}

/// 不知道进度的加载条。24 格底格，6 格黄块按格从左走到右，一圈 1.2s。
/// 元素只在加载期间存在，完成即停；减少动态效果时停在首格。
pub fn unknown_progress() -> impl IntoElement {
    const CELLS: usize = 24;
    const RUN: usize = 6;

    div()
        .flex_none()
        .w_full()
        .max_w(px(360.))
        .h(px(10.))
        .flex()
        .gap(px(2.))
        .with_animation(
            "unknown-progress",
            Animation::new(Duration::from_millis(MOTION_LOADER_MS)).repeat(),
            |bar, t| {
                let head = ((t * CELLS as f32) as usize) % CELLS;
                bar.children((0..CELLS).map(move |i| {
                    let lit = (i + CELLS - head) % CELLS < RUN;
                    div().flex_1().h_full().bg(if lit {
                        theme::accent()
                    } else {
                        theme::elevated()
                    })
                }))
            },
        )
}
