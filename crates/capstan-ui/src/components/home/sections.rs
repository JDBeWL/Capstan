//! 横向滚动区块，以及区块里的三种卡（专辑海报卡、歌单卡、最近播放卡）。
//! `overflow_x_scroll` 的偏移存在帧状态里没有 getter，箭头滚动只能走 [`ScrollHandle`]。
//! 不挂 `.track_scroll()` 时 `bounds_for_item` 恒为 `None`，步长就没法算。

use capstan_core::TrackId;

use crate::common::button::IconButton;
use crate::components::detail::HeroInfo;
use crate::cover::CoverCache;
use crate::entities::{LibraryEntity, Scope};
use crate::icon::Glyph;
use crate::platform::*;
use crate::shell::{NavSection, ShellEntity};
use crate::theme;

use super::HomeView;
use super::cards::RecentCard;

const RECENT_PAD: f32 = theme::GAP_SM;
/// 两行文字的预留高度。字号 15 / 12 的常规行高合计约 38px，留 44 才不会被下一节压住。
const RECENT_TEXT_H: f32 = 44.0;
/// 横向滚动区块里每张海报的封面边长（整卡宽 = 它 + 左右内边距）。
const POSTER_COVER: f32 = 160.0;
const POSTER_PAD: f32 = theme::GAP_MD;

impl HomeView {
    /// 取（必要时新建）某个横向区块的滚动句柄；每帧新建一个等于把偏移归零。
    fn row_handle(&self, id: &'static str) -> ScrollHandle {
        self.rows.borrow_mut().entry(id).or_default().clone()
    }

    /// 箭头一格等于卡片宽度加卡间距，依据实际边界而非断点推算。
    /// 无需自行夹取边界，`overflow` 那一轮会将偏移夹回 `[-max, 0]` 并写回同一个 `RefCell`。
    fn nudge_row(handle: &ScrollHandle, forward: bool, cx: &mut Context<Self>) {
        let step = handle
            .bounds_for_item(0)
            .map_or(200.0, |bounds| f32::from(bounds.size.width) + theme::GAP_MD);
        let current = f32::from(handle.offset().x);
        let next = if forward {
            current - step
        } else {
            current + step
        };
        handle.set_offset(point(px(next), px(0.)));
        cx.notify();
    }
}

/// 「正好到头」的容差。`max_offset` 按 2 位小数取整，加上亚像素滚动，差零点几像素是常态；
/// 不留容差的话，差 0.3px 就判成「没到头」，右箭头将始终无法置灰。
const EDGE_EPS: f32 = 0.5;

/// 一排的两端状态，箭头据此显隐。首帧（还没布局过）两个方向都算能滚。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) struct RowEdges {
    at_start: bool,
    at_end: bool,
}

impl RowEdges {
    /// 由句柄读到的三个数推出来。`viewport_w <= 0` 就是「还没量过」，此时返回「两端都没到」。
    pub(super) fn of(viewport_w: f32, offset_x: f32, max_offset_x: f32) -> Self {
        if viewport_w <= 0.0 {
            return Self {
                at_start: false,
                at_end: false,
            };
        }
        Self {
            at_start: offset_x >= -EDGE_EPS,
            // 内容装得下时 `max_offset_x` 是 0，此时两个方向同时到头。
            at_end: offset_x <= -max_offset_x + EDGE_EPS,
        }
    }

    /// 这个方向还能不能滚。未测量时两个方向都算能。
    fn can_go(self, forward: bool) -> bool {
        if forward {
            !self.at_end
        } else {
            !self.at_start
        }
    }
}

pub(super) fn section_title(index: usize, english: &'static str, text: &'static str) -> Div {
    div()
        .flex_none()
        .pb(px(theme::GAP_MD))
        .border_b_1()
        .border_color(theme::border())
        .child(section_heading(index, english, text))
}

/// 区块标题。`// 01 / RECENT` 眉标在上，中文粗体在下。编号按区块在首页的出现顺序从 01 起。
fn section_heading(index: usize, english: &'static str, text: &'static str) -> impl IntoElement {
    div()
        .flex()
        .flex_col()
        .gap(px(2.))
        .child(crate::common::section::eyebrow(format!(
            "{index:02} / {english}"
        )))
        .child(
            div()
                .text_size(px(theme::FONT_TITLE))
                .font_weight(theme::WEIGHT_TITLE)
                .text_color(theme::text())
                .child(text),
        )
}

