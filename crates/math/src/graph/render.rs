//! Graph の 1 frame を出力の大きさの RGBA（straight alpha）に描く。preview と書き出しが同じ関数を通す。
//!
//! 配置は C++ 版と同じ: plot は出力の中央 3/4、格子は 10 分割（#FF333A44・1 px）、軸は白（2 px）、
//! 線の太さは出力が 1920x1080 のときの px（小さい出力では縮める）。曲線は plot の内側だけに描く。
//! ラベル（x 軸・y 軸・各関数）は LaTeX で描いた mask（[`LABEL_FONT_EM`]）を帯の内側へ縮めて白で置く。
//! y 軸のラベルは 90 度回す。

use std::sync::Arc;

use mvm_project::graph_clip::{GraphClipData, GraphValidationStatus};
use mvm_project::parse_argb_color;
use tiny_skia::{
    Color, FillRule, LineCap, LineJoin, Paint, PathBuilder, Pixmap, Rect, Stroke, Transform,
};

use crate::artifact::Mask;
use crate::compose::over_pixel;
use crate::error::MathError;
use crate::graph::expression::{CompileStatus, compile};
use crate::graph::sample::{Geometry, Point, SamplingRequest, SamplingStatus, reveal, sample};

/// ラベルを描く 1 em の px（Manim の font_size 48）。
pub const LABEL_FONT_EM: i32 = 68;
const GRID_COLOR: u32 = 0xFF33_3A44;
const AXIS_COLOR: u32 = 0xFFFF_FFFF;
const LABEL_COLOR: u32 = 0xFFFF_FFFF;
const MAXIMUM_DIMENSION: u32 = 8192;

#[derive(Clone, Copy, Debug, PartialEq)]
struct Area {
    left: f64,
    top: f64,
    width: f64,
    height: f64,
}

impl Area {
    /// 帯の内側の、ラベルを置く範囲。
    fn content(self) -> Self {
        Self {
            left: self.left + self.width * 0.05,
            top: self.top + self.height * 0.1,
            width: self.width * 0.9,
            height: self.height * 0.8,
        }
    }
}

struct Curve {
    geometry: Geometry,
    color: u32,
    width: f64,
}

struct Label {
    text: String,
    area: Area,
    rotated: bool,
}

/// 標本化まで済ませた Graph。frame ごとの描画（Draw の途中）で使い回す。
pub struct GraphLayout {
    width: u32,
    height: u32,
    x_min: f64,
    x_max: f64,
    y_min: f64,
    y_max: f64,
    plot: Area,
    background: Vec<([Point; 2], u32, f64)>,
    curves: Vec<Curve>,
    labels: Vec<Label>,
    draw_frames: i64,
}

/// 線の太さの倍率（1920x1080 を 1 とする）。
fn stroke_scale(width: u32, height: u32) -> f64 {
    (f64::from(width) / 1920.0).min(f64::from(height) / 1080.0)
}

/// ラベルの文字（空は描かない）。描画の準備（LaTeX）に使う。
pub fn graph_label_texts(data: &GraphClipData) -> Vec<String> {
    let mut texts = vec![data.axes.x_label.clone(), data.axes.y_label.clone()];
    texts.extend(data.functions.iter().map(|function| function.label.clone()));
    texts.retain(|text| !text.trim().is_empty());
    texts
}

