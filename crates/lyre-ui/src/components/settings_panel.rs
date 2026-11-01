//! 设置面板：每项平铺成「标题 + 一组互斥选项」，最多三个选项；另有只读的快捷键一览与数据统计。

use std::rc::Rc;

use crate::common::anim;
use crate::common::button::{ButtonKind, TAB_STOP, text_button_animated};
use crate::common::thousands;
use crate::entities::{LibraryEntity, PlaybackEntity, PlaybackEvent};
use crate::icon::{Glyph, icon};
use crate::persist::data_dir;
use crate::platform::*;
use crate::settings::Settings;
use crate::shell::{Density, NavSection, PanelTab, ShellEntity, SidebarState};
use crate::theme;
use lyre_audio::OutputMode;

pub struct SettingsPanel {
    settings: Entity<SettingsEntity>,
    shell: Entity<ShellEntity>,
    library: Entity<LibraryEntity>,
    /// 音频相关的偏好改完要立刻下发，不能只等下次启动。
    playback: Entity<PlaybackEntity>,
    /// 快捷键卡的展开态，本地状态、不落盘。
    shortcuts_open: bool,
    /// 面板整块可纵向滚动，矮窗口下「导入」以下会被裁掉。
    /// gpui 不画滚动条，要自己叠一条（见 `common::scroll::scrollbar`）；句柄必须常驻，每帧新建等于归零。
    scroll: ScrollHandle,
    _subs: Vec<Subscription>,
}

impl SettingsPanel {
    pub fn new(
        settings: Entity<SettingsEntity>,
        shell: Entity<ShellEntity>,
        library: Entity<LibraryEntity>,
        playback: Entity<PlaybackEntity>,
        cx: &mut Context<Self>,
    ) -> Self {
        let sub = cx.observe(&settings, |_, _, cx| cx.notify());
        // 数据卡写着各项计数，所以要 observe 曲库；曲库仅在扫描时每 80ms notify 一批。
        let library_sub = cx.observe(&library, |_, _, cx| cx.notify());
        // 只关心输出模式：observe 播放实体会跟着进度的 10Hz 重绘一起白跑
        let mode_sub = cx.subscribe(&playback, |_, _, event: &PlaybackEvent, cx| {
            if matches!(event, PlaybackEvent::OutputModeChanged) {
                cx.notify();
            }
        });
        Self {
            settings,
            shell,
            library,
            playback,
            shortcuts_open: false,
            scroll: ScrollHandle::new(),
            _subs: vec![sub, library_sub, mode_sub],
        }
    }

    /// 独占协商到的口径。非独占或还没协商到时没有可显示的东西。
    fn output_detail(&self, cx: &Context<Self>) -> Option<impl IntoElement> {
        let playback = self.playback.read(cx);
        if playback.output_mode() != OutputMode::Exclusive {
            return None;
        }
        let detail = playback.output_detail();
        let text = if detail.is_empty() {
            "正在协商独占格式…".to_owned()
        } else {
            format!("当前独占格式：{detail}")
        };
        Some(
            div()
                .text_size(px(theme::FONT_CAPTION))
                .text_color(theme::text_muted())
                .child(text),
        )
    }
}

