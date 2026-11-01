//! 页头工具条，含大标题、扫描指示、排序、清空与多选开关。

use lyre_core::SortKey;

use crate::common::button::{ButtonKind, text_button_animated};
use crate::common::thousands;
use crate::entities::{LibraryEntity, ScanProgress, ScopeKind};
use crate::icon::{Glyph, icon_inherit};
use crate::platform::*;
use crate::theme;

use super::{TrackList, TrackListEvent};

/// 工具条渲染需要的全部数值，一次从曲库读出。
#[derive(Clone)]
#[allow(clippy::struct_excessive_bools)]
pub(super) struct ToolbarState {
    /// 可见行数（过滤生效时小于 `total`）
    pub(super) count: usize,
    /// 曲库总数。
    pub(super) total: usize,
    /// 用户真的输入了搜索词，而不是「有没有投影」，见 [`LibraryEntity::has_query`]。
    pub(super) filtering: bool,
    pub(super) sort_key: SortKey,
    pub(super) scanning: Option<ScanProgress>,
    /// 是否处于专辑与艺术家详情里，决定要不要给浮动返回按钮让出左内边距。
    pub(super) scoped: bool,
    /// 作用域种类，决定眉标写 `// ALBUM` 还是 `// ARTIST`。
    pub(super) kind: ScopeKind,
    /// 当前作用域是歌单与收藏（`ScopeKind::All` 分不出这两者）
    pub(super) from_list: bool,
    /// 详情页的标题（专辑名与艺术家名）。`None` 表示没有收窄作用域。
    pub(super) scope: Option<String>,
    /// 上方已有页头（详情页头图与搜索页大标题）时不再画大标题；列头照常画。
    pub(super) headed_elsewhere: bool,
    pub(super) in_search: bool,
    pub(super) selecting: bool,
}

pub(super) fn can_reorder(in_collection: bool, has_query: bool, rows: usize) -> bool {
    in_collection && !has_query && rows > 1
}

/// 眉标，当前页是什么。搜索词优先。
fn header_eyebrow(filtering: bool, kind: ScopeKind, from_list: bool) -> &'static str {
    if filtering {
        return "Search";
    }
    match kind {
        ScopeKind::Album => "Album",
        ScopeKind::Artist => "Artist",
        ScopeKind::All if from_list => "Playlist",
        ScopeKind::All => "Library",
    }
}

/// 中文标题。搜索优先，其次作用域名，最后是「曲库」。
fn header_title(filtering: bool, scope: Option<&str>) -> String {
    if filtering {
        "搜索结果".to_owned()
    } else if let Some(name) = scope.filter(|name| !name.trim().is_empty()) {
        name.to_owned()
    } else {
        "曲库".to_owned()
    }
}

/// 标题下的一行真实字段，计数与命中比。
fn header_meta(filtering: bool, count: usize, total: usize) -> String {
    if filtering {
        format!("找到 {} / {} 首", thousands(count), thousands(total))
    } else {
        format!("{} 首", thousands(total))
    }
}

/// 「排序」与「清空」只在曲库列表页显示，详情页与搜索页都不画。
/// 判据是「停在搜索页」而不是「有搜索词」，曲库页用顶栏过滤时按钮留在原地。
fn shows_actions(scoped: bool, in_search: bool) -> bool {
    !scoped && !in_search
}

pub(super) fn toolbar(
    library: Entity<LibraryEntity>,
    list: WeakEntity<TrackList>,
    state: ToolbarState,
    window: &mut Window,
    cx: &mut App,
) -> impl IntoElement {
    let ToolbarState {
        count,
        total,
        filtering,
        sort_key,
        scanning,
        scoped,
        kind,
        from_list,
        scope,
        headed_elsewhere,
        in_search,
        selecting,
    } = state;

    // 进详情时让出左内边距，免得大标题被浮动返回按钮压住；与按钮位置同源于 `crate::root::HEADER_BACK_INSET`。
    let back_inset = if scoped {
        crate::root::HEADER_BACK_INSET
    } else {
        theme::GAP_XL
    };

    // 详情里的上留白再叠一档返回按钮的高度，让大标题落在按钮下方。
    let top_pad = if scoped {
        crate::root::HEADER_BACK_INSET + theme::GAP_XL
    } else {
        theme::GAP_XL
    };

    let title = header_title(filtering, scope.as_deref());
    let meta = header_meta(filtering, count, total);
    let eyebrow = header_eyebrow(filtering, kind, from_list);

    let mut bar = div()
        .flex_none()
        .flex()
        .items_end()
        .gap(px(theme::GAP_MD))
        .pt(px(if headed_elsewhere { 0.0 } else { top_pad }))
        .pb(px(theme::GAP_MD))
        .pl(px(back_inset))
        .pr(px(theme::GAP_XL))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .items_start()
                .gap(px(6.))
                // 扫描指示与页头归属无关，`headed_elsewhere` 时仍要画。
                .when(!headed_elsewhere, |col| {
                    col.child(crate::common::section::eyebrow(eyebrow))
                        .child(crate::common::section::page_title(title).flex_shrink_1())
                        .child(crate::common::section::field_band(meta))
                })
                .children(scanning.map(scan_indicator)),
        );

    if shows_actions(scoped, in_search) {
        bar = bar.child(sort_menu(library.clone(), total, sort_key, window, cx));

        if total > 0 {
            let list = list.clone();
            bar = bar.child(text_button_animated(
                "header-clear",
                "清空",
                ButtonKind::Secondary,
                window,
                cx,
                move |_, _, cx| {
                    let previous = library.update(cx, LibraryEntity::clear);
                    list.update(cx, |_, cx| cx.emit(TrackListEvent::Cleared(previous)))
                        .ok();
                },
            ));
        }
    }

    // 多选开关只留在曲库页工具条里。详情页走头图的「更多操作」，搜索页不提供多选。
    if !headed_elsewhere && count > 0 {
        bar = bar.child(selection_toggle(list, selecting, window, cx));
    }

    bar
}