impl GraphLayout {
    pub fn new(
        data: &GraphClipData,
        length: i64,
        width: u32,
        height: u32,
    ) -> Result<Self, MathError> {
        match data.validate(length) {
            GraphValidationStatus::Valid => {}
            GraphValidationStatus::InvalidViewport => {
                return Err(MathError::new("グラフの表示範囲が不正です"));
            }
            GraphValidationStatus::InvalidGraph => {
                return Err(MathError::new("グラフの構造が不正です"));
            }
        }
        if width == 0 || height == 0 || width > MAXIMUM_DIMENSION || height > MAXIMUM_DIMENSION {
            return Err(MathError::new("グラフの canvas の上限を超えています"));
        }
        let (w, h) = (f64::from(width), f64::from(height));
        let plot = Area {
            left: w / 8.0,
            top: h / 8.0,
            width: w * 0.75,
            height: h * 0.75,
        };
        let viewport = data.viewport;
        let mut layout = Self {
            width,
            height,
            x_min: viewport.x_min,
            x_max: viewport.x_max,
            y_min: viewport.y_min,
            y_max: viewport.y_max,
            plot,
            background: Vec::new(),
            curves: Vec::new(),
            labels: Vec::new(),
            draw_frames: data.intro.frames,
        };
        let scale = stroke_scale(width, height);
        if data.axes.show_grid {
            for index in 0..=10 {
                let x = plot.left + plot.width * (f64::from(index) / 10.0);
                let y = plot.top + plot.height * (f64::from(index) / 10.0);
                layout.background.push((
                    [
                        Point { x, y: plot.top },
                        Point {
                            x,
                            y: plot.top + plot.height,
                        },
                    ],
                    GRID_COLOR,
                    scale,
                ));
                layout.background.push((
                    [
                        Point { x: plot.left, y },
                        Point {
                            x: plot.left + plot.width,
                            y,
                        },
                    ],
                    GRID_COLOR,
                    scale,
                ));
            }
        }
        if data.axes.show_axes {
            if layout.y_min <= 0.0 && layout.y_max >= 0.0 {
                let a = layout.map(Point {
                    x: layout.x_min,
                    y: 0.0,
                });
                let b = layout.map(Point {
                    x: layout.x_max,
                    y: 0.0,
                });
                layout.background.push(([a, b], AXIS_COLOR, 2.0 * scale));
            }
            if layout.x_min <= 0.0 && layout.x_max >= 0.0 {
                let a = layout.map(Point {
                    x: 0.0,
                    y: layout.y_min,
                });
                let b = layout.map(Point {
                    x: 0.0,
                    y: layout.y_max,
                });
                layout.background.push(([a, b], AXIS_COLOR, 2.0 * scale));
            }
        }
        layout.labels.push(Label {
            text: data.axes.x_label.clone(),
            area: Area {
                left: plot.left,
                top: h * 0.875,
                width: plot.width,
                height: h * 0.125,
            },
            rotated: false,
        });
        layout.labels.push(Label {
            text: data.axes.y_label.clone(),
            area: Area {
                left: 0.0,
                top: plot.top,
                width: w * 0.125,
                height: plot.height,
            },
            rotated: true,
        });
        let band = plot.width / data.functions.len().max(1) as f64;
        for (index, function) in data.functions.iter().enumerate() {
            let compiled = compile(&function.expression);
            match compiled.status {
                CompileStatus::Success => {}
                CompileStatus::UnsupportedExpression => {
                    return Err(MathError::new(format!(
                        "{} 番目の関数に未対応の記法があります: {}",
                        index + 1,
                        function.expression
                    )));
                }
                CompileStatus::InvalidExpression => {
                    return Err(MathError::new(format!(
                        "{} 番目の関数の式が不正です（{} 文字目付近）: {}",
                        index + 1,
                        compiled.offset + 1,
                        function.expression
                    )));
                }
            }
            let request = SamplingRequest {
                domain_min: function.domain_min.unwrap_or(layout.x_min),
                domain_max: function.domain_max.unwrap_or(layout.x_max),
                x_min: layout.x_min,
                x_max: layout.x_max,
                y_min: layout.y_min,
                y_max: layout.y_max,
                plot_width: plot.width,
                plot_height: plot.height,
            };
            let (status, geometry) = sample(&compiled.expression, &request);
            match status {
                SamplingStatus::Success | SamplingStatus::ValidEmptyCurve => {}
                SamplingStatus::NoFiniteSamples => {
                    return Err(MathError::new(format!(
                        "{} 番目の関数は表示範囲で値を持ちません",
                        index + 1
                    )));
                }
                _ => {
                    return Err(MathError::new(format!(
                        "{} 番目の関数の点列を作れません",
                        index + 1
                    )));
                }
            }
            let color = parse_argb_color(&function.color)
                .ok_or_else(|| MathError::new("曲線の色が不正です"))?;
            layout.curves.push(Curve {
                geometry,
                color,
                width: function.stroke_width * scale,
            });
            layout.labels.push(Label {
                text: function.label.clone(),
                area: Area {
                    left: plot.left + index as f64 * band,
                    top: 0.0,
                    width: band,
                    height: h * 0.125,
                },
                rotated: false,
            });
        }
        Ok(layout)
    }

