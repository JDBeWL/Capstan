//! 曲库列表，用虚拟列表渲染。
//! 曲库页工具条固定、只滚行；专辑或艺人详情与搜索结果把页头并进同一个滚动区，
//! 这样整页一起滚，滚动条从内容区顶部贯穿到底部。

mod batch;
mod cells;
mod columns;
mod drag;
mod empty;
mod row;
mod selection;
mod toolbar;

use std::collections::HashSet;
use std::rc::Rc;
use std::time::{Duration, Instant};

use capstan_core::{CollectionId, Track, TrackId};

use crate::common::anim;
use crate::cover::CoverCache;
use crate::entities::{LibraryEntity, LibraryEvent, Scope, ScopeKind};
use crate::platform::*;
use crate::settings::TrackColumns;
use crate::shell::{Breakpoint, NavSection, ShellEntity};
use crate::theme;

use super::SearchPage;
use super::SettingsEntity;
use super::detail::{self, HeroInfo};

/// 拖拽手柄列宽，只在歌单作用域出现。
const HANDLE_W: f32 = 26.0;
/// 勾选列宽，只在多选模式出现。
const CHECK_W: f32 = 32.0;
const CHECK_BOX: f32 = 18.0;
/// 标题行首的封面缩略图边长（舒适密度）。
const THUMB: f32 = 40.0;
/// 紧凑密度下的封面边长，40px 行高塞不下 40px 封面 + 文字。
const THUMB_COMPACT: f32 = 32.0;
/// 双行阈值。行高 >= 它才画两行，缩略图也用大号。必须落在两档行高（40 / 52）之间。
const TWO_LINE_MIN: f32 = 48.0;
/// 收藏列宽：容得下表头「LIKE / 喜欢」和居中的心形，并和右侧时长留出间隔。
const LIKE_W: f32 = 88.0;
/// 标题列的最小宽度。再窄就只剩封面，表头文字会竖着排。
const TITLE_MIN_W: f32 = 140.0;
/// 专辑列的硬下限。拖到最窄只剩几个截断字；主区连它都放不下就收起整列。
const ALBUM_MIN_W: f32 = 64.0;

/// 列表向上层汇报的事件。用事件而不是回调字段，列表不持有 `RootView`。
#[derive(Clone, Debug, PartialEq)]
pub enum TrackListEvent {
    /// 移除了一首，附原曲目与原行号（撤销要用）
    Removed(Track, usize),
    /// 清空了曲库，附原来的全部曲目。
    Cleared(Vec<Track>),
    /// 即将进入专辑与艺术家详情。落点由 `RootView` 持有；不发这一句，页头不会出现返回按钮。
    RecordFromTrack,
}

/// 页头与列表合成一页时的页头标识。换了页头（换艺人或进出搜索）整表重置。
#[derive(Clone, PartialEq, Eq)]
enum PageHead {
    /// 搜索结果页，摘要就是 [`SearchPage`] 这一块。
    Search,
    /// 专辑与艺人详情页，头图从作用域现算。
    Scope(Scope),
}

