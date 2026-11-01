//! 按钮一族，含图标按钮 [`IconButton`]、文字按钮 [`text_button`]、胶囊 chip 与四种规格 [`ButtonKind`]。

use crate::icon::{Glyph, icon_inherit};
use crate::platform::*;
use crate::theme;

/// 图标按钮。链式配置，最后调 [`render`](IconButton::render)。
pub struct IconButton {
    id: ElementId,
    glyph: Glyph,
    box_size: f32,
    glyph_size: f32,
    color: Rgba,
    hover_color: Rgba,
    base_bg: Rgba,
    hover_bg: Rgba,
    enabled: bool,
    label: Option<&'static str>,
    /// 悬停擦入进度；`None` 表示该按钮不参与动效，即调用方未提供 window。
    hover: Option<f32>,
    on_click: Option<ClickHandler>,
}

/// 图标按钮的点击回调形状。到处写全 `Box<dyn Fn…>` 已经跨过 clippy 的复杂度上限。
type ClickHandler = Box<dyn Fn(&ClickEvent, &mut Window, &mut App) + 'static>;

impl IconButton {
    pub fn new(id: impl Into<ElementId>, glyph: Glyph) -> Self {
        Self {
            id: id.into(),
            glyph,
            box_size: 32.0,
            glyph_size: 16.0,
            color: theme::text_muted(),
            hover_color: theme::text(),
            base_bg: theme::transparent(),
            hover_bg: theme::elevated(),
            enabled: true,
            label: None,
            hover: None,
            on_click: None,
        }
    }

    pub fn box_size(mut self, size: f32) -> Self {
        self.box_size = size;
        self
    }

    pub fn glyph_size(mut self, size: f32) -> Self {
        self.glyph_size = size;
        self
    }

    /// 常态颜色。悬停色不在这里配，由 `new` 定成 `text`。
    pub fn color(mut self, color: Rgba) -> Self {
        self.color = color;
        self
    }

    /// 禁用态，降级颜色且不绑事件。
    pub fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    /// 悬停提示。同时写入无障碍名称 `aria_label`，图标按钮除图标外没有其它名字。
    pub fn label(mut self, label: &'static str) -> Self {
        self.label = Some(label);
        self
    }

    /// 采样悬停进度，让悬停底色从左擦入。必须在渲染期调用。
    pub fn hover_progress(mut self, window: &mut Window, cx: &mut App) -> Self {
        self.hover = Some(crate::common::hover::progress(self.id.clone(), window, cx));
        self
    }

    /// 选中态，整块黄底墨字。只靠白与灰两档认不出开关是开是关。
    pub fn active(mut self, active: bool) -> Self {
        if active {
            self.base_bg = theme::accent();
            self.hover_bg = theme::accent_hover();
            self.color = theme::text_on_accent();
            self.hover_color = theme::text_on_accent();
        }
        self
    }

    pub fn on_click(
        mut self,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_click = Some(Box::new(handler));
        self
    }

    /// 返回带 id 的元素，可直接作为下拉菜单的触发器，见 [`crate::common::menu::dropdown`]。
    pub fn render(self) -> Stateful<Div> {
        let IconButton {
            id,
            glyph,
            box_size,
            glyph_size,
            color,
            hover_color,
            base_bg,
            hover_bg,
            enabled,
            label,
            hover,
            on_click,
        } = self;

        let effective_color = if enabled { color } else { theme::text_faint() };
        let hover_id = id.clone();

        let mut element = div()
            .id(id)
            .relative()
            .flex_none()
            .size(px(box_size))
            .flex()
            .items_center()
            .justify_center()
            .text_color(effective_color);

        if let Some(progress) = hover {
            element = element.child(
                div()
                    .absolute()
                    .left_0()
                    .top_0()
                    .bottom_0()
                    .w(relative(progress))
                    .bg(hover_bg),
            );
        }

        element = element.child(icon_inherit(glyph, glyph_size));

        if enabled {
            element = element
                .cursor_pointer()
                .on_hover(crate::common::hover::bind(hover_id))
                .hover(move |style| style.text_color(hover_color))
                // 按下就下沉一像素。100ms 级的回应，不做过渡。
                .active(|style| style.top(px(1.)).opacity(0.92))
                // 进入 Tab 序列。gpui 不提供默认的 Tab 绑定，需由 `lyre-app::keymap` 自行绑定；
                // 但聚焦后按 Enter / Space 触发 `on_click` 属内建行为，无需自行接管。
                .tab_index(TAB_STOP)
                .focus_visible(theme::focus_ring)
                .role(Role::Button);
            if let Some(handler) = on_click {
                element = element.on_click(handler);
            }
        } else {
            // 禁用态不进 Tab 序列。
            // 光标要显式设成默认箭头；否则沿用指针形状，看着仍像可点。
            element = element.cursor_default();
        }

        if base_bg != theme::transparent() {
            element = element.bg(base_bg);
        }

        if let Some(label) = label {
            element = element
                .aria_label(label)
                .tooltip(crate::common::tooltip::builder(label));
        }

        element
    }
}