    fn map(&self, point: Point) -> Point {
        Point {
            x: self.plot.left
                + ((point.x - self.x_min) / (self.x_max - self.x_min)) * self.plot.width,
            y: self.plot.top
                + ((self.y_max - point.y) / (self.y_max - self.y_min)) * self.plot.height,
        }
    }

    /// Draw の素材 frame `frame` で描く（`None` は Draw の後と同じ全体）。`label` はラベルの mask を返す。
    /// 描くべきラベルの mask が無ければ失敗する（黙って抜かない）。
    pub fn render(
        &self,
        frame: Option<i64>,
        label: &dyn Fn(&str) -> Option<Arc<Mask>>,
    ) -> Result<Vec<u8>, MathError> {
        let mut pixmap = Pixmap::new(self.width, self.height)
            .ok_or_else(|| MathError::new("グラフの canvas を確保できません"))?;
        let mut clip = tiny_skia::Mask::new(self.width, self.height)
            .ok_or_else(|| MathError::new("グラフの clip を確保できません"))?;
        let plot_rect = Rect::from_xywh(
            self.plot.left as f32,
            self.plot.top as f32,
            self.plot.width as f32,
            self.plot.height as f32,
        )
        .ok_or_else(|| MathError::new("グラフの plot が不正です"))?;
        clip.fill_path(
            &PathBuilder::from_rect(plot_rect),
            FillRule::Winding,
            false,
            Transform::identity(),
        );
        for (points, color, width) in &self.background {
            stroke(&mut pixmap, points, *color, *width, None);
        }
        for curve in &self.curves {
            let geometry = match frame {
                Some(frame) => reveal(&curve.geometry, frame, self.draw_frames),
                None => curve.geometry.clone(),
            };
            for segment in &geometry.segments {
                let mapped: Vec<Point> = segment.iter().map(|point| self.map(*point)).collect();
                stroke(&mut pixmap, &mapped, curve.color, curve.width, Some(&clip));
            }
        }
        let mut rgba = demultiply(pixmap.data());
        for item in &self.labels {
            if item.text.trim().is_empty() {
                continue;
            }
            let mask = label(&item.text).ok_or_else(|| {
                MathError::new(format!(
                    "グラフのラベル「{}」がまだ描けていません",
                    item.text
                ))
            })?;
            place_label(
                &mut rgba,
                self.width,
                self.height,
                &mask,
                item.area.content(),
                item.rotated,
            );
        }
        Ok(rgba)
    }
}

fn stroke(
    pixmap: &mut Pixmap,
    points: &[Point],
    color: u32,
    width: f64,
    clip: Option<&tiny_skia::Mask>,
) {
    if points.len() < 2 || width <= 0.0 {
        return;
    }
    let mut builder = PathBuilder::new();
    builder.move_to(points[0].x as f32, points[0].y as f32);
    for point in &points[1..] {
        builder.line_to(point.x as f32, point.y as f32);
    }
    let Some(path) = builder.finish() else {
        return;
    };
    let [a, r, g, b] = color.to_be_bytes();
    let mut paint = Paint::default();
    paint.set_color(Color::from_rgba8(r, g, b, a));
    paint.anti_alias = true;
    let stroke = Stroke {
        width: width as f32,
        line_cap: LineCap::Round,
        line_join: LineJoin::Round,
        ..Stroke::default()
    };
    pixmap.stroke_path(&path, &paint, &stroke, Transform::identity(), clip);
}

