//! 根视图上的动作。一次用户意图落到多个实体上，这里不持有状态。

use std::time::Duration;

use capstan_analysis::{ArrangeItem, PlanContext, arrange_queue};
use capstan_audio::rodio_backend::MIN_LEAD_SHARED;
use capstan_audio::{NextTrack, OutputMode};
use capstan_audio_exclusive::MIN_LEAD_EXCLUSIVE;
use capstan_core::{CollectionId, SortKey, TrackId};

use crate::components::mix_editor::same_album_consecutive;
use crate::entities::{AnalysisPriority, LibraryEntity, PlaybackEntity, PlaybackState, Step};
use crate::platform::*;

use super::RootView;

/// 键盘焦点是否落在可 Tab 到的控件上。gpui 内建会将 Enter / Space 转为聚焦控件自身的 `on_click`，
/// 而这两个键已绑定播放动作；鼠标点击按钮同样留下焦点，故还需判定「上一次输入为键盘」。
pub(super) fn control_has_focus(window: &Window, cx: &App) -> bool {
    window.last_input_was_keyboard() && window.focused(cx).is_some_and(|handle| handle.tab_stop)
}

impl RootView {
    /// 什么都没装载时音频线程会忽略 `PlayerCommand::TogglePlay`，所以这里先送一首进播放器。
    /// 队列走到头后也要补回 `now_playing`，否则列表一行都不亮。
    pub(super) fn toggle_play(&mut self, cx: &mut Context<Self>) {
        let (current, stopped) = {
            let playback = self.playback.read(cx);
            (
                playback.current().map(|track| track.id),
                playback.state() == PlaybackState::Stopped,
            )
        };

        if let Some(id) = current {
            if stopped {
                self.library
                    .update(cx, |library, cx| library.set_now_playing(Some(id), cx));
            }
            self.playback.update(cx, PlaybackEntity::toggle_play);
            return;
        }

        // 什么都没装载时不能走「光标那一首」，光标完全可能停在一首引擎播不了的曲目上
        // （列表里它们照常显示、置灰），那时按空格只该得到一条提示。
        self.library.update(cx, LibraryEntity::play_first_playable);
    }

    /// 队列推进时同步 `now_playing`，否则高亮停在上一首。
    pub(super) fn apply_step(&mut self, step: Step, cx: &mut Context<Self>) {
        match step {
            Step::Play(track) => {
                let id = track.id;
                self.library
                    .update(cx, |library, cx| library.set_now_playing(Some(id), cx));
                self.playback
                    .update(cx, |playback, cx| playback.load(track, true, cx));
            }
            Step::Restart => {
                self.playback.update(cx, |playback, _| playback.restart());
            }
            Step::Stop => {
                self.library
                    .update(cx, |library, cx| library.set_now_playing(None, cx));
                self.playback.update(cx, PlaybackEntity::stop);
            }
        }
    }

    /// 让音频线程预载下一首。下一首的口径必须与自然播完一致，故取 `peek_on_end`。
    /// 仅在 `(当前曲, 下一曲)` 变化时下发；槽位替换不影响正在播放的段。
    pub(super) fn sync_preload(&mut self, cx: &mut Context<Self>) {
        self.sync_preload_inner(false, cx);
    }

    /// 链路重建后重新下发预载，即使下一首的 id 未变。seek、换设备与换输出模式
    /// 都会重建，新链路上没有预载。
    pub(super) fn refresh_preload(&mut self, cx: &mut Context<Self>) {
        self.sync_preload_inner(true, cx);
    }

