//! 行内各格，含勾选框、标题与副标题、专辑，以及行尾收藏。

use capstan_core::{Track, TrackId, UNKNOWN_ALBUM, UNKNOWN_ARTIST};

use crate::cover::CoverCache;
use crate::entities::{LibraryEntity, ScopeKind};
use crate::icon::{Glyph, icon, icon_inherit};
use crate::platform::*;
use crate::theme;

use super::row::RowState;
use super::{CHECK_BOX, CHECK_W, LIKE_W, RowEnv, TrackList, TrackListEvent};
use crate::components::track_drag::{DragGhost, DragTrack};

pub(super) fn check_cell(
    index: usize,
    track: TrackId,
    picked: bool,
    list: WeakEntity<TrackList>,
) -> AnyElement {
    div()
        .id(("row-check", index))
        .flex_none()
        .w(px(CHECK_W))
        .h_full()
        .flex()
        .items_center()
        .justify_center()
        .cursor_pointer()
        .tooltip(crate::common::tooltip::builder(if picked {
            "取消选中"
        } else {
            "选中（Shift 连选）"
        }))
        .on_click(move |event, _window, cx| {
            let shift = event.modifiers().shift;
            list.update(cx, |list, cx| list.pick_row(index, track, shift, cx))
                .ok();
        })
        .child(
            div()
                .size(px(CHECK_BOX))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(theme::RADIUS_SM))
                .border_1()
                .border_color(if picked {
                    theme::control()
                } else {
                    theme::text_faint()
                })
                .bg(if picked {
                    theme::control()
                } else {
                    theme::transparent()
                })
                .when(picked, |frame| {
                    frame.child(icon(Glyph::Check, 12., theme::text_on_control()))
                }),
        )
        .into_any_element()
}

#[allow(clippy::too_many_arguments)]
pub(super) fn title_cell(
    index: usize,
    track: &Track,
    state: RowState,
    env: RowEnv,
    library: &Entity<LibraryEntity>,
    list: &WeakEntity<TrackList>,
    covers: &Entity<CoverCache>,
    track_id: TrackId,
    cx: &mut App,
) -> impl IntoElement {
    let two_lines = env.two_lines();
    let artist = track.artist.clone().unwrap_or_default();
    let thumb_size = env.thumb_size();
    let cover = crate::common::cover::thumb(
        covers,
        library,
        track_id,
        thumb_size,
        theme::RADIUS_SM,
        Glyph::Music,
        crate::common::cover::initial_of(&track.title),
        cx,
    );

    let row_library = library.clone();
    let row_list = list.clone();
    let selectable = env.selectable;

    let mut cell = div()
        .id(("track-pick", index))
        .flex()
        .flex_col()
        .flex_1()
        .min_w_0()
        .pr(px(theme::GAP_MD))
        .justify_center()
        .cursor_pointer()
        // 拖到队列行或侧栏歌单。轻点仍是播放 / 勾选，gpui 只在真的拖出距离后才起拖。
        .on_drag(
            DragTrack {
                id: track_id,
                title: SharedString::from(track.title.clone()),
            },
            |drag: &DragTrack, _, _, cx| cx.new(|_| DragGhost::new(drag.title.clone())),
        )
        .on_click(move |event, _, cx| {
            if selectable {
                let shift = event.modifiers().shift;
                row_list
                    .update(cx, |list, cx| list.pick_row(index, track_id, shift, cx))
                    .ok();
            } else {
                row_library.update(cx, |library, cx| {
                    library.activate_in_view(track_id, cx);
                });
            }
        });

    let mut title_line = div()
        .flex()
        .items_center()
        .gap(px(theme::GAP_SM))
        .min_w_0()
        .child(
            div()
                .id(("track-title", index))
                .min_w_0()
                .flex_shrink(1.0)
                .truncate()
                .text_size(px(theme::FONT_TRACK))
                .font_weight(state.title_weight())
                .text_color(RowState::title_color())
                // 只给标题文字挂提示，挂在整个标题格上会盖住下面的艺术家与专辑链接。
                .tooltip(crate::common::tooltip::builder(track.title.clone()))
                .child(track.title.clone()),
        );

    if two_lines {
        // 副标题自己拿宽度。`w_full` 在 gpui 下解不出一个确定宽度，`truncate` 就只剩裁切、
        // 没有省略号；专辑列之所以能收尾，是因为它的单元格写死了宽度。
        let subtitle_w =
            (env.title_width - env.thumb_size() - theme::GAP_MD - theme::GAP_MD).max(0.0);
        cell = cell.child(title_line).child(subtitle_line(
            index,
            track.artist.clone(),
            library,
            list,
            subtitle_w,
        ));
    } else {
        // 紧凑密度只有一行。标题与艺术家挤在同一段宽度里，两边都必须能收缩，
        // 否则长艺术家名会画到专辑列上。艺术家 shrink 更大，先让位。
        if !artist.is_empty() && env.kind != ScopeKind::Artist {
            title_line = title_line.child(
                div()
                    .min_w_0()
                    .flex_shrink(2.0)
                    .truncate()
                    .text_size(px(theme::FONT_CAPTION))
                    .text_color(theme::text_muted())
                    .child(artist),
            );
        }
        cell = cell.child(title_line);
    }

    // 点击区只覆盖文字块，封面不可点。标题列是固定宽，拖表头分隔线才会变。
    div()
        .flex()
        .items_center()
        .gap(px(theme::GAP_MD))
        .flex_none()
        .w(px(env.title_width))
        .child(cover)
        .child(cell)
}

