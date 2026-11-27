//! 过渡自动化曲线。采用 Fritsch–Carlson 分段单调三次插值，控制点之间绝不过冲。

use std::f32::consts::FRAC_PI_2;

use serde::{Deserialize, Serialize};

/// 预设曲线形态。所有预设均采样为控制点集合，与自定义曲线共用同一求值器。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum CurvePreset {
    Linear,
    #[default]
    EqualPower,
    SCurve,
    Exponential,
    Logarithmic,
    Custom,
}

impl CurvePreset {
    /// 供界面列出的内置预设（不含 `Custom`）。
    pub const BUILTINS: [Self; 5] = [
        Self::EqualPower,
        Self::Linear,
        Self::SCurve,
        Self::Exponential,
        Self::Logarithmic,
    ];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Linear => "线性",
            Self::EqualPower => "等功率",
            Self::SCurve => "S 形",
            Self::Exponential => "指数",
            Self::Logarithmic => "对数",
            Self::Custom => "自定义",
        }
    }
}

/// 曲线控制点。`t` 为归一化过渡进度（0..=1），`value` 为目标量（线性增益或 dB）。
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct ControlPoint {
    pub t: f32,
    pub value: f32,
}

impl ControlPoint {
    pub const fn new(t: f32, value: f32) -> Self {
        Self { t, value }
    }
}

/// 分段单调三次插值曲线。空点集表示不施加该项自动化（如不做低频滤波）。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Automation {
    points: Vec<ControlPoint>,
}

/// 预设采样点数。17 点配合单调三次插值可将等功率误差压在 1e-4 以内。
const PRESET_SAMPLES: usize = 17;

impl Automation {
    /// 构造曲线并按 `t` 升序规范化，剔除非法浮点与重复横坐标。
    pub fn new(mut points: Vec<ControlPoint>) -> Self {
        points.retain(|p| p.t.is_finite() && p.value.is_finite());
        for p in &mut points {
            p.t = p.t.clamp(0.0, 1.0);
        }
        points.sort_by(|a, b| a.t.total_cmp(&b.t));

        let mut deduped: Vec<ControlPoint> = Vec::with_capacity(points.len());
        for p in points {
            if let Some(last) = deduped.last_mut()
                && (p.t - last.t).abs() < 1e-5
            {
                last.value = p.value;
            } else {
                deduped.push(p);
            }
        }
        Self { points: deduped }
    }

    pub fn empty() -> Self {
        Self { points: Vec::new() }
    }

    pub fn flat(value: f32) -> Self {
        Self::new(vec![
            ControlPoint::new(0.0, value),
            ControlPoint::new(1.0, value),
        ])
    }

    pub fn is_empty(&self) -> bool {
        self.points.is_empty()
    }

    pub fn points(&self) -> &[ControlPoint] {
        &self.points
    }

    /// 生成前曲（A）的淡出音量曲线（1 → 0）。
    pub fn preset_out(preset: CurvePreset) -> Self {
        sample_preset(|t| eval_preset_out(preset, t))
    }

    /// 生成后曲（B）的淡入音量曲线（0 → 1）。
    pub fn preset_in(preset: CurvePreset) -> Self {
        sample_preset(|t| eval_preset_in(preset, t))
    }

    /// 前曲（A）的低频交换曲线，前半段维持 0 dB，在交换窗内切至 `cut_db`。
    pub fn low_swap_out(swap_t: f32, half_width: f32, cut_db: f32) -> Self {
        let a = (swap_t - half_width).clamp(0.02, 0.96);
        let b = (swap_t + half_width).clamp(a + 0.02, 0.99);
        Self::new(vec![
            ControlPoint::new(0.0, 0.0),
            ControlPoint::new(a, 0.0),
            ControlPoint::new(b, cut_db),
            ControlPoint::new(1.0, cut_db),
        ])
    }