    fn sync_preload_inner(&mut self, force: bool, cx: &mut Context<Self>) {
        if force {
            self.preload = None;
        }

        let (state, current, position, output_mode) = {
            let playback = self.playback.read(cx);
            (
                playback.state(),
                playback.current().cloned(),
                playback.position(),
                playback.output_mode(),
            )
        };

        let next = if current.is_some()
            && matches!(state, PlaybackState::Playing | PlaybackState::Paused)
        {
            self.library.read(cx).peek_on_end()
        } else {
            None
        };

        let prefs = self.settings.read(cx).settings().auto_mix;

        // 仅在启用自动混音时按需投递当前曲与接下来两首的分析任务。
        if prefs.enabled
            && let Some(after) = current.as_ref()
        {
            let upcoming = self.library.read(cx).upcoming(3);
            let analysis = self.analysis.read(cx);
            analysis.request(after, AnalysisPriority::High);
            for track in upcoming.iter().skip(1) {
                analysis.request(track, AnalysisPriority::Medium);
            }
            if let Some(next_track) = next.as_ref() {
                analysis.request(next_track, AnalysisPriority::Medium);
            }
        }

        let next_payload = match (current.as_ref(), next.as_ref()) {
            (Some(after), Some(next_track)) => {
                // 单曲循环（下一首仍是同一曲目）不触发混音，保持无缝直连。
                let plan = if prefs.enabled && next_track.id != after.id {
                    let (an_a, an_b) = {
                        let analysis = self.analysis.read(cx);
                        (analysis.get(after.id), analysis.get(next_track.id))
                    };
                    if let (Some(an_a), Some(an_b)) = (an_a, an_b) {
                        let override_plan = self
                            .overrides
                            .read(cx)
                            .get(after.id, next_track.id)
                            .cloned();
                        let min_lead = match output_mode {
                            OutputMode::Shared => MIN_LEAD_SHARED,
                            OutputMode::Exclusive => MIN_LEAD_EXCLUSIVE,
                        };
                        let ctx = PlanContext {
                            current_position: position,
                            same_album_consecutive: same_album_consecutive(after, next_track),
                            min_lead,
                        };
                        Some(capstan_analysis::plan(
                            &an_a,
                            &an_b,
                            &prefs,
                            override_plan.as_ref(),
                            ctx,
                        ))
                    } else {
                        None
                    }
                } else {
                    None
                };
                Some(NextTrack {
                    path: next_track.path.clone(),
                    mix: plan,
                })
            }
            _ => None,
        };

        let fingerprint = next_payload
            .as_ref()
            .and_then(|n| n.mix.as_ref())
            .map(capstan_audio::MixPlan::fingerprint);
        let key = current
            .as_ref()
            .map(|track| (track.id, next.as_ref().map(|n| n.id), fingerprint));
        if self.preload == key {
            return;
        }
        self.preload = key;

        let Some(after) = current else {
            return;
        };
        self.playback.update(cx, |playback, _| {
            playback.preload_next(&after.path, next_payload);
        });
    }

    /// 分析完成时同步 BPM 到曲库，按需刷新 BPM 排序、智能混排待播队列与混音预载。
    pub(super) fn on_analysis_ready(&mut self, id: TrackId, cx: &mut Context<Self>) {
        if let Some(bpm) = self.analysis.read(cx).get(id).map(|a| a.bpm) {
            let changed = self
                .library
                .update(cx, |library, cx| library.set_analyzed_bpm(id, bpm, cx));
            if changed && self.library.read(cx).sort_key() == SortKey::Bpm {
                self.schedule_bpm_resort(cx);
            }
        }

        let in_upcoming_smart_mix = {
            let library = self.library.read(cx);
            if library.is_smart_mix() {
                let order = library.playlist().order();
                let cursor = library.playlist().cursor();
                order.get(cursor..).is_some_and(|tail| tail.contains(&id))
            } else {
                false
            }
        };
        if in_upcoming_smart_mix {
            self.schedule_smart_mix_refresh(cx);
        }

        let current_id = self.playback.read(cx).current().map(|t| t.id);
        let next_id = self.library.read(cx).peek_on_end().map(|t| t.id);
        if current_id == Some(id) || next_id == Some(id) {
            self.refresh_preload(cx);
        }
    }

    /// 磁盘分析缓存批量恢复完成，一次性写入 BPM、同步当前排序与智能混排，并补派未分析曲目。
    pub(super) fn on_analysis_hydrated(&mut self, bpms: &[(TrackId, f32)], cx: &mut Context<Self>) {
        if !bpms.is_empty() {
            let changed = self.library.update(cx, |library, cx| {
                library.set_analyzed_bpms(bpms.iter().copied(), cx)
            });
            if changed && self.library.read(cx).sort_key() == SortKey::Bpm {
                self.library.update(cx, LibraryEntity::apply_current_sort);
            }
        }
        if self.library.read(cx).is_smart_mix() {
            self.arrange_smart_mix(false, cx);
        }
        self.sync_analysis_demand(cx);
        self.refresh_preload(cx);
    }

