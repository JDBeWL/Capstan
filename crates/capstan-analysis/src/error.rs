//! 分析模块的错误类型。

use thiserror::Error;

#[derive(Debug, Error)]
pub enum AnalysisError {
    #[error("解码失败：{0}")]
    Audio(#[from] capstan_audio::AudioError),
    #[error("重采样失败：{0}")]
    Resampler(String),
    #[error("分析已取消")]
    Cancelled,
    #[error("音频时长过短，无法分析节奏")]
    TooShort,
    #[error("缓存读写失败：{0}")]
    Io(#[from] std::io::Error),
    #[error("缓存序列化失败：{0}")]
    Json(#[from] serde_json::Error),
}

pub type Result<T> = std::result::Result<T, AnalysisError>;
