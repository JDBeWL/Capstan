//! 智能混排队列编排器。综合 BPM 兼容度（含倍速与半速）、Camelot 调性、
//! 能量走势、响度接近度与艺人及专辑多样性，为自动混音生成过渡最顺滑的播放顺序。

use std::collections::{BTreeMap, HashSet};

use capstan_core::TrackId;

use crate::model::{KeyInfo, TrackAnalysis};

/// 超过该规模时启用 BPM 分桶近邻搜索与窗口 2-opt，确保万首曲库也能在毫秒级完成。
const LARGE_QUEUE_THRESHOLD: usize = 600;
/// 常规队列 2-opt 最大迭代轮数。
const MAX_TWO_OPT_PASSES: usize = 4;
/// 大队列窗口 2-opt 的局部搜索半径。
const LARGE_TWO_OPT_WINDOW: usize = 48;

/// 参与智能混排的曲目特征快照。
#[derive(Clone, Debug)]
pub struct ArrangeItem {
    pub id: TrackId,
    pub bpm: Option<f32>,
    pub key: Option<KeyInfo>,
    /// 全曲归一化平均能量（0..=1）。
    pub energy: Option<f32>,
    /// 积分响度（LUFS），低于 -60 LUFS 时视作无效。
    pub loudness_lufs: Option<f32>,
    /// 艺术家指纹（0 表示未知），用于平分时避免同艺人扎堆。
    pub artist_hash: u64,
    /// 专辑指纹（0 表示未知），用于平分时避免同专辑连续扎堆。
    pub album_hash: u64,
    /// 是否具备稳定的节拍网格，可用于对拍混音。
    pub beat_matchable: bool,
}

impl ArrangeItem {
    /// 从完整离线分析结果构造编排特征。
    pub fn from_analysis(
        id: TrackId,
        analysis: &TrackAnalysis,
        artist: Option<&str>,
        album: Option<&str>,
    ) -> Self {
        let bpm = (analysis.bpm.is_finite() && analysis.bpm > 1.0).then_some(analysis.bpm);
        let energy = mean_energy(analysis);
        let loudness_lufs = (analysis.loudness_lufs.is_finite() && analysis.loudness_lufs > -60.0)
            .then_some(analysis.loudness_lufs);
        let beat_matchable = analysis.bpm_confidence >= 0.40
            && analysis.tempo_stability >= 0.60
            && analysis.beats.len() >= 8;
        Self {
            id,
            bpm,
            key: analysis.key,
            energy,
            loudness_lufs,
            artist_hash: hash_opt_str(artist),
            album_hash: hash_opt_str(album),
            beat_matchable,
        }
    }

    /// 尚未完成离线分析时，退回标签 BPM 与基础元数据构造特征。
    pub fn from_metadata(
        id: TrackId,
        bpm_tag: Option<f32>,
        artist: Option<&str>,
        album: Option<&str>,
    ) -> Self {
        let bpm = bpm_tag.filter(|b| b.is_finite() && *b > 1.0);
        Self {
            id,
            bpm,
            key: None,
            energy: None,
            loudness_lufs: None,
            artist_hash: hash_opt_str(artist),
            album_hash: hash_opt_str(album),
            beat_matchable: false,
        }
    }
}

/// 主动编排曲目顺序以提升相邻过渡质量。若指定了队列中的 `anchor`，则强制固定在队首。
pub fn arrange_queue(items: &[ArrangeItem], anchor: Option<TrackId>, seed: u64) -> Vec<TrackId> {
    if items.len() <= 1 {
        return items.iter().map(|item| item.id).collect();
    }

    // 去重并保持输入顺序，同时剥离锚点与无速度曲目。
    let mut dedup = HashSet::with_capacity(items.len());
    let mut unique: Vec<&ArrangeItem> = Vec::with_capacity(items.len());
    for item in items {
        if dedup.insert(item.id) {
            unique.push(item);
        }
    }
    if unique.len() <= 1 {
        return unique.into_iter().map(|item| item.id).collect();
    }

    let anchor_pos = anchor.and_then(|target| unique.iter().position(|item| item.id == target));
    let anchor_item = anchor_pos.map(|idx| unique.remove(idx));

    let mut known: Vec<&ArrangeItem> = Vec::with_capacity(unique.len());
    let mut unknown: Vec<TrackId> = Vec::new();
    for item in unique {
        if item.bpm.is_some() {
            known.push(item);
        } else {
            unknown.push(item.id);
        }
    }

    let mut result = Vec::with_capacity(items.len());
    if known.is_empty() {
        if let Some(head) = anchor_item {
            result.push(head.id);
        }
        result.extend(unknown);
        return result;
    }

    // 锚点有 BPM 时直接以它为路径起点；锚点无 BPM 时先保留在队首，再从已知集合里挑首曲。
    let mut route_items: Vec<&ArrangeItem> = Vec::with_capacity(known.len() + 1);
    let fixed_head = match anchor_item {
        Some(head) if head.bpm.is_some() => {
            route_items.push(head);
            route_items.extend(known);
            true
        }
        Some(head) => {
            result.push(head.id);
            route_items.extend(known);
            false
        }
        None => {
            route_items.extend(known);
            false
        }
    };

    let ordered_indices = if route_items.len() <= LARGE_QUEUE_THRESHOLD {
        arrange_dense(&route_items, fixed_head, seed)
    } else {
        arrange_bucketed(&route_items, fixed_head, seed)
    };

    for idx in ordered_indices {
        result.push(route_items[idx].id);
    }
    result.extend(unknown);
    result
}

