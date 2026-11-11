//! 歌词全屏视图。盖在主区卡片上，当前句居中、逐字点亮、点击跳转。
//! 重绘只有两个来源，即换行与换字各一次，以及正在填的那一格每 `SWEEP_STEP` 一次。
//! 后者只在确有一格字正在被填时推帧，其余时间这一屏静止。

use std::cell::Cell;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::{Duration, Instant};

use capstan_core::{LyricLine, Lyrics, Track, UNKNOWN_ARTIST, format_duration};

use crate::entities::PlaybackEntity;
use crate::icon::Glyph;
use crate::platform::*;
use crate::shell::ShellEntity;
use crate::theme;

/// 文字盒高 = 字号 × 这个比例。gpui 默认行高是字号的 1.618 倍，用它会使原文盒高 52 远大于译文盒。
const LINE_BOX_RATIO: f32 = 1.3;
/// 药丸的上下内边距，左右用 `GAP_LG`。
const PILL_PAD_Y: f32 = theme::GAP_XS;
const ROW_GAP: f32 = theme::GAP_SM;
/// 整列共用的基准字号档位，从大到小取第一个放得下典型长句的一档。
const BASE_TIERS: [f32; 3] = [theme::FONT_HERO, 26.0, 22.0];
/// 最小基准字号，必须与 `BASE_TIERS` 末项一致。
const BASE_FLOOR: f32 = 22.0;
const _: () = assert!(BASE_FLOOR == BASE_TIERS[BASE_TIERS.len() - 1]);
/// 挑档位时按多长的句子算「放得下」。再长的让它折行，而不是把整列拖小。
const LONG_LINE_EM: f32 = 20.0;
/// 邻行相对当前行的字号比例。用比例而非固定 px 差，基准降到 22 时再减 8 就只剩 14。
const IDLE_RATIO: f32 = 0.7;
/// 邻行字号的可读下限。基准最低为 22，`22 × 0.7 = 15.4` 会触及该下限。
const IDLE_FLOOR: f32 = 16.0;
/// 译文相对原文的字号差，单位 px。
const TRANSLATION_STEP: f32 = 4.0;
/// 邻行按距离衰减的不透明度。gpui 的元素样式没有 `blur`，用它代替雾化。
const OPACITY_NEAR: f32 = 0.5;
const OPACITY_FAR: f32 = 0.22;
/// 当前行里还没唱到的字的不透明度，必须高于邻行。
const OPACITY_UNSUNG: f32 = 0.6;
const _: () = assert!(OPACITY_UNSUNG > OPACITY_NEAR);
/// 逐字扫过的柔化带宽度，占一格的多少。canvas 逐像素绘制，已唱段实心，
/// 末尾按该宽度由亮渐隐到暗；取约三分之一字身，接近卡拉OK字幕的渐变。
const WIPE_FEATHER: f32 = 0.35;
/// 逐字填充的刷新间隔。这一屏每帧重排一次，帧率直接决定成本，间隔到了才画。
/// 一格的演唱时长在 150–400 ms，60 Hz 已是每格 9–24 档，更密无法分辨，只增加 CPU 开销。
const SWEEP_STEP: Duration = Duration::from_millis(16);

/// 换行滚动的刷新间隔。呈现不等垂直同步，后端为 `Present(0)`，由 DWM 合成上屏，
/// 因此只能多画几帧，保证每次合成都有新画面；滚动是 260 ms 内整列位移，比逐字填充快一个数量级，
/// 帧率接近 60 Hz 时会周期性把同一帧合成两次，表现为丢帧。
const SCROLL_STEP: Duration = Duration::from_millis(12);

/// 换行滚动的时长。缓动起步快、贴近目标时收住，不回弹。
const SCROLL_SECONDS: f32 = 0.26;
/// 缓动结束后多补一帧的额度。`is_animating` 在渲染之前判断，只按 260 ms 判定
/// 会把终点那一帧排除在外，最后一帧停在 99%，直到下一次换行才补齐。
const SCROLL_SETTLE: Duration = Duration::from_millis(24);

/// 空状态的眉标。比正文里的 `// ...` 大一档，否则会被下方 30px 的标题压住。
const NOTICE_EYEBROW: f32 = 15.0;
/// 入场位移的时长，比遮罩自身的淡入晚到位。
const MOTION_IN_MS: u64 = 320;
/// 正文比页头晚这一档起步，两段位移才分得开。
const MOTION_BODY_DELAY_MS: u64 = 38;

#[derive(Clone, Debug)]
enum Loaded {
    Idle,
    Loading,
    Ready(Rc<Lyrics>),
    /// 确认没有同名 `.lrc` / `.ass`，或读不出内容。
    Missing,
}

/// 歌词列的滚动位置，按时间缓动。`with_spring` 在动画期间随刷新率持续补帧，
/// 每次换行都会重绘整个窗口；这里由逐字节拍顺带推进。
struct Scroll {
    from: f32,
    to: f32,
    started: Option<Instant>,
}

impl Scroll {
    fn value(&self) -> f32 {
        let Some(started) = self.started else {
            return self.to;
        };
        let t = (started.elapsed().as_secs_f32() / SCROLL_SECONDS).min(1.0);
        // ease-out cubic，起步快、贴近目标时收住。
        let eased = 1.0 - (1.0 - t).powi(3);
        self.from + (self.to - self.from) * eased
    }

    /// 目标变了才重开一段缓动。半像素以内视为没变，避免高度取整带来的抖动。
    fn animate_to(&mut self, to: f32) {
        if (to - self.to).abs() < 0.5 {
            return;
        }
        self.from = self.value();
        self.to = to;
        self.started = Some(Instant::now());
    }

    fn is_animating(&self) -> bool {
        let Some(started) = self.started else {
            return false;
        };
        started.elapsed().as_secs_f32() < SCROLL_SECONDS + SCROLL_SETTLE.as_secs_f32()
    }
}

pub struct LyricsView {
    playback: Entity<PlaybackEntity>,
    shell: Entity<ShellEntity>,
    state: Loaded,
    /// `state` 属于哪首曲目。换曲靠它识别，也用来丢弃过期请求的结果。
    path: Option<PathBuf>,
    /// 当前行与当前字。只在它们变化时 `notify`，把跟随进度的重绘压到每句每字一次。
    active: Option<usize>,
    word: Option<usize>,
    /// 逐字填充的节拍器。见 [`Self::new`] 里的说明。
    _sweep_clock: Task<()>,
    /// 槽位高度表，与 [`Self::heights_key`] 配套。`slot_height` 要对每一行折行，
    /// 每帧重算的开销随曲长线性增长，100 行 × 60 fps 即每秒六千次折行。
    heights: Vec<f32>,
    /// `heights` 对应的歌词与正文宽度。歌词用 `Rc` 地址判定，换曲即换地址。
    heights_key: Option<(usize, f32)>,
    /// 列的滚动位置。换行时缓动过去，暂停时仍会走完。
    scroll: Scroll,
    /// 当前句视图。逐字节拍只通知它；它挂在遮罩层上与本视图并列，不是本视图的子树。
    active_row: Entity<ActiveRow>,
    /// 遮罩原点（窗口坐标）。本视图的根与正文列各有一块 canvas 在 prepaint 时写入，
    /// `ActiveRow` 按两者之差把正文列的矩形换算成遮罩内坐标。只有改窗口尺寸时才会变。
    overlay_origin: Rc<Cell<Option<Point<Pixels>>>>,
    /// 正文列矩形（窗口坐标）。
    body_rect: Rc<Cell<Option<Bounds<Pixels>>>>,
    /// 正文列宽预算。`render` 写一次，本视图与当前句读它定字号与折行点。
    budget: f32,
    /// 本帧的滚动偏移。本视图据此平移整列，当前句据此把行定位到同一条锚线上。
    shown_offset: f32,
    _subs: Vec<Subscription>,
}

