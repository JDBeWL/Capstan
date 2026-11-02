//! 等高线。缓坡上的涟漪，等值线从片的一边穿到另一边，不闭合成圈。
//! `assets/contours/draw-contours.mjs` 同源，先在网格上算一层
//! 「坡度 + 波浪」的标量场，再用 marching squares 把等值线连成折线，只留横穿整片的开放线。
//! 一片算一次就够，几何按像素尺寸缓存在线程里；换窗口大小时才重算。

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

/// 网格边长。越小线越圆，代价是采样点更多。
const CELL: f32 = 6.0;
/// 相邻两条等值线的间距。坡度项在片内大约跨两个单位，条数由此反推。
const SPACING: f32 = 20.0;
/// 每隔几条加粗一条，像地形图上的计曲线，一眼看得出坡的走向。
const INDEX_EVERY: usize = 5;
/// 整片往中心收的衰减档数。越靠边的线画得越淡，片就没有轮廓。
/// 档数够多，最外一档淡到看不见，线才不会被片边掐断。
pub const FADE_STEPS: u8 = 6;
/// 缓存几片。窗口来回改尺寸时只保留最近用过的这几份。
const CACHE_LEN: usize = 8;

/// 一条画出来的等值线，坐标是片内的局部坐标。
pub struct Run {
    pub points: Vec<(f32, f32)>,
    /// 1..=`FADE_STEPS`，越大越靠片心，画得越实。
    pub weight: u8,
    /// 计曲线，比普通线粗一点。
    pub heavy: bool,
}

/// 一片等高线。
pub struct Patch {
    pub runs: Vec<Run>,
}

/// 缓存表，像素尺寸与种子对应一片等高线。
type Cache = Vec<((i32, i32, u32), Rc<Patch>)>;

thread_local! {
    static CACHE: RefCell<Cache> = const { RefCell::new(Vec::new()) };
}

/// 取一片等高线；同样的尺寸与种子只算一次。
pub fn patch(width: f32, height: f32, seed: u32) -> Rc<Patch> {
    let key = (width.round() as i32, height.round() as i32, seed);
    CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        if let Some(index) = cache.iter().position(|(cached, _)| *cached == key) {
            let hit = cache.remove(index);
            let draw = Rc::clone(&hit.1);
            cache.push(hit);
            return draw;
        }
        let built = Rc::new(Patch::build(width, height, seed));
        cache.push((key, Rc::clone(&built)));
        if cache.len() > CACHE_LEN {
            cache.remove(0);
        }
        built
    })
}

impl Patch {
    fn build(width: f32, height: f32, seed: u32) -> Self {
        let terrain = Terrain::new(seed, width, height);
        let columns = ((width / CELL).round() as usize).clamp(8, 120);
        let rows = ((height / CELL).round() as usize).clamp(8, 120);
        let stride = columns + 1;
        let mut values = vec![0.0_f32; stride * (rows + 1)];
        let (mut low, mut high) = (f32::MAX, f32::MIN);
        for row in 0..=rows {
            for column in 0..=columns {
                let value = terrain.at(column as f32 * CELL, row as f32 * CELL);
                values[row * stride + column] = value;
                low = low.min(value);
                high = high.max(value);
            }
        }
        if high <= low {
            return Self { runs: Vec::new() };
        }

        let count = ((high - low) * width / SPACING).round().clamp(4.0, 48.0) as usize;
        let center = (width * 0.5, height * 0.5);
        let radius = (width * 0.5, height * 0.5);
        let mut runs = Vec::new();
        for step in 1..=count {
            let level = low + (high - low) * step as f32 / (count + 1) as f32;
            let heavy = step % INDEX_EVERY == 0;
            for line in trace(&values, columns, rows, level) {
                if is_scrap(&line) {
                    continue;
                }
                split_by_fade(&mut runs, &line, center, radius, heavy);
            }
        }
        Self { runs }
    }
}

/// 碎屑。太短、太挤的线，画出来只是一道划痕。
fn is_scrap(line: &[(f32, f32)]) -> bool {
    if line.len() < 5 {
        return true;
    }
    let (mut left, mut right) = (f32::MAX, f32::MIN);
    let (mut top, mut bottom) = (f32::MAX, f32::MIN);
    for &(x, y) in line {
        left = left.min(x);
        right = right.max(x);
        top = top.min(y);
        bottom = bottom.max(y);
    }
    (right - left).hypot(bottom - top) < 40.0
}

/// 按离片心的远近把一条线切成几段。同一段共用一个透明度档，片缘自然就淡没了。
fn split_by_fade(
    runs: &mut Vec<Run>,
    line: &[(f32, f32)],
    center: (f32, f32),
    radius: (f32, f32),
    heavy: bool,
) {
    let mut start = 0;
    let mut weight = weight_at(midpoint(line[0], line[1]), center, radius);
    for index in 1..line.len() {
        let next = if index + 1 < line.len() {
            weight_at(midpoint(line[index], line[index + 1]), center, radius)
        } else {
            weight
        };
        if next != weight {
            push_run(runs, &line[start..=index], weight, heavy);
            start = index;
            weight = next;
        }
    }
    push_run(runs, &line[start..], weight, heavy);
}

