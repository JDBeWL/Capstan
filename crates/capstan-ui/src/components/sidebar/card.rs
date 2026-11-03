//! 侧边栏的两张卡片与卡片内部的小件；图标态下整栏由 [`Sidebar::render_icons`] 画。

use std::rc::Rc;

use crate::common::{anim, thousands, tooltip};
use crate::entities::LibraryEntity;
use crate::icon::{Glyph, icon_inherit};
use crate::platform::*;
use crate::shell::{NavSection, SidebarState};
use crate::theme;

use super::Sidebar;
use super::entries::{Listing, SideEntry, lib_row};
use super::nav::{
    LibraryItem, NavDeps, active_index, home_item, icon_index, lib_fallback_item, library_item,
    library_item_selected,
};

const CARD_PAD: f32 = 8.0;
/// 下卡片虚拟列表最多渲染的行数。
const LIB_MAX_ROWS: usize = 1000;

impl Sidebar {
    /// 顶部。字标加首页、曲库两项，定高不参与滚动。
    pub(super) fn home_card(
        &self,
        state: SidebarState,
        counts: (usize, usize, usize),
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let labels = state.shows_labels();
        let section = self.shell.read(cx).nav_section();
        let scoped = self.library.read(cx).scope().is_scoped();
        let mut card = div()
            .flex_none()
            .flex()
            .flex_col()
            .bg(theme::window_base())
            .px(px(CARD_PAD))
            .pb(px(CARD_PAD));

        if labels {
            card = card.child(brand_row());
        }

        let deps = || NavDeps {
            shell: self.shell.clone(),
            library: self.library.clone(),
        };

        // 悬停曲库行时把 epoch 加一。计数换 id 重播一次乱码，移出不重播。
        let bump_epoch = {
            let sidebar = cx.entity().downgrade();
            move |_window: &mut Window, cx: &mut App| {
                sidebar
                    .update(cx, |sidebar, cx| {
                        sidebar.count_epoch = sidebar.count_epoch.wrapping_add(1);
                        cx.notify();
                    })
                    .ok();
            }
        };

        let mut rail = div().flex_none().relative().flex().flex_col();
        if let Some(index) = active_index(section, scoped) {
            let slide = anim::nav_slide(SharedString::from("side-nav"), index, window, cx);
            rail = rail.child(
                div()
                    .absolute()
                    .left_0()
                    .top(px(slide * theme::SIDEBAR_ITEM_H))
                    .w_full()
                    .h(px(theme::SIDEBAR_ITEM_H))
                    .bg(theme::accent())
                    .child(crate::common::ornament::cut_br(
                        theme::CUT_SM,
                        theme::window_base(),
                    )),
            );
        }

        rail = rail
            .child(home_item(
                "side-home",
                "首页",
                labels,
                section == NavSection::Home,
                deps(),
            ))
            .child(library_item(
                LibraryItem {
                    id: "side-library",
                    label: "曲库",
                    show_label: labels,
                    selected: library_item_selected(section, scoped),
                    count: counts.0,
                    epoch: self.count_epoch,
                    on_hover: Some(Box::new(bump_epoch)),
                },
                deps(),
            ));

        card.child(rail).into_any_element()
    }

