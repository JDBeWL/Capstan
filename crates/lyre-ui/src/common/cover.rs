//! 封面槽的共用取图流程：查缓存 → 未命中且并非「确定无封面」才发请求 → 否则由调用方画占位。

use std::sync::Arc;
use std::time::Duration;

use lyre_core::TrackId;

use crate::cover::CoverCache;
use crate::entities::LibraryEntity;
use crate::icon::{Glyph, icon_inherit};
use crate::platform::*;
use crate::theme;

/// 无封面占位的底色：墨色块配白字，像一张编号标签。不引入第二种强调色。
pub fn monogram_color(_initial: char) -> Rgba {
    theme::text()
}

/// 封面槽的三态。必须区分「排队中」与「没有封面」：
/// 前者画骨架屏（内容在路上），后者才是真占位（首字色块 / 人形图标 / 音符）。
pub enum Cover {
    Ready(Arc<RenderImage>),
    Missing,
    Pending,
}

/// 取封面。未命中且并非「确定无封面」时发起一次解码请求，返回 [`Cover::Pending`]。
/// 分组场景要传该组的 `representative`（第一个有封面的曲目）而非第一首。
/// `size` 是这一格的逻辑像素边长：缓存按它分档取图。
pub fn cover_state(
    covers: &Entity<CoverCache>,
    library: &Entity<LibraryEntity>,
    id: TrackId,
    size: f32,
    cx: &mut App,
) -> Cover {
    let cached = covers.read(cx).ready(id, size);
    if let Some(image) = cached {
        return Cover::Ready(image);
    }
    // 漏掉这一档，无封面的曲目会每帧重发一次解码请求
    if covers.read(cx).is_missing(id, size) {
        return Cover::Missing;
    }

    // 每个可见行都要走一次，所以必须是哈希查表而不是线性扫
    let path = App::update_entity(cx, library, |library, _| {
        library.playlist().by_id(id).map(|track| track.path.clone())
    });
    match path {
        Some(path) => {
            covers.update(cx, |covers, cx| covers.request(id, path, size, cx));
            Cover::Pending
        }
        // 曲库里查不到这首：没有可解码的来源，直接判缺失
        None => Cover::Missing,
    }
}

pub fn cover_image(
    covers: &Entity<CoverCache>,
    library: &Entity<LibraryEntity>,
    id: TrackId,
    size: f32,
    cx: &mut App,
) -> Option<Arc<RenderImage>> {
    match cover_state(covers, library, id, size, cx) {
        Cover::Ready(image) => Some(image),
        Cover::Missing | Cover::Pending => None,
    }
}

/// 解码期间的骨架块：浅灰 chip 色，不闪光、不渐变。
pub fn skeleton(height: f32, radius: f32) -> Div {
    div()
        .w_full()
        .h(px(height))
        .rounded(px(radius))
        .bg(theme::elevated())
}

/// 歌单封面的一格：用户指定的图 → 成员的内嵌封面 → 按名字派生的渐变块。
#[allow(clippy::too_many_arguments)]
pub fn list_cover(
    covers: &Entity<CoverCache>,
    library: &Entity<LibraryEntity>,
    size: f32,
    radius: f32,
    cover_file: Option<&std::path::Path>,
    cover_id: Option<TrackId>,
    seed: &str,
    cx: &mut App,
) -> AnyElement {
    if let Some(path) = cover_file {
        let owned = path.to_path_buf();
        covers.update(cx, |cache, cx| cache.request_file(owned, size, cx));
        return match covers.read(cx).ready_file(path, size) {
            Some(image) => image_cell(image, size, radius),
            // 还在解码、或那张图根本读不出来：退到派生块
            None => derived_cover(size, radius, seed),
        };
    }

    let Some(id) = cover_id else {
        // 空歌单没有任何曲目能代表封面
        return derived_cover(size, radius, seed);
    };
    match cover_state(covers, library, id, size, cx) {
        Cover::Ready(image) => image_cell(image, size, radius),
        Cover::Missing => derived_cover(size, radius, seed),
        Cover::Pending => div()
            .flex_none()
            .size(px(size))
            .rounded(px(radius))
            .child(skeleton(size, radius))
            .into_any_element(),
    }
}

/// 列表 / 侧栏的行首缩略图，尺寸、圆角与缺封面时的占位图标由参数给。
pub fn thumb(
    covers: &Entity<CoverCache>,
    library: &Entity<LibraryEntity>,
    id: TrackId,
    size: f32,
    radius: f32,
    placeholder: Glyph,
    cx: &mut App,
) -> AnyElement {
    let state = cover_state(covers, library, id, size, cx);

    let cell = div()
        .flex_none()
        .size(px(size))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(radius))
        .bg(theme::elevated());

    match state {
        Cover::Ready(image) => cell
            .child(
                img(ImageSource::Render(image))
                    .size_full()
                    .rounded(px(radius))
                    .object_fit(ObjectFit::Cover),
            )
            .into_any_element(),
        Cover::Missing => cell
            .border_1()
            .border_color(theme::border())
            .text_color(theme::text_faint())
            .child(icon_inherit(placeholder, 16.))
            .into_any_element(),
        Cover::Pending => cell.child(skeleton(size, radius)).into_any_element(),
    }
}

/// 一张已解码的封面铺满那一格。
pub fn image_cell(image: Arc<RenderImage>, size: f32, radius: f32) -> AnyElement {
    let key = Arc::as_ptr(&image);
    let cell = div()
        .flex_none()
        .size(px(size))
        .rounded(px(radius))
        .overflow_hidden()
        .child(
            img(ImageSource::Render(image))
                .size_full()
                .rounded(px(radius))
                .object_fit(ObjectFit::Cover),
        );
    fade_in_key(key, cell).into_any_element()
}

/// 解码完成的一格淡入。key 带图片指针：换图重播，同一张图重绘不重播。
pub fn fade_in<E>(image: &Arc<RenderImage>, el: E) -> AnimationElement<E>
where
    E: Styled + IntoElement + 'static,
{
    fade_in_key(Arc::as_ptr(image), el)
}

fn fade_in_key<E>(key: *const RenderImage, el: E) -> AnimationElement<E>
where
    E: Styled + IntoElement + 'static,
{
    el.with_animation(
        SharedString::from(format!("cover-{key:p}")),
        Animation::new(Duration::from_millis(crate::common::anim::MOTION_COVER_MS))
            .with_easing(theme::ease_std()),
        Styled::opacity,
    )
}

fn derived_cover(size: f32, radius: f32, _seed: &str) -> AnyElement {
    div()
        .relative()
        .flex_none()
        .size(px(size))
        .rounded(px(radius))
        .overflow_hidden()
        .flex()
        .items_center()
        .justify_center()
        .bg(theme::elevated())
        .border_1()
        .border_color(theme::border())
        .child(crate::common::ornament::hatch_fill(
            theme::text().opacity(0.06),
        ))
        .child(crate::icon::icon(
            crate::icon::Glyph::Music,
            (size * 0.25).round(),
            theme::text(),
        ))
        .into_any_element()
}
