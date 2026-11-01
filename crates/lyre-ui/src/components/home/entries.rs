//! 首页「浏览」那排跳转卡，切角看板 + 顶部墨色页签 + 真实计数。
//! 不压历史栈。历史栈只记「作用域收窄」，否则返回会退到一半变成切区段。

use crate::entities::LibraryEntity;
use crate::icon::{Glyph, icon_inherit};
use crate::platform::*;
use crate::shell::{NavSection, ShellEntity};
use crate::theme;

const ENTRY_H: f32 = 96.0;

/// 一张跳转看板，点击切到对应区段。
pub(super) fn entry_card(
    id: &'static str,
    glyph: Glyph,
    title: &'static str,
    count: String,
    section: NavSection,
    shell: &Entity<ShellEntity>,
    library: &Entity<LibraryEntity>,
) -> AnyElement {
    let shell = shell.clone();
    let library = library.clone();
    let english = match section {
        NavSection::Tracks => "Tracks",
        NavSection::Albums => "Albums",
        NavSection::Artists => "Artists",
        _ => "Browse",
    };

    div()
        .id(id)
        .relative()
        .flex_none()
        .h(px(ENTRY_H))
        .flex_1()
        .min_w_0()
        .flex()
        .flex_col()
        .justify_center()
        .gap(px(2.))
        .px(px(theme::GAP_MD))
        .pr(px(theme::GAP_MD + theme::CUT))
        .bg(theme::window_base())
        .border_1()
        .border_color(theme::border())
        .cursor_pointer()
        .hover(|style| style.bg(theme::elevated()))
        .on_click(move |_, _, cx| {
            // `go_to` 本身会退出作用域并记录一条历史，顶栏 ← 因此可退回首页。
            crate::shell::go_to(section, &shell, &library, cx);
        })
        .child(crate::common::ornament::cut_br(theme::CUT, theme::bg()))
        // 顶边一小段墨色页签，悬停时铺满整条边。
        .child(
            div()
                .absolute()
                .top_0()
                .left_0()
                .h(px(5.))
                .w(px(32.))
                .bg(theme::text()),
        )
        .child(
            div()
                .flex()
                .w_full()
                .items_center()
                .justify_between()
                .child(crate::common::section::eyebrow(english))
                .child(
                    div()
                        .flex_none()
                        .text_color(theme::text_faint())
                        .child(icon_inherit(glyph, 16.)),
                ),
        )
        .child(
            div()
                .font(theme::field_font())
                .text_size(px(theme::FONT_TITLE))
                .font_weight(theme::WEIGHT_TITLE)
                .text_color(theme::text())
                .child(count),
        )
        .child(
            div()
                .text_size(px(theme::FONT_CAPTION))
                .text_color(theme::text_muted())
                .child(title),
        )
        .into_any_element()
}