pub struct TrackList {
    library: Entity<LibraryEntity>,
    shell: Entity<ShellEntity>,
    /// 行首缩略图的封面缓存，列表只读、只发请求。
    covers: Entity<CoverCache>,
    /// 搜索结果页的摘要，合成页时作为列表项 0 的一部分出现在页头里。
    search_page: Entity<SearchPage>,
    /// 列宽的落盘点。拖动只改内存，停手后经它写 settings.json。
    settings: Entity<SettingsEntity>,
    /// NO./专辑的存储宽度与列尾空白；标题列吃剩余宽度，时长是常量。
    columns: TrackColumns,
    /// 窗口伸缩时标题与专辑分摊宽度的占比。拖动分隔线后置空，下一次布局重新取样。
    album_ratio: Option<f32>,
    scroll: UniformListScrollHandle,
    /// 页头并入滚动区时用的列表。项 0 是页头，之后才是曲目行。
    page: ListState,
    /// 当前合成页的页头；`None` 表示纯列表模式（工具条固定）。
    page_head: Option<PageHead>,
    /// 合成页当前的行数（不含页头那一项）。
    page_rows: usize,
    /// 页头高度依赖查询、多选开关与断点，变化时让页头重新量。
    page_query: String,
    page_breakpoint: Breakpoint,
    /// 搜索页头还随可用宽度换列与换行，宽度变了也要重新量。
    page_center_width: f32,
    page_selecting: bool,
    /// 多选模式开关。开着才画勾选列，点标题也从「播放」变成「选中」。
    selecting: bool,
    /// 已选中的曲目。包一层 `Rc`。交给列表闭包的拷贝在整库勾选时是 O(n)，每帧一次。
    selected: Rc<HashSet<TrackId>>,
    /// 最近一次点中的行号，Shift 连选从这里起算。
    anchor: Option<usize>,
    /// 上次清理选择时对应的可见投影代次。只在换代时清理，渲染本身不再扫全表。
    selection_generation: u64,
    /// 正在播退出动画的那一行；播完才真正从曲库删。
    removing: Option<Removal>,
    /// 删除动画的世代号；行模型变了也不会复用上一轮的动画进度。
    remove_seq: u64,
    /// 可见投影代次变化后延时启动的错开入场。
    stagger_generation: u64,
    stagger_seq: u64,
    stagger_started: Option<Instant>,
    _stagger_task: Option<Task<()>>,
    /// 列宽防抖落盘。每次拖动重置，只有停手后那一次真正写。
    _columns_save: Option<Task<()>>,
    _subs: Vec<Subscription>,
}

#[derive(Clone, Copy)]
struct Removal {
    visible_index: usize,
    generation: u64,
}

impl EventEmitter<TrackListEvent> for TrackList {}

impl TrackList {
    pub fn new(
        library: Entity<LibraryEntity>,
        shell: Entity<ShellEntity>,
        covers: Entity<CoverCache>,
        search_page: Entity<SearchPage>,
        settings: Entity<SettingsEntity>,
        cx: &mut Context<Self>,
    ) -> Self {
        let observe = cx.observe(&library, |_, _, cx| cx.notify());
        let observe_shell = cx.observe(&shell, |_, _, cx| cx.notify());
        // 封面解码完成时要重绘，否则行首占位图一直挂着。
        let observe_covers = cx.observe(&covers, |_, _, cx| cx.notify());

        // 键盘光标移动时滚动跟随。用 `Nearest`，`Center` 每次按方向键都大幅跳动。
        let scroll = UniformListScrollHandle::default();
        let follow = cx.subscribe(&library, move |this, _, event: &LibraryEvent, cx| {
            if let LibraryEvent::FocusMoved(index) = event {
                if this.page_head.is_some() {
                    // 合成页的项 0 是页头，行号要顺延一个。
                    this.page.scroll_to_reveal_item(index + 1);
                    cx.notify();
                } else {
                    this.scroll.scroll_to_item(*index, ScrollStrategy::Nearest);
                }
            }
        });
        let page_breakpoint = shell.read(cx).breakpoint();
        let columns = settings.read(cx).settings().track_columns.clamped();

        Self {
            selection_generation: library.read(cx).visible_generation(),
            removing: None,
            remove_seq: 0,
            stagger_generation: library.read(cx).visible_generation(),
            stagger_seq: 0,
            stagger_started: None,
            _stagger_task: None,
            _columns_save: None,
            library,
            shell,
            covers,
            search_page,
            settings,
            columns,
            album_ratio: None,
            scroll,
            page: ListState::new(0, ListAlignment::Top, px(0.)),
            page_head: None,
            page_rows: 0,
            page_query: String::new(),
            page_breakpoint,
            page_center_width: 0.0,
            page_selecting: false,
            selecting: false,
            selected: Rc::new(HashSet::new()),
            anchor: None,
            _subs: vec![observe, observe_shell, observe_covers, follow],
        }
    }