fn push_run(runs: &mut Vec<Run>, points: &[(f32, f32)], weight: u8, heavy: bool) {
    if weight == 0 || points.len() < 2 {
        return;
    }
    runs.push(Run {
        points: points.to_vec(),
        weight,
        heavy,
    });
}

/// 椭圆半径归一后收到零。片心一小块保持全实，剩下的一路淡到片边。
fn weight_at(at: (f32, f32), center: (f32, f32), radius: (f32, f32)) -> u8 {
    let unit = ((at.0 - center.0) / radius.0).hypot((at.1 - center.1) / radius.1);
    let fade = ((1.0 - unit) / FADE_RUN).clamp(0.0, 1.0);
    (fade * f32::from(FADE_STEPS)).ceil() as u8
}

/// 衰减占半径的比例。0.55 表示片心 45% 的范围内线是实的，其余一路淡下去。
const FADE_RUN: f32 = 0.55;

fn midpoint(from: (f32, f32), to: (f32, f32)) -> (f32, f32) {
    (from.0.midpoint(to.0), from.1.midpoint(to.1))
}

/// 格内四角中位于等值线之上的角位，决定该格内线段的连接方式。0 = 左上，1 = 右上，2 = 右下，3 = 左下。
/// 一格里最多两条线段（55 与 1010 两种对角情形）。
const SIDES: [&[(u8, u8)]; 16] = [
    &[],
    &[(2, 1)],
    &[(1, 3)],
    &[(2, 3)],
    &[(0, 3)],
    &[(0, 2), (1, 3)],
    &[(0, 1)],
    &[(0, 2)],
    &[(0, 2)],
    &[(0, 1)],
    &[(0, 3), (2, 1)],
    &[(0, 3)],
    &[(2, 3)],
    &[(1, 3)],
    &[(1, 2)],
    &[],
];

/// 边的四种位置。0 上，1 下，2 左，3 右。换成网格上的边后，横边看左右两格点，竖边看上下两格点。
fn edge_of(side: u8, column: usize, row: usize) -> (u8, usize, usize) {
    match side {
        0 => (0, column, row),
        1 => (0, column, row + 1),
        2 => (1, column, row),
        _ => (1, column + 1, row),
    }
}

/// marching squares。把所有格的线段缝成折线，只返回两端落在片边界上的开放线。
fn trace(values: &[f32], columns: usize, rows: usize, level: f32) -> Vec<Vec<(f32, f32)>> {
    let stride = columns + 1;
    let mut net = Net::default();
    for row in 0..rows {
        for column in 0..columns {
            let corners = [
                (column, row),
                (column + 1, row),
                (column + 1, row + 1),
                (column, row + 1),
            ];
            let mut case = 0;
            for (bit, (x, y)) in corners.iter().enumerate() {
                if values[y * stride + x] > level {
                    case |= 8 >> bit;
                }
            }
            for &(from, to) in SIDES[case] {
                let start = net.point(edge_of(from, column, row), values, stride, level);
                let end = net.point(edge_of(to, column, row), values, stride, level);
                net.join(start, end);
            }
        }
    }
    net.walk_open()
}

#[derive(Default)]
struct Net {
    points: Vec<(f32, f32)>,
    links: Vec<Vec<usize>>,
    /// 同一条边两个邻格都要用，按下标只插值一次，缝的时候才连得上。
    edges: HashMap<(u8, usize, usize), usize>,
}

impl Net {
    fn point(
        &mut self,
        edge: (u8, usize, usize),
        values: &[f32],
        stride: usize,
        level: f32,
    ) -> usize {
        if let Some(&index) = self.edges.get(&edge) {
            return index;
        }
        let (kind, column, row) = edge;
        let (left, right) = if kind == 0 {
            (
                values[row * stride + column],
                values[row * stride + column + 1],
            )
        } else {
            (
                values[row * stride + column],
                values[(row + 1) * stride + column],
            )
        };
        let t = if (right - left).abs() < f32::EPSILON {
            0.5
        } else {
            ((level - left) / (right - left)).clamp(0.0, 1.0)
        };
        let at = if kind == 0 {
            ((column as f32 + t) * CELL, row as f32 * CELL)
        } else {
            (column as f32 * CELL, (row as f32 + t) * CELL)
        };
        self.points.push(at);
        self.links.push(Vec::new());
        let index = self.points.len() - 1;
        self.edges.insert(edge, index);
        index
    }

    fn join(&mut self, start: usize, end: usize) {
        self.links[start].push(end);
        self.links[end].push(start);
    }

