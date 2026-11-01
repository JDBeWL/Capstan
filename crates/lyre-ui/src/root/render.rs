//! 根视图的 `Render` 实现与元素树装配。
//! `read(cx)` 集中在 [`Frame::read`] 一次做完，其余函数只吃那份快照。

use std::time::Duration;

use crate::actions::*;
use crate::common::toast::ToastEntity;
use crate::components::VolumeControl;
use crate::entities::LibraryEntity;
use crate::icon::{Glyph, icon};
use crate::platform::*;
use crate::shell::{NavSection, PanelTab, ShellEntity, SidebarState};
use crate::theme;

use super::RootView;
use super::actions::control_has_focus;
use super::nav::{back_tooltip, floating_back, toggle_settings};

// 步进量的权威在 `shortcuts.rs`，设置页的键位表显示的是同一批数字
const SEEK_STEP: Duration = Duration::from_secs(crate::shortcuts::SEEK_SECONDS);
const SEEK_STEP_LONG: Duration = Duration::from_secs(crate::shortcuts::SEEK_LONG_SECONDS);
const VOLUME_STEP: f32 = crate::shortcuts::VOLUME_STEP;
const DROP_FADE: Duration = Duration::from_millis(120);

// 元素 id 与键位上下文是命中测试 / 键位绑定的键，写错不报错
/// 键位上下文名。权威在 `shortcuts.rs`，`lyre-app` 的 `keymap.rs` 读的是同一个常量。
const KEY_CONTEXT: &str = crate::shortcuts::CONTEXT;
/// 搜索页探索区的滚动容器。
const SEARCH_BROWSE_SCROLL_ID: &str = "search-browse-scroll";
/// 拖拽导入遮罩。
const DROP_HINT_ID: &str = "drop-hint";
/// 遮罩淡入动画的 id。与 [`DROP_HINT_ID`] 属两套 id 空间，字符串相同只是巧合。
const DROP_HINT_ANIM: &str = "drop-hint";

/// 一帧渲染要从各实体读出的快照。
/// 三个 `Entity` 按值存 —— `floating_back` 与元素树都要按值收句柄。
struct Frame {
    /// 右面板当前页签；`None` = 整块收起。
    right_panel: Option<PanelTab>,
    /// 右面板是否以浮层盖在主区上（窄档）。浮层不占同级列，改由 [`RootView::overlays`] 画。
    panel_overlay: bool,
    /// 歌词遮罩是否正盖在主区上。
    lyrics_open: bool,
    sidebar: SidebarState,
    nav: NavSection,
    /// 这一页画不画左上角的浮动返回按钮：只给「钻进去」的页面（作用域收窄、设置），一级区段走顶栏。
    /// 搜索页用顶栏的后退和 Esc 退出，不画浮动按钮。
    /// 判据是历史栈而不是作用域 —— 从网格进专辑时作用域收窄了，但历史里记着网格。
    show_back: bool,
    /// 返回按钮的悬停提示：取历史栈顶（退到哪去），不是当前作用域。
    back_hint: SharedString,
    /// 底部提示条距窗口底的距离，按当前底栏高度算（窄窗口下底栏是两行、更高）。
    toast_bottom: f32,
    /// 底栏高度。浮层右面板要停在底栏上沿。
    bottom_bar_h: f32,
    toast: Entity<ToastEntity>,
    library: Entity<LibraryEntity>,
    shell: Entity<ShellEntity>,
}

