use std::path::PathBuf;
use std::time::Duration;

use crate::mix::{MixPlan, NextTrack};

#[derive(Clone, Debug, PartialEq)]
pub enum PlayerCommand {
    /// 装载一首曲目。`autoplay` 为假时装载后停在开头。
    Load {
        path: PathBuf,
        autoplay: bool,
    },
    /// 预载下一首及可选混音计划。`after` 是调用方记录的当前曲目，不匹配即丢弃。
    SetNext {
        after: PathBuf,
        next: Option<NextTrack>,
    },
    /// 试听过渡编辑结果，从 `from_start` 起播 `from` 并按 `plan` 混入 `to`。
    PreviewMix {
        from: PathBuf,
        from_start: Duration,
        to: PathBuf,
        plan: MixPlan,
    },
    /// 通知后端自动混音总开关状态，独占后端据此决定是否启用预载混音链。
    SetAutoMix(bool),
    Play,
    Pause,
    TogglePlay,
    Stop,
    /// 跳转到指定位置。仅在用户松开进度条时下发一次。rodio 的 seek 会重建解码器，连续调用会卡顿并爆音。
    Seek(Duration),
    SetVolume(f32),
    /// 枚举输出设备。结果经 `PlayerEvent::Devices` 返回，因 cpal 不适合在 UI 线程上调用。
    EnumerateDevices,
    /// 切换输出设备，`None` 表示跟随系统默认。
    SetDevice {
        id: Option<String>,
    },
    /// 换设备时是否交叉淡化。设置项活在 UI 线程、后端活在音频线程，只能这样送过来。
    SetSmoothSwitch(bool),
    /// 切换输出模式，取共享或 WASAPI 独占。独占与共享在同一台设备上互斥，切换会重开输出流。
    SetOutputMode {
        exclusive: bool,
    },
    /// 让音频线程退出。由 `AudioHandle::drop` 发出。
    Shutdown,
}