/// 一张最近播放卡片。封面 + 专辑名 + 艺术家，点击即从这首开始播。
pub(super) fn recent_card(
    index: usize,
    card: RecentCard,
    cover_size: f32,
    library: &Entity<LibraryEntity>,
    covers: &Entity<CoverCache>,
    cx: &mut App,
) -> AnyElement {
    let id = card.id;
    let target = library.clone();
    let play_library = library.clone();
    let group = SharedString::from(format!("home-recent-g-{index}"));
    let cover = cover_slot(covers, library, id, cover_size, &card.album, cx);
    let play_title = card.album.clone();
    // 点卡 = 播整张专辑，队列按专辑名定。
    let queue_album = card.album.clone();
    let subtitle = if card.artist.is_empty() {
        card.meta.clone()
    } else {
        format!("{} │ {}", card.artist, card.meta)
    };
    // 卡片文字一律截断，悬停时给出完整专辑名与艺术家。
    let tip = if card.artist.is_empty() {
        card.album.clone()
    } else {
        format!("{} │ {}", card.album, card.artist)
    };

    div()
        .id(SharedString::from(format!("home-recent-{index}")))
        .group(group.clone())
        .flex_none()
        .w(px(cover_size))
        .h(px(cover_size + RECENT_PAD * 2.0 + 8.0 + RECENT_TEXT_H))
        .flex()
        .flex_col()
        .gap(px(8.0))
        .py(px(RECENT_PAD))
        .cursor_pointer()
        .tooltip(crate::common::tooltip::builder(tip))
        .on_click(move |_, _, cx| {
            // `activate` 只发事件，装载播放器由根视图做。
            target.update(cx, |library, cx| {
                library.activate_album(&queue_album, id, cx);
            });
        })
        .child(div().relative().flex_none().w_full().child(cover).child(
            crate::components::card_play_button(
                SharedString::from(format!("home-recent-play-{index}")),
                group,
                id,
                &play_library,
                &play_title,
                NavSection::Albums,
            ),
        ))
        .child(
            div()
                .flex_none()
                .min_w_0()
                .flex()
                .flex_col()
                .child(
                    div()
                        .text_size(px(theme::FONT_BODY))
                        .font_weight(theme::WEIGHT_SUBTITLE)
                        .text_color(theme::text())
                        .truncate()
                        .child(card.album),
                )
                .child(
                    div()
                        .text_size(px(theme::FONT_CAPTION))
                        .text_color(theme::text_muted())
                        .truncate()
                        .child(subtitle),
                ),
        )
        .into_any_element()
}

fn scroll_row(
    index: usize,
    english: &'static str,
    id: &'static str,
    title: &'static str,
    cards: Vec<AnyElement>,
    home: &HomeView,
    view: Entity<HomeView>,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let handle = home.row_handle(id);
    div()
        .flex_none()
        .flex()
        .flex_col()
        .child(section_header(
            index,
            english,
            title,
            id,
            handle.clone(),
            view,
            window,
            cx,
        ))
        .child(
            div()
                .id(id)
                .flex_none()
                .flex()
                .gap(px(theme::GAP_MD))
                .pt(px(theme::GAP_MD))
                .pb(px(theme::GAP_XL))
                .track_scroll(&handle)
                .overflow_x_scroll()
                .children(cards),
        )
        .into_any_element()
}

/// 标题那一行。字在左，一对箭头在右。到头的那个方向变灰、不绑事件、也不进 Tab 序列。
fn section_header(
    index: usize,
    english: &'static str,
    title: &'static str,
    id: &'static str,
    handle: ScrollHandle,
    view: Entity<HomeView>,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let edges = RowEdges::of(
        f32::from(handle.bounds().size.width),
        f32::from(handle.offset().x),
        f32::from(handle.max_offset().x),
    );

    div()
        .flex_none()
        .flex()
        .items_end()
        .justify_between()
        .pb(px(theme::GAP_SM))
        .border_b_1()
        .border_color(theme::border())
        .child(section_heading(index, english, title))
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(theme::GAP_XS))
                .child(row_arrow(
                    id,
                    false,
                    handle.clone(),
                    view.clone(),
                    edges,
                    window,
                    cx,
                ))
                .child(row_arrow(id, true, handle, view, edges, window, cx)),
        )
        .into_any_element()
}

