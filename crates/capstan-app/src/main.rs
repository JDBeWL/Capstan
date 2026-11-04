//! Capstan 入口。
// 防止 Windows 下的 panic 弹窗阻断日志收集，并在 release 下关闭命令行窗口。
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod keymap;

use std::path::PathBuf;

use capstan_ui::RootView;
use capstan_ui::platform::*;
use capstan_ui::settings::{self, Settings};

fn main() {
    application().run(|cx: &mut App| {
        // 日志必须先初始化。
        capstan_ui::logging::init();
        // 组件库必须在开窗之前初始化。
        capstan_ui::platform::init(cx);
        keymap::bind(cx);

        // 窗口几何要在这里定，`RootView::new` 读设置时窗口已经开好了。存档落在屏幕外时退回居中。
        let stored = Settings::load();
        let bounds = stored
            .window
            .and_then(|rect| rect.fit(&visible_areas(cx)))
            .map_or_else(
                || {
                    Bounds::centered(
                        None,
                        size(
                            px(settings::DEFAULT_WINDOW_W),
                            px(settings::DEFAULT_WINDOW_H),
                        ),
                        cx,
                    )
                },
                |rect| {
                    Bounds::new(
                        point(px(rect.x), px(rect.y)),
                        size(px(rect.width), px(rect.height)),
                    )
                },
            );

        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            // 底栏左右两栏各 240px、进度条两端时间标签各 52px。
            window_min_size: Some(size(px(settings::MIN_WINDOW_W), px(settings::MIN_WINDOW_H))),
            titlebar: Some(TitlebarOptions {
                // `appears_transparent: true` 关闭系统标题栏，仅 macOS 与 Windows 支持；拖拽与窗口控件仍由 GPUI 应答。
                // `title` 进任务栏 tooltip 与无障碍树的窗口名，留空会报出匿名窗口。
                title: Some(SharedString::from("Capstan")),
                appears_transparent: true,
                ..Default::default()
            }),
            ..Default::default()
        };

        // 开窗失败只记日志退出，不 panic，release 下没有控制台。
        if let Err(err) = cx.open_window(options, |window, cx| {
            let view = cx.new(|cx| RootView::new(window, cx));

            // 命令行给的路径直接导入，也是「用本应用打开」的落点。
            let initial: Vec<PathBuf> = std::env::args_os()
                .skip(1)
                .map(PathBuf::from)
                .filter(|path| path.exists())
                .collect();
            if !initial.is_empty() {
                view.update(cx, |view, cx| view.import(initial, cx));
            }

            // 窗口根必须包一层组件库的 Root，否则主题、浮层、焦点陷阱拿不到 Context。
            capstan_ui::platform::rooted(view, window, cx)
        }) {
            log::error!("打开主窗口失败：{err}");
        }
    });
}

/// 每块屏幕的可见区域（已扣掉任务栏），逻辑像素。存档里的窗口要落进其中一块才算可用。
fn visible_areas(cx: &App) -> Vec<(f32, f32, f32, f32)> {
    cx.displays()
        .iter()
        .map(|display| {
            let bounds = display.visible_bounds();
            (
                f32::from(bounds.origin.x),
                f32::from(bounds.origin.y),
                f32::from(bounds.size.width),
                f32::from(bounds.size.height),
            )
        })
        .collect()
}
