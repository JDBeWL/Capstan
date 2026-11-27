//! 曲目集合的破坏性改动。删一首、清空、换排序键，以及它们的撤销。
//! 撤销就是把曲目插回原来的曲库下标，调用方手上的 `(Track, usize)` 是全部所需信息。

use capstan_core::{SortKey, Track, TrackId};

use crate::platform::*;

use super::LibraryEntity;
use super::LibraryEvent;
use super::scope::Scope;

impl LibraryEntity {
    /// 移除一首，返回被移除的曲目与它的曲库下标（撤销要用这两样，故在消失前克隆带出）。
    /// 下标必须是曲库空间的。`insert_at` 作用在曲库上，带过滤时与可见行号不同。
    pub fn remove(&mut self, id: TrackId, cx: &mut Context<Self>) -> Option<(Track, usize)> {
        let index = self.playlist.index_of(id)?;
        let track = self.playlist.by_id(id)?.clone();

        if !self.playlist.remove(id) {
            return None;
        }
        self.invalidate();
        self.persist();

        // 投影基于曲库下标，不重算可见行就会指向错位的曲目。
        self.recompute_visible(cx);

        // 光标不能悬空。落到原位置的下一首，到队尾就落最后一首；走 `set_focus` 列表才会滚过去。
        if self.focus == Some(id) {
            let len = self.playlist.len();
            let next = if len == 0 {
                None
            } else {
                self.playlist.tracks().get(index.min(len - 1)).map(|t| t.id)
            };
            match next {
                Some(next) => self.set_focus(next, cx),
                None => self.focus = None,
            }
        }

        cx.notify();
        Some((track, index))
    }

    /// 清空曲库，返回原来的全部曲目（撤销要用）
    pub fn clear(&mut self, cx: &mut Context<Self>) -> Vec<Track> {
        let previous = self.playlist.tracks().to_vec();
        self.playlist.clear();
        self.invalidate();
        self.persist();
        self.scope = Scope::All;
        self.focus = None;
        self.recompute_visible(cx);
        cx.notify();
        previous
    }

    /// 换排序键。它重排曲库本体与播放顺序，曲库下标整体变化，投影必须跟着重算。
    pub fn sort_by(&mut self, key: SortKey, cx: &mut Context<Self>) {
        if self.sort == key {
            return;
        }
        self.sort = key;
        self.playlist.sort_by(key);
        self.invalidate();
        self.recompute_visible(cx);
        cx.emit(LibraryEvent::PrefsChanged);
        cx.notify();
    }

    /// 注入单首曲目的离线分析 BPM，通知界面刷新 BPM 列。
    pub fn set_analyzed_bpm(&mut self, id: TrackId, bpm: f32, cx: &mut Context<Self>) -> bool {
        if !self.playlist.set_analyzed_bpm(id, bpm) {
            return false;
        }
        cx.notify();
        true
    }

    /// 批量注入离线分析 BPM，用于启动时一次性恢复磁盘缓存。
    pub fn set_analyzed_bpms(
        &mut self,
        entries: impl IntoIterator<Item = (TrackId, f32)>,
        cx: &mut Context<Self>,
    ) -> bool {
        if !self.playlist.set_analyzed_bpms(entries) {
            return false;
        }
        cx.notify();
        true
    }

    /// 按当前排序键重新排序并刷新投影，用于新分析的 BPM 落地后同步 BPM 排序。
    pub fn apply_current_sort(&mut self, cx: &mut Context<Self>) {
        self.playlist.apply_current_sort();
        self.invalidate();
        self.recompute_visible(cx);
        cx.notify();
    }

    pub fn remove_focused(&mut self, cx: &mut Context<Self>) -> Option<(Track, usize)> {
        let id = self.focus?;
        self.remove(id, cx)
    }

    /// 撤销用的回插。把曲目放回原来的曲库位置，并把光标落在它上面。
    pub fn restore(&mut self, track: &Track, index: usize, cx: &mut Context<Self>) {
        if self.playlist.insert_at(track.clone(), index) {
            self.invalidate();
            self.recompute_visible(cx);
            self.set_focus(track.id, cx);
            self.persist();
        }
    }

    /// 撤销「清空」。按原顺序整批插回。
    pub fn restore_all(&mut self, tracks: Vec<Track>, cx: &mut Context<Self>) {
        if self.playlist.restore_all(tracks) == 0 {
            return;
        }
        self.invalidate();
        self.recompute_visible(cx);
        self.persist();
        cx.notify();
    }
}
