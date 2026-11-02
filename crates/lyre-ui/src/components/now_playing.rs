//! 右侧「正在播放」面板，封面 + 标题 + 关于艺术家 + 制作人员 + 下一首。
//! 封面按需加载。曲目变化时才读标签、解码，结果落到 `CoverCache`。

use std::sync::Arc;
use std::time::Duration;

use lyre_core::{Track, UNKNOWN_ARTIST, artist_keys_str, format_duration};

use crate::common::thousands;
use crate::cover::CoverCache;
use crate::entities::{LibraryEntity, PlaybackEntity, PlaybackEvent};
use crate::icon::Glyph;
use crate::platform::*;
use crate::shell::ShellEntity;
use crate::theme;

use super::panel_close_button;

const COVER_SIZE: f32 = 200.0;
/// 「下一首」卡片里的小封面边长。
const UP_NEXT_COVER: f32 = 44.0;

pub struct NowPlaying {
    playback: Entity<PlaybackEntity>,
    covers: Entity<CoverCache>,
    library: Entity<LibraryEntity>,
    shell: Entity<ShellEntity>,
    /// 这一栏正在画的两张封面（当前曲 + 下一首），用来判断缓存的 notify 到底有没有改变画面。
    shown: [Option<Arc<RenderImage>>; 2],
    _track_sub: Subscription,
    _cover_sub: Subscription,
    _library_sub: Subscription,
    _shell_sub: Subscription,
}

impl NowPlaying {
    pub fn new(
        playback: Entity<PlaybackEntity>,
        covers: Entity<CoverCache>,
        library: Entity<LibraryEntity>,
        shell: Entity<ShellEntity>,
        cx: &mut Context<Self>,
    ) -> Self {
        // 用 `subscribe` 而不是 `observe`。`PlaybackEntity` 播放期间每 100ms 因进度 notify 一次，
        // 而 gpui 的 `notify` 是全树失效；逐帧进度归 `SeekBar` / `VolumeControl`。
        let track_sub = cx.subscribe(&playback, |this, _, event: &PlaybackEvent, cx| {
            match event {
                PlaybackEvent::TrackChanged | PlaybackEvent::Restored => {
                    this.refresh_covers(cx);
                    cx.notify();
                }
                // 状态变化也要跟。加载失败走的是 `Error → StateChanged`，只听换曲会让错误提示拖到切下一首才出现。
                PlaybackEvent::StateChanged => cx.notify(),
                // 设备列表只有底栏的设备菜单关心。失败提示由 `RootView` 统一弹 toast，
                // 这一栏的错误卡片跟着 `StateChanged` 重绘就够了。
                PlaybackEvent::Finished
                | PlaybackEvent::VolumeChanged
                | PlaybackEvent::DevicesChanged
                | PlaybackEvent::OutputModeChanged
                | PlaybackEvent::Failed(_)
                | PlaybackEvent::Notice(_) => {}
            }
        });

        // 封面解码完成时缓存会 notify。只在这一栏要画的那两张真的换了才重绘：
        // 否则滚动曲库时每张落地封面都拖着整栏重排一次。
        let cover_sub = cx.observe(&covers, |this, _, cx| {
            this.refresh_covers(cx);
        });

        // 关注状态、艺术家统计、下一首都来自曲库；曲库变了要重画。
        let library_sub = cx.observe(&library, |_, _, cx| cx.notify());
        // 浮层模式下右上角新增关闭键；面板实体不一定随根视图重绘，需自行订阅外壳。
        let shell_sub = cx.observe(&shell, |_, _, cx| cx.notify());

        let mut this = Self {
            playback,
            covers,
            library,
            shell,
            shown: [None, None],
            _track_sub: track_sub,
            _cover_sub: cover_sub,
            _library_sub: library_sub,
            _shell_sub: shell_sub,
        };
        this.refresh_covers(cx);
        this
    }

    /// 缓存里任何一张图解码完都会 notify，这里只回答「这一栏要画的两张封面换了没有」。
    /// 发请求交给渲染路径的 [`crate::common::cover::cover_state`]，它本来就管三态。
    fn refresh_covers(&mut self, cx: &mut Context<Self>) {
        let current = self.playback.read(cx).current().map(|track| track.id);
        let next = App::update_entity(cx, &self.library, |library, _| {
            library.upcoming_ids(2).into_iter().nth(1)
        });
        let fresh = [
            current.and_then(|id| self.covers.read(cx).ready(id, COVER_SIZE)),
            next.and_then(|id| self.covers.read(cx).ready(id, UP_NEXT_COVER)),
        ];

        let changed = fresh
            .iter()
            .zip(&self.shown)
            .any(|(new, old)| match (new, old) {
                (Some(new), Some(old)) => !Arc::ptr_eq(new, old),
                (new, old) => new.is_some() != old.is_some(),
            });
        if changed {
            self.shown = fresh;
            cx.notify();
        }
    }

