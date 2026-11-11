//! 曲库实体，管播放列表、队列顺序、作用域与导入流程。
//! 曲目集合变了要走三件套，`persist` + `invalidate` + `recompute_visible`。

use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use capstan_core::{
    AlbumGroup, ArtistGroup, Collections, PlayMode, Playlist, Roots, SortKey, Track, TrackId,
};

use crate::platform::*;

mod collections;
mod groups;
mod likes_follows;
mod mutation;
mod navigation;
mod playback_control;
mod query;
mod recent;
mod scan;
mod scope;

pub use collections::CollectionSummary;
pub use playback_control::{RepeatState, Step};
pub use query::searching;
pub use scan::ScanProgress;
pub use scope::{FAVORITES_NAME, Scope, ScopeKind};

#[derive(Clone, Debug, PartialEq)]
pub enum LibraryEvent {
    Play(Track),
    /// 键盘光标移到了第 `usize` 行。滚动是副作用，不能放在 render 里做，只能用事件推到明确时机。
    FocusMoved(usize),
    /// 关注状态变了。详情页头图由 `RootView` 内联渲染、不 observe 实体，只能靠这个事件推重绘。
    FollowChanged,
    /// 作用域真的换了才发。`set_scope` 是唯一发出者，值没变不发；歌词遮罩只监听它。
    ScopeChanged,
    /// 一次导入或补扫完成。`scanned` 为扫到的文件数，含已在库的；`added` 为新入库的数量，
    /// `refreshed` 是重读过标签的；`quiet` 表示这是启动补扫，没有新增就不必打扰用户。
    Imported {
        scanned: usize,
        added: usize,
        refreshed: usize,
        quiet: bool,
    },
    /// 启动时的曲库缓存已经读完并合并。续播要等它，才不会在空曲库里把 resume.json 清掉。
    Loaded,
    /// 一次「重读全部标签」跑完了。与 [`Self::Imported`] 分开：
    /// 那是扫描目录的结果，这是用户明确点的一次刷新，文案不该说「已在曲库里」。
    Refreshed {
        /// 真正重读了几首。
        count: usize,
    },
    /// 一次操作未能完成，附带的文案直接展示给用户，即底部提示。
    Failed(String),
    /// 曲库侧的偏好变了。随机、循环、排序键、跟随播放。落盘由 `RootView` 统一负责，
    /// 曲库不持有设置实体，也不该知道 `settings.json` 长什么样。
    PrefsChanged,
}