/// 评估两首曲目相邻混音的对称亲和度（值越高过渡越顺滑）。
/// 函数满足 `pair_score(a, b) == pair_score(b, a)`，以保证 2-opt 反转子段时增量计算精确。
pub fn pair_score(first: &ArrangeItem, second: &ArrangeItem) -> f32 {
    let tempo = match (first.bpm, second.bpm) {
        (Some(bpm_a), Some(bpm_b)) => {
            let (delta, mult_penalty) = symmetric_tempo_delta(bpm_a, bpm_b);
            let base = if delta <= 0.03 {
                0.88 + 0.12 * (1.0 - delta / 0.03)
            } else if delta <= 0.08 {
                0.58 + 0.30 * (1.0 - (delta - 0.03) / 0.05)
            } else {
                0.58 * (-((delta - 0.08) / 0.14)).exp()
            };
            let match_bonus = if first.beat_matchable && second.beat_matchable && delta <= 0.08 {
                0.04
            } else {
                0.0
            };
            (base * mult_penalty + match_bonus).min(1.0)
        }
        _ => 0.35,
    };

    let key = match (first.key, second.key) {
        (Some(key_a), Some(key_b)) => key_a.compatibility(&key_b),
        _ => 0.50,
    };

    let energy = match (first.energy, second.energy) {
        (Some(eng_a), Some(eng_b)) => 1.0 - (eng_a - eng_b).abs().min(1.0),
        _ => 0.55,
    };

    let loudness = match (first.loudness_lufs, second.loudness_lufs) {
        (Some(lufs_a), Some(lufs_b)) => 1.0 - ((lufs_a - lufs_b).abs() / 12.0).min(1.0),
        _ => 0.60,
    };

    let mut penalty = 0.0_f32;
    if first.artist_hash != 0 && first.artist_hash == second.artist_hash {
        penalty += 0.05;
    }
    if first.album_hash != 0 && first.album_hash == second.album_hash {
        penalty += 0.03;
    }

    0.50 * tempo + 0.22 * key + 0.18 * energy + 0.10 * loudness - penalty
}

/// 常规规模队列，用贪心最近邻构建初始路径，再用 2-opt 局部反转消除末端孤岛与突兀跳变。
fn arrange_dense(items: &[&ArrangeItem], fixed_head: bool, seed: u64) -> Vec<usize> {
    let count = items.len();
    if count <= 1 {
        return (0..count).collect();
    }

    let start = if fixed_head {
        0
    } else {
        pick_opener(items, seed)
    };

    let mut visited = vec![false; count];
    let mut route = Vec::with_capacity(count);
    visited[start] = true;
    route.push(start);

    for step in 1..count {
        let prev = route[step - 1];
        let mut best_idx = 0usize;
        let mut best_val = f32::NEG_INFINITY;
        for cand in 0..count {
            if visited[cand] {
                continue;
            }
            let jitter = tie_jitter(items[prev].id, items[cand].id, seed);
            let val = pair_score(items[prev], items[cand]) + jitter;
            if val > best_val {
                best_val = val;
                best_idx = cand;
            }
        }
        visited[best_idx] = true;
        route.push(best_idx);
    }

    improve_two_opt(&mut route, items, count, MAX_TWO_OPT_PASSES);
    route
}