    /// 从曲库派生的三样东西，即当前曲的艺术家、每位艺术家的分组统计与队列里的下一首。
    /// 全部读缓存与只取两首，适合每帧调用。
    fn derived(&self, track: Option<&Track>, cx: &mut App) -> Derived {
        App::update_entity(cx, &self.library, |library, _| {
            // 标签里的「甲/乙」是两个人，不是一个人名，艺术家分组里也没有这一整串。
            // 按拆开的单人查统计与关注，一栏里就该列几行；查整串会让统计和关注一起消失。
            let groups = library.artists();
            let artists = shown_artists(track.and_then(|t| t.artist.as_deref()))
                .into_iter()
                .map(|name| ArtistLine {
                    stats: groups
                        .iter()
                        .find(|group| group.name == name)
                        .map(|group| (group.track_count(), group.album_count())),
                    following: library.is_following(name),
                    name: name.to_owned(),
                })
                .collect();

            // `upcoming(2)` 的第一首是当前曲（光标位），第二首才是真正的「下一首」。
            let next = library.upcoming(2).into_iter().nth(1);

            Derived { artists, next }
        })
    }
}

/// 「关于艺术家」卡里的一行。一首歌有几位艺术家就有几行。每行各自关注、各自计数。
struct ArtistLine {
    /// 拆分后的单个名字，见 `lyre_core::artist_keys`。
    name: String,
    /// 这位艺术家名下的曲目数与专辑数；曲库里查不到（刚移出）时为 `None`。
    stats: Option<(usize, usize)>,
    following: bool,
}

struct Derived {
    /// 空表表示这一首没有可显示的艺术家，整张卡不画。
    artists: Vec<ArtistLine>,
    next: Option<Track>,
}

impl Render for NowPlaying {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let overlay = self.shell.read(cx).right_panel_overlays();
        let (track, duration, error) = {
            let playback = self.playback.read(cx);
            (
                playback.current().cloned(),
                playback.duration(),
                playback.error().map(ToOwned::to_owned),
            )
        };
        let Derived { artists, next } = self.derived(track.as_ref(), cx);

        let canvas = div()
            .relative()
            .flex_none()
            .size(px(COVER_SIZE))
            .bg(theme::elevated())
            .flex()
            .items_center()
            .justify_center();

        let canvas = match track.as_ref() {
            Some(track) => {
                let state = crate::common::cover::cover_state(
                    &self.covers,
                    &self.library,
                    track.id,
                    COVER_SIZE,
                    cx,
                );
                match state {
                    crate::common::cover::Cover::Ready(image) => canvas.child(
                        img(ImageSource::Render(image))
                            .size_full()
                            .object_fit(ObjectFit::Cover),
                    ),
                    crate::common::cover::Cover::Pending => {
                        canvas.child(crate::common::cover::skeleton(COVER_SIZE, theme::RADIUS_LG))
                    }
                    crate::common::cover::Cover::Missing => {
                        canvas.child(crate::common::cover::monogram(
                            track.title.chars().next(),
                            COVER_SIZE,
                            theme::RADIUS_LG,
                        ))
                    }
                }
            }
            None => canvas.child(crate::common::cover::monogram(
                None,
                COVER_SIZE,
                theme::RADIUS_LG,
            )),
        }
        .child(crate::common::ornament::cut_br(14., theme::bg()));

        let (title, subtitle) = match track.as_ref() {
            Some(track) => (track.title.clone(), track.subtitle()),
            None => ("未在播放".to_owned(), String::new()),
        };
        let has_subtitle = !subtitle.is_empty();