/// 区块标题右侧的一枚箭头。gpui 没有 `transform`，镜像 `ChevronRight` 走不通，所以用一对方向图标。
fn row_arrow(
    id: &'static str,
    forward: bool,
    handle: ScrollHandle,
    view: Entity<HomeView>,
    edges: RowEdges,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let glyph = if forward {
        Glyph::ArrowRight
    } else {
        Glyph::ArrowLeft
    };
    let enabled = edges.can_go(forward);
    let key = SharedString::from(format!(
        "{id}-arrow-{}",
        if forward { "next" } else { "prev" }
    ));

    // tooltip 同时是无障碍名称，禁用态也要说清为什么点不动。
    let label = match (enabled, forward) {
        (true, true) => "下一批",
        (true, false) => "上一批",
        (false, true) => "已经是最后一批",
        (false, false) => "已经是第一批",
    };

    IconButton::new(key, glyph)
        .box_size(26.0)
        .glyph_size(13.0)
        .label(label)
        .enabled(enabled)
        .on_click(move |_, _, cx| {
            view.update(cx, |_home, cx| HomeView::nudge_row(&handle, forward, cx));
        })
        .hover_progress(window, cx)
        .render()
        .into_any_element()
}

pub(super) fn horizontal_section(
    index: usize,
    english: &'static str,
    id: &'static str,
    title: &'static str,
    cards: Vec<RecentCard>,
    home: &HomeView,
    view: Entity<HomeView>,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let posters = cards
        .into_iter()
        .enumerate()
        .map(|(i, card)| poster_card(id, i, card, &home.library, &home.covers, cx))
        .collect();
    scroll_row(index, english, id, title, posters, home, view, window, cx)
}

/// 横向区块里的海报卡，竖向排布，封面在上、两行文字在下。
fn poster_card(
    section: &'static str,
    index: usize,
    card: RecentCard,
    library: &Entity<LibraryEntity>,
    covers: &Entity<CoverCache>,
    cx: &mut App,
) -> AnyElement {
    let id = card.id;
    let target = library.clone();
    let play_library = library.clone();
    let group = SharedString::from(format!("{section}-g-{index}"));
    let cover = cover_slot(covers, library, id, POSTER_COVER, &card.album, cx);
    let play_title = card.album.clone();
    let queue_album = card.album.clone();
    let subtitle = if card.artist.is_empty() {
        card.meta.clone()
    } else {
        format!("{} │ {}", card.artist, card.meta)
    };
    let tip = if card.artist.is_empty() {
        card.album.clone()
    } else {
        format!("{} │ {}", card.album, card.artist)
    };

    div()
        .id((section, index))
        .group(group.clone())
        .flex_none()
        .w(px(POSTER_COVER + POSTER_PAD * 2.0))
        .flex()
        .flex_col()
        .gap(px(theme::GAP_SM))
        .p(px(POSTER_PAD))
        .cursor_pointer()
        .tooltip(crate::common::tooltip::builder(tip))
        .on_click(move |_, _, cx| {
            target.update(cx, |library, cx| {
                library.activate_album(&queue_album, id, cx);
            });
        })
        .child(div().relative().flex_none().w_full().child(cover).child(
            crate::components::card_play_button(
                SharedString::from(format!("{section}-play-{index}")),
                group,
                id,
                &play_library,
                &play_title,
                NavSection::Albums,
            ),
        ))
        .child(
            div()
                .w_full()
                .min_w_0()
                .text_size(px(theme::FONT_BODY))
                .font_weight(theme::WEIGHT_SUBTITLE)
                .text_color(theme::text())
                .truncate()
                .child(card.album),
        )
        .child(
            div()
                .w_full()
                .min_w_0()
                .text_size(px(theme::FONT_CAPTION))
                .text_color(theme::text_muted())
                .truncate()
                .child(subtitle),
        )
        .into_any_element()
}

/// 一排歌单卡，收藏 + 每个自建歌单。
pub(super) fn list_section(
    index: usize,
    english: &'static str,
    id: &'static str,
    title: &'static str,
    cards: Vec<(Scope, HeroInfo)>,
    home: &HomeView,
    view: Entity<HomeView>,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let row = cards
        .into_iter()
        .enumerate()
        .map(|(index, (scope, info))| {
            list_card(
                index,
                scope,
                info,
                &home.library,
                &home.covers,
                &home.shell,
                cx,
            )
        })
        .collect();
    scroll_row(index, english, id, title, row, home, view, window, cx)
}

