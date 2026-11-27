//! 合成素材端到端测试，覆盖 BPM、节拍相位、小节线、八度先验、稳定性、响度、静音、调性与缓存。
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::f32::consts::TAU;

use capstan_analysis::{
    ANALYSIS_VERSION, AnalysisCache, Cancel, MusicalMode, analyze_file, analyze_pcm,
};
use capstan_core::{FileStamp, TrackId};

/// 确定性 LCG，避免测试引入随机源依赖。
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Self(seed)
    }

    fn next_f32(&mut self) -> f32 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1);
        let bits = (self.0 >> 40) as u32;
        (bits as f32 / (1u32 << 24) as f32) * 2.0 - 1.0
    }
}

/// 生成带 4/4 低频重音、轻微背景噪声与 ±5 ms 抖动的点击轨（22.05 kHz 单声道）。
fn click_track(bpm: f32, seconds: f32, offset_secs: f32, jitter_ms: f32) -> (Vec<f32>, Vec<f32>) {
    let rate = 22_050usize;
    let total = (seconds * rate as f32).round() as usize;
    let mut pcm = vec![0.0_f32; total];
    let mut rng = Rng::new(u64::from(bpm.to_bits()) ^ 0x51a7_9b3d);

    // 极轻底噪，模拟真实录制背景。
    for s in &mut pcm {
        *s = rng.next_f32() * 0.008;
    }

    let period = 60.0 / bpm;
    let mut truth_beats = Vec::new();
    let mut beat_idx = 0usize;
    loop {
        let jitter = rng.next_f32() * (jitter_ms / 1000.0);
        let t = offset_secs + beat_idx as f32 * period + jitter;
        if t + 0.08 >= seconds {
            break;
        }
        truth_beats.push(t);
        let start = (t * rate as f32).round() as usize;
        let is_downbeat = beat_idx.is_multiple_of(4);
        let click_len = (0.045 * rate as f32) as usize;
        for i in 0..click_len {
            let pos = start + i;
            if pos >= total {
                break;
            }
            let dt = i as f32 / rate as f32;
            let env = (-dt * 70.0).exp();
            // 第一拍叠加更强的 70 Hz 底鼓，其余拍为中高频敲击 + 较轻底鼓。
            let kick_amp = if is_downbeat { 0.85 } else { 0.30 };
            let click_amp = if is_downbeat { 0.40 } else { 0.35 };
            let kick = (TAU * 70.0 * dt).sin() * kick_amp;
            let body = (TAU * 420.0 * dt).sin() * click_amp;
            let snap = (TAU * 2200.0 * dt).sin() * 0.20 * (-dt * 180.0).exp();
            pcm[pos] = (pcm[pos] + (kick + body) * env + snap).clamp(-1.0, 1.0);
        }
        beat_idx += 1;
    }

    (pcm, truth_beats)
}

fn median_beat_phase_error_ms(detected: &[f32], truth: &[f32]) -> f32 {
    let mut errors = Vec::new();
    for &t in truth {
        if let Some(&nearest) = detected
            .iter()
            .min_by(|a, b| (*a - t).abs().total_cmp(&(*b - t).abs()))
        {
            errors.push((nearest - t).abs() * 1000.0);
        }
    }
    errors.sort_by(f32::total_cmp);
    errors[errors.len() / 2]
}

#[test]
fn detects_bpm_beats_and_downbeats_across_five_tempos() {
    for bpm in [90.0_f32, 120.0, 128.0, 140.0, 174.0] {
        let (pcm, truth) = click_track(bpm, 24.0, 0.5, 5.0);
        let analysis = analyze_pcm(
            22_050,
            1,
            pcm.into_iter(),
            None,
            FileStamp::default(),
            &Cancel::new(),
        )
        .expect("合成点击轨应当分析成功");

        let bpm_err = (analysis.bpm - bpm).abs();
        assert!(
            bpm_err <= 0.5,
            "目标 {bpm} BPM 估计成 {:.2} BPM（误差 {bpm_err:.2}）",
            analysis.bpm
        );
        assert!(
            analysis.bpm_confidence >= 0.4,
            "{bpm} BPM 置信度过低：{:.2}",
            analysis.bpm_confidence
        );
        assert!(
            analysis.tempo_stability >= 0.7,
            "{bpm} BPM 稳定度过低：{:.2}",
            analysis.tempo_stability
        );

        let phase_err_ms = median_beat_phase_error_ms(&analysis.beats, &truth);
        assert!(
            phase_err_ms <= 15.0,
            "{bpm} BPM 节拍中位相位误差 {phase_err_ms:.1} ms 超过 15 ms"
        );

        // 验证小节线确实落在真实第 0、4、8... 拍上。
        let downbeats = analysis.downbeats();
        assert!(!downbeats.is_empty(), "{bpm} BPM 应当有小节线");
        let true_downbeats: Vec<f32> = truth.iter().step_by(4).copied().collect();
        let db_err_ms = median_beat_phase_error_ms(&downbeats, &true_downbeats);
        assert!(
            db_err_ms <= 25.0,
            "{bpm} BPM 小节线相位误差 {db_err_ms:.1} ms（phase={}）",
            analysis.downbeat_phase
        );
    }
}