        // 宽度撑满才能截断长标题。
        let mut hero = div()
            .flex_none()
            .w_full()
            .flex()
            .flex_col()
            .items_center()
            .gap(px(theme::GAP_LG))
            .child(canvas)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap(px(theme::GAP_XS))
                    .w_full()
                    .child(crate::common::section::eyebrow("Now Playing"))
                    .child(
                        div()
                            .id("np-title")
                            .w_full()
                            .text_center()
                            .text_size(px(theme::FONT_TITLE))
                            .font_weight(theme::WEIGHT_TITLE)
                            .text_color(theme::text())
                            .truncate()
                            .tooltip(crate::common::tooltip::builder(title.clone()))
                            .child(title),
                    )
                    .child(
                        div()
                            .id("np-subtitle")
                            .w_full()
                            .text_center()
                            .text_size(px(theme::FONT_CAPTION))
                            .text_color(theme::text_muted())
                            .truncate()
                            .when(has_subtitle, |line| {
                                line.tooltip(crate::common::tooltip::builder(subtitle.clone()))
                            })
                            .child(subtitle),
                    ),
            );

        if track.as_ref().is_some_and(|t| t.duration.is_some()) {
            hero = hero.child(crate::common::section::field(format_duration(duration)));
        }

        // 换曲。只让封面 + 标题这一块重新入场，下面的关于与制作信息直接换。
        let track_key = track.as_ref().map_or(0, |track| track.id.0);

        if let Some(error) = error {
            let notice = div()
                .relative()
                .w(px(COVER_SIZE))
                .flex()
                .items_start()
                .gap(px(theme::GAP_SM))
                .bg(theme::elevated())
                .border_l(px(4.))
                .border_color(theme::text())
                .p(px(theme::GAP_SM))
                .text_size(px(theme::FONT_CAPTION))
                .text_color(theme::text())
                .child(
                    div()
                        .flex_none()
                        .mt(px(3.))
                        .size(px(8.))
                        .bg(theme::accent()),
                )
                .child(error.clone());
            hero = hero.child(notice.with_animation(
                SharedString::from(format!("np-error-{track_key}-{error}")),
                Animation::new(Duration::from_millis(280)).with_easing(theme::ease_mech()),
                |notice, t| {
                    notice.child(
                        // 纸色盖板从右往左退开，左侧先露出提示，前缘随进度移动。
                        div()
                            .absolute()
                            .top_0()
                            .bottom_0()
                            .right_0()
                            .w(px(COVER_SIZE * (1.0 - t)))
                            .bg(theme::panel()),
                    )
                },
            ));
        }

        let hero = crate::common::anim::enter(
            SharedString::from(format!("np-hero-{track_key}")),
            0,
            crate::common::anim::MOTION_COME_MS,
            10.0,
            hero,
        );

        let mut body = div()
            .w_full()
            .flex()
            .flex_col()
            .gap(px(theme::GAP_LG))
            .child(hero);

        if !artists.is_empty() {
            body = body.child(about_artist(artists, &self.library));
        }

        if let Some(track) = track.as_ref() {
            body = body.child(credits(track));
        }

        if let Some(next) = next {
            body = body.child(up_next(&next, &self.covers, &self.library, cx));
        }

        div()
            .flex_none()
            .relative()
            .w(px(theme::RIGHT_PANEL_W))
            // 只写 `w()` 压不住内容。flex 默认 `min-width: auto`，长歌名会把面板顶宽。`min_w` + `max_w` 钉死。
            .min_w(px(theme::RIGHT_PANEL_W))
            .max_w(px(theme::RIGHT_PANEL_W))
            .overflow_hidden()
            .h_full()
            .flex()
            .flex_col()
            .bg(theme::panel())
            .child(
                div()
                    .id("now-playing-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .p(px(theme::GAP_XL))
                    .child(body),
            )
            // 浮层覆盖主区时没有独立标题栏，关闭键浮于封面之外的右上角。
            .when(overlay, |panel| {
                panel.child(
                    div()
                        .absolute()
                        .top(px(theme::GAP_SM))
                        .right(px(theme::GAP_SM))
                        .child(panel_close_button("np-close", &self.shell)),
                )
            })
    }
}

fn card() -> Div {
    div()
        .w_full()
        .flex()
        .flex_col()
        .gap(px(theme::GAP_SM))
        .bg(theme::surface())
        .border_1()
        .border_color(theme::border())
        .p(px(theme::GAP_MD))
}

/// 这一栏要列的艺术家。标签按「/」「,」拆成单个人，并剔掉「未知艺术家」。
/// 该位艺术家既无主页也无统计，列出仅为一行噪声，此时整张卡片不予绘制。
/// 拆分口径必须与 [`lyre_core::artist_keys`] 一致，否则点进去的组与这里的名字对不上。
fn shown_artists(artist: Option<&str>) -> Vec<&str> {
    artist_keys_str(artist)
        .into_iter()
        .filter(|name| *name != UNKNOWN_ARTIST)
        .collect()
}

