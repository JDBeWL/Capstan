//! 行的四态、一行的骨架，以及列表的列头。

use std::time::Duration;

use lyre_core::{Track, TrackId, format_duration};

use crate::cover::CoverCache;
use crate::entities::{LibraryEntity, ScopeKind};
use crate::icon::{Glyph, icon};
use crate::platform::*;
use crate::theme;

use super::{
    CHECK_W, HANDLE_W, LIKE_W, RowEnv, TrackList, TrackListEvent, cells, columns, drag,
};

/// 专辑列要不要画。断点允许，且专辑详情里永远不画（每行都是同一张专辑）。
pub(super) fn show_album_column(breakpoint_allows: bool, kind: ScopeKind) -> bool {
    breakpoint_allows && kind != ScopeKind::Album
}

/// 列头。三处分隔线可拖：每根线管它左边那一列（NO./标题/专辑）。
/// 主区多余宽度落在时长右侧的空白里，拖一列不会牵动别的列。
pub(super) fn column_header(env: &RowEnv, list: WeakEntity<TrackList>) -> impl IntoElement {
    let mut row = div()
        .flex_none()
        .h(px(theme::COLUMN_HEADER_H))
        .flex()
        .items_center()
        .pl(px(theme::ROW_PAD_LEFT))
        .pr(px(theme::GAP_MD))
        .border_b_1()
        .border_color(theme::text())
        .font(theme::label_font())
        .text_size(px(theme::FONT_EYEBROW))
        .font_weight(theme::WEIGHT_BUTTON)
        .text_color(theme::text_faint())
        // 勾选列与手柄列没有表头文字，但要占同样的宽，否则切换多选会在每行前面挖出一道错位。
        .when(env.selectable, |row| {
            row.child(div().flex_none().w(px(CHECK_W)))
        })
        .when(env.reorderable, |row| {
            row.child(div().flex_none().w(px(HANDLE_W)))
        })
        .child(
            div()
                .relative()
                .flex_none()
                .h_full()
                .w(px(env.columns.index))
                .flex()
                .items_center()
                .justify_end()
                .pr(px(theme::ROW_INDEX_GAP))
                .child("NO.")
                .child(columns::handle(
                    columns::ColumnEdge::Index,
                    env.columns.index,
                    env.gap,
                    list.clone(),
                )),
        )
        .child(
            div()
                .relative()
                .flex_none()
                .h_full()
                .w(px(env.title_width))
                .flex()
                .items_center()
                // 标题格前面还有封面缩略图；列头让出同一段宽度才和歌名同一条竖线。
                .pl(px(env.thumb_size() + theme::GAP_MD))
                .child(div().min_w_0().truncate().child("TITLE / 标题"))
                .child(columns::handle(
                    columns::ColumnEdge::Title,
                    env.title_width,
                    env.gap,
                    list.clone(),
                )),
        );

    if env.show_album {
        row = row.child(
            div()
                .relative()
                .flex_none()
                .h_full()
                .w(px(env.album_width))
                .flex()
                .items_center()
                // 给左缘的分隔线让出与标题侧相同的间距。
                .pl(px(theme::GAP_MD))
                .child(div().min_w_0().truncate().child("ALBUM / 专辑"))
                .child(columns::handle(
                    columns::ColumnEdge::Album,
                    env.album_width,
                    env.gap,
                    list.clone(),
                )),
        );
    }
    row.child(
        // 表头与行里的心形同宽同中心；心形居中，和右边时长留出间距。
        div()
            .flex_none()
            .w(px(LIKE_W))
            .flex()
            .items_center()
            .justify_center()
            .child("LIKE / 喜欢"),
    )
    .child(
        div()
            .flex_none()
            .w(px(theme::COLUMN_TIME_W))
            .flex()
            .items_center()
            .justify_start()
            .child("TIME / 时长"),
    )
    .child(div().flex_1())
}

/// 行状态，四个态互斥。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum RowState {
    Idle,
    /// 光标所在行（键盘操作的目标）
    Focused,
    /// 正在播放。
    Playing,
    /// 既在播放又是光标行。
    PlayingFocused,
}

/// 删除动画传进每一行的位移，让本行擦出、下方的行先向上补位。
#[derive(Clone, Copy, Default)]
pub(super) struct RowMotion {
    pub(super) remove: f32,
    pub(super) shift: f32,
}

