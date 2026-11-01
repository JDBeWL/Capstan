use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

/// 曲目的稳定标识：对路径求 FNV-1a。不用 `DefaultHasher` —— 跨 Rust 版本不保证一致，落盘会换 id。
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug, Serialize, Deserialize)]
pub struct TrackId(pub u64);

impl TrackId {
    pub fn from_path(path: &Path) -> Self {
        // FNV-1a 64 位
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        for byte in path.to_string_lossy().as_bytes() {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        Self(hash)
    }
}

impl std::fmt::Display for TrackId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:016x}", self.0)
    }
}

/// 解码前的音频规格，来自容器 / 流头而不是标签。
/// 三项全可选：读不到就留 `None`，不拿 0 或 44.1kHz 占位。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AudioInfo {
    /// 采样率，单位 Hz（44100 / 48000 / 96000…）。
    pub sample_rate: Option<u32>,
    /// 每样本位数。有损编码没有这个概念，恒为 `None`。
    pub bit_depth: Option<u8>,
    /// 总码率，单位 kbps。
    pub bitrate: Option<u32>,
}

/// 底栏音质徽章的档位。判据见 [`Track::quality`]。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Quality {
    /// 有损编码：不亮徽章
    Lossy,
    /// 无损，但采样规格没超过 CD
    Lossless,
    /// 无损且高于 CD
    HiRes,
}

/// 无损编码集合。判据是容器 / 编码而不是采样规格：
/// 16bit/44.1kHz 的 FLAC 仍是无损，24bit 的 mp3 仍是有损。
const LOSSLESS_FORMATS: [&str; 5] = ["flac", "wav", "ape", "alac", "aiff"];

/// 高于 CD 的门槛：≥ 24bit 且 ≥ 96kHz，两个条件都要 —— 只抬采样率而仍是 16bit，量化噪声没变。
const HIRES_BITS: u8 = 24;
const HIRES_RATE: u32 = 96_000;

/// 一个字段的值是从哪来的。落盘，用来保护手改、也用来在界面上说明「这是推断的，不是标签里写着的」。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum FieldSource {
    /// 音频标签里有。
    #[default]
    Tag,
    /// 目录名给的（父目录当专辑、祖父目录当艺术家）。
    Folder,
    /// 文件名推断的。
    Filename,
    /// 用户手动改的。任何自动推断都不许覆盖它。
    Manual,
}

/// 三个文本字段各自的来源。老 `library.json` 里没有这一段，缺省按 [`FieldSource::Tag`] 读 ——
/// 那些条目的标题确实来自标签，只有读标签失败的那几条会被说成来自标签，无伤大雅。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FieldSources {
    pub title: FieldSource,
    pub artist: FieldSource,
    pub album: FieldSource,
}

/// 文件的时间戳与大小，用来判断「重扫时这一首还要不要重读标签」。
///
/// 两项都可能是 `None`（读不到、或旧版本落盘的条目里根本没有这一段）。
/// 任一边拿不准就一律当「变了」—— 多读一次标签只是慢，漏读会让旧标签永远留在库里。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileStamp {
    /// 修改时间，Unix 毫秒。
    pub modified: Option<u64>,
    /// 字节数。
    pub size: Option<u64>,
}

impl FileStamp {
    /// 读一次文件的元数据。失败就给全 `None`，不影响入库。
    pub fn of(path: &Path) -> Self {
        let Ok(meta) = std::fs::metadata(path) else {
            return Self::default();
        };
        Self {
            modified: meta
                .modified()
                .ok()
                .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|since| since.as_millis() as u64),
            size: Some(meta.len()),
        }
    }

    /// 是不是同一份文件。自己这一边没读到时间戳时恒为 `false`。
    ///
    /// 只看修改时间不够：某些文件系统的时间精度是两秒，同一秒内覆盖的文件会漏判；
    /// 加上大小能把这一类挡住大半。
    pub fn matches(&self, other: &Self) -> bool {
        self.modified.is_some() && self.modified == other.modified && self.size == other.size
    }
}

/// 一首曲目的元信息。封面字节不在里面，只记 `has_cover`，按需走 [`crate::metadata::read_cover`]。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Track {
    pub id: TrackId,
    pub path: PathBuf,
    pub title: String,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub duration: Option<Duration>,
    pub has_cover: bool,
    #[serde(default)]
    pub audio: AudioInfo,
    #[serde(default)]
    pub sources: FieldSources,
    /// 读这一首时的文件指纹。重扫拿它和磁盘上的现值比，一样就跳过整段标签解析。
    #[serde(default)]
    pub stamp: FileStamp,
}

