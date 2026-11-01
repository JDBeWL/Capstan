//! 曲库根落盘到 `roots.json`：一个裸的字符串数组，按用户加进来的顺序。
//! 读不到就当没有根（老版本没有这份文件），写不进只记日志 —— 丢了根顶多是下次不补扫，
//! 曲目本身在 `library.json` 里，不该因此启动失败。

use std::path::PathBuf;

use lyre_core::Roots;

use crate::persist;

const FILE: &str = "roots.json";

pub fn load() -> Roots {
    let paths: Vec<PathBuf> = persist::load_or_default(FILE);
    // 存进来的可能互相包含（老版本手改、目录被挪到一起），读回来时重新折一遍。
    Roots::from_paths(paths)
}

/// 用紧凑排版：根通常只有几行，但和曲库一样不需要缩进。
pub fn save(roots: &Roots) {
    persist::save_compact_lossy(FILE, roots.items().to_vec());
}

#[cfg(test)]
mod tests {
    // 不写 `use super::*`：`crate::platform::*` 带进来的 gpui `test` 宏会遮蔽内置 `#[test]`。

    /// 落盘形状是裸数组：老版本读到 `[{"path": ...}]` 会因为类型对不上整份丢掉。
    #[test]
    fn 落盘是裸字符串数组() {
        let text = serde_json::to_string(&vec![
            std::path::PathBuf::from("/music"),
            std::path::PathBuf::from("/other"),
        ])
        .unwrap();
        assert!(text.starts_with("[\""), "顶层必须是数组：{text}");

        let back: Vec<std::path::PathBuf> = serde_json::from_str(&text).unwrap();
        assert_eq!(back.len(), 2);
    }

    /// 读回来要按 `Roots` 的规矩折一遍，不能把父子目录并排摆着。
    #[test]
    fn 读回来会折掉互相包含的根() {
        let base = std::env::temp_dir().join("mp-roots");
        let text = serde_json::to_string(&vec![
            base.clone(),
            base.join("Live"),
            base.with_file_name("mp-roots-old"),
        ])
        .unwrap();

        let decoded: Vec<std::path::PathBuf> = serde_json::from_str(&text).unwrap();
        let roots = lyre_core::Roots::from_paths(decoded);
        assert_eq!(roots.len(), 2, "子目录应当被父目录吸收，兄弟目录要留着");
    }
}