impl LyricsView {
    pub fn new(
        playback: Entity<PlaybackEntity>,
        shell: Entity<ShellEntity>,
        cx: &mut Context<Self>,
    ) -> Self {
        // observe 而不是 subscribe。进度本身不发事件，只会 `notify` 播放实体。
        let observe = cx.observe(&playback, |this, _, cx| this.on_playback(cx));
        // 暂停时播放实体不再通知，遮罩打开那一刻要靠外壳的这次通知补上当前句。
        let watch = cx.observe(&shell, |this, _, cx| this.sync_active(cx));
        // 逐字填充按播放位置计算，不用 `with_animation`，后者每帧重排整叠裁剪层。
        // 这里按固定相位推帧，间隔以绝对时刻对齐；滚动与填充各用各的间隔，见两个常量。
        let executor = cx.background_executor().clone();
        let sweep_clock = cx.spawn(async move |this, cx| {
            let mut step = SWEEP_STEP;
            loop {
                executor.timer(step).await;
                // 只有滚动的缓动与正在填的那一格需要补帧。字间空隙与整行唱完后的间奏
                // 这条链是静止的，继续重绘整屏只增加 CPU 开销。
                step = match this.update(cx, |this, cx| {
                    let scrolling = this.scroll.is_animating();
                    let sweeping = scrolling || this.sweep_running(cx);
                    if scrolling {
                        // 整列都要随偏移平移，这一档无法省略。
                        cx.notify();
                    }
                    if sweeping {
                        // 当前句与本视图并列，通知它不会把整列标脏。
                        this.active_row.update(cx, |_, cx| cx.notify());
                    }
                    if scrolling { SCROLL_STEP } else { SWEEP_STEP }
                }) {
                    Ok(step) => step,
                    Err(_) => break,
                };
            }
        });
        // 当前句只持本视图的弱句柄，状态只有一份。创建时本实体尚未就位，
        // `upgrade` 在此刻会失败，渲染期不会。
        let weak = cx.weak_entity();
        let active_row = cx.new(|_| ActiveRow::new(weak));
        Self {
            playback,
            shell,
            state: Loaded::Idle,
            path: None,
            active: None,
            word: None,
            _sweep_clock: sweep_clock,
            heights: Vec::new(),
            heights_key: None,
            scroll: Scroll {
                from: 0.0,
                to: 0.0,
                started: None,
            },
            active_row,
            overlay_origin: Rc::new(Cell::new(None)),
            body_rect: Rc::new(Cell::new(None)),
            budget: 0.0,
            shown_offset: 0.0,
            _subs: vec![observe, watch],
        }
    }

    /// 当前句的逐字时间轴是否仍在推进。只有这一段按帧重绘，开唱前、整行唱完后的间奏与
    /// 无逐字轴的行只有静止的两档亮度。判据是位置落在第一格与最后一格之间，不用 `word`，
    /// 因为后者由 10 Hz 进度泵更新，换字时最多滞后 100 ms，据它门控会误判为已唱完而停帧。
    fn sweep_running(&self, cx: &App) -> bool {
        if !self.shell.read(cx).shows_lyrics() || !self.playback.read(cx).is_playing() {
            return false;
        }
        let Loaded::Ready(lyrics) = &self.state else {
            return false;
        };
        let Some(line) = self.active.and_then(|index| lyrics.lines.get(index)) else {
            return false;
        };
        let (Some(first), Some(last)) = (line.words.first(), line.words.last()) else {
            return false;
        };
        let position = self.playback.read(cx).position();
        position >= first.start && position < last.end
    }

    /// 已解析好的歌词。当前句每帧从它读取，取不到即为空状态。
    fn lyrics(&self) -> Option<&Lyrics> {
        match &self.state {
            Loaded::Ready(lyrics) => Some(lyrics),
            Loaded::Idle | Loaded::Loading | Loaded::Missing => None,
        }
    }

    /// 重算槽位高度表。整列每帧都要用它，但只有歌词更换或正文宽度变化时才需要重算。
    fn ensure_heights(&mut self, lyrics: &Rc<Lyrics>, budget: f32) {
        let key = (Rc::as_ptr(lyrics) as usize, budget);
        if self.heights_key == Some(key) {
            return;
        }
        self.heights = lyrics
            .lines
            .iter()
            .map(|line| slot_height(line, budget))
            .collect();
        self.heights_key = Some(key);
    }

    /// 作废与当前这份歌词绑定的派生数据。换曲与换歌词都必须调用，
    /// 否则高度表会用上一首的行高排这一首。
    fn forget_lyrics(&mut self, cx: &mut Context<Self>) {
        self.active = None;
        self.word = None;
        self.heights.clear();
        self.heights_key = None;
        self.refresh_active_row(cx);
    }

    /// 播放实体每次 `notify` 都会走到这里，约 10Hz，但只有换曲、换行、换字才可能重绘。
    fn on_playback(&mut self, cx: &mut Context<Self>) {
        let current = self
            .playback
            .read(cx)
            .current()
            .map(|track| track.path.clone());

        match current {
            Some(path)
                if self.path.as_deref() != Some(path.as_path())
                    || matches!(self.state, Loaded::Idle) =>
            {
                self.request(path, cx);
            }
            None if !matches!(self.state, Loaded::Idle) => {
                self.path = None;
                self.state = Loaded::Idle;
                self.forget_lyrics(cx);
                cx.notify();
            }
            _ => {}
        }

        self.sync_active(cx);
    }

    /// 后台读侧文件。发起时不 `notify`。遮罩要么还没打开，要么显示的仍是上一句。
    fn request(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        self.path = Some(path.clone());
        self.state = Loaded::Loading;
        self.forget_lyrics(cx);

        // `background_executor()` 返回借用，必须 clone 才能带进 'static 任务。
        let executor = cx.background_executor().clone();
        let loaded_for = path.clone();
        cx.spawn(async move |this, cx| {
            let loaded = executor
                .spawn(async move { capstan_core::lyrics::load_for_audio(&loaded_for) })
                .await;
            this.update(cx, |this, cx| {
                // 期间又换过曲就丢弃这份结果，否则会把上一首的歌词贴到当前这首上。
                if this.path.as_deref() == Some(path.as_path()) {
                    this.state = match loaded {
                        Some(lyrics) => Loaded::Ready(Rc::new(lyrics)),
                        None => Loaded::Missing,
                    };
                    this.forget_lyrics(cx);
                    cx.notify();
                    // 读完后补算当前句。暂停时播放实体不会再通知，缺这一步要等下一次播放。
                    this.sync_active(cx);
                }
            })
            .ok();
        })
        .detach();
    }

    /// 逐字层。它由遮罩层置于**本视图的兄弟**位置，见 [`ActiveRow`] 的说明。
    pub fn active_row(&self) -> &Entity<ActiveRow> {
        &self.active_row
    }

    /// 让当前句视图重新读取本视图的状态。换行、换词与换歌词都必须调用。
    fn refresh_active_row(&self, cx: &mut Context<Self>) {
        self.active_row.update(cx, |_, cx| cx.notify());
    }

    /// 重算当前行与当前字。遮罩关着就不动，两者都没变也不重绘。
    fn sync_active(&mut self, cx: &mut Context<Self>) {
        if !self.shell.read(cx).shows_lyrics() {
            return;
        }
        let Loaded::Ready(lyrics) = &self.state else {
            return;
        };
        let position = self.playback.read(cx).position();
        let next = lyrics.active_index(position);
        let next_word = next.and_then(|index| lyrics.lines[index].word_index(position));
        if next != self.active {
            self.active = next;
            self.word = next_word;
            // 换行时整列的邻行字号与浓淡都变，列与当前句都要重画。
            self.refresh_active_row(cx);
            cx.notify();
        } else if next_word != self.word {
            self.word = next_word;
            // 换字时只有当前句的字色变化，静态行不动。
            self.refresh_active_row(cx);
        }
    }
}

fn seek_handler(
    playback: &Entity<PlaybackEntity>,
    start: Duration,
) -> impl Fn(&ClickEvent, &mut Window, &mut App) + 'static {
    let playback = playback.clone();
    move |_, _, cx| playback.update(cx, |playback, cx| playback.seek(start, cx))
}

