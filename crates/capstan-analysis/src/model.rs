//! 曲目分析结果的数据模型与查询方法。

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use capstan_core::FileStamp;
use serde::{Deserialize, Serialize};

/// 缓存版本号。分析算法或字段含义变更时递增，使旧缓存自动失效。
pub const ANALYSIS_VERSION: u32 = 2;

/// 跨线程取消令牌。后台分析线程定期检查，队列变化或退出时及时中止。
#[derive(Clone, Debug, Default)]
pub struct Cancel(Arc<AtomicBool>);

impl Cancel {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancel(&self) {
        self.0.store(true, Ordering::Release);
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}

/// 调式，仅区分大调与小调。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum MusicalMode {
    Major,
    Minor,
}

/// 调性估计结果，附带 Camelot 编号用于快速判断和声兼容度。
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct KeyInfo {
    /// 主音半音序号，0 = C，1 = C#，…，11 = B。
    pub tonic: u8,
    pub mode: MusicalMode,
    /// Camelot 轮编号，取值 1..=12。
    pub camelot_num: u8,
    /// `'B'` 表示大调，`'A'` 表示小调。
    pub camelot_letter: char,
    /// 置信度，取值 0..=1。
    pub confidence: f32,
}

const PITCH_NAMES: [&str; 12] = [
    "C", "C#", "D", "Eb", "E", "F", "F#", "G", "Ab", "A", "Bb", "B",
];

impl KeyInfo {
    /// Camelot 短码，如 `"8B"` 或 `"5A"`。
    pub fn camelot_code(&self) -> String {
        format!("{}{}", self.camelot_num, self.camelot_letter)
    }

    /// 供界面展示的调性名称，如 `"C 大调 · 8B"`。
    pub fn label(&self) -> String {
        let name = PITCH_NAMES
            .get(usize::from(self.tonic % 12))
            .copied()
            .unwrap_or("?");
        let mode = match self.mode {
            MusicalMode::Major => "大调",
            MusicalMode::Minor => "小调",
        };
        format!("{name} {mode} · {}", self.camelot_code())
    }

    /// 按 Camelot 轮计算两个调性的兼容度，取值 0..=1。
    /// 同调返回 1.0，相邻五度或关系大小调返回 0.85，距离两步返回 0.5，其余返回 0.15。
    pub fn compatibility(&self, other: &Self) -> f32 {
        let a = i32::from(self.camelot_num);
        let b = i32::from(other.camelot_num);
        let diff = (a - b).abs();
        let circle = diff.min(12 - diff);
        let same_mode = self.camelot_letter == other.camelot_letter;

        match (circle, same_mode) {
            (0, true) => 1.0,
            (0, false) | (1, true) => 0.85,
            (1, false) | (2, true) => 0.5,
            _ => 0.15,
        }
    }
}

/// 一首曲目的完整离线分析结果。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TrackAnalysis {
    pub version: u32,
    pub stamp: FileStamp,
    /// 音频实际解码时长，单位秒。
    pub duration: f32,
    /// 估计速度，单位 BPM。
    pub bpm: f32,
    /// 速度置信度，取值 0..=1。
    pub bpm_confidence: f32,
    /// 速度在全曲中的稳定度，取值 0..=1。低稳定度曲目不参与对拍混音。
    pub tempo_stability: f32,
    /// 节拍时刻序列，单位秒，单调递增。
    pub beats: Vec<f32>,
    /// 第一个小节线在 `beats` 中的下标，取值 0..4。
    pub downbeat_phase: u8,
    /// 乐句长度，单位小节，取 8 或 16。
    pub phrase_bars: u8,
    /// 第一个乐句边界相对第一个小节线的偏移，单位小节。
    pub phrase_offset_bars: u16,
    /// 逐小节的三频带能量（低、中、高），归一化量化到 0..=255。
    pub bar_energy: Vec<[u8; 3]>,
    /// 逐小节的人声存在度估计，归一化量化到 0..=255。
    #[serde(default)]
    pub bar_vocal: Vec<u8>,
    /// 逐拍的均方根电平，归一化量化到 0..=255，用于编辑器绘制能量包络。
    pub beat_rms: Vec<u8>,
    /// 每 100 ms 一格的三频彩色波形包络（低、中、高），量化到 0..=255。
    #[serde(default)]
    pub waveform_rgb: Vec<[u8; 3]>,
    /// ITU-R BS.1770-4 积分响度，单位 LUFS。
    pub loudness_lufs: f32,
    /// 片头静音时长，单位秒。
    pub lead_silence: f32,
    /// 片尾静音时长，单位秒。
    pub tail_silence: f32,
    /// 前奏结束时刻，单位秒。
    pub intro_end: f32,
    /// 尾奏开始时刻，单位秒。
    pub outro_start: f32,
    /// 首次进入明显人声段的时刻，单位秒。
    #[serde(default)]
    pub vocal_start: f32,
    /// 最后一段明显人声结束的时刻，单位秒。
    #[serde(default)]
    pub vocal_end: f32,
    /// 估计调性；纯打击乐或过短素材时为 `None`。
    pub key: Option<KeyInfo>,
}