    /// 「你的音乐库」卡片，标题行 + Chip 筛选 + 专辑与艺术家列表。
    pub(super) fn library_card(
        &self,
        state: SidebarState,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let labels = state.shows_labels();
        let nav = self.shell.read(cx).nav_section();
        let effective = self.effective_listing(nav);

        let entries = self.entries(nav, cx);

        let header = lib_header(labels, &self.library);

        if !labels {
            // 图标态放不下带缩略图的行，退回到单个「库」图标。
            return card_shell(false)
                .child(header)
                .child(lib_fallback_item(
                    nav.in_library(),
                    NavDeps {
                        shell: self.shell.clone(),
                        library: self.library.clone(),
                    },
                ))
                .into_any_element();
        }

        let chips = Self::chips_row(effective, state.width(), window, cx);

        if entries.is_empty() {
            return card_shell(true)
                .child(header)
                .child(chips)
                .child(empty_hint(self.library.clone()))
                .into_any_element();
        }

        // 防御性截断。`item_count` 过大时 `uniform_list` 的总高溢出成负数，列表一进来就滚到底。
        let rows: Rc<Vec<SideEntry>> = Rc::new(entries);
        let row_count = rows.len().min(LIB_MAX_ROWS);

        // `min_h_0` 不能省。少了它 `uniform_list` 的最小高度会被内容撑到全高，卡片滚不动。
        let list = uniform_list("side-library-list", row_count, {
            let rows = Rc::clone(&rows);
            let shell = self.shell.clone();
            let library = self.library.clone();
            let covers = self.covers.clone();
            move |range, window, cx| {
                range
                    .map(|ix| {
                        lib_row(
                            rows[ix].clone(),
                            shell.clone(),
                            library.clone(),
                            &covers,
                            window,
                            cx,
                        )
                    })
                    .collect()
            }
        })
        .flex_1()
        .min_h_0()
        .into_any_element();

        card_shell(true)
            .child(header)
            .child(chips)
            .child(list)
            .into_any_element()
    }

    /// 「你的音乐库」标题下的切角 Chip 行，用于手动切换下卡片列歌单、艺术家还是专辑。
    fn chips_row(
        effective: Listing,
        width: f32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let gap = theme::GAP_XS;
        let chip_w = (width - 2.0 * CARD_PAD - 2.0 * gap) / 3.0;
        let active = match effective {
            Listing::Collections => 0,
            Listing::Artists => 1,
            Listing::Albums => 2,
        };
        let slide = anim::segment_slide(SharedString::from("side-lib-chips"), active, window, cx);

        let mut row = div()
            .id("side-lib-chips")
            .relative()
            .flex_none()
            .flex()
            .gap(px(gap))
            .pb(px(theme::GAP_SM));

        // 灰底铺在后面，黄块在它们上面滑动，文字最后画。
        for index in 0..3 {
            row = row.child(
                div()
                    .absolute()
                    .left(px(index as f32 * (chip_w + gap)))
                    .top_0()
                    .w(px(chip_w))
                    .h(px(28.))
                    .bg(theme::elevated())
                    .child(crate::common::ornament::cut_br(
                        theme::CUT_SM,
                        theme::window_base(),
                    )),
            );
        }
        row = row.child(
            div()
                .absolute()
                .left(px(slide * (chip_w + gap)))
                .top_0()
                .w(px(chip_w))
                .h(px(28.))
                .bg(theme::accent())
                .child(crate::common::ornament::cut_br(
                    theme::CUT_SM,
                    theme::window_base(),
                )),
        );

        let mut labels = div().flex_none().flex().gap(px(gap)).h(px(28.));
        for (index, (id, label, listing)) in [
            ("side-chip-collections", "歌单", Listing::Collections),
            ("side-chip-artists", "艺术家", Listing::Artists),
            ("side-chip-albums", "专辑", Listing::Albums),
        ]
        .into_iter()
        .enumerate()
        {
            // 字色跟着黄块走，滑动中间不会有一半时间字压在对比不足的底色上。
            let under = (slide - index as f32).abs() < 0.5;
            labels = labels.child(
                div()
                    .id(id)
                    .w(px(chip_w))
                    .h_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_size(px(theme::FONT_CAPTION))
                    .font_weight(theme::WEIGHT_BUTTON)
                    .cursor_pointer()
                    .text_color(if under {
                        theme::text_on_accent()
                    } else {
                        theme::text_muted()
                    })
                    .when(!under, |chip| {
                        chip.hover(|style| style.text_color(theme::text()))
                    })
                    .on_click(cx.listener(move |this, _, _, cx| this.set_listing(listing, cx)))
                    .child(label),
            );
        }

        row.child(labels).into_any_element()
    }

