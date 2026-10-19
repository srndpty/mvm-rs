//! 式の標本化（plot の幅の 4 倍の点）と、Draw の途中で見せる範囲。
//!
//! 隣り合う 2 点の間は、中点の値が直線から plot の高さの半画素より離れていない・plot の高さの 1/8 より
//! 跳ねていない・両端と中点が有限、のときだけ線でつなぐ（漸近線や不連続点を結ばない）。

use crate::graph::expression::{EvaluationStatus, Expression};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Point {
    pub x: f64,
    pub y: f64,
}

pub type Segment = Vec<Point>;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SamplingRequest {
    pub domain_min: f64,
    pub domain_max: f64,
    pub x_min: f64,
    pub x_max: f64,
    pub y_min: f64,
    pub y_max: f64,
    pub plot_width: f64,
    pub plot_height: f64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SamplingStatus {
    Success,
    ValidEmptyCurve,
    NoFiniteSamples,
    InvalidRequest,
    SamplingBudgetExceeded,
    EvaluationFailure,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Geometry {
    pub domain_min: f64,
    pub domain_max: f64,
    pub segments: Vec<Segment>,
}

const MAXIMUM_SAMPLES: f64 = 32_768.0;

/// Liang–Barsky。境界への一点接触は線ではない。
fn clip_edge(a: &mut Point, b: &mut Point, r: &SamplingRequest) -> bool {
    let dx = b.x - a.x;
    let dy = b.y - a.y;
    if !dx.is_finite() || !dy.is_finite() {
        return false;
    }
    let (mut low, mut high) = (0.0f64, 1.0f64);
    let mut cut = |p: f64, q: f64| {
        if p == 0.0 {
            return q >= 0.0;
        }
        let t = q / p;
        if p < 0.0 {
            low = low.max(t);
        } else {
            high = high.min(t);
        }
        low < high
    };
    if !cut(-dx, a.x - r.x_min)
        || !cut(dx, r.x_max - a.x)
        || !cut(-dy, a.y - r.y_min)
        || !cut(dy, r.y_max - a.y)
    {
        return false;
    }
    // std::lerp と同じく、t = 0 と t = 1 では端点そのものを返す（次の辺の始点と一致させる）。
    let lerp = |from: f64, to: f64, t: f64| {
        if t == 0.0 {
            from
        } else if t == 1.0 {
            to
        } else {
            from + (to - from) * t
        }
    };
    let begin = Point {
        x: lerp(a.x, b.x, low).clamp(r.x_min, r.x_max),
        y: lerp(a.y, b.y, low).clamp(r.y_min, r.y_max),
    };
    let end = Point {
        x: lerp(a.x, b.x, high).clamp(r.x_min, r.x_max),
        y: lerp(a.y, b.y, high).clamp(r.y_min, r.y_max),
    };
    *a = begin;
    *b = end;
    a != b && a.x.is_finite() && a.y.is_finite() && b.x.is_finite() && b.y.is_finite()
}

pub fn sample(expression: &Expression, r: &SamplingRequest) -> (SamplingStatus, Geometry) {
    let empty = Geometry {
        domain_min: 0.0,
        domain_max: 0.0,
        segments: Vec::new(),
    };
    let values = [
        r.domain_min,
        r.domain_max,
        r.x_min,
        r.x_max,
        r.y_min,
        r.y_max,
        r.plot_width,
        r.plot_height,
    ];
    let span = r.y_max - r.y_min;
    if values.iter().any(|value| !value.is_finite())
        || !(r.x_min < r.x_max && r.y_min < r.y_max && r.domain_min < r.domain_max)
        || !(r.x_max - r.x_min).is_finite()
        || !span.is_finite()
        || r.plot_width <= 0.0
        || r.plot_height <= 0.0
    {
        return (SamplingStatus::InvalidRequest, empty);
    }
    let low = r.domain_min.max(r.x_min);
    let high = r.domain_max.min(r.x_max);
    if low >= high {
        return (SamplingStatus::InvalidRequest, empty);
    }
    let density = (4.0 * r.plot_width).ceil();
    if !density.is_finite() || density > MAXIMUM_SAMPLES {
        return (SamplingStatus::SamplingBudgetExceeded, empty);
    }
    if expression.node_count() == 0 {
        return (SamplingStatus::EvaluationFailure, empty);
    }
    let count = density as usize;
    let mut geometry = Geometry {
        domain_min: low,
        domain_max: high,
        segments: Vec::new(),
    };
    let lerp = |t: f64| low + (high - low) * t;
    let mut previous = expression.evaluate(low);
    let mut previous_x = low;
    let mut drawable = false;
    let mut connected = false;
    for index in 1..=count {
        let x = lerp(index as f64 / count as f64);
        let value = expression.evaluate(x);
        let mut edge =
            previous.status == EvaluationStatus::Finite && value.status == EvaluationStatus::Finite;
        if edge {
            let middle = expression.evaluate(0.5 * (previous_x + x));
            if middle.status != EvaluationStatus::Finite {
                edge = false;
            } else {
                let jump = ((value.value - previous.value) / span).abs();
                let deviation =
                    ((middle.value - 0.5 * (previous.value + value.value)) / span).abs();
                // 跳ねた・中点が直線から離れた辺は結ばない（漸近線や不連続点）。
                let jumped = !jump.is_finite() || jump > 0.125;
                let bent = !deviation.is_finite() || deviation > 0.5 / r.plot_height;
                if jumped || bent {
                    edge = false;
                }
            }
        }
        let mut a = Point {
            x: previous_x,
            y: previous.value,
        };
        let mut b = Point { x, y: value.value };
        if !edge || a == b {
            connected = false;
        } else {
            drawable = true;
            if !clip_edge(&mut a, &mut b, r) {
                connected = false;
            } else {
                match geometry.segments.last_mut() {
                    Some(segment) if connected && segment.last() == Some(&a) => segment.push(b),
                    _ => geometry.segments.push(vec![a, b]),
                }
                connected = true;
            }
        }
        previous = value;
        previous_x = x;
    }
    let status = if !drawable {
        SamplingStatus::NoFiniteSamples
    } else if geometry.segments.is_empty() {
        SamplingStatus::ValidEmptyCurve
    } else {
        SamplingStatus::Success
    };
    (status, geometry)
}

/// Draw の frame `frame`（0..frames）で見せる範囲。終わった後は全体。
pub fn reveal(input: &Geometry, frame: i64, frames: i64) -> Geometry {
    if frames == 0 || frame >= frames {
        return input.clone();
    }
    let mut out = Geometry {
        domain_min: input.domain_min,
        domain_max: input.domain_max,
        segments: Vec::new(),
    };
    if frame <= 0 || frames < 0 {
        return out;
    }
    let boundary =
        input.domain_min + (input.domain_max - input.domain_min) * (frame as f64 / frames as f64);
    for segment in &input.segments {
        let mut visible = Vec::new();
        for (index, point) in segment.iter().enumerate() {
            if point.x <= boundary {
                visible.push(*point);
            } else {
                if index > 0 && segment[index - 1].x < boundary {
                    let a = segment[index - 1];
                    let t = (boundary - a.x) / (point.x - a.x);
                    visible.push(Point {
                        x: boundary,
                        y: a.y + (point.y - a.y) * t,
                    });
                }
                break;
            }
        }
        if visible.len() > 1 {
            out.segments.push(visible);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::expression::compile;

    fn request() -> SamplingRequest {
        SamplingRequest {
            domain_min: -5.0,
            domain_max: 5.0,
            x_min: -5.0,
            x_max: 5.0,
            y_min: -5.0,
            y_max: 5.0,
            plot_width: 100.0,
            plot_height: 100.0,
        }
    }

    #[test]
    fn a_parabola_is_one_clipped_segment() {
        let expression = compile("x^2").expression;
        let (status, geometry) = sample(&expression, &request());
        assert_eq!(status, SamplingStatus::Success);
        assert_eq!(geometry.segments.len(), 1, "放物線が途切れました");
        let segment = &geometry.segments[0];
        // y = 5 で切られるので、x は ±sqrt(5) の付近で始まり終わる。
        assert!(
            (segment[0].x + 5f64.sqrt()).abs() < 0.05,
            "{:?}",
            segment[0]
        );
        assert!((segment.last().unwrap().x - 5f64.sqrt()).abs() < 0.05);
        assert!(segment.iter().all(|point| point.y <= 5.0 && point.y >= 0.0));
    }

    #[test]
    fn a_pole_splits_the_curve_instead_of_joining_it() {
        let expression = compile("1 / x").expression;
        let (status, geometry) = sample(&expression, &request());
        assert_eq!(status, SamplingStatus::Success);
        assert_eq!(geometry.segments.len(), 2, "漸近線をまたいで結びました");
        assert!(geometry.segments[0].iter().all(|point| point.x < 0.0));
        assert!(geometry.segments[1].iter().all(|point| point.x > 0.0));
    }

    #[test]
    fn nothing_finite_and_off_screen_curves_are_distinguished() {
        let (status, _) = sample(&compile("sqrt(-1 - x^2)").expression, &request());
        assert_eq!(status, SamplingStatus::NoFiniteSamples);
        let (status, geometry) = sample(&compile("x + 100").expression, &request());
        assert_eq!(status, SamplingStatus::ValidEmptyCurve);
        assert!(geometry.segments.is_empty());
        let mut bad = request();
        bad.y_min = 10.0;
        assert_eq!(
            sample(&compile("x").expression, &bad).0,
            SamplingStatus::InvalidRequest
        );
    }

    #[test]
    fn reveal_cuts_at_the_progress_boundary() {
        let (_, geometry) = sample(&compile("x").expression, &request());
        let half = reveal(&geometry, 5, 10);
        let last = half.segments[0].last().copied().unwrap();
        assert!(
            (last.x - 0.0).abs() < 1e-9 && (last.y - 0.0).abs() < 1e-9,
            "{last:?}"
        );
        assert!(reveal(&geometry, 0, 10).segments.is_empty());
        assert_eq!(reveal(&geometry, 10, 10), geometry);
    }
}
