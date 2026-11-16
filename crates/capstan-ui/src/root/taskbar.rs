//! 任务栏联动：窗口标题、缩略图封面、缩略图按钮与点击分发。
//! 窗口过程把点击放进队列，这里用定时泵搬到 UI 线程；Windows 之外整体是空实现。

use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::Duration;

use capstan_core::Track;
use capstan_taskbar::{Art, Labels, Taskbar, TaskbarAction};

use crate::cover::{CoverKey, MAX_BUCKET};
use crate::icon::Glyph;
use crate::platform::*;

use super::RootView;

const ACTION_POLL: Duration = Duration::from_millis(50);
const APP_TITLE: &str = "Capstan";
const FALLBACK_PNG: &[u8] = include_bytes!("../../assets/brand/capstan-mark-128.png");

impl RootView {
    /// 挂上任务栏集成。失败只影响缩略图，不影响窗口与播放。
    pub(super) fn attach_taskbar(&mut self, window: &Window, cx: &mut Context<Self>) {
        let Some(hwnd) = window_hwnd(window) else {
            return;
        };
        let labels = Labels {
            prev: Glyph::SkipBack.label(),
            play: Glyph::Play.label(),
            pause: Glyph::Pause.label(),
            next: Glyph::SkipForward.label(),
            like: Glyph::Heart.label(),
            unlike: Glyph::HeartFilled.label(),
        };
        let Some(taskbar) = Taskbar::attach(hwnd, labels) else {
            log::warn!("任务栏集成不可用，缩略图继续显示实时窗口");
            return;
        };
        taskbar.set_art(fallback_art());
        taskbar.set_track(APP_TITLE, false);

        let actions = taskbar.actions();
        self.taskbar = Some(taskbar);
        self.spawn_taskbar_pump(actions, cx);
    }

    #[allow(clippy::used_underscore_binding)]
    fn spawn_taskbar_pump(
        &mut self,
        actions: Arc<Mutex<Vec<TaskbarAction>>>,
        cx: &mut Context<Self>,
    ) {
        let executor = cx.background_executor().clone();
        self._taskbar_pump = Some(cx.spawn(async move |this, cx| {
            loop {
                executor.timer(ACTION_POLL).await;
                let pending: Vec<TaskbarAction> = {
                    let mut queue = actions.lock().unwrap_or_else(PoisonError::into_inner);
                    queue.drain(..).collect()
                };
                if pending.is_empty() {
                    continue;
                }
                if this
                    .update(cx, |view, cx| view.handle_taskbar_actions(pending, cx))
                    .is_err()
                {
                    break;
                }
            }
        }));
    }

    fn handle_taskbar_actions(&mut self, actions: Vec<TaskbarAction>, cx: &mut Context<Self>) {
        for action in actions {
            match action {
                TaskbarAction::Prev => {
                    let step = self.library.update(cx, |library, _| library.step_prev());
                    self.apply_step(step, cx);
                }
                TaskbarAction::Next => {
                    let step = self.library.update(cx, |library, _| library.step_next());
                    self.apply_step(step, cx);
                }
                TaskbarAction::TogglePlay => self.toggle_play(cx),
                TaskbarAction::ToggleLike => {
                    let Some(id) = self.playback.read(cx).current().map(|track| track.id) else {
                        continue;
                    };
                    self.library
                        .update(cx, |library, cx| library.toggle_like(id, cx));
                    self.sync_taskbar_like(cx);
                }
            }
        }
    }

    /// 换曲后同步标题、按钮可用状态、收藏态与缩略图。
    pub(super) fn sync_taskbar_track(&mut self, cx: &mut Context<Self>) {
        let current = self.playback.read(cx).current().cloned();
        if let Some(taskbar) = &self.taskbar {
            if let Some(track) = &current {
                taskbar.set_track(&track_title(track), true);
                taskbar.set_art(fallback_art());
            } else {
                taskbar.set_track(APP_TITLE, false);
                taskbar.set_art(fallback_art());
            }
        }
        if let Some(track) = &current {
            self.sync_taskbar_cover(track, cx);
        }
        self.sync_taskbar_like(cx);
    }

    pub(super) fn sync_taskbar_state(&mut self, cx: &App) {
        if let Some(taskbar) = &self.taskbar {
            taskbar.set_playing(self.playback.read(cx).is_playing());
        }
    }

    pub(super) fn sync_taskbar_like(&mut self, cx: &App) {
        let Some(taskbar) = &self.taskbar else {
            return;
        };
        let liked = self
            .playback
            .read(cx)
            .current()
            .is_some_and(|track| self.library.read(cx).is_liked(track.id));
        taskbar.set_liked(liked);
    }

    /// 缓存里已有最高档就直接用，没有就请求；请求期间先挂着品牌标。
    fn sync_taskbar_cover(&mut self, track: &Track, cx: &mut Context<Self>) {
        let key = CoverKey::Track(track.id, MAX_BUCKET);
        let cached = self.covers.read(cx).ready_key(key);
        if let Some(image) = cached {
            if let Some(taskbar) = &self.taskbar {
                taskbar.set_art(art_from_image(&image));
            }
            return;
        }

        let missing = self.covers.read(cx).is_missing(track.id, MAX_BUCKET as f32);
        if missing {
            return;
        }
        let path = track.path.clone();
        self.covers.update(cx, |covers, cx| {
            covers.request(track.id, path, MAX_BUCKET as f32, cx);
        });
    }

    /// 封面解码完成。只认最高档与当前曲，迟到的旧曲事件直接丢弃。
    pub(super) fn on_cover_ready(&mut self, key: CoverKey, cx: &App) {
        if key.bucket() != MAX_BUCKET {
            return;
        }
        let current = self.playback.read(cx).current().map(|track| track.id);
        if current != Some(key.track_id()) {
            return;
        }
        let Some(image) = self.covers.read(cx).ready_key(key) else {
            return;
        };
        if let Some(taskbar) = &self.taskbar {
            taskbar.set_art(art_from_image(&image));
        }
    }

    /// 窗口标题与缩略图头部共用的名字。
    pub(super) fn taskbar_title(&self, cx: &App) -> String {
        self.playback
            .read(cx)
            .current()
            .map_or_else(|| APP_TITLE.to_owned(), track_title)
    }
}

fn track_title(track: &Track) -> String {
    match track.artist.as_deref().filter(|artist| !artist.is_empty()) {
        Some(artist) => format!("{} - {artist}", track.title),
        None => track.title.clone(),
    }
}

fn art_from_image(image: &RenderImage) -> Option<Art> {
    let bytes = image.as_bytes(0)?;
    let size = image.size(0);
    Some(Art {
        width: u32::try_from(size.width.0).ok()?,
        height: u32::try_from(size.height.0).ok()?,
        bgra: bytes.to_vec(),
    })
}

/// 无曲目或没有封面时的缩略图。PNG 只解一次，之后克隆字节。
fn fallback_art() -> Option<Art> {
    static FALLBACK: OnceLock<Option<Art>> = OnceLock::new();
    FALLBACK
        .get_or_init(|| {
            let image = image::load_from_memory(FALLBACK_PNG).ok()?.to_rgba8();
            let (width, height) = image.dimensions();
            let mut bgra = image.into_raw();
            for pixel in bgra.as_chunks_mut::<4>().0 {
                pixel.swap(0, 2);
            }
            Some(Art {
                width,
                height,
                bgra,
            })
        })
        .clone()
}