impl TrackAnalysis {
    /// 单拍时长，单位秒。BPM 无效时退回 0.5 秒（120 BPM）。
    pub fn beat_period(&self) -> f32 {
        if self.bpm > 1.0 && self.bpm.is_finite() {
            60.0 / self.bpm
        } else {
            0.5
        }
    }

    /// 单小节时长（按 4/4 拍计），单位秒。
    pub fn bar_period(&self) -> f32 {
        self.beat_period() * 4.0
    }

    /// 全部小节线时刻，单位秒。
    pub fn downbeats(&self) -> Vec<f32> {
        let phase = usize::from(self.downbeat_phase % 4);
        if phase >= self.beats.len() {
            return Vec::new();
        }
        self.beats[phase..].iter().step_by(4).copied().collect()
    }

    /// 小节总数。
    pub fn bar_count(&self) -> usize {
        let phase = usize::from(self.downbeat_phase % 4);
        if phase >= self.beats.len() {
            return 0;
        }
        (self.beats.len() - phase).div_ceil(4)
    }

    /// 第 `bar` 个小节的起始时刻，单位秒。
    pub fn bar_time(&self, bar: usize) -> Option<f32> {
        let beat_idx = usize::from(self.downbeat_phase % 4) + bar * 4;
        self.beats.get(beat_idx).copied()
    }

    /// 指定时刻所在的小节序号。早于第一个小节线返回 `None`。
    pub fn bar_at(&self, time: f32) -> Option<usize> {
        let downbeats = self.downbeats();
        if downbeats.is_empty() || time < downbeats[0] {
            return None;
        }
        let pos = downbeats.partition_point(|&t| t <= time);
        pos.checked_sub(1)
    }

    /// 全部乐句边界时刻，单位秒。
    pub fn phrase_boundaries(&self) -> Vec<f32> {
        let downbeats = self.downbeats();
        let step = usize::from(self.phrase_bars.max(1));
        let offset = usize::from(self.phrase_offset_bars);
        if offset >= downbeats.len() {
            return downbeats;
        }
        downbeats[offset..].iter().step_by(step).copied().collect()
    }

    /// 不早于 `after` 的第一个小节线时刻。
    pub fn next_downbeat(&self, after: f32) -> Option<f32> {
        let phase = usize::from(self.downbeat_phase % 4);
        if phase >= self.beats.len() {
            return None;
        }
        self.beats[phase..]
            .iter()
            .step_by(4)
            .copied()
            .find(|&t| t >= after)
    }

    /// 将任意时刻吸附到最近的网格线（乐句线或小节线）；没有网格时原样返回。
    pub fn snap_to_grid(&self, time: f32, prefer_phrase: bool) -> f32 {
        let grid = if prefer_phrase {
            let phrases = self.phrase_boundaries();
            if phrases.is_empty() {
                self.downbeats()
            } else {
                phrases
            }
        } else {
            self.downbeats()
        };
        if grid.is_empty() {
            return time.clamp(0.0, self.duration);
        }
        grid.into_iter()
            .min_by(|a, b| (a - time).abs().total_cmp(&(b - time).abs()))
            .unwrap_or(time)
            .clamp(0.0, self.duration)
    }

    /// 指定小节的总能量（低中高频均分），归一化到 0..=1。
    pub fn bar_total_energy(&self, bar: usize) -> f32 {
        let Some(&[lo, mid, hi]) = self.bar_energy.get(bar) else {
            return 0.0;
        };
        (f32::from(lo) + f32::from(mid) + f32::from(hi)) / (3.0 * 255.0)
    }

    /// 指定小节的中频能量（主要对应人声与主旋律），归一化到 0..=1。
    pub fn bar_mid_energy(&self, bar: usize) -> f32 {
        self.bar_energy
            .get(bar)
            .map_or(0.0, |&[_, mid, _]| f32::from(mid) / 255.0)
    }

    /// 指定小节的低频能量（主要对应底鼓与贝斯），归一化到 0..=1。
    pub fn bar_low_energy(&self, bar: usize) -> f32 {
        self.bar_energy
            .get(bar)
            .map_or(0.0, |&[lo, _, _]| f32::from(lo) / 255.0)
    }

    /// 指定小节的人声存在度，归一化到 0..=1。旧数据缺少该字段时回退到中频能量估计。
    pub fn bar_vocal_level(&self, bar: usize) -> f32 {
        if self.bar_vocal.is_empty() {
            return self.bar_mid_energy(bar) * 0.7;
        }
        self.bar_vocal
            .get(bar)
            .map_or(0.0, |&v| f32::from(v) / 255.0)
    }

    /// 查询指定时刻所在小节的人声存在度，归一化到 0..=1。
    pub fn vocal_at_time(&self, time: f32) -> f32 {
        let Some(bar) = self.bar_at(time) else {
            return 0.0;
        };
        self.bar_vocal_level(bar)
    }
}