/// 大规模队列，按 4 BPM 宽度分桶检索近邻（含倍速与半速桶），随后执行有界窗口 2-opt。
fn arrange_bucketed(items: &[&ArrangeItem], fixed_head: bool, seed: u64) -> Vec<usize> {
    let count = items.len();
    if count <= 1 {
        return (0..count).collect();
    }

    let mut buckets: BTreeMap<i32, Vec<usize>> = BTreeMap::new();
    for (idx, item) in items.iter().enumerate() {
        buckets.entry(bpm_bucket(item.bpm)).or_default().push(idx);
    }

    let start = if fixed_head {
        0
    } else {
        pick_opener(items, seed)
    };

    let mut visited = vec![false; count];
    let mut route = Vec::with_capacity(count);
    visited[start] = true;
    route.push(start);

    let bucket_keys: Vec<i32> = buckets.keys().copied().collect();

    for step in 1..count {
        let prev = route[step - 1];
        let prev_bucket = bpm_bucket(items[prev].bpm);

        // 先搜 1×、2×、0.5× 附近桶；都已耗尽时退回按桶距向外扩圈寻找最近剩余桶。
        let mut best_idx: Option<usize> = None;
        let mut best_val = f32::NEG_INFINITY;

        let target_buckets = [
            prev_bucket,
            prev_bucket - 1,
            prev_bucket + 1,
            prev_bucket - 2,
            prev_bucket + 2,
            prev_bucket * 2,
            prev_bucket * 2 - 1,
            prev_bucket * 2 + 1,
            prev_bucket / 2,
            prev_bucket / 2 - 1,
            prev_bucket / 2 + 1,
        ];
        for key in target_buckets {
            if let Some(list) = buckets.get(&key) {
                scan_bucket_candidates(
                    list,
                    &visited,
                    items,
                    prev,
                    seed,
                    &mut best_idx,
                    &mut best_val,
                );
            }
        }

        if best_idx.is_none() {
            // 按与当前桶的距离升序探测，找到第一个仍含未访问曲目的桶即止。
            let mut by_dist = bucket_keys.clone();
            by_dist.sort_by_key(|key| (key - prev_bucket).abs());
            for key in by_dist {
                if let Some(list) = buckets.get(&key) {
                    scan_bucket_candidates(
                        list,
                        &visited,
                        items,
                        prev,
                        seed,
                        &mut best_idx,
                        &mut best_val,
                    );
                    if best_idx.is_some() {
                        break;
                    }
                }
            }
        }

        let chosen =
            best_idx.unwrap_or_else(|| visited.iter().position(|&used| !used).unwrap_or(0));
        visited[chosen] = true;
        route.push(chosen);
    }

    improve_two_opt(&mut route, items, LARGE_TWO_OPT_WINDOW, 2);
    route
}

fn scan_bucket_candidates(
    list: &[usize],
    visited: &[bool],
    items: &[&ArrangeItem],
    prev: usize,
    seed: u64,
    best_idx: &mut Option<usize>,
    best_val: &mut f32,
) {
    // 单个同速桶过大时抽样扫描，兼顾多样性与时间复杂度。
    let stride = (list.len() / 64).max(1);
    let offset = (seed as usize) % stride;
    let mut checked = 0usize;
    let mut pos = offset;
    while pos < list.len() && checked < 64 {
        let cand = list[pos];
        if !visited[cand] {
            let val = pair_score(items[prev], items[cand])
                + tie_jitter(items[prev].id, items[cand].id, seed);
            if val > *best_val {
                *best_val = val;
                *best_idx = Some(cand);
            }
            checked += 1;
        }
        pos += stride;
    }
    if checked == 0 {
        for &cand in list {
            if !visited[cand] {
                let val = pair_score(items[prev], items[cand])
                    + tie_jitter(items[prev].id, items[cand].id, seed);
                if val > *best_val {
                    *best_val = val;
                    *best_idx = Some(cand);
                }
                break;
            }
        }
    }
}

/// 2-opt 局部优化，始终锁死 `route[0]`，反转 `route[left..=right]` 若能提高总过渡分则采纳。
fn improve_two_opt(route: &mut [usize], items: &[&ArrangeItem], max_span: usize, passes: usize) {
    let len = route.len();
    if len < 4 {
        return;
    }
    for _ in 0..passes {
        let mut improved = false;
        for left in 1..(len - 1) {
            let right_limit = (left + max_span).min(len - 1);
            for right in (left + 1)..=right_limit {
                let before_a = pair_score(items[route[left - 1]], items[route[left]]);
                let after_a = pair_score(items[route[left - 1]], items[route[right]]);
                let (before_b, after_b) = if right + 1 < len {
                    (
                        pair_score(items[route[right]], items[route[right + 1]]),
                        pair_score(items[route[left]], items[route[right + 1]]),
                    )
                } else {
                    (0.0, 0.0)
                };
                if (after_a + after_b) > (before_a + before_b) + 1e-4 {
                    route[left..=right].reverse();
                    improved = true;
                }
            }
        }
        if !improved {
            break;
        }
    }
}

