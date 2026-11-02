# Capstan

基于 [GPUI](https://gpui.rs) 的 Endfield-like flavor 的桌面音乐播放器。

![License](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)

## 功能

- **播放**：播放 / 暂停、上下曲、拖动跳转与悬停预览落点、音量与静音。随机与循环为两条独立轴，组合出四种行为。播放失败与输出降级均有提示，不会只出现在收起的面板中
- **队列**：来源于当前浏览的列表 —— 在歌单 / 专辑 / 艺术家中点歌，队列即该列表。面板显示实际播放顺序，点击任意曲目可直接跳播，也可将曲目拖拽到队列任意位置。执行下一首播放 / 加入队列 / 从队列移除 / 拖拽等手动编辑后，队列固化为显式列表，直到重新点歌
- **曲库**：原生文件对话框、拖放、命令行路径三种导入方式。导入过的文件夹记为曲库根，其中新增曲目在每次启动时自动补入，也可在设置中手动重扫。同一路径重复导入只刷新标签，不重复入库。收藏、关注与自建歌单支持拖拽排序、指定封面，按加入时间倒序排列，曲目可直接拖入
- **浏览**：虚拟化曲目列表与专辑 / 艺术家网格，网格支持点击下钻和面包屑返回。多艺术家按 `/` `,` 拆分为独立条目；搜索多词以 AND 收窄，支持中文输入法。列表可启用「跟随播放」，自动切歌时将当前曲目滚动到可视区域
- **歌词**：`.lrc` 支持 GBK / GB18030 编码，另有 `.ass` / `.ssa`；双语按「同一时刻两行」排布，ASS 的 `{\kf}` 驱动逐字卡拉OK。`L` 展开全屏遮罩
- **续播**：退出时记录曲目与播放秒数，下次启动恢复到同一位置，不自动开播
- **可撤销**：删除与清空在 6 秒内可撤销
- 三档响应式断点，侧栏 / 主区 / 右面板各自折叠。26 条快捷键可在设置页查看

## 技术栈

- **语言**：Rust edition 2024，MSRV 1.99.1
- **UI**：`gpui-pre` 0.3.10，Zed main 的快照分叉，配 `gpui-component` 0.8.1 提供输入框 / 菜单 / 浮层
- **音频输出**：`rodio` + `cpal`
- **解码**：`symphonia` 0.6 全格式
- **标签**：`lofty`；`.lrc` 的编码嗅探用 `encoding_rs`
- **数据**：`serde` / `serde_json` 落盘，`walkdir` 扫目录

## 技术路线

- **分层**：`capstan-core` 是纯领域逻辑，不依赖 UI 与音频，可脱离窗口和声卡单测。音频对象位于 `capstan-audio`，视图与状态位于 `capstan-ui`，`capstan-app` 只保留入口与键位绑定。
- **音频独占线程**：cpal 的 `Stream` 不实现 `Send`，音频对象只能存在于专属线程的函数体内。UI 通过 `PlayerCommand` / `PlayerEvent` 两条单向通道与原子 `Snapshot` 通信，进度条每秒读取数十次不加锁。
- **解码链**：`SymphoniaDecoder → LockFreeSource → UniformSourceIterator → FadeIn → rodio`。有界队列将解码与文件 IO 隔离在 cpal 回调线程之外；显式重采样；seek 重建音源时以淡入消除爆音。
- **渲染按变化频率分层**：gpui 的脏标记会传播到所有祖先，因此进度条、音量等高频组件自行订阅 `PlaybackEntity`，仅随换曲变化的部分订阅语义事件，避免跟随 10Hz 重绘。
- **布局不写死列数**：断点只决定「哪块区域在不在」，每行列数由可用宽度反推，列数与卡宽同源计算。
- **gpui 单一引入点**：全项目只有 `crates/capstan-ui/src/platform.rs` 可出现 `gpui` 名称，其余文件一律 `use crate::platform::*;`。CI 用一条 grep 断言维护该隔离，切换分叉只需改动这一处。
- **无边框窗口缩放**：上游 `gpui-pre-windows` 在隐藏标题栏后只注册了顶边热区，本仓库 vendor 并补丁其 `WM_NCHITTEST`，见 `vendor/gpui-pre-windows/PATCH.md`。
- **持久化**：8 份 JSON 存放于用户数据目录，先写 `.tmp` 再 `rename`；读取失败退回默认值，写入失败仅记录日志，数据目录损坏不会阻止播放器启动。

## 组件

| crate | 功能 | 规模 |
|---|---|---|
| `capstan-core` | 曲目与标签模型、播放队列语义、分组、歌词解析、文件名推测、目录扫描 | ~6.5k 行 |
| `capstan-audio` | 解码链、播放线程、后端抽象、命令 / 事件通道、进度快照 | ~7.1k 行 |
| `capstan-audio-simd` | 采样转 PCM 的 AVX2 / SSE2 内核，标量兜底 | ~550 行 |
| `capstan-audio-exclusive` | WASAPI 独占输出、格式协商、采样环与重采样 | ~3.8k 行 |
| `capstan-ui` | GPUI 视图与组件、播放 / 曲库实体、主题令牌、持久化 | ~28k 行 |
| `capstan-app` | 二进制入口、全局快捷键绑定 | ~150 行 |

## 快速上手开发

需要 Rust 1.99.1 以上。首次构建需编译含 832 个包的完整 GPUI 依赖树，耗时数分钟，此后增量构建较快。

```bash
cargo run -p capstan-app                # 启动
cargo run -p capstan-app -- <path-to-file>    # 启动并导入这个文件 / 目录
```

Rust 无代码级热更新，修改 `.rs` 后需重新编译并重启；Windows 下运行中的 exe 会被锁定。

```bash
cargo install cargo-watch --locked
cargo watch -x "run -p capstan-app"
```

提交前执行 CI 的三项检查，别名定义在 `.cargo/config.toml`：

```bash
cargo f    # fmt --all -- --check
cargo c    # clippy --workspace --all-targets -- -D warnings
cargo t    # test
```

其他常用命令：

```bash
cargo doc --workspace --no-deps                    # CI 不执行，intra-doc 链接失效仅在此处报告
CAPSTAN_DEBUG_IMPORT="<path-to-file>" cargo run -p capstan-app   # 指定另一批测试素材
```

用户数据位于 `%APPDATA%\Capstan\`，macOS 为 `~/Library/Application Support/Capstan`，Linux 为 `$XDG_DATA_HOME/Capstan`：
`library.json` `settings.json` `likes.json` `follows.json` `collections.json` `recents.json` `resume.json` `queue.json`，
外加 `covers/` 与 `logs/app.log`。如需重建数据目录，删除该目录即可。
旧版本用的是 `Lyre` 这一层名字，首次启动会把整个目录接管过来（同卷直接 rename，跨卷退回复制），
曲库索引、收藏与设置都不会丢；确认新版本正常后，旧的 `Lyre` 目录可以手动删除。

## 许可

MIT OR Apache-2.0 双许可，全文见 [LICENSE-MIT](LICENSE-MIT) 与 [LICENSE-APACHE](LICENSE-APACHE)。
Copyright 2026 JDBeWL。

第三方组件保留各自许可：`gpui-pre` / `gpui-component` / `cpal` 为 Apache-2.0，`rodio` / `lofty` /
`crossbeam` / `serde` 为 MIT OR Apache-2.0，`symphonia` 为 MPL-2.0，`walkdir` 为 Unlicense OR MIT，
内嵌图标数据派生自 Lucide v0.544.0，采用 ISC 许可；界面字体 Saira 为 SIL OFL 1.1，许可全文见
`crates/capstan-ui/assets/fonts/Saira-OFL.txt`。