    /// 根据设置、BPM 排序与智能混排状态，动态开启后台分析并按需投递未分析曲目。
    pub(super) fn sync_analysis_demand(&self, cx: &App) {
        let prefs = self.settings.read(cx).settings().auto_mix;
        let (sort_bpm, smart_mix) = {
            let library = self.library.read(cx);
            (library.sort_key() == SortKey::Bpm, library.is_smart_mix())
        };
        let want_background = (prefs.enabled && prefs.background_analyze) || sort_bpm || smart_mix;
        let analysis = self.analysis.read(cx);
        analysis.set_background_enabled(want_background);

        if smart_mix {
            // 智能混排优先高快分析当前播放队列中的未分析曲目。
            let queue_tracks: Vec<_> = {
                let library = self.library.read(cx);
                let playlist = library.playlist();
                playlist
                    .order()
                    .iter()
                    .filter_map(|id| playlist.by_id(*id).cloned())
                    .collect()
            };
            let fast_cap = queue_tracks.len().min(120);
            analysis.request_batch(&queue_tracks[..fast_cap], AnalysisPriority::Medium);
            if queue_tracks.len() > fast_cap {
                analysis.request_batch(&queue_tracks[fast_cap..], AnalysisPriority::Low);
            }
        }

        if (prefs.enabled && prefs.background_analyze) || sort_bpm {
            self.enqueue_background_analysis(cx);
        }
    }

    /// 将整库可播曲目按低优先级投递给分析线程。
    pub(super) fn enqueue_background_analysis(&self, cx: &App) {
        let tracks: Vec<_> = self.library.read(cx).playlist().tracks().to_vec();
        self.analysis
            .read(cx)
            .request_batch(&tracks, AnalysisPriority::Low);
    }

    /// 结合分析结果与标签元数据主动编排队列。`upcoming_only` 为真时锁住已播与当前曲，只重排后续待播段。
    pub(super) fn arrange_smart_mix(&mut self, upcoming_only: bool, cx: &mut Context<Self>) {
        let (order, cursor, playing, seed) = {
            let library = self.library.read(cx);
            if !library.is_smart_mix() {
                return;
            }
            let playlist = library.playlist();
            (
                playlist.order().to_vec(),
                playlist.cursor(),
                library.now_playing(),
                library.seed(),
            )
        };
        if order.len() <= 1 {
            return;
        }

        let (prefix, target_ids, anchor) =
            if upcoming_only && playing.is_some() && cursor < order.len() {
                (&order[..cursor], &order[cursor..], Some(order[cursor]))
            } else {
                let anchor = playing.filter(|id| order.contains(id));
                (&order[..0], &order[..], anchor)
            };

        let items: Vec<ArrangeItem> = {
            let library = self.library.read(cx);
            let playlist = library.playlist();
            let analysis = self.analysis.read(cx);
            target_ids
                .iter()
                .filter_map(|&id| {
                    let track = playlist.by_id(id)?;
                    Some(match analysis.get(id) {
                        Some(info) => ArrangeItem::from_analysis(
                            id,
                            &info,
                            track.artist.as_deref(),
                            track.album.as_deref(),
                        ),
                        None => ArrangeItem::from_metadata(
                            id,
                            track.bpm_tag,
                            track.artist.as_deref(),
                            track.album.as_deref(),
                        ),
                    })
                })
                .collect()
        };

        let mut next_order = Vec::with_capacity(order.len());
        next_order.extend_from_slice(prefix);
        next_order.extend(arrange_queue(&items, anchor, seed));

        if next_order != order {
            self.library.update(cx, |library, cx| {
                library.apply_smart_mix_order(next_order, cx);
            });
        }
    }

    /// 一次性按 BPM、调性与能量重排指定自建歌单的持久化成员顺序，并为尚未分析的曲目排入中优先级分析。
    pub(super) fn smart_reorder_collection(&mut self, id: CollectionId, cx: &mut Context<Self>) {
        let Some((name, members, seed)) = ({
            let library = self.library.read(cx);
            library
                .collection_members(id)
                .map(|(name, members)| (name, members, library.seed()))
        }) else {
            return;
        };

        let (items, missing): (Vec<ArrangeItem>, Vec<capstan_core::Track>) = {
            let library = self.library.read(cx);
            let playlist = library.playlist();
            let analysis = self.analysis.read(cx);
            let mut items = Vec::with_capacity(members.len());
            let mut missing = Vec::new();
            for member_id in members {
                let Some(track) = playlist.by_id(member_id) else {
                    continue;
                };
                if let Some(info) = analysis.get(member_id) {
                    items.push(ArrangeItem::from_analysis(
                        member_id,
                        &info,
                        track.artist.as_deref(),
                        track.album.as_deref(),
                    ));
                } else {
                    missing.push(track.clone());
                    items.push(ArrangeItem::from_metadata(
                        member_id,
                        track.bpm_tag,
                        track.artist.as_deref(),
                        track.album.as_deref(),
                    ));
                }
            }
            (items, missing)
        };

        if items.len() <= 1 {
            self.toast.update(cx, |toast, cx| {
                toast.show_plain("歌单曲目不足，无需重排".to_owned(), cx);
            });
            return;
        }

        let ordered = arrange_queue(&items, None, seed);
        self.library.update(cx, |library, cx| {
            library.apply_collection_order(id, &ordered, cx);
        });

        let missing_count = missing.len();
        if missing_count > 0 {
            self.analysis
                .read(cx)
                .request_batch(&missing, AnalysisPriority::Medium);
        }

        let message = if missing_count == 0 {
            format!("已按 BPM 与调性智能重排歌单「{name}」")
        } else {
            format!(
                "已重排歌单「{name}」（{missing_count} 首曲目正在后台补齐分析，完成后可再次重排）"
            )
        };
        self.toast
            .update(cx, |toast, cx| toast.show_plain(message, cx));
    }