impl RowState {
    pub(super) fn of(id: TrackId, playing: Option<TrackId>, focus: Option<TrackId>) -> Self {
        match (playing == Some(id), focus == Some(id)) {
            (true, true) => Self::PlayingFocused,
            (true, false) => Self::Playing,
            (false, true) => Self::Focused,
            (false, false) => Self::Idle,
        }
    }

    pub(super) fn is_playing(self) -> bool {
        matches!(self, Self::Playing | Self::PlayingFocused)
    }

    fn is_focused(self) -> bool {
        matches!(self, Self::Focused | Self::PlayingFocused)
    }

    fn background(self) -> Rgba {
        // 播放行靠行首黄块与粗体标出，不再整行铺淡黄，黄色面积要小。
        match self {
            Self::Focused => theme::cursor(),
            Self::Playing | Self::PlayingFocused | Self::Idle => theme::transparent(),
        }
    }

    /// 画不画行首那道竖条（= 键盘光标在哪）。两个播放态的底色与字色相同，只能靠它分开。
    /// 竖条是绝对定位，不做 flex 子元素或 `border`。那两者参与布局，会把整表的列推歪。
    fn shows_cursor_bar(self) -> bool {
        self.is_focused()
    }

    /// 黄只做填充；文字一律墨色，播放态靠字重与行首黄块区分。
    pub(super) fn title_color() -> Rgba {
        theme::text()
    }

    pub(super) fn title_weight(self) -> FontWeight {
        if self.is_playing() {
            theme::WEIGHT_SUBTITLE
        } else {
            theme::WEIGHT_TRACK
        }
    }
}