    /// 图标态（56px）的整栏渲染。
    pub(super) fn render_icons(
        &self,
        state: SidebarState,
        counts: (usize, usize, usize),
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let section = self.shell.read(cx).nav_section();

        let mut card = div()
            .flex_none()
            .w(px(state.width()))
            .h_full()
            .flex()
            .flex_col()
            .bg(theme::window_base())
            .border_r_1()
            .border_color(theme::border())
            // 图标态没有字标行兜底，上留白要和左右一致，首页按钮才不会贴住顶栏。
            .p(px(CARD_PAD));

        let deps = || NavDeps {
            shell: self.shell.clone(),
            library: self.library.clone(),
        };

        // 两块图标之间隔一条细线。细线画在黄块之前（下层），中间用等高空档占位：
        // 黄块从上面滑过时不会把线割成两截。
        let step = theme::SIDEBAR_ITEM_H + theme::GAP_XS * 2.0 + 1.0;
        let mut rail = div()
            .flex_none()
            .relative()
            .flex()
            .flex_col()
            .w(px(theme::SIDEBAR_ITEM_H));

        rail = rail.child(
            div()
                .absolute()
                .left(px(theme::GAP_SM))
                .top(px(theme::SIDEBAR_ITEM_H + theme::GAP_XS))
                .w(px(theme::SIDEBAR_ITEM_H - theme::GAP_SM * 2.0))
                .h(px(1.))
                .bg(theme::border_soft()),
        );

        if let Some(index) = icon_index(section) {
            let slide = anim::nav_slide(SharedString::from("side-nav-icons"), index, window, cx);
            rail = rail.child(
                div()
                    .absolute()
                    .left_0()
                    .top(px(slide * step))
                    .size(px(theme::SIDEBAR_ITEM_H))
                    .bg(theme::accent())
                    .child(crate::common::ornament::cut_br(
                        theme::CUT_SM,
                        theme::window_base(),
                    )),
            );
        }

        rail = rail
            .child(home_item(
                "side-home-icon",
                "首页",
                false,
                section == NavSection::Home,
                deps(),
            ))
            .child(div().flex_none().h(px(step - theme::SIDEBAR_ITEM_H)))
            .child(lib_fallback_item(section.in_library(), deps()));
        card = card.child(rail);

        let (tracks, albums, artists) = counts;
        let label = format!(
            "音乐库 │ {} 首 / {} 张专辑 / {} 位艺术家",
            thousands(tracks),
            thousands(albums),
            thousands(artists)
        );
        card.id("side-icons")
            .tooltip(tooltip::builder(label))
            .into_any_element()
    }
}

/// 下卡片外壳。`fills` = true 时占满剩余高度（展开态列表要滚动），false 时按内容定高（图标态）。
/// 左右内边距与导航卡片相同。标题、Chip 与列表块共用一条边线。
fn card_shell(fills: bool) -> Div {
    let card = div()
        .flex()
        .flex_col()
        .min_h_0()
        .bg(theme::window_base())
        .px(px(CARD_PAD));

    if fills {
        card.flex_1()
    } else {
        card.flex_none()
    }
}

/// 品牌 C。GPUI 只取 SVG 的 alpha 蒙版，填色由 `text_color` 决定；
/// 内径收到 46，让 15px 下的环宽对齐 21px 字标。
const BRAND_C_SVG: &str = r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="-1 -1 178 178"><path d="M162.628 134.633A88 88 0 1 1 162.628 41.367L127.010 63.624A46 46 0 1 0 127.010 112.376Z"/></svg>"#;
const BRAND_C_SIZE: f32 = 15.0;
const BRAND_DOT: f32 = 4.0;