    /// 后曲（B）的低频交换曲线，前半段压至 `cut_db`，在交换窗内升回 0 dB。
    pub fn low_swap_in(swap_t: f32, half_width: f32, cut_db: f32) -> Self {
        let a = (swap_t - half_width).clamp(0.02, 0.96);
        let b = (swap_t + half_width).clamp(a + 0.02, 0.99);
        Self::new(vec![
            ControlPoint::new(0.0, cut_db),
            ControlPoint::new(a, cut_db),
            ControlPoint::new(b, 0.0),
            ControlPoint::new(1.0, 0.0),
        ])
    }

    /// 分段斜坡曲线，`[0, t0]` 保持 `v0`，`[t0, t1]` 平滑过渡到 `v1`，`[t1, 1]` 保持 `v1`。
    pub fn ramp(t0: f32, v0: f32, t1: f32, v1: f32) -> Self {
        let start = t0.clamp(0.0, 0.98);
        let end = t1.clamp(start + 0.02, 1.0);
        let mut pts = Vec::with_capacity(4);
        pts.push(ControlPoint::new(0.0, v0));
        if start > 0.01 {
            pts.push(ControlPoint::new(start, v0));
        }
        if end < 0.99 {
            pts.push(ControlPoint::new(end, v1));
        }
        pts.push(ControlPoint::new(1.0, v1));
        Self::new(pts)
    }

    /// 脉冲窗曲线，从 0 升至 `peak`，在 `[hold_start, hold_end]` 维持后回落至 0。
    pub fn pulse(
        rise_start: f32,
        hold_start: f32,
        hold_end: f32,
        fall_end: f32,
        peak: f32,
    ) -> Self {
        let t0 = rise_start.clamp(0.0, 0.92);
        let t1 = hold_start.clamp(t0 + 0.02, 0.95);
        let t2 = hold_end.clamp(t1 + 0.02, 0.97);
        let t3 = fall_end.clamp(t2 + 0.02, 1.0);
        let mut pts = Vec::with_capacity(6);
        pts.push(ControlPoint::new(0.0, 0.0));
        if t0 > 0.01 {
            pts.push(ControlPoint::new(t0, 0.0));
        }
        pts.push(ControlPoint::new(t1, peak));
        pts.push(ControlPoint::new(t2, peak));
        if t3 < 0.99 {
            pts.push(ControlPoint::new(t3, 0.0));
        }
        pts.push(ControlPoint::new(1.0, 0.0));
        Self::new(pts)
    }

    /// 移动指定控制点。首尾两点的横坐标固定在 0 与 1，内部点受限于邻点之间。
    pub fn move_point(&mut self, index: usize, t: f32, value: f32, min_val: f32, max_val: f32) {
        let len = self.points.len();
        if index >= len {
            return;
        }
        let clamped_v = value.clamp(min_val, max_val);
        let clamped_t = if index == 0 {
            0.0
        } else if index + 1 == len {
            1.0
        } else {
            let lo = self.points[index - 1].t + 0.01;
            let hi = self.points[index + 1].t - 0.01;
            t.clamp(lo.min(hi), hi.max(lo))
        };
        self.points[index] = ControlPoint::new(clamped_t, clamped_v);
    }

    /// 在 `t` 处插入一个控制点，返回新点下标。若与既有控制点过近则就地更新。
    pub fn insert_point(&mut self, t: f32, value: f32, min_val: f32, max_val: f32) -> usize {
        let t = t.clamp(0.0, 1.0);
        let value = value.clamp(min_val, max_val);
        if let Some(idx) = self.points.iter().position(|p| (p.t - t).abs() < 0.015) {
            self.points[idx].value = value;
            return idx;
        }
        let pos = self.points.partition_point(|p| p.t < t);
        self.points.insert(pos, ControlPoint::new(t, value));
        pos
    }

