//! 右侧面板的「队列」页签，显示真实播放顺序 `Playlist::order`，随机模式下与曲库顺序完全不同。
//! 这里不参与搜索也不增删曲目，顺序只由播放模式决定。

use std::rc::Rc;

use lyre_core::{PlayMode, Track, TrackId, format_duration};

use crate::common::thousands;
use crate::cover::CoverCache;
use crate::entities::LibraryEntity;
use crate::icon::{Glyph, icon, icon_inherit};
use crate::platform::*;
use crate::shell::ShellEntity;
use crate::theme;

use super::panel_close_button;
use super::track_drag::{DragGhost, DragTrack};

/// 行高要容得下 14px 标题行 + 12px 副标题行，合计约 51。
const ROW_H: f32 = theme::ROW_H_COMFORTABLE;

const INDEX_W: f32 = theme::ROW_INDEX_W;
const DURATION_W: f32 = 48.0;
/// 行首封面边长。行高 52 里上下各留 8px。
const COVER: f32 = 36.0;

const DIVIDER_H: f32 = 28.0;

pub struct QueuePanel {
    library: Entity<LibraryEntity>,
    shell: Entity<ShellEntity>,
    /// 行首封面。解码完成时会 notify，这一栏跟着重绘。
    covers: Entity<CoverCache>,
    _sub: Subscription,
    _shell_sub: Subscription,
    _covers_sub: Subscription,
    scroll: ListState,
    // `observe(library)` 在任何曲库变化时都会触发，所以记住上次定位的曲目，
    // 只在正在播放的那首真的换了时才滚动，否则用户翻看队列时会被拽回当前曲。
    last_anchored: Option<TrackId>,
    /// 缓存的轻量行模型。只存 `TrackId`，曲目数据在可视区里现查。
    rows: QueueRows,
    /// 行模型对应的队列代次、游标、当前曲；任一变化才重建。
    rows_key: Option<QueueKey>,
}

impl QueuePanel {
    pub fn new(
        library: Entity<LibraryEntity>,
        shell: Entity<ShellEntity>,
        covers: Entity<CoverCache>,
        cx: &mut Context<Self>,
    ) -> Self {
        // 换曲和切模式都改变队列，所以 observe 整个曲库实体。
        let sub = cx.observe(&library, |this, library, cx| {
            this.anchor(&library, cx);
            cx.notify();
        });
        // 浮层开关仅改变面板外观（关闭键显隐），但面板实体并非每次根视图重绘都会重渲染，
        // 需自行订阅外壳。
        let shell_sub = cx.observe(&shell, |_, _, cx| cx.notify());
        // 封面是异步解码的，落地时这一栏要重绘一次，否则行首一直挂着占位图。
        let covers_sub = cx.observe(&covers, |_, _, cx| cx.notify());
        Self {
            library,
            shell,
            covers,
            _sub: sub,
            _shell_sub: shell_sub,
            _covers_sub: covers_sub,
            scroll: ListState::new(0, ListAlignment::Top, px(ROW_H * 4.0)),
            last_anchored: None,
            rows: QueueRows::default(),
            rows_key: None,
        }
    }

    /// 队列代次、游标或当前曲变了才重建行模型；每帧几十次的重绘不再全量取曲目。
    fn sync_rows(&mut self, cx: &mut App) {
        let key = {
            let library = self.library.read(cx);
            QueueKey {
                revision: library.playlist().revision(),
                cursor: library.playlist().cursor(),
                playing: library.now_playing(),
            }
        };
        if self.rows_key == Some(key) {
            return;
        }

        let (played, upcoming, current) = {
            let library = self.library.read(cx);
            let playlist = library.playlist();
            let (before, after) = queue_sources(playlist.order(), playlist.cursor());
            (before, after, library.now_playing())
        };
        let rows = build_entries(played, upcoming, current);
        // 只把真正变化的区间交给列表重测，滚到别处时不会因为队尾增删被拉回顶部。
        let splice = {
            let old = &self.rows.entries;
            let new = &rows.entries;
            (old != new).then(|| {
                let shared = old.len().min(new.len());
                let prefix = old
                    .iter()
                    .zip(new.iter())
                    .take_while(|(a, b)| a == b)
                    .count();
                let suffix = old
                    .iter()
                    .rev()
                    .zip(new.iter().rev())
                    .take(shared - prefix)
                    .take_while(|(a, b)| a == b)
                    .count();
                (prefix..old.len() - suffix, new.len() - prefix - suffix)
            })
        };
        if let Some((range, count)) = splice {
            self.scroll.splice(range, count);
            self.scroll = self.scroll.clone().with_uniform_item_height(px(ROW_H));
        }
        self.rows = rows;
        self.rows_key = Some(key);
    }