/// 单个字符的估计宽度。CJK、假名与全角标点 ≈ 1 em，其余 ASCII ≈ 0.52 em，
/// 空格 0.30 em，粗体 ×1.06。只用来数折行的行数，估大比估小安全。
fn char_width(c: char, size: f32, bold: bool) -> f32 {
    let em = match c {
        ' ' => 0.30,
        c if c < '\u{2E80}' => 0.52,
        _ => 1.0,
    };
    em * size * if bold { 1.06 } else { 1.0 }
}

/// 一行在给定字号下的估计宽度。
fn text_width(text: &str, size: f32, bold: bool) -> f32 {
    text.chars().map(|c| char_width(c, size, bold)).sum()
}

/// 可以断在这些字符之后。文本系统按字符切，会把「越えたら」拆到两行，
/// 第二行只剩半句；先按标点断开，一行才是完整的短语。
fn breaks_after(c: char) -> bool {
    matches!(
        c,
        '、' | '，' | '。' | '！' | '？' | '；' | '：' | '…' | ' '
    )
}

/// 标点断点至少占满这一比例的行宽才采用。太靠前的标点会让上一行只剩几个字，
/// 不如退回按字符切。
const BREAK_MIN_RATIO: f32 = 0.45;

/// 折行后的文本。放得下就原样返回；放不下则贪心断行，断点优先落在标点上。
fn wrap_text(text: &str, size: f32, bold: bool, column: f32) -> String {
    // 有词边界的语言交给文本系统断行，手工按字符切会拆开单词。
    if text_width(text, size, bold) <= column || !text.chars().any(|c| c >= '\u{2E80}') {
        return text.to_owned();
    }
    let chars: Vec<char> = text.chars().collect();
    let mut rows: Vec<String> = Vec::new();
    let mut start = 0;
    let mut width = 0.0;
    // 最近一个可用断点，以及断点之前的行宽。
    let mut preferred: Option<(usize, f32)> = None;

    for (index, ch) in chars.iter().enumerate() {
        width += char_width(*ch, size, bold);
        if width > column && index > start {
            let cut = match preferred {
                Some((cut, cut_width))
                    if cut > start
                        && cut_width <= column
                        && cut_width >= column * BREAK_MIN_RATIO =>
                {
                    cut
                }
                _ => index,
            };
            rows.push(
                chars[start..cut]
                    .iter()
                    .collect::<String>()
                    .trim_end()
                    .to_owned(),
            );
            start = cut;
            width = chars[start..=index]
                .iter()
                .map(|c| char_width(*c, size, bold))
                .sum();
            preferred = None;
        }
        if breaks_after(*ch) {
            preferred = Some((index + 1, width));
        }
    }
    rows.push(
        chars[start..]
            .iter()
            .collect::<String>()
            .trim_end()
            .to_owned(),
    );
    rows.join("\n")
}

/// 正文可用的文字宽度。gpui 渲染期拿不到元素实际宽度，只能从布局状态推：
/// 卡片宽 = 窗口宽 − 两侧 GUTTER − 侧栏宽 − 右面板开着时的缝与面板宽，再扣掉内边距。
fn text_budget(window: f32, sidebar: f32, panel_open: bool) -> f32 {
    let panel = if panel_open {
        theme::GUTTER + theme::RIGHT_PANEL_W
    } else {
        0.0
    };
    (window - 2.0 * theme::GUTTER - sidebar - panel - 2.0 * theme::GAP_XL - 2.0 * theme::GAP_LG)
        .max(0.0)
}

/// 整列的基准字号。只看正文预算，不看每句长短，因签名中不含句子。
fn active_size(budget: f32) -> f32 {
    *BASE_TIERS
        .iter()
        .find(|size| LONG_LINE_EM * **size <= budget)
        .unwrap_or(&BASE_FLOOR)
}

/// 邻行的字号。基准退一档比例，夹在可读下限上。
fn idle_size(budget: f32) -> f32 {
    (active_size(budget) * IDLE_RATIO).round().max(IDLE_FLOOR)
}

fn visual_rows(text: &str, size: f32, budget: f32, bold: bool) -> usize {
    (text_width(text, size, bold) / budget.max(1.0))
        .ceil()
        .max(1.0) as usize
}

fn translation_size(original: f32) -> f32 {
    original - TRANSLATION_STEP
}

/// 药丸内部的文字列宽。药丸左右各有 `GAP_LG` 内边距，正文不能顶到行边。
fn text_column(budget: f32) -> f32 {
    (budget - 2.0 * theme::GAP_LG).max(1.0)
}

/// 一个文字盒的高度，同时用作 `line_height` 和 `h`，所以布局不依赖字体度量：
/// gpui 把墨迹在 `line_height` 里上下均分，盒高按字号等比才能让各行墨迹中心等距。
fn line_box(size: f32) -> f32 {
    (size * LINE_BOX_RATIO).round()
}

/// 译文占用的行数。每行分别绘制各自的译文，故行高不统一。
fn translation_rows(line: &LyricLine) -> Vec<&str> {
    line.translation
        .as_deref()
        .map(|text| text.lines().filter(|row| !row.trim().is_empty()).collect())
        .unwrap_or_default()
}

/// 药丸内容区高度。原文加其每行译文，各自按 `size` 计算所占视觉行数。
/// 译文永远按常规字重画，粗体只影响原文行。
fn pill_height(line: &LyricLine, size: f32, budget: f32, bold: bool) -> f32 {
    let trans = translation_size(size);
    let column = text_column(budget);
    let original: usize = wrap_text(&line.text, size, bold, column)
        .lines()
        .map(|row| visual_rows(row, size, column, bold))
        .sum();
    original as f32 * line_box(size)
        + translation_rows(line)
            .iter()
            .map(|row| visual_rows(row, trans, column, false) as f32 * line_box(trans))
            .sum::<f32>()
}

/// 一行的槽位高度。按「这一行是当前行」的字号与粗体算，槽位才永远装得下药丸；
/// 且不随谁是当前行变化，否则弹簧每步位移不等距，滚动会一长一短地跳。
fn slot_height(line: &LyricLine, budget: f32) -> f32 {
    pill_height(line, active_size(budget), budget, true) + 2.0 * PILL_PAD_Y + ROW_GAP
}

/// 当前行的中心到正文列顶部的距离，列由「容器正中」的锚点向上偏移该距离。
/// 不用外层 flex 居中。列比容器高得多，溢出的 flex 子项怎么居中由布局引擎的 overflow 对齐决定。
fn active_center_from_top(heights: &[f32], active: usize) -> f32 {
    let Some(last) = heights.len().checked_sub(1) else {
        return 0.0;
    };
    let active = active.min(last);
    heights[..active].iter().sum::<f32>() + heights[active] / 2.0
}

/// 采样率写成千赫。44100 不能整除 1000，写成 44.1 kHz。
fn sample_rate_label(rate: u32) -> String {
    if rate.is_multiple_of(1000) {
        format!("{} kHz", rate / 1000)
    } else {
        format!("{:.1} kHz", rate as f32 / 1000.0)
    }
}

