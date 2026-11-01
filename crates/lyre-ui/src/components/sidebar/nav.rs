//! 侧栏的导航项：首页 / 曲库，以及图标态下替代整个列表的那一项。
//! 都走 [`crate::shell::go_to`]：先记一条历史再跳，顶栏的 ← 因此能退回来源页。
//! 当前项整格黄块、右下切角、墨字加粗 —— 颜色之外还有形状与字重两个通道。

use crate::common::{anim, thousands, tooltip};
use crate::entities::LibraryEntity;
use crate::icon::{Glyph, icon_inherit};
use crate::platform::*;
use crate::shell::{NavSection, ShellEntity};
use crate::theme;

pub(super) struct NavDeps {
    pub(super) shell: Entity<ShellEntity>,
    pub(super) library: Entity<LibraryEntity>,
}

/// 悬停进入导航项时的回调。曲库项用它让计数重新落定。
pub(super) type HoverEnter = Box<dyn Fn(&mut Window, &mut App)>;

/// 导航项的全部外观参数。参数多于 clippy 的阈值，打包传递。
struct NavItem {
    id: &'static str,
    glyph: Glyph,
    english: &'static str,
    label: &'static str,
    count: Option<NavCount>,
    show_label: bool,
    selected: bool,
}

struct NavCount {
    value: usize,
    epoch: u64,
}

/// 曲库项比首页多一个计数和悬停回调。
pub(super) struct LibraryItem {
    pub(super) id: &'static str,
    pub(super) label: &'static str,
    pub(super) show_label: bool,
    pub(super) selected: bool,
    pub(super) count: usize,
    pub(super) epoch: u64,
    pub(super) on_hover: Option<HoverEnter>,
}

/// 「首页」项：记一条历史再跳，并退出作用域。
pub(super) fn home_item(
    id: &'static str,
    label: &'static str,
    show_label: bool,
    selected: bool,
    deps: NavDeps,
) -> AnyElement {
    let NavDeps { shell, library, .. } = deps;

    let mut item = nav_item_shell(
        NavItem {
            id,
            glyph: Glyph::Home,
            english: "Home",
            label,
            count: None,
            show_label,
            selected,
        },
        None,
    )
    .on_click(move |_, _, cx| crate::shell::go_to(NavSection::Home, &shell, &library, cx));

    if !show_label {
        item = item.tooltip(tooltip::builder(label));
    }
    item.into_any_element()
}

/// 「曲库」项：回到整个曲库（全部曲目），切到 `Tracks`。右侧计数是真实曲目数。
pub(super) fn library_item(item: LibraryItem, deps: NavDeps) -> AnyElement {
    let NavDeps { shell, library, .. } = deps;
    let LibraryItem {
        id,
        label,
        show_label,
        selected,
        count,
        epoch,
        on_hover,
    } = item;

    let mut item = nav_item_shell(
        NavItem {
            id,
            glyph: Glyph::Folder,
            english: "Library",
            label,
            count: Some(NavCount {
                value: count,
                epoch,
            }),
            show_label,
            selected,
        },
        on_hover,
    )
    .on_click(move |_, _, cx| crate::shell::go_to(NavSection::Tracks, &shell, &library, cx));

    if !show_label {
        item = item.tooltip(tooltip::builder(format!(
            "{label} │ {} 首",
            thousands(count)
        )));
    }
    item.into_any_element()
}

/// 「曲库」是否选中：在看曲库的切面，且没有停在某个具体条目里。
pub(super) fn library_item_selected(nav: NavSection, scoped: bool) -> bool {
    nav.in_library() && !scoped
}

/// 展开态滑动块的目标：0 首页、1 曲库；搜索页不属于任何一项。
pub(super) fn active_index(section: NavSection, scoped: bool) -> Option<usize> {
    if section == NavSection::Home {
        Some(0)
    } else if library_item_selected(section, scoped) {
        Some(1)
    } else {
        None
    }
}

/// 图标态与展开态同一套判据：主区在曲库切面（含某个条目）就点亮库图标。
pub(super) fn icon_index(section: NavSection) -> Option<usize> {
    if section == NavSection::Home {
        Some(0)
    } else if section.in_library() {
        Some(1)
    } else {
        None
    }
}