pub struct LibraryEntity {
    playlist: Playlist,
    /// 键盘光标。`↑↓` 移动、`Enter` 才播放。默认跟随播放移动，见 `follow_playback`。
    /// 不变量。要么 `None`，要么在可见行里，由 `recompute_visible` 维持。
    focus: Option<TrackId>,
    /// 底色是否绘制。光标值始终存在，`Enter` / `Delete` 依赖它；底色仅在键盘确实移动过之后绘制。
    cursor_visible: bool,
    /// 自动切歌时列表要不要把当前曲滚进视野。默认关，见 [`LibraryEntity::follow_playback`]。
    list_follows_playback: bool,
    /// 实际装入播放器的曲目，不等于 `playlist.current_id()`；后者在导入完成后即指向第一首。
    now_playing: Option<TrackId>,
    query: String,
    /// 可见曲目在 `playlist.tracks()` 里的下标；`None` = 无过滤，不建恒等索引表。
    visible: Option<Vec<usize>>,
    /// 可见行号的 `TrackId` 反查，仅过滤生效时建立；键盘导航与选择清理靠它做到 O(1)。
    row_by_id: Option<HashMap<TrackId, usize>>,
    /// 预先小写化的检索字段，与 `playlist.tracks()` 同序；曲目集合变化时失效。
    search_fields: Option<Rc<Vec<crate::entities::filter::SearchFields>>>,
    /// 可见投影的代次。查询、作用域、删歌导致投影重算时加一，派生缓存按它失效。
    visible_generation: u64,
    /// 曲目、歌单、收藏、最近播放等数据内容的总代次；首页与搜索页的重型派生按它缓存。
    data_revision: u64,
    scope: Scope,
    scanning: Option<ScanProgress>,
    /// 扫描期间上次失效分组与检索缓存的时刻。每批都失效会让侧栏与首页按批整库重分组，
    /// 导入成本随曲数二次增长；节流到 [`scan::IMPORT_INVALIDATE_INTERVAL`]，结束强制失效。
    import_invalidate_at: std::time::Instant,
    /// 分组缓存，`None` 表示需要重算。
    album_groups: Option<std::rc::Rc<Vec<AlbumGroup>>>,
    artist_groups: Option<std::rc::Rc<Vec<ArtistGroup>>>,
    /// 分组缓存的代次，[`Self::invalidate`] 每失效一次加一。
    groups_generation: u64,
    /// 洗牌种子。进随机时按固定乘加序列推进，决定洗牌顺序。
    seed: u64,
    /// 导入时是否递归子目录，来自设置。
    recursive: bool,
    /// 导入过的文件夹。曲库只存曲目，没有「是从哪扫来的」这层信息，
    /// 补扫新歌与「我加过哪几个文件夹」都要靠这份根。
    roots: Roots,
    /// 当前这轮扫描的停止标志。换目标时先设置，后台线程才不会继续读取无效标签。
    scan_cancel: Option<Arc<AtomicBool>>,
    /// 随机轴，另一轴是 [`Self::repeat`]，由 [`Self::effective_mode`] 折算成 `PlayMode`。
    shuffle: bool,
    repeat: RepeatState,
    /// 用户选的排序键。`playlist` 自己也记着，但它在首屏合并时会被整个换掉，
    /// 那份记录随之退回默认，所以这里另存一份用来在合并之后重排。
    sort: SortKey,
    liked: crate::likes::Likes,
    /// 已关注的艺术家，按名字键控，而非 id。
    followed: crate::follows::Follows,
    collections: Collections,
    /// 最近播放，跨重启。
    recents: crate::recents::Recents,
    /// 最近搜索，跨重启。搜索页未输入时列它。
    history: crate::search_history::SearchHistory,
    /// 首屏还在等后台读 `library.json`。加载期间不落盘，避免把只有新导入项的曲库覆盖回去。
    loading: bool,
    /// 首屏加载任务，只写不读，留着它才能在实体销毁时取消。
    #[allow(dead_code)]
    load_task: Option<Task<()>>,
    /// 后台扫描任务，只写不读，留着它是为了 Drop 时取消扫描。`Task` 一 drop 就中止。
    _scan: Option<Task<()>>,
    /// 启动后回后台核对文件是否还在的任务，只写不读，同样靠 drop 中止。
    #[allow(dead_code)]
    verify_task: Option<Task<()>>,
}

impl EventEmitter<LibraryEvent> for LibraryEntity {}

impl LibraryEntity {
    pub fn new(cx: &mut Context<Self>) -> Self {
        // 窗口先出来。读盘、解析 JSON 与逐首核对文件都在后台线程做，完成后再合并。
        let mut this = Self {
            playlist: Playlist::new(),
            focus: None,
            cursor_visible: false,
            list_follows_playback: false,
            now_playing: None,
            query: String::new(),
            visible: None,
            row_by_id: None,
            search_fields: None,
            visible_generation: 0,
            data_revision: 0,
            scope: Scope::All,
            scanning: None,
            import_invalidate_at: std::time::Instant::now(),
            album_groups: None,
            artist_groups: None,
            groups_generation: 0,
            seed: 0x2545_f491_4f6c_dd1d,
            recursive: true,
            roots: crate::roots_store::load(),
            scan_cancel: None,
            shuffle: false,
            repeat: RepeatState::Off,
            sort: SortKey::ImportOrder,
            liked: crate::likes::Likes::load(),
            followed: crate::follows::Follows::load(),
            collections: crate::collections_store::load(),
            recents: crate::recents::Recents::load(),
            history: crate::search_history::SearchHistory::load(),
            loading: true,
            load_task: None,
            _scan: None,
            verify_task: None,
        };

        let executor = cx.background_executor().clone();
        let load = cx.spawn(async move |this, cx| {
            let tracks = executor
                .spawn(async move { crate::library_store::load() })
                .await;
            this.update(cx, |this, cx| this.finish_initial_load(tracks, cx))
                .ok();
        });
        this.load_task = Some(load);
        this
    }

