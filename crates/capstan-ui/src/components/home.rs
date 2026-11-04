//! 首页由最近播放快捷网格、横向滚动区块与浏览入口组成。横向区块含你的歌单、为你推荐与热门专辑。
//! 「最近播放」取自 [`crate::recents::Recents`]（跨重启），与队列面板的「已播放」不是同一份数据。

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};

use capstan_core::{AlbumGroup, TrackId};
use chrono::Timelike as _;

use crate::common::{anim, thousands};
use crate::components::detail::{HeroInfo, hero_for};
use crate::cover::{CoverCache, CoverCacheEvent};
// `Scope` 必须显式导入。`platform::*` 里带着 `gpui::Scope<'_>`，靠 glob 引进来会撞名。
use crate::entities::{LibraryEntity, Scope};
use crate::icon::Glyph;
use crate::platform::*;
use crate::shell::{NavSection, ShellEntity};
use crate::theme;

mod cards;
mod entries;
mod sections;

pub use cards::{RecentCard, popular_cards, recent_cards, recommend_cards};

use entries::entry_card;
use sections::{horizontal_section, list_section, recent_card, section_title, showcase_frame};

const PAD: f32 = theme::PAGE_PAD;
const RECENT_MAX: usize = 8;
const SECTION_MAX: usize = 12;

/// 23 点后归入「夜深了」而不是「晚上好」；边界写错不会报错，只会在特定时段显示错误的问候语。
fn greeting(hour: u32) -> &'static str {
    match hour {
        5..=11 => "早上好",
        12..=17 => "下午好",
        18..=22 => "晚上好",
        _ => "夜深了",
    }
}

/// 读一次系统时钟；换算「距上次渲染多久」会在休眠唤醒后错得离谱。
fn local_hour() -> u32 {
    chrono::Local::now().hour()
}

fn greeting_hint(empty: bool, has_recents: bool) -> &'static str {
    if empty {
        "把音频文件拖进窗口，或者点侧栏的「导入音乐」"
    } else if has_recents {
        "接着上次的继续，或者从下面挑一个"
    } else {
        "曲库已经就绪，从下面挑一个开始听"
    }
}

pub struct HomeView {
    shell: Entity<ShellEntity>,
    library: Entity<LibraryEntity>,
    covers: Entity<CoverCache>,
    /// 每个横向区块一条滚动状态。句柄必须活过这一帧，每帧新建的话偏移永远归零。
    /// `RefCell` 而不是 `&mut self`，区块构造函数还要同时借 `self.library` 等字段。
    rows: RefCell<HashMap<&'static str, ScrollHandle>>,
    /// 首页重型派生的缓存；只在曲库数据代次变化时重建。
    data: Option<HomeData>,
    _subs: Vec<Subscription>,
}

/// 首页一帧需要的全部派生数据。封面完成只让视图重绘，不该重新分组和排序。
struct HomeData {
    revision: u64,
    recents: Vec<RecentCard>,
    counts: (usize, usize, usize),
    recommended: Vec<RecentCard>,
    popular: Vec<RecentCard>,
    lists: Vec<(Scope, HeroInfo)>,
    /// 当前页面显示会用到封面的曲目与图片文件 id；封面事件据此过滤。
    cover_ids: HashSet<TrackId>,
}

impl HomeView {
    pub fn new(
        shell: Entity<ShellEntity>,
        library: Entity<LibraryEntity>,
        covers: Entity<CoverCache>,
        cx: &mut Context<Self>,
    ) -> Self {
        let observe_library = cx.observe(&library, |_, _, cx| cx.notify());
        let observe_shell = cx.observe(&shell, |_, _, cx| cx.notify());
        // 不整体 observe 封面缓存，每张图解码完成都重绘整页会导致分组与排序无效重算。
        let on_cover = cx.subscribe(&covers, |this, _, event: &CoverCacheEvent, cx| {
            let relevant = match event {
                CoverCacheEvent::Ready(key) => this
                    .data
                    .as_ref()
                    .is_some_and(|data| data.cover_ids.contains(&key.track_id())),
                // 淘汰很少发生；错过它会让被清掉的封面一直停在占位图。
                CoverCacheEvent::Evicted => true,
            };
            if relevant {
                cx.notify();
            }
        });

        Self {
            shell,
            library,
            covers,
            rows: RefCell::default(),
            data: None,
            _subs: vec![observe_library, observe_shell, on_cover],
        }
    }

