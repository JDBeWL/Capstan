//! 设计令牌。纸、板、墨三色，单一强调色、细线与切角，以及字体与动效。
//! 浅色是默认，深色是一整套平行取值。不是把浅色反相，而是各自的纸、板、墨、细线。
//! 所有文字色对背景的对比度必须 ≥ 4.5:1，即 WCAG AA 的要求；本文件末尾的测试对两套主题逐项强制。

use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};

use serde::{Deserialize, Serialize};

use crate::platform::*;

/// 界面明暗。存档里写 `"Light"` / `"Dark"`。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Appearance {
    #[default]
    Light,
    Dark,
}

impl Appearance {
    pub fn is_dark(self) -> bool {
        matches!(self, Self::Dark)
    }
}

/// 一套完整的主题取值，字段是原始 sRGB。函数只负责按当前明暗取色。
/// 两套的层级关系一致。板比纸亮，切角露出的始终是更暗的纸。
#[derive(Clone, Copy)]
pub(crate) struct Palette {
    pub(crate) dark: bool,
    pub(crate) paper: u32,
    pub(crate) panel: u32,
    pub(crate) ink: u32,
    pub(crate) ink2: u32,
    pub(crate) faint: u32,
    pub(crate) rule: u32,
    pub(crate) rule_soft: u32,
    pub(crate) chip: u32,
    pub(crate) signal: u32,
    pub(crate) on_signal: u32,
    pub(crate) hover: u32,
    pub(crate) cursor: u32,
    pub(crate) tag_bg: u32,
    pub(crate) danger: u32,
    pub(crate) on_danger: u32,
    pub(crate) on_control: u32,
}

const LIGHT: Palette = Palette {
    dark: false,
    paper: 0xf4f4f2,
    panel: 0xffffff,
    ink: 0x161616,
    ink2: 0x3f3f3f,
    faint: 0x5c5c5c,
    rule: 0xd9d9d6,
    rule_soft: 0xe4e4e0,
    chip: 0xe4e4e0,
    signal: 0xfffa00,
    on_signal: 0x161616,
    hover: 0xf4f4f2,
    cursor: 0xe4e4e0,
    tag_bg: 0xe4e4e0,
    danger: 0xb3_26_1e,
    on_danger: 0xffffff,
    on_control: 0xffffff,
};

/// 深色。纸 #191919、板 #222222、墨 #EDEDED，黄不变。
const DARK: Palette = Palette {
    dark: true,
    paper: 0x191919,
    panel: 0x222222,
    ink: 0xededed,
    ink2: 0xa3a3a3,
    faint: 0x9c9c9c,
    rule: 0x353535,
    rule_soft: 0x2a2a2a,
    chip: 0x2e2e2e,
    signal: 0xfffa00,
    on_signal: 0x161616,
    hover: 0x191919,
    cursor: 0x303030,
    tag_bg: 0x3a3a3a,
    danger: 0xff8a7a,
    on_danger: 0x161616,
    on_control: 0x161616,
};

static MODE: AtomicU8 = AtomicU8::new(0);

pub fn mode() -> Appearance {
    if MODE.load(Ordering::Relaxed) == 1 {
        Appearance::Dark
    } else {
        Appearance::Light
    }
}

pub fn set_mode(mode: Appearance) {
    MODE.store(u8::from(mode.is_dark()), Ordering::Relaxed);
}

fn palette() -> &'static Palette {
    match mode() {
        Appearance::Light => &LIGHT,
        Appearance::Dark => &DARK,
    }
}

#[cfg(test)]
pub(crate) fn palette_for(mode: Appearance) -> &'static Palette {
    match mode {
        Appearance::Light => &LIGHT,
        Appearance::Dark => &DARK,
    }
}

fn c(value: u32) -> Rgba {
    rgb(value)
}

/// 纸色。窗口底、侧栏、顶栏和底栏。比板色更暗，让板浮出来。
pub fn window_base() -> Rgba {
    c(palette().paper)
}
/// 板色。主内容区与右面板的底，是给人读的那一张纸。
pub fn bg() -> Rgba {
    c(palette().panel)
}
/// 通栏外壳（顶栏与底栏）的底色，与纸同色，靠 1px 细线分区。
pub fn chrome() -> Rgba {
    window_base()
}