    /// 首屏曲库是否仍在后台读取。
    pub fn loading(&self) -> bool {
        self.loading
    }

    /// 合并后台读回的曲库。加载期间用户已经导入的曲目排在后面，不丢任何一边。
    fn finish_initial_load(&mut self, loaded: Vec<capstan_core::Track>, cx: &mut Context<Self>) {
        if !self.loading {
            return;
        }
        let pending: Vec<capstan_core::Track> = self.playlist.tracks().to_vec();
        let had_pending = !pending.is_empty();
        self.playlist = merge_tracks(loaded, pending);
        // 合并用的是新建的 `Playlist`，它自带的排序键是默认值；用户上次选的那一档要从实体上补回去，
        // 否则工具栏会显示「导入顺序」而列表其实是按标题排的。
        self.playlist.sort_by(self.sort);
        self.loading = false;

        self.invalidate();
        self.recompute_visible(cx);
        if had_pending {
            // 加载期间用户导入的曲目要补写；没有新导入就不必每次启动都重写整库。
            self.persist();
        }
        // 队列先复位，`Loaded` 之后的续播才能在这条队列里找到上次那首。
        self.restore_queue();
        cx.emit(LibraryEvent::Loaded);
        cx.notify();
        // 缓存先当真画出来，核对文件还在不在放到后台单独跑。慢盘上几千次 stat 是启动路径
        // 上最贵的一段，而它只决定「哪几行是幽灵」，不决定「曲库长什么样」。
        self.verify_files(cx);
        // 上次退出之后新加进这些文件夹的歌，在这里补进来；
        // 排在 `Loaded` 之后，续播先按旧曲库装好位置，补扫只往后追加。
        if !self.roots.is_empty() {
            self.rescan_quietly(cx);
        }
    }

    /// 后台核对缓存里的文件是否还在。核完才剔。剔之前那几行就是幽灵。
    fn verify_files(&mut self, cx: &mut Context<Self>) {
        let entries: Vec<(TrackId, std::path::PathBuf)> = self
            .playlist
            .tracks()
            .iter()
            .map(|track| (track.id, track.path.clone()))
            .collect();
        if entries.is_empty() {
            return;
        }

        let executor = cx.background_executor().clone();
        let verify = cx.spawn(async move |this, cx| {
            let vanished = executor
                .spawn(async move { crate::library_store::gone(&entries) })
                .await;
            if vanished.is_empty() {
                return;
            }
            this.update(cx, |this, cx| this.drop_vanished(&vanished, cx))
                .ok();
        });
        self.verify_task = Some(verify);
    }

    /// 批量剔掉已经不在磁盘上的曲目。整趟只落盘一次、只重算一次投影。
    /// 逐条走 [`Self::remove`] 会把库重写 N 遍。
    fn drop_vanished(&mut self, ids: &[TrackId], cx: &mut Context<Self>) {
        let mut removed = 0usize;
        for id in ids {
            removed += usize::from(self.playlist.remove(*id));
        }
        if removed == 0 {
            return;
        }
        // 光标不能悬空，它指着的那一行可能刚被剔掉。
        if self
            .focus
            .is_some_and(|id| self.playlist.by_id(id).is_none())
        {
            self.focus = None;
        }
        self.invalidate();
        self.persist();
        self.recompute_visible(cx);
        cx.notify();
    }

    /// 把曲库按导入顺序写盘。曲目集合变化的每个出口都要调一次；排序与播放模式不落盘。
    fn persist(&self) {
        if self.loading {
            return;
        }
        crate::library_store::save(self.playlist.in_import_order().cloned().collect());
    }