#[test]
fn octave_ambiguity_respects_tag_prior_and_defaults_to_higher_prior() {
    // 140 BPM 轻敲击，仅偶数拍（70 BPM）有强底鼓。
    let rate = 22_050usize;
    let seconds = 24.0_f32;
    let total = (seconds * rate as f32).round() as usize;
    let mut pcm = vec![0.0_f32; total];
    let period = 60.0 / 140.0;

    let mut idx = 0usize;
    loop {
        let t = 0.4 + idx as f32 * period;
        if t + 0.06 >= seconds {
            break;
        }
        let start = (t * rate as f32).round() as usize;
        let len = (0.04 * rate as f32) as usize;
        let even = idx.is_multiple_of(2);
        for i in 0..len {
            let pos = start + i;
            if pos >= total {
                break;
            }
            let dt = i as f32 / rate as f32;
            let env = (-dt * 75.0).exp();
            let sample = if even {
                (TAU * 65.0 * dt).sin() * 0.8 + (TAU * 1200.0 * dt).sin() * 0.25
            } else {
                (TAU * 1800.0 * dt).sin() * 0.35
            };
            pcm[pos] = (pcm[pos] + sample * env).clamp(-1.0, 1.0);
        }
        idx += 1;
    }

    let without_tag = analyze_pcm(
        22_050,
        1,
        pcm.iter().copied(),
        None,
        FileStamp::default(),
        &Cancel::new(),
    )
    .expect("无先验应当分析成功");
    assert!(
        (without_tag.bpm - 140.0).abs() <= 1.0,
        "无标签先验时应落向先验更高的 140 BPM，实测 {:.2}",
        without_tag.bpm
    );

    let with_tag = analyze_pcm(
        22_050,
        1,
        pcm.into_iter(),
        Some(70.0),
        FileStamp::default(),
        &Cancel::new(),
    )
    .expect("带 70 BPM 标签先验应当分析成功");
    assert!(
        (with_tag.bpm - 70.0).abs() <= 1.0,
        "配合 70 BPM 标签先验时应判为 70 BPM，实测 {:.2}",
        with_tag.bpm
    );
}

#[test]
fn drifting_tempo_reports_low_stability() {
    // 速度在 28 秒内从 95 BPM 线性拉升到 150 BPM。
    let rate = 22_050usize;
    let seconds = 28.0_f32;
    let total = (seconds * rate as f32).round() as usize;
    let mut pcm = vec![0.0_f32; total];
    let mut t = 0.2_f32;
    while t + 0.05 < seconds {
        let bpm = 95.0 + (150.0 - 95.0) * (t / seconds);
        let start = (t * rate as f32).round() as usize;
        let len = (0.035 * rate as f32) as usize;
        for i in 0..len {
            if start + i < total {
                let dt = i as f32 / rate as f32;
                pcm[start + i] =
                    ((TAU * 120.0 * dt).sin() * (-dt * 80.0).exp() * 0.8).clamp(-1.0, 1.0);
            }
        }
        t += 60.0 / bpm;
    }

    let analysis = analyze_pcm(
        22_050,
        1,
        pcm.into_iter(),
        None,
        FileStamp::default(),
        &Cancel::new(),
    )
    .expect("变速素材应当分析成功");
    assert!(
        analysis.tempo_stability < 0.5,
        "速度大幅渐变素材的稳定度应当 < 0.5，实测 {:.2}",
        analysis.tempo_stability
    );
}

#[test]
fn bs1770_loudness_matches_reference_sine_wave() {
    // 1 kHz、峰值 -20 dBFS（0.1）的单声道正弦波，标准 BS.1770-4 积分响度为 -23.01 LUFS。
    let rate = 48_000u32;
    let frames = rate as usize * 3;
    let amp = 10.0_f32.powf(-20.0 / 20.0);
    let pcm = (0..frames).map(|i| (TAU * 1000.0 * i as f32 / rate as f32).sin() * amp);

    let analysis = analyze_pcm(rate, 1, pcm, None, FileStamp::default(), &Cancel::new())
        .expect("正弦波应当分析成功");
    let err = (analysis.loudness_lufs - (-23.01)).abs();
    assert!(
        err <= 0.3,
        "1 kHz -20 dBFS 正弦波响度应为 -23.0 ± 0.3 LUFS，实测 {:.2}",
        analysis.loudness_lufs
    );
}