/// 「关于艺术家」卡。纸色底 + 1px 细线，不引入第二套色相。
/// 一首歌挂了几位艺术家就画几行。标签里的「甲/乙」是两个人，不是一个人名，
/// 关注与曲目数都得各算各的，所以下面没有「合起来」的那一条。
fn about_artist(artists: Vec<ArtistLine>, library: &Entity<LibraryEntity>) -> AnyElement {
    let mut card = div()
        .w_full()
        .flex()
        .flex_col()
        .gap(px(theme::GAP_SM))
        .bg(theme::window_base())
        .border_1()
        .border_color(theme::border())
        .p(px(theme::GAP_MD))
        .child(crate::common::section::eyebrow("Artist"));

    for (index, line) in artists.into_iter().enumerate() {
        // 第二位起先压一条细线。不留缝隙时多位艺术家的名字会连成一段，无法辨为两人。
        if index > 0 {
            card = card.child(div().w_full().h(px(1.)).bg(theme::border()));
        }
        card = card.child(artist_row(index, line, library.clone()));
    }

    card.into_any_element()
}

/// 一位艺术家。名字 + 关注按钮，下面是这位名下的曲目数与专辑数。
fn artist_row(index: usize, line: ArtistLine, library: Entity<LibraryEntity>) -> AnyElement {
    let ArtistLine {
        name,
        stats,
        following,
    } = line;

    // id 必须带序号。同一个卡里出现两枚同 id 的按钮，悬停与点击状态会串。
    let follow = crate::common::button::toggle_chip(
        SharedString::from(format!("np-follow-{index}")),
        if following {
            Glyph::HeartFilled
        } else {
            Glyph::Heart
        },
        if following { "已关注" } else { "关注" },
        following,
    )
    .tooltip(crate::common::tooltip::builder(if following {
        format!("取消关注 {name}")
    } else {
        format!("关注 {name}")
    }))
    .on_click({
        let name = name.clone();
        move |_, _, cx| {
            library.update(cx, |library, cx| {
                library.toggle_follow(name.clone(), cx);
            });
        }
    });

    let mut row = div()
        .w_full()
        .flex()
        .flex_col()
        .gap(px(theme::GAP_SM))
        .child(
            div()
                .w_full()
                .flex()
                .items_center()
                .gap(px(theme::GAP_SM))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_size(px(theme::FONT_BODY))
                        .font_weight(theme::WEIGHT_SUBTITLE)
                        .text_color(theme::text())
                        .child(name),
                )
                .child(follow),
        );

    if let Some((tracks, albums)) = stats {
        row = row.child(
            div()
                .text_size(px(theme::FONT_CAPTION))
                .text_color(theme::text_muted())
                .child(format!(
                    "{} 首 │ {} 张专辑",
                    thousands(tracks),
                    thousands(albums)
                )),
        );
    }

    row.into_any_element()
}

/// 「制作人员」。曲目标签里只有艺术家、专辑与格式，没有词曲、制作人，只列可得的字段。
/// 艺术家逐行列（第二行起不重复标签）。一行只写一个人，不把「甲/乙」当成一个人名。
fn credits(track: &Track) -> AnyElement {
    let format = track.format().unwrap_or_else(|| "未知".to_owned());
    let artists = shown_artists(track.artist.as_deref());

    let mut card = card()
        .child(crate::common::section::eyebrow("Credits"))
        .child(crate::common::section::section_heading("制作人员"));

    if artists.is_empty() {
        card = card.child(credit_row("艺术家", "未知"));
    } else {
        for (index, name) in artists.into_iter().enumerate() {
            card = card.child(credit_row(if index == 0 { "艺术家" } else { "" }, name));
        }
    }

    card.child(credit_row("专辑", track.album.as_deref().unwrap_or("未知")))
        .child(credit_row("格式", format.as_str()))
        .into_any_element()
}

fn credit_row(label: &'static str, value: &str) -> impl IntoElement {
    div()
        .w_full()
        .flex()
        .items_center()
        .gap(px(theme::GAP_SM))
        .child(
            div()
                .flex_none()
                .w(px(48.))
                .text_size(px(theme::FONT_CAPTION))
                .text_color(theme::text_faint())
                .child(label),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .text_size(px(theme::FONT_CAPTION))
                .text_color(theme::text())
                .child(value.to_owned()),
        )
}