impl Render for SettingsPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let settings = self.settings.read(cx).settings();

        // `on_pick` 要求 `'static`，每个闭包各捕获一份句柄克隆，不能共用外层变量。
        let (for_appearance, for_density, for_view, for_recursive) = (
            self.settings.clone(),
            self.settings.clone(),
            self.settings.clone(),
            self.settings.clone(),
        );
        let (for_panel, for_sidebar, for_resume) = (
            self.settings.clone(),
            self.settings.clone(),
            self.settings.clone(),
        );
        let (for_smooth, for_output) = (self.settings.clone(), self.settings.clone());
        let (smooth_player, output_player) = (self.playback.clone(), self.playback.clone());
        let shell = self.shell.clone();
        let library = self.library.clone();

        let appearance: Picker = Rc::new(move |index: usize, cx: &mut App| {
            let value = if index == 0 {
                theme::Appearance::Light
            } else {
                theme::Appearance::Dark
            };
            for_appearance.update(cx, |this, cx| this.set_appearance(value, cx));
        });

        let density: Picker = Rc::new(move |index: usize, cx: &mut App| {
            let value = if index == 0 {
                Density::Compact
            } else {
                Density::Comfortable
            };
            // 既落盘供下次启动，也立刻应用到 shell，否则改完看不出变化。
            for_density.update(cx, |this, cx| this.set_density(value, cx));
            shell.update(cx, |shell, cx| shell.set_density(value, cx));
        });

        let view: Picker = Rc::new(move |index: usize, cx: &mut App| {
            let value = match index {
                1 => NavSection::Albums,
                2 => NavSection::Artists,
                _ => NavSection::Tracks,
            };
            for_view.update(cx, |this, cx| this.set_default_view(value, cx));
        });

        let recursive: Picker = Rc::new(move |index: usize, cx: &mut App| {
            let value = index == 0;
            for_recursive.update(cx, |this, cx| this.set_recursive_import(value, cx));
            library.update(cx, |library, cx| library.set_recursive(value, cx));
        });

        // 下面三项是启动时的偏好：改完不动当前界面，否则「收起右面板」会变成一条关不掉的设置。
        let panel: Picker = Rc::new(move |index: usize, cx: &mut App| {
            let value = match index {
                1 => Some(PanelTab::NowPlaying),
                2 => Some(PanelTab::Queue),
                _ => None,
            };
            for_panel.update(cx, |this, cx| this.set_default_panel(value, cx));
        });

        let sidebar: Picker = Rc::new(move |index: usize, cx: &mut App| {
            let value = match index {
                1 => Some(SidebarState::Expanded),
                2 => Some(SidebarState::Icons),
                _ => None,
            };
            for_sidebar.update(cx, |this, cx| this.set_default_sidebar(value, cx));
        });

        let resume: Picker = Rc::new(move |index: usize, cx: &mut App| {
            for_resume.update(cx, |this, cx| this.set_resume_position(index == 0, cx));
        });

        // 音频线程是另一条线程，落盘之外还得当场把新值送过去
        let smooth: Picker = Rc::new(move |index: usize, cx: &mut App| {
            let value = index == 0;
            for_smooth.update(cx, |this, cx| this.set_smooth_device_switch(value, cx));
            smooth_player.update(cx, |playback, _cx| playback.set_smooth_switch(value));
        });

        let output: Picker = Rc::new(move |index: usize, cx: &mut App| {
            let exclusive = index == 0;
            for_output.update(cx, |this, cx| this.set_exclusive_output(exclusive, cx));
            output_player.update(cx, |playback, _cx| playback.set_exclusive_output(exclusive));
        });

        // `center_width()` 已扣掉侧栏与右面板，两列的判断只读它。
        let (two_columns, back_inset) = {
            let shell = self.shell.read(cx);
            let width = shell.center_width();
            (
                width >= TWO_COL_MIN,
                shell.can_go_back() && width - 2.0 * theme::GAP_LG <= CONTENT_MAX_W,
            )
        };

        let appearance_card = card("Appearance", "外观").child(choice_row(
            window,
            cx,
            Glyph::Appearance,
            "主题",
            &[
                ("浅色", settings.appearance == theme::Appearance::Light),
                ("深色", settings.appearance == theme::Appearance::Dark),
            ],
            &appearance,
        ));
        let list_card = card("List", "列表").child(choice_row(
            window,
            cx,
            Glyph::List,
            "行密度",
            &[
                ("紧凑", settings.density == Density::Compact),
                ("舒适", settings.density == Density::Comfortable),
            ],
            &density,
        ));
        let startup_card = card("Startup", "启动")
            .child(choice_row(
                window,
                cx,
                Glyph::Home,
                "默认视图",
                &[
                    ("曲目", settings.default_view == NavSection::Tracks),
                    ("专辑", settings.default_view == NavSection::Albums),
                    ("艺术家", settings.default_view == NavSection::Artists),
                ],
                &view,
            ))
            .child(choice_row(
                window,
                cx,
                Glyph::PanelRight,
                "右面板",
                &[
                    ("收起", settings.default_panel.is_none()),
                    (
                        "现在播放",
                        settings.default_panel == Some(PanelTab::NowPlaying),
                    ),
                    ("播放队列", settings.default_panel == Some(PanelTab::Queue)),
                ],
                &panel,
            ))
            .child(choice_row(
                window,
                cx,
                Glyph::PanelLeft,
                "侧栏",
                &[
                    ("跟随宽度", settings.default_sidebar.is_none()),
                    (
                        "总是展开",
                        settings.default_sidebar == Some(SidebarState::Expanded),
                    ),
                    (
                        "只用图标",
                        settings.default_sidebar == Some(SidebarState::Icons),
                    ),
                ],
                &sidebar,
            ))
            .child(choice_row(
                window,
                cx,
                Glyph::Clock,
                "上次播放",
                &[
                    ("接着播", settings.resume_position),
                    ("每次从头", !settings.resume_position),
                ],
                &resume,
            ));
        let import_card = card("Import", "导入")
            .child(choice_row(
                window,
                cx,
                Glyph::Folder,
                "文件夹",
                &[
                    ("含子目录", settings.recursive_import),
                    ("仅顶层", !settings.recursive_import),
                ],
                &recursive,
            ))
            .child(roots_block(&self.library, window, cx));

        let audio_card = card("Audio", "音频")
            .child(choice_row(
                window,
                cx,
                Glyph::VolumeHigh,
                "输出模式",
                &[
                    ("独占", settings.exclusive_output),
                    ("共享", !settings.exclusive_output),
                ],
                &output,
            ))
            .child(choice_row(
                window,
                cx,
                Glyph::Devices,
                "切换输出设备",
                &[
                    ("平滑过渡", settings.smooth_device_switch),
                    ("直接切换", !settings.smooth_device_switch),
                ],
                &smooth,
            ))
            .children(self.output_detail(cx));

        let left = vec![appearance_card, list_card, startup_card];
        let right = vec![audio_card, import_card, data_section(&self.library, cx)];
        let choices = if two_columns {
            div()
                .w_full()
                .flex()
                .gap(px(theme::GAP_LG))
                .child(column(left))
                .child(column(right))
                .into_any_element()
        } else {
            let mut stacked = left;
            stacked.extend(right);
            column(stacked)
        };

        let body = div()
            .w_full()
            .flex()
            .flex_col()
            .gap(px(theme::GAP_LG))
            // 右边多留一档：滚动条叠在滚动区上，不避让会压在数字和路径上。
            .p(px(theme::PAGE_PAD))
            .child(choices)
            .child(shortcuts_section(two_columns, self.shortcuts_open, cx));

        // 内容比窗口高时会被裁掉且够不着，所以整块可滚；`track_scroll` 同时是滚动条读偏移量的来源。
        let scroll_area = div()
            .id("settings-scroll")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .track_scroll(&self.scroll)
            .child(
                div()
                    .flex()
                    .justify_center()
                    .w_full()
                    .child(div().w_full().max_w(px(CONTENT_MAX_W)).child(body)),
            );

        div()
            .flex_1()
            .min_w_0()
            .w_full()
            .h_full()
            .relative()
            .flex()
            .flex_col()
            // 不铺底色：铺一层不透明会把浮在左上角的返回按钮整个盖掉
            .child(header(back_inset))
            .child(scroll_area)
            .child(crate::common::scroll::scrollbar(
                &self.scroll,
                "settings-scrollbar",
            ))
    }
}

