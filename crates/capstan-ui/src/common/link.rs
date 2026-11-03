//! 可点的文字。名字与周围文字同色，指到才变实色并加下划线：
//! 标题、名字一多，常态就给它们上色会让整页像一张链接列表。

use crate::platform::*;
use crate::theme;

/// 一行里的一个文字链。`id` 必须逐处唯一，同 id 的两个元素会共用命中区域与悬停态。
///
/// 只在父级是 flex 容器时收缩。父级是块级时它自然铺满整行，省略号照样出得来。
pub fn text_link(
    id: impl Into<ElementId>,
    label: String,
    tooltip: String,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> AnyElement {
    div()
        .id(id.into())
        .min_w_0()
        .flex_shrink(1.0)
        .truncate()
        .cursor_pointer()
        .hover(|style| style.text_color(theme::text()).underline())
        .tooltip(crate::common::tooltip::builder(tooltip))
        .on_click(on_click)
        .child(label)
        .into_any_element()
}