impl Render for LyricsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (track, duration) = {
            let playback = self.playback.read(cx);
            (playback.current().cloned(), playback.duration())
        };
        let budget = {
            let shell = self.shell.read(cx);
            text_budget(
                shell.width(),
                shell.sidebar().width(),
                shell.right_panel().is_some(),
            )
        };
        // 列宽与滚动偏移只在这里计算一次，当前句读的是同一份数。
        // 逐字节拍里本视图不重绘，只有当前句那一个视图在动。
        self.budget = budget;
        let anchor = self.active.unwrap_or(0);
        let offset = active_center_from_top(&self.heights, anchor);
        self.scroll.animate_to(offset);
        self.shown_offset = self.scroll.value();

        // 槽位高度表按歌词与正文宽度缓存，换曲或窗口宽度变化时才重算一次。
        // 逐字节拍每 16 ms 重绘一帧，这段处于每帧必经路径，必须先查表而不是重算。
        let ready = match &self.state {
            Loaded::Ready(lyrics) => Some(Rc::clone(lyrics)),
            _ => None,
        };
        if let Some(lyrics) = &ready {
            self.ensure_heights(lyrics, budget);
        }

        let body = match (&self.state, track.as_ref()) {
            (Loaded::Ready(lyrics), Some(_)) => self.lyrics_body(lyrics),
            (Loaded::Loading, _) => notice(
                "Loading",
                "正在读取歌词",
                Some("读完会在这一屏显示，不用切歌。".to_owned()),
                true,
            )
            .into_any_element(),
            (_, None) | (Loaded::Idle, _) => notice(
                "Idle",
                "没有在播放的歌",
                Some("先播放一首，歌词会跟着进度往上滚。".to_owned()),
                false,
            )
            .into_any_element(),
            (Loaded::Missing, Some(_)) => {
                notice("No Lyrics", "这首歌没有歌词", None, false).into_any_element()
            }
        };

        let head = header(
            track.as_ref(),
            duration,
            budget >= 640.0,
            self.shell.clone(),
            window,
            cx,
        );
        // 遮罩淡入时页头再往下落 8px，正文晚一档。减少动态效果时两者都不位移。
        let reduce = cx.reduce_motion();
        let head = if reduce {
            head.into_any_element()
        } else {
            drop_in("lyrics-head", 0, 8.0, head).into_any_element()
        };
        // 上下留出与进度线的空隙，否则第一句会被贴着线切掉半个字。
        let body = div()
            .flex_1()
            .min_h_0()
            .flex()
            .py(px(theme::GAP_XL))
            .child(body);
        let body = if reduce {
            body.into_any_element()
        } else {
            drop_in("lyrics-body", MOTION_BODY_DELAY_MS, 12.0, body).into_any_element()
        };

        // 遮罩原点交给 `ActiveRow` 换算坐标。它与本视图同处一个遮罩框，坐标差即原点之差。
        let origin_cell = Rc::clone(&self.overlay_origin);
        let capture_origin = move |bounds: Bounds<Pixels>, _window: &mut Window, _cx: &mut App| {
            origin_cell.set(Some(bounds.origin));
        };

        div()
            // 遮罩须自行拦截鼠标事件。命中测试默认继续向下传递，主区里的卡片仍被判定为悬停，
            // 它的浮层提示就会画在这一屏之上。
            .id("lyrics-overlay")
            .occlude()
            .absolute()
            .top_0()
            .left_0()
            .right_0()
            .bottom_0()
            .flex()
            .flex_col()
            .overflow_hidden()
            .bg(theme::bg())
            .child(
                canvas(capture_origin, |_bounds, _state, _window, _cx| {})
                    .absolute()
                    .size_full(),
            )
            .child(crate::common::ornament::page_backdrop())
            // 遮罩盖掉了主区卡片顶上的三色条，这一屏需另起一条。
            .child(crate::common::ornament::tri_strip())
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .flex_col()
                    .px(px(theme::GAP_XL))
                    .pt(px(theme::GAP_XL))
                    .pb(px(theme::GAP_LG))
                    .child(head)
                    .child(body)
                    .into_any_element(),
            )
            // 遮罩淡入 200ms；退出时 shell 直接移除。
            .with_animation(
                "lyrics-overlay-in",
                Animation::new(Duration::from_millis(200)).with_easing(theme::ease_std()),
                Styled::opacity,
            )
    }
}

impl LyricsView {
    /// 歌词正文。每行一个固定槽位，见 [`slot_height`]；由缓动偏移把当前句送到容器垂直中央。
    /// 当前句由兄弟视图 [`ActiveRow`] 叠在同一个位置，这里给它留一块等高的空位。
    fn lyrics_body(&self, lyrics: &Rc<Lyrics>) -> AnyElement {
        let anchor = self.active.unwrap_or(0);
        let (first, last) = visible_window(self.active, lyrics.lines.len());
        let above: f32 = self.heights[..first].iter().sum();
        let below: f32 = self.heights[last + 1..].iter().sum();

        let mut column = div()
            .relative()
            .flex()
            .flex_col()
            .items_center()
            .w_full()
            .min_w_0()
            // 位置由 `Scroll` 按时间计算，不交给动画系统：
            // 后者会在动画期间随刷新率补帧并重绘整个窗口。
            .top(px(-self.shown_offset))
            .child(div().flex_none().h(px(above)));

        for (index, line) in lyrics
            .lines
            .iter()
            .enumerate()
            .skip(first)
            .take(last + 1 - first)
        {
            // 当前句由 `ActiveRow` 画在同一个位置，这里留等高空位，后面的行才不会错位。
            if self.active == Some(index) {
                column = column.child(div().flex_none().w_full().h(px(self.heights[index])));
                continue;
            }
            column = column.child(lyric_row(
                index,
                line,
                false,
                None,
                Duration::ZERO,
                self.heights[index],
                self.budget,
                anchor,
                &self.playback,
            ));
        }
        column = column.child(div().flex_none().h(px(below)));

        // 正文列的矩形交给 `ActiveRow`，它要按同一块区域定位与裁切。
        let body_cell = Rc::clone(&self.body_rect);
        let capture_body = move |bounds: Bounds<Pixels>, _window: &mut Window, _cx: &mut App| {
            body_cell.set(Some(bounds));
        };

        div()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .relative()
            .overflow_hidden()
            .child(
                canvas(capture_body, |_bounds, _state, _window, _cx| {})
                    .absolute()
                    .size_full(),
            )
            // 容器正中的 0 高锚点。列由此向上偏移，绕开「居中溢出盒子」的语义。
            .child(
                div()
                    .absolute()
                    .left_0()
                    .right_0()
                    .top(relative(0.5))
                    .h(px(0.0))
                    .child(column),
            )
            .into_any_element()
    }
}

/// 页面顶部。眉标与关闭按钮、歌名、一行真实字段，最后一条细线收尾。
fn header(
    track: Option<&Track>,
    duration: Duration,
    wide: bool,
    shell: Entity<ShellEntity>,
    window: &mut Window,
    cx: &mut App,
) -> Div {
    let close = crate::common::button::IconButton::new("lyrics-close", Glyph::Cross)
        .label("关闭歌词")
        .box_size(28.)
        .glyph_size(14.)
        .color(theme::text_muted())
        .on_click(move |_, _, cx| shell.update(cx, ShellEntity::toggle_lyrics))
        .hover_progress(window, cx)
        .render();

    let title = track.map_or("未在播放", |track| track.title.as_str());
    let shown_duration = track.and_then(|track| track.duration).unwrap_or(duration);

    div()
        .flex_none()
        .w_full()
        .min_w_0()
        .flex()
        .flex_col()
        .child(
            div()
                .w_full()
                .flex()
                .items_center()
                .justify_between()
                .gap(px(theme::GAP_MD))
                .child(crate::common::section::eyebrow("Lyrics"))
                .child(close),
        )
        .child(
            div()
                .id("lyrics-title")
                .mt(px(theme::GAP_XS))
                .w_full()
                .min_w_0()
                .truncate()
                .text_size(px(theme::FONT_TITLE))
                .font_weight(theme::WEIGHT_TITLE)
                .text_color(theme::text())
                .tooltip(crate::common::tooltip::builder(title.to_owned()))
                .child(title.to_owned()),
        )
        .child(
            div()
                .mt(px(theme::GAP_XS))
                .child(meta_row(track, shown_duration, wide)),
        )
        .child(
            div()
                .mt(px(theme::GAP_LG))
                .child(crate::common::section::rule()),
        )
}

