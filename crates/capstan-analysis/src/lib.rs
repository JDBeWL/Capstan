//! 离线音频分析内核，包含 BPM、节拍网格、小节线、乐句、能量包络、响度、调性与缓存。

pub mod arrange;
pub mod beats;
pub mod cache;
pub mod error;
pub mod key;
pub mod loudness;
pub mod model;
pub mod onset;
pub mod pcm;
pub mod planner;
pub mod stft;
pub mod structure;
pub mod tempo;

use std::path::Path;

use capstan_core::FileStamp;

pub use arrange::{ArrangeItem, arrange_queue, pair_score};
pub use cache::AnalysisCache;
pub use error::{AnalysisError, Result};
pub use model::{ANALYSIS_VERSION, Cancel, KeyInfo, MusicalMode, TrackAnalysis};
pub use planner::{
    AutoMixPrefs, PlanContext, StylePreference, TransitionLength, TrimLevel, apply_style_recipe,
    gapless_plan, plan,
};
pub use structure::WAVEFORM_BIN_SECS;

/// 离线分析一首音频文件。`bpm_tag` 作为速度先验，无标签传 `None`。
pub fn analyze_file(path: &Path, bpm_tag: Option<f32>, cancel: &Cancel) -> Result<TrackAnalysis> {
    let stamp = FileStamp::of(path);
    let mut stft_proc = stft::StreamingStft::new();
    let mut onset_col = onset::OnsetCollector::new();
    let mut chroma_col = key::ChromaCollector::new();

    let summary = pcm::stream_file(path, cancel, |chunk| {
        stft_proc.push(chunk, |magnitudes, rms| {
            onset_col.push_frame(magnitudes, rms);
            chroma_col.push_frame(magnitudes);
        });
    })?;

    if cancel.is_cancelled() {
        return Err(AnalysisError::Cancelled);
    }

    finalize(&summary, stamp, onset_col, chroma_col, bpm_tag)
}

/// 对内存中的交错 PCM 流执行完整分析，与 [`analyze_file`] 共享同一套 DSP 管线。
pub fn analyze_pcm<I>(
    sample_rate: u32,
    channels: u16,
    samples: I,
    bpm_tag: Option<f32>,
    stamp: FileStamp,
    cancel: &Cancel,
) -> Result<TrackAnalysis>
where
    I: Iterator<Item = f32>,
{
    let mut stft_proc = stft::StreamingStft::new();
    let mut onset_col = onset::OnsetCollector::new();
    let mut chroma_col = key::ChromaCollector::new();

    let summary = pcm::stream_interleaved(sample_rate, channels, samples, cancel, |chunk| {
        stft_proc.push(chunk, |magnitudes, rms| {
            onset_col.push_frame(magnitudes, rms);
            chroma_col.push_frame(magnitudes);
        });
    })?;

    if cancel.is_cancelled() {
        return Err(AnalysisError::Cancelled);
    }

    finalize(&summary, stamp, onset_col, chroma_col, bpm_tag)
}

fn finalize(
    summary: &pcm::StreamSummary,
    stamp: FileStamp,
    onset_col: onset::OnsetCollector,
    chroma_col: key::ChromaCollector,
    bpm_tag: Option<f32>,
) -> Result<TrackAnalysis> {
    if summary.duration < 1.0 {
        return Err(AnalysisError::TooShort);
    }

    let features = onset_col.finish();
    let (key_info, chroma_frames) = chroma_col.finish();
    let tempo_est = tempo::estimate(&features.onset, bpm_tag);
    let (beats, downbeat_phase) = beats::track_beats(
        &features.onset,
        &features.low_onset,
        &features.band_energy,
        tempo_est.bpm,
    );
    let struct_est = structure::analyze(
        summary.duration,
        &features.frame_rms,
        &features.band_energy,
        &features.frame_vocal,
        &summary.center_ratio_100ms,
        &chroma_frames,
        &beats,
        downbeat_phase,
    );

    Ok(TrackAnalysis {
        version: ANALYSIS_VERSION,
        stamp,
        duration: summary.duration,
        bpm: tempo_est.bpm,
        bpm_confidence: tempo_est.confidence,
        tempo_stability: tempo_est.stability,
        beats,
        downbeat_phase,
        phrase_bars: struct_est.phrase_bars,
        phrase_offset_bars: struct_est.phrase_offset_bars,
        bar_energy: struct_est.bar_energy,
        bar_vocal: struct_est.bar_vocal,
        beat_rms: struct_est.beat_rms,
        waveform_rgb: struct_est.waveform_rgb,
        loudness_lufs: summary.loudness_lufs,
        lead_silence: struct_est.lead_silence,
        tail_silence: struct_est.tail_silence,
        intro_end: struct_est.intro_end,
        outro_start: struct_est.outro_start,
        vocal_start: struct_est.vocal_start,
        vocal_end: struct_est.vocal_end,
        key: key_info,
    })
}