    #[allow(clippy::used_underscore_binding)]
    fn schedule_bpm_resort(&mut self, cx: &mut Context<Self>) {
        let executor = cx.background_executor().clone();
        self._bpm_sort_task = Some(cx.spawn(async move |this, cx| {
            executor.timer(Duration::from_millis(800)).await;
            let _ = this.update(cx, |this, cx| {
                this._bpm_sort_task = None;
                if this.library.read(cx).sort_key() == SortKey::Bpm {
                    this.library.update(cx, LibraryEntity::apply_current_sort);
                }
            });
        }));
    }

    #[allow(clippy::used_underscore_binding)]
    fn schedule_smart_mix_refresh(&mut self, cx: &mut Context<Self>) {
        let executor = cx.background_executor().clone();
        self._smart_mix_task = Some(cx.spawn(async move |this, cx| {
            executor.timer(Duration::from_millis(1200)).await;
            let _ = this.update(cx, |this, cx| {
                this._smart_mix_task = None;
                if this.library.read(cx).is_smart_mix() {
                    this.arrange_smart_mix(true, cx);
                }
            });
        }));
    }

    /// 打开指定两首曲目的过渡编辑器弹窗。
    pub(super) fn open_mix_editor(&mut self, a_id: TrackId, b_id: TrackId, cx: &mut Context<Self>) {
        let pair = {
            let library = self.library.read(cx);
            let playlist = library.playlist();
            playlist
                .by_id(a_id)
                .cloned()
                .zip(playlist.by_id(b_id).cloned())
        };
        let Some((track_a, track_b)) = pair else {
            return;
        };
        self.mix_editor
            .update(cx, |editor, cx| editor.open(track_a, track_b, cx));
        cx.notify();
    }

    pub(super) fn nudge_seek(&mut self, delta: i64, cx: &mut Context<Self>) {
        self.playback.update(cx, |playback, cx| {
            let current = playback.position().as_millis() as i64;
            let total = playback.duration().as_millis() as i64;
            let target = (current + delta).clamp(0, total.max(0)) as u64;
            playback.seek(Duration::from_millis(target), cx);
        });
    }

    pub(super) fn remove_focused(&mut self, cx: &mut Context<Self>) {
        let removed = self.library.update(cx, LibraryEntity::remove_focused);
        if let Some((track, index)) = removed {
            self.offer_remove_undo(track, index, cx);
        }
    }

    /// 弹「已移除 + 撤销」，撤销时把曲目插回原处。
    /// 闭包要 `'static`，所以把 `Entity` 句柄克隆进去。
    pub(super) fn offer_remove_undo(
        &mut self,
        track: capstan_core::Track,
        index: usize,
        cx: &mut Context<Self>,
    ) {
        let title = track.title.clone();
        let library = self.library.clone();

        self.toast.update(cx, |toast, cx| {
            toast.show_undo(
                format!("已从曲库移除《{title}》"),
                move |_window, cx| {
                    // 撤销只动曲库，正在播的音频不受影响。
                    library.update(cx, |library, cx| library.restore(&track, index, cx));
                },
                cx,
            );
        });
    }

    pub(super) fn offer_clear_undo(
        &mut self,
        tracks: Vec<capstan_core::Track>,
        cx: &mut Context<Self>,
    ) {
        let count = tracks.len();
        let library = self.library.clone();

        self.toast.update(cx, |toast, cx| {
            toast.show_undo(
                format!("已清空曲库（{count} 首）"),
                move |_window, cx| {
                    library.update(cx, |library, cx| library.restore_all(tracks, cx));
                },
                cx,
            );
        });
    }
}