/// 宽到多少才排两列，窄一档回到单列。
const TWO_COL_MIN: f32 = 760.0;
/// 整块内容的上限宽度，再宽就居中留白。
const CONTENT_MAX_W: f32 = 1080.0;

fn card(english: &str, title: &str) -> Div {
    div()
        .relative()
        .w_full()
        .flex()
        .flex_col()
        .gap(px(theme::GAP_MD))
        .p(px(theme::GAP_LG))
        .pt(px(theme::GAP_LG + 4.))
        .bg(theme::window_base())
        .border_1()
        .border_color(theme::border())
        // 顶边一小段墨色页签，和首页的浏览看板同一种形状语言
        .child(
            div()
                .absolute()
                .top_0()
                .left_0()
                .h(px(5.))
                .w(px(48.))
                .bg(theme::text()),
        )
        .child(crate::common::section::eyebrow(english))
        .child(crate::common::section::section_heading(title))
}

/// `flex_1` + `min_w_0`：长路径与长标题不许把某一列顶宽。
fn column(cards: Vec<Div>) -> AnyElement {
    cards
        .into_iter()
        .fold(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap(px(theme::GAP_LG)),
            ParentElement::child,
        )
        .into_any_element()
}

/// 让位量与浮动返回按钮的位置同源于 `root::BACK_*`，两处不一致按钮就会压住标题。
fn header(back_inset: bool) -> impl IntoElement {
    // 返回键占一条 40px 高的顶行：眉标与它同高，大标题落到它下方，并与卡片共用页边线。
    let eyebrow_left = if back_inset {
        crate::root::HEADER_BACK_INSET - theme::PAGE_PAD + theme::GAP_SM
    } else {
        0.0
    };

    div().flex_none().w_full().flex().justify_center().child(
        div()
            .w_full()
            .max_w(px(CONTENT_MAX_W))
            .flex()
            .flex_col()
            .pt(px(if back_inset {
                crate::root::BACK_INSET_TOP
            } else {
                theme::GAP_XL
            }))
            .pb(px(theme::GAP_LG))
            .px(px(theme::PAGE_PAD))
            .child(
                div()
                    .h(px(if back_inset {
                        crate::root::BACK_BTN
                    } else {
                        theme::GAP_LG
                    }))
                    .flex()
                    .items_center()
                    .pl(px(eyebrow_left))
                    .child(crate::common::section::eyebrow("Config / 设置")),
            )
            .child(
                div()
                    .w_full()
                    .flex()
                    .items_end()
                    .justify_between()
                    .gap(px(theme::GAP_MD))
                    .child(crate::common::section::page_title("设置"))
                    .child(crate::common::section::field(concat!(
                        "v",
                        env!("CARGO_PKG_VERSION")
                    ))),
            ),
    )
}