    /// 换曲后把正在播的那一行滚进可视区。
    /// `Nearest` 而非 `Center` 与 `Top`。已在可视区内时不做任何移动，不打断用户自行翻看。
    fn anchor(&mut self, library: &Entity<LibraryEntity>, cx: &mut Context<Self>) {
        let current = library.read(cx).now_playing();
        if current == self.last_anchored {
            return;
        }
        self.last_anchored = current;
        if current.is_none() {
            return;
        }

        // 锚点是正在播的那一行本身，不是已播段。已播那段可能比它短，
        // 分段标签还要再占一行。行号只能由 `build_entries` 给出。
        self.sync_rows(cx);
        if let Some(row) = self.rows.current_row {
            self.scroll.scroll_to_reveal_item(row);
        }
    }
}

impl Render for QueuePanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.sync_rows(cx);
        let library = self.library.read(cx);
        let playing = library.now_playing();
        let mode = library.mode();
        let overlay = self.shell.read(cx).right_panel_overlays();
        // 待播段不截断。整条队列都要能滚到，列表本身是虚拟化的，多几行不增加渲染成本。
        // 行模型已经缓存，这里只读一个数字。
        let ahead = self.rows.ahead;
        // 「共 N 首」说的是队列，不是曲库。有队列时两者不相等（列着 4 首却写 131 首）。
        let total = library.queue_len();

        let header = div()
            .flex_none()
            .w_full()
            .min_w_0()
            .flex()
            .items_center()
            .gap(px(theme::GAP_SM))
            .px(px(theme::GAP_MD))
            .h(px(52.))
            .border_b_1()
            .border_color(theme::border())
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(px(1.))
                    .child(crate::common::section::eyebrow("Queue"))
                    .child(
                        div()
                            .text_size(px(theme::FONT_SUBTITLE))
                            .font_weight(theme::WEIGHT_TITLE)
                            .text_color(theme::text())
                            .child("播放队列"),
                    ),
            )
            .child(crate::common::section::field(thousands(total)))
            .child(mode_badge(mode))
            .when(overlay, |row| {
                row.child(panel_close_button("queue-close", &self.shell))
            });

        let summary = div()
            .flex_none()
            .w_full()
            .min_w_0()
            .truncate()
            .px(px(theme::GAP_MD))
            .py(px(theme::GAP_SM))
            .font(theme::field_font())
            .text_size(px(theme::FONT_CAPTION))
            .text_color(theme::text_muted())
            .child(if total == 0 {
                "曲库为空".to_owned()
            } else if playing.is_none() {
                format!("共 {} 首 │ 尚未开始播放", thousands(total))
            } else {
                format!(
                    "共 {} 首 │ 接下来 {} 首",
                    thousands(total),
                    thousands(ahead)
                )
            });

        let row_count = self.rows.entries().len();
        let rows = Rc::clone(&self.rows.entries);
        let library_entity = self.library.clone();
        let covers_entity = self.covers.clone();

        let body: AnyElement = if row_count == 0 {
            empty_queue(total)
        } else {
            list(self.scroll.clone(), move |ix, window, cx| {
                let Some(entry) = rows.get(ix) else {
                    return div().into_any_element();
                };
                match entry {
                    QueueEntry::Track { id, kind, number } => {
                        // 先拷出曲目再放开曲库读取，行元素的回调还要可变借用 `cx`。
                        let track = library_entity.read(cx).playlist().by_id(*id).cloned();
                        match track {
                            Some(track) => queue_row(
                                *number,
                                &track,
                                *kind,
                                &library_entity,
                                &covers_entity,
                                window,
                                cx,
                            )
                            .into_any_element(),
                            None => div().into_any_element(),
                        }
                    }
                    QueueEntry::Divider(label) => section_divider(label).into_any_element(),
                }
            })
            .size_full()
            .into_any_element()
        };

        div()
            .flex_none()
            .w(px(theme::RIGHT_PANEL_W))
            // 只写 `w()` 压不住内容。flex 默认 `min-width: auto`，长歌名会把面板顶宽，
            // 要 `min_w` + `max_w` 一起钉死。
            .min_w(px(theme::RIGHT_PANEL_W))
            .max_w(px(theme::RIGHT_PANEL_W))
            .overflow_hidden()
            .h_full()
            .flex()
            .flex_col()
            .bg(theme::panel())
            .child(header)
            .child(summary)
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .min_w_0()
                    .relative()
                    // 拖到空白处 = 接在当前曲的下一行。行内落点会先消费掉拖拽，所以这里只在
                    // 没压在任何一行上时才生效。
                    .on_drop::<DragTrack>({
                        let library = self.library.clone();
                        move |drag, _window, cx| {
                            library.update(cx, |library, cx| {
                                let at = library.playlist().cursor() + 1;
                                library.insert_in_queue(&[drag.id], at, cx);
                            });
                        }
                    })
                    .child(body)
                    .child(crate::common::scroll::scrollbar(
                        &self.scroll,
                        "queue-list-scrollbar",
                    )),
            )
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum QueueEntry {
    /// 一行曲目。`number` 是显示序号，按曲目数而不是行数递增。分段标签占一行但不占号。
    Track {
        id: TrackId,
        kind: RowKind,
        number: usize,
    },
    /// 带文字的刻度行。只在没有当前曲时标出「待播放」。
    Divider(&'static str),
}

/// 队列的行模型，包含行本身与正在播的那一行的下标。滚动锚点只能从这里拿。
/// 「已播那段有几行」就是光标的位置；刚开播时是空的，
/// 没有当前曲时「播放历史」标签还占一行。
#[derive(Default)]
struct QueueRows {
    /// 行模型只存 id。整条队列的 `Track` 每次重绘都 clone 一遍，那才是真正的大头。
    entries: Rc<Vec<QueueEntry>>,
    /// 正在播的那一行的下标。没有正在播的曲目、或它不在待播区里时为 `None`。
    current_row: Option<usize>,
    /// 当前曲之后还有几首（不含当前曲），摘要文案用。
    ahead: usize,
}

impl QueueRows {
    fn entries(&self) -> &[QueueEntry] {
        &self.entries
    }
}

/// 行模型的缓存键，队列结构、游标、当前曲任一变化都要重建。
#[derive(Clone, Copy, PartialEq, Eq)]
struct QueueKey {
    revision: u64,
    cursor: usize,
    playing: Option<TrackId>,
}

/// 队列面板要显示整条队列：光标之前的项也是队列成员，不能只留几首当上下文。
fn queue_sources(order: &[TrackId], cursor: usize) -> (Vec<TrackId>, Vec<TrackId>) {
    let cursor = cursor.min(order.len());
    (order[..cursor].to_vec(), order[cursor..].to_vec())
}

/// 把「已播与待播」两段拼成一条连续队列。有当前曲时不再插「接下来」标签，
/// 它直接接在已播记录后面；没有当前曲、两段都非空时才插「待播放」。序号按曲目连续、标签不占号。
/// `upcoming` 第一首只有与 `now_playing` 相同才算 [`RowKind::Current`]。
fn build_entries(
    played: Vec<TrackId>,
    upcoming: Vec<TrackId>,
    current: Option<TrackId>,
) -> QueueRows {
    let played_count = played.len();
    let ahead = upcoming.len().saturating_sub(1);
    let mut rows: Vec<QueueEntry> = Vec::with_capacity(played_count + upcoming.len() + 1);
    let mut number = 1;

    for id in played {
        rows.push(QueueEntry::Track {
            id,
            kind: RowKind::Played,
            number,
        });
        number += 1;
    }

    let mut rest = upcoming.into_iter();
    let mut current_row = None;
    if let Some(id) = rest.next() {
        let kind = if current == Some(id) {
            RowKind::Current
        } else {
            RowKind::Upcoming
        };
        // 这里数出来的才是真行号。上面已播那段在开头可能是空的。
        if kind == RowKind::Current {
            current_row = Some(rows.len());
        }
        rows.push(QueueEntry::Track { id, kind, number });
        number += 1;
    }
    for id in rest {
        rows.push(QueueEntry::Track {
            id,
            kind: RowKind::Upcoming,
            number,
        });
        number += 1;
    }

    // 当前曲本身就是分界，不再额外插「接下来」。
    if current.is_none() && played_count > 0 && rows.len() > played_count {
        rows.insert(played_count, QueueEntry::Divider("待播放"));
    }

    QueueRows {
        entries: Rc::new(rows),
        current_row,
        ahead,
    }
}

fn section_divider(label: &'static str) -> impl IntoElement {
    div()
        .flex_none()
        .h(px(DIVIDER_H))
        .flex()
        .items_center()
        .gap(px(theme::GAP_SM))
        .px(px(theme::GAP_MD))
        .child(div().flex_1().h(px(1.)).bg(theme::border_soft()))
        .child(
            div()
                .flex_none()
                .child(crate::common::section::section_heading(label)),
        )
        .child(div().flex_1().h(px(1.)).bg(theme::border_soft()))
}

fn empty_queue(total: usize) -> AnyElement {
    let (hint, detail) = if total == 0 {
        ("队列是空的", "先导入音乐，队列才会有内容")
    } else {
        ("尚未开始播放", "在上面挑一首开始，这里就会跟上")
    };

    div()
        .size_full()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap(px(theme::GAP_SM))
        .px(px(theme::GAP_LG))
        .child(icon(Glyph::Queue, 32., theme::text_faint()))
        .child(
            div()
                .text_size(px(theme::FONT_BODY))
                .text_color(theme::text_muted())
                .child(hint),
        )
        .child(
            div()
                .text_size(px(theme::FONT_CAPTION))
                .text_color(theme::text_faint())
                .child(detail),
        )
        .into_any_element()
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RowKind {
    Played,
    /// 正在播与按下播放会从这里开始。
    Current,
    Upcoming,
}

fn mode_badge(mode: PlayMode) -> impl IntoElement {
    let (label, glyph, active) = match mode {
        PlayMode::Sequential => ("顺序", Glyph::List, false),
        PlayMode::Shuffle => ("随机", Glyph::Shuffle, true),
        PlayMode::RepeatAll => ("列表循环", Glyph::RepeatAll, true),
        PlayMode::RepeatOne => ("单曲循环", Glyph::RepeatOne, true),
    };

    div()
        .flex_none()
        .flex()
        .items_center()
        .gap(px(theme::GAP_XS))
        .px(px(theme::GAP_SM))
        .py(px(2.))
        .bg(if active {
            theme::control()
        } else {
            theme::elevated()
        })
        .text_color(if active {
            theme::text_on_control()
        } else {
            theme::text_muted()
        })
        .text_size(px(theme::FONT_MICRO))
        .child(
            div()
                .flex()
                .items_center()
                .justify_center()
                .child(icon_inherit(glyph, 12.)),
        )
        .child(label)
}

/// 行内的半个落点。`below` 为真时插到这一行之后，否则之前。
/// 上半 + 下半拼起来正好铺满一行，落点线画在哪一半，曲子就插到哪一边。
fn queue_drop_zone(
    number: usize,
    below: bool,
    track_id: TrackId,
    library: &Entity<LibraryEntity>,
) -> AnyElement {
    let library = library.clone();
    div()
        .id((
            if below {
                "queue-drop-below"
            } else {
                "queue-drop-above"
            },
            number,
        ))
        .absolute()
        .left_0()
        .right_0()
        .h(px(ROW_H / 2.0))
        .when(below, |zone| zone.bottom(px(0.)))
        .when(!below, |zone| zone.top(px(0.)))
        .drag_over::<DragTrack>(move |style, _, _, _| {
            let style = style.border_color(theme::control());
            if below {
                style.border_b_2()
            } else {
                style.border_t_2()
            }
        })
        .on_drop::<DragTrack>(move |drag, _window, cx| {
            library.update(cx, |library, cx| {
                let pos = library
                    .playlist()
                    .order()
                    .iter()
                    .position(|id| *id == track_id);
                library.insert_in_queue(&[drag.id], drop_index(pos, below), cx);
            });
        })
        .into_any_element()
}

/// 落点 → `order` 里的插入下标。找不到这一行（曲目刚被删）时按队首算，别把整条队列往后挪。
fn drop_index(order_pos: Option<usize>, below: bool) -> usize {
    order_pos.map_or(0, |pos| if below { pos + 1 } else { pos })
}

/// 队列的一行。`kind` 决定配色，点一下跳到这一首。
/// `number` 兼作元素 id。id 跟着曲目而不是行数，换曲时标签增减才不会让滚动错位。
fn queue_row(
    number: usize,
    track: &Track,
    kind: RowKind,
    library: &Entity<LibraryEntity>,
    covers: &Entity<CoverCache>,
    window: &mut Window,
    cx: &mut App,
) -> impl IntoElement {
    let track_id = track.id;
    let duration = track
        .duration
        .map_or_else(|| "--:--".to_owned(), format_duration);
    let current = kind == RowKind::Current;
    let played = kind == RowKind::Played;
    let hover_id = ElementId::from(("queue-row", number));
    let hover = crate::common::hover::row_progress(hover_id.clone(), window, cx);

    let title_color = match kind {
        RowKind::Played => theme::text_muted(),
        RowKind::Current | RowKind::Upcoming => theme::text(),
    };
    let meta_color = match kind {
        RowKind::Played => theme::text_faint(),
        RowKind::Current | RowKind::Upcoming => theme::text_muted(),
    };

    // 每行独立的 group 名。共用一个的话鼠标进任意一行、所有行都会亮。
    let group: SharedString = format!("queue-row-{number}").into();

    let mut row = div()
        .id(hover_id.clone())
        .group(group.clone())
        .flex_none()
        .relative()
        // `uniform_list` 的行默认按内容定宽。`w_full` 给时长列右对齐的参照系，
        // `min_w_0` 才允许标题列收缩。
        .w_full()
        .min_w_0()
        .h(px(ROW_H))
        .flex()
        .items_center()
        .pl(px(
            theme::ROW_PAD_LEFT + if current { 0.0 } else { 6.0 * hover }
        ))
        .pr(px(theme::GAP_MD))
        .cursor_pointer()
        .border_b_1()
        .border_color(theme::border_soft())
        .bg(theme::transparent())
        .when(!current, |row| {
            row.on_hover(crate::common::hover::bind(hover_id))
        });

    if !current {
        row = row
            .child(div().absolute().inset_0().bg(theme::hover()).opacity(hover))
            .child(
                div()
                    .absolute()
                    .left(px(0.))
                    .top(px(0.))
                    .w(px(4.))
                    .h(relative(hover))
                    .bg(theme::trace()),
            );
    }

    row = row.on_click({
        let library = library.clone();
        move |_, _, cx| {
            library.update(cx, |library, cx| {
                library.activate(track_id, cx);
            });
        }
    });

    // 上/下两半各是一个落点：上半插到这一行之前，下半插到之后。
    // 插入位置在落的那一刻按曲目在 `order` 里的下标算，不能用行号：分段标签也占行号。
    row = row
        .child(queue_drop_zone(number, false, track_id, library))
        .child(queue_drop_zone(number, true, track_id, library));

    row = row
        // 竖条不用 `border`，那会把内容整体往右推，切换当前曲时整列文字横跳。
        .child(
            div()
                .flex_none()
                .w(px(2.))
                .h(px(ROW_H * 0.5))
                .bg(if current {
                    theme::accent()
                } else {
                    theme::transparent()
                }),
        )
        .child(index_cell(
            number,
            current,
            group.clone(),
            track_id,
            library,
        ))
        .child(
            div()
                .flex_none()
                .pr(px(theme::GAP_SM))
                .child(crate::common::cover::thumb(
                    covers,
                    library,
                    track_id,
                    COVER,
                    theme::RADIUS_SM,
                    Glyph::Music,
                    crate::common::cover::initial_of(&track.title),
                    cx,
                )),
        )
        .child(title_cell(number, track, title_color, meta_color));

    // 队列行自己也能拖：拖到别的行上就是重排（`queue_insert` 对已在队列里的曲目是挪位）。
    row = row.on_drag(
        DragTrack {
            id: track_id,
            title: SharedString::from(track.title.clone()),
        },
        |drag: &DragTrack, _, _, cx| cx.new(|_| DragGhost::new(drag.title.clone())),
    );

    // 时长列固定宽度而不是 `flex_1`，后者的右边界取决于标题长度，会把这一列顶宽。
    let mut row = row.child(
        div()
            .flex_none()
            .w(px(DURATION_W))
            .flex()
            .justify_end()
            .text_size(px(theme::FONT_CAPTION))
            .font(theme::field_font())
            .text_color(if played {
                theme::text_faint()
            } else {
                theme::text_muted()
            })
            .child(duration),
    );

    // 引擎解不了的格式照常列出，只压暗并给出原因；队列推进会跳过它们，点击只得到一条提示。
    if let Some(reason) = track.unplayable_reason() {
        row = row
            .opacity(theme::UNPLAYABLE_OPACITY)
            .tooltip(crate::common::tooltip::builder(reason));
    }

    // 这里的「播放」只能跳播、不能重设队列。重设会把随机顺序洗回曲库顺序。
    let hooks = crate::common::menu::TrackMenuHooks::none()
        .in_queue()
        .play({
            let library = library.clone();
            move |id, cx| {
                library.update(cx, |library, cx| {
                    library.activate(id, cx);
                });
            }
        });
    row.context_menu(crate::common::menu::track_context_menu(
        library.clone(),
        track_id,
        hooks,
    ))
}

/// 序号位，平时是序号，悬停时换成 ▶，点一下跳到这一首。
/// 两层重叠在同一格，靠 `group_hover` 切透明度。若逐行记 `hovered`，滑过每行都要 `notify`。
fn index_cell(
    number: usize,
    current: bool,
    group: SharedString,
    track_id: lyre_core::TrackId,
    library: &Entity<LibraryEntity>,
) -> impl IntoElement {
    if current {
        return div()
            .flex_none()
            .w(px(INDEX_W))
            .flex()
            .items_center()
            .justify_end()
            .pr(px(theme::ROW_INDEX_GAP))
            .child(
                div()
                    .relative()
                    .flex()
                    .items_center()
                    .justify_center()
                    .w(px(24.))
                    .h(px(18.))
                    .bg(theme::accent())
                    .child(crate::common::ornament::cut_br(theme::CUT_SM, theme::bg()))
                    .child(
                        div()
                            .font(theme::field_font())
                            .text_size(px(theme::FONT_CAPTION))
                            .text_color(theme::text_on_accent())
                            .child(format!("{number}")),
                    ),
            )
            .into_any_element();
    }

    div()
        .id(("queue-index", number))
        .flex_none()
        .relative()
        .w(px(INDEX_W))
        .flex()
        .items_center()
        .justify_end()
        .pr(px(theme::ROW_INDEX_GAP))
        .cursor_pointer()
        .on_click({
            let library = library.clone();
            move |_, _, cx| {
                // 必须阻断冒泡。整行上已经有一个 `activate` 回调，不阻断的话一次点击会重播两遍。
                cx.stop_propagation();
                library.update(cx, |library, cx| {
                    library.activate(track_id, cx);
                });
            }
        })
        .child(
            div()
                .text_size(px(theme::FONT_CAPTION))
                .text_color(theme::text_faint())
                .group_hover(group.clone(), |style| style.opacity(0.0))
                .child(format!("{number}")),
        )
        .child(
            div()
                .absolute()
                .right(px(theme::ROW_INDEX_GAP))
                .top(relative(0.5))
                .mt(px(-6.))
                .size(px(12.))
                .flex()
                .items_center()
                .justify_center()
                .opacity(0.0)
                .group_hover(group, |style| style.opacity(1.0))
                .child(icon(Glyph::Play, 10., theme::text())),
        )
        .into_any_element()
}

/// 标题格。第一行歌名，第二行「艺术家 · 专辑」。
/// 队列行第二行写什么。只写艺术家：队列是有序的行，专辑名在这里不参与辨认，写上去只是占宽。
/// 没有艺术家时退回专辑，但专辑若等于文件所在文件夹名（扫描时拿文件夹名兜的底），
/// 就当它没填——否则整列都是同一个文件夹名，一眼看过去像全都一样。
fn queue_meta(track: &Track) -> String {
    if let Some(artist) = track
        .artist
        .as_deref()
        .map(str::trim)
        .filter(|a| !a.is_empty())
    {
        return artist.to_owned();
    }

    let Some(album) = track
        .album
        .as_deref()
        .map(str::trim)
        .filter(|a| !a.is_empty())
    else {
        return String::new();
    };

    let folder = track
        .path
        .parent()
        .and_then(|path| path.file_name())
        .map(|name| name.to_string_lossy().into_owned());
    if folder.as_deref() == Some(album) {
        return String::new();
    }
    album.to_owned()
}

fn title_cell(
    number: usize,
    track: &Track,
    title_color: Rgba,
    meta_color: Rgba,
) -> impl IntoElement {
    let subtitle = queue_meta(track);

    let title_line = div()
        .flex()
        .items_center()
        .gap(px(theme::GAP_SM))
        .w_full()
        .min_w_0()
        .child(
            div()
                .id(("queue-title", number))
                .min_w_0()
                .truncate()
                .text_size(px(theme::FONT_TRACK))
                .text_color(title_color)
                .tooltip(crate::common::tooltip::builder(track.title.clone()))
                .child(track.title.clone()),
        );

    div()
        .flex_1()
        .min_w_0()
        .flex()
        .flex_col()
        .justify_center()
        .gap(px(1.))
        .child(title_line)
        .when(!subtitle.is_empty(), |cell| {
            cell.child(
                div()
                    .text_size(px(theme::FONT_CAPTION))
                    .text_color(meta_color)
                    .truncate()
                    .child(subtitle),
            )
        })
}

#[cfg(test)]
mod tests {
    // 不用 glob 导入，`platform::*` 里的 gpui `test` 宏会遮蔽内置 `#[test]`，导致无限递归。
    use super::{QueueEntry, QueueRows, RowKind, build_entries, drop_index, queue_sources};
    use lyre_core::TrackId;
    use std::path::Path;

    /// 落到下半截要插到这一行后面，上半截插到前面；行消失时按队首算。
    #[test]
    fn a_drop_above_goes_before_the_row_and_below_goes_after() {
        assert_eq!(drop_index(Some(3), false), 3);
        assert_eq!(drop_index(Some(3), true), 4);
        assert_eq!(drop_index(Some(0), true), 1);
        assert_eq!(drop_index(None, true), 0, "行不在了也不该把整条队列往后挪");
    }

    fn id(name: &str) -> TrackId {
        TrackId::from_path(Path::new(&format!("/music/{name}.mp3")))
    }

    fn ids(names: &[&str]) -> Vec<TrackId> {
        names.iter().map(|name| id(name)).collect()
    }

    fn track_ids(rows: &QueueRows) -> Vec<TrackId> {
        rows.entries()
            .iter()
            .filter_map(|row| match row {
                QueueEntry::Track { id, .. } => Some(*id),
                QueueEntry::Divider(_) => None,
            })
            .collect()
    }

    fn kinds(rows: &QueueRows) -> Vec<RowKind> {
        rows.entries()
            .iter()
            .filter_map(|row| match row {
                QueueEntry::Track { kind, .. } => Some(*kind),
                QueueEntry::Divider(_) => None,
            })
            .collect()
    }

    fn numbers(rows: &QueueRows) -> Vec<usize> {
        rows.entries()
            .iter()
            .filter_map(|row| match row {
                QueueEntry::Track { number, .. } => Some(*number),
                QueueEntry::Divider(_) => None,
            })
            .collect()
    }

    fn divider_at(rows: &QueueRows) -> Option<usize> {
        rows.entries()
            .iter()
            .position(|row| matches!(row, QueueEntry::Divider(_)))
    }

    fn row_marked_current(rows: &QueueRows) -> Option<usize> {
        rows.entries().iter().position(|row| {
            matches!(
                row,
                QueueEntry::Track {
                    kind: RowKind::Current,
                    ..
                }
            )
        })
    }

    #[test]
    fn first_upcoming_is_the_current_track() {
        // `upcoming()` 从 cursor 起取，第一首就是光标所在那首；错了整条分界线会偏一首。
        let rows = build_entries(ids(&["a", "b"]), ids(&["c", "d", "e"]), Some(id("c")));

        assert_eq!(track_ids(&rows), ids(&["a", "b", "c", "d", "e"]));
        assert_eq!(
            kinds(&rows),
            [
                RowKind::Played,
                RowKind::Played,
                RowKind::Current,
                RowKind::Upcoming,
                RowKind::Upcoming,
            ]
        );
    }

    #[test]
    fn the_cursor_row_is_not_current_until_it_actually_plays() {
        let rows = build_entries(Vec::new(), ids(&["a", "b"]), None);

        assert_eq!(kinds(&rows), [RowKind::Upcoming, RowKind::Upcoming]);
        assert_eq!(track_ids(&rows), ids(&["a", "b"]));
    }

    #[test]
    fn a_different_track_playing_leaves_the_cursor_row_upcoming() {
        let rows = build_entries(vec![id("a")], ids(&["b", "c"]), Some(id("a")));

        assert_eq!(
            kinds(&rows),
            [RowKind::Played, RowKind::Upcoming, RowKind::Upcoming]
        );
    }

    #[test]
    fn no_divider_between_played_and_current() {
        let rows = build_entries(vec![id("a")], ids(&["b", "c"]), Some(id("b")));

        assert_eq!(divider_at(&rows), None);
        assert_eq!(rows.entries().len(), 3, "1 已播 + 2 曲目");
    }

    #[test]
    fn no_divider_when_nothing_has_played_yet() {
        let rows = build_entries(Vec::new(), ids(&["a", "b"]), Some(id("a")));

        assert_eq!(divider_at(&rows), None);
        assert_eq!(track_ids(&rows), ids(&["a", "b"]));
    }

    #[test]
    fn no_divider_when_there_is_nothing_ahead() {
        let rows = build_entries(ids(&["a", "b"]), Vec::new(), Some(id("b")));

        assert_eq!(divider_at(&rows), None);
        assert_eq!(track_ids(&rows), ids(&["a", "b"]));
    }

    #[test]
    fn divider_only_marks_pending_when_stopped() {
        let playing = build_entries(vec![id("a")], vec![id("b")], Some(id("b")));
        let stopped = build_entries(vec![id("a")], vec![id("b")], None);

        let label = |rows: &QueueRows| match rows.entries().iter().find_map(|r| match r {
            QueueEntry::Divider(l) => Some(*l),
            // 显式列出变体而不是 `_`，给 `QueueEntry` 加新变体时这里才会编译失败。
            QueueEntry::Track { .. } => None,
        }) {
            Some(l) => l,
            None => panic!("这一段应当有分段标签"),
        };

        assert_eq!(divider_at(&playing), None, "有当前曲时不再插「接下来」");
        assert_eq!(label(&stopped), "待播放");
    }

    #[test]
    fn empty_queue_stays_empty() {
        let rows = build_entries(Vec::new(), Vec::new(), None);
        assert!(rows.entries().is_empty());
        assert_eq!(rows.current_row, None);
        assert_eq!(rows.ahead, 0);
    }

    #[test]
    fn ahead_counts_only_the_tracks_after_the_current_one() {
        let rows = build_entries(vec![id("a")], ids(&["b", "c", "d"]), Some(id("b")));
        assert_eq!(rows.ahead, 2, "当前曲不算「接下来」");
    }

    #[test]
    fn track_numbers_stay_contiguous_across_the_divider() {
        let rows = build_entries(ids(&["a", "b", "c"]), ids(&["d", "e"]), None);

        assert_eq!(divider_at(&rows), Some(3), "先确认标签确实插在中间");
        assert_eq!(numbers(&rows), [1, 2, 3, 4, 5], "序号必须连续");
    }

    #[test]
    fn track_numbers_start_at_one_without_a_divider() {
        let rows = build_entries(Vec::new(), ids(&["a", "b"]), Some(id("a")));
        assert_eq!(divider_at(&rows), None);
        assert_eq!(numbers(&rows), [1, 2]);
    }

    /// 点击曲库中靠后的歌时，光标前的歌也属于队列，面板必须把整条 order 都列出来。
    #[test]
    fn the_queue_panel_lists_the_entire_order() {
        let order: Vec<TrackId> = (0..10).map(|i| id(&format!("t{i}"))).collect();
        let (before, upcoming) = queue_sources(&order, 7);
        assert_eq!(before.len(), 7, "光标前的项不能截成几首上下文");
        assert_eq!(upcoming.len(), 3);

        let rows = build_entries(before, upcoming, Some(order[7]));
        assert_eq!(rows.entries().len(), 10, "整条队列都要在行模型里");
        assert_eq!(numbers(&rows), (1..=10).collect::<Vec<usize>>());
    }

    /// 滚动锚点是正在播的那一行本身；已播段无论多长，锚点都不能取一个固定常数。
    #[test]
    fn the_anchor_is_the_current_row_however_long_the_played_block_is() {
        for played in 0..=8 {
            let played_tracks: Vec<TrackId> = (0..played).map(|i| id(&format!("p{i}"))).collect();
            let head = id("cur");
            let upcoming = vec![head, id("n1"), id("n2")];

            let rows = build_entries(played_tracks, upcoming, Some(head));
            assert_eq!(
                rows.current_row,
                row_marked_current(&rows),
                "已播 {played} 首时，锚点必须正好落在标着「当前」的那一行"
            );
        }
    }

    /// 刚开播时已播区是空的，当前就是第 0 行，锚点不能取固定常数。
    #[test]
    fn a_queue_that_just_started_anchors_on_row_zero() {
        let head = id("cur");
        let rows = build_entries(Vec::new(), vec![head, id("n1")], Some(head));

        assert_eq!(divider_at(&rows), None, "没有已播就不该有分段标签");
        assert_eq!(rows.current_row, Some(0));
    }

    /// 没有分段标签时，已播几首，当前就在第几行。
    #[test]
    fn played_context_keeps_the_anchor_on_the_next_row() {
        let head = id("cur");
        let rows = build_entries(vec![id("p0")], vec![head, id("n1")], Some(head));

        assert_eq!(divider_at(&rows), None);
        assert_eq!(rows.current_row, Some(1), "已播 1 行，当前直接接在第 1 行");
    }

    /// 没在播时下面那段是「待播放」，第一行不算「当前」，没有锚点。
    #[test]
    fn there_is_no_anchor_when_nothing_is_playing() {
        let rows = build_entries(vec![id("p0")], ids(&["a", "b"]), None);

        assert_eq!(row_marked_current(&rows), None);
        assert_eq!(rows.current_row, None);
    }

    /// 正在播的那首不在待播区里时同样没有锚点，此时一行高亮都没有。
    #[test]
    fn there_is_no_anchor_when_the_playing_track_is_off_the_upcoming_block() {
        let rows = build_entries(vec![id("p0")], ids(&["a", "b"]), Some(id("p0")));

        assert_eq!(rows.current_row, None);
    }

    /// 第二行优先写艺术家，不写专辑。
    #[test]
    fn the_second_line_prefers_the_artist() {
        let mut track = lyre_core::Track::fallback(Path::new("/music/TestMusic/x.wav"));
        track.artist = Some("陈屿".into());
        assert_eq!(super::queue_meta(&track), "陈屿");
    }

    /// 一个标签都没有的曲目：`subtitle()` 会拿文件夹名兜底，队列这一列不要那个兜底。
    #[test]
    fn a_track_without_tags_gets_no_second_line() {
        let track = lyre_core::Track::fallback(Path::new("/music/TestMusic/x.wav"));
        assert_eq!(track.subtitle(), "TestMusic", "确认兜底写的就是文件夹名");
        assert_eq!(super::queue_meta(&track), "");
    }

    /// 真的填了专辑、又没有艺术家时，专辑还是要显示，否则这一行什么都没写。
    #[test]
    fn a_real_album_still_shows_when_there_is_no_artist() {
        let mut track = lyre_core::Track::fallback(Path::new("/music/TestMusic/x.wav"));
        track.album = Some("夜の航路".into());
        assert_eq!(super::queue_meta(&track), "夜の航路");
    }
}
