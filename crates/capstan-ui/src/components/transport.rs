//! 播放控制栏，含上一曲、播放暂停与下一曲。
//! 只 `subscribe` 语义事件、不 `observe`。进度每 100ms 变一次，observe 会让按钮每秒重绘十次。

use crate::entities::{PlaybackEntity, PlaybackEvent};
use crate::icon::{Glyph, icon};
use crate::platform::*;
use crate::theme;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TransportEvent {
    TogglePlay,
    Prev,
    Next,
}

pub struct Transport {
    playback: Entity<PlaybackEntity>,
    playing: bool,
    has_track: bool,
    _sub: Subscription,
}

impl EventEmitter<TransportEvent> for Transport {}

impl Transport {
    pub fn new(playback: Entity<PlaybackEntity>, cx: &mut Context<Self>) -> Self {
        let (playing, has_track) = {
            let playback = playback.read(cx);
            (playback.is_playing(), playback.current().is_some())
        };

        let subscription = cx.subscribe(
            &playback,
            |this, _, event: &PlaybackEvent, cx| match event {
                PlaybackEvent::StateChanged
                | PlaybackEvent::TrackChanged
                | PlaybackEvent::Restored => {
                    let playback = this.playback.read(cx);
                    this.playing = playback.is_playing();
                    this.has_track = playback.current().is_some();
                    cx.notify();
                }
                // `Finished` 由上层推进队列、设备列表只有底栏的设备菜单关心，失败与降级由 `RootView` 弹提示，这里都不处理。
                PlaybackEvent::Finished
                | PlaybackEvent::VolumeChanged
                | PlaybackEvent::DevicesChanged
                | PlaybackEvent::OutputModeChanged
                | PlaybackEvent::Failed(_)
                | PlaybackEvent::Notice(_) => {}
            },
        );

        Self {
            playback,
            playing,
            has_track,
            _sub: subscription,
        }
    }
}

impl Render for Transport {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let enabled = self.has_track;
        let prev_hover =
            enabled.then(|| crate::common::hover::progress("transport-prev", window, cx));
        let play_hover =
            enabled.then(|| crate::common::hover::progress("transport-play", window, cx));
        let next_hover =
            enabled.then(|| crate::common::hover::progress("transport-next", window, cx));

        // 三个按钮共享的外观壳子。tooltip 同时充当悬停提示和无障碍名称。
        let round = |id: &'static str, size: f32, hover: Option<f32>, fill: Rgba| {
            let hover_id = ElementId::from(id);
            let mut button = div()
                .id(id)
                .relative()
                .flex_none()
                .size(px(size))
                .flex()
                .items_center()
                .justify_center()
                .child(
                    div()
                        .absolute()
                        .left_0()
                        .top_0()
                        .bottom_0()
                        .w(relative(hover.unwrap_or(0.0)))
                        .bg(fill),
                )
                .child(crate::common::ornament::cut_br(
                    theme::CUT_SM,
                    theme::chrome(),
                ));
            if hover.is_some() {
                button = button.on_hover(crate::common::hover::bind(hover_id));
            }
            button
        };

        let mut prev = round("transport-prev", 32., prev_hover, theme::elevated()).child(icon(
            Glyph::SkipBack,
            16.,
            if enabled {
                theme::text_muted()
            } else {
                theme::text_faint()
            },
        ));
        // 柠檬黄实心底配近黑三角，同官网播放钮。黑压黄远超 AA，反过来黄三角在浅底上太弱。
        let mut play = round("transport-play", 40., play_hover, theme::text_muted())
            .bg(if enabled {
                theme::control()
            } else {
                theme::track()
            })
            .child(icon(
                if self.playing {
                    Glyph::Pause
                } else {
                    Glyph::Play
                },
                if self.playing { 15. } else { 17. },
                if enabled {
                    theme::text_on_control()
                } else {
                    // 禁用态图标必须用 `text()`，`text_faint` 压 `track()` 底达不到 AA。
                    theme::text()
                },
            ));
        let mut next = round("transport-next", 32., next_hover, theme::elevated()).child(icon(
            Glyph::SkipForward,
            16.,
            if enabled {
                theme::text_muted()
            } else {
                theme::text_faint()
            },
        ));

        if enabled {
            prev = prev
                .cursor_pointer()
                .hover(|style| style.text_color(theme::text()))
                .on_click(cx.listener(|_this, _: &ClickEvent, _window, cx| {
                    cx.emit(TransportEvent::Prev);
                }))
                .tooltip(crate::common::tooltip::builder("上一曲"));
            // 播放键的事件只看 `enabled`，不看 `playing`。
            play = play
                .cursor_pointer()
                .on_click(cx.listener(|_this, _: &ClickEvent, _window, cx| {
                    cx.emit(TransportEvent::TogglePlay);
                }))
                .tooltip(crate::common::tooltip::builder(if self.playing {
                    "暂停"
                } else {
                    "播放"
                }));
            next = next
                .cursor_pointer()
                .hover(|style| style.text_color(theme::text()))
                .on_click(cx.listener(|_this, _: &ClickEvent, _window, cx| {
                    cx.emit(TransportEvent::Next);
                }))
                .tooltip(crate::common::tooltip::builder("下一曲"));
        } else {
            // 禁用态也要给读屏用户一个说明。按钮存在，只是还不可用。
            play = play
                .cursor_default()
                .tooltip(crate::common::tooltip::builder("先导入音乐才能播放"));
        }

        div()
            .flex()
            .items_center()
            .gap_3()
            .child(prev)
            .child(play)
            .child(next)
    }
}