#[test]
fn lead_and_tail_silence_are_measured_within_50ms() {
    let rate = 22_050usize;
    let pad = vec![0.0_f32; rate * 3];
    let (body, _) = click_track(120.0, 10.0, 0.0, 0.0);
    let mut pcm = Vec::with_capacity(pad.len() * 2 + body.len());
    pcm.extend_from_slice(&pad);
    pcm.extend_from_slice(&body);
    pcm.extend_from_slice(&pad);

    let analysis = analyze_pcm(
        22_050,
        1,
        pcm.into_iter(),
        None,
        FileStamp::default(),
        &Cancel::new(),
    )
    .expect("带静音头尾的素材应当分析成功");

    assert!(
        (analysis.lead_silence - 3.0).abs() <= 0.05,
        "片头静音应接近 3.0 s，实测 {:.3} s",
        analysis.lead_silence
    );
    assert!(
        (analysis.tail_silence - 3.0).abs() <= 0.05,
        "片尾静音应接近 3.0 s，实测 {:.3} s",
        analysis.tail_silence
    );
}

#[test]
fn c_major_progression_is_detected_as_c_major_8b() {
    // 合成 C 大调主属下属和弦进行（C-F-G-C），各持续 2 秒。
    let rate = 22_050usize;
    let chords: [&[f32]; 4] = [
        &[261.63, 329.63, 392.00, 523.25], // C
        &[349.23, 440.00, 523.25, 261.63], // F
        &[392.00, 493.88, 587.33, 293.66], // G
        &[261.63, 329.63, 392.00, 523.25], // C
    ];
    let mut pcm = Vec::with_capacity(rate * 8);
    for chord in chords {
        for i in 0..(rate * 2) {
            let t = i as f32 / rate as f32;
            let sample: f32 = chord.iter().map(|&hz| (TAU * hz * t).sin() * 0.2).sum();
            pcm.push(sample);
        }
    }

    let analysis = analyze_pcm(
        22_050,
        1,
        pcm.into_iter(),
        None,
        FileStamp::default(),
        &Cancel::new(),
    )
    .expect("和弦素材应当分析成功");

    let key = analysis.key.expect("有调性素材应当检出调性");
    assert_eq!(key.tonic, 0, "C 大调主音应为 0，实测 {key:?}");
    assert_eq!(key.mode, MusicalMode::Major);
    assert_eq!(key.camelot_code(), "8B");
}

#[test]
fn cache_invalidates_on_stamp_or_version_change_and_ignores_corrupt_json() {
    let dir = tempfile::tempdir().expect("临时目录");
    let cache = AnalysisCache::new(dir.path().to_path_buf());
    let id = TrackId(42);
    let stamp = FileStamp {
        modified: Some(1_700_000_000_000),
        size: Some(12_345),
    };
    let (pcm, _) = click_track(124.0, 8.0, 0.2, 0.0);
    let mut analysis =
        analyze_pcm(22_050, 1, pcm.into_iter(), None, stamp, &Cancel::new()).expect("分析成功");

    cache.save(id, &analysis).expect("写缓存");
    assert_eq!(cache.load(id, stamp), Some(analysis.clone()));

    // 文件大小变动即未命中。
    let changed = FileStamp {
        modified: stamp.modified,
        size: Some(99_999),
    };
    assert_eq!(cache.load(id, changed), None, "文件指纹变动必须失效");

    // 版本号变动即未命中。
    analysis.version = ANALYSIS_VERSION + 1;
    cache.save(id, &analysis).expect("覆盖旧版本");
    assert_eq!(cache.load(id, stamp), None, "版本号不符必须失效");

    // 坏 JSON 返回 None 而不是 panic。
    let corrupt_path = dir.path().join(format!("{id}.json"));
    std::fs::write(&corrupt_path, "{坏掉的 json").expect("写入坏缓存");
    assert_eq!(cache.load(id, stamp), None, "损坏 JSON 应视为未命中");
}