/// 没有固定锚点时，从中等速度区间（第 25–75 百分位）按种子挑选开场曲，避免落在极端慢曲或快曲上。
fn pick_opener(items: &[&ArrangeItem], seed: u64) -> usize {
    let mut indices: Vec<usize> = (0..items.len()).collect();
    indices.sort_by(|&a, &b| {
        items[a]
            .bpm
            .unwrap_or(120.0)
            .total_cmp(&items[b].bpm.unwrap_or(120.0))
    });
    let lo = indices.len() / 4;
    let hi = (indices.len() * 3 / 4).max(lo + 1).min(indices.len());
    let span = (hi - lo).max(1);
    let pick = lo + ((mix_u64(seed) as usize) % span);
    indices[pick]
}

/// 对称对数速度差，在 `{1×, 2×, 0.5×}` 中取最小偏差，半速与倍速施以轻微折减。
fn symmetric_tempo_delta(bpm_a: f32, bpm_b: f32) -> (f32, f32) {
    let mut best_delta = f32::INFINITY;
    let mut best_penalty = 1.0_f32;
    for (mult, penalty) in [(1.0_f32, 1.0_f32), (2.0, 0.94), (0.5, 0.94)] {
        let delta = (bpm_a / (bpm_b * mult)).ln().abs();
        if delta < best_delta {
            best_delta = delta;
            best_penalty = penalty;
        }
    }
    (best_delta, best_penalty)
}

fn bpm_bucket(bpm: Option<f32>) -> i32 {
    (bpm.unwrap_or(120.0) / 4.0).round() as i32
}

fn mean_energy(analysis: &TrackAnalysis) -> Option<f32> {
    if !analysis.bar_energy.is_empty() {
        let sum: f32 = analysis
            .bar_energy
            .iter()
            .map(|&[lo, mid, hi]| (f32::from(lo) + f32::from(mid) + f32::from(hi)) / (3.0 * 255.0))
            .sum();
        return Some((sum / analysis.bar_energy.len() as f32).clamp(0.0, 1.0));
    }
    if !analysis.beat_rms.is_empty() {
        let sum: f32 = analysis
            .beat_rms
            .iter()
            .map(|&v| f32::from(v) / 255.0)
            .sum();
        return Some((sum / analysis.beat_rms.len() as f32).clamp(0.0, 1.0));
    }
    None
}

