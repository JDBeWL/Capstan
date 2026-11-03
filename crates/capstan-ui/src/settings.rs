//! 可持久化的界面偏好，落盘在用户数据目录的 `settings.json`。
//! 读不到退回默认值，写不进只记日志。不弹窗、不阻塞启动。

use capstan_core::SortKey;
use serde::{Deserialize, Serialize};

use crate::components::sidebar::Listing;
use crate::entities::RepeatState;
use crate::persist;
use crate::shell::{Density, NavSection, PanelTab, SidebarState};
use crate::theme::Appearance;

const FILE: &str = "settings.json";

/// 窗口尺寸的下限，与开窗时传的 `window_min_size` 同源。
pub const MIN_WINDOW_W: f32 = 560.0;
pub const MIN_WINDOW_H: f32 = 400.0;
/// 首次启动的窗口尺寸。
pub const DEFAULT_WINDOW_W: f32 = 1160.0;
pub const DEFAULT_WINDOW_H: f32 = 750.0;
/// 存档里的窗口至少要在某块屏幕上露出这么宽、这么高的一条，否则当作位置已失效。
const KEEP_VISIBLE: f32 = 120.0;

/// 上次退出时的窗口位置与尺寸，逻辑像素。原点是窗口左上角。
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct WindowRect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

impl WindowRect {
    /// 与某块屏幕的可见区域（已扣掉任务栏）交叠得够多时返回自身，否则返回 `None`。
    /// 显示器被拔掉、分辨率变小都会让存档落到屏幕外，此时调用方应当退回居中开窗。
    /// `areas` 是每块屏幕的 (x, y, width, height)。
    pub fn fit(self, areas: &[(f32, f32, f32, f32)]) -> Option<Self> {
        let width = self.width.max(MIN_WINDOW_W);
        let height = self.height.max(MIN_WINDOW_H);
        let reachable = areas.iter().any(|(ax, ay, aw, ah)| {
            let overlap_w = (self.x + width).min(ax + aw) - self.x.max(*ax);
            let overlap_h = (self.y + height).min(ay + ah) - self.y.max(*ay);
            overlap_w >= KEEP_VISIBLE && overlap_h >= KEEP_VISIBLE
        });
        reachable.then_some(Self {
            x: self.x,
            y: self.y,
            width,
            height,
        })
    }
}

/// 曲库列表的列宽（逻辑像素）。
/// 编号与「空白」固定；标题和专辑分摊剩余宽度，窗口伸缩时按最后的比例一起变。
/// 拖某一列的分隔线只改那一列与空白，专辑占比随后重新取样。
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TrackColumns {
    pub index: f32,
    /// 时长列右侧、窗口右缘之前的空白。拖窄某列时它会变宽，拖宽时先吃它。
    pub gap: f32,
    /// 专辑列宽；窗口伸缩时按当前占比跟着标题一起变。
    pub album: f32,
}

impl Default for TrackColumns {
    fn default() -> Self {
        Self {
            index: crate::theme::ROW_INDEX_W,
            // 默认不留空白：标题把能用的宽度全吃掉。
            gap: 0.0,
            album: crate::theme::COLUMN_ALBUM_W,
        }
    }
}

impl TrackColumns {
    /// 拖过头或存档被改坏时把宽度收回可用区间。索引列再窄会切掉三位数，
    /// 专辑列允许拖到很窄，空白不能为负。
    pub fn clamped(self) -> Self {
        Self {
            index: self.index.clamp(36.0, 88.0),
            gap: self.gap.clamp(0.0, 4000.0),
            album: self.album.clamp(64.0, 480.0),
        }
    }
}

