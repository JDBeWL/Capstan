//! 区级标题、眉标、成对标题与字段。

use crate::platform::*;
use crate::theme;

pub fn section_heading(text: impl Into<SharedString>) -> impl IntoElement {
    div()
        .min_w_0()
        .truncate()
        .text_size(px(theme::FONT_BODY))
        .font_weight(theme::WEIGHT_TITLE)
        .text_color(theme::text())
        .child(text.into())
}

/// `// RECORDS` 眉标，宽体、大写、次要墨色三样。
pub fn eyebrow(text: impl Into<SharedString>) -> Div {
    let text: SharedString = text.into();
    div()
        .min_w_0()
        .truncate()
        .font(theme::label_font())
        .text_size(px(theme::FONT_EYEBROW))
        .font_weight(theme::WEIGHT_BUTTON)
        .text_color(theme::text_faint())
        .child(SharedString::from(format!("// {}", text.to_uppercase())))
}

/// 英文小字在上、中文粗体在下，给段落和看板起名。
pub fn pair_heading(english: impl Into<SharedString>, chinese: impl Into<SharedString>) -> Div {
    div()
        .min_w_0()
        .flex()
        .flex_col()
        .gap(px(2.))
        .child(eyebrow(english))
        .child(
            div()
                .min_w_0()
                .truncate()
                .text_size(px(theme::FONT_TITLE))
                .font_weight(theme::WEIGHT_TITLE)
                .text_color(theme::text())
                .child(chinese.into()),
        )
}

/// 页头大标题。黄块不再做装饰；层级交给眉标与字重。
pub fn page_title(text: impl Into<SharedString>) -> Div {
    div()
        .min_w_0()
        .truncate()
        .text_size(px(theme::FONT_HERO))
        .font_weight(theme::WEIGHT_TITLE)
        .text_color(theme::text())
        .child(text.into())
}

/// 字段。宽体等宽数字，次要墨色。日期写成 `2026.10.12`，计数带单位。
pub fn field(text: impl Into<SharedString>) -> Div {
    div()
        .flex_none()
        .font(theme::field_font())
        .text_size(px(theme::FONT_FIELD_SIZE))
        .text_color(theme::text_faint())
        .child(text.into())
}

/// 页头那行字段，后面拖一条极淡的斜线纹带，把标题和右边的控件接起来。
/// 带子占满剩余宽度，窄窗口下自行收缩，不会挤压字段。
pub fn field_band(text: impl Into<SharedString>) -> Div {
    div()
        .w_full()
        .flex()
        .items_center()
        .gap(px(theme::GAP_MD))
        .child(field(text))
        .child(
            div()
                .relative()
                .flex_1()
                .min_w_0()
                .h(px(10.))
                .overflow_hidden()
                .child(crate::common::ornament::hatch_fill(
                    theme::text().opacity(0.05),
                )),
        )
}

/// 竖线分隔的一组字段。
pub fn fields(items: impl IntoIterator<Item = impl Into<SharedString>>) -> Div {
    let mut row = div().flex().items_center().gap(px(theme::GAP_SM));
    let mut first = true;
    for item in items {
        if !first {
            row = row.child(
                div()
                    .flex_none()
                    .font(theme::field_font())
                    .text_size(px(theme::FONT_FIELD_SIZE))
                    .text_color(theme::border())
                    .child("│"),
            );
        }
        first = false;
        row = row.child(field(item));
    }
    row
}

/// 一条 1px 细线。
pub fn rule() -> impl IntoElement {
    div()
        .flex_none()
        .h(px(theme::RULE_W))
        .w_full()
        .bg(theme::border())
}
