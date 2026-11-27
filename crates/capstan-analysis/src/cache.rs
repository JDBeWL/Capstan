//! 分析结果的磁盘缓存。每首曲目独立落盘为 `<TrackId>.json`，先写 `.tmp` 再原子替换。

use std::fs;
use std::path::{Path, PathBuf};

use capstan_core::{FileStamp, TrackId};

use crate::error::Result;
use crate::model::{ANALYSIS_VERSION, TrackAnalysis};

/// 磁盘缓存句柄，读写指定目录下的曲目分析 JSON。
#[derive(Clone, Debug)]
pub struct AnalysisCache {
    dir: PathBuf,
}

impl AnalysisCache {
    pub fn new(dir: PathBuf) -> Self {
        Self { dir }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn path_of(&self, id: TrackId) -> PathBuf {
        self.dir.join(format!("{id}.json"))
    }

    /// 读取缓存并校验版本号与文件指纹。缺失、损坏或已过期均返回 `None`。
    pub fn load(&self, id: TrackId, stamp: FileStamp) -> Option<TrackAnalysis> {
        let path = self.path_of(id);
        let text = fs::read_to_string(&path).ok()?;
        let analysis: TrackAnalysis = match serde_json::from_str(&text) {
            Ok(parsed) => parsed,
            Err(err) => {
                log::debug!("忽略损坏的分析缓存 {}：{err}", path.display());
                return None;
            }
        };
        if analysis.version != ANALYSIS_VERSION || !analysis.stamp.matches(&stamp) {
            return None;
        }
        Some(analysis)
    }

    /// 原子写入单首曲目的分析结果。
    pub fn save(&self, id: TrackId, analysis: &TrackAnalysis) -> Result<()> {
        fs::create_dir_all(&self.dir)?;
        let path = self.path_of(id);
        let tmp = self.dir.join(format!("{id}.json.tmp"));
        let text = serde_json::to_string(analysis)?;
        fs::write(&tmp, text)?;
        fs::rename(&tmp, &path)?;
        Ok(())
    }
}