/// 快捷键表里每组的小标题。用 `text_muted` 而不是 `text_faint`：它是一组行的抬头，
/// 比行文字还淡的话整块会先糊掉。
fn sub_title(text: &str) -> impl IntoElement {
    div()
        .font(theme::label_font())
        .text_size(px(theme::FONT_CAPTION))
        .font_weight(theme::WEIGHT_BUTTON)
        .text_color(theme::text_muted())
        .child(text.to_owned())
}

/// 一行设置：`title` + 一组互斥选项。`on_pick` 收到选项下标，由调用方映射成枚举。
/// 回调签名压成 `Picker`：到处写全 `Rc<dyn Fn…>` 已跨过 clippy 的类型复杂度上限。
type Picker = Rc<dyn Fn(usize, &mut App)>;

fn choice_row(
    window: &mut Window,
    cx: &mut Context<SettingsPanel>,
    glyph: Glyph,
    title: &str,
    options: &[(&str, bool)],
    on_pick: &Picker,
) -> impl IntoElement {
    div()
        .flex()
        .flex_col()
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(theme::GAP_SM))
                .child(icon(glyph, 15., theme::text_muted()))
                .child(
                    div()
                        .text_size(px(theme::FONT_BODY))
                        .text_color(theme::text())
                        .child(title.to_owned()),
                ),
        )
        .child(
            div()
                .flex()
                .child(segmented(window, cx, title, options, on_pick)),
        )
}