fn hash_opt_str(value: Option<&str>) -> u64 {
    let Some(text) = value.map(str::trim).filter(|s| !s.is_empty()) else {
        return 0;
    };
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for ch in text.chars().flat_map(char::to_lowercase) {
        let mut buf = [0u8; 4];
        for byte in ch.encode_utf8(&mut buf).as_bytes() {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    hash.max(1)
}

/// 极小幅确定性抖动（±0.008），让相近候选在不同种子下能换出新鲜顺序而不破坏质量。
fn tie_jitter(first: TrackId, second: TrackId, seed: u64) -> f32 {
    let mixed = mix_u64(first.0 ^ second.0.rotate_left(17) ^ seed);
    let unit = (mixed & 0xFFFF) as f32 / 65535.0;
    (unit - 0.5) * 0.016
}

const fn mix_u64(mut value: u64) -> u64 {
    value ^= value >> 30;
    value = value.wrapping_mul(0xbf58_476d_1ce4_e5b9);
    value ^= value >> 27;
    value = value.wrapping_mul(0x94d0_49bb_1331_11eb);
    value ^ (value >> 31)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::MusicalMode;

    fn item(
        id: u64,
        bpm: Option<f32>,
        camelot: Option<(u8, char)>,
        energy: Option<f32>,
    ) -> ArrangeItem {
        ArrangeItem {
            id: TrackId(id),
            bpm,
            key: camelot.map(|(num, letter)| KeyInfo {
                tonic: 0,
                mode: if letter == 'B' {
                    MusicalMode::Major
                } else {
                    MusicalMode::Minor
                },
                camelot_num: num,
                camelot_letter: letter,
                confidence: 0.8,
            }),
            energy,
            loudness_lufs: Some(-10.0),
            artist_hash: id + 1,
            album_hash: id + 100,
            beat_matchable: bpm.is_some(),
        }
    }

    #[test]
    fn pair_score_is_strictly_symmetric() {
        let first = item(1, Some(124.0), Some((8, 'B')), Some(0.7));
        let second = item(2, Some(128.0), Some((9, 'B')), Some(0.5));
        assert!((pair_score(&first, &second) - pair_score(&second, &first)).abs() < 1e-6);
    }

    #[test]
    fn anchor_is_locked_at_head_and_clusters_flow_smoothly() {
        let items = vec![
            item(1, Some(128.0), Some((8, 'B')), Some(0.7)),
            item(2, Some(92.0), Some((3, 'A')), Some(0.3)),
            item(3, Some(129.0), Some((8, 'B')), Some(0.72)),
            item(4, Some(94.0), Some((3, 'A')), Some(0.32)),
            item(5, Some(127.0), Some((9, 'B')), Some(0.68)),
            item(6, Some(91.0), Some((2, 'A')), Some(0.28)),
        ];

        let order = arrange_queue(&items, Some(TrackId(1)), 42);
        assert_eq!(order[0], TrackId(1), "当前曲锚点必须留在首位");
        assert_eq!(order.len(), items.len());
        // 128 BPM 集群（1, 3, 5）应当连续排在一起，不与 92 BPM 集群穿插跳变。
        let fast_set: HashSet<TrackId> = [TrackId(1), TrackId(3), TrackId(5)].into_iter().collect();
        assert!(
            order[0..3].iter().all(|id| fast_set.contains(id)),
            "128 BPM 相近曲目应当聚合在前段，实测 {order:?}"
        );
    }

    #[test]
    fn harmonic_compatibility_prefers_matching_key_at_same_bpm() {
        let items = vec![
            item(1, Some(126.0), Some((8, 'B')), Some(0.6)),
            item(2, Some(126.0), Some((2, 'A')), Some(0.6)),
            item(3, Some(126.0), Some((8, 'A')), Some(0.6)),
        ];

        let order = arrange_queue(&items, Some(TrackId(1)), 7);
        assert_eq!(order[0], TrackId(1));
        assert_eq!(
            order[1],
            TrackId(3),
            "同 BPM 下应优先接调性兼容的 8A 而非冲突的 2A"
        );
    }

    #[test]
    fn half_time_bridge_is_preferred_over_unrelated_tempo() {
        let items = vec![
            item(1, Some(128.0), Some((8, 'B')), Some(0.6)),
            item(2, Some(103.0), Some((8, 'B')), Some(0.6)),
            item(3, Some(64.0), Some((8, 'B')), Some(0.6)),
        ];

        let order = arrange_queue(&items, Some(TrackId(1)), 11);
        assert_eq!(
            order[1],
            TrackId(3),
            "64 BPM 是 128 BPM 的半速，应优先于 103 BPM"
        );
    }

    #[test]
    fn unknown_bpm_tracks_stay_at_tail_without_dropping_any_id() {
        let items = vec![
            item(1, Some(120.0), None, None),
            item(2, None, None, None),
            item(3, Some(122.0), None, None),
            item(4, None, None, None),
        ];

        let order = arrange_queue(&items, Some(TrackId(1)), 5);
        assert_eq!(order, vec![TrackId(1), TrackId(3), TrackId(2), TrackId(4)]);
    }

    #[test]
    fn large_bucketed_queue_preserves_every_track_and_reduces_tempo_jumps() {
        let mut items = Vec::with_capacity(800);
        for idx in 0..800u64 {
            // 故意打乱 BPM，检验分桶编排能将相邻跳变大幅收窄。
            let bpm = 80.0 + ((idx * 37) % 90) as f32;
            items.push(item(
                idx + 1,
                Some(bpm),
                Some((((idx % 12) + 1) as u8, 'B')),
                Some(0.5),
            ));
        }

        let order = arrange_queue(&items, Some(TrackId(1)), 99);
        assert_eq!(order.len(), 800);
        let unique: HashSet<TrackId> = order.iter().copied().collect();
        assert_eq!(unique.len(), 800, "大队列编排不得丢失或重复任何曲目");
        assert_eq!(order[0], TrackId(1));

        let by_id: std::collections::HashMap<TrackId, f32> = items
            .iter()
            .filter_map(|it| it.bpm.map(|b| (it.id, b)))
            .collect();
        let avg_jump: f32 = order
            .windows(2)
            .map(|pair| {
                let (delta, _) = symmetric_tempo_delta(by_id[&pair[0]], by_id[&pair[1]]);
                delta
            })
            .sum::<f32>()
            / (order.len() - 1) as f32;
        assert!(
            avg_jump < 0.05,
            "编排后相邻平均对数速度差应远小于随机顺序，实测 {avg_jump:.4}"
        );
    }
}