    fn sync_data(&mut self, cx: &mut App) {
        let revision = self.library.read(cx).data_revision();
        if self
            .data
            .as_ref()
            .is_some_and(|data| data.revision == revision)
        {
            return;
        }

        let recents = self.recents(cx);
        let counts = self.counts(cx);
        let groups = self.album_groups(cx);
        let recommended = recommend_cards(&groups, SECTION_MAX);
        let popular = popular_cards(&groups, SECTION_MAX);
        let lists = self.list_cards(cx);

        let mut cover_ids = HashSet::new();
        for card in recents.iter().chain(&recommended).chain(&popular) {
            cover_ids.insert(card.id);
        }
        for (_, info) in &lists {
            if let Some(id) = info.cover_id {
                cover_ids.insert(id);
            }
            if let Some(file) = &info.cover_file {
                cover_ids.insert(TrackId::from_path(file));
            }
        }

        self.data = Some(HomeData {
            revision,
            recents,
            counts,
            recommended,
            popular,
            lists,
            cover_ids,
        });
    }

    fn recents(&self, cx: &mut App) -> Vec<RecentCard> {
        let tracks = App::update_entity(cx, &self.library, |library, _| {
            library.recent_tracks(RECENT_MAX * 4)
        });
        recent_cards(&tracks, RECENT_MAX)
    }

    fn counts(&self, cx: &mut App) -> (usize, usize, usize) {
        App::update_entity(cx, &self.library, |library, _| {
            // 先触发分组，`album_count` / `artist_count` 读的是缓存。
            library.albums();
            library.artists();
            (library.len(), library.album_count(), library.artist_count())
        })
    }

    fn album_groups(&self, cx: &mut App) -> std::rc::Rc<Vec<AlbumGroup>> {
        App::update_entity(cx, &self.library, |library, _| library.albums())
    }

    /// 「你的歌单」那一排，卡片数据复用详情页的 [`hero_for`]，两处口径必须是同一个函数。
    /// 查不到的（歌单刚被删）整张卡不画。
    fn list_cards(&self, cx: &mut App) -> Vec<(Scope, HeroInfo)> {
        let scopes = App::update_entity(cx, &self.library, |library, _| library.list_scopes());
        scopes
            .into_iter()
            .filter_map(|scope| {
                let info =
                    App::update_entity(cx, &self.library, |library, _| hero_for(library, &scope))?;
                Some((scope, info))
            })
            .collect()
    }
}