/// 超过这么多字符就不再拆成链接、整行当一段文字排。
/// 一排可点的名字各自 truncate，只会把每个名字都切成两三个字，反倒谁也读不出来。
const SUBTITLE_PLAIN_CHARS: usize = 40;

/// 副标题：艺术家。短名单拆成可点的人名，长名单（合唱曲目的 CV 墙）整行一句。
fn subtitle_line(
    index: usize,
    artist: Option<String>,
    library: &Entity<LibraryEntity>,
    list: &WeakEntity<TrackList>,
    width: f32,
) -> AnyElement {
    let base = div()
        .w(px(width))
        .min_w_0()
        .text_size(px(theme::FONT_CAPTION))
        .text_color(theme::text_muted());

    // 「未知艺术家」不可点，点进去是空作用域。
    let Some(name) = artist.filter(|a| capstan_core::artist_key_str(Some(a)) != UNKNOWN_ARTIST)
    else {
        return base.into_any_element();
    };
    let parts = capstan_core::artist_keys_str(Some(&name));
    // 只有名单本身太长时才退回整行一句。人数多但名字短（「甲 / 乙 / 丙」）时逐个可点，
    // 每一位都跳得过去，行宽也放得下。
    if name.chars().count() > SUBTITLE_PLAIN_CHARS {
        return base
            .id(SharedString::from(format!("subtitle-name-{index}")))
            .truncate()
            .tooltip(crate::common::tooltip::builder(name.clone()))
            .child(name)
            .into_any_element();
    }

    let mut out = Vec::new();
    for (position, part) in parts.iter().enumerate() {
        if position > 0 {
            out.push(artist_sep());
        }
        // 元素 id 必须逐行、逐人唯一，重复的 id 会让 gpui 复用前一个元素的命中区域。
        out.push(artist_link(
            format!("subtitle-artist-{index}-{position}"),
            (*part).to_owned(),
            library.clone(),
            list.clone(),
        ));
    }
    base.flex().items_center().children(out).into_any_element()
}

