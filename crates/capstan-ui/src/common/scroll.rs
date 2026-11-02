//! 为滚动容器叠加一条滚动条。gpui 本身不绘制滚动条，列表仅维护偏移量。

use crate::platform::*;

/// 叠在滚动容器之上的一条竖向滚动条。必须先用 `track_scroll` 把 `handle` 挂到容器上，
/// 否则读到的偏移量永远是零。泛型。虚拟列表用 `UniformListScrollHandle`，普通滚动区用 `ScrollHandle`。
pub fn scrollbar<H: ScrollbarHandle + Clone>(handle: &H, id: &'static str) -> AnyElement {
    div()
        .absolute()
        .inset_0()
        .child(
            Scrollbar::vertical(handle)
                .id(id)
                // 该节点是滚动区的兄弟节点；若不自声明按自身布局取视口，则会依 handle 的边界计算，
                // 与外层 `inset_0` 存在一处偏差，两者都属于不会报错的错误。
                .viewport_from_layout()
                // 默认的 `Scrolling` 模式停手就淡出，淡出期间点不中。
                .mode(ScrollbarMode::Always),
        )
        .into_any_element()
}