    /// 删除先播 200ms 退出动画。这期间行仍留在数据里，下面的行先向上补位；
    /// 动画结束才真正从曲库移除，视觉上正好接住。
    #[allow(clippy::used_underscore_binding)]
    pub(super) fn begin_remove(&mut self, id: TrackId, index: usize, cx: &mut Context<Self>) {
        if self.removing.is_some() {
            return;
        }
        self.remove_seq = self.remove_seq.wrapping_add(1);
        let generation = self.remove_seq;
        self.removing = Some(Removal {
            visible_index: index,
            generation,
        });
        cx.notify();

        let library = self.library.clone();
        let executor = cx.background_executor().clone();
        cx.spawn(async move |this, cx| {
            executor
                .timer(Duration::from_millis(crate::theme::MOTION_ROW_MS))
                .await;
            this.update(cx, |this, cx| {
                if this.removing.as_ref().map(|row| row.generation) != Some(generation) {
                    return;
                }
                this.removing = None;
                let removed = library.update(cx, |library, cx| library.remove(id, cx));
                // 本轮为删除操作自身触发的投影换代，不再额外排队一次错开入场。
                this.stagger_generation = library.read(cx).visible_generation();
                this.stagger_started = None;
                this._stagger_task = None;
                if let Some((track, at)) = removed {
                    cx.emit(TrackListEvent::Removed(track, at));
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// 等输入停手 180ms 再启动逐行入场；连续打字只有最后一次算数。
    #[allow(clippy::used_underscore_binding)]
    fn schedule_stagger(&mut self, cx: &mut Context<Self>) {
        let executor = cx.background_executor().clone();
        self._stagger_task = Some(cx.spawn(async move |this, cx| {
            executor
                .timer(Duration::from_millis(anim::STAGGER_DEBOUNCE_MS))
                .await;
            this.update(cx, |this, cx| {
                this.stagger_seq = this.stagger_seq.wrapping_add(1);
                this.stagger_started = Some(Instant::now());
                cx.notify();
            })
            .ok();
        }));
    }

    /// 表头拖宽。宽度已按拖动方向算好，这里只负责写回、钳制和重绘。
    /// `old` 是这一列当前画出来的宽度，差值记到空白上，别的列不动。
    fn resize_column(
        &mut self,
        edge: columns::ColumnEdge,
        old: f32,
        new: f32,
        gap: f32,
        cx: &mut Context<Self>,
    ) {
        let mut next = self.columns;
        columns::apply(&mut next, edge, old, new, gap);
        let next = next.clamped();
        if next == self.columns {
            return;
        }
        self.columns = next;
        self.album_ratio = None;
        self.schedule_columns_save(cx);
        cx.notify();
    }

    /// 拖动中每帧写盘太重。停手 300ms 后只写最后一份，中间的过程不值得落盘。
    #[allow(clippy::used_underscore_binding)]
    fn schedule_columns_save(&mut self, cx: &mut Context<Self>) {
        let executor = cx.background_executor().clone();
        let settings = self.settings.clone();
        let columns = self.columns;
        self._columns_save = Some(cx.spawn(async move |_, cx| {
            executor.timer(Duration::from_millis(300)).await;
            settings.update(cx, |settings, cx| {
                settings.set_track_columns(columns, cx);
            });
        }));
    }
}

impl Render for TrackList {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // 查询、作用域、删歌都会让可见投影换代；只有这时才需要清掉看不见的勾选。
        let generation = self.library.read(cx).visible_generation();
        if generation != self.selection_generation {
            self.selection_generation = generation;
            self.prune_selection(cx);
        }
        if generation != self.stagger_generation {
            self.stagger_generation = generation;
            self.schedule_stagger(cx);
        }
        let stagger = self.stagger_started.and_then(|started| {
            (started.elapsed() < Duration::from_millis(anim::STAGGER_WINDOW_MS))
                .then_some(self.stagger_seq)
        });

        // 删除动画。一次渲染只采样一次，所有行共用同一个进度。
        let removal = self.removing;
        let remove_progress = removal.map_or(0.0, |row| {
            motion::transition(
                (
                    ElementId::named_usize("row-remove", row.generation as usize),
                    "progress",
                ),
                1.0,
                motion::Transition::new(Duration::from_millis(theme::MOTION_ROW_MS))
                    .ease(theme::ease_mech()),
                window,
                cx,
            )
        });

        let (
            count,
            total,
            playing,
            focus,
            filtering,
            sort_key,
            scanning,
            scoped,
            scope,
            from_list,
            loading,
        ) = {
            let library = self.library.read(cx);
            (
                library.visible_len(),
                library.len(),
                // 用 `now_playing` 而不是队列光标 `current_id`，后者导入完成时
                // 就指向第一首，会把第 1 行误标成播放中。
                library.now_playing(),
                // 光标的值一直在（`Enter` / `Delete` 要它），底色只在键盘真的动过之后才画。
                library.focus().filter(|_| library.shows_cursor()),
                // 用 `has_query()` 而不是 `is_filtering()`，后者在「只进了一张
                // 专辑、没搜索」时也是 `true`。
                library.has_query(),
                library.sort_key(),
                library.scanning(),
                library.scope().is_scoped(),
                library.scope_label(),
                matches!(library.scope(), Scope::Collection(_) | Scope::Favorites),
                library.loading(),
            )
        };
        let (kind, collection) = {
            let library = self.library.read(cx);
            let scope = library.scope();
            (
                scope.kind(),
                match scope {
                    Scope::Collection(id) => Some(*id),
                    _ => None,
                },
            )
        };
        let reorderable = toolbar::can_reorder(collection.is_some(), filtering, count);

        // `in_search` 判的是「停在搜索页」而不是「有搜索词」，曲库页用顶栏过滤时仍算曲库页。
        let in_search = self.shell.read(cx).nav_section() == crate::shell::NavSection::Search;
        let headed_elsewhere = scoped || in_search;

        // 搜索页不提供多选；在曲库开着多选再搜进来时，选择状态留着，界面先按单选画。
        let chosen_count = self.selected.len();
        let selecting = self.selecting && !in_search;
        let selected = Rc::clone(&self.selected);

        let center_width = self.shell.read(cx).center_width();
        let breakpoint = self.shell.read(cx).breakpoint();
        let row_extras = theme::ROW_PAD_LEFT
            + theme::GAP_MD
            + if selecting { CHECK_W } else { 0.0 }
            + if reorderable { HANDLE_W } else { 0.0 };
        let content_width = (center_width - row_extras).max(0.0);
        let available =
            (content_width - self.columns.index - LIKE_W - theme::COLUMN_TIME_W - self.columns.gap)
                .max(0.0);
        self.columns.album =
            shared_album_width(available, self.columns.album, &mut self.album_ratio);
        let fit = column_fit(
            self.columns,
            content_width,
            row::show_album_column(breakpoint.shows_album_column(), kind),
        );

        let env = RowEnv {
            row_height: self.shell.read(cx).row_height(),
            show_album: fit.album.is_some(),
            title_width: fit.title,
            album_width: fit.album.unwrap_or(0.0),
            gap: fit.gap,
            kind,
            reorderable,
            selectable: selecting,
            columns: self.columns,
        };

        // 页头并入滚动区只发生在两种页面，即搜索结果与专辑或艺人详情。
        // 曲库页没有页头，工具条固定、只滚行。
        let current_scope = self.library.read(cx).scope().clone();
        let hero: Option<HeroInfo> = if self.shell.read(cx).nav_section() == NavSection::Tracks {
            App::update_entity(cx, &self.library, |library, _| {
                detail::hero_for(library, &current_scope)
            })
        } else {
            None
        };
        let page_head = if in_search && filtering && count > 0 {
            Some(PageHead::Search)
        } else if hero.is_some() {
            Some(PageHead::Scope(current_scope))
        } else {
            None
        };
        let query = self.library.read(cx).query().to_owned();

        let list_weak = Self::weak(cx);
        let toolbar_state = toolbar::ToolbarState {
            count,
            total,
            filtering,
            sort_key,
            scanning,
            scoped,
            kind,
            from_list,
            scope: scope.clone(),
            headed_elsewhere,
            in_search,
            selecting,
            follow: self.library.read(cx).list_follows_playback(),
        };

        if let Some(head) = page_head {
            // 页头是列表项 0，整页共用一个滚动条。
            let overdraw = px(env.row_height * 4.0);
            if self.page_head.as_ref() == Some(&head) {
                if count != self.page_rows {
                    self.page.splice(1..self.page_rows + 1, count);
                    // 新行给统一高度提示，否则滚到没渲染过的区域时滚动条长度会缩水。
                    self.page = self
                        .page
                        .clone()
                        .with_uniform_item_height(px(env.row_height));
                    self.page_rows = count;
                    if head == PageHead::Search {
                        // 搜索结果增减会让分组的显隐变化，页头高度跟着变。
                        self.page.remeasure_items(0..1);
                    }
                }
                if query != self.page_query {
                    if head == PageHead::Search {
                        // 结果摘要的高度随命中项变化，页头要重新量。
                        self.page.remeasure_items(0..1);
                    }
                    self.page.scroll_to(ListOffset {
                        item_ix: 0,
                        offset_in_item: px(0.),
                    });
                    self.page_query.clone_from(&query);
                }
                if breakpoint != self.page_breakpoint {
                    // 断点变化后页头会重排；保留旧高度当提示，等它可见时重新量。
                    self.page.remeasure_items(0..1);
                    self.page_breakpoint = breakpoint;
                }
                if head == PageHead::Search && (center_width - self.page_center_width).abs() >= 0.5
                {
                    // 面板开合与拖动窗口会换分组列数或并排方式，页头高度必须重新量。
                    self.page.remeasure_items(0..1);
                    self.page_center_width = center_width;
                }
                if selecting != self.page_selecting {
                    // 批量操作条出现与消失会改变页头高度。
                    self.page.remeasure_items(0..1);
                    self.page_selecting = selecting;
                }
            } else {
                self.page = ListState::new(count + 1, ListAlignment::Top, overdraw)
                    .with_uniform_item_height(px(env.row_height));
                self.page_rows = count;
                self.page_query.clone_from(&query);
                self.page_breakpoint = breakpoint;
                self.page_center_width = center_width;
                self.page_selecting = selecting;
            }
            self.page_head = Some(head.clone());

            let page = self.page.clone();
            let library = self.library.clone();
            let covers = self.covers.clone();
            let shell = self.shell.clone();
            let search_page = self.search_page.clone();
            let list_weak = list_weak.clone();
            let selected = Rc::clone(&selected);
            let toolbar = toolbar_state.clone();
            let page_query = query.clone();
            let scope_label = scope.clone();
            let page_body = list(page.clone(), move |ix, window, cx| {
                if ix == 0 {
                    let mut header = div().flex_none().flex().flex_col().w_full();
                    match &head {
                        PageHead::Search => header = header.child(search_page.clone()),
                        PageHead::Scope(_) => {
                            if let Some(info) = hero.as_ref() {
                                // 多选开关进头图的「更多操作」菜单，不再单独占一行。
                                let multi_select = (count > 0).then(|| detail::MultiSelectAction {
                                    selecting,
                                    on_toggle: Rc::new({
                                        let list = list_weak.clone();
                                        move |cx: &mut App| {
                                            list.update(cx, TrackList::toggle_selection_mode).ok();
                                        }
                                    }),
                                });
                                header = header.child(detail::hero(
                                    info,
                                    library.clone(),
                                    &covers,
                                    shell.clone(),
                                    multi_select,
                                    cx,
                                ));
                            }
                        }
                    }
                    header = header.child(chrome(
                        &library,
                        list_weak.clone(),
                        env,
                        &toolbar,
                        selecting,
                        chosen_count,
                        Rc::clone(&selected),
                        count,
                        collection,
                        window,
                        cx,
                    ));
                    if count == 0 {
                        header = header.child(empty_body(
                            loading,
                            total,
                            0,
                            &library,
                            &page_query,
                            scope_label.clone(),
                            from_list,
                            window,
                            cx,
                        ));
                    }
                    return header.into_any_element();
                }

                let index = ix - 1;
                let Some(track) = library
                    .read(cx)
                    .visible_tracks(index..index + 1)
                    .into_iter()
                    .next()
                else {
                    return div().into_any_element();
                };
                let state = row::RowState::of(track.id, playing, focus);
                let picked = selected.contains(&track.id);
                let motion = row_motion(index, removal, remove_progress, env.row_height);
                let enter = stagger.and_then(|seq| stagger_enter(seq, index));
                row::row(
                    index, &track, state, picked, env, &library, &list_weak, &covers, motion,
                    enter, window, cx,
                )
            })
            .size_full()
            .into_any_element();

            return div()
                .flex()
                .flex_col()
                .size_full()
                .min_h_0()
                .child(div().flex_1().min_h_0().relative().child(page_body).child(
                    crate::common::scroll::scrollbar(&self.page, "track-page-scrollbar"),
                ))
                .into_any_element();
        }

        self.page_head = None;
        let header = chrome(
            &self.library,
            list_weak.clone(),
            env,
            &toolbar_state,
            selecting,
            chosen_count,
            Rc::clone(&self.selected),
            count,
            collection,
            window,
            cx,
        );

        let body: AnyElement = if count == 0 {
            empty_body(
                loading,
                total,
                0,
                &self.library,
                &query,
                scope,
                from_list,
                window,
                cx,
            )
        } else {
            let library = self.library.clone();
            let covers = self.covers.clone();
            let list = Self::weak(cx);
            uniform_list("track-list", count, move |range, window, cx| {
                // 走可见投影而不是曲库本体，只拷可视区那几首。
                let tracks = library.read(cx).visible_tracks(range.clone());
                tracks
                    .into_iter()
                    .enumerate()
                    .map(|(offset, track)| {
                        let index = range.start + offset;
                        let state = row::RowState::of(track.id, playing, focus);
                        let picked = selected.contains(&track.id);
                        let motion = row_motion(index, removal, remove_progress, env.row_height);
                        let enter = stagger.and_then(|seq| stagger_enter(seq, index));
                        row::row(
                            index, &track, state, picked, env, &library, &list, &covers, motion,
                            enter, window, cx,
                        )
                    })
                    .collect::<Vec<AnyElement>>()
            })
            .size_full()
            .track_scroll(&self.scroll)
            .into_any_element()
        };

        div()
            .flex()
            .flex_col()
            .size_full()
            .min_h_0()
            .child(header)
            .child(div().flex_1().min_h_0().relative().child(body).child(
                crate::common::scroll::scrollbar(&self.scroll, "track-list-scrollbar"),
            ))
            .into_any_element()
    }
}

/// 工具条、批量操作条与列头，纯列表模式固定在顶部，合成页里属于页头那一项。
fn chrome(
    library: &Entity<LibraryEntity>,
    list: WeakEntity<TrackList>,
    env: RowEnv,
    state: &toolbar::ToolbarState,
    selecting: bool,
    chosen_count: usize,
    selected: Rc<HashSet<TrackId>>,
    rows: usize,
    collection: Option<CollectionId>,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let toolbar_visible = !state.headed_elsewhere || state.scanning.is_some();

    let mut column = div().flex_none().flex().flex_col();
    if toolbar_visible {
        column = column.child(toolbar::toolbar(
            library.clone(),
            list.clone(),
            state.clone(),
            window,
            cx,
        ));
    }

    if selecting {
        column = column.child(batch::batch_bar(batch::BatchBar {
            chosen_count,
            selected,
            rows,
            collection,
            library: library.clone(),
            list: list.clone(),
        }));
    }

    column
        .child(row::column_header(&env, list))
        .into_any_element()
}

fn empty_body(
    loading: bool,
    total: usize,
    count: usize,
    library: &Entity<LibraryEntity>,
    query: &str,
    scope: Option<String>,
    from_list: bool,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    if loading {
        empty::loading_library().into_any_element()
    } else if total == 0 {
        empty::empty_library(library.clone(), window, cx).into_any_element()
    } else if count == 0 {
        if query.trim().is_empty() {
            empty::empty_scope(library.clone(), scope, from_list, window, cx).into_any_element()
        } else {
            empty::no_matches(library.clone(), total, query, window, cx).into_any_element()
        }
    } else {
        div().into_any_element()
    }
}

fn row_motion(
    index: usize,
    removal: Option<Removal>,
    progress: f32,
    row_height: f32,
) -> row::RowMotion {
    match removal {
        Some(row) if index == row.visible_index => row::RowMotion {
            remove: progress,
            shift: 0.0,
        },
        Some(row) if index > row.visible_index => row::RowMotion {
            remove: 0.0,
            shift: -row_height * progress,
        },
        _ => row::RowMotion::default(),
    }
}

/// 本轮逐行入场的延迟。只照顾首屏前 `STAGGER_ROWS` 行，延迟按行号累加但不超过 6 行。
fn stagger_enter(seq: u64, index: usize) -> Option<(u64, u64)> {
    (index < anim::STAGGER_ROWS).then_some((seq, (index.min(6) as u64) * anim::STAGGER_ROW_MS))
}

/// 列宽在当前主区宽度下的实际布局。
/// 标题列永远是剩余宽度（窗口变宽就变宽），所以不存它；
/// 放不下时先扣掉空白，再压专辑，专辑到下限还不行就收起。
struct ColumnFit {
    title: f32,
    album: Option<f32>,
    gap: f32,
}

/// 窗口伸缩时，专辑按上一次的占比跟着标题一起缩放；占比在拖动分隔线后重新取样。
fn shared_album_width(available: f32, album: f32, ratio: &mut Option<f32>) -> f32 {
    let share = *ratio.get_or_insert_with(|| {
        if available > 0.0 {
            (album / available).clamp(0.0, 1.0)
        } else {
            0.0
        }
    });
    if available <= 0.0 || share <= 0.0 {
        return album;
    }
    (available * share).clamp(ALBUM_MIN_W, 480.0)
}

fn column_fit(columns: TrackColumns, content_width: f32, album_allowed: bool) -> ColumnFit {
    let fixed = columns.index + LIKE_W + theme::COLUMN_TIME_W;
    let mut gap = columns.gap.max(0.0);
    let mut album = album_allowed.then(|| columns.album.max(ALBUM_MIN_W));
    let mut title = content_width - fixed - album.unwrap_or(0.0) - gap;

    if title < TITLE_MIN_W {
        // 标题先吃空白，再压专辑，保住地板。
        gap = (gap - (TITLE_MIN_W - title)).max(0.0);
        title = content_width - fixed - album.unwrap_or(0.0) - gap;
    }
    if let Some(width) = album
        && title < TITLE_MIN_W
    {
        let room = content_width - fixed - gap - TITLE_MIN_W;
        album = (room >= ALBUM_MIN_W).then(|| width.min(room));
        title = content_width - fixed - album.unwrap_or(0.0) - gap;
    }
    let title = title.max(0.0);

    ColumnFit { title, album, gap }
}

/// 行与列头共用的布局与作用域参数。
#[derive(Clone, Copy)]
struct RowEnv {
    /// 行高，来自密度设置；`uniform_list` 用它算每一项的位置。
    row_height: f32,
    show_album: bool,
    /// 标题列的实际宽度（剩余宽度）。
    title_width: f32,
    /// 专辑列的实际宽度。主区窄时小于用户拖出来的值，见 [`column_fit`]。
    album_width: f32,
    /// 能借用的空白，落在时长右侧。拖宽某列时最多放到这里。
    gap: f32,
    /// 作用域种类，决定副标题省哪段、列头第二段写谁。
    kind: ScopeKind,
    /// 行首拖拽手柄 + 行内落点区。
    reorderable: bool,
    selectable: bool,
    /// 编号/空白/专辑的存储值；标题宽度不存，时长是常量。
    columns: TrackColumns,
}

impl RowEnv {
    /// 与 `cells::title_cell` 同一判据，列头据此让出缩略图的宽度。
    fn two_lines(self) -> bool {
        self.row_height >= TWO_LINE_MIN
    }

    fn thumb_size(self) -> f32 {
        if self.two_lines() {
            THUMB
        } else {
            THUMB_COMPACT
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        Removal, TWO_LINE_MIN, anim, column_fit, row_motion, shared_album_width, stagger_enter,
    };
    use crate::settings::TrackColumns;
    use crate::shell::Density;

    fn columns(index: f32, gap: f32, album: f32) -> TrackColumns {
        TrackColumns { index, gap, album }
    }

    #[test]
    fn the_title_fills_the_window_by_default() {
        // 内容 720、固定 36+88+60=184 → 剩余 536，专辑 220 → 标题 316。
        let fit = column_fit(columns(36.0, 0.0, 220.0), 720.0, true);
        assert_eq!(fit.title, 316.0);
        assert_eq!(fit.album, Some(220.0));
        assert_eq!(fit.gap, 0.0);
    }

    #[test]
    fn a_wider_window_widens_the_title_not_the_gap() {
        let narrow = column_fit(columns(36.0, 0.0, 220.0), 720.0, true);
        let wide = column_fit(columns(36.0, 0.0, 220.0), 900.0, true);
        assert_eq!(wide.gap, 0.0, "窗口多出来的宽度不该落到空白里");
        assert_eq!(wide.title - narrow.title, 180.0, "宽出来多少，标题就宽多少");
    }

    #[test]
    fn the_gap_from_compressing_a_column_holds_the_title_still() {
        // 专辑缩到 100，空出 120：标题仍拿剩余宽度 316。
        let fit = column_fit(columns(36.0, 120.0, 100.0), 720.0, true);
        assert_eq!(fit.title, 316.0);
        assert_eq!(fit.album, Some(100.0));
        assert_eq!(fit.gap, 120.0);
    }

    #[test]
    fn a_narrow_window_eats_the_gap_before_the_album() {
        // 标题不够地板：先扣 120 的空白，还差 84 → 专辑让到 396。
        let fit = column_fit(columns(36.0, 120.0, 480.0), 720.0, true);
        assert_eq!(fit.gap, 0.0, "先扣空白");
        assert_eq!(fit.album, Some(396.0), "再压专辑给标题留地板");
        assert_eq!(fit.title, 140.0);
    }

    #[test]
    fn too_narrow_drops_the_album_column() {
        let fit = column_fit(columns(36.0, 0.0, 220.0), 300.0, true);
        assert_eq!(fit.album, None, "标题地板与专辑下限都放不下时收起整列");
        assert_eq!(fit.title, 116.0, "专辑收起后标题拿回全部剩余宽度");

        let fit = column_fit(columns(36.0, 0.0, 220.0), 200.0, true);
        assert_eq!(fit.album, None);
        assert_eq!(fit.title, 16.0, "连标题地板都放不下时只守剩余宽度");
    }

    #[test]
    fn an_album_detail_gives_the_whole_remainder_to_the_title() {
        let fit = column_fit(columns(36.0, 0.0, 220.0), 900.0, false);
        assert_eq!(fit.album, None);
        assert_eq!(fit.title, 900.0 - 36.0 - 88.0 - 60.0);
    }

    #[test]
    fn window_resize_shares_the_delta_between_title_and_album() {
        let mut ratio = None;
        // 可用区 1000、专辑 250 → 占比 0.25。
        assert_eq!(shared_album_width(1000.0, 250.0, &mut ratio), 250.0);
        // 窗口拉宽，专辑跟着标题一起变宽。
        assert_eq!(shared_album_width(1200.0, 250.0, &mut ratio), 300.0);
        // 再缩窄，两列一起变窄。
        assert_eq!(shared_album_width(800.0, 300.0, &mut ratio), 200.0);
    }

    #[test]
    fn dragging_a_divider_resamples_the_ratio() {
        // 拖动分隔线后调用方会把占比置空，下一次布局按新宽度重新取样。
        let mut ratio = None;
        assert_eq!(shared_album_width(800.0, 300.0, &mut ratio), 300.0);
        assert_eq!(shared_album_width(1200.0, 300.0, &mut ratio), 450.0);
    }

    #[test]
    fn the_two_line_threshold_separates_the_two_densities() {
        assert!(
            Density::Comfortable.row_height() >= TWO_LINE_MIN,
            "舒适档（{}px）应当画两行（阈值 {TWO_LINE_MIN}px）",
            Density::Comfortable.row_height()
        );
        assert!(
            Density::Compact.row_height() < TWO_LINE_MIN,
            "紧凑档（{}px）应当只有一行",
            Density::Compact.row_height()
        );
    }

    #[test]
    fn stagger_covers_only_the_first_screen_rows() {
        assert_eq!(stagger_enter(7, 0), Some((7, 0)));
        assert_eq!(stagger_enter(7, 3), Some((7, 3 * anim::STAGGER_ROW_MS)));
        assert_eq!(
            stagger_enter(7, 6),
            Some((7, 6 * anim::STAGGER_ROW_MS)),
            "第 6 行开始延迟封顶"
        );
        assert_eq!(
            stagger_enter(7, anim::STAGGER_ROWS - 1),
            Some((7, 6 * anim::STAGGER_ROW_MS))
        );
        assert_eq!(stagger_enter(7, anim::STAGGER_ROWS), None);
    }

    #[test]
    fn removal_fades_the_row_and_pulls_the_rows_below_up() {
        let removal = Removal {
            visible_index: 3,
            generation: 1,
        };
        let target = row_motion(3, Some(removal), 0.5, 52.0);
        assert_eq!(target.remove, 0.5);
        assert_eq!(target.shift, 0.0);

        let below = row_motion(4, Some(removal), 0.5, 52.0);
        assert_eq!(below.remove, 0.0);
        assert_eq!(below.shift, -26.0);

        let above = row_motion(2, Some(removal), 0.5, 52.0);
        assert_eq!(above.remove, 0.0);
        assert_eq!(above.shift, 0.0);
    }
}