/// 歌名下方的一行字段。艺术家与专辑可截断，数字与规格不折行。
fn meta_row(track: Option<&Track>, duration: Duration, wide: bool) -> Div {
    // 没有曲目时不写「未知艺术家」，那会把「没在播放」说成一首歌。
    let Some(track) = track else {
        return div();
    };
    let mut row = div()
        .w_full()
        .min_w_0()
        .flex()
        .items_center()
        .gap(px(theme::GAP_SM))
        .overflow_hidden();
    let mut first = true;

    let artist = track.artist.clone();
    row = push_field(
        row,
        &mut first,
        artist.unwrap_or_else(|| UNKNOWN_ARTIST.to_owned()),
        true,
    );
    if wide && let Some(album) = track.album.clone() {
        row = push_field(row, &mut first, album, true);
    }
    if duration > Duration::ZERO {
        row = push_field(row, &mut first, format_duration(duration), false);
    }
    if let Some(format) = track.format() {
        row = push_field(row, &mut first, format.to_uppercase(), false);
    }
    let audio = track.audio;
    if let Some(rate) = audio.sample_rate {
        row = push_field(row, &mut first, sample_rate_label(rate), false);
    }
    match (audio.bit_depth, audio.bitrate) {
        (Some(bits), _) if wide => {
            row = push_field(row, &mut first, format!("{bits} bit"), false);
        }
        (None, Some(kbps)) if wide => {
            row = push_field(row, &mut first, format!("{kbps} kbps"), false);
        }
        _ => {}
    }

    row
}

fn push_field(row: Div, first: &mut bool, text: String, truncate: bool) -> Div {
    let row = if *first {
        row
    } else {
        row.child(field_separator())
    };
    *first = false;
    row.child(field_item(text, truncate))
}

fn field_item(text: String, truncate: bool) -> Div {
    let item = div()
        .font(theme::field_font())
        .text_size(px(theme::FONT_FIELD_SIZE))
        .text_color(theme::text_faint());
    let item = if truncate {
        item.min_w_0().truncate()
    } else {
        item.flex_none()
    };
    item.child(text)
}

fn field_separator() -> Div {
    div()
        .flex_none()
        .font(theme::field_font())
        .text_size(px(theme::FONT_FIELD_SIZE))
        .text_color(theme::border())
        .child("│")
}

/// 空状态的眉标。与 [`crate::common::section::eyebrow`] 同款，只是字级大一档。
fn notice_eyebrow(text: &'static str) -> Div {
    div()
        .flex_none()
        .font(theme::label_font())
        .text_size(px(NOTICE_EYEBROW))
        .font_weight(theme::WEIGHT_BUTTON)
        .text_color(theme::text_faint())
        .child(SharedString::from(format!("// {}", text.to_uppercase())))
}

/// 居中一块提示，用于加载中、没有歌词与没在播放三种情形。
fn notice(
    eyebrow: &'static str,
    headline: &'static str,
    detail: Option<String>,
    loading: bool,
) -> Div {
    let mark = if loading {
        div()
            .flex_none()
            .w(px(280.))
            .child(crate::common::anim::unknown_progress())
            .into_any_element()
    } else {
        // 斜线纹只在这里与禁用态出现，空状态不必再画框；尺寸跟着眉标放大，否则只是一条灰线。
        div()
            .relative()
            .flex_none()
            .w(px(240.))
            .h(px(12.))
            .overflow_hidden()
            .child(crate::common::ornament::hatch_fill(
                theme::text().opacity(0.22),
            ))
            .into_any_element()
    };

    div()
        .flex_1()
        .min_w_0()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap(px(theme::GAP_MD))
        .child(mark)
        .child(notice_eyebrow(eyebrow))
        .child(
            div()
                .flex_none()
                .text_size(px(theme::FONT_DISPLAY))
                .font_weight(theme::WEIGHT_TITLE)
                .text_color(theme::text())
                .child(headline),
        )
        .when_some(detail, |block, detail| {
            block.child(
                div()
                    .flex_none()
                    .max_w(px(460.))
                    .text_center()
                    .line_height(px(theme::FONT_BODY * 1.6))
                    .text_size(px(theme::FONT_BODY))
                    .text_color(theme::text_muted())
                    .child(detail),
            )
        })
}

/// 可见行窗口，当前句上下各 `VISIBLE_ROWS` 行。只渲染这一窗，全量渲染一帧约十几毫秒。
const VISIBLE_ROWS: usize = 8;

/// 当前句上下各 `VISIBLE_ROWS` 行的闭区间。没有当前句时从第一行起算。
fn visible_window(active: Option<usize>, total: usize) -> (usize, usize) {
    let Some(last_line) = total.checked_sub(1) else {
        return (0, 0);
    };
    let anchor = active.unwrap_or(0).min(last_line);
    (
        anchor.saturating_sub(VISIBLE_ROWS),
        (anchor + VISIBLE_ROWS).min(last_line),
    )
}

/// 第 `index` 行顶边到容器正中锚线的距离，负值在上方。滚动偏移已计入。
/// 列与当前句各排一次，几何只由高度表决定，两边算出的位置必须逐像素一致。
fn row_top(heights: &[f32], offset: f32, index: usize) -> f32 {
    heights[..index].iter().sum::<f32>() - offset
}

/// 一行歌词。`is_active` 为真时按当前句的字号与逐字进度上色，否则整行是一个文本元素。
/// 列与当前句共用它，差别只在传进来的 `is_active` 与 `word`。
#[allow(clippy::too_many_arguments)]
fn lyric_row(
    index: usize,
    line: &LyricLine,
    is_active: bool,
    word: Option<usize>,
    position: Duration,
    height: f32,
    budget: f32,
    anchor: usize,
    playback: &Entity<PlaybackEntity>,
) -> Stateful<Div> {
    let distance = if is_active {
        0
    } else {
        index.abs_diff(anchor).max(1)
    };
    let opacity = match distance {
        0 => 1.0,
        1..=2 => OPACITY_NEAR,
        _ => OPACITY_FAR,
    };
    let size = if is_active {
        active_size(budget)
    } else {
        idle_size(budget)
    };
    let original_box = line_box(size);

    // 宽度约束必须落在容器上。整句包进 `flex_none` 子项时按最大内容宽度测量，
    // 只会横向撑出正文列；flex 只在子项之间折行，不在子项内部折行。
    let text_w = text_column(budget);
    let original = if is_active && !line.words.is_empty() {
        // 逐字行每格一个子项，只在格与格之间折行，各格按演唱进度调亮度。
        div()
            .flex_none()
            .flex()
            .flex_wrap()
            .items_center()
            .justify_center()
            .max_w(px(text_w))
            .min_w_0()
            .line_height(px(original_box))
            .text_size(px(size))
            .font_weight(theme::WEIGHT_TITLE)
            .text_color(theme::text())
            .children(word_spans(line, word, position))
    } else {
        // 整行是一个文本元素，交给文本系统断行；中文没有空格，只有文本系统能判断换行点。
        div()
            .flex_none()
            .max_w(px(text_w))
            .min_w_0()
            .text_center()
            .line_height(px(original_box))
            .text_size(px(size))
            .font_weight(if is_active {
                theme::WEIGHT_TITLE
            } else {
                theme::WEIGHT_TRACK
            })
            .text_color(theme::text())
            .opacity(opacity)
            .child(wrap_text(&line.text, size, is_active, text_w))
    };

    // 药丸按内容收缩；槽位高度只用于滚动偏移。
    let group = SharedString::from(format!("lyric-row-{index}"));
    let mut pill = div()
        .flex_none()
        .flex()
        .flex_col()
        .items_center()
        .px(px(theme::GAP_LG))
        .py(px(PILL_PAD_Y))
        .rounded(px(theme::RADIUS_LG))
        .group_hover(group.clone(), |style| style.bg(theme::hover()))
        .child(original);

    for part in translation_rows(line) {
        let trans_size = translation_size(size);
        let trans_box = line_box(trans_size);
        pill = pill.child(
            div()
                .flex_none()
                .max_w(px(text_w))
                .min_w_0()
                .text_center()
                .line_height(px(trans_box))
                .opacity(opacity)
                .text_size(px(trans_size))
                .text_color(theme::text_muted())
                .child(part.to_owned()),
        );
    }

    // 行首竖条。当前句常亮，其余行悬停时出现，表明这一句可以点击。
    // 颜色用 trace 而非强调色；浅色主题中黄色与纸色对比度仅 1.1:1，细条无法辨认。
    let marker = div()
        .absolute()
        .left_0()
        .top_0()
        .bottom_0()
        .w(px(if is_active { 3. } else { 2. }))
        .bg(theme::trace())
        .when(!is_active, |bar| {
            bar.opacity(0.0)
                .group_hover(group.clone(), |style| style.opacity(0.45))
        });

    div()
        .id(SharedString::from(format!("lyric-{index}")))
        .group(group)
        .flex_none()
        .w_full()
        .h(px(height))
        .relative()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .text_center()
        .cursor_pointer()
        .on_click(seek_handler(playback, line.start))
        .child(
            div()
                .relative()
                .flex()
                .flex_col()
                .items_center()
                .max_w(px(text_w))
                .min_w_0()
                .child(pill)
                // 竖条排在药丸之后，才不会被悬停底色盖住。
                .child(marker),
        )
}