fn up_next(
    next: &Track,
    covers: &Entity<CoverCache>,
    library: &Entity<LibraryEntity>,
    cx: &mut App,
) -> AnyElement {
    let id = next.id;
    let state = crate::common::cover::cover_state(covers, library, id, UP_NEXT_COVER, cx);

    let thumb = div()
        .flex_none()
        .size(px(UP_NEXT_COVER))
        .rounded(px(theme::RADIUS_LG))
        .overflow_hidden()
        .bg(theme::elevated())
        .flex()
        .items_center()
        .justify_center();

    let note = || {
        div()
            .text_size(px(theme::FONT_BODY))
            .text_color(theme::text_faint())
            .child("♪")
    };

    let thumb: AnyElement = match state {
        crate::common::cover::Cover::Ready(image) => {
            let cell = thumb.child(
                img(ImageSource::Render(std::sync::Arc::clone(&image)))
                    .size_full()
                    .object_fit(ObjectFit::Cover),
            );
            crate::common::cover::fade_in(&image, cell).into_any_element()
        }
        crate::common::cover::Cover::Missing => thumb.child(note()).into_any_element(),
        crate::common::cover::Cover::Pending => thumb
            .child(crate::common::cover::skeleton(
                UP_NEXT_COVER,
                theme::RADIUS_SM,
            ))
            .into_any_element(),
    };

    let subtitle = next.subtitle();
    let activate = library.clone();

    div()
        .w_full()
        .child(crate::common::section::eyebrow("Next"))
        .child(crate::common::section::section_heading("下一首"))
        .child(
            div()
                .id("np-up-next")
                .w_full()
                .mt(px(theme::GAP_SM))
                .flex()
                .items_center()
                .gap(px(theme::GAP_MD))
                .cursor_pointer()
                .rounded(px(theme::RADIUS_MD))
                .child(thumb)
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .child(
                            div()
                                .w_full()
                                .truncate()
                                .text_size(px(theme::FONT_BODY))
                                .text_color(theme::text())
                                .child(next.title.clone()),
                        )
                        .when(!subtitle.is_empty(), |col| {
                            col.child(
                                div()
                                    .w_full()
                                    .truncate()
                                    .text_size(px(theme::FONT_CAPTION))
                                    .text_color(theme::text_muted())
                                    .child(subtitle),
                            )
                        }),
                )
                .on_click(move |_, _, cx| {
                    activate.update(cx, |library, cx| {
                        library.activate(id, cx);
                    });
                }),
        )
        .into_any_element()
}

#[cfg(test)]
mod tests {
    // 不写 `use super::*`，`crate::platform::*` 里的 gpui `test` 过程宏会遮蔽内置 `#[test]`。
    use super::shown_artists;
    use lyre_core::UNKNOWN_ARTIST;

    /// 一栏里列的是人，不是标签原文，`23.exe/初音ミク` 必须拆成两位。
    #[test]
    fn a_collaboration_credit_lists_each_artist() {
        assert_eq!(
            shown_artists(Some("23.exe/初音ミク")),
            ["23.exe", "初音ミク"]
        );
        assert_eq!(
            shown_artists(Some("初星学園, SHiNTA")),
            ["初星学園", "SHiNTA"],
            "逗号也是分隔符"
        );
        assert_eq!(
            shown_artists(Some("カンザキイオリ/25時、ナイトコードで。")),
            ["カンザキイオリ", "25時、ナイトコードで。"],
            "顿号属于组合名本身，不能切开"
        );
    }

    /// 拆出来的名字必须与艺术家分组同名，否则统计与关注按钮查不到东西。
    #[test]
    fn split_names_match_the_artist_groups() {
        let track = lyre_core::Track {
            artist: Some("23.exe/初音ミク".to_owned()),
            ..lyre_core::Track::fallback(std::path::Path::new("/music/a.flac"))
        };
        assert_eq!(
            shown_artists(track.artist.as_deref()),
            lyre_core::artist_keys(&track)
        );
    }

    /// 没有艺术家就一行都不列，整张卡不画。
    #[test]
    fn no_artist_lists_nothing() {
        assert!(shown_artists(None).is_empty());
        assert!(shown_artists(Some("   ")).is_empty());
        assert!(shown_artists(Some("/")).is_empty());
        assert!(
            !shown_artists(None).contains(&UNKNOWN_ARTIST),
            "「未知艺术家」不该占一行"
        );
    }
}