/// 面板底色，与 [`bg`] 同为板色。
pub fn panel() -> Rgba {
    bg()
}

/// 输入框、搜索框的底色。与板同色，靠 1px 细线分界。
pub fn surface() -> Rgba {
    bg()
}
/// 安静的填充，用于待机按钮、标签块与列头。
pub fn elevated() -> Rgba {
    c(palette().chip)
}
/// 列表行的悬停底色，退回纸色。
pub fn hover() -> Rgba {
    c(palette().hover)
}
/// 键盘光标所在行的底色。比 [`hover`] 更醒目的一档；浅色更深，深色更浅。
pub fn cursor() -> Rgba {
    c(palette().cursor)
}

/// 分隔线。
pub fn border() -> Rgba {
    c(palette().rule)
}
/// 比 [`border`] 更弱的分隔线。
pub fn border_soft() -> Rgba {
    c(palette().rule_soft)
}
/// 有意义的线，用于当前项、选中段与强调边框。
pub fn rule_strong() -> Rgba {
    text()
}

/// 标出「这里」的线。浅色用墨，深色用黄。行首的悬停、光标条与焦点环都走它。
pub fn trace() -> Rgba {
    trace_for(palette())
}

pub(crate) fn trace_for(palette: &Palette) -> Rgba {
    if palette.dark {
        c(palette.signal)
    } else {
        c(palette.ink)
    }
}

pub fn text() -> Rgba {
    c(palette().ink)
}
/// 歌手、专辑、时长这类次要信息。
pub fn text_muted() -> Rgba {
    c(palette().ink2)
}
/// 序号、占位符、次要图标。
pub fn text_faint() -> Rgba {
    c(palette().faint)
}

/// 唯一的强调色。当前位置、可点击内容、醒目提醒。只做填充，上面写深色字。
pub fn accent() -> Rgba {
    c(palette().signal)
}
pub fn accent_hover() -> Rgba {
    c(0xe6e100)
}
/// 强调色上的字与图形。两套主题都是深墨色，黄块上不存在浅色字。
pub fn text_on_accent() -> Rgba {
    c(palette().on_signal)
}
/// 当前行与拖放高亮的淡底色，`accent` 的 0.20 alpha。
pub fn accent_soft() -> Rgba {
    accent().opacity(0.20)
}

/// 主控件的填充色。播放键、进度条已播段、勾选框。墨色，悬停时被黄色擦入。
pub fn control() -> Rgba {
    text()
}
/// 主控件悬停档，黄色。字与图形同时换成 [`text_on_accent`]。
pub fn control_hover() -> Rgba {
    accent()
}
/// 压在 [`control`] 上的图形，如播放三角与勾选框的对勾，浅色用白、深色用墨。
pub fn text_on_control() -> Rgba {
    c(palette().on_control)
}

/// 红色只给删除，其余告警用黄块和墨线。
pub fn danger() -> Rgba {
    c(palette().danger)
}
pub fn text_on_danger() -> Rgba {
    c(palette().on_danger)
}
pub fn danger_soft() -> Rgba {
    danger().opacity(0.10)
}
/// 扫描等进行中指示。与正文同色，靠黄色小旗提示，不引入红绿语义色。
pub fn success() -> Rgba {
    text_muted()
}

/// 进度与音量轨道的底色。
pub fn track() -> Rgba {
    elevated()
}

/// 格式标签（wav / mp3）的底色。
pub fn tag_bg() -> Rgba {
    c(palette().tag_bg)
}

/// 完全透明的背景，用于「未选中行」占位。
pub fn transparent() -> Rgba {
    rgba(0x00000000)
}

/// 界面宽体拉丁。Saira，静态切到 100% 宽度。
pub const FONT_UI: &str = "Saira";
/// 字段数字。Saira 112% 宽，等宽数字。
pub const FONT_FIELD: &str = "Saira Field";
/// 眉标与字标，Saira 125% 宽。
pub const FONT_LABEL: &str = "Saira Label";