/// 当前句。逐字节拍唯一重绘的视图，一帧的工作量是一行字加一格逐像素亮字。
/// 它位于歌词屏的**兄弟**位置，见根视图的遮罩层。gpui 标脏会向上传播到祖先，而重绘
/// 一个视图时整棵子树参与重排，故它若留在 `LyricsView` 内，每拍都会重排整列 17 行。
pub struct ActiveRow {
    view: WeakEntity<LyricsView>,
    /// 音频快照的值与读取时刻。快照每 60 ms 才写一次，直接据此计算进度会使填充逐格跳变。
    /// 平滑必须留在这个视图内，因为歌词屏不按帧重绘，其位置只停在上次换行那一刻。
    pos_anchor: Option<(Duration, Instant)>,
}

impl ActiveRow {
    fn new(view: WeakEntity<LyricsView>) -> Self {
        Self {
            view,
            pos_anchor: None,
        }
    }

    /// 把 60 ms 一跳的音频快照补成连续时间。两次快照之间按单调时钟外推，
    /// 新快照一到就以它为锚重新对齐，误差不超过一次快照间隔。
    fn smooth_position(&mut self, raw: Duration, playing: bool, duration: Duration) -> Duration {
        let now = Instant::now();
        let anchored_at = match self.pos_anchor {
            Some((value, at)) if playing && value == raw => at,
            _ => {
                self.pos_anchor = Some((raw, now));
                now
            }
        };
        let elapsed = now.saturating_duration_since(anchored_at);
        (raw + elapsed).min(duration)
    }
}

impl Render for ActiveRow {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(view) = self.view.upgrade() else {
            return div().into_any_element();
        };
        let view = view.read(cx);
        let (Some(lyrics), Some(index)) = (view.lyrics(), view.active) else {
            return div().into_any_element();
        };
        let (Some(line), Some(&height)) = (lyrics.lines.get(index), view.heights.get(index)) else {
            return div().into_any_element();
        };
        // 正文列的矩形由歌词屏在 prepaint 时写入。首帧尚未写入，不绘制。
        let (Some(origin), Some(body)) = (view.overlay_origin.get(), view.body_rect.get()) else {
            return div().into_any_element();
        };
        // 进度必须在这里按帧计算。`LyricsView` 只在换行换字时重绘，其所持位置已经过期。
        let (raw, playing, duration) = {
            let playback = view.playback.read(cx);
            (
                playback.position(),
                playback.is_playing(),
                playback.duration(),
            )
        };
        let position = self.smooth_position(raw, playing, duration);
        let width = f32::from(body.size.width);
        let body_h = f32::from(body.size.height);
        if width <= 0.0 || body_h <= 0.0 {
            return div().into_any_element();
        }

        // 与静态行同一条锚线，即容器正中减去滚动偏移，再加上前面各行的高度。
        // 两边的几何都只由高度表决定，逐像素一致。
        let top_in_body = body_h / 2.0 + row_top(&view.heights, view.shown_offset, index);

        div()
            .absolute()
            .left(px(f32::from(body.origin.x - origin.x)))
            .top(px(f32::from(body.origin.y - origin.y)))
            .w(px(width))
            .h(px(body_h))
            .overflow_hidden()
            .child(
                div()
                    .absolute()
                    .left_0()
                    .right_0()
                    .top(px(top_in_body))
                    .flex()
                    .flex_col()
                    .items_center()
                    .child(lyric_row(
                        index,
                        line,
                        true,
                        // 当前格也按平滑位置计算，因为 `view.word` 以 10 Hz 更新，据它上色
                        // 会使换字滞后，格子交界处表现为卡顿。
                        line.word_index(position),
                        position,
                        height,
                        view.budget,
                        index,
                        &view.playback,
                    )),
            )
            .into_any_element()
    }
}

/// 正在唱的那一格的亮字。已唱段实心，交界处按像素逐条绘制，
/// 各带不透明度连续下降，形成渐变而非阶梯。
fn karaoke(text: SharedString, progress: f32, color: Hsla) -> AnyElement {
    canvas(
        move |bounds, window, _cx| {
            let style = window.text_style();
            let font = style.font();
            let size = style.font_size.to_pixels(window.rem_size());
            let width = f32::from(bounds.size.width);
            // 一像素一条，带宽最多也就几十条，逐条 shape 的成本远低于一次全量重排。
            let band = (width * WIPE_FEATHER).max(1.0);
            let steps = band.ceil() as usize;
            let text_system = window.text_system();
            let lines = (0..=steps)
                .map(|k| {
                    let alpha = 1.0 - k as f32 / steps as f32;
                    text_system.shape_line(
                        text.clone(),
                        size,
                        &[TextRun {
                            len: text.len(),
                            font: font.clone(),
                            color: color.opacity(alpha),
                            background_color: None,
                            underline: None,
                            strikethrough: None,
                        }],
                        None,
                    )
                })
                .collect::<Vec<_>>();
            (lines, band)
        },
        move |bounds, state, window, cx| {
            let (lines, band) = state;
            let origin = bounds.origin;
            let height = bounds.size.height;
            let width = f32::from(bounds.size.width);
            let cut = width * progress.clamp(0.0, 1.0);
            let solid = (cut - band).max(0.0);

            if solid > 0.0 {
                let clip = Bounds {
                    origin,
                    size: size(px(solid), height),
                };
                // `paint_layer` 只管图层顺序，不裁内容；裁切要显式设内容遮罩，
                // 少了这一步每条带都会画满整格，整字一起变亮。
                window.with_content_mask(Some(ContentMask { bounds: clip }), |window| {
                    lines[0]
                        .paint(origin, height, TextAlign::Left, None, window, cx)
                        .ok();
                });
            }

            let steps = lines.len().saturating_sub(1);
            if steps == 0 {
                return;
            }
            let step = band / steps as f32;
            for k in 0..steps {
                let from = solid + k as f32 * step;
                let to = (from + step).min(cut);
                if to <= from {
                    break;
                }
                let clip = Bounds {
                    origin: point(px(f32::from(origin.x) + from), origin.y),
                    size: size(px(to - from), height),
                };
                window.with_content_mask(Some(ContentMask { bounds: clip }), |window| {
                    lines[k + 1]
                        .paint(origin, height, TextAlign::Left, None, window, cx)
                        .ok();
                });
            }
        },
    )
    .absolute()
    .top_0()
    .left_0()
    .h_full()
    .w_full()
    .into_any_element()
}

