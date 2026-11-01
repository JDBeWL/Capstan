//! GPUI 依赖的唯一入口。crates.io 上 `gpui` 有多个互不兼容、版本不互通的分叉，换分叉只改这个文件。
//! 约定。只有这里可以出现 `gpui` / `gpui_component` 这两个名字，其余文件一律 `use crate::platform::*;`。
//! `pub use gpui::*` 带入的同名 `test` 宏会遮蔽内置 `#[test]`，测试模块因此不 glob。

use std::borrow::Cow;

pub use gpui::*;

pub use gpui::prelude::*;

/// 组件库的窗口根。每个窗口的根视图都必须包一层，否则主题、浮层、焦点陷阱拿不到宿主上下文。
pub use gpui_component::Root;

/// 图标元素。gpui 把 SVG 渲染成纯 alpha 蒙版、只用 `text_color` 填色，SVG 里的 fill 无效。
pub use gpui_component::Icon;

pub use gpui_component::Sizable;

/// 文本输入。自带 `key_context("Input")`，根视图上的裸字母快捷键要靠 `&& !Input` 谓词避让，
/// 否则在搜索框里输入「s」会切换播放模式。
pub use gpui_component::input::{Input, InputEvent, InputState};

pub use gpui_component::ThemeMode;

/// 纯时序过渡。状态键、缓动和逐帧采样，不绑任何视觉属性。
pub use gpui_base::motion;

/// 组件库的主题全局态，只管组件库控件内部的颜色；本项目的配色在 `crate::theme`。
pub use gpui_component::Theme;
/// 右键菜单。`context_menu` 只在右键打开；左键点击展开用 [`crate::common::menu::dropdown`]。
pub use gpui_component::menu::{ContextMenuExt, DropdownMenu, PopupMenu, PopupMenuItem};

/// 点击展开的浮层。触发器要 `Selectable`，本项目用 [`crate::common::menu::MenuTrigger`] 适配。
pub use gpui_component::popover::Popover;

/// 模态对话框。`window.open_dialog(cx, |dialog, ..| ..)` 由组件库的 `Root` 托管层与焦点，
/// 因此必须经由 `WindowExt`，而非自行绘制一层遮罩。
pub use gpui_component::WindowExt;

pub use gpui_component::Selectable;

/// 滚动条。gpui 本身不绘制滚动条，虚拟列表仅维护偏移量，屏幕上空无一物。
/// 组件库这个独立元素要的是 [`ScrollbarHandle`]，两个句柄都实现了它。
pub use gpui_component::scroll::{Scrollbar, ScrollbarHandle, ScrollbarMode};

pub fn application() -> Application {
    gpui_platform::application()
}

/// 内嵌的 Saira 静态切字。100% / 112% / 125% 三种宽度，各带常用字重。
/// gpui 没有 `font-stretch`，宽度只能在字体文件里切好，再用不同族名选用。
const UI_FONTS: [&[u8]; 8] = [
    include_bytes!("../assets/fonts/Saira-Regular.ttf"),
    include_bytes!("../assets/fonts/Saira-SemiBold.ttf"),
    include_bytes!("../assets/fonts/Saira-Bold.ttf"),
    include_bytes!("../assets/fonts/Saira-ExtraBold.ttf"),
    include_bytes!("../assets/fonts/SairaField-Regular.ttf"),
    include_bytes!("../assets/fonts/SairaField-SemiBold.ttf"),
    include_bytes!("../assets/fonts/SairaLabel-SemiBold.ttf"),
    include_bytes!("../assets/fonts/SairaLabel-ExtraBold.ttf"),
];

/// 初始化组件库。必须在开窗前调用，否则开窗时 panic：
/// `no state of type gpui_component::theme::Theme exists`。
pub fn init(cx: &mut App) {
    if let Err(err) = cx.text_system().add_fonts(
        UI_FONTS
            .iter()
            .map(|data| Cow::Borrowed(*data))
            .collect::<Vec<_>>(),
    ) {
        log::error!("加载界面字体失败：{err}");
    }

    gpui_component::init(cx);
    // 组件库只管输入框与菜单等浮层内部的颜色，本项目的配色在 `crate::theme`。
    set_appearance(crate::theme::Appearance::Light, cx);
}

/// 一次切换两边的明暗。先定本项目的 palette，再把组件库的 `ThemeMode` 和颜色跟上。
/// 窗口已经开着时，调用方还要 `cx.refresh_windows()`，否则缓存过的元素不会重画。
pub fn set_appearance(appearance: crate::theme::Appearance, cx: &mut App) {
    crate::theme::set_mode(appearance);
    let mode = if appearance.is_dark() {
        ThemeMode::Dark
    } else {
        ThemeMode::Light
    };
    Theme::change(mode, None, cx);

    Theme::update(cx, |theme| {
        theme.radius = px(0.);
        theme.radius_lg = px(0.);
        theme.shadow = false;
        theme.font_family = crate::theme::FONT_UI.into();
        theme.mono_font_family = crate::theme::FONT_FIELD.into();
        theme.font_size = px(crate::theme::FONT_BODY);

        theme.colors.background = crate::theme::bg().into();
        theme.colors.foreground = crate::theme::text().into();
        theme.colors.border = crate::theme::border().into();
        theme.colors.input = crate::theme::border().into();
        theme.colors.popover = crate::theme::panel().into();
        theme.colors.popover_foreground = crate::theme::text().into();
        theme.colors.muted = crate::theme::elevated().into();
        theme.colors.muted_foreground = crate::theme::text_faint().into();
        theme.colors.accent = crate::theme::accent().into();
        theme.colors.accent_foreground = crate::theme::text_on_accent().into();
        theme.colors.primary = crate::theme::control().into();
        theme.colors.primary_foreground = crate::theme::text_on_control().into();
        theme.colors.primary_hover = crate::theme::text_muted().into();
        theme.colors.primary_active = crate::theme::text_muted().into();
        theme.colors.ring = crate::theme::text().into();
        theme.colors.caret = crate::theme::text().into();
        theme.colors.selection = crate::theme::control().opacity(0.24).into();
        theme.colors.scrollbar_thumb = crate::theme::border().into();
        theme.colors.scrollbar_thumb_hover = crate::theme::text_faint().into();
    });
}

pub fn rooted<T: Render + 'static>(
    view: Entity<T>,
    window: &mut Window,
    cx: &mut App,
) -> Entity<Root> {
    cx.new(|cx| Root::new(view, window, cx))
}