#[test]
fn analyze_file_reads_wav_through_track_decoder() {
    let dir = tempfile::tempdir().expect("临时目录");
    let (pcm, _) = click_track(128.0, 10.0, 0.25, 2.0);
    let path = write_mono_wav(dir.path(), 22_050, &pcm);

    let analysis = analyze_file(&path, None, &Cancel::new()).expect("WAV 文件应当分析成功");
    assert!(
        (analysis.bpm - 128.0).abs() <= 0.5,
        "WAV 128 BPM 实测 {:.2}",
        analysis.bpm
    );
    assert!(analysis.stamp.size.is_some());
}

fn write_mono_wav(dir: &std::path::Path, rate: u32, pcm: &[f32]) -> std::path::PathBuf {
    let data_len = (pcm.len() * 2) as u32;
    let mut bytes = Vec::with_capacity(44 + data_len as usize);
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&(36 + data_len).to_le_bytes());
    bytes.extend_from_slice(b"WAVE");
    bytes.extend_from_slice(b"fmt ");
    bytes.extend_from_slice(&16u32.to_le_bytes());
    bytes.extend_from_slice(&1u16.to_le_bytes());
    bytes.extend_from_slice(&1u16.to_le_bytes());
    bytes.extend_from_slice(&rate.to_le_bytes());
    bytes.extend_from_slice(&(rate * 2).to_le_bytes());
    bytes.extend_from_slice(&2u16.to_le_bytes());
    bytes.extend_from_slice(&16u16.to_le_bytes());
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&data_len.to_le_bytes());
    for &s in pcm {
        let q = (s.clamp(-1.0, 1.0) * 32_767.0).round() as i16;
        bytes.extend_from_slice(&q.to_le_bytes());
    }
    let path = dir.join("click.wav");
    std::fs::write(&path, bytes).expect("写 WAV");
    path
}

#[test]
fn vocal_detector_and_rgb_waveform_distinguish_center_vocals_from_instrumental_sections() {
    // 合成 24 秒 120 BPM 立体声素材，前 8 秒纯低频底鼓与侧置宽噪声，中 8 秒叠加中置人声泛音，后 8 秒回落器乐。
    let rate = 22_050usize;
    let seconds = 24.0_f32;
    let frames = (seconds * rate as f32).round() as usize;
    let mut interleaved = vec![0.0_f32; frames * 2];
    let mut rng = Rng::new(0x2026_cafe);

    for i in 0..frames {
        let t = i as f32 / rate as f32;
        let beat_phase = (t * 2.0).fract();
        let kick = if beat_phase < 0.08 {
            (TAU * 75.0 * beat_phase).sin() * (-beat_phase * 55.0).exp() * 0.45
        } else {
            0.0
        };
        let side_noise = rng.next_f32() * 0.14;
        // 中段 8..16 s 叠加中置人声基频与谐波（440 Hz + 880 Hz + 1320 Hz）。
        let vocal = if (8.0..16.0).contains(&t) {
            let vib = 1.0 + 0.008 * (TAU * 5.2 * t).sin();
            (TAU * 440.0 * vib * t).sin() * 0.32
                + (TAU * 880.0 * vib * t).sin() * 0.22
                + (TAU * 1320.0 * vib * t).sin() * 0.12
        } else {
            0.0
        };
        interleaved[i * 2] = (kick + vocal + side_noise).clamp(-1.0, 1.0);
        interleaved[i * 2 + 1] = (kick + vocal - side_noise).clamp(-1.0, 1.0);
    }

    let analysis = analyze_pcm(
        rate as u32,
        2,
        interleaved.into_iter(),
        Some(120.0),
        FileStamp::default(),
        &Cancel::new(),
    )
    .expect("立体声合成素材应当分析成功");

    assert!(
        analysis.waveform_rgb.len() >= 230,
        "24 秒 100 ms 彩色波形至少 230 格，实测 {}",
        analysis.waveform_rgb.len()
    );
    assert!(!analysis.bar_vocal.is_empty(), "应产出逐小节人声存在度");

    let intro_vocal = analysis.vocal_at_time(4.0);
    let chorus_vocal = analysis.vocal_at_time(12.0);
    let outro_vocal = analysis.vocal_at_time(20.0);
    assert!(
        chorus_vocal > intro_vocal + 0.30 && chorus_vocal > outro_vocal + 0.30,
        "中段人声存在度（{chorus_vocal:.2}）应显著高于纯器乐前后段（{intro_vocal:.2} / {outro_vocal:.2}）"
    );
    assert!(
        (analysis.vocal_start - 8.0).abs() <= 2.5,
        "人声起点应落在 8 s 附近，实测 {:.2} s",
        analysis.vocal_start
    );
    assert!(
        (analysis.vocal_end - 16.0).abs() <= 2.5,
        "人声终点应落在 16 s 附近，实测 {:.2} s",
        analysis.vocal_end
    );
}