fn demultiply(premultiplied: &[u8]) -> Vec<u8> {
    let mut out = premultiplied.to_vec();
    for pixel in out.as_chunks_mut::<4>().0 {
        let alpha = u32::from(pixel[3]);
        if alpha == 0 {
            pixel[..3].fill(0);
            continue;
        }
        for channel in &mut pixel[..3] {
            *channel = ((u32::from(*channel) * 255 + alpha / 2) / alpha).min(255) as u8;
        }
    }
    out
}

/// glyph の外接矩形を、`area` に収まる最大の倍率で中央に置く（双線形で標本化する）。
fn place_label(rgba: &mut [u8], width: u32, height: u32, mask: &Mask, area: Area, rotated: bool) {
    let Some((left, top, right, bottom)) = mask.bounds() else {
        return;
    };
    let (glyph_w, glyph_h) = (f64::from(right - left), f64::from(bottom - top));
    let (shown_w, shown_h) = if rotated {
        (glyph_h, glyph_w)
    } else {
        (glyph_w, glyph_h)
    };
    let scale = (area.width / shown_w).min(area.height / shown_h);
    if !scale.is_finite() || scale <= 0.0 {
        return;
    }
    let out_w = shown_w * scale;
    let out_h = shown_h * scale;
    let origin_x = area.left + (area.width - out_w) / 2.0;
    let origin_y = area.top + (area.height - out_h) / 2.0;
    let sample = |x: f64, y: f64| -> f64 {
        // mask の座標（glyph の矩形の中）を双線形に読む。
        let x = x + f64::from(left) - 0.5;
        let y = y + f64::from(top) - 0.5;
        let x0 = x.floor();
        let y0 = y.floor();
        let fx = x - x0;
        let fy = y - y0;
        let at = |xi: f64, yi: f64| -> f64 {
            if xi < 0.0 || yi < 0.0 || xi >= f64::from(mask.width) || yi >= f64::from(mask.height) {
                return 0.0;
            }
            f64::from(mask.coverage[yi as usize * mask.width as usize + xi as usize])
        };
        let top_row = at(x0, y0) * (1.0 - fx) + at(x0 + 1.0, y0) * fx;
        let bottom_row = at(x0, y0 + 1.0) * (1.0 - fx) + at(x0 + 1.0, y0 + 1.0) * fx;
        (top_row * (1.0 - fy) + bottom_row * fy) / 255.0
    };
    let color = LABEL_COLOR.to_be_bytes();
    let color = [
        f32::from(color[1]) / 255.0,
        f32::from(color[2]) / 255.0,
        f32::from(color[3]) / 255.0,
        f32::from(color[0]) / 255.0,
    ];
    let x_begin = origin_x.floor().max(0.0) as u32;
    let y_begin = origin_y.floor().max(0.0) as u32;
    let x_end = ((origin_x + out_w).ceil() as u32).min(width);
    let y_end = ((origin_y + out_h).ceil() as u32).min(height);
    for y in y_begin..y_end {
        for x in x_begin..x_end {
            // 出力の画素の中心を glyph の座標へ戻す。y ラベルは反時計回りに 90 度回した向き。
            let u = (f64::from(x) + 0.5 - origin_x) / scale;
            let v = (f64::from(y) + 0.5 - origin_y) / scale;
            let (gx, gy) = if rotated { (glyph_w - v, u) } else { (u, v) };
            if gx < 0.0 || gy < 0.0 || gx > glyph_w || gy > glyph_h {
                continue;
            }
            let coverage = sample(gx, gy) as f32;
            if coverage <= 0.0 {
                continue;
            }
            let at = ((y * width + x) * 4) as usize;
            over_pixel(&mut rgba[at..at + 4], color, coverage * color[3]);
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use mvm_project::graph_clip::{GraphFunction, GraphFunctionId};

    use super::*;

    fn data(expression: &str) -> GraphClipData {
        let mut data = GraphClipData::with_default_function(GraphFunctionId("f1".to_owned()));
        data.functions[0] = GraphFunction {
            id: GraphFunctionId("f1".to_owned()),
            expression: expression.to_owned(),
            color: "#FFFF0000".to_owned(),
            stroke_width: 6.0,
            ..GraphFunction::default()
        };
        data.axes.x_label = String::new();
        data.axes.y_label = String::new();
        data
    }

    fn pixel(rgba: &[u8], width: u32, x: u32, y: u32) -> [u8; 4] {
        let at = ((y * width + x) * 4) as usize;
        [rgba[at], rgba[at + 1], rgba[at + 2], rgba[at + 3]]
    }

    #[test]
    fn the_curve_passes_through_mapped_points_and_stays_inside_the_plot() {
        // 出力 960x960（線の倍率 0.5、曲線は 3 px）、表示範囲 -5..5。plot は (120, 120) から 720x720。
        let layout = GraphLayout::new(&data("x"), 60, 960, 960).unwrap();
        let rgba = layout.render(None, &|_| None).unwrap();
        let red = |pixel: [u8; 4]| pixel[0] > 200 && pixel[1] < 120 && pixel[3] > 200;
        // 原点は plot の中央 (480, 480)、(2.5, 2.5) は (660, 300)。赤い曲線が通る。
        assert!(
            red(pixel(&rgba, 960, 660, 300)),
            "{:?}",
            pixel(&rgba, 960, 660, 300)
        );
        assert!(
            red(pixel(&rgba, 960, 300, 660)),
            "{:?}",
            pixel(&rgba, 960, 300, 660)
        );
        // plot の外は透明。曲線は plot の外へ出ない。
        assert_eq!(pixel(&rgba, 960, 60, 60)[3], 0);
        assert_eq!(pixel(&rgba, 960, 900, 30)[3], 0);
        // 負の対照: y = x から離れた plot の中（(660, 660)）は曲線の色ではない。
        assert!(
            !red(pixel(&rgba, 960, 660, 660)),
            "{:?}",
            pixel(&rgba, 960, 660, 660)
        );
    }

    #[test]
    fn draw_reveals_from_the_left_edge() {
        let mut graph = data("x");
        graph.intro.kind = mvm_project::graph_clip::GraphIntroKind::Draw;
        graph.intro.frames = 10;
        let layout = GraphLayout::new(&graph, 60, 960, 960).unwrap();
        let half = layout.render(Some(5), &|_| None).unwrap();
        // 半分では左下の (300, 660) まで描き、右上の (660, 300) はまだ描かない。
        assert!(pixel(&half, 960, 300, 660)[0] > 200);
        assert!(
            pixel(&half, 960, 660, 300)[0] < 100,
            "{:?}",
            pixel(&half, 960, 660, 300)
        );
    }

    #[test]
    fn invalid_expressions_and_missing_labels_fail() {
        assert!(GraphLayout::new(&data("x +"), 60, 400, 400).is_err());
        assert!(GraphLayout::new(&data("floor(x)"), 60, 400, 400).is_err());
        let mut labeled = data("x");
        labeled.axes.x_label = "x".to_owned();
        let layout = GraphLayout::new(&labeled, 60, 960, 960).unwrap();
        assert!(
            layout.render(None, &|_| None).is_err(),
            "ラベルを黙って抜きました"
        );
        let mask = Arc::new(Mask::new(4, 2, vec![255; 8]).unwrap());
        let rgba = layout.render(None, &|_| Some(Arc::clone(&mask))).unwrap();
        // x ラベルの帯（下の 1/8）の中央 (480, 900) は白く塗られる。
        let label = pixel(&rgba, 960, 480, 900);
        assert_eq!(label, [255, 255, 255, 255]);
    }
}