/// 无障碍状态由 `aria_selected` 与提示里的「（已选中）」表达。
/// 当前段用一块墨底在等宽分段之间滑动，文字层再盖在上面。
fn segmented(
    window: &mut Window,
    cx: &mut Context<SettingsPanel>,
    title: &str,
    options: &[(&str, bool)],
    on_pick: &Picker,
) -> Div {
    let gap = 2.0;
    let pad = 2.0;
    let active = options.iter().position(|(_, on)| *on).unwrap_or(0);
    // 中文字宽约等于字号；按最长一项定等宽，选项少时也不会忽宽忽窄。
    let max_chars = options
        .iter()
        .map(|(label, _)| label.chars().count())
        .max()
        .unwrap_or(1);
    let seg_w = max_chars as f32 * theme::FONT_CAPTION + 2.0 * theme::GAP_MD;
    let slide = anim::segment_slide(
        SharedString::from(format!("settings-seg-{title}")),
        active,
        window,
        cx,
    );

    let mut track = div()
        .mt(px(theme::GAP_MD))
        .relative()
        .flex()
        .gap(px(gap))
        .p(px(pad))
        .border_1()
        .border_color(theme::text())
        .bg(theme::bg())
        .child(
            div()
                .absolute()
                .top(px(pad))
                .bottom(px(pad))
                .left(px(pad + slide * (seg_w + gap)))
                .w(px(seg_w))
                .bg(theme::control()),
        );

    for (index, (label, active)) in options.iter().enumerate() {
        let label = (*label).to_owned();
        let active = *active;
        // 字色跟着墨块走：滑到哪一段，哪一段反白；只按逻辑选中切换会有一半时间白字压白底。
        let under = (slide - index as f32).abs() < 0.5;
        let on_pick = Rc::clone(on_pick);
        let a11y = format!("{title}：{label}{}", if active { "（已选中）" } else { "" });

        track = track.child(
            div()
                .id(SharedString::from(format!("choice-{title}-{index}")))
                .w(px(seg_w))
                .flex()
                .items_center()
                .justify_center()
                .py(px(5.))
                .tab_index(TAB_STOP)
                .focus_visible(theme::focus_ring)
                .role(Role::Button)
                .aria_selected(active)
                .text_size(px(theme::FONT_CAPTION))
                .font_weight(theme::WEIGHT_BUTTON)
                .text_color(if under {
                    theme::text_on_control()
                } else {
                    theme::text_muted()
                })
                .cursor_pointer()
                // 悬停只改字色：再铺一层底色会盖住滑过来的墨块
                .when(!under, |seg| {
                    seg.hover(|style| style.text_color(theme::text()))
                })
                .on_click(cx.listener(move |_, _, _, cx| on_pick(index, cx)))
                .tooltip(crate::common::tooltip::builder(a11y))
                .child(label),
        );
    }

    track
}

/// 一行「名字 + 值」的只读信息（快捷键表的每一行、独占格式那类短值）。
/// 名字吃剩余宽度、放不下就折行；值固定不收缩、靠右 —— 反过来做的话，长名字
/// （`后退一层：歌词 → 返回 → 面板 → 搜索`）会把按键挤成省略号，等于把要查的东西藏了。
/// 字号用 `FONT_CAPTION` 而不是 `FONT_MICRO`：10px 的中文在浅色底上糊成一片。
fn info_row(label: &str, value: impl Into<String>) -> impl IntoElement {
    div()
        .flex()
        .items_start()
        .justify_between()
        .gap(px(theme::GAP_MD))
        .text_size(px(theme::FONT_CAPTION))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .text_color(theme::text_muted())
                .child(label.to_owned()),
        )
        .child(
            div()
                .flex_none()
                .text_color(theme::text())
                .text_right()
                .child(value.into()),
        )
}

/// 一行「名字 + 长值」（数据目录那种路径）。与 [`info_row`] 相反：值必须能截断，
/// 否则一条 `/Users/…/Library/Application Support/…` 会把整张卡顶宽。
fn path_row(label: &str, value: String) -> impl IntoElement {
    div()
        .flex()
        .items_start()
        .justify_between()
        .gap(px(theme::GAP_MD))
        .text_size(px(theme::FONT_CAPTION))
        .child(
            div()
                .flex_none()
                .text_color(theme::text_muted())
                .child(label.to_owned()),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .text_color(theme::text())
                .text_right()
                .child(value),
        )
}