/// 当前行的逐字正文。已唱完的格整格点亮，未唱的格压暗；
/// 正在唱的那格从左往右填亮，填到的地方与没填到的交界是一段渐变，不是一条硬线。
fn word_spans(line: &LyricLine, word: Option<usize>, position: Duration) -> Vec<AnyElement> {
    let sung = word.unwrap_or(0);
    line.words
        .iter()
        .enumerate()
        .map(|(index, part)| {
            if index < sung {
                return div()
                    .flex_none()
                    .opacity(1.0)
                    .child(part.text.clone())
                    .into_any_element();
            }
            if index > sung {
                return div()
                    .flex_none()
                    .opacity(OPACITY_UNSUNG)
                    .child(part.text.clone())
                    .into_any_element();
            }
            let width = part.end.saturating_sub(part.start);
            // 已演唱的比例。暂停时停在当前位置，播放时随平滑位置推进。
            // 时间戳倒挂时位置差按 0 计；字时长为 0 时视为已唱完，避免除零。
            let played = if width.is_zero() {
                1.0
            } else {
                (position.saturating_sub(part.start).as_secs_f32() / width.as_secs_f32()).min(1.0)
            };
            // 压暗的那份留在正常流里，决定格宽高；亮的那份由 canvas 叠加。
            div()
                .flex_none()
                .relative()
                .child(
                    div()
                        .flex_none()
                        .opacity(OPACITY_UNSUNG)
                        .child(part.text.clone()),
                )
                .child(karaoke(
                    part.text.clone().into(),
                    played,
                    theme::text().into(),
                ))
                .into_any_element()
        })
        .collect()
}