/// 多选开关，只出现在曲库页工具条里。
pub(super) fn selection_toggle(
    list: WeakEntity<TrackList>,
    selecting: bool,
    window: &mut Window,
    cx: &mut App,
) -> impl IntoElement {
    text_button_animated(
        "header-select",
        if selecting { "退出多选" } else { "多选" },
        if selecting {
            ButtonKind::Secondary
        } else {
            ButtonKind::Ghost
        },
        window,
        cx,
        move |_, _, cx| {
            list.update(cx, TrackList::toggle_selection_mode).ok();
        },
    )
}

/// 扫描进度指示。不确定进度（只报已找到数），因为扫描前不知道总文件数，百分比只能是假的。
/// 黄块 + 墨字 = 需要注意，不用红绿。
fn scan_indicator(progress: ScanProgress) -> impl IntoElement {
    div()
        .flex_none()
        .flex()
        .items_center()
        .gap(px(theme::GAP_SM))
        .child(div().flex_none().size(px(8.)).bg(theme::accent()))
        .child(
            div()
                .flex_none()
                .text_size(px(theme::FONT_CAPTION))
                .font_weight(theme::WEIGHT_BUTTON)
                .text_color(theme::text())
                .child("正在扫描"),
        )
        .child(crate::common::section::field(format!(
            "已找到 {}",
            thousands(progress.found)
        )))
}

/// 排序入口。不用 `context_menu`。它只在右键打开，左键点下去毫无反应。
fn sort_menu(
    library: Entity<LibraryEntity>,
    total: usize,
    current: SortKey,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    if total == 0 {
        return div().into_any_element();
    }

    let trigger = crate::common::button::button_shell_animated(
        "sort-menu",
        crate::common::button::ButtonKind::Outline,
        window,
        cx,
    )
    .tooltip(crate::common::tooltip::builder("排序方式"))
    .child(icon_inherit(Glyph::Sort, 14.))
    .child(current.label());

    // 菜单项在闭包里现造。`PopupMenuItem` 不是 `Clone`（内含 `Box<dyn Fn>`），而 builder 会被反复调用。
    crate::common::menu::dropdown(trigger, Anchor::TopRight, move |menu, _window, _cx| {
        let library = library.clone();
        let mut menu = menu;
        for key in SortKey::ALL {
            let library = library.clone();
            let checked = if key == current { "✓ " } else { "" };
            menu = menu.item(
                PopupMenuItem::new(format!("{checked}{}", key.label())).on_click(
                    move |_, _, cx| {
                        library.update(cx, |library, cx| library.sort_by(key, cx));
                    },
                ),
            );
        }
        menu
    })
}

#[cfg(test)]
mod tests {
    use super::{can_reorder, header_eyebrow, header_meta, header_title, shows_actions};
    use crate::entities::ScopeKind;

    #[test]
    fn reorder_needs_a_playlist_a_still_list_and_something_to_move() {
        assert!(can_reorder(true, false, 2));
        assert!(!can_reorder(true, false, 1), "一行没什么可排的");
        assert!(!can_reorder(true, true, 20), "搜索筛出来的行不该拖");
        assert!(
            !can_reorder(false, false, 20),
            "曲库 / 专辑 / 艺术家的位置不由用户定"
        );
    }

    #[test]
    fn actions_only_belong_to_the_library_page() {
        assert!(!shows_actions(true, false), "详情页不该有排序 / 清空");
        assert!(!shows_actions(false, true), "搜索页不该有排序 / 清空");
        assert!(shows_actions(false, false), "曲库列表页要有排序 / 清空");
        assert!(!shows_actions(true, true), "详情页 + 搜索页也不该有");
    }

    #[test]
    fn detail_page_titles_the_thing_you_entered() {
        assert_eq!(
            header_title(false, Some("Chill Studio")),
            "Chill Studio",
            "详情页的大标题就是专辑 / 艺术家的名字，不是搜索结果文案"
        );
        assert_eq!(header_eyebrow(false, ScopeKind::Album, false), "Album");
        assert_eq!(header_eyebrow(false, ScopeKind::Artist, false), "Artist");
        assert_eq!(header_eyebrow(false, ScopeKind::All, true), "Playlist");
    }

    #[test]
    fn plain_library_titles_the_whole_collection() {
        assert_eq!(header_title(false, None), "曲库");
        assert_eq!(header_meta(false, 24, 24), "24 首");
        assert_eq!(header_eyebrow(false, ScopeKind::All, false), "Library");
    }

    #[test]
    fn searching_wins_over_the_scope_it_happens_inside() {
        assert_eq!(header_title(true, Some("Chill Studio")), "搜索结果");
        assert_eq!(header_meta(true, 1, 24), "找到 1 / 24 首");
        assert_eq!(header_eyebrow(true, ScopeKind::Album, true), "Search");
    }

    #[test]
    fn summary_never_renders_an_empty_title() {
        for filtering in [true, false] {
            for scope in [None, Some(""), Some("某个名字")] {
                for (count, total) in [(0usize, 0usize), (0, 24), (3, 24), (9, 9)] {
                    assert!(
                        !header_title(filtering, scope).trim().is_empty(),
                        "filtering={filtering} scope={scope:?} 时标题成了空串"
                    );
                    assert!(
                        !header_meta(filtering, count, total).trim().is_empty(),
                        "filtering={filtering} count={count} total={total} 时字段成了空串"
                    );
                }
            }
        }
    }
}