    /// 移除内部控制点。首尾端点不得移除。
    pub fn remove_point(&mut self, index: usize) -> bool {
        if index == 0 || index + 1 >= self.points.len() {
            return false;
        }
        self.points.remove(index);
        true
    }

    /// 预计算切线供音频回调无分配求值。
    pub fn compile(&self) -> CompiledAutomation {
        CompiledAutomation::from_points(&self.points)
    }

    /// 直接求值。界面绘图与测试调用此入口；实时音频路径应使用 [`CompiledAutomation`]。
    pub fn eval(&self, t: f32) -> f32 {
        self.compile().eval(t)
    }
}

/// 预计算好 Fritsch–Carlson 切线的曲线求值器，音频回调内零分配运行。
#[derive(Clone, Debug)]
pub struct CompiledAutomation {
    points: Vec<ControlPoint>,
    tangents: Vec<f32>,
}

impl CompiledAutomation {
    pub fn from_points(points: &[ControlPoint]) -> Self {
        let n = points.len();
        if n <= 1 {
            return Self {
                points: points.to_vec(),
                tangents: vec![0.0; n],
            };
        }

        let mut slopes = Vec::with_capacity(n - 1);
        for i in 0..(n - 1) {
            let dx = points[i + 1].t - points[i].t;
            let dy = points[i + 1].value - points[i].value;
            slopes.push(if dx > 1e-6 { dy / dx } else { 0.0 });
        }

        let mut tangents = vec![0.0_f32; n];
        if n == 2 {
            tangents[0] = slopes[0];
            tangents[1] = slopes[0];
        } else {
            // PCHIP 端点非对称三点差分，兼顾单调性与端点导数精度。
            tangents[0] = endpoint_tangent(
                points[1].t - points[0].t,
                points[2].t - points[1].t,
                slopes[0],
                slopes[1],
            );
            tangents[n - 1] = endpoint_tangent(
                points[n - 1].t - points[n - 2].t,
                points[n - 2].t - points[n - 3].t,
                slopes[n - 2],
                slopes[n - 3],
            );
        }

        for i in 1..(n - 1) {
            if slopes[i - 1] * slopes[i] <= 0.0 {
                tangents[i] = 0.0;
            } else {
                tangents[i] = f32::midpoint(slopes[i - 1], slopes[i]);
            }
        }

        // Fritsch–Carlson 单调性约束，防止三次样条在陡峭段产生过冲。
        for (i, &d) in slopes.iter().enumerate() {
            if d.abs() <= 1e-7 {
                tangents[i] = 0.0;
                tangents[i + 1] = 0.0;
            } else {
                let alpha = tangents[i] / d;
                let beta = tangents[i + 1] / d;
                let mag2 = alpha * alpha + beta * beta;
                if mag2 > 9.0 {
                    let tau = 3.0 / mag2.sqrt();
                    tangents[i] = tau * alpha * d;
                    tangents[i + 1] = tau * beta * d;
                }
            }
        }

        Self {
            points: points.to_vec(),
            tangents,
        }
    }

    /// 在 `t ∈ [0, 1]` 处求值。空曲线返回 0.0。
    #[inline]
    pub fn eval(&self, t: f32) -> f32 {
        let n = self.points.len();
        if n == 0 {
            return 0.0;
        }
        if n == 1 || t <= self.points[0].t {
            return self.points[0].value;
        }
        if t >= self.points[n - 1].t {
            return self.points[n - 1].value;
        }

        let hi = self.points.partition_point(|p| p.t <= t).min(n - 1);
        let lo = hi.saturating_sub(1);
        let p0 = self.points[lo];
        let p1 = self.points[hi];
        let h = p1.t - p0.t;
        if h <= 1e-6 {
            return p1.value;
        }
        let u = ((t - p0.t) / h).clamp(0.0, 1.0);
        let u2 = u * u;
        let u3 = u2 * u;

        let h00 = 2.0 * u3 - 3.0 * u2 + 1.0;
        let h10 = u3 - 2.0 * u2 + u;
        let h01 = -2.0 * u3 + 3.0 * u2;
        let h11 = u3 - u2;

        h00 * p0.value + h10 * h * self.tangents[lo] + h01 * p1.value + h11 * h * self.tangents[hi]
    }
}

