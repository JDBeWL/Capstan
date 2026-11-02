//! 跨列表拖拽的载荷：一首曲目。落点是队列行与侧栏歌单。
//! 歌单页内部的重排仍走 `track_list::drag` 的 `DragRow`，两者是不同载荷，互不干扰。

use capstan_core::TrackId;

use crate::icon::{Glyph, icon};
use crate::platform::*;
use crate::theme;

/// 被拖起来的一首曲目。
#[derive(Clone)]
pub struct DragTrack {
    pub id: TrackId,
    /// 跟随鼠标那张小卡片上的字。
    pub title: SharedString,
}

/// 跟着鼠标走的小卡片。gpui 的拖拽影像必须是一个视图实体，所以它只是个会画标题的壳。
pub struct DragGhost {
    title: SharedString,
}

impl DragGhost {
    pub fn new(title: SharedString) -> Self {
        Self { title }
    }
}

impl Render for DragGhost {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .items_center()
            .gap(px(theme::GAP_SM))
            .px(px(theme::GAP_MD))
            .py(px(theme::GAP_XS))
            .rounded(px(theme::RADIUS_MD))
            .bg(theme::panel())
            .border_1()
            .border_color(theme::border())
            .text_size(px(theme::FONT_TRACK))
            .text_color(theme::text())
            .child(icon(Glyph::Music, 12., theme::text_muted()))
            .child(self.title.clone())
    }
}
