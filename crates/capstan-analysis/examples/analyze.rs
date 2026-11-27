//! 单首曲目分析诊断工具。
//! 用法：`cargo run -p capstan-analysis --release --example analyze -- <audio-file>`

use std::path::PathBuf;
use std::time::Instant;

use capstan_analysis::{Cancel, analyze_file};

fn main() {
    let Some(path) = std::env::args_os().nth(1).map(PathBuf::from) else {
        eprintln!("用法：analyze <音频文件路径>");
        std::process::exit(2);
    };

    let bpm_tag = capstan_core::read_track(&path)
        .ok()
        .and_then(|track| track.bpm_tag);
    let started = Instant::now();
    let result = analyze_file(&path, bpm_tag, &Cancel::new());
    let elapsed = started.elapsed();

    match result {
        Ok(a) => {
            println!("文件：{}", path.display());
            println!("耗时：{:.2} ms", elapsed.as_secs_f64() * 1000.0);
            println!("时长：{:.2} s", a.duration);
            if let Some(tag) = bpm_tag {
                println!("标签 BPM：{tag:.2}");
            }
            println!(
                "BPM：{:.2}（置信度 {:.2}，稳定度 {:.2}）",
                a.bpm, a.bpm_confidence, a.tempo_stability
            );
            println!(
                "节拍：{} 个，小节线相位 {}，乐句 {} 小节（偏移 {}）",
                a.beats.len(),
                a.downbeat_phase,
                a.phrase_bars,
                a.phrase_offset_bars
            );
            println!("响度：{:.2} LUFS", a.loudness_lufs);
            println!(
                "静音：片头 {:.2} s，片尾 {:.2} s；前奏止于 {:.2} s，尾奏始于 {:.2} s",
                a.lead_silence, a.tail_silence, a.intro_end, a.outro_start
            );
            if let Some(key) = a.key {
                println!("调性：{}（置信度 {:.2}）", key.label(), key.confidence);
            }
            let phrases = a.phrase_boundaries();
            let preview: Vec<String> = phrases.iter().take(8).map(|t| format!("{t:.2}s")).collect();
            println!(
                "乐句边界（前 {} 个）：{}",
                preview.len(),
                preview.join("，")
            );
        }
        Err(err) => {
            eprintln!("分析失败：{err}");
            std::process::exit(1);
        }
    }
}