/// Tab 序列里的位置。所有控件均用 0，即按文档顺序前进。
pub const TAB_STOP: isize = 0;

/// 按钮的几何规格，尺寸与字级只有这一个来源。
pub const BTN_PX: f32 = theme::GAP_LG;
pub const BTN_PY: f32 = theme::GAP_SM;
pub const BTN_FONT: f32 = theme::FONT_BODY;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ButtonKind {
    /// 实心强调色，一屏最多一个，用作主行动。
    Primary,
    /// 常驻灰底，次要但常驻的操作。
    Secondary,
    /// 透明底，悬停才出现底色。
    Ghost,
    /// 透明底 + 描边，下拉触发器用。
    Outline,
}

impl ButtonKind {
    /// 纵向内边距。gpui 的盒子高度 = 内容 + padding + border，
    /// 所以带描边的规格减 1px，四种规格的外框高度才相同。
    pub fn pad_y(self) -> f32 {
        match self {
            Self::Outline => BTN_PY - 1.0,
            Self::Primary | Self::Secondary | Self::Ghost => BTN_PY,
        }
    }

    /// 外框高度，不含文字行高，只计 padding 与边框。
    pub fn frame_height(self) -> f32 {
        let border = if self == Self::Outline { 2.0 } else { 0.0 };
        self.pad_y() * 2.0 + border
    }

    /// 常态底色、常态文字色、悬停底色与悬停文字色。
    fn colors(self) -> (Rgba, Rgba, Rgba, Rgba) {
        match self {
            Self::Primary => (
                theme::control(),
                theme::text_on_control(),
                theme::accent(),
                theme::text_on_accent(),
            ),
            Self::Secondary => (
                theme::elevated(),
                theme::text(),
                theme::accent(),
                theme::text_on_accent(),
            ),
            // Ghost 与 Outline 仅在描边上有别，由 `pad_y` / `frame_height` 处理；颜色完全一致。
            Self::Ghost | Self::Outline => (
                theme::transparent(),
                theme::text_muted(),
                theme::elevated(),
                theme::text(),
            ),
        }
    }
}

/// 按钮外壳。直角、右下切角、内边距、字级、悬停反馈；内容用 `.child`、行为用 `.on_click` 补。
/// 切角里不压字，右侧内边距 = 常态内边距 + 切角宽度。
pub fn button_shell(id: impl Into<ElementId>, kind: ButtonKind) -> Stateful<Div> {
    button_shell_with_hover(id, kind, None)
}

/// 与 [`button_shell`] 相同，但悬停底色从左擦入。
pub fn button_shell_animated(
    id: impl Into<ElementId>,
    kind: ButtonKind,
    window: &mut Window,
    cx: &mut App,
) -> Stateful<Div> {
    let id = id.into();
    let hover = crate::common::hover::progress(id.clone(), window, cx);
    button_shell_with_hover(id, kind, Some(hover))
}