/// PCHIP 端点切线估计，保持与首段割线同号且不超过其三倍斜率，保证区间单调无过冲。
fn endpoint_tangent(h0: f32, h1: f32, d0: f32, d1: f32) -> f32 {
    let mut m = ((2.0 * h0 + h1) * d0 - h0 * d1) / (h0 + h1).max(1e-6);
    if m.signum() != d0.signum() {
        m = 0.0;
    } else if d0.signum() != d1.signum() && m.abs() > 3.0 * d0.abs() {
        m = 3.0 * d0;
    }
    m
}

fn sample_preset(f: impl Fn(f32) -> f32) -> Automation {
    let points = (0..PRESET_SAMPLES)
        .map(|i| {
            let t = i as f32 / (PRESET_SAMPLES - 1) as f32;
            ControlPoint::new(t, f(t))
        })
        .collect();
    Automation::new(points)
}

fn eval_preset_out(preset: CurvePreset, t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    match preset {
        CurvePreset::Linear => 1.0 - t,
        CurvePreset::EqualPower | CurvePreset::Custom => (t * FRAC_PI_2).cos(),
        CurvePreset::SCurve => 1.0 - (3.0 * t * t - 2.0 * t * t * t),
        CurvePreset::Exponential => (1.0 - t).powi(2),
        CurvePreset::Logarithmic => 1.0 - t * t,
    }
}

fn eval_preset_in(preset: CurvePreset, t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    match preset {
        CurvePreset::Linear => t,
        CurvePreset::EqualPower | CurvePreset::Custom => (t * FRAC_PI_2).sin(),
        CurvePreset::SCurve => 3.0 * t * t - 2.0 * t * t * t,
        CurvePreset::Exponential => t * t,
        CurvePreset::Logarithmic => 1.0 - (1.0 - t).powi(2),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn equal_power_preserves_constant_power_across_transition() {
        let a = Automation::preset_out(CurvePreset::EqualPower).compile();
        let b = Automation::preset_in(CurvePreset::EqualPower).compile();

        for i in 0..=100 {
            let t = i as f32 / 100.0;
            let power = a.eval(t).powi(2) + b.eval(t).powi(2);
            assert!(
                (power - 1.0).abs() <= 1e-3,
                "t={t:.2} 处等功率偏差超限：a²+b²={power}"
            );
        }
    }

    #[test]
    fn monotone_cubic_never_overshoots_step_like_control_points() {
        let curve = Automation::new(vec![
            ControlPoint::new(0.0, 0.0),
            ControlPoint::new(0.45, 0.0),
            ControlPoint::new(0.55, -24.0),
            ControlPoint::new(1.0, -24.0),
        ])
        .compile();

        assert_eq!(curve.eval(0.0), 0.0);
        assert_eq!(curve.eval(1.0), -24.0);
        for i in 0..=200 {
            let t = i as f32 / 200.0;
            let v = curve.eval(t);
            assert!(
                (-24.0 - 1e-5..=1e-5).contains(&v),
                "单调三次插值在 t={t:.3} 处产生过冲：{v}"
            );
        }
    }

    #[test]
    fn endpoints_are_exact_for_all_presets() {
        for preset in CurvePreset::BUILTINS {
            let a = Automation::preset_out(preset);
            let b = Automation::preset_in(preset);
            assert!((a.eval(0.0) - 1.0).abs() < 1e-6);
            assert!(a.eval(1.0).abs() < 1e-6);
            assert!(b.eval(0.0).abs() < 1e-6);
            assert!((b.eval(1.0) - 1.0).abs() < 1e-6);
        }
    }
}