impl Render for HomeView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // 横向区块的箭头点击后要立即重绘，所以取本视图的句柄，而不是 &mut self。
        let view = cx.entity();
        let loading = self.library.read(cx).loading();
        self.sync_data(cx);
        let Some(data) = self.data.as_ref() else {
            // `sync_data` 刚刚填充过；此处只可能是内部状态被破坏，宁可空白一帧也不 panic。
            return div().into_any_element();
        };
        let recents = data.recents.clone();
        let (tracks, albums, artists) = data.counts;
        let recommended = data.recommended.clone();
        let popular = data.popular.clone();
        let lists = data.lists.clone();

        let mut page = div()
            .id("home-page")
            .size_full()
            .flex()
            .flex_col()
            .overflow_y_scroll()
            .px(px(PAD))
            .pb(px(PAD));

        let center_width = self.shell.read(cx).center_width();
        let stamp = chrono::Local::now().format("%Y.%m.%d").to_string();
        let mut hero = div()
            .flex_none()
            .flex()
            .gap(px(theme::GAP_XL * 2.0))
            .pt(px(theme::GAP_XL))
            .pb(px(theme::GAP_XL))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .items_start()
                    .gap(px(theme::GAP_MD))
                    // 骨架（眉标）硬切；字标两行错开推入，数据晚 160ms 才落位。
                    .child(crate::common::section::eyebrow("CAPSTAN / ARCHIVE"))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .child(anim::drive(
                                "home-wordmark-solid",
                                0,
                                div()
                                    .font(theme::label_font())
                                    .text_size(px(theme::FONT_WORDMARK))
                                    .font_weight(FontWeight::EXTRA_BOLD)
                                    .text_color(theme::text())
                                    .child("CAPSTAN"),
                            ))
                            .child(anim::drive(
                                "home-wordmark-outline",
                                anim::WORDMARK_STAGGER_MS,
                                div()
                                    .font(theme::label_font())
                                    .text_size(px(theme::FONT_TITLE + 6.))
                                    .font_weight(theme::WEIGHT_BUTTON)
                                    .text_color(theme::border())
                                    .child(".COLLECTION"),
                            )),
                    )
                    .child(anim::scramble_content(
                        SharedString::from(format!("home-fields-{tracks}-{albums}-{artists}")),
                        anim::DATA_DELAY_MS + 40,
                        anim::MOTION_SCRAMBLE_MS,
                        div(),
                        move |progress| {
                            let stamp = stamp.clone();
                            crate::common::section::fields([
                                anim::scramble(&format!("{} 首", thousands(tracks)), progress),
                                anim::scramble(&format!("{} 张专辑", thousands(albums)), progress),
                                anim::scramble(
                                    &format!("{} 位艺术家", thousands(artists)),
                                    progress,
                                ),
                                stamp,
                            ])
                            .into_any_element()
                        },
                    ))
                    .child(anim::enter(
                        "home-greeting",
                        420,
                        anim::MOTION_COME_MS,
                        10.0,
                        div()
                            .pt(px(theme::GAP_SM))
                            .text_size(px(theme::FONT_TITLE))
                            .font_weight(theme::WEIGHT_TITLE)
                            .text_color(theme::text())
                            .child(greeting(local_hour())),
                    ))
                    .child(anim::enter(
                        "home-hint",
                        460,
                        anim::MOTION_COME_MS,
                        10.0,
                        div()
                            .min_w_0()
                            .text_size(px(theme::FONT_BODY))
                            .text_color(theme::text_muted())
                            .child(if loading {
                                "正在读取曲库…".to_owned()
                            } else {
                                greeting_hint(tracks == 0, !recents.is_empty()).to_owned()
                            }),
                    ))
                    .when(loading, |col| {
                        col.child(crate::common::anim::unknown_progress())
                    }),
            );

        // 字标 336 + 间距 48 + showcase 360 + 两侧 PAD 48，低于这个宽度宁可先不摆 showcase，
        // 否则 `flex_1` 的左列被压到字标以下，「CAPSTAN」会压到封面框上。
        if center_width >= 800.0 {
            hero = hero.child(anim::enter(
                "home-showcase",
                anim::DATA_DELAY_MS + 40,
                anim::MOTION_SCRAMBLE_MS,
                10.0,
                div().child(showcase_frame(
                    recents.first(),
                    &self.library,
                    &self.covers,
                    cx,
                )),
            ));
        }
        page = page.child(hero);

        // 区块编号按出现顺序从 01 起。最近播放为空时，下一个区块就是 01。
        let mut section_no = 0usize;

        if !recents.is_empty() {
            section_no += 1;
            page = page.child(anim::enter(
                "home-recent-title",
                anim::DATA_DELAY_MS,
                anim::MOTION_COME_MS,
                10.0,
                section_title(section_no, "RECENT", "最近播放"),
            ));
            // gpui 0.3.8 没有 flex_wrap，手工分行。列数与卡宽直接取曲库网格的，
            // 两处封面同宽，最近播放不会比曲库少一列。
            let grid_cols = self.shell.read(cx).grid_columns().max(1);
            let cover_size = self.shell.read(cx).grid_card_width().max(1.0);
            let mut grid = div()
                .flex_none()
                .flex()
                .flex_col()
                .gap(px(theme::GAP_MD))
                .pt(px(theme::GAP_MD))
                .pb(px(theme::GAP_XL));
            for (row, chunk) in recents.chunks(grid_cols).enumerate() {
                grid = grid.child(div().flex_none().flex().gap(px(theme::GAP_MD)).children(
                    chunk.iter().enumerate().map(|(col, card)| {
                        recent_card(
                            row * grid_cols + col,
                            card.clone(),
                            cover_size,
                            &self.library,
                            &self.covers,
                            cx,
                        )
                    }),
                ));
            }
            page = page.child(anim::enter(
                "home-recent-grid",
                anim::DATA_DELAY_MS,
                anim::MOTION_COME_MS,
                10.0,
                grid,
            ));
        }

        if !lists.is_empty() {
            section_no += 1;
            page = page.child(anim::enter(
                "home-lists",
                anim::DATA_DELAY_MS + anim::STAGGER_BLOCK_MS,
                anim::MOTION_COME_MS,
                10.0,
                div().flex_none().flex().flex_col().child(list_section(
                    section_no,
                    "PLAYLISTS",
                    "home-my-lists",
                    "你的歌单",
                    lists,
                    self,
                    view.clone(),
                    window,
                    cx,
                )),
            ));
        }

        if !recommended.is_empty() {
            section_no += 1;
            page = page.child(anim::enter(
                "home-recommend",
                anim::DATA_DELAY_MS + anim::STAGGER_BLOCK_MS * 2,
                anim::MOTION_COME_MS,
                10.0,
                div()
                    .flex_none()
                    .flex()
                    .flex_col()
                    .child(horizontal_section(
                        section_no,
                        "FOR YOU",
                        "home-recommend",
                        "为你推荐",
                        recommended,
                        self,
                        view.clone(),
                        window,
                        cx,
                    )),
            ));
        }

        if !popular.is_empty() {
            section_no += 1;
            page = page.child(anim::enter(
                "home-popular",
                anim::DATA_DELAY_MS + anim::STAGGER_BLOCK_MS * 3,
                anim::MOTION_COME_MS,
                10.0,
                div()
                    .flex_none()
                    .flex()
                    .flex_col()
                    .child(horizontal_section(
                        section_no,
                        "TRENDING",
                        "home-popular",
                        "热门专辑",
                        popular,
                        self,
                        view.clone(),
                        window,
                        cx,
                    )),
            ));
        }

        section_no += 1;
        page = page.child(anim::enter(
            "home-browse-title",
            anim::DATA_DELAY_MS + anim::STAGGER_BLOCK_MS * 4,
            anim::MOTION_COME_MS,
            10.0,
            section_title(section_no, "BROWSE", "浏览"),
        ));
        page = page.child(anim::enter(
            "home-browse-row",
            anim::DATA_DELAY_MS + anim::STAGGER_BLOCK_MS * 4,
            anim::MOTION_COME_MS,
            10.0,
            div()
                .flex_none()
                .flex()
                .gap(px(theme::GAP_MD))
                .pt(px(theme::GAP_MD))
                .children([
                    entry_card(
                        "home-entry-tracks",
                        Glyph::List,
                        "曲目",
                        format!("{} 首", thousands(tracks)),
                        NavSection::Tracks,
                        &self.shell,
                        &self.library,
                    ),
                    entry_card(
                        "home-entry-albums",
                        Glyph::Album,
                        "专辑",
                        format!("{} 张", thousands(albums)),
                        NavSection::Albums,
                        &self.shell,
                        &self.library,
                    ),
                    entry_card(
                        "home-entry-artists",
                        Glyph::Artist,
                        "艺术家",
                        format!("{} 位", thousands(artists)),
                        NavSection::Artists,
                        &self.shell,
                        &self.library,
                    ),
                ]),
        ));

        page.into_any_element()
    }
}