/// 一张歌单卡。点它是收窄作用域并切到曲库页，不是开播，所以不画 hover 播放键。
fn list_card(
    index: usize,
    scope: Scope,
    info: HeroInfo,
    library: &Entity<LibraryEntity>,
    covers: &Entity<CoverCache>,
    shell: &Entity<ShellEntity>,
    cx: &mut App,
) -> AnyElement {
    let cover = crate::common::cover::list_cover(
        covers,
        library,
        POSTER_COVER,
        theme::RADIUS_LG,
        info.cover_file.as_deref(),
        info.cover_id,
        &info.title,
        cx,
    );
    let shell = shell.clone();
    let library = library.clone();
    let tip = if info.meta.is_empty() {
        info.title.clone()
    } else {
        format!("{} │ {}", info.title, info.meta)
    };

    div()
        .id(("home-list", index))
        .flex_none()
        .w(px(POSTER_COVER + POSTER_PAD * 2.0))
        .flex()
        .flex_col()
        .gap(px(theme::GAP_SM))
        .p(px(POSTER_PAD))
        .cursor_pointer()
        .tooltip(crate::common::tooltip::builder(tip))
        .on_click(move |_, _, cx| {
            crate::shell::enter_scope_from_here(scope.clone(), &shell, &library, cx);
        })
        .child(cover)
        .child(
            div()
                .w_full()
                .min_w_0()
                .text_size(px(theme::FONT_BODY))
                .font_weight(theme::WEIGHT_SUBTITLE)
                .text_color(theme::text())
                .truncate()
                .child(info.title),
        )
        .child(
            div()
                .w_full()
                .min_w_0()
                .text_size(px(theme::FONT_CAPTION))
                .text_color(theme::text_muted())
                .truncate()
                .child(info.meta),
        )
        .into_any_element()
}

/// 封面槽。命中就显示图片，否则发一次请求并显示占位。
fn cover_slot(
    covers: &Entity<CoverCache>,
    library: &Entity<LibraryEntity>,
    id: TrackId,
    size: f32,
    label: &str,
    cx: &mut App,
) -> AnyElement {
    let state = crate::common::cover::cover_state(covers, library, id, size, cx);

    let cell = div()
        .flex_none()
        .size(px(size))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(theme::RADIUS_LG))
        .bg(theme::elevated());

    match state {
        crate::common::cover::Cover::Ready(image) => {
            let cell = cell.child(
                img(ImageSource::Render(std::sync::Arc::clone(&image)))
                    .size_full()
                    .rounded(px(theme::RADIUS_LG))
                    .object_fit(ObjectFit::Cover),
            );
            crate::common::cover::fade_in(&image, cell).into_any_element()
        }
        // 确认没有封面，首字块。
        crate::common::cover::Cover::Missing => cell
            .child(crate::common::cover::monogram(
                crate::common::cover::initial_of(label),
                size,
                theme::RADIUS_LG,
            ))
            .into_any_element(),
        // 解码中，骨架屏。
        crate::common::cover::Cover::Pending => cell
            .child(crate::common::cover::skeleton(size, theme::RADIUS_LG))
            .into_any_element(),
    }
}