/// 行首黄块切角露出的底色。光标行是 chip 色，其余是白板。
fn if_focused_backdrop(state: RowState) -> Rgba {
    if state.is_focused() {
        theme::cursor()
    } else {
        theme::bg()
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn row(
    index: usize,
    track: &Track,
    state: RowState,
    picked: bool,
    env: RowEnv,
    library: &Entity<LibraryEntity>,
    list: &WeakEntity<TrackList>,
    covers: &Entity<CoverCache>,
    motion: RowMotion,
    // `(世代, 延迟 ms)`，这一行属于本轮逐行入场。
    enter: Option<(u64, u64)>,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let track_id = track.id;
    // 行高只从 `env.row_height` 来。`uniform_list` 按它算每一项的位置，这里另算一份就会错位。
    let row_height = env.row_height;
    let group: SharedString = format!("track-row-{index}").into();
    let hover_id = ElementId::from(("track-row", index));
    let hover = crate::common::hover::row_progress(hover_id.clone(), window, cx);
    let idle = matches!(state, RowState::Idle);
    let liked = library.read(cx).is_liked(track_id);

    let mut element = div()
        .id(hover_id.clone())
        .group(group.clone())
        // 落点区是绝对定位的，要有这个才钉在本行而不是钉在列表上。
        .relative()
        .h(px(row_height))
        .w_full()
        // `min_w_0` 与 `w_full` 配套。否则行收缩不到比内容更窄，长标题会把专辑列和时长列顶歪。
        .min_w_0()
        .flex()
        .items_center()
        .pl(px(
            theme::ROW_PAD_LEFT + if idle { 6.0 * hover } else { 0.0 }
        ))
        .pr(px(theme::GAP_MD))
        .border_b_1()
        .border_color(theme::border_soft())
        .bg(state.background())
        .on_hover(crate::common::hover::bind(hover_id));

    // 悬停底色只给空闲行；光标行已有底色，叠上去会分不清。
    if idle {
        element = element.child(div().absolute().inset_0().bg(theme::hover()).opacity(hover));
    }

    // 悬停时左边落下一道 4px 当前条，高度随悬停进度落到底；深色主题里这道线是黄。
    element = element.child(
        div()
            .absolute()
            .left(px(0.))
            .top(px(0.))
            .w(px(4.))
            .h(relative(hover))
            .bg(theme::trace()),
    );

    // `left(0)` 相对行的 padding box，绝对定位不挤占任何一列。
    if state.shows_cursor_bar() {
        element = element.child(
            div()
                .absolute()
                .left(px(0.))
                .top(px(row_height * 0.25))
                .w(px(theme::ROW_CURSOR_BAR_W))
                .h(px(row_height * 0.5))
                .bg(theme::trace()),
        );
    }

    // 行首两列都定宽，切换多选或进出歌单不该让整表的列跳一下。
    if env.selectable {
        element = element.child(cells::check_cell(index, track_id, picked, list.clone()));
    }
    if env.reorderable {
        // 拖拽影像要的是标题，只有真能拖的时候才付一次 clone。
        element = element.child(drag::handle_cell(index, track.title.clone(), group.clone()));
    }

    let index_cell: AnyElement = if state.is_playing() {
        div()
            .flex_none()
            .w(px(env.columns.index))
            .flex()
            .justify_end()
            .pr(px(theme::ROW_INDEX_GAP))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_center()
                    .w(px(24.))
                    .h(px(18.))
                    .bg(theme::accent())
                    .relative()
                    .child(crate::common::ornament::cut_br(
                        theme::CUT_SM,
                        if_focused_backdrop(state),
                    ))
                    .child(
                        div()
                            .font(theme::field_font())
                            .text_size(px(theme::FONT_CAPTION))
                            .text_color(theme::text_on_accent())
                            .child(format!("{}", index + 1)),
                    ),
            )
            .into_any_element()
    } else {
        // 序号与悬停播放键重叠在同一格，用 `group_hover` 切换两层透明度。
        // 不用父级 `hover` + 每行的 `hovered` 布尔。滑过一行就 notify 一次，滚动时每秒几十次重绘。
        let number_color = if state.is_focused() {
            theme::text_muted()
        } else {
            theme::text_faint()
        };

        div()
            .id(("track-index", index))
            .flex_none()
            .relative()
            .w(px(env.columns.index))
            .flex()
            .items_center()
            .justify_end()
            .pr(px(theme::ROW_INDEX_GAP))
            .cursor_pointer()
            // 点序号 = 播放这一首，与点标题同走 `activate`。
            .on_click({
                let library = library.clone();
                move |_, _, cx| {
                    library.update(cx, |library, cx| {
                        library.activate_in_view(track_id, cx);
                    });
                }
            })
            .child(
                div()
                    .font(theme::field_font())
                    .text_size(px(theme::FONT_BODY))
                    .text_color(number_color)
                    .group_hover(group.clone(), |style| style.opacity(0.0))
                    .child(format!("{}", index + 1)),
            )
            .child(
                div()
                    .absolute()
                    .right(px(theme::ROW_INDEX_GAP))
                    .top(relative(0.5))
                    .mt(px(-6.))
                    .size(px(12.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .opacity(0.0)
                    .group_hover(group.clone(), |style| style.opacity(1.0))
                    .child(icon(Glyph::Play, 10., theme::text())),
            )
            .into_any_element()
    };

    element = element.child(index_cell).child(cells::title_cell(
        index, track, state, env, library, list, covers, track_id, cx,
    ));

    if env.show_album {
        element = element.child(cells::album_cell(
            index,
            track.album.as_deref(),
            env.album_width,
            library.clone(),
            list.clone(),
        ));
    }

    let hooks = crate::common::menu::TrackMenuHooks::none()
        .before_leave({
            let list = list.clone();
            move |cx| {
                // 先报落点再收窄作用域，顺序反了会记成「进入之后」
                list.update(cx, |_, cx| cx.emit(TrackListEvent::RecordFromTrack))
                    .ok();
            }
        })
        .remove({
            let list = list.clone();
            move |id, cx| {
                list.update(cx, |list, cx| list.begin_remove(id, index, cx))
                    .is_ok()
            }
        });
    let menu = || crate::common::menu::track_context_menu(library.clone(), track_id, hooks.clone());

    element = element
        .child(cells::like_cell(
            index,
            state,
            group,
            track_id,
            liked,
            library,
        ))
        .child(
            div()
                .flex_none()
                .w(px(theme::COLUMN_TIME_W))
                .flex()
                .items_center()
                .justify_start()
                .font(theme::field_font())
                .text_size(px(theme::FONT_CAPTION))
                .text_color(theme::text_muted())
                .child(
                    track
                        .duration
                        .map_or_else(|| "--:--".to_owned(), format_duration),
                ),
        )
        // 剩下的宽度留在时长右侧，与列头的空白同一条位置。
        .child(div().flex_1());

    // 落点高亮用 `drag_over` 而不是视图状态，拖到行外松手不会留下残留高亮。
    if env.reorderable {
        element = element
            .child(drag::drop_zone(
                index,
                drag::DropEdge::Above,
                row_height,
                library.clone(),
            ))
            .child(drag::drop_zone(
                index,
                drag::DropEdge::Below,
                row_height,
                library.clone(),
            ));
    }

    // 换内容后的逐行亮起。从左滑入并淡入，延迟按行号错开。
    if let Some((generation, delay_ms)) = enter {
        let key = ElementId::named_usize(
            "row-enter",
            (generation as usize).wrapping_mul(1024) + index,
        );
        let progress = motion::transition(
            key,
            1.0,
            motion::Transition::new(Duration::from_millis(crate::common::anim::MOTION_COME_MS))
                .delay(Duration::from_millis(delay_ms))
                .ease(theme::ease_snap()),
            window,
            cx,
        );
        element = element.opacity(progress).left(px(-14.0 * (1.0 - progress)));
    }

    // 引擎解不了的格式照常列出，只是压暗并给出原因（判据见 `lyre_core::UNSUPPORTED_EXTENSIONS`）
    if let Some(reason) = track.unplayable_reason() {
        element = element
            .opacity(theme::UNPLAYABLE_OPACITY)
            .tooltip(crate::common::tooltip::builder(reason));
    }

    // 删除。本行向右擦出并变淡；它下面的行同步向上补位，等真正移除时正好接住。
    if motion.shift != 0.0 {
        element = element.top(px(motion.shift));
    }
    if motion.remove > 0.0 {
        element = element
            .left(px(14.0 * motion.remove))
            .opacity(1.0 - motion.remove);
    }

    // `context_menu` 返回 `ContextMenu<Div>` 而不是 `Div`，只能放在链式末尾。
    element.context_menu(menu()).into_any_element()
}

#[cfg(test)]
mod tests {
    use super::{RowState, show_album_column};
    use crate::entities::ScopeKind;
    use lyre_core::TrackId;

    #[test]
    fn album_column_vanishes_inside_an_album_detail() {
        assert!(show_album_column(true, ScopeKind::All));
        assert!(show_album_column(true, ScopeKind::Artist));
        assert!(!show_album_column(true, ScopeKind::Album));
        assert!(!show_album_column(false, ScopeKind::All));
    }

    #[test]
    fn row_state_separates_cursor_from_playback() {
        use RowState::{Focused, Idle, Playing, PlayingFocused};
        let a = TrackId(1);
        let b = TrackId(2);

        assert_eq!(RowState::of(a, Some(a), Some(a)), PlayingFocused);
        assert_eq!(RowState::of(a, Some(a), Some(b)), Playing);
        assert_eq!(RowState::of(a, Some(b), Some(a)), Focused);
        assert_eq!(RowState::of(a, None, None), Idle);
    }

    #[test]
    fn playing_rows_are_visually_distinct_from_focused_rows() {
        assert!(RowState::Playing.is_playing());
        assert!(!RowState::Focused.is_playing());
        assert!(RowState::Focused.is_focused());
        assert!(!RowState::Playing.is_focused());
        // 两者都用墨字；区分靠「播放 = 行首黄块 + 粗体」和「光标 = 左侧竖条」。
        assert_ne!(
            RowState::Playing.title_weight(),
            RowState::Focused.title_weight(),
            "播放行必须比光标行更粗，否则只靠形状区分不够"
        );
        assert_ne!(
            RowState::Playing.background(),
            RowState::Focused.background(),
            "光标行有底色，播放行没有"
        );
    }

    #[test]
    fn playing_focused_row_shows_the_cursor_bar() {
        assert_eq!(
            RowState::Playing.background(),
            RowState::PlayingFocused.background()
        );
        assert_eq!(RowState::title_color(), RowState::title_color());
        assert_eq!(
            RowState::Playing.title_weight(),
            RowState::PlayingFocused.title_weight()
        );

        assert!(!RowState::Playing.shows_cursor_bar());
        assert!(RowState::PlayingFocused.shows_cursor_bar());
        assert!(RowState::Focused.shows_cursor_bar());
        assert!(!RowState::Idle.shows_cursor_bar());
    }

    #[test]
    fn cursor_row_is_not_the_hover_color() {
        assert_ne!(RowState::Focused.background(), crate::theme::hover());
        assert_ne!(RowState::Focused.background(), RowState::Idle.background());
    }

    #[test]
    fn playing_row_is_bold_so_color_is_not_the_only_channel() {
        assert_ne!(
            RowState::Playing.title_weight(),
            RowState::Idle.title_weight()
        );
        assert_ne!(
            RowState::Playing.title_weight(),
            RowState::Focused.title_weight()
        );
    }
}