fn stat_cell(label: &str, value: String) -> Div {
    div()
        .flex_1()
        .min_w_0()
        .flex()
        .flex_col()
        .gap(px(2.))
        .child(
            div()
                .text_size(px(theme::FONT_SUBTITLE))
                .font_weight(theme::WEIGHT_TITLE)
                .text_color(theme::text())
                .child(value),
        )
        .child(
            // 数字下面那行说明也用 `FONT_CAPTION` + `text_muted`：10px 的淡灰在纸色底上
            // 只剩下一道影子，读不出这一格统计的是什么。
            div()
                .text_size(px(theme::FONT_CAPTION))
                .text_color(theme::text_muted())
                .child(label.to_owned()),
        )
}

fn shortcuts_section(two_columns: bool, open: bool, cx: &mut Context<SettingsPanel>) -> Div {
    let chevron = if open {
        Glyph::ChevronDown
    } else {
        Glyph::ChevronRight
    };
    let head = div()
        .id("shortcuts-toggle")
        .flex()
        .items_center()
        .gap(px(theme::GAP_SM))
        .cursor_pointer()
        .tab_index(TAB_STOP)
        .focus_visible(theme::focus_ring)
        .role(Role::Button)
        .aria_selected(open)
        .on_click(cx.listener(|this, _, _, cx| {
            this.shortcuts_open = !this.shortcuts_open;
            cx.notify();
        }))
        .tooltip(crate::common::tooltip::builder(if open {
            "收起快捷键"
        } else {
            "展开快捷键"
        }))
        .child(icon(chevron, 14., theme::text_muted()))
        .child(crate::common::section::section_heading("快捷键"));

    let mut card = div()
        .w_full()
        .flex()
        .flex_col()
        .gap(px(theme::GAP_MD))
        .p(px(theme::GAP_LG))
        .rounded(px(theme::RADIUS_CARD))
        .bg(theme::surface())
        .child(head);

    if !open {
        return card;
    }

    let mut groups = div()
        .w_full()
        .flex()
        .gap(px(theme::GAP_LG))
        .mt(px(theme::GAP_SM));
    if !two_columns {
        groups = groups.flex_col();
    }

    for (group, entries) in crate::shortcuts::GROUPS {
        let mut col = div()
            .flex_1()
            .min_w_0()
            .flex()
            .flex_col()
            .gap(px(theme::GAP_XS));
        col = col.child(sub_title(group));
        for entry in *entries {
            col = col.child(info_row(entry.label, pretty_keys(entry.keys)));
        }
        groups = groups.child(col);
    }

    card = card.child(groups);
    card
}

fn data_section(library: &Entity<LibraryEntity>, cx: &App) -> Div {
    let library = library.read(cx);
    let counts = [
        ("曲目", library.len()),
        ("专辑", library.album_count()),
        ("艺术家", library.artist_count()),
        ("已收藏", library.liked_count()),
        ("已关注", library.followed_count()),
        ("最近播放", library.recent_count()),
        ("歌单", library.collections().len()),
    ];

    let mut grid = div()
        .flex()
        .flex_col()
        .gap(px(theme::GAP_MD))
        .mt(px(theme::GAP_SM));
    for pair in counts.chunks(2) {
        let mut row = div().flex().gap(px(theme::GAP_MD));
        for &(label, count) in pair {
            row = row.child(stat_cell(label, thousands(count)));
        }
        // 奇数项时补一个空位，否则末格被 flex_1 拉成整行宽。
        if pair.len() == 1 {
            row = row.child(div().flex_1().min_w_0());
        }
        grid = grid.child(row);
    }

    let mut content = div().flex().flex_col().child(grid);
    if let Some(dir) = data_dir() {
        content = content.child(
            div()
                .mt(px(theme::GAP_MD))
                .child(path_row("数据目录", dir.display().to_string())),
        );
    }

    card("Data", "数据").child(content)
}

