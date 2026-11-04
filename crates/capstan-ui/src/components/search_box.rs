//! 搜索框，全局只有顶栏这一份输入状态。
//! 根视图的裸字母快捷键要 `&& !Input` 避让 `Input` 的 key context，否则打字会触发播放快捷键。

use std::time::Duration;

use crate::entities::LibraryEntity;
use crate::platform::*;
use crate::theme;

/// 等待 120ms 再提交。低于 80ms 起不到防抖作用，高于 200ms 列表明显迟钝。
const DEBOUNCE: Duration = Duration::from_millis(120);

/// 查询词停这么久没有变化才记进历史。120ms 防抖决定提交时机，
/// 历史只记录用户真正停留过的查询，输入途中的中间态不留。
const SETTLE: Duration = Duration::from_millis(1500);

#[derive(Clone, Copy, Debug)]
pub enum SearchBoxEvent {
    /// 用户开始打字。主区可能停在任意一页，靠这个事件把用户带到能看见结果的那页。
    /// 不带载荷，查询词只存在 `LibraryEntity::query` 里。
    Typing,
    /// 框获得焦点。点它就该切到搜索页，否则点击没有反馈。
    Focused,
}

pub struct SearchBox {
    input: Entity<InputState>,
    library: Entity<LibraryEntity>,
    /// 每次提交递增；防抖任务只在仍是当前代时才提交，避免把中间态提交上去。
    generation: u64,
    /// 最后一次由这个框提交给曲库的文字。
    submitted: String,
    _sub: Subscription,
    _mirror: Subscription,
}

impl EventEmitter<SearchBoxEvent> for SearchBox {}

impl SearchBox {
    pub fn new(
        window: &mut Window,
        library: Entity<LibraryEntity>,
        cx: &mut Context<Self>,
    ) -> Self {
        let input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("搜索标题、艺术家或专辑")
                .clean_on_escape()
        });
        let sub = cx.subscribe_in(&input, window, on_input_event);
        let mirror = cx.observe_in(&library, window, |this, _, window, cx| {
            this.mirror_query(window, cx);
        });

        Self {
            input,
            library,
            generation: 0,
            submitted: String::new(),
            _sub: sub,
            _mirror: mirror,
        }
    }

    /// 别处清除查询词后输入框需要同步，否则会保留一个已不存在的过滤条件。
    /// 只镜像非本组件提交的值。防抖的 120ms 内用户可能继续输入，旧值覆盖新值会导致按键丢失。
    fn mirror_query(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let query = self.library.read(cx).query().to_string();
        if query == self.submitted {
            return;
        }
        // 进行中的防抖任务要作废，否则它会把刚镜像进去的文字当成旧值再提交一次。
        self.generation = self.generation.wrapping_add(1);
        query.clone_into(&mut self.submitted);
        // `set_value` 刻意不发 `InputEvent::Change`，所以这里不会被当成「用户在打字」。
        self.input.update(cx, |input, cx| {
            input.set_value(query.as_str(), window, cx);
        });
    }

    /// 聚焦搜索框。`/` 与 `Ctrl+K` 快捷键、以及点框都走这里。
    pub fn focus(&self, window: &mut Window, cx: &mut App) {
        self.input.read(cx).focus_handle(cx).focus(window, cx);
    }

    /// 清空输入框与查询。`Esc` 在输入框外按下时走这里。
    pub fn clear(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.input.update(cx, |input, cx| {
            input.set_value("", window, cx);
        });
        self.generation = self.generation.wrapping_add(1);
        self.submitted.clear();
        self.library.update(cx, LibraryEntity::clear_query);
        cx.notify();
    }

    fn schedule_submit(&mut self, cx: &mut Context<Self>) {
        self.generation = self.generation.wrapping_add(1);
        let generation = self.generation;
        let executor = cx.background_executor().clone();

        cx.spawn(async move |this, cx| {
            executor.timer(DEBOUNCE).await;
            this.update(cx, |this, cx| {
                if this.generation == generation {
                    let query = this.input.read(cx).value().to_string();
                    query.clone_into(&mut this.submitted);
                    this.library
                        .update(cx, |library, cx| library.set_query(query, cx));
                    this.schedule_history(cx);
                }
            })
            .ok();
        })
        .detach();
    }

    /// 停下来之后把这次的查询词记进历史。任何后续输入都会推进代次，使进行中的任务作废。
    fn schedule_history(&mut self, cx: &mut Context<Self>) {
        let generation = self.generation;
        let executor = cx.background_executor().clone();

        cx.spawn(async move |this, cx| {
            executor.timer(SETTLE).await;
            this.update(cx, |this, cx| {
                if this.generation != generation {
                    return;
                }
                let query = this.library.read(cx).query().to_string();
                this.library
                    .update(cx, |library, cx| library.record_search(&query, cx));
            })
            .ok();
        })
        .detach();
    }

    fn submit(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        // 作废旧的防抖代，否则它随后会把旧值再提交一次。
        self.generation = self.generation.wrapping_add(1);
        let query = self.input.read(cx).value().to_string();
        query.clone_into(&mut self.submitted);
        self.library
            .update(cx, |library, cx| library.set_query(query.clone(), cx));
        // 回车是明确的「就搜这个」，不必等停顿。
        self.library
            .update(cx, |library, cx| library.record_search(&query, cx));
        cx.notify();
    }
}