    /// 把当前显式队列写盘。`queue()` 为 `None`（跟着曲库走）时写 `null`。
    /// 队列变化的每个出口都要调一次，重启后接回这条队列。
    fn persist_queue(&self) {
        if self.loading {
            return;
        }
        let paths = self.playlist.queue().map(|ids| {
            ids.iter()
                .filter_map(|id| self.playlist.by_id(*id).map(|track| track.path.clone()))
                .collect::<Vec<_>>()
        });
        crate::queue_store::save(paths);
    }

    /// 接回上次的显式队列。一首都对不上（整库换过、路径改过）时退回「跟着曲库走」，
    /// 而不是留下一条空队列让界面看起来像坏了。
    fn restore_queue(&mut self) {
        let Some(paths) = crate::queue_store::load() else {
            return;
        };
        let ids: Vec<capstan_core::TrackId> = paths
            .iter()
            .map(|path| capstan_core::TrackId::from_path(path))
            .collect();
        self.playlist.set_queue(Some(ids), self.seed);
        if self
            .playlist
            .queue()
            .is_some_and(<[capstan_core::TrackId]>::is_empty)
        {
            self.playlist.set_queue(None, self.seed);
        }
    }

    pub fn playlist(&self) -> &Playlist {
        &self.playlist
    }

    /// 全部曲目，未经搜索过滤。
    pub fn tracks(&self) -> &[Track] {
        self.playlist.tracks()
    }

    /// 曲库总数，未经过滤。
    pub fn len(&self) -> usize {
        self.playlist.len()
    }

    pub fn is_empty(&self) -> bool {
        self.playlist.is_empty()
    }

    pub fn mode(&self) -> PlayMode {
        self.playlist.mode()
    }

    /// 当前排序键。读的是实体自身保存的那份，首屏合并前后都要给出用户选的那一档。
    pub fn sort_key(&self) -> SortKey {
        self.sort
    }

    pub fn data_revision(&self) -> u64 {
        self.data_revision
    }

    pub(super) fn touch_data(&mut self) {
        self.data_revision = self.data_revision.wrapping_add(1);
    }
}

/// 后台读回的曲库在前，加载期间新导入的曲目在后；同 id 用新导入的标签覆盖。
fn merge_tracks(loaded: Vec<Track>, pending: Vec<Track>) -> Playlist {
    let mut playlist = Playlist::new();
    playlist.push(loaded);
    playlist.push(pending);
    playlist
}

#[cfg(test)]
mod tests {
    // 不写 `use super::*`，`crate::platform::*` 带的 gpui `test` 宏会遮蔽内置 `#[test]`。
    use std::path::{Path, PathBuf};

    use capstan_core::{Track, TrackId};

    use super::merge_tracks;

    fn track(name: &str, title: &str) -> Track {
        let path = PathBuf::from(format!("/music/{name}.flac"));
        let mut track = Track::fallback(&path);
        track.title = title.to_owned();
        track
    }

    #[test]
    fn loaded_tracks_stay_before_anything_imported_while_loading() {
        let merged = merge_tracks(
            vec![track("a", "旧 A"), track("b", "旧 B")],
            vec![track("c", "新 C")],
        );
        let titles: Vec<&str> = merged.tracks().iter().map(|t| t.title.as_str()).collect();
        assert_eq!(titles, ["旧 A", "旧 B", "新 C"]);
    }

    #[test]
    fn a_track_imported_while_loading_refreshes_the_loaded_label() {
        let merged = merge_tracks(vec![track("a", "旧标题")], vec![track("a", "重导后的标题")]);
        assert_eq!(merged.len(), 1, "同路径不该变成两首");
        assert_eq!(merged.tracks()[0].title, "重导后的标题");
        assert_eq!(
            merged.tracks()[0].id,
            TrackId::from_path(Path::new("/music/a.flac"))
        );
    }
}