/// 把内部写法 `ctrl-up` 翻成界面上的 `Ctrl + ↑`，只翻展示层。
/// `keymap.rs` 绑定的仍是原始串，两边都从 `crate::shortcuts::keys` 取值。
/// 音乐文件夹：把「记住哪些目录」摆出来，并且能加、能重扫、能移除。
/// 曲库只存曲目，不存「这首是从哪个文件夹扫来的」，所以这层关系只能显式记一份根。
fn roots_block(
    library: &Entity<LibraryEntity>,
    window: &mut Window,
    cx: &mut Context<SettingsPanel>,
) -> Div {
    let (roots, counts, scanning) = {
        let state = library.read(cx);
        let roots = state.roots().to_vec();
        let mut counts = vec![0usize; roots.len()];
        for track in state.tracks() {
            if let Some(index) = roots.iter().position(|root| track.path.starts_with(root)) {
                counts[index] += 1;
            }
        }
        (roots, counts, state.scanning().is_some())
    };

    let mut list = div().flex().flex_col().gap(px(theme::GAP_SM));
    if roots.is_empty() {
        list = list.child(
            div()
                .text_size(px(theme::FONT_CAPTION))
                .text_color(theme::text_muted())
                .child("还没有音乐文件夹。添加之后，里面新加的歌下次启动会自动补进来。"),
        );
    }
    for (index, root) in roots.iter().enumerate() {
        let path = root.clone();
        let remove = text_button_animated(
            ("root-remove", index),
            "移除",
            ButtonKind::Ghost,
            window,
            cx,
            {
                let library = library.clone();
                move |_, _, cx| {
                    library.update(cx, |library, cx| library.forget_root(&path, cx));
                }
            },
        );

        list = list.child(
            div()
                .flex()
                .items_center()
                .gap(px(theme::GAP_MD))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .gap(px(2.))
                        .child(
                            div()
                                .min_w_0()
                                .truncate()
                                .text_size(px(theme::FONT_CAPTION))
                                .text_color(theme::text())
                                .child(root.display().to_string()),
                        )
                        // 移动硬盘没插、共享盘没挂载都会走到这里：不是错误，只是现在扫不到
                        .when(!root.is_dir(), |column| {
                            column.child(
                                div()
                                    .text_size(px(theme::FONT_MICRO))
                                    .text_color(theme::text_muted())
                                    .child("这个文件夹现在不在，接上之后重新扫描即可"),
                            )
                        }),
                )
                .child(
                    div()
                        .flex_none()
                        .font(theme::field_font())
                        .text_size(px(theme::FONT_FIELD_SIZE))
                        .text_color(theme::text_faint())
                        .child(format!("{} 首", thousands(counts[index]))),
                )
                .child(remove),
        );
    }

    div()
        .flex()
        .flex_col()
        .gap(px(theme::GAP_MD))
        .mt(px(theme::GAP_SM))
        .child(sub_title("音乐文件夹"))
        .child(list)
        .child(
            div()
                .flex()
                .gap(px(theme::GAP_SM))
                .child(text_button_animated(
                    "root-add",
                    "添加文件夹",
                    ButtonKind::Secondary,
                    window,
                    cx,
                    {
                        let library = library.clone();
                        move |_, _, cx| {
                            library.update(cx, LibraryEntity::open_dialog);
                        }
                    },
                ))
                .child(text_button_animated(
                    "root-rescan",
                    if scanning {
                        "正在扫描"
                    } else {
                        "重新扫描"
                    },
                    ButtonKind::Primary,
                    window,
                    cx,
                    {
                        let library = library.clone();
                        move |_, _, cx| {
                            library.update(cx, LibraryEntity::rescan);
                        }
                    },
                )),
        )
}

fn pretty_keys(raw: &str) -> String {
    raw.split('-')
        .map(|part| match part {
            "ctrl" => "Ctrl".to_owned(),
            "alt" => "Alt".to_owned(),
            "shift" => "Shift".to_owned(),
            "up" => "↑".to_owned(),
            "down" => "↓".to_owned(),
            "left" => "←".to_owned(),
            "right" => "→".to_owned(),
            "space" => "Space".to_owned(),
            "escape" => "Esc".to_owned(),
            "enter" => "Enter".to_owned(),
            "delete" => "Delete".to_owned(),
            "home" => "Home".to_owned(),
            "end" => "End".to_owned(),
            other if other.len() == 1 => other.to_uppercase(),
            other => other.to_owned(),
        })
        .collect::<Vec<_>>()
        .join(" + ")
}

