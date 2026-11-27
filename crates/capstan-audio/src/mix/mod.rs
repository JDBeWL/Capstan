//! 自动化曲线、三段均衡、扫频滤镜、节拍回声与混音计划，供双轨混音源与上下层共享。
//! 本模块不依赖 BPM 或分析模型，只描述时间点、配方与曲线求值。

pub mod curve;
pub mod filter;
pub mod fx;
pub mod plan;
pub mod tempo;

pub use curve::{Automation, CompiledAutomation, ControlPoint, CurvePreset};
pub use filter::{LowShelf, LowShelfBank, SweepFilterBank, ThreeBandEqBank};
pub use fx::{EchoOutBank, SoftLimiter};
pub use plan::{CompiledMix, MixKind, MixPlan, MixStyle, NextTrack, TrimRamp};
pub use tempo::{TempoAdapter, TempoMode};