impl Frame {
    fn read(view: &RootView, cx: &App) -> Self {
        let shell = view.shell.read(cx);
        let right_panel = shell.right_panel();
        let panel_overlay = shell.right_panel_overlays();
        let lyrics_open = shell.shows_lyrics();
        let sidebar = shell.sidebar();
        let nav = shell.nav_section();
        let can_back = shell.can_go_back();
        // 搜索页不画浮动返回按钮：即使从艺术家里搜进来、作用域还收窄着，这一页也用顶栏和 Esc 退出。
        let show_back = can_back
            && nav != NavSection::Search
            && (view.library.read(cx).scope().is_scoped() || nav == NavSection::Settings);
        let bottom_bar_h = theme::bottom_bar_height(shell.stacks_bottom_bar());
        let toast_bottom = bottom_bar_h + theme::GAP_LG;
        let back_hint =
            SharedString::from(back_tooltip(shell.peek_history().map(
                |(section, scope)| (section, view.library.read(cx).label_for(&scope)),
            )));

        Self {
            right_panel,
            panel_overlay,
            lyrics_open,
            sidebar,
            nav,
            show_back,
            back_hint,
            toast_bottom,
            bottom_bar_h,
            toast: view.toast.clone(),
            library: view.library.clone(),
            shell: view.shell.clone(),
        }
    }
}

impl Render for RootView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let frame = Frame::read(self, cx);

        div()
            .key_context(KEY_CONTEXT)
            .track_focus(&self.focus)
            .on_action(cx.listener(|this, _: &TogglePlay, window, cx| {
                // gpui 内建会把 Enter / Space 转成聚焦控件的 click，这里再放一次就是一次按键干两件事
                if control_has_focus(window, cx) {
                    return;
                }
                this.toggle_play(cx);
            }))
            .on_action(cx.listener(|_this, _: &FocusNextControl, window, cx| {
                window.focus_next(cx);
            }))
            .on_action(cx.listener(|_this, _: &FocusPrevControl, window, cx| {
                window.focus_prev(cx);
            }))
            .on_action(cx.listener(|this, _: &NextTrack, _window, cx| {
                let step = this.library.update(cx, |library, _| library.step_next());
                this.apply_step(step, cx);
            }))
            .on_action(cx.listener(|this, _: &PrevTrack, _window, cx| {
                let step = this.library.update(cx, |library, _| library.step_prev());
                this.apply_step(step, cx);
            }))
            .on_action(cx.listener(|this, _: &SeekForward, _window, cx| {
                this.nudge_seek(SEEK_STEP.as_millis() as i64, cx);
            }))
            .on_action(cx.listener(|this, _: &SeekBackward, _window, cx| {
                this.nudge_seek(-(SEEK_STEP.as_millis() as i64), cx);
            }))
            .on_action(cx.listener(|this, _: &SeekForwardLong, _window, cx| {
                this.nudge_seek(SEEK_STEP_LONG.as_millis() as i64, cx);
            }))
            .on_action(cx.listener(|this, _: &SeekBackwardLong, _window, cx| {
                this.nudge_seek(-(SEEK_STEP_LONG.as_millis() as i64), cx);
            }))
            .on_action(cx.listener(|this, _: &VolumeUp, _window, cx| {
                this.volume
                    .update(cx, |volume, cx| volume.nudge(VOLUME_STEP, cx));
            }))
            .on_action(cx.listener(|this, _: &VolumeDown, _window, cx| {
                this.volume
                    .update(cx, |volume, cx| volume.nudge(-VOLUME_STEP, cx));
            }))
            .on_action(cx.listener(|this, _: &ToggleMute, _window, cx| {
                this.volume.update(cx, VolumeControl::toggle_mute);
            }))
            .on_action(cx.listener(|this, _: &CyclePlayMode, _window, cx| {
                this.library.update(cx, LibraryEntity::cycle_mode);
            }))
            .on_action(cx.listener(|this, _: &SelectNext, _window, cx| {
                this.library.update(cx, LibraryEntity::focus_next);
            }))
            .on_action(cx.listener(|this, _: &SelectPrev, _window, cx| {
                this.library.update(cx, LibraryEntity::focus_prev);
            }))
            .on_action(cx.listener(|this, _: &FocusFirst, _window, cx| {
                this.library.update(cx, LibraryEntity::focus_first);
            }))
            .on_action(cx.listener(|this, _: &FocusLast, _window, cx| {
                this.library.update(cx, LibraryEntity::focus_last);
            }))
            .on_action(cx.listener(|this, _: &PlayFocused, window, cx| {
                if control_has_focus(window, cx) {
                    return;
                }
                // play_focused 只发 Play 事件，装载由 on_play 订阅完成
                this.library.update(cx, |library, cx| {
                    let _ = library.play_focused(cx);
                });
            }))
            .on_action(cx.listener(|this, _: &RemoveFocused, _window, cx| {
                this.remove_focused(cx);
            }))
            .on_action(cx.listener(|this, _: &FocusSearch, window, cx| {
                // 聚焦的是顶栏那唯一的框，会触发 `SearchBoxEvent::Focused`，由订阅切到搜索页
                this.search
                    .update(cx, |search, cx| search.focus(window, cx));
            }))
            .on_action(cx.listener(|this, _: &Escape, window, cx| {
                this.escape(window, cx);
            }))
            .on_action(cx.listener(|this, _: &ToggleSettings, _window, cx| {
                let (shell, library) = (this.shell.clone(), this.library.clone());
                toggle_settings(&shell, &library, cx);
            }))
            .on_action(cx.listener(|this, _: &ToggleLyrics, _window, cx| {
                this.shell.update(cx, ShellEntity::toggle_lyrics);
            }))
            .on_action(cx.listener(|this, _: &ToggleShuffle, _window, cx| {
                this.library.update(cx, LibraryEntity::toggle_shuffle);
            }))
            // 按载荷类型筛：列表内拖拽排序也会置起 `active_drag`，
            // 用 `has_active_drag()` 判就会在排歌序时弹出导入遮罩
            .on_drag_move::<ExternalPaths>(cx.listener(
                |this, _: &DragMoveEvent<ExternalPaths>, _window, cx| {
                    if !this.drop_active {
                        this.drop_active = true;
                        cx.notify();
                    }
                },
            ))
            .on_file_drop_exit(cx.listener(|this, _event: &FileDropEvent, _window, cx| {
                if this.drop_active {
                    this.drop_active = false;
                    cx.notify();
                }
            }))
            // Submit 之前不一定有 Exited，这里自己清标志位，否则提示一直挂着
            .on_drop(cx.listener(|this, paths: &ExternalPaths, _window, cx| {
                this.drop_active = false;
                let paths = paths.paths().to_vec();
                this.library
                    .update(cx, |library, cx| library.import(paths, cx));
                cx.notify();
            }))
            .relative()
            .flex()
            .flex_col()
            .size_full()
            .bg(theme::window_base())
            .font(theme::ui_font())
            .text_color(theme::text())
            .child(self.top_bar.clone())
            .child(self.main_content(&frame, window, cx))
            .child(self.bottom_bar.clone())
            // 浮层排在最后：后画的在上面
            .children(self.overlays(&frame))
    }
}