/// gpui 的鼠标事件会冒泡到标题格的 `on_click`，必须 `stop_propagation`，
/// 否则一次点击既进详情又把这首送进播放器。
fn artist_link(
    id: String,
    name: String,
    library: Entity<LibraryEntity>,
    list: WeakEntity<TrackList>,
) -> AnyElement {
    let target = name.clone();
    div()
        .id(SharedString::from(id))
        .min_w_0()
        // gpui 的 flex 项默认不收缩，光有 `truncate` 只会被父级裁掉、没有省略号。
        .flex_shrink(1.0)
        .truncate()
        .cursor_pointer()
        .hover(|style| style.text_color(theme::text()).underline())
        .tooltip(crate::common::tooltip::builder(name.clone()))
        .on_click(move |_, _, cx| {
            cx.stop_propagation();
            // 先记落点再收窄作用域，顺序不能反。
            list.update(cx, |_, cx| cx.emit(TrackListEvent::RecordFromTrack))
                .ok();
            library.update(cx, |library, cx| library.enter_artist(target.clone(), cx));
        })
        .child(name)
        .into_any_element()
}

/// 专辑列的一格，可点进这张专辑的详情页。
pub(super) fn album_cell(
    index: usize,
    album: Option<&str>,
    width: f32,
    library: Entity<LibraryEntity>,
    list: WeakEntity<TrackList>,
) -> AnyElement {
    // 空 / 「未知专辑」不可点，点进去是空作用域。
    let known = capstan_core::album_key_str(album) != UNKNOWN_ALBUM;
    let text = album.unwrap_or("—").to_owned();

    let mut cell = div()
        .flex_none()
        .w(px(width))
        // 给左缘的分隔线让出与标题侧相同的间距。
        .pl(px(theme::GAP_MD))
        .pr(px(theme::GAP_MD))
        .text_size(px(theme::FONT_CAPTION))
        .text_color(theme::text_muted());

    if known {
        let target = text.clone();
        cell = cell.child(
            div()
                .id(("album-link", index))
                .min_w_0()
                .truncate()
                .cursor_pointer()
                .hover(|style| style.text_color(theme::text()).underline())
                .tooltip(crate::common::tooltip::builder(text.clone()))
                .on_click(move |_, _, cx| {
                    list.update(cx, |_, cx| cx.emit(TrackListEvent::RecordFromTrack))
                        .ok();
                    library.update(cx, |library, cx| library.enter_album(target.clone(), cx));
                })
                .child(text),
        );
    } else {
        cell = cell.truncate().child(text);
    }
    cell.into_any_element()
}

fn artist_sep() -> AnyElement {
    div().flex_none().child(" / ").into_any_element()
}

/// 收藏键。已收藏或正在播放时常显，其余悬停才浮现。
/// 删除等操作留在右键菜单里，行内不再放会误触的按钮。
pub(super) fn like_cell(
    index: usize,
    state: RowState,
    group: SharedString,
    track_id: TrackId,
    liked: bool,
    library: &Entity<LibraryEntity>,
) -> impl IntoElement {
    let visible = liked || state.is_playing();

    div()
        .flex_none()
        .w(px(LIKE_W))
        .flex()
        .items_center()
        .justify_center()
        .child(
            div()
                .id(("track-like", index))
                .flex_none()
                .size(px(28.))
                .flex()
                .items_center()
                .justify_center()
                .text_color(if liked {
                    theme::text()
                } else {
                    theme::text_faint()
                })
                .cursor_pointer()
                .opacity(if visible { 1.0 } else { 0.0 })
                .when(!visible, |button| {
                    button.group_hover(group, |style| style.opacity(1.0))
                })
                .hover(|style| style.bg(theme::elevated()).text_color(theme::text()))
                .tooltip(crate::common::tooltip::builder(if liked {
                    "取消收藏"
                } else {
                    "收藏"
                }))
                .on_click({
                    let library = library.clone();
                    move |_, _, cx| {
                        library.update(cx, |library, cx| {
                            library.toggle_like(track_id, cx);
                        });
                    }
                })
                .child(icon_inherit(
                    if liked {
                        Glyph::HeartFilled
                    } else {
                        Glyph::Heart
                    },
                    15.,
                )),
        )
}
