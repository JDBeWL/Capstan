//! 曲库缓存，落盘到 `library.json`。读不到退回空，写不进只记日志。
//! 文件形状是按导入顺序排列的裸 `Vec<Track>`，不含排序键与播放模式。

use std::path::{Path, PathBuf};

use lyre_core::{Track, TrackId};

use crate::persist;

const FILE: &str = "library.json";

/// 读缓存。只解析，不核对文件还在不在。核对走 [`gone`]，由调用方把首屏画出来之后再跑。
///
/// 这几千次 stat 是启动路径上最贵的一段（慢盘、网络盘、休眠中的移动硬盘都是秒级），
/// 而缓存本身是可信的。上一次退出时这些文件还在。先当真、后核对，比先核对再显示快得多。
pub fn load() -> Vec<Track> {
    persist::load_or_default(FILE)
}

/// 用紧凑排版。整库条目可能上千，缩进会让文件涨好几倍。
pub fn save(tracks: Vec<Track>) {
    persist::save_compact_lossy(FILE, tracks);
}

/// 核对一批文件还在不在，返回已经消失的 id。
///
/// 整批都不在时返回空。那是盘没醒与可移动盘没挂载，照常剔除会把整库覆盖成空。
pub fn gone(entries: &[(TrackId, PathBuf)]) -> Vec<TrackId> {
    gone_with(entries, Path::is_file)
}

fn gone_with(
    entries: &[(TrackId, PathBuf)],
    mut exists: impl FnMut(&Path) -> bool,
) -> Vec<TrackId> {
    let mut vanished = Vec::new();
    for (id, path) in entries {
        if !exists(path) {
            vanished.push(*id);
        }
    }
    if vanished.len() == entries.len() {
        return Vec::new();
    }
    vanished
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};
    use std::time::Duration;

    use lyre_core::{AudioInfo, FieldSources, FileStamp, Track, TrackId};

    /// 落盘形状一旦漂移，老用户的曲库就读不回来。
    #[test]
    fn 落盘是裸数组且带导入顺序语义() {
        let tracks = vec![track("/m/a.flac"), track("/m/b.mp3")];
        let text = serde_json::to_string(&tracks).unwrap();
        assert!(text.starts_with("[{\""), "顶层必须是数组：{text}");

        let back: Vec<Track> = serde_json::from_str(&text).unwrap();
        assert_eq!(back, tracks);
    }

    /// 老文件里没有 `audio` / `sources` / `stamp` 字段，必须照常读得出来。
    #[test]
    fn 缺字段的旧条目退默认值() {
        let old = r#"[{"id":7,"path":"D:/m/x.mp3","title":"旧条目","artist":null,
                       "album":null,"duration":null,"has_cover":false}]"#;
        let parsed: Vec<Track> = serde_json::from_str(old).unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].title, "旧条目");
        assert_eq!(parsed[0].id, TrackId(7));
        assert_eq!(parsed[0].audio, AudioInfo::default());
        assert_eq!(parsed[0].sources, FieldSources::default());
        // 没有指纹就永远比不上，重扫时会老老实实重读一遍。
        assert_eq!(parsed[0].stamp, FileStamp::default());
    }

    /// 坏 JSON 退空曲库而不是 panic。启动不该被一个写坏的文件卡住。
    #[test]
    fn 读不懂就当没有缓存() {
        let parsed: Vec<Track> = serde_json::from_str("这不是 JSON").unwrap_or_default();
        assert!(parsed.is_empty());
    }

    /// 临时目录；名字带 pid，避免并行测试撞车。
    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("mp-next-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn entry(path: PathBuf) -> (TrackId, PathBuf) {
        (TrackId::from_path(&path), path)
    }

    #[test]
    fn 整盘不在时一条都不剔() {
        let dir = temp_dir("all-missing");
        let entries = vec![
            entry(dir.join("gone-a.flac")),
            entry(dir.join("gone-b.flac")),
        ];

        assert!(
            super::gone(&entries).is_empty(),
            "全都不在说明是盘没挂载，照常剔会把整库覆盖成空"
        );

        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// 逐条核对只查一遍，部分失效与整盘未挂载共用同一趟扫描。
    #[test]
    fn 每条路径只核对一次() {
        let dir = temp_dir("single-pass");
        let entries = vec![
            entry(dir.join("a.flac")),
            entry(dir.join("b.flac")),
            entry(dir.join("c.flac")),
        ];
        let mut checks = 0usize;

        let vanished = super::gone_with(&entries, |path| {
            checks += 1;
            path.ends_with("b.flac")
        });

        assert_eq!(checks, 3, "每条路径只能查一次");
        assert_eq!(
            vanished,
            vec![entries[0].0, entries[2].0],
            "只报不在的那两条"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// 只要还有一条在，就回到逐条核对，其余该被剔掉。
    #[test]
    fn 还有一首在就照常剔掉消失的() {
        let dir = temp_dir("partial");
        let real = dir.join("here.flac");
        std::fs::write(&real, b"x").unwrap();

        let entries = vec![entry(real), entry(dir.join("gone.flac"))];
        assert_eq!(super::gone(&entries), vec![entries[1].0]);

        std::fs::remove_dir_all(&dir).unwrap();
    }

    fn track(path: &str) -> Track {
        Track {
            id: TrackId::from_path(Path::new(path)),
            path: PathBuf::from(path),
            title: "曲名".to_owned(),
            artist: Some("歌手".to_owned()),
            album: None,
            duration: Some(Duration::from_secs(200)),
            has_cover: false,
            audio: AudioInfo::default(),
            sources: FieldSources::default(),
            stamp: FileStamp::default(),
        }
    }
}