/// 可持久化的界面偏好。进度、光标、滚动位置这类一过性状态不在这里。
/// 音量、播放模式与窗口几何是例外：它们描述「用户上次要的样子」，而不是「这一次的位置」。
// `#[serde(default)]`。缺字段用默认值补齐，否则新增设置项会让老配置文件整体失效。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// 浅色与深色。旧存档没有这个字段时按默认的浅色补齐。
    pub appearance: Appearance,
    pub density: Density,
    pub default_view: NavSection,
    pub recursive_import: bool,
    /// 启动时右面板停在哪一页，`None` 表示收起。窄一档一律收起，断点优先于偏好。
    pub default_panel: Option<PanelTab>,
    /// 启动时侧栏的形态，`None` 表示跟随窗口宽度。拖窗口仍会自动调整。
    pub default_sidebar: Option<SidebarState>,
    /// 记住上次播放的位置，下次启动装回同一首的同一秒（落盘见 [`crate::resume`]）。
    pub resume_position: bool,
    /// 用户要的音量，取值 0..=1。静音时这里存的仍是静音前的值，见 [`Settings::muted`]。
    pub volume: f32,
    /// 退出时是否处于静音。
    pub muted: bool,
    /// 输出设备的稳定标识（见 `capstan_audio::AudioDeviceInfo::id`）；`None` 表示跟随系统默认。
    /// 标识是机器绑定的，换机器或重装驱动后可能失效，那时静默回退默认设备。
    pub preferred_device: Option<String>,
    /// 换设备时让旧输出流淡出、新流淡入，把重建链路的空窗盖掉。
    /// 默认关。两路短暂并存时可能有轻微的相位抵消，听感不合就还是硬切干净。
    pub smooth_device_switch: bool,
    /// 用 WASAPI 独占输出。默认关。独占会占住设备，其它程序一起听不到声音。
    pub exclusive_output: bool,
    /// 曲库列表 NO./专辑两列的宽度与列尾空白。标题列吃剩余宽度，不单独存。
    pub track_columns: TrackColumns,
    /// 「你的音乐库」卡片上次手动点选的粒度（歌单 / 艺术家 / 专辑）。`None` 表示跟随主区区段。
    pub library_listing: Option<Listing>,
    /// 随机播放轴。与 `repeat` 合起来才是用户看到的播放模式。
    pub shuffle: bool,
    /// 循环轴：关 / 列表循环 / 单曲循环。
    pub repeat: RepeatState,
    /// 曲库列表的排序键。曲目顺序本身随 `library.json` 落盘，这里记住的是用户选的是哪一档。
    pub sort_key: SortKey,
    /// 自动切歌时列表是否把当前曲滚进视野（曲库工具条的「跟随播放」）。
    pub follow_playback: bool,
    /// 上次退出时的窗口位置与尺寸。`None` 表示首次启动，按默认尺寸居中。
    pub window: Option<WindowRect>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            appearance: Appearance::Light,
            density: Density::Compact,
            default_view: NavSection::Tracks,
            recursive_import: true,
            // 默认值必须等于「加这几个字段之前的行为」，否则老用户的 settings.json 一升级就变了样。
            default_panel: Some(PanelTab::NowPlaying),
            default_sidebar: None,
            resume_position: true,
            volume: 1.0,
            muted: false,
            preferred_device: None,
            smooth_device_switch: false,
            exclusive_output: false,
            track_columns: TrackColumns::default(),
            library_listing: None,
            shuffle: false,
            repeat: RepeatState::Off,
            sort_key: SortKey::ImportOrder,
            follow_playback: false,
            window: None,
        }
    }
}

impl Settings {
    /// 读不到（首次启动、文件损坏、目录不可读）或字段缺失时退回默认值。
    pub fn load() -> Self {
        let mut settings: Self = persist::load_or_default(FILE);
        settings.default_view = settings.default_view.as_default_view();
        settings
    }

    pub fn save(&self) {
        persist::save_lossy(FILE, self.clone());
    }
}

#[cfg(test)]
mod tests {
    use super::{Settings, TrackColumns, WindowRect};
    use crate::entities::RepeatState;
    use crate::shell::{Density, NavSection, PanelTab, SidebarState};
    use crate::theme::Appearance;
    use capstan_core::SortKey;

    #[test]
    fn defaults_are_the_conservative_choice() {
        let settings = Settings::default();
        assert_eq!(settings.appearance, Appearance::Light, "默认浅色");
        assert_eq!(settings.density, Density::Compact);
        assert_eq!(settings.default_view, NavSection::Tracks);
        assert!(
            settings.recursive_import,
            "递归导入是默认，因为音乐目录几乎总是分层的"
        );
        assert_eq!(settings.default_panel, Some(PanelTab::NowPlaying));
        assert_eq!(settings.default_sidebar, None, "侧栏默认跟随窗口宽度");
        assert!(settings.resume_position, "接着上次播是大多数人想要的");
        assert_eq!(
            settings.volume, 1.0,
            "默认满音量，与加这个字段之前的行为一致"
        );
        assert!(!settings.muted);
        assert_eq!(settings.preferred_device, None, "默认跟随系统默认设备");
        assert!(
            !settings.smooth_device_switch,
            "默认硬切，与加这个字段之前的行为一致"
        );
        assert!(
            !settings.exclusive_output,
            "默认共享输出，因为独占会占住设备，不能默认打开"
        );
        assert_eq!(settings.track_columns, TrackColumns::default());
        assert_eq!(settings.track_columns.index, crate::theme::ROW_INDEX_W);
        assert_eq!(settings.track_columns.gap, 0.0, "默认不留空白，标题吃满");
        assert_eq!(settings.track_columns.album, crate::theme::COLUMN_ALBUM_W);
        assert_eq!(
            settings.library_listing, None,
            "默认跟随主区区段，与加这个字段之前的行为一致"
        );
        assert!(!settings.shuffle, "默认不随机");
        assert_eq!(settings.repeat, RepeatState::Off, "默认不循环");
        assert_eq!(settings.sort_key, SortKey::ImportOrder, "默认按导入顺序");
        assert!(!settings.follow_playback, "默认不跟随播放滚动");
        assert_eq!(settings.window, None, "首次启动按默认尺寸居中");
    }