/// 从上方落到位。遮罩自身在淡入，这里只补位移，两段不会叠成一次更慢的淡入。
fn drop_in<E>(id: &'static str, delay_ms: u64, offset: f32, el: E) -> AnimationElement<E>
where
    E: Styled + IntoElement + 'static,
{
    let mut animations = Vec::with_capacity(2);
    if delay_ms > 0 {
        animations.push(Animation::new(Duration::from_millis(delay_ms)));
    }
    animations
        .push(Animation::new(Duration::from_millis(MOTION_IN_MS)).with_easing(theme::ease_snap()));

    el.with_animations(SharedString::from(id), animations, move |el, ix, t| {
        // 延迟那一段停在起点；最后一段才真正位移。
        let shown = if ix == 0 && delay_ms > 0 { 0.0 } else { t };
        el.relative().top(px(-offset * (1.0 - shown)))
    })
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::{
        LINE_BOX_RATIO, PILL_PAD_Y, ROW_GAP, TRANSLATION_STEP, WIPE_FEATHER,
        active_center_from_top, active_size, idle_size, line_box, pill_height, sample_rate_label,
        slot_height, text_budget, text_column, text_width, translation_rows, translation_size,
        visual_rows, wrap_text,
    };
    use crate::theme;
    use capstan_core::{LyricLine, LyricWord};

    const H: f32 = 60.0;
    const T: f32 = 24.0;

    fn line(text: &str, translation: Option<&str>, words: usize) -> LyricLine {
        LyricLine {
            start: Duration::ZERO,
            end: Duration::from_secs(4),
            text: text.to_owned(),
            translation: translation.map(str::to_owned),
            words: (0..words)
                .map(|index| LyricWord {
                    start: Duration::from_millis(index as u64 * 200),
                    end: Duration::from_millis(index as u64 * 200 + 200),
                    text: "字".to_owned(),
                })
                .collect(),
        }
    }

    /// 宽到够得着最大档的预算。
    const WIDE: f32 = 1500.0;
    /// 最小窗口时的预算，此时宽 560、侧栏收成图标条。
    const NARROW: f32 = 150.0;

    /// 608 = 1264px 窗口开着右面板，355 = 1011px。
    #[test]
    fn 字号只看预算不看句子() {
        for budget in [WIDE, 900.0, 608.0, 355.0, NARROW, 0.0] {
            let (current, idle) = (active_size(budget), idle_size(budget));
            assert!(
                (22.0..=theme::FONT_HERO).contains(&current),
                "预算 {budget} 时整列字号 {current} 越界"
            );
            assert!(
                current - idle >= 4.0,
                "预算 {budget}：当前句 {current}、邻句 {idle}，差 {} 看不出来",
                current - idle
            );
        }
        assert_eq!(active_size(WIDE), theme::FONT_HERO);
        assert!(
            active_size(NARROW) < active_size(WIDE),
            "窄窗口必须降一档，否则整屏都在折行"
        );
    }

    /// 放不下的长句折行，所以槽位要按「几个视觉行」留高。只留一行高就会把第二行裁掉。
    #[test]
    fn 长句按行数占位() {
        let budget = 608.0;
        let size = active_size(budget);
        assert_eq!(
            visual_rows(&"字".repeat(30), size, budget, true),
            2,
            "30 em 在 {budget}px 里放不下两行都不止"
        );
        let short = line("短句", None, 0);
        let long = line(&"字".repeat(30), None, 0);
        assert!(
            slot_height(&long, budget) >= 2.0 * line_box(size) + 2.0 * PILL_PAD_Y + ROW_GAP,
            "两行的句子只留了一行高：{}",
            slot_height(&long, budget)
        );
        assert!(slot_height(&long, budget) > slot_height(&short, budget));
    }

    /// 放不下的句子要折行，且优先断在标点后；按字符切会把一个词拆到两行。
    #[test]
    fn 长句折行时优先断在标点处() {
        let size = 26.0;
        let column = 600.0;
        let text = "目前にして押し寄せてくる最後の痛みを、ああ越えたら";
        let wrapped = wrap_text(text, size, true, column);
        let rows: Vec<&str> = wrapped.lines().collect();
        assert_eq!(rows.len(), 2, "断点不对：{wrapped}");
        assert!(rows[0].ends_with('、'), "第一行没断在标点后：{wrapped}");
        for row in &rows {
            assert!(
                text_width(row, size, true) <= column,
                "折行后仍然超宽：{row}"
            );
        }
        assert_eq!(
            wrap_text("短句", size, true, column),
            "短句",
            "放得下就不该动它"
        );
    }

    /// 英文有词边界，手工按字符切会把单词拆开，交给文本系统处理。
    #[test]
    fn 英文不手工折行() {
        let text = "a very long english line that has no business being split mid-word";
        assert_eq!(wrap_text(text, 26.0, true, 300.0), text);
    }

    /// 没有标点的长句照样要折行，只是断点由字符决定。
    #[test]
    fn 没有标点也能折行() {
        let column = 600.0;
        let wrapped = wrap_text(&"字".repeat(30), 26.0, true, column);
        assert_eq!(wrapped.lines().count(), 2, "{wrapped}");
        assert!(
            wrapped
                .lines()
                .all(|row| text_width(row, 26.0, true) <= column)
        );
    }

    /// 药丸内边距不能算进正文宽度，否则文字会贴着行边。
    #[test]
    fn 正文列宽扣掉药丸内边距() {
        assert_eq!(text_column(500.0), 500.0 - 2.0 * theme::GAP_LG);
        assert_eq!(text_column(10.0), 1.0, "极窄窗口不能扣成 0 或负数");
    }

    /// 译文常远长于原文，行数需按其自身字号计算，不可仅按原文预留高度。
    #[test]
    fn 译文行按自己的字号数行数() {
        let budget = 608.0;
        let size = active_size(budget);
        let trans = translation_size(size);
        let plain = line("答えは もう", None, 0);
        let bilingual = line("答えは もう", Some(&"我的回答是的".repeat(6)), 0);
        assert!(
            slot_height(&bilingual, budget) >= slot_height(&plain, budget) + line_box(trans),
            "长译文没多占行数：{} vs {}",
            slot_height(&bilingual, budget),
            slot_height(&plain, budget)
        );
    }

    #[test]
    fn 宽度估算按全角半角分档() {
        assert!(text_width("ああああ", 20.0, false) > text_width("aaaa", 20.0, false));
        assert!(text_width("    ", 20.0, false) < text_width("aaaa", 20.0, false));
        assert!(text_width("同一句", 20.0, true) > text_width("同一句", 20.0, false));
        assert_eq!(text_width("", 32.0, true), 0.0, "空行不该占宽度");
    }

    /// 预算的每一项都对应 root.rs 里的一处真实留白，少扣一处就会把句尾裁掉。
    #[test]
    fn 预算从窗口宽一路扣到正文() {
        let wide = text_budget(1920.0, theme::SIDEBAR_W, false);
        assert!(
            wide > 1500.0,
            "最大化窗口该给正文留下 1500px 以上，实测 {wide}"
        );
        assert!(
            text_budget(1920.0, theme::SIDEBAR_W, true) + theme::GUTTER + theme::RIGHT_PANEL_W
                == wide,
            "右面板展开时要正好扣掉一格面板宽"
        );
        assert!(
            text_budget(560.0, theme::SIDEBAR_ICON_W, true) > 0.0,
            "最小窗口不能扣成负数"
        );
        assert_eq!(
            text_budget(100.0, theme::SIDEBAR_W, true),
            0.0,
            "扣穿了要夹到 0"
        );
    }

    #[test]
    fn 译文只比原文小四像素() {
        for budget in [WIDE, 900.0, 608.0, 355.0, NARROW] {
            let original = active_size(budget);
            assert_eq!(
                translation_size(original),
                original - TRANSLATION_STEP,
                "预算 {budget} 时译文差得不对"
            );
            assert!(
                translation_size(idle_size(budget)) >= 12.0,
                "邻句的译文小到读不清：{}",
                translation_size(idle_size(budget))
            );
        }
    }

    #[test]
    fn 柔化带占小半个字身() {
        let band = std::hint::black_box(WIPE_FEATHER);
        assert!(
            (0.2..=0.6).contains(&band),
            "带太窄和硬切没区别，太宽整个字都泡在渐变里：{band}"
        );
    }

    #[test]
    fn 均匀行高时中心距离就是行号加半格() {
        for count in 1..10 {
            let heights = vec![H; count];
            for active in 0..count {
                assert_eq!(
                    active_center_from_top(&heights, active),
                    (active as f32 + 0.5) * H,
                    "{count} 行第 {active} 行的锚距不对"
                );
            }
        }
        assert_eq!(active_center_from_top(&[], 3), 0.0, "空列不能 panic");
        // active 越界时需夹住，如换曲瞬间的旧序号，不得越界读取。
        let heights = vec![H; 3];
        assert_eq!(active_center_from_top(&heights, 99), 2.5 * H);
    }

    #[test]
    fn 换行时锚距按上一行的实际高度推进() {
        let uniform = vec![H; 8];
        for active in 0..7 {
            assert_eq!(
                active_center_from_top(&uniform, active + 1)
                    - active_center_from_top(&uniform, active),
                H,
                "行高统一时换一句恰好走一行高，弹簧每步位移才等距"
            );
        }
        // 上一行更高时，换到下一行要多走它的整高，而不是一个固定行高。
        let mut mixed = vec![H; 8];
        mixed[3] = H + T;
        assert_eq!(
            active_center_from_top(&mixed, 4) - active_center_from_top(&mixed, 3),
            H + T / 2.0
        );
    }

    #[test]
    fn 当前行下方的行不影响它是否居中() {
        // 锚点法只看「列顶到当前行中心」的距离，所以下面的行长成什么样都无所谓。
        let base = vec![H; 7];
        let mut below = base.clone();
        below[5] += 3.0 * T;
        assert_eq!(
            active_center_from_top(&base, 2),
            active_center_from_top(&below, 2)
        );

        // 上方的行变高，当前行就被顶下去它的整高。
        let mut above = base.clone();
        above[1] += T;
        assert_eq!(
            active_center_from_top(&above, 2) - active_center_from_top(&base, 2),
            T
        );

        // 当前行自身增高只计入一半。此处需要中心对齐，而非顶对齐。
        let mut own = base.clone();
        own[2] += T;
        assert_eq!(
            active_center_from_top(&own, 2) - active_center_from_top(&base, 2),
            T / 2.0
        );
    }

    #[test]
    fn 槽位包含自己的译文行且永远装得下药丸() {
        let plain = line("原文原文", None, 0);
        let bilingual = line("原文原文", Some("译文译文"), 0);
        let trans_box = line_box(translation_size(active_size(WIDE)));
        assert!(
            slot_height(&bilingual, WIDE) > slot_height(&plain, WIDE),
            "多一行译文就要多占一行"
        );
        assert_eq!(
            slot_height(&bilingual, WIDE) - slot_height(&plain, WIDE),
            trans_box,
            "译文行按自己的字号加高"
        );

        let three_languages = line("原文", Some("译文一\n译文二"), 0);
        assert_eq!(translation_rows(&three_languages), ["译文一", "译文二"]);
        assert_eq!(
            translation_rows(&line("原文", Some("  \n \n"), 0)),
            [] as [&str; 0]
        );
        assert_eq!(
            slot_height(&three_languages, WIDE) - slot_height(&line("原文", None, 0), WIDE),
            2.0 * trans_box
        );

        // 槽位按「当前行」的字号与行数计算，邻行字号更小、行数更少，永远装得下。
        for chars in [3usize, 14, 15, 24, 25, 60] {
            let text = "字".repeat(chars);
            for translation in [None, Some("译文译文"), Some(&"译".repeat(50) as &str)] {
                let row = line(&text, translation, 0);
                for budget in [WIDE, NARROW, 320.0, 608.0] {
                    for (size, bold) in [(active_size(budget), true), (idle_size(budget), false)] {
                        let pill = pill_height(&row, size, budget, bold) + 2.0 * PILL_PAD_Y;
                        assert!(
                            pill + ROW_GAP <= slot_height(&row, budget),
                            "{chars} 字 / 译文 {translation:?} / 预算 {budget} / 字号 {size} 顶出槽位：{pill} > {}",
                            slot_height(&row, budget) - ROW_GAP
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn 双语行的墨迹中心落在药丸中心() {
        // 墨迹在各自的盒子里居中，联合中心按墨迹高度加权。
        let ink_center = |boxes: &[f32], sizes: &[f32]| -> f32 {
            let mut tops = vec![0.0];
            for box_h in &boxes[..boxes.len() - 1] {
                let next = tops.last().unwrap() + box_h;
                tops.push(next);
            }
            let inks: Vec<f32> = sizes.iter().map(|size| size * 0.9).collect();
            let total: f32 = inks.iter().sum();
            inks.iter()
                .zip(&tops)
                .zip(boxes)
                .map(|((ink, top), box_h)| ink * (top + box_h / 2.0))
                .sum::<f32>()
                / total
        };
        let middle = |boxes: &[f32]| boxes.iter().sum::<f32>() / 2.0;

        let size = active_size(WIDE);
        let sizes = [size, translation_size(size)];
        let boxes = [line_box(sizes[0]), line_box(sizes[1])];
        assert!(
            (ink_center(&boxes, &sizes) - middle(&boxes)).abs() < 0.5,
            "等比盒高的墨迹中心 {} 应等于内容中心 {}",
            ink_center(&boxes, &sizes),
            middle(&boxes)
        );

        let old = [sizes[0] * 1.618, 26.0];
        assert!(
            ink_center(&old, &sizes) > middle(&old) + 3.0,
            "固定译文盒应把墨迹压低（旧 bug）"
        );
    }

    #[test]
    fn 文字盒随字号等比缩放() {
        assert_eq!(line_box(20.0), (20.0 * LINE_BOX_RATIO).round());
        assert!(line_box(28.0) > 26.0, "28px 的译文盒不能矮过旧的固定 26px");
        assert!(line_box(32.0) > line_box(28.0));
    }

    #[test]
    fn 未唱到的字比唱过的淡() {
        let karaoke = line("甲乙", None, 3);
        assert_eq!(karaoke.word_index(Duration::ZERO), Some(0));
        assert_eq!(karaoke.word_index(Duration::from_millis(400)), Some(2));
        // 行尾之后仍停在最后一格，不会翻到下一格。
        assert_eq!(karaoke.word_index(Duration::from_secs(9)), Some(2));
        // 没有逐字信息的行返回 None，界面整行一起点亮。
        assert_eq!(line("甲乙", None, 0).word_index(Duration::ZERO), None);
    }

    #[test]
    fn 采样率写成千赫() {
        assert_eq!(sample_rate_label(48_000), "48 kHz");
        assert_eq!(sample_rate_label(44_100), "44.1 kHz");
        assert_eq!(sample_rate_label(96_000), "96 kHz");
        assert_eq!(sample_rate_label(192_000), "192 kHz");
    }
}