/// 界面字体。Saira 没有中文字形，回退到系统黑体。
pub fn ui_font() -> Font {
    Font {
        family: FONT_UI.into(),
        features: FontFeatures::default(),
        fallbacks: Some(FontFallbacks::from_fonts(cjk_fallbacks())),
        weight: FontWeight::NORMAL,
        style: FontStyle::Normal,
    }
}

/// 字段字体，宽体 + 等宽数字。
pub fn field_font() -> Font {
    Font {
        family: FONT_FIELD.into(),
        features: tabular_features(),
        fallbacks: Some(FontFallbacks::from_fonts(cjk_fallbacks())),
        weight: FontWeight::NORMAL,
        style: FontStyle::Normal,
    }
}

/// 眉标与字标字体，125% 宽。
pub fn label_font() -> Font {
    Font {
        family: FONT_LABEL.into(),
        features: FontFeatures::default(),
        fallbacks: Some(FontFallbacks::from_fonts(cjk_fallbacks())),
        weight: FontWeight::SEMIBOLD,
        style: FontStyle::Normal,
    }
}

/// 系统里能接住中文的黑体。顺序即优先级，缺哪个就落下一个。
pub fn cjk_fallbacks() -> Vec<String> {
    if cfg!(target_os = "windows") {
        vec![
            "Microsoft YaHei UI".to_owned(),
            "Microsoft YaHei".to_owned(),
        ]
    } else if cfg!(target_os = "macos") {
        vec!["PingFang SC".to_owned(), "Hiragino Sans GB".to_owned()]
    } else {
        vec![
            "Noto Sans CJK SC".to_owned(),
            "Noto Sans SC".to_owned(),
            "WenQuanYi Micro Hei".to_owned(),
        ]
    }
}

/// 等宽数字特性。Saira 自带 `tnum`，不依赖系统字体。
fn tabular_features() -> FontFeatures {
    FontFeatures(Arc::new(vec![("tnum".to_owned(), 1)]))
}

/// 徽标、快捷键提示、眉标。
pub const FONT_MICRO: f32 = 10.0;
/// 时长、时间戳、辅助信息、格式标签的基准。
pub const FONT_CAPTION: f32 = 12.0;
/// 列表副标题、按钮、导航项。
pub const FONT_BODY: f32 = 14.0;
/// 列表主标题，即歌名。
pub const FONT_TRACK: f32 = 15.0;
/// 面板标题、底栏当前曲名。
pub const FONT_SUBTITLE: f32 = 16.0;
/// 页面大标题、区块标题。
pub const FONT_TITLE: f32 = 22.0;
/// 页面头部标题，即压在标题区上的那一行。
pub const FONT_HERO: f32 = 34.0;
/// 字号之间的相对关系是布局前提，写成编译期断言。
const _: () = assert!(FONT_HERO > FONT_TITLE);
const _: () = assert!(FONT_HERO > FONT_SUBTITLE);
const _: () = assert!(FONT_HERO <= 40.0);
/// 空状态。
pub const FONT_DISPLAY: f32 = 30.0;
/// 格式标签（wav / mp3）的字号。
pub const FONT_TAG: f32 = 10.0;
/// 眉标与字段。
pub const FONT_EYEBROW: f32 = 10.0;
pub const FONT_FIELD_SIZE: f32 = 12.0;
/// 首页字标。
pub const FONT_WORDMARK: f32 = 64.0;
/// 展示页的空心大编号。
pub const FONT_HOLLOW_NO: f32 = 120.0;

/// 数字（时长、列表序号、区块编号）的宽体等宽字体。
pub const FONT_MONO: &str = FONT_FIELD;

pub const WEIGHT_TITLE: FontWeight = FontWeight::BOLD;
pub const WEIGHT_SUBTITLE: FontWeight = FontWeight::SEMIBOLD;
/// 按钮与导航项的字重，600。
pub const WEIGHT_BUTTON: FontWeight = FontWeight::SEMIBOLD;
/// 列表歌名。常态 regular，播放态额外加粗，见 `track_list`。
pub const WEIGHT_TRACK: FontWeight = FontWeight::NORMAL;