fn on_input_event(
    this: &mut SearchBox,
    _source: &Entity<InputState>,
    event: &InputEvent,
    window: &mut Window,
    cx: &mut Context<SearchBox>,
) {
    match event {
        InputEvent::Change => {
            // 顺序有意义。切页要立刻发生，查询提交走 120ms 防抖；反过来切页会晚 120ms。
            cx.emit(SearchBoxEvent::Typing);
            this.schedule_submit(cx);
        }
        InputEvent::PressEnter { .. } => {
            this.submit(window, cx);
        }
        InputEvent::Focus => {
            // 聚焦要同时重绘与切页，两个通知都发。
            cx.notify();
            cx.emit(SearchBoxEvent::Focused);
        }
        InputEvent::Blur => cx.notify(),
    }
}

impl Render for SearchBox {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let has_text = !self.input.read(cx).value().is_empty();
        let focused = self.input.read(cx).focus_handle(cx).is_focused(window);

        let border_color = if focused {
            theme::text()
        } else {
            theme::border()
        };

        // 左内边距必须恒定，否则打进第一个字符的瞬间整段文本右跳；需要让位的只有右端的清空按钮。
        let input_left = theme::GAP_SM;
        let input_right = if has_text {
            theme::GAP_XL
        } else {
            theme::GAP_SM
        };

        let mut root = div()
            .flex_none()
            .relative()
            // 跟着顶栏的中组收缩，`SEARCH_W` 只是上限，定宽会在窄窗口里把窗口控件挤出边界。
            .w_full()
            .max_w(px(theme::SEARCH_W))
            .h(px(theme::SEARCH_H))
            .flex()
            .items_center()
            .bg(theme::surface())
            .border_1()
            .border_color(border_color)
            .hover(|style| style.border_color(theme::text_faint()))
            // `Input` 只有 32px 高且左右各留 12px 内边距，点击这些空白不会聚焦；
            // 整条转发一次焦点，任意位置点击都聚焦搜索框；在 `Input` 内部时 `focus` 幂等。
            .cursor_text()
            .on_mouse_down(MouseButton::Left, {
                let input = self.input.clone();
                move |_, window, cx| {
                    input.update(cx, |input, cx| input.focus(window, cx));
                }
            })
            // `//` 前缀。字段语言的一部分，不额外占一个图标。
            .child(
                div()
                    .flex_none()
                    .pl(px(theme::GAP_MD))
                    .font(theme::field_font())
                    .text_size(px(theme::FONT_CAPTION))
                    .text_color(theme::text_faint())
                    .child("//"),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    // `Input` 是组件库写死的定高元素，在壳里要显式居中。
                    .flex()
                    .items_center()
                    .pl(px(input_left))
                    .pr(px(input_right))
                    .child(
                        Input::new(&self.input)
                            .appearance(false)
                            // 组件库的清空按钮出现时机与配色不受本项目控制。
                            .cleanable(false),
                    ),
            );

        if has_text {
            root = root.child(
                div()
                    .id("search-clear")
                    .absolute()
                    .right(px(theme::GAP_XS))
                    .top_0()
                    .bottom_0()
                    .flex()
                    .items_center()
                    .cursor_pointer()
                    .text_color(theme::text_faint())
                    .hover(|style| style.text_color(theme::text()))
                    .tooltip(crate::common::tooltip::builder("清空搜索"))
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                        this.clear(window, cx);
                    }))
                    // 图标继承父级文字色，hover 变色才能作用在它身上。
                    .child(crate::icon::icon_inherit(crate::icon::Glyph::Cross, 14.)),
            );
        }

        root
    }
}

#[cfg(test)]
mod tests {
    // 不写 `use super::*`，`platform::*` 里的 gpui `test` 过程宏会遮蔽内置 `#[test]`。

    /// 两套主题下，组件库 `Input` 的文字色对搜索框底色都须达到 AA 对比度。
    /// 文字色来自组件库全局态而非 `crate::theme`，两边的明暗必须一起切换。
    #[test]
    fn input_text_reads_against_the_search_pill_in_both_themes() {
        use crate::platform::{Rgba, rgb};
        use crate::theme::{self, Appearance};

        // theme.rs 里那份 WCAG 计算是 `#[cfg(test)]` 私有函数，跨不到这里。
        fn luminance(c: Rgba) -> f32 {
            fn ch(v: f32) -> f32 {
                if v <= 0.039_28 {
                    v / 12.92
                } else {
                    ((v + 0.055) / 1.055).powf(2.4)
                }
            }
            0.2126 * ch(c.r) + 0.7152 * ch(c.g) + 0.0722 * ch(c.b)
        }
        fn ratio(fg: Rgba, bg: Rgba) -> f32 {
            let (l1, l2) = (luminance(fg), luminance(bg));
            let (hi, lo) = if l1 > l2 { (l1, l2) } else { (l2, l1) };
            (hi + 0.05) / (lo + 0.05)
        }

        // 组件库亮色主题下 `Input` 的文字色是 `neutral-900`（`#0a0a0a`），深色下是近白。
        let input_text_light = rgb(0x0a0a0a);
        let input_text_dark = rgb(0xfafafa);
        let light = rgb(theme::palette_for(Appearance::Light).panel);
        let dark = rgb(theme::palette_for(Appearance::Dark).panel);

        assert!(
            ratio(input_text_light, light) >= 4.5,
            "组件库亮色下的输入文字对搜索框底色只有 {:.2}:1，低于 AA",
            ratio(input_text_light, light)
        );
        assert!(
            ratio(input_text_dark, dark) >= 4.5,
            "组件库深色下的输入文字对搜索框底色只有 {:.2}:1，低于 AA",
            ratio(input_text_dark, dark)
        );
        assert!(
            ratio(input_text_dark, light) < 4.5 && ratio(input_text_light, dark) < 4.5,
            "输入文字色与主题明暗对不上：亮色文字压在亮底、或深色文字压在深底上"
        );
    }
}