/// 设置的持有者：改动一律经 `update_and_save` 落盘，绕过它就等于改了不保存。
pub struct SettingsEntity {
    settings: Settings,
}

impl SettingsEntity {
    pub fn new(settings: Settings, cx: &mut Context<Self>) -> Self {
        let _ = cx;
        Self { settings }
    }

    pub fn settings(&self) -> Settings {
        self.settings.clone()
    }

    fn update_and_save(&mut self, cx: &mut Context<Self>, edit: impl FnOnce(&mut Settings)) {
        edit(&mut self.settings);
        self.settings.save();
        cx.notify();
    }

    pub fn set_appearance(&mut self, appearance: theme::Appearance, cx: &mut Context<Self>) {
        self.update_and_save(cx, |settings| settings.appearance = appearance);
        // 两套明暗都要立刻生效：本项目 palette + 组件库 ThemeMode，再整窗重画。
        crate::platform::set_appearance(appearance, cx);
        cx.refresh_windows();
    }

    pub fn set_density(&mut self, density: Density, cx: &mut Context<Self>) {
        self.update_and_save(cx, |settings| settings.density = density);
    }

    pub fn set_default_view(&mut self, view: NavSection, cx: &mut Context<Self>) {
        self.update_and_save(cx, |settings| settings.default_view = view);
    }

    pub fn set_recursive_import(&mut self, recursive: bool, cx: &mut Context<Self>) {
        self.update_and_save(cx, |settings| settings.recursive_import = recursive);
    }

    pub fn set_default_panel(&mut self, panel: Option<PanelTab>, cx: &mut Context<Self>) {
        self.update_and_save(cx, |settings| settings.default_panel = panel);
    }

    pub fn set_default_sidebar(&mut self, sidebar: Option<SidebarState>, cx: &mut Context<Self>) {
        self.update_and_save(cx, |settings| settings.default_sidebar = sidebar);
    }

    pub fn set_resume_position(&mut self, resume: bool, cx: &mut Context<Self>) {
        self.update_and_save(cx, |settings| settings.resume_position = resume);
        // 关掉就把文件删了，否则重新打开会跳回一个几个月前的位置
        if !resume {
            crate::resume::Resume::clear();
        }
    }

    /// 音量与静音必须一起写：静音时 `volume` 存的仍是静音前的值，分开写会留下「取消静音回到 0」的存档。
    pub fn set_volume(&mut self, volume: f32, muted: bool, cx: &mut Context<Self>) {
        self.update_and_save(cx, |settings| {
            settings.volume = volume;
            settings.muted = muted;
        });
    }

    /// 记住输出设备。`None` 表示跟随系统默认。
    pub fn set_preferred_device(&mut self, device: Option<String>, cx: &mut Context<Self>) {
        self.update_and_save(cx, |settings| settings.preferred_device = device);
    }

    pub fn set_smooth_device_switch(&mut self, smooth: bool, cx: &mut Context<Self>) {
        self.update_and_save(cx, |settings| settings.smooth_device_switch = smooth);
    }

    pub fn set_exclusive_output(&mut self, exclusive: bool, cx: &mut Context<Self>) {
        self.update_and_save(cx, |settings| settings.exclusive_output = exclusive);
    }
}

#[cfg(test)]
mod tests {
    use super::pretty_keys;

    #[test]
    fn modifier_and_arrow_keys_read_as_ui_text() {
        assert_eq!(pretty_keys("ctrl-up"), "Ctrl + ↑");
        assert_eq!(pretty_keys("shift-left"), "Shift + ←");
        assert_eq!(pretty_keys("alt-right"), "Alt + →");
        assert_eq!(pretty_keys("space"), "Space");
        assert_eq!(pretty_keys("escape"), "Esc");
    }

    #[test]
    fn single_character_keys_survive_their_own_hyphen() {
        // `ctrl-,` 切出来是 ["ctrl", ","]，单字符要变大写而不是当键名查表
        assert_eq!(pretty_keys("ctrl-,"), "Ctrl + ,");
        assert_eq!(pretty_keys("/"), "/");
        assert_eq!(pretty_keys("m"), "M");
    }
}
