//! 实体层，应用状态的宿主。

pub mod analysis;
pub mod filter;
mod library;
mod playback;

pub use analysis::{AnalysisEntity, AnalysisEvent, AnalysisPriority};
pub use capstan_audio::PlaybackState;
pub use library::{
    CollectionSummary, FAVORITES_NAME, LibraryEntity, LibraryEvent, RepeatState, ScanProgress,
    Scope, ScopeKind, Step, searching,
};
pub use playback::{PlaybackEntity, PlaybackEvent};