/// 没有封面时的占位统一走 [`crate::common::cover::monogram`]，此文件不再自行绘制。
/// 首页右侧的展示画框，切角封面 + 硬边斜线纹影子 + 墨色铭牌。
/// 内容全部来自最近播放的那张专辑；没有记录时给出空状态。
pub(super) fn showcase_frame(
    card: Option<&RecentCard>,
    library: &Entity<LibraryEntity>,
    covers: &Entity<CoverCache>,
    cx: &mut App,
) -> AnyElement {
    let shadow = div()
        .absolute()
        .top(px(12.))
        .left(px(12.))
        .size_full()
        .bg(theme::elevated())
        .child(crate::common::ornament::hatch_fill(
            theme::text().opacity(0.22),
        ));

    let mut frame = div()
        .flex_none()
        .relative()
        .w(px(360.))
        .h(px(280.))
        .child(shadow);

    let Some(card) = card else {
        let empty = div()
            .relative()
            .size_full()
            .bg(theme::bg())
            .border_1()
            .border_color(theme::text())
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap(px(theme::GAP_SM))
            .child(crate::common::ornament::hatch_fill(
                theme::text().opacity(0.05),
            ))
            .child(
                div()
                    .text_size(px(theme::FONT_BODY))
                    .font_weight(theme::WEIGHT_BUTTON)
                    .text_color(theme::text())
                    .child("还没有播放记录"),
            )
            .child(
                div()
                    .text_size(px(theme::FONT_CAPTION))
                    .text_color(theme::text_muted())
                    .child("播放任意一首后，最近听的那张专辑会出现在这里。"),
            )
            .child(crate::common::ornament::cut_tl(theme::CUT_LG, theme::bg()))
            .child(crate::common::ornament::cut_br(theme::CUT_LG, theme::bg()));
        frame = frame.child(empty);
        return frame.into_any_element();
    };

    let mut panel = div()
        .relative()
        .size_full()
        .bg(theme::bg())
        .border_1()
        .border_color(theme::text());

    match crate::common::cover::cover_image(covers, library, card.id, 360., cx) {
        Some(image) => {
            panel = panel.child(crate::common::cover::fade_in(
                &image,
                img(ImageSource::Render(std::sync::Arc::clone(&image)))
                    .absolute()
                    .size_full()
                    .object_fit(ObjectFit::Cover),
            ));
        }
        None => {
            panel = panel
                .bg(theme::control())
                .flex()
                .items_center()
                .justify_center()
                .child(
                    div()
                        .font(theme::label_font())
                        .text_size(px(72.))
                        .font_weight(FontWeight::EXTRA_BOLD)
                        .text_color(theme::text_on_control())
                        .child(
                            card.album
                                .chars()
                                .next()
                                .map_or_else(|| "♪".to_owned(), |ch| ch.to_string()),
                        ),
                );
        }
    }

    let artist = if card.artist.is_empty() {
        card.meta.clone()
    } else {
        format!("{} │ {}", card.artist, card.meta)
    };

    panel = panel
        .child(crate::common::ornament::cut_tl(theme::CUT_LG, theme::bg()))
        .child(crate::common::ornament::cut_br(theme::CUT_LG, theme::bg()))
        .child(
            div()
                .absolute()
                .left_0()
                .bottom_0()
                .max_w(px(290.))
                .flex()
                .flex_col()
                .gap(px(2.))
                .pl(px(theme::GAP_MD))
                .pr(px(theme::GAP_MD + theme::CUT_LG))
                .py(px(theme::GAP_SM))
                .bg(theme::control())
                .child(
                    div()
                        .font(theme::label_font())
                        .text_size(px(theme::FONT_EYEBROW))
                        .font_weight(theme::WEIGHT_BUTTON)
                        .text_color(theme::text_on_control())
                        .child("// RECENT / 最近"),
                )
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .text_size(px(theme::FONT_SUBTITLE))
                        .font_weight(theme::WEIGHT_TITLE)
                        .text_color(theme::text_on_control())
                        .child(card.album.clone()),
                )
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .font(theme::field_font())
                        .text_size(px(theme::FONT_CAPTION))
                        .text_color(theme::text_on_control().opacity(0.72))
                        .child(artist),
                ),
        );

    frame = frame.child(panel);
    frame.into_any_element()
}

#[cfg(test)]
mod tests {
    // 不写 `use super::*`，`platform::*` 里的 gpui `test` 过程宏会遮蔽内置 `#[test]`。
    use super::RowEdges;

    /// 首帧放行。布局还没跑过时读到的 `bounds` / `max_offset` 都是 0，据此断言「到底」会让两枚箭头同时隐藏。
    #[test]
    fn the_first_frame_claims_nothing() {
        let unmeasured = RowEdges::of(0.0, 0.0, 0.0);
        assert!(unmeasured.can_go(false), "未测量时左箭头要可用");
        assert!(unmeasured.can_go(true), "未测量时右箭头要可用");
    }

    #[test]
    fn a_row_that_fits_disables_both_arrows() {
        let edges = RowEdges::of(600.0, 0.0, 0.0);
        assert!(!edges.can_go(false) && !edges.can_go(true));
    }

    #[test]
    fn each_arrow_dies_only_at_its_own_end() {
        let at_left = RowEdges::of(600.0, 0.0, 900.0);
        assert!(!at_left.can_go(false), "在最左边，左箭头该灰");
        assert!(at_left.can_go(true), "在最左边，右箭头该亮");

        let at_right = RowEdges::of(600.0, -900.0, 900.0);
        assert!(at_right.can_go(false), "在最右边，左箭头该亮");
        assert!(!at_right.can_go(true), "在最右边，右箭头该灰");
    }

    #[test]
    fn the_end_tolerance_is_half_a_pixel() {
        assert!(!RowEdges::of(600.0, -899.7, 900.0).can_go(true));
        assert!(RowEdges::of(600.0, -880.0, 900.0).can_go(true));
        assert!(!RowEdges::of(600.0, -0.3, 900.0).can_go(false));
        assert!(RowEdges::of(600.0, -8.0, 900.0).can_go(false));
    }
}