/// 圆角一律为 0，形状语言只分直角与切角。
pub const RADIUS_SM: f32 = 0.0;
pub const RADIUS_MD: f32 = 0.0;
pub const RADIUS_LG: f32 = 0.0;
pub const RADIUS_XL: f32 = 0.0;
pub const RADIUS_CARD: f32 = 0.0;
/// 胶囊圆角退役，保留常量给仍按「旧规格」取值的控件，值为 0。
pub const RADIUS_PILL: f32 = 0.0;

/// 切角尺寸。可点的小块切右下角，抽屉切左上角。
pub const CUT_SM: f32 = 6.0;
pub const CUT: f32 = 8.0;
pub const CUT_LG: f32 = 24.0;
/// 细线宽度。
pub const RULE_W: f32 = 1.0;
/// 三色条的厚度。
pub const TRI_H: f32 = 3.0;

/// 卡片之间的缝宽，也是窗口四周的留白。
pub const GUTTER: f32 = 8.0;

pub const GAP_XS: f32 = 4.0;
pub const GAP_SM: f32 = 8.0;
pub const GAP_MD: f32 = 12.0;
pub const GAP_LG: f32 = 16.0;
pub const GAP_XL: f32 = 24.0;

/// 引擎播不了的曲目在列表里压暗到此值。
pub const UNPLAYABLE_OPACITY: f32 = 0.5;

/// 顶部栏高度。
pub const TOP_BAR_H: f32 = 52.0;
pub const BOTTOM_BAR_H: f32 = 76.0;
/// 底部控制栏在窄窗口下的高度，此时分两行，传输区与进度条。
pub const BOTTOM_BAR_H_STACKED: f32 = 100.0;

/// 底栏高度。底栏自身与根视图的 toast 定位均需读取此值。
pub fn bottom_bar_height(stacked: bool) -> f32 {
    if stacked {
        BOTTOM_BAR_H_STACKED
    } else {
        BOTTOM_BAR_H
    }
}

/// 页面内容的左右边距。页头、区块与卡片共用这一条竖线。
pub const PAGE_PAD: f32 = GAP_XL;

/// 侧边导航展开宽度。
pub const SIDEBAR_W: f32 = 232.0;
pub const SIDEBAR_ICON_W: f32 = 64.0;
/// 导航项高度，容得下英文眉标 + 中文两行。
pub const SIDEBAR_ITEM_H: f32 = 48.0;
pub const SIDEBAR_INDICATOR_W: f32 = 4.0;
pub const RIGHT_PANEL_W: f32 = 360.0;
/// 底栏左栏满宽，即曲目身份区。要放得下 52px 封面、一个不太早截断的歌名和音质徽章；
/// 240 只够四个汉字，长一点的日文曲名一律变成「ありが…」。
pub const BOTTOM_LEFT_W: f32 = 360.0;
/// 中档窗口（[`crate::shell::MEDIUM_MIN`]）下的左栏宽度。那一档主区只剩一条传输带，
/// 左栏必须在这里停一下，否则传输键组会被挤掉。
pub const BOTTOM_LEFT_MEDIUM_W: f32 = 240.0;
/// 窗口宽到这个数，左栏才涨到 [`BOTTOM_LEFT_W`]。
pub const BOTTOM_LEFT_FULL_W: f32 = 1200.0;
/// 底栏右栏宽度，即功能入口与音量。
pub const BOTTOM_RIGHT_W: f32 = 320.0;
pub const BOTTOM_CENTER_MAX: f32 = 620.0;
pub const ROW_H_COMPACT: f32 = 40.0;
pub const ROW_H_COMFORTABLE: f32 = 52.0;
/// 行首「光标在这」竖条的宽度。
pub const ROW_CURSOR_BAR_W: f32 = 3.0;
/// 行首索引列宽。
pub const ROW_INDEX_W: f32 = 36.0;
/// 曲库列表专辑列与时长列的默认宽度。用户拖动表头后覆盖，落盘在 `settings.json`。
pub const COLUMN_ALBUM_W: f32 = 220.0;
pub const COLUMN_TIME_W: f32 = 60.0;
/// 列表行与列头的左内边距。
pub const ROW_PAD_LEFT: f32 = 12.0;
/// 行首编号、播放图标与右侧内容之间的间距。
pub const ROW_INDEX_GAP: f32 = 16.0;
pub const COLUMN_HEADER_H: f32 = 32.0;
pub const SEARCH_H: f32 = 34.0;
pub const SEARCH_W: f32 = 280.0;
pub const MINI_COVER: f32 = 52.0;