    #[test]
    fn column_widths_are_pulled_back_into_range() {
        let wild = TrackColumns {
            index: 2.0,
            gap: -10.0,
            album: 900.0,
        }
        .clamped();
        assert_eq!(wild.index, 36.0);
        assert_eq!(wild.gap, 0.0);
        assert_eq!(wild.album, 480.0);

        let tiny = TrackColumns {
            album: 10.0,
            ..TrackColumns::default()
        }
        .clamped();
        assert_eq!(tiny.album, 64.0, "专辑列要能拖到很窄");

        // 合法值原样保留，钳制不该顺手改动手动调好的宽度。
        let fine = TrackColumns {
            index: 48.0,
            gap: 120.0,
            album: 300.0,
        };
        assert_eq!(fine.clamped(), fine);
    }

    #[test]
    fn round_trips_through_json() {
        let settings = Settings {
            appearance: Appearance::Dark,
            density: Density::Comfortable,
            default_view: NavSection::Albums,
            recursive_import: false,
            default_panel: Some(PanelTab::Queue),
            default_sidebar: Some(SidebarState::Icons),
            resume_position: false,
            volume: 0.35,
            muted: true,
            preferred_device: Some("wasapi:{0.0.0.00000000}.{abc}".to_owned()),
            smooth_device_switch: true,
            exclusive_output: true,
            track_columns: TrackColumns {
                index: 48.0,
                gap: 120.0,
                album: 300.0,
            },
            library_listing: Some(crate::components::sidebar::Listing::Collections),
            shuffle: true,
            repeat: RepeatState::One,
            sort_key: SortKey::Title,
            follow_playback: true,
            window: Some(WindowRect {
                x: -120.0,
                y: 48.0,
                width: 1280.0,
                height: 860.0,
            }),
        };
        let text = serde_json::to_string(&settings).expect("序列化不该失败");
        let back: Settings = serde_json::from_str(&text).expect("反序列化不该失败");
        assert_eq!(settings, back);
    }

    #[test]
    fn an_old_file_still_parses_and_behaves_as_before() {
        // 缺的要补成升级前的行为，也就是整个结构体的默认值，而不是 `None` / `false`。
        let old = r#"{"density":"Compact","default_view":"Tracks","recursive_import":true}"#;
        let back: Settings = serde_json::from_str(old).expect("老文件不该因为多出来的字段读不回来");
        assert_eq!(back, Settings::default());
    }

    #[test]
    fn missing_fields_fall_back_to_defaults() {
        let back: Settings = serde_json::from_str(r#"{"density":"Compact"}"#)
            .expect("缺字段时应当用默认值补齐而不是报错");
        assert_eq!(back.default_view, NavSection::Tracks);
        assert!(back.recursive_import);
    }

    #[test]
    fn corrupt_file_yields_defaults() {
        let back: Settings = serde_json::from_str("这不是 JSON").unwrap_or_default();
        assert_eq!(back, Settings::default());
    }

    /// 显示器被拔掉后，存档里的窗口会落在屏幕外。这种位置必须被拒掉，否则下次开窗看不见。
    #[test]
    fn a_window_off_every_screen_is_rejected() {
        let primary = [(0.0, 0.0, 1920.0, 1040.0)];

        let inside = WindowRect {
            x: 100.0,
            y: 80.0,
            width: 1200.0,
            height: 800.0,
        };
        assert_eq!(inside.fit(&primary), Some(inside));

        // 副屏被拔掉：坐标落在不存在的区域里。
        let gone = WindowRect {
            x: 3000.0,
            y: 200.0,
            width: 1200.0,
            height: 800.0,
        };
        assert_eq!(gone.fit(&primary), None);

        // 只露出很窄的一条边，拖不回来。
        let sliver = WindowRect {
            x: -1180.0,
            y: 80.0,
            width: 1200.0,
            height: 800.0,
        };
        assert_eq!(sliver.fit(&primary), None);

        // 一块屏幕都没有（远程会话）时一律回退默认。
        assert_eq!(inside.fit(&[]), None);
    }

    /// 存档里的尺寸可能来自更小的屏幕，开窗时至少要撑到窗口下限。
    #[test]
    fn a_tiny_saved_window_grows_to_the_minimum() {
        let rect = WindowRect {
            x: 10.0,
            y: 10.0,
            width: 200.0,
            height: 120.0,
        };
        let fitted = rect.fit(&[(0.0, 0.0, 1920.0, 1040.0)]).expect("位置有效");
        assert_eq!(fitted.width, super::MIN_WINDOW_W);
        assert_eq!(fitted.height, super::MIN_WINDOW_H);
    }
}