#[cfg(test)]
mod tests {
    // 不写 `use super::*`，`crate::platform::*` 会把 gpui 的 `test` 过程宏
    // 带进来遮蔽内置 `#[test]`，报递归上限。
    use super::{greeting, greeting_hint};

    #[test]
    fn greeting_covers_every_hour_and_switches_at_the_right_one() {
        for hour in 0..24 {
            assert!(
                !greeting(hour).is_empty(),
                "{hour} 点没有问候语，标题会塌成空白的一行"
            );
        }

        assert_eq!(greeting(0), "夜深了");
        assert_eq!(greeting(4), "夜深了");
        assert_eq!(greeting(5), "早上好");
        assert_eq!(greeting(11), "早上好");
        assert_eq!(greeting(12), "下午好");
        assert_eq!(greeting(17), "下午好");
        assert_eq!(greeting(18), "晚上好");
        assert_eq!(greeting(22), "晚上好");
        assert_eq!(greeting(23), "夜深了");
    }

    #[test]
    fn greeting_hint_only_suggests_what_is_actually_possible() {
        assert!(
            greeting_hint(true, false).contains("导入"),
            "空曲库应当引导去导入"
        );
        assert!(greeting_hint(false, false).contains("挑一个开始听"));
        assert!(greeting_hint(false, true).contains("接着上次"));
    }
}