/// 键盘焦点环的宽度。画在元素外侧，不占布局。
pub const FOCUS_RING_W: f32 = 2.0;

/// 焦点环用 [`trace`]。浅色墨、深色黄，对纸、板与 chip 都有 3:1 以上。
fn focus_ring_color() -> Hsla {
    trace().into()
}

pub fn focus_ring(style: StyleRefinement) -> StyleRefinement {
    style.shadow(vec![
        BoxShadow::new(px(0.), px(0.), focus_ring_color()).spread_radius(px(FOCUS_RING_W)),
    ])
}

/// 机械动效。慢起硬停，像闸门。
pub fn ease_mech() -> impl Fn(f32) -> f32 {
    cubic_bezier(0.7, 0.0, 0.2, 1.0)
}

/// 出现与到位，迅速进入后稳住。
pub fn ease_snap() -> impl Fn(f32) -> f32 {
    cubic_bezier(0.16, 1.0, 0.3, 1.0)
}

/// 普通过渡，用于颜色与三色条扫过。
pub fn ease_std() -> impl Fn(f32) -> f32 {
    cubic_bezier(0.2, 0.7, 0.1, 1.0)
}

/// 一次操作的回应急在 100–400ms；单个入场 ≤ 650ms。
pub const MOTION_PRESS_MS: u64 = 100;
pub const MOTION_ROW_MS: u64 = 200;
/// 悬停回应。按钮 280ms mech，列表行 250ms snap。
pub const MOTION_HOVER_MS: u64 = 280;
pub const MOTION_HOVER_ROW_MS: u64 = 250;
pub const MOTION_PANEL_IN_MS: u64 = 280;
/// 抽屉退出 240ms；遮罩与歌词退出 160ms。
pub const MOTION_PANEL_OUT_MS: u64 = 240;
pub const MOTION_OVERLAY_OUT_MS: u64 = 160;
pub const MOTION_PAGE_MS: u64 = 360;
pub const MOTION_TOAST_IN_MS: u64 = 280;
pub const MOTION_TOAST_OUT_MS: u64 = 160;

/// CSS `cubic-bezier(x1, y1, x2, y2)` 的求值。牛顿迭代解 x(u) = t，再取 y(u)。
/// gpui 的 `with_easing` 只接受 `Fn(f32) -> f32`，没有现成的贝塞尔构造器。
fn cubic_bezier(x1: f32, y1: f32, x2: f32, y2: f32) -> impl Fn(f32) -> f32 {
    let sample = move |a: f32, b: f32, u: f32| {
        let v = 1.0 - u;
        3.0 * v * v * u * a + 3.0 * v * u * u * b + u * u * u
    };
    let slope = move |a: f32, b: f32, u: f32| {
        let v = 1.0 - u;
        3.0 * v * v * a + 6.0 * v * u * (b - a) + 3.0 * u * u * (1.0 - b)
    };

    move |t: f32| {
        let t = t.clamp(0.0, 1.0);
        let mut u = t;
        for _ in 0..6 {
            let x = sample(x1, x2, u) - t;
            if x.abs() < 1e-5 {
                break;
            }
            let d = slope(x1, x2, u);
            if d.abs() < 1e-5 {
                break;
            }
            u = (u - x / d).clamp(0.0, 1.0);
        }
        sample(y1, y2, u)
    }
}

/// WCAG 2.1 相对亮度。
#[cfg(test)]
fn relative_luminance(color: Rgba) -> f32 {
    fn channel(c: f32) -> f32 {
        if c <= 0.039_28 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    }
    0.2126 * channel(color.r) + 0.7152 * channel(color.g) + 0.0722 * channel(color.b)
}