impl Track {
    /// 什么都读不到时的兜底：文件名当标题，其余留空 —— 不因一个坏 ID3 标签丢掉整个文件。
    /// 这一版不做任何推断（它就是「什么都不知道」的构造器），带目录上下文的推断走
    /// [`crate::metadata::read_track`]。
    pub fn fallback(path: &Path) -> Self {
        let title = path.file_stem().map_or_else(
            || path.to_string_lossy().into_owned(),
            |s| s.to_string_lossy().into_owned(),
        );

        Self {
            id: TrackId::from_path(path),
            path: path.to_path_buf(),
            title,
            artist: None,
            album: None,
            duration: None,
            has_cover: false,
            audio: AudioInfo::default(),
            sources: FieldSources {
                title: FieldSource::Filename,
                artist: FieldSource::Filename,
                album: FieldSource::Filename,
            },
            stamp: FileStamp::of(path),
        }
    }

    /// 把上一版里手改过的字段搬回来。
    ///
    /// 重读标签会用磁盘上的值原地替换条目，但用户手改过的那几项必须留着 ——
    /// 重扫是自动的、重读全部标签是用户主动点的，哪一种都不该把人的修正抹掉。
    /// 目前还没有编辑标签的界面，这条护栏是给那一天先留好的座位。
    pub fn keep_manual(&mut self, previous: &Self) {
        if previous.sources.title == FieldSource::Manual {
            self.title.clone_from(&previous.title);
            self.sources.title = FieldSource::Manual;
        }
        if previous.sources.artist == FieldSource::Manual {
            self.artist.clone_from(&previous.artist);
            self.sources.artist = FieldSource::Manual;
        }
        if previous.sources.album == FieldSource::Manual {
            self.album.clone_from(&previous.album);
            self.sources.album = FieldSource::Manual;
        }
    }

    pub fn is_unknown_title(&self) -> bool {
        self.title.is_empty()
    }

    /// 列表副标题：优先「艺术家 · 专辑」，缺失时退化到父目录名。
    pub fn subtitle(&self) -> String {
        match (self.artist.as_deref(), self.album.as_deref()) {
            (Some(a), Some(b)) => format!("{a} · {b}"),
            (Some(a), None) => a.to_string(),
            (None, Some(b)) => b.to_string(),
            (None, None) => self
                .path
                .parent()
                .and_then(|p| p.file_name())
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default(),
        }
    }

    /// 小写扩展名，用于格式小标签；无扩展名返回 `None`。
    pub fn format(&self) -> Option<String> {
        let ext = self.path.extension()?.to_str()?;
        if ext.is_empty() {
            return None;
        }
        Some(ext.to_ascii_lowercase())
    }

    /// 是否无损编码。判据是容器而不是采样规格，见 `LOSSLESS_FORMATS`。
    pub fn is_lossless(&self) -> bool {
        self.format()
            .is_some_and(|f| LOSSLESS_FORMATS.contains(&f.as_str()))
    }

    /// 当前引擎能不能把这一首放出声音。判据是扩展名对应的编码有没有解码实现，
    /// 见 [`crate::scanner::UNSUPPORTED_EXTENSIONS`]。与 [`Self::is_lossless`] 是两件事：
    /// WMA 有损且引擎解不了，APE 无损且能播。
    pub fn is_playable(&self) -> bool {
        self.format()
            .is_none_or(|ext| crate::scanner::is_decodable_extension(&ext))
    }

    /// 播不了的原因，一句话直接给用户看；能播时是 `None`。列表提示与底部通知共用这一句。
    pub fn unplayable_reason(&self) -> Option<String> {
        if self.is_playable() {
            return None;
        }
        Some(match self.format() {
            Some(ext) => format!("《{}》是 {} 格式，当前引擎不支持播放", self.title, ext),
            None => format!("《{}》的格式当前引擎不支持播放", self.title),
        })
    }

    /// 底栏徽章该写哪一档。读不到位深 / 采样率时最多只报 [`Quality::Lossless`]，不许猜 Hi-Res。
    pub fn quality(&self) -> Quality {
        if !self.is_lossless() {
            return Quality::Lossy;
        }
        match (self.audio.bit_depth, self.audio.sample_rate) {
            (Some(bits), Some(rate)) if bits >= HIRES_BITS && rate >= HIRES_RATE => Quality::HiRes,
            _ => Quality::Lossless,
        }
    }
}

impl AudioInfo {
    /// 三项里有没有任何一项 —— 决定徽章点开有没有东西可看。
    pub fn is_empty(&self) -> bool {
        self.sample_rate.is_none() && self.bit_depth.is_none() && self.bitrate.is_none()
    }

