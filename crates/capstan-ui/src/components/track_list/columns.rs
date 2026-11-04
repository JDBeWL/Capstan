//! 表头列宽拖拽。分隔线是绝对定位的细条，只改相邻列的宽度，不参与布局。
//! gpui 没有单独的手势 API，这里借拖拽系统的 `on_drag_move`。它面向「调整大小」这类
//! 非拖放交互，拖动开始后指针移出元素仍能收到移动事件。

use crate::platform::*;
use crate::settings::TrackColumns;
use crate::theme;

use super::TrackList;

/// 拖的是哪条分隔线。每根线管它左边那一列。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum ColumnEdge {
    Index,
    Title,
    Album,
}

impl ColumnEdge {
    fn id(self) -> usize {
        match self {
            Self::Index => 0,
            Self::Title => 1,
            Self::Album => 2,
        }
    }
}

/// 拖拽载荷只带方向。宽度由指针的绝对位置反推，不从按下点累加：
/// 按下会让 gpui 重绘一帧，元素闭包里存的起点跟着重建，累加式会从 0 重新开始。
struct ColumnDrag {
    edge: ColumnEdge,
}

/// gpui 要求拖拽影像是视图实体。调列宽不需要跟随鼠标的卡片，给一个空占位视图。
struct NoGhost;

impl Render for NoGhost {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
    }
}

/// 命中宽度。细了不好抓，粗了会盖住表头文字。
const HIT_W: f32 = 9.0;

/// 把手中心 + 当前宽度 → 新宽度。把手都画在列的右缘，左缘固定，宽度 = 指针 - 固定边。
/// `center` 与 `current` 取自同一帧，固定边不随拖动漂移。
fn width_from_pointer(center: f32, current: f32, pointer: f32) -> f32 {
    pointer - (center - current)
}

/// 把新宽度写回对应的列，其余列不动。改出来的差值记到空白上：
/// 标题是剩余宽度，因此只有拖动标题列本身才会改变标题，拖别的列只动那一列和空白。
pub(super) fn apply(columns: &mut TrackColumns, edge: ColumnEdge, old: f32, new: f32, gap: f32) {
    let delta = new - old;
    match edge {
        ColumnEdge::Index => columns.index = new,
        ColumnEdge::Album => columns.album = new,
        ColumnEdge::Title => {}
    }
    columns.gap = (gap - delta).max(0.0);
}

/// 表头上的一根分隔线。`width` 是这一列当前宽度，`gap` 是可借用的空白；
/// 拖动最多把该列加宽到「当前 + 空白」，不挤别的列。
/// 线常显。绝对定位的小命中区拿不到 `on_hover`，悬停高亮不可靠，改用浅色竖线标记可拖位置。
pub(super) fn handle(
    edge: ColumnEdge,
    width: f32,
    gap: f32,
    list: WeakEntity<TrackList>,
) -> AnyElement {
    let limit = width + gap;
    div()
        .id(("col-resize", edge.id()))
        .absolute()
        .top(px(0.))
        .bottom(px(0.))
        .w(px(HIT_W))
        .flex()
        .justify_center()
        .cursor(CursorStyle::ResizeLeftRight)
        .on_drag(ColumnDrag { edge }, |_: &ColumnDrag, _, _, cx| {
            cx.new(|_| NoGhost)
        })
        .on_drag_move::<ColumnDrag>(move |event, _, cx| {
            // 三根分隔线挂着同一种载荷，只有起点匹配时才响应。
            if event.drag(cx).edge != edge {
                return;
            }
            let pointer = f32::from(event.event.position.x);
            let center =
                f32::from(event.bounds.origin.x) + f32::from(event.bounds.size.width) / 2.0;
            let next = width_from_pointer(center, width, pointer).min(limit);
            list.update(cx, |list, cx| {
                list.resize_column(edge, width, next, gap, cx);
            })
            .ok();
        })
        .child(
            div()
                .flex_none()
                .w(px(2.))
                .h_full()
                .bg(theme::border_soft()),
        )
        .right(px(-HIT_W / 2.0))
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::{ColumnEdge, apply, width_from_pointer};
    use crate::settings::TrackColumns;

    #[test]
    fn each_divider_sizes_the_column_on_its_left() {
        // 把手在列的右缘。指针右移，列变宽。
        assert_eq!(width_from_pointer(100.0, 36.0, 110.0), 46.0);
        assert_eq!(width_from_pointer(620.0, 520.0, 630.0), 530.0);
        assert_eq!(width_from_pointer(840.0, 220.0, 850.0), 230.0);
    }

    #[test]
    fn the_reference_edge_survives_the_handle_moving() {
        // 标题列左缘固定。第一帧中心 620、宽 520，指针 630 → 宽 530。
        let width = width_from_pointer(620.0, 520.0, 630.0);
        // 下一帧把手随宽度右移；固定左缘仍是 100，指针 640 → 宽 540。
        let center = 620.0 + (width - 520.0);
        assert_eq!(width_from_pointer(center, width, 640.0), 540.0);
    }

    #[test]
    fn shrinking_a_column_grows_the_gap_and_leaves_the_title_alone() {
        let mut columns = TrackColumns::default();
        apply(&mut columns, ColumnEdge::Album, 300.0, 250.0, 40.0);
        assert_eq!(columns.album, 250.0);
        assert_eq!(columns.gap, 90.0, "缩掉的 50px 变成空白");
        assert_eq!(columns.index, TrackColumns::default().index);
    }

    #[test]
    fn widening_a_column_eats_the_gap() {
        let mut columns = TrackColumns::default();
        apply(&mut columns, ColumnEdge::Album, 60.0, 100.0, 50.0);
        assert_eq!(columns.album, 100.0);
        assert_eq!(columns.gap, 10.0);
    }

    #[test]
    fn the_title_divider_only_moves_the_gap() {
        let mut columns = TrackColumns::default();
        apply(&mut columns, ColumnEdge::Title, 300.0, 350.0, 80.0);
        assert_eq!(columns.gap, 30.0);
        assert_eq!(columns.index, TrackColumns::default().index);
        assert_eq!(columns.album, TrackColumns::default().album);
    }
}
