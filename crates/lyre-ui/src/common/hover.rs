//! 悬停进度。鼠标进出一行或一个按钮时，逐帧返回 0→1 或 1→0。
//! 用 `window` 的 keyed state 存进度；悬停目标放在线程本地，切页时整窗重绘一次即可。

use std::cell::RefCell;
use std::time::Duration;

use crate::platform::*;
use crate::theme;

thread_local! {
    static ACTIVE: RefCell<Option<ElementId>> = const { RefCell::new(None) };
}

/// 把元素的 `on_hover` 接到全局悬停目标上。目标变了就整窗重绘，
/// 各元素在下一帧用 [`progress`] 重新采样。
pub fn bind(id: impl Into<ElementId>) -> impl Fn(&bool, &mut Window, &mut App) + 'static {
    let id = id.into();
    move |hovered, window, _cx| {
        let changed = ACTIVE.with(|active| {
            let mut active = active.borrow_mut();
            if *hovered {
                if active.as_ref() == Some(&id) {
                    false
                } else {
                    *active = Some(id.clone());
                    true
                }
            } else if active.as_ref() == Some(&id) {
                *active = None;
                true
            } else {
                false
            }
        });
        if changed {
            window.refresh();
        }
    }
}

fn is_active(id: &ElementId) -> bool {
    ACTIVE.with(|active| active.borrow().as_ref() == Some(id))
}

/// 按钮类，280ms mech。
pub fn progress(id: impl Into<ElementId>, window: &mut Window, cx: &mut App) -> f32 {
    sample(
        id.into(),
        theme::MOTION_HOVER_MS,
        theme::ease_mech(),
        window,
        cx,
    )
}

/// 列表行，250ms snap。
pub fn row_progress(id: impl Into<ElementId>, window: &mut Window, cx: &mut App) -> f32 {
    sample(
        id.into(),
        theme::MOTION_HOVER_ROW_MS,
        theme::ease_snap(),
        window,
        cx,
    )
}

fn sample(
    id: ElementId,
    duration_ms: u64,
    easing: impl Fn(f32) -> f32 + 'static,
    window: &mut Window,
    cx: &mut App,
) -> f32 {
    let target = if is_active(&id) { 1.0 } else { 0.0 };
    motion::transition(
        (id, SharedString::from("hover")),
        target,
        motion::Transition::new(Duration::from_millis(duration_ms)).ease(easing),
        window,
        cx,
    )
}
