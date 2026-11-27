//! 批量评估工具，扫描目录中带 BPM 标签的曲目，汇报 Acc1（±4%）与 Acc2（允许倍频或半频）准确率。
//! 用法：`cargo run -p capstan-analysis --release --example bpm_eval -- <music-dir>`

use std::path::PathBuf;
use std::time::Instant;

use capstan_analysis::{Cancel, analyze_file};
use capstan_core::{collect_audio_paths, read_track};

fn main() {
    let Some(dir) = std::env::args_os().nth(1).map(PathBuf::from) else {
        eprintln!("用法：bpm_eval <音乐目录>");
        std::process::exit(2);
    };

    let paths = collect_audio_paths(&[dir]);
    let tagged: Vec<_> = paths
        .into_iter()
        .filter_map(|path| read_track(&path).ok())
        .filter_map(|track| track.bpm_tag.map(|bpm| (track, bpm)))
        .collect();

    if tagged.is_empty() {
        println!("目录中没有带 BPM 标签的曲目");
        return;
    }

    let mut acc1 = 0usize;
    let mut acc2 = 0usize;
    let mut evaluated = 0usize;
    let started = Instant::now();

    for (track, truth) in &tagged {
        // 评估纯音频估计准确率，故意不传入标签先验。
        match analyze_file(&track.path, None, &Cancel::new()) {
            Ok(analysis) => {
                evaluated += 1;
                let est = analysis.bpm;
                let hit1 = ((est - truth) / truth).abs() <= 0.04;
                let hit2 = hit1
                    || ((est - truth * 2.0) / (truth * 2.0)).abs() <= 0.04
                    || ((est - truth * 0.5) / (truth * 0.5)).abs() <= 0.04;
                if hit1 {
                    acc1 += 1;
                }
                if hit2 {
                    acc2 += 1;
                }
                if !hit1 {
                    println!(
                        "[偏差] {} | 标签 {truth:.1} -> 估计 {est:.1}（置信 {:.2}）",
                        track.title, analysis.bpm_confidence
                    );
                }
            }
            Err(err) => {
                eprintln!("[跳过] {}：{err}", track.title);
            }
        }
    }

    if evaluated > 0 {
        println!(
            "完成 {evaluated}/{} 首，耗时 {:.2} s | Acc1 {:.1}% | Acc2 {:.1}%",
            tagged.len(),
            started.elapsed().as_secs_f64(),
            acc1 as f64 / evaluated as f64 * 100.0,
            acc2 as f64 / evaluated as f64 * 100.0,
        );
    }
}