/// 把 `fg` 合成到 `bg` 上。`opacity()` 只改 alpha、不做预乘，故按 source-over 计算。
#[cfg(test)]
fn over(fg: Rgba, bg: Rgba) -> Rgba {
    Rgba {
        r: fg.r * fg.a + bg.r * (1.0 - fg.a),
        g: fg.g * fg.a + bg.g * (1.0 - fg.a),
        b: fg.b * fg.a + bg.b * (1.0 - fg.a),
        a: 1.0,
    }
}

/// WCAG 2.1 对比度，返回 1.0 ~ 21.0。两个参数都允许带透明度，先依次压到 `base` 上再算。
#[cfg(test)]
fn contrast_ratio(foreground: Rgba, background: Rgba, base: Rgba) -> f32 {
    let bg_opaque = over(background, base);
    let fg_opaque = over(foreground, bg_opaque);

    let (l1, l2) = (relative_luminance(fg_opaque), relative_luminance(bg_opaque));
    let (hi, lo) = if l1 > l2 { (l1, l2) } else { (l2, l1) };
    (hi + 0.05) / (lo + 0.05)
}

#[cfg(test)]
mod tests {
    use crate::platform::{Rgba, rgb};

    use super::{Appearance, Palette, trace_for};

    /// WCAG AA 正文的门槛。
    const AA: f32 = 4.5;
    /// 非文字对比度的门槛，适用于焦点环与当前行底色。
    const NON_TEXT_AA: f32 = 3.0;

    fn color(value: u32) -> Rgba {
        rgb(value)
    }