impl RootView {
    /// 顶栏之下、底栏之上的主区：侧栏 + 中间白板 +（可选）右面板。
    /// 三块之间只用 1px 细线分区，没有缝、没有圆角卡片。
    fn main_content(
        &self,
        frame: &Frame,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        div()
            .flex()
            .flex_1()
            .min_h_0()
            .when(frame.sidebar != SidebarState::Hidden, |row| {
                row.child(self.sidebar.clone())
            })
            .child(self.center_card(frame, window, cx))
            .when_some(
                frame.right_panel.filter(|_| !frame.panel_overlay),
                |main, tab| main.child(self.right_panel(tab)),
            )
    }

    /// 中间那张白板：页面背景 + 三色条 +（可选）浮动返回按钮 + 当前区段内容 +（可选）歌词遮罩。
    fn center_card(
        &self,
        frame: &Frame,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        div()
            .flex_1()
            .min_w_0()
            .flex()
            .flex_col()
            .bg(theme::bg())
            .relative()
            // 排在最前面所以画在最底层，内容叠上去
            .child(crate::common::ornament::page_backdrop())
            .when(frame.show_back, |main| {
                main.child(floating_back(
                    frame.back_hint.clone(),
                    frame.shell.clone(),
                    frame.library.clone(),
                    window,
                    cx,
                ))
            })
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .flex_col()
                    .child(self.nav_content(frame, cx)),
            )
            // 屏幕开始的三色条：绝对定位，不占页头留白
            .child(
                div()
                    .absolute()
                    .top_0()
                    .left_0()
                    .right_0()
                    .child(crate::common::ornament::tri_strip_sweep()),
            )
            // 遮罩排在卡片内容最后：后画的在上面
            .when(frame.lyrics_open || self.lyrics_closing, |card| {
                card.child(self.lyrics_overlay(frame))
            })
    }

    fn lyrics_overlay(&self, frame: &Frame) -> AnyElement {
        let root = div()
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            .child(self.lyrics.clone());
        if self.lyrics_closing && !frame.lyrics_open {
            root.with_animation(
                "lyrics-out",
                Animation::new(Duration::from_millis(theme::MOTION_OVERLAY_OUT_MS))
                    .with_easing(theme::ease_std()),
                |el, delta| el.opacity(1.0 - delta),
            )
            .into_any_element()
        } else {
            root.into_any_element()
        }
    }

    /// 主区内容按当前区段分派。首页自己按块错开入场，其余页面走一次换页入场。
    fn nav_content(&self, frame: &Frame, cx: &mut Context<Self>) -> AnyElement {
        let content = match frame.nav {
            NavSection::Home => div()
                .flex_1()
                .min_h_0()
                .child(self.home.clone())
                .into_any_element(),
            NavSection::Search => {
                // 有歌曲命中时整页交给 TrackList：搜索摘要、工具条与列表共用一个滚动区；
                // 否则整页由探索区自己滚动
                let has_query = App::update_entity(cx, &self.library, |library, _| {
                    (library.has_query(), library.visible_len())
                });
                if has_query.0 && has_query.1 > 0 {
                    div()
                        .flex_1()
                        .min_h_0()
                        .child(self.track_list.clone())
                        .into_any_element()
                } else {
                    div()
                        .id(SEARCH_BROWSE_SCROLL_ID)
                        .flex_1()
                        .min_h_0()
                        .overflow_y_scroll()
                        .child(self.search_page.clone())
                        .into_any_element()
                }
            }
            NavSection::Tracks => div()
                .flex_1()
                .min_h_0()
                .child(self.track_list.clone())
                .into_any_element(),
            NavSection::Albums | NavSection::Artists => div()
                .flex_1()
                .min_h_0()
                .child(self.album_grid.clone())
                .into_any_element(),
            NavSection::Settings => div()
                .flex_1()
                .min_h_0()
                .child(self.settings_panel.clone())
                .into_any_element(),
        };

        if frame.nav == NavSection::Home {
            return content;
        }
        // 作用域也进 key：专辑 / 艺术家详情与「曲库」同为 Tracks，靠它区分换页。
        let scope = self.library.read(cx).scope().clone();
        crate::common::anim::page_enter(
            SharedString::from(format!("page-{:?}-{scope:?}", frame.nav)),
            div()
                .flex_1()
                .min_h_0()
                .min_w_0()
                .flex()
                .flex_col()
                .child(content),
        )
        .into_any_element()
    }

    /// 右侧面板那张卡片，按列停靠时用。
    fn right_panel(&self, tab: PanelTab) -> AnyElement {
        let panel = match tab {
            PanelTab::NowPlaying => self.now_playing.clone().into_any_element(),
            PanelTab::Queue => self.queue_panel.clone().into_any_element(),
        };
        // id 带页签名：切页签会重播，只改内容不重播
        let id = SharedString::from(format!("panel-{}", panel_id(tab)));
        crate::common::anim::enter(
            id,
            0,
            theme::MOTION_PANEL_IN_MS,
            8.0,
            div()
                .flex_none()
                .h_full()
                .w(px(theme::RIGHT_PANEL_W))
                .border_l_1()
                .border_color(theme::border())
                .bg(theme::bg())
                .child(panel),
        )
        .into_any_element()
    }

    /// 停靠面板收起时浮在右侧播 240ms 退出：曲子区域已经按“面板没了”重排，
    /// 不能再让旧面板占着 flex 列，否则网格列数会先变宽、内容被挤一下。
    fn closing_docked_panel(&self, tab: PanelTab, frame: &Frame) -> AnyElement {
        let panel = match tab {
            PanelTab::NowPlaying => self.now_playing.clone().into_any_element(),
            PanelTab::Queue => self.queue_panel.clone().into_any_element(),
        };
        div()
            .absolute()
            .top(px(theme::TOP_BAR_H))
            .bottom(px(frame.bottom_bar_h))
            .right_0()
            .w(px(theme::RIGHT_PANEL_W))
            .border_l_1()
            .border_color(theme::border())
            .bg(theme::bg())
            // 退出这 240ms 内别再让点击穿到已经展开的内容上
            .occlude()
            .child(panel)
            .with_animation(
                SharedString::from(format!("panel-dock-out-{}", panel_id(tab))),
                Animation::new(Duration::from_millis(theme::MOTION_PANEL_OUT_MS))
                    .with_easing(theme::ease_mech()),
                |el, delta| el.opacity(1.0 - delta).right(px(-24.0 * delta)),
            )
            .into_any_element()
    }

    /// 窄档的右面板：遮罩 + 贴右边的一列卡片，只盖顶栏与底栏之间的主区。
    fn right_panel_overlay(&self, tab: PanelTab, frame: &Frame, closing: bool) -> AnyElement {
        let panel = match tab {
            PanelTab::NowPlaying => self.now_playing.clone().into_any_element(),
            PanelTab::Queue => self.queue_panel.clone().into_any_element(),
        };
        let shell = self.shell.clone();
        let id = SharedString::from(format!("panel-overlay-{}", panel_id(tab)));

        let backdrop = div()
            .id("panel-overlay-backdrop")
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            .bg(theme::text().opacity(0.5))
            // 命中测试默认会连下层一起收：不拦住的话点遮罩会顺手点到底下的卡片。
            .occlude()
            .on_click(move |_, _, cx| {
                shell.update(cx, ShellEntity::close_panel);
            });

        let backdrop: AnyElement = if closing {
            backdrop
                .with_animation(
                    "panel-overlay-backdrop-out",
                    Animation::new(Duration::from_millis(theme::MOTION_OVERLAY_OUT_MS))
                        .with_easing(theme::ease_std()),
                    |el, delta| el.opacity(1.0 - delta),
                )
                .into_any_element()
        } else {
            backdrop
                .with_animation(
                    "panel-overlay-backdrop-in",
                    Animation::new(Duration::from_millis(200)).with_easing(theme::ease_std()),
                    Styled::opacity,
                )
                .into_any_element()
        };

        let card = div()
            .id(id.clone())
            .absolute()
            .top_0()
            .bottom_0()
            .right_0()
            .w(px(theme::RIGHT_PANEL_W))
            .border_l_1()
            .border_color(theme::border())
            .bg(theme::bg())
            .child(panel);

        let card: AnyElement = if closing {
            card.with_animation(
                SharedString::from(format!("panel-overlay-out-{}", panel_id(tab))),
                Animation::new(Duration::from_millis(theme::MOTION_PANEL_OUT_MS))
                    .with_easing(theme::ease_mech()),
                |el, delta| el.opacity(1.0 - delta).right(px(-24.0 * delta)),
            )
            .into_any_element()
        } else {
            // 从右侧推入：右侧内缩量从 -24 回到 0，透明度同时亮起。
            card.with_animation(
                id,
                Animation::new(Duration::from_millis(theme::MOTION_PAGE_MS))
                    .with_easing(theme::ease_mech()),
                |el, delta| el.opacity(delta).right(px(-24.0 * (1.0 - delta))),
            )
            .into_any_element()
        };

        div()
            .absolute()
            .top(px(theme::TOP_BAR_H))
            .bottom(px(frame.bottom_bar_h))
            .left_0()
            .right_0()
            .child(backdrop)
            .child(card)
            .into_any_element()
    }

    /// 浮在整棵树之上的三层：窄屏右面板浮层、拖拽导入遮罩、底部提示条。
    /// 返回 `Vec` 而不是包一层 `div()`：多一个 flex 子项会把根布局挤歪。
    fn overlays(&self, frame: &Frame) -> Vec<AnyElement> {
        let mut layers = Vec::new();
        let overlay_panel = frame
            .right_panel
            .filter(|_| frame.panel_overlay)
            .map(|tab| (tab, false))
            .or_else(|| {
                self.panel_closing
                    .filter(|_| self.panel_closing_overlay)
                    .map(|tab| (tab, true))
            });
        if let Some(tab) = self.panel_closing.filter(|_| !self.panel_closing_overlay) {
            layers.push(self.closing_docked_panel(tab, frame));
        }
        if let Some((tab, closing)) = overlay_panel {
            layers.push(self.right_panel_overlay(tab, frame, closing));
        }
        // 拖拽导入提示要盖在浮层之上
        if self.drop_active {
            layers.push(drop_hint().into_any_element());
        }
        layers.push(
            div()
                .absolute()
                .bottom(px(frame.toast_bottom))
                .left_0()
                .right_0()
                .flex()
                .justify_center()
                .child(frame.toast.clone())
                .into_any_element(),
        );
        layers
    }
}