fn brand_mark() -> impl IntoElement {
    // 点比原几何外移 2 个单位，落在开口中央，不蹭 C 的下臂。
    let dot_center = BRAND_C_SIZE * 169.0 / 178.0;
    let dot_offset = dot_center - BRAND_DOT / 2.0 - BRAND_C_SIZE;

    div()
        .flex_none()
        .flex()
        .items_center()
        .child(
            Icon::default()
                .data(BRAND_C_SVG.as_bytes())
                .with_size(px(BRAND_C_SIZE))
                .text_color(theme::text()),
        )
        .child(
            div()
                .flex_none()
                .ml(px(dot_offset))
                .size(px(BRAND_DOT))
                .rounded_full()
                .bg(theme::accent()),
        )
}

/// 侧栏顶部的字标。C 作首字母，黄点留在开口里，颜色随主题走，不开新卡片。
fn brand_row() -> AnyElement {
    div()
        .flex_none()
        .h(px(56.))
        .flex()
        .items_center()
        .gap(px(0.))
        .px(px(4.))
        .child(brand_mark())
        .child(
            div()
                .font(theme::label_font())
                .text_size(px(21.))
                .font_weight(FontWeight::EXTRA_BOLD)
                .text_color(theme::text())
                .child("APSTAN"),
        )
        .into_any_element()
}

/// 下卡片标题行。`// LIBRARY` + 中文粗体 + 导入按钮，这一行本身不参与选中。
fn lib_header(labels: bool, library: &Entity<LibraryEntity>) -> AnyElement {
    let mut row = div()
        .flex_none()
        .h(px(48.))
        .flex()
        .items_center()
        .gap(px(theme::GAP_SM));

    if labels {
        row = row.child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap(px(2.))
                .child(crate::common::section::eyebrow("Your Library"))
                .child(crate::common::section::section_heading("你的音乐库")),
        );

        row = row.child(
            div()
                .flex_none()
                .flex()
                .items_center()
                .gap(px(theme::GAP_MD))
                .child(header_action("side-lib-add", Glyph::Plus, "导入音乐", {
                    let library = library.clone();
                    move |_, _, cx| library.update(cx, LibraryEntity::open_dialog)
                })),
        );
    }

    row.into_any_element()
}

/// 标题行右侧的小切角按钮。
fn header_action(
    id: &'static str,
    glyph: Glyph,
    label: &'static str,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> AnyElement {
    div()
        .id(id)
        .relative()
        .flex_none()
        .size(px(24.))
        .flex()
        .items_center()
        .justify_center()
        .cursor_pointer()
        .text_color(theme::text_muted())
        .hover(|style| style.bg(theme::elevated()).text_color(theme::text()))
        .on_click(on_click)
        .child(icon_inherit(glyph, 16.))
        .tooltip(tooltip::builder(label))
        .into_any_element()
}

/// 空曲库提示，一句说明 + 一个可点的导入入口。
fn empty_hint(library: Entity<LibraryEntity>) -> AnyElement {
    div()
        .id("side-lib-empty")
        .flex_col()
        .gap(px(theme::GAP_SM))
        .px(px(theme::GAP_SM))
        .py(px(theme::GAP_MD))
        .child(
            div()
                .text_size(px(theme::FONT_CAPTION))
                .text_color(theme::text_faint())
                .child("还没有音乐"),
        )
        .child(
            div()
                .id("side-lib-empty-import")
                .cursor_pointer()
                // 悬停给一块 chip 底。黄只做填充：浅色下黄字压在纸色上只有 1.05:1，
                // 鼠标一放上去字就没了。
                .mx(px(-4.))
                .px(px(4.))
                .text_size(px(theme::FONT_CAPTION))
                .font_weight(theme::WEIGHT_BUTTON)
                .text_color(theme::text())
                .hover(|style| style.bg(theme::elevated()))
                .on_click(move |_, _, cx| library.update(cx, LibraryEntity::open_dialog))
                .child("导入音乐"),
        )
        .into_any_element()
}