    fn modes() -> [(Appearance, &'static Palette); 2] {
        [
            (Appearance::Light, super::palette_for(Appearance::Light)),
            (Appearance::Dark, super::palette_for(Appearance::Dark)),
        ]
    }

    fn assert_pass(fg: Rgba, surface: Rgba, base: Rgba, name: &str) {
        let ratio = super::contrast_ratio(fg, surface, base);
        assert!(
            ratio >= AA,
            "{name} 的对比度只有 {ratio:.2}:1，低于 AA 要求的 {AA}:1"
        );
    }

    fn assert_same_color(a: Rgba, b: Rgba, name: &str) {
        for (channel, a, b) in [("r", a.r, b.r), ("g", a.g, b.g), ("b", a.b, b.b)] {
            assert!(
                (a - b).abs() < 0.02,
                "{name} 的 {channel} 通道是 {a:.3}，期望 {b:.3}"
            );
        }
    }

    #[test]
    fn light_palette_keeps_the_established_values() {
        let p = super::palette_for(Appearance::Light);
        assert_eq!(p.paper, 0xf4f4f2);
        assert_eq!(p.panel, 0xffffff);
        assert_eq!(p.ink, 0x161616);
        assert_eq!(p.ink2, 0x3f3f3f);
        assert_eq!(p.faint, 0x5c5c5c);
        assert_eq!(p.rule, 0xd9d9d6);
        assert_eq!(p.chip, 0xe4e4e0);
        assert_eq!(p.signal, 0xfffa00);
        assert_eq!(p.on_control, 0xffffff);
    }

    #[test]
    fn dark_palette_matches_the_second_theme_spec() {
        let light = super::palette_for(Appearance::Light);
        let dark = super::palette_for(Appearance::Dark);

        assert_eq!(dark.paper, 0x191919);
        assert_eq!(dark.panel, 0x222222);
        assert_eq!(dark.ink, 0xededed);
        assert_eq!(dark.ink2, 0xa3a3a3);
        assert_eq!(dark.rule, 0x353535);
        assert_eq!(dark.chip, 0x2e2e2e);
        assert_eq!(dark.danger, 0xff8a7a);
        // 封面占位是墨块 + 反白字。深色的墨是浅色，字必须跟着翻成深墨，
        // 否则浅墨底压白字，整张占位只剩一块空白色块。
        assert_eq!(
            dark.on_control, 0x161616,
            "深色的封面首字必须与墨底反相，白字压在浅墨上读不出来"
        );

        assert_eq!(light.signal, dark.signal, "强调色两套一致");
        assert_eq!(
            light.on_signal, dark.on_signal,
            "黄块上的字两套都必须是深墨"
        );
        assert_same_color(trace_for(light), color(light.ink), "浅色的 trace");
        assert_same_color(trace_for(dark), color(dark.signal), "深色的 trace");
        assert_same_color(super::text_on_control(), rgb(0xffffff), "封面占位上的字母");
    }

    #[test]
    fn text_colors_pass_aa_on_every_background_layer() {
        for (mode, p) in modes() {
            let base = color(p.panel);
            let layers = [
                ("bg", color(p.panel)),
                ("panel", color(p.panel)),
                ("elevated", color(p.chip)),
                ("surface", color(p.panel)),
                ("chrome", color(p.paper)),
            ];
            let colors = [
                ("text", color(p.ink)),
                ("text_muted", color(p.ink2)),
                ("text_faint", color(p.faint)),
            ];

            for (layer_name, layer) in layers {
                for (color_name, fg) in colors {
                    assert_pass(
                        fg,
                        layer,
                        base,
                        &format!("{mode:?} {color_name} on {layer_name}"),
                    );
                }
            }
        }
    }

    #[test]
    fn accent_variants_pass_aa_in_their_intended_role() {
        for (mode, p) in modes() {
            let base = color(p.panel);
            assert_pass(
                color(p.on_signal),
                color(p.signal),
                base,
                &format!("{mode:?} 黄块上的字"),
            );
            assert_pass(
                color(p.ink),
                color(p.panel),
                base,
                &format!("{mode:?} 正文 on 板"),
            );
            assert_pass(
                color(p.ink),
                color(p.paper),
                base,
                &format!("{mode:?} 正文 on 纸"),
            );
        }
    }

    #[test]
    fn control_fill_keeps_opposite_glyphs_legible() {
        for (mode, p) in modes() {
            let base = color(p.panel);
            assert_pass(
                color(p.on_control),
                color(p.ink),
                base,
                &format!("{mode:?} glyph on control"),
            );
            assert_pass(
                color(p.on_signal),
                color(p.signal),
                base,
                &format!("{mode:?} glyph on control hover"),
            );
        }
    }

    #[test]
    fn window_close_button_uses_contrast_glyph_on_danger() {
        for (mode, p) in modes() {
            let base = color(p.panel);
            assert_pass(
                color(p.on_danger),
                color(p.danger),
                base,
                &format!("{mode:?} text_on_danger on danger"),
            );
        }
    }

    #[test]
    fn playing_row_is_readable() {
        for (mode, p) in modes() {
            let base = color(p.panel);
            assert_pass(
                color(p.ink),
                color(p.signal).opacity(0.20),
                base,
                &format!("{mode:?} ink on accent_soft"),
            );
        }
    }

    #[test]
    fn cards_sit_brighter_than_the_gap_around_them() {
        for (mode, p) in modes() {
            let bg = super::relative_luminance(color(p.panel));
            let gap = super::relative_luminance(color(p.paper));
            let chrome = super::relative_luminance(color(p.paper));
            assert!(bg > gap + 1e-6, "{mode:?} 主内容的板必须比纸色外壳亮");
            assert!(bg > chrome + 1e-6, "{mode:?} 主内容的板必须比通栏外壳亮");
        }

        assert_eq!(
            super::panel(),
            super::bg(),
            "侧栏 / 右面板与主区同色：分区交给 1px 细线，不交给明暗差"
        );
        assert_eq!(super::chrome(), super::window_base());
    }

    #[test]
    fn status_colors_pass_aa() {
        for (mode, p) in modes() {
            let base = color(p.panel);
            assert_pass(
                color(p.danger),
                color(p.panel),
                base,
                &format!("{mode:?} danger on panel"),
            );
            assert_pass(
                color(p.ink2),
                color(p.panel),
                base,
                &format!("{mode:?} success on panel"),
            );
        }
    }

    #[test]
    fn settings_selection_is_readable_without_color() {
        for (mode, p) in modes() {
            let base = color(p.panel);
            assert_pass(
                color(p.ink2),
                color(p.panel),
                base,
                &format!("{mode:?} 未选中分段文字"),
            );
            assert_pass(
                color(p.on_control),
                color(p.ink),
                base,
                &format!("{mode:?} 选中分段文字"),
            );
        }
    }

    #[test]
    fn hover_row_stays_readable() {
        for (mode, p) in modes() {
            let base = color(p.panel);
            assert_pass(
                color(p.ink),
                color(p.hover),
                base,
                &format!("{mode:?} text on hover"),
            );
            assert_pass(
                color(p.ink2),
                color(p.hover),
                base,
                &format!("{mode:?} text_muted on hover"),
            );
        }
    }

    #[test]
    fn disabled_transport_button_is_readable() {
        for (mode, p) in modes() {
            let base = color(p.panel);
            for (name, fg) in [
                ("text", p.ink),
                ("text_muted", p.ink2),
                ("text_faint", p.faint),
            ] {
                assert_pass(
                    color(fg),
                    color(p.chip),
                    base,
                    &format!("{mode:?} {name} on track"),
                );
            }
        }
    }

    #[test]
    fn cursor_row_is_distinct_from_hover_and_the_page() {
        for (mode, p) in modes() {
            let base = color(p.panel);
            let hover = color(p.hover);
            let cursor = color(p.cursor);
            let hover_l = super::relative_luminance(hover);
            let cursor_l = super::relative_luminance(cursor);

            assert!(
                (cursor_l - hover_l).abs() > 1e-3,
                "{mode:?} 的光标行和悬停行同色，鼠标停 A 行、光标在 B 行就分不出来"
            );

            let separation = super::contrast_ratio(cursor, hover, base);
            assert!(
                separation >= 1.15,
                "{mode:?} 光标行与悬停行只差 {separation:.2}:1"
            );

            let ratio = super::contrast_ratio(cursor, base, base);
            assert!(
                ratio >= 1.2,
                "{mode:?} 光标行底色对列表底只有 {ratio:.2}:1，长列表里找不出是哪一行"
            );

            for (name, fg) in [
                ("text", p.ink),
                ("text_muted", p.ink2),
                ("text_faint", p.faint),
            ] {
                assert_pass(
                    color(fg),
                    cursor,
                    base,
                    &format!("{mode:?} {name} on cursor"),
                );
            }
        }
    }

    #[test]
    fn focus_ring_is_trace_and_visible() {
        for (mode, p) in modes() {
            let ring = trace_for(p);
            let base = color(p.panel);
            for (name, surface) in [
                ("bg", color(p.panel)),
                ("elevated", color(p.chip)),
                ("chrome", color(p.paper)),
            ] {
                let ratio = super::contrast_ratio(ring, surface, base);
                assert!(
                    ratio >= NON_TEXT_AA,
                    "{mode:?} 焦点环压在 {name} 上只有 {ratio:.2}:1，\
                     低于非文字对比度的 {NON_TEXT_AA}:1"
                );
            }
        }

        let ring: Rgba = super::focus_ring_color().into();
        assert_same_color(
            ring,
            trace_for(super::palette_for(Appearance::Light)),
            "焦点环（浅色）",
        );
    }

    #[test]
    fn tag_text_is_readable_on_its_pill() {
        for (mode, p) in modes() {
            let base = color(p.panel);
            assert_pass(
                color(p.ink2),
                color(p.tag_bg),
                base,
                &format!("{mode:?} 格式标签文字 on 标签底"),
            );
        }
    }

    #[test]
    fn luminance_endpoints_are_sane() {
        let white = rgb(0xffffff);
        let black = rgb(0x000000);
        assert!((super::relative_luminance(white) - 1.0).abs() < 1e-4);
        assert!(super::relative_luminance(black).abs() < 1e-6);
        assert!((super::contrast_ratio(white, black, black) - 21.0).abs() < 0.05);
    }

    #[test]
    fn motion_curves_start_and_end_where_they_should() {
        fn check(curve: impl Fn(f32) -> f32) {
            assert!(curve(0.0).abs() < 0.01, "缓动曲线的起点必须是 0");
            assert!((curve(1.0) - 1.0).abs() < 0.01, "缓动曲线的终点必须是 1");
        }
        check(super::ease_snap());
        check(super::ease_mech());
        check(super::ease_std());
    }
}
