//! 最近搜索词，落盘到 `search-history.json`。只有用户停留过的查询才进来，打字途中的中间态不留。
//! 读不到退回空表，写不进只记日志。

use crate::persist;

/// 落盘文件名。
const FILE: &str = "search-history.json";

/// 留几条。再多，搜索页顶部这一块会把「浏览全部」推到首屏之外。
pub const HISTORY_CAP: usize = 8;

/// 最近在前的查询词。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SearchHistory {
    queries: Vec<String>,
}

impl SearchHistory {
    /// 读不到（首次启动、文件损坏、目录不可读）就退回空表。
    pub fn load() -> Self {
        Self {
            queries: persist::load_or_default(FILE),
        }
    }

    pub fn save(&self) {
        persist::save_lossy(FILE, self.queries.clone());
    }

    /// 记一条。同一个词再搜一次只挪到最前，不占两格；首尾空白不算内容。
    pub fn record(&mut self, query: &str) {
        let query = query.trim();
        if query.is_empty() {
            return;
        }
        self.queries.retain(|existing| existing != query);
        self.queries.insert(0, query.to_owned());
        self.queries.truncate(HISTORY_CAP);
    }

    pub fn clear(&mut self) {
        self.queries.clear();
    }

    /// 最近在前。
    pub fn entries(&self) -> &[String] {
        &self.queries
    }

    pub fn is_empty(&self) -> bool {
        self.queries.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::{HISTORY_CAP, SearchHistory};

    #[test]
    fn searching_the_same_word_again_moves_it_to_the_front() {
        let mut history = SearchHistory::default();
        history.record("椎名林檎");
        history.record("坂本龍一");
        history.record("椎名林檎");

        assert_eq!(
            history.entries(),
            ["椎名林檎", "坂本龍一"],
            "同一个词不该占两格"
        );
    }

    #[test]
    fn the_oldest_entry_is_pushed_out() {
        let mut history = SearchHistory::default();
        for index in 0..(HISTORY_CAP + 4) {
            history.record(&format!("第 {index} 次"));
        }

        assert_eq!(history.entries().len(), HISTORY_CAP);
        assert_eq!(
            history.entries().first().map(String::as_str),
            Some(format!("第 {} 次", HISTORY_CAP + 3).as_str()),
            "最新的一条排最前"
        );
        assert!(
            !history.entries().iter().any(|entry| entry == "第 0 次"),
            "最旧的那条应当已经被挤出去"
        );
    }

    /// 空格与空串不进历史。搜索框清空、只敲了个空格都不该留下记录。
    #[test]
    fn blank_queries_are_dropped_and_surrounding_space_is_trimmed() {
        let mut history = SearchHistory::default();
        history.record("");
        history.record("   ");
        assert!(history.is_empty());

        history.record("  青叶市子  ");
        assert_eq!(history.entries(), ["青叶市子"]);
    }

    /// 落盘形状是 `Vec<String>`；形状一变，老用户的文件就读不回来了。
    #[test]
    fn round_trips_through_json() {
        let mut history = SearchHistory::default();
        history.record("A");
        history.record("B");

        let text = serde_json::to_string(history.entries()).expect("序列化不该失败");
        assert_eq!(text, r#"["B","A"]"#);

        let parsed: Vec<String> = serde_json::from_str(&text).expect("反序列化不该失败");
        assert_eq!(
            SearchHistory { queries: parsed },
            history,
            "读回来的顺序与写下去的一致"
        );
    }

    #[test]
    fn corrupt_file_yields_empty_history() {
        let parsed: Vec<String> = serde_json::from_str("这不是 JSON").unwrap_or_default();
        assert!(parsed.is_empty());
    }
}