    /// 从每个端点走到另一个端点。没有端点的线是闭合圈，不画，就这么丢掉。
    fn walk_open(self) -> Vec<Vec<(f32, f32)>> {
        let mut seen = vec![false; self.points.len()];
        let mut lines = Vec::new();
        for start in 0..self.points.len() {
            if self.links[start].len() != 1 || seen[start] {
                continue;
            }
            let mut line = vec![self.points[start]];
            seen[start] = true;
            let mut here = start;
            while let Some(next) = self.links[here].iter().copied().find(|next| !seen[*next]) {
                seen[next] = true;
                line.push(self.points[next]);
                here = next;
            }
            lines.push(line);
        }
        lines
    }
}

/// 缓坡加涟漪。整体朝一个方向斜落，上面叠几条低频波，等值线于是横向流动、左右飘。
struct Terrain {
    width: f32,
    height: f32,
    slope: (f32, f32),
    waves: Vec<Wave>,
}

struct Wave {
    /// 波的朝向按「每像素几个周期」给，跟片的大小无关。
    freq: (f32, f32),
    phase: f32,
    amplitude: f32,
}

impl Terrain {
    fn new(seed: u32, width: f32, height: f32) -> Self {
        let mut rng = Rng(seed);
        let angle = rng.next() * std::f32::consts::TAU;
        let waves = (0..9)
            .map(|step| {
                let frequency = 0.8 + step as f32 * 0.75 + rng.next() * 0.5;
                let heading = rng.next() * std::f32::consts::TAU;
                Wave {
                    freq: (
                        frequency * heading.cos() / width,
                        frequency * heading.sin() / width,
                    ),
                    phase: rng.next() * std::f32::consts::TAU,
                    amplitude: 0.11 / (1.0 + step as f32 * 0.8),
                }
            })
            .collect();
        Self {
            width,
            height,
            slope: (angle.cos(), angle.sin()),
            waves,
        }
    }

    fn at(&self, x: f32, y: f32) -> f32 {
        // 先将坐标系轻微弯曲，线条才会偏移，不会笔直穿过整片。
        let bent_x = x + 0.06 * self.width * bend(y / self.height, 1.3, 1.7);
        let bent_y = y + 0.06 * self.height * bend(x / self.width, 0.9, 0.4);
        let mut value = (bent_x * self.slope.0 + bent_y * self.slope.1) / self.width;
        for wave in &self.waves {
            let travel = wave.freq.0 * bent_x + wave.freq.1 * bent_y;
            value += wave.amplitude * (std::f32::consts::TAU * travel + wave.phase).sin();
        }
        value
    }
}

fn bend(t: f32, frequency: f32, phase: f32) -> f32 {
    (std::f32::consts::TAU * frequency * t + phase).sin()
}

/// mulberry32。换个种子就换一片地形。
struct Rng(u32);

impl Rng {
    fn next(&mut self) -> f32 {
        self.0 = self.0.wrapping_add(0x6d2b_79f5);
        let mut t = self.0;
        t = (t ^ (t >> 15)).wrapping_mul(t | 1);
        t ^= t.wrapping_add((t ^ (t >> 7)).wrapping_mul(t | 0x3d));
        (t ^ (t >> 14)) as f32 / 4_294_967_296.0
    }
}

#[cfg(test)]
mod tests {
    // 不写 `use super::*`，`crate::platform::*` 带进来的 gpui `test` 宏会遮蔽内置 `#[test]`。

    #[test]
    fn patch_stays_inside_its_box() {
        let patch = super::patch(320.0, 200.0, 7);
        assert!(!patch.runs.is_empty(), "一片里应该有线");
        for run in &patch.runs {
            for &(x, y) in &run.points {
                assert!((0.0..=320.0).contains(&x), "x 越界：{x}");
                assert!((0.0..=200.0).contains(&y), "y 越界：{y}");
            }
        }
    }

    #[test]
    fn same_size_is_cached() {
        let first = super::patch(280.0, 180.0, 3);
        let again = super::patch(280.0, 180.0, 3);
        assert!(std::rc::Rc::ptr_eq(&first, &again));
    }

    #[test]
    fn every_run_is_inside_the_fade_range_and_some_are_solid() {
        let patch = super::patch(300.0, 220.0, 11);
        let mut solid = 0;
        for run in &patch.runs {
            assert!(
                (1..=super::FADE_STEPS).contains(&run.weight),
                "档位越界：{}",
                run.weight
            );
            if run.weight == super::FADE_STEPS {
                solid += 1;
            }
        }
        assert!(solid > 0, "片心应该有画实的那一档");
    }

    #[test]
    fn index_lines_are_thicker() {
        let patch = super::patch(300.0, 220.0, 5);
        let heavy = patch.runs.iter().filter(|run| run.heavy).count();
        let plain = patch.runs.iter().filter(|run| !run.heavy).count();
        assert!(heavy > 0, "每隔几条要有计曲线");
        assert!(heavy < plain, "计曲线是少数");
    }
}