fn button_shell_with_hover(
    id: impl Into<ElementId>,
    kind: ButtonKind,
    hover: Option<f32>,
) -> Stateful<Div> {
    let (base_bg, base_color, hover_bg, hover_color) = kind.colors();
    let id = id.into();

    let mut shell = div()
        .id(id.clone())
        .relative()
        .flex_none()
        .flex()
        .items_center()
        .gap(px(theme::GAP_XS))
        .pl(px(BTN_PX))
        .pr(px(BTN_PX + theme::CUT))
        .py(px(kind.pad_y()))
        .text_size(px(BTN_FONT))
        .font_weight(theme::WEIGHT_BUTTON)
        .text_color(base_color)
        .bg(base_bg)
        .cursor_pointer()
        .tab_index(TAB_STOP)
        .focus_visible(theme::focus_ring)
        .role(Role::Button);

    shell = match hover {
        Some(progress) => shell
            // 黄块从左擦入，盖在常态底色上、切角和文字之下。
            .on_hover(crate::common::hover::bind(id))
            .hover(move |style| style.text_color(hover_color))
            .child(
                div()
                    .absolute()
                    .left_0()
                    .top_0()
                    .bottom_0()
                    .w(relative(progress))
                    .bg(hover_bg),
            ),
        None => shell.hover(move |style| style.bg(hover_bg).text_color(hover_color)),
    };

    // 切角画在背景之后、内容之前；右侧留白保证内容不进入切角。
    shell = shell
        .active(|style| style.top(px(1.)).opacity(0.92))
        .child(crate::common::ornament::cut_br(theme::CUT, theme::bg()));

    if kind == ButtonKind::Outline {
        shell.border_1().border_color(theme::border())
    } else {
        shell
    }
}

pub fn chip_colors(active: bool) -> (Rgba, Rgba) {
    if active {
        (theme::accent(), theme::text_on_accent())
    } else {
        (theme::elevated(), theme::text_muted())
    }
}

/// 「开关」小块。图标 + 文字，选中态走 [`chip_colors`]。右下切角、计数是真实数字。
pub fn toggle_chip(
    id: impl Into<ElementId>,
    glyph: Glyph,
    label: impl Into<SharedString>,
    active: bool,
) -> Stateful<Div> {
    let label: SharedString = label.into();
    let (base_bg, base_color) = chip_colors(active);

    div()
        .id(id)
        .relative()
        // 可收缩而不是 `flex_none`。详情页头图那一排在 560px 窗口下放不下四件东西，
        // 不肯收缩就会被主区卡片裁掉最右的「更多」。文字由下面的 `truncate` 兜住。
        .min_w_0()
        .flex()
        .items_center()
        .gap(px(theme::GAP_SM))
        .pl(px(theme::GAP_MD))
        .pr(px(theme::GAP_MD + theme::CUT))
        .py(px(6.))
        .tab_index(TAB_STOP)
        .focus_visible(theme::focus_ring)
        .role(Role::Button)
        .aria_selected(active)
        .bg(base_bg)
        .text_color(base_color)
        .cursor_pointer()
        .when(!active, |chip| {
            chip.hover(|style| {
                style
                    .bg(theme::accent())
                    .text_color(theme::text_on_accent())
            })
        })
        .active(|style| style.top(px(1.)).opacity(0.92))
        .text_size(px(theme::FONT_CAPTION))
        .font_weight(theme::WEIGHT_BUTTON)
        .child(crate::common::ornament::cut_br(theme::CUT, theme::bg()))
        // 图标跟随父级 `text_color`，见 [`icon_inherit`]。
        .child(icon_inherit(glyph, 14.))
        .child(div().min_w_0().truncate().child(label))
}

pub fn text_button(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    kind: ButtonKind,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    // `IntoElement` 只对具体类型实现，泛型 `impl Into<SharedString>` 要先落成 `SharedString`。
    let label: SharedString = label.into();
    button_shell(id, kind).on_click(on_click).child(label)
}

/// 与 [`text_button`] 相同，但悬停底色从左擦入。
pub fn text_button_animated(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    kind: ButtonKind,
    window: &mut Window,
    cx: &mut App,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    let label: SharedString = label.into();
    button_shell_animated(id, kind, window, cx)
        .on_click(on_click)
        .child(label)
}

#[cfg(test)]
mod tests {
    use super::ButtonKind;

    #[test]
    fn button_kinds_are_distinct() {
        assert_ne!(ButtonKind::Primary, ButtonKind::Ghost);
        assert_ne!(ButtonKind::Primary, ButtonKind::Secondary);
        assert_ne!(ButtonKind::Secondary, ButtonKind::Ghost);
        assert_ne!(ButtonKind::Outline, ButtonKind::Ghost);
    }

    #[test]
    fn every_kind_has_the_same_frame_height() {
        let kinds = [
            ButtonKind::Primary,
            ButtonKind::Secondary,
            ButtonKind::Ghost,
            ButtonKind::Outline,
        ];
        let expected = kinds[0].frame_height();
        for kind in kinds {
            assert_eq!(
                kind.frame_height(),
                expected,
                "{kind:?} 的外框高度和别的规格不一致，并排时会一高一矮"
            );
        }
    }
}
