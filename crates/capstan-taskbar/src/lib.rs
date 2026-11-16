//! 任务栏缩略图与缩略图工具栏。仅 Windows 有实现，其余平台提供空实现。
//!
//! 窗口缩略图用 DWM 的标志性缩略图替换实时画面，名字与缩略图按钮经
//! `ITaskbarList3` 下发；按钮点击由窗口子类化转成 [`TaskbarAction`] 入队。

/// 缩略图工具栏按钮点击。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TaskbarAction {
    Prev,
    TogglePlay,
    Next,
    ToggleLike,
}

/// 一张 BGRA 位图，供任务栏缩略图绘制。宽高是像素数，不是显示单位。
#[derive(Clone)]
pub struct Art {
    pub width: u32,
    pub height: u32,
    pub bgra: Vec<u8>,
}

/// 缩略图按钮的 tooltip。文案由界面层给出，与无障碍名称同源。
#[derive(Clone, Copy)]
pub struct Labels {
    pub prev: &'static str,
    pub play: &'static str,
    pub pause: &'static str,
    pub next: &'static str,
    pub like: &'static str,
    pub unlike: &'static str,
}

#[cfg(windows)]
mod win;

#[cfg(not(windows))]
mod stub;

#[cfg(windows)]
pub use win::Taskbar;

#[cfg(not(windows))]
pub use stub::Taskbar;