/// 面板的元素 id 后缀。动画 id 靠它区分「换了页签」和「同一页签内容变了」。
fn panel_id(tab: PanelTab) -> &'static str {
    match tab {
        PanelTab::NowPlaying => "now-playing",
        PanelTab::Queue => "queue",
    }
}

/// 只在拖拽期间存在于元素树：常驻但靠透明度隐藏的遮罩仍会吃掉整窗点击（透明度不参与命中测试）。
fn drop_hint() -> impl IntoElement {
    div()
        .id(DROP_HINT_ID)
        .absolute()
        .top_0()
        .left_0()
        .size_full()
        .flex()
        .items_center()
        .justify_center()
        .bg(theme::text().opacity(0.85))
        .child(
            div()
                .relative()
                .flex_none()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap(px(theme::GAP_MD))
                .w(px(420.))
                .h(px(240.))
                .bg(theme::panel())
                .child(crate::common::ornament::cut_br(
                    theme::CUT_LG,
                    theme::text().opacity(0.85),
                ))
                .child(
                    div()
                        .absolute()
                        .size_full()
                        .child(crate::common::ornament::hatch_fill(
                            theme::text().opacity(0.04),
                        )),
                )
                .child(icon(Glyph::Import, 40., theme::text()))
                .child(
                    div()
                        .text_size(px(theme::FONT_SUBTITLE))
                        .font_weight(theme::WEIGHT_TITLE)
                        .text_color(theme::text())
                        .child("松手导入音频文件或文件夹"),
                )
                .child(
                    div()
                        .text_size(px(theme::FONT_CAPTION))
                        .text_color(theme::text_muted())
                        .child("支持 MP3 / FLAC / M4A / WAV / OGG / Opus 等"),
                ),
        )
        .with_animation(
            DROP_HINT_ANIM,
            Animation::new(DROP_FADE).with_easing(theme::ease_mech()),
            Styled::opacity,
        )
}
