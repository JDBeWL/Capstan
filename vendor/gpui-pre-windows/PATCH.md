# gpui-pre-windows 本地补丁

本目录是 `gpui-pre-windows 0.3.10` 的 vendored 副本，上游为 `zed@08cbb88` 快照，由根
`Cargo.toml` 的 `[patch.crates-io]` 指向此处。

共三处改动。

1. **四边四角缩放热区**，见 `src/events.rs::handle_hit_test_msg`。上游对无标题栏窗口只注册
   顶边的缩放热区，左、右、下三边没有原生缩放能力；应用侧兜底只能走 `Window::resize`，
   而它是 `SetWindowPos(.., SWP_NOMOVE)`，原点固定，拖左边时对边移动，表现为反向拉伸。
   改法是把顶边判定扩为四条边与四个角，命中时只返回 `HT*` 码，位置、尺寸、光标形状与 DPI
   全部交给 Windows 处理。角的热区比边放宽 `CORNER_GRAB_SCALE`（2）倍，否则对角仅剩一个
   8×8 方块，偏移数像素即退化为单向边缩放。

2. **窗口移动的合并上报**，见 `src/events.rs::handle_move_msg` 与 `report_move_settled`。
   上游每收到一条 `WM_MOVE` 就调用一次 `moved` 回调，该回调在 gpui 侧经
   `Window::bounds_changed` → `refresh()` 把整个窗口标脏，下一帧整棵树重排重画。
   拖动窗口时 `WM_MOVE` 随鼠标逐像素到达，约 113 次/秒，而窗口位置与布局无关，
   一次拖动即产生上百帧逐像素相同的重画，单核占用由空载 1.6% 升至 16~21%。
   改法是 `WM_MOVE` 只置 `move_settle_pending` 并重置一个 120 ms 单次定时器，
   定时器到点或 `WM_EXITSIZEMOVE` 时上报一次；改后拖动期间为 2.3~2.7%，与空载持平。
   状态字段 `WindowsWindowState::move_settle_pending` 位于 `src/window.rs`。

3. **拖边缩放的合并与限帧**，见 `src/events.rs::handle_size_change`、`apply_pending_size`
   与 `draw_window`。上游对每条 `WM_SIZE` 都做两件即时的事，即 `ResizeBuffers` 重建
   swap chain，再让 `refresh()` 把整窗标脏。拖动边框时 `WM_SIZE` 为 90~110 条/秒，
   而每条 `WM_SIZE` 与每次 vsync 各引来一次 `WM_PAINT`，即 180 次绘制/秒、每次 2.5~6 ms，
   单核占用 77%。改法三条：
   - 渲染器尺寸存入 `pending_renderer_size`，由 `apply_pending_size` 在绘制前交给
     `DirectXRenderer::resize`，一次绘制最多重建一次 swap chain；
   - 记录 `last_size_change`，最后一次 `WM_SIZE` 之后 `RESIZE_BURST`（200 ms）内视为拖动中；
   - 该区间内，距 `last_draw` 不足 `RESIZE_FRAME_INTERVAL`（16 ms）的 `WM_PAINT` 直接
     `ValidateRect` 返回。跳过的帧不会丢内容，因为 vsync 线程每拍都会 `RDW_INVALIDATE`，
     下一拍必然再收到一次 `WM_PAINT`，见 `begin_vsync_thread`。
   改后曲库页由 77% 降至 20%，歌词页由 4.3% 降至 3.5%。

## 换 gpui 版本时的重做步骤

1. 从 `~/.cargo/registry/src/<index>/gpui-pre-windows-<ver>/` 重新复制 `src/`、`Cargo.toml`、
   `build.rs`、`LICENSE-*`，不要带 crate 自带的 `Cargo.lock` 与 `.cargo-ok`。
2. 在新版 `handle_hit_test_msg` 中重新套用第 1 处改动。
3. 在新版 `handle_move_msg`、`handle_timer_msg` 与 `handle_size_move_loop_exit` 中重新套用
   第 2 处改动，并把 `move_settle_pending` 字段补进 `WindowsWindowState`。
4. 在新版 `handle_size_change` 与 `draw_window` 中重新套用第 3 处改动，并补上
   `last_size_change`、`pending_renderer_size` 与 `last_draw` 三个字段。
5. 同步根 `Cargo.toml` 中 `gpui` 与 `gpui-platform` 的 `=x.y.z` 钉版本，以及本目录
   `Cargo.toml` 的 `version`。

## 许可

上游 Apache-2.0，见 `LICENSE-APACHE`。本地修补不改变许可。