/// 导航项共用的外壳，不绑行为：点击由调用方接。
/// 选中底色由外层的滑动块负责，行只改文字色 —— 否则整行底色会盖住滑过来的黄块。
fn nav_item_shell(item: NavItem, on_hover: Option<HoverEnter>) -> Stateful<Div> {
    let NavItem {
        id,
        glyph,
        english,
        label,
        count,
        show_label,
        selected,
    } = item;

    let mut item = div()
        .id(id)
        .relative()
        .flex_none()
        .h(px(theme::SIDEBAR_ITEM_H))
        .flex()
        .items_center()
        .cursor_pointer();

    item = if show_label {
        item.gap(px(theme::GAP_MD))
            .px(px(theme::GAP_MD))
            .pr(px(theme::GAP_MD + theme::CUT_SM))
    } else {
        // 与 lib_fallback_item 同尺寸同居中：收起侧栏后上下两个图标才落回同一条竖线
        item.w(px(theme::SIDEBAR_ITEM_H)).justify_center()
    };

    let mut item = item
        // 选中行整行压在黄块上：两套主题都用深墨色，深色主题里不能跟着 text() 变浅。
        .when(selected, |item| item.text_color(theme::text_on_accent()))
        .when(!selected, |item| item.text_color(theme::text_muted()))
        .hover(|style| {
            // 当前项的黄块由外层画，悬停不再铺一层底，免得盖住正在滑动的块
            if selected {
                style.text_color(theme::text_on_accent())
            } else {
                style.bg(theme::elevated()).text_color(theme::text())
            }
        })
        .child(
            div()
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .w(px(20.))
                .child(icon_inherit(glyph, 18.)),
        );

    if show_label {
        item = item.child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap(px(1.))
                .child(
                    crate::common::section::eyebrow(english).text_color(if selected {
                        theme::text_on_accent()
                    } else {
                        theme::text_faint()
                    }),
                )
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .text_size(px(theme::FONT_BODY))
                        .font_weight(theme::WEIGHT_BUTTON)
                        .child(label),
                ),
        );
        if let Some(NavCount { value, epoch }) = count {
            let counter = div()
                .flex_none()
                .font(theme::field_font())
                .text_size(px(theme::FONT_EYEBROW))
                .text_color(if selected {
                    theme::text_on_accent()
                } else {
                    theme::text_faint()
                });
            // 打开时用 600ms，之后每次悬停进入换一个 id 重播 360ms；
            // 移出悬停不改 id，计数保持落定后的样子。
            let duration = if epoch == 0 {
                anim::MOTION_SCRAMBLE_MS
            } else {
                anim::MOTION_SCRAMBLE_HOVER_MS
            };
            item = item.child(anim::scramble_content(
                SharedString::from(format!("side-count-{value}-{epoch}")),
                0,
                duration,
                counter,
                move |progress| anim::scramble(&thousands(value), progress),
            ));
        }
    }

    if let Some(on_hover) = on_hover {
        item = item.on_hover(move |hovered, window, cx| {
            if *hovered {
                on_hover(window, cx);
            }
        });
    }

    item
}

/// 图标态下替代整个列表的那一项。只要主区停在曲库切面（含某个条目）就点亮，
/// 与展开态的「曲库」项一致；搜索页不属于曲库，不亮。
pub(super) fn lib_fallback_item(selected: bool, deps: NavDeps) -> AnyElement {
    let NavDeps { shell, library, .. } = deps;

    div()
        .id("side-lib-fallback")
        .relative()
        .flex_none()
        .size(px(theme::SIDEBAR_ITEM_H))
        .flex()
        .items_center()
        .justify_center()
        .cursor_pointer()
        .when(selected, |item| item.text_color(theme::text_on_accent()))
        .when(!selected, |item| item.text_color(theme::text_muted()))
        .hover(|style| {
            if selected {
                style.text_color(theme::text_on_accent())
            } else {
                style.bg(theme::elevated()).text_color(theme::text())
            }
        })
        .on_click(move |_, _, cx| crate::shell::go_to(NavSection::Tracks, &shell, &library, cx))
        .child(icon_inherit(Glyph::Folder, 18.))
        .tooltip(tooltip::builder("曲库"))
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::{active_index, icon_index};
    use crate::shell::NavSection;

    #[test]
    fn slide_block_only_lands_on_a_current_item() {
        assert_eq!(active_index(NavSection::Home, false), Some(0));
        assert_eq!(active_index(NavSection::Tracks, false), Some(1));
        assert_eq!(active_index(NavSection::Albums, false), Some(1));
        assert_eq!(active_index(NavSection::Tracks, true), None);
        assert_eq!(active_index(NavSection::Search, false), None);

        assert_eq!(icon_index(NavSection::Home), Some(0));
        assert_eq!(icon_index(NavSection::Tracks), Some(1));
        assert_eq!(icon_index(NavSection::Artists), Some(1));
        assert_eq!(icon_index(NavSection::Search), None);
    }
}