    /// 详情浮层里的「标签 / 值」行，只收有值的项。
    pub fn details(&self) -> Vec<(&'static str, String)> {
        let mut rows = Vec::new();
        if let Some(rate) = self.sample_rate {
            rows.push(("采样率", format!("{} kHz", kilo(rate))));
        }
        if let Some(bits) = self.bit_depth {
            rows.push(("位深", format!("{bits}-bit")));
        }
        if let Some(bitrate) = self.bitrate {
            rows.push(("码率", format!("{bitrate} kbps")));
        }
        rows
    }
}

/// Hz → kHz 的显示值：44100 要读成 44.1 而不是 44.10，96000 要读成 96 而不是 96.00。
fn kilo(hz: u32) -> String {
    format!("{:.2}", hz as f32 / 1000.0)
        .trim_end_matches('0')
        .trim_end_matches('.')
        .to_owned()
}

/// 把时长格式化成 `m:ss` 或 `h:mm:ss`。
pub fn format_duration(d: Duration) -> String {
    let total = d.as_secs();
    let (h, m, s) = (total / 3600, (total % 3600) / 60, total % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn track_id_is_stable_for_same_path() {
        let p = Path::new("/music/夜航西飞.flac");
        assert_eq!(TrackId::from_path(p), TrackId::from_path(p));
    }

    #[test]
    fn track_id_differs_across_paths() {
        assert_ne!(
            TrackId::from_path(Path::new("/music/a.flac")),
            TrackId::from_path(Path::new("/music/b.flac"))
        );
    }

    #[test]
    fn fallback_uses_file_stem() {
        let t = Track::fallback(Path::new("/music/纸飞机与旧磁带.mp3"));
        assert_eq!(t.title, "纸飞机与旧磁带");
        assert!(!t.has_cover);
    }

    #[test]
    fn subtitle_degrades_gracefully() {
        let mut t = Track::fallback(Path::new("/music/album/x.mp3"));
        assert_eq!(t.subtitle(), "album");

        t.artist = Some("陈屿".into());
        assert_eq!(t.subtitle(), "陈屿");

        t.album = Some("城市漫游".into());
        assert_eq!(t.subtitle(), "陈屿 · 城市漫游");
    }

    #[test]
    fn duration_formatting() {
        assert_eq!(format_duration(Duration::from_secs(227)), "3:47");
        assert_eq!(format_duration(Duration::from_secs(3725)), "1:02:05");
    }

    #[test]
    fn format_is_lowercased_extension() {
        assert_eq!(
            Track::fallback(Path::new("/music/x.MP3"))
                .format()
                .as_deref(),
            Some("mp3")
        );
        assert_eq!(
            Track::fallback(Path::new("/music/x.Flac"))
                .format()
                .as_deref(),
            Some("flac")
        );
    }

    #[test]
    fn format_is_none_without_an_extension() {
        assert_eq!(Track::fallback(Path::new("/music/无扩展名")).format(), None);
    }

    fn with_audio(path: &str, rate: Option<u32>, bits: Option<u8>, bitrate: Option<u32>) -> Track {
        let mut t = Track::fallback(Path::new(path));
        t.audio = AudioInfo {
            sample_rate: rate,
            bit_depth: bits,
            bitrate,
        };
        t
    }

    #[test]
    fn 无损判定看编码不看采样规格() {
        for f in ["flac", "wav", "ape", "alac", "aiff"] {
            assert!(
                Track::fallback(Path::new(&format!("/m/x.{f}"))).is_lossless(),
                "{f} 是无损编码"
            );
        }
        for f in ["mp3", "m4a", "aac", "ogg", "opus"] {
            assert!(
                !Track::fallback(Path::new(&format!("/m/x.{f}"))).is_lossless(),
                "{f} 是有损编码"
            );
        }
        assert!(!Track::fallback(Path::new("/m/无扩展名")).is_lossless());
    }

    #[test]
    fn 可播性看编码有没有解码实现而不是无损与否() {
        // 有损且引擎解不了：标签、时长都读得到，就是放不出声
        assert!(!Track::fallback(Path::new("/m/x.wma")).is_playable());
        assert!(!Track::fallback(Path::new("/m/x.wma")).is_lossless());
        // 无损且能播
        assert!(Track::fallback(Path::new("/m/x.ape")).is_playable());
        assert!(Track::fallback(Path::new("/m/x.ape")).is_lossless());

        for f in ["ape", "wv", "opus"] {
            assert!(
                Track::fallback(Path::new(&format!("/m/x.{f}"))).is_playable(),
                "{f} 已经有专门的解码器"
            );
        }
        for f in ["flac", "mp3", "m4a", "aac", "wav", "ogg", "mka", "alac"] {
            assert!(
                Track::fallback(Path::new(&format!("/m/x.{f}"))).is_playable(),
                "{f} 应当能播"
            );
        }
    }

    #[test]
    fn 可播性大小写不敏感() {
        assert!(!Track::fallback(Path::new("/m/x.WMA")).is_playable());
        assert!(Track::fallback(Path::new("/m/x.FLAC")).is_playable());
    }

    #[test]
    fn 播不了的原因点名曲目与格式() {
        assert_eq!(
            Track::fallback(Path::new("/m/x.flac")).unplayable_reason(),
            None,
            "能播的曲目没有原因可讲"
        );

        let reason = Track::fallback(Path::new("/m/夜航西飞.wma"))
            .unplayable_reason()
            .expect("wma 应当给出原因");
        assert!(reason.contains("夜航西飞"), "要点名是哪一首：{reason}");
        assert!(reason.contains("wma"), "要点明是哪个格式：{reason}");
    }

    #[test]
    fn 采样规格够高的无损才算高解析() {
        let cd = with_audio("a.flac", Some(44100), Some(16), Some(900));
        assert_eq!(
            cd.quality(),
            Quality::Lossless,
            "CD 规格的 FLAC 不配 Hi-Res"
        );
        let hires = with_audio("a.flac", Some(96000), Some(24), Some(2834));
        assert_eq!(hires.quality(), Quality::HiRes);
        // 两个条件缺一不可：只抬采样率、位深仍是 16bit
        assert_eq!(
            with_audio("a.flac", Some(192_000), Some(16), None).quality(),
            Quality::Lossless
        );
        assert_eq!(
            with_audio("a.flac", Some(44100), Some(32), None).quality(),
            Quality::Lossless
        );
        // 有损编码哪怕标着 24bit/96kHz 也不该亮徽章
        assert_eq!(
            with_audio("a.mp3", Some(96000), Some(24), Some(320)).quality(),
            Quality::Lossy
        );
        // 读不到规格时无损仍是无损，只是不敢报 Hi-Res
        assert_eq!(
            with_audio("a.wav", None, None, None).quality(),
            Quality::Lossless
        );
    }

    #[test]
    fn 采样率显示去掉无意义的零() {
        assert_eq!(kilo(96000), "96");
        assert_eq!(kilo(44100), "44.1");
        assert_eq!(kilo(48000), "48");
        assert_eq!(kilo(22050), "22.05");
        assert_eq!(kilo(8000), "8");
    }

    #[test]
    fn 详情只列有值的项且顺序稳定() {
        let full = AudioInfo {
            sample_rate: Some(96000),
            bit_depth: Some(24),
            bitrate: Some(2834),
        };
        assert_eq!(
            full.details(),
            vec![
                ("采样率", "96 kHz".to_owned()),
                ("位深", "24-bit".to_owned()),
                ("码率", "2834 kbps".to_owned()),
            ]
        );

        // 有损编码没有位深，那一行就不该出现
        let lossy = AudioInfo {
            sample_rate: Some(44100),
            bit_depth: None,
            bitrate: Some(320),
        };
        assert_eq!(
            lossy.details(),
            vec![
                ("采样率", "44.1 kHz".to_owned()),
                ("码率", "320 kbps".to_owned()),
            ]
        );

        assert!(AudioInfo::default().is_empty());
        assert!(AudioInfo::default().details().is_empty());
    }

    /// 指纹只回答一个问题：这两次读到的还是同一份文件吗。
    #[test]
    fn 指纹只认同一次读取() {
        let dir = std::env::temp_dir().join(format!("mp-stamp-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("建临时目录");
        let file = dir.join("a.flac");
        std::fs::write(&file, b"abc").expect("写临时文件");

        let first = FileStamp::of(&file);
        assert!(first.modified.is_some(), "文件在就应当读到时间戳");
        assert_eq!(first.size, Some(3));
        assert!(
            first.matches(&FileStamp::of(&file)),
            "没动过的文件两次读到的一样"
        );

        std::fs::write(&file, b"abcdef").expect("改写临时文件");
        assert!(
            !first.matches(&FileStamp::of(&file)),
            "大小变了必须认出来：只比时间戳会被同秒覆写骗过去"
        );

        // 没读到的一律当「变了」：宁可多读一遍标签，也不能让旧标签永远留在库里
        assert!(!first.matches(&FileStamp::default()));
        assert!(!FileStamp::default().matches(&first));
        assert!(!FileStamp::default().matches(&FileStamp::default()));

        std::fs::remove_dir_all(&dir).ok();
    }
}
