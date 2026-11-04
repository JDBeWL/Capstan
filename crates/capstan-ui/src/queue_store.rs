//! 播放队列落盘到 `queue.json`。存路径而不是 `TrackId`。id 由路径算出，换一轮导入可能变，
//! 路径才是跨启动稳定的键。`None` 表示队列跟着曲库走（没有显式队列），文件里写 `null`。

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::persist;

const FILE: &str = "queue.json";

#[derive(Serialize, Deserialize)]
struct Stored {
    /// `None` = 队列跟着曲库走。
    paths: Option<Vec<PathBuf>>,
}

/// 读上次的显式队列。文件不存在、损坏、或本来就跟着曲库走，都返回 `None`。
pub fn load() -> Option<Vec<PathBuf>> {
    persist::load_or_none::<Stored>(FILE).and_then(|stored| stored.paths)
}

/// 写回显式队列；`None` 写成 `null`，下次启动继续跟着曲库走。
pub fn save(paths: Option<Vec<PathBuf>>) {
    persist::save_lossy(FILE, Stored { paths });
}

#[cfg(test)]
mod tests {
    // 不写 `use super::*`，`crate::platform::*` 带进来的 gpui `test` 宏会遮蔽内置 `#[test]`。
    use super::Stored;

    /// 「跟着曲库走」与「显式队列」必须在落盘形状上分得开，否则重启时会多出一条空队列。
    #[test]
    fn 跟着曲库走和有显式队列在落盘形状上分得开() {
        let following: Stored = serde_json::from_str(r#"{"paths":null}"#).expect("null 要能读");
        assert!(following.paths.is_none());

        let explicit: Stored =
            serde_json::from_str(r#"{"paths":["D:/m/a.flac","D:/m/b.flac"]}"#).expect("数组要能读");
        assert_eq!(explicit.paths.map(|paths| paths.len()), Some(2));
    }
}
