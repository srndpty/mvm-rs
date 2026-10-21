//! 線で描くアイコン。画像アセットを持たず、C++ 版と同じ 16x16 の座標系の図形を painter で描く。
//! 文字（▶ など）で代用しない。フォントの置き換えで崩れ、太さも周りの線と揃わない。

use std::f32::consts::FRAC_PI_4;

use eframe::egui::{
    self, Color32, Painter, Pos2, Rect, Response, Sense, Shape, Stroke, Vec2, WidgetInfo,
    WidgetType, epaint,
};
use mvm_project::timeline::TimelineTool;

use crate::theme;

/// 16x16 の座標を、描く矩形へ写す。
#[derive(Clone, Copy)]
struct Grid {
    origin: Pos2,
    scale: f32,
}

impl Grid {
    fn new(rect: Rect) -> Self {
        let side = rect.width().min(rect.height());
        Self {
            origin: rect.center() - Vec2::splat(side / 2.0),
            scale: side / 16.0,
        }
    }

    fn p(self, x: f32, y: f32) -> Pos2 {
        self.origin + Vec2::new(x, y) * self.scale
    }

    fn stroke(self, width: f32, color: Color32) -> Stroke {
        Stroke::new(width * self.scale, color)
    }
}

struct Pen<'a> {
    painter: &'a Painter,
    grid: Grid,
    stroke: Stroke,
    color: Color32,
}

impl Pen<'_> {
    fn line(&self, points: &[(f32, f32)]) {
        let points = points.iter().map(|&(x, y)| self.grid.p(x, y)).collect();
        self.painter.add(Shape::line(points, self.stroke));
    }

    fn closed(&self, points: &[(f32, f32)]) {
        let points = points.iter().map(|&(x, y)| self.grid.p(x, y)).collect();
        self.painter.add(Shape::closed_line(points, self.stroke));
    }

    fn fill(&self, points: &[(f32, f32)]) {
        let points: Vec<Pos2> = points.iter().map(|&(x, y)| self.grid.p(x, y)).collect();
        fill_polygon(self.painter, &points, self.color);
    }

    fn quad(&self, from: (f32, f32), control: (f32, f32), to: (f32, f32)) {
        self.painter
            .add(epaint::QuadraticBezierShape::from_points_stroke(
                [
                    self.grid.p(from.0, from.1),
                    self.grid.p(control.0, control.1),
                    self.grid.p(to.0, to.1),
                ],
                false,
                Color32::TRANSPARENT,
                self.stroke,
            ));
    }

    fn circle(&self, x: f32, y: f32, radius: f32) {
        self.painter
            .circle_stroke(self.grid.p(x, y), radius * self.grid.scale, self.stroke);
    }

    fn dot(&self, x: f32, y: f32, radius: f32) {
        self.painter
            .circle_filled(self.grid.p(x, y), radius * self.grid.scale, self.color);
    }

    /// 横向きの矢印。先端の三角は toX の外側に付く。
    fn horizontal_arrow(&self, from_x: f32, to_x: f32, y: f32) {
        self.line(&[(from_x, y), (to_x, y)]);
        let direction = if to_x > from_x { 1.0 } else { -1.0 };
        let tip = to_x + direction;
        self.fill(&[
            (tip, y),
            (tip - 3.0 * direction, y - 3.0),
            (tip - 3.0 * direction, y + 3.0),
        ]);
    }

    /// "]" / "[" 形の編集点。open が 1 なら右へ開く "["。
    fn bracket(&self, x: f32, open: f32) {
        self.line(&[
            (x + 2.5 * open, 2.5),
            (x, 2.5),
            (x, 13.5),
            (x + 2.5 * open, 13.5),
        ]);
    }
}

/// 単純多角形（凹でもよい）を塗る。耳刈りで三角形へ分け、縁は細い線で anti-alias する。
fn fill_polygon(painter: &Painter, points: &[Pos2], color: Color32) {
    if points.len() < 3 {
        return;
    }
    let area: f32 = points
        .iter()
        .zip(points.iter().cycle().skip(1))
        .map(|(a, b)| a.x * b.y - b.x * a.y)
        .sum();
    let orientation = area.signum();
    let mut mesh = epaint::Mesh::default();
    for point in points {
        mesh.colored_vertex(*point, color);
    }
    let mut remaining: Vec<u32> = (0..points.len() as u32).collect();
    let corner = |a: Pos2, b: Pos2, c: Pos2| (b - a).x * (c - b).y - (b - a).y * (c - b).x;
    let inside = |p: Pos2, a: Pos2, b: Pos2, c: Pos2| {
        let d1 = corner(a, b, p);
        let d2 = corner(b, c, p);
        let d3 = corner(c, a, p);
        let negative = d1 < 0.0 || d2 < 0.0 || d3 < 0.0;
        let positive = d1 > 0.0 || d2 > 0.0 || d3 > 0.0;
        !(negative && positive)
    };
    let mut guard = 0;
    while remaining.len() > 3 && guard < points.len() * points.len() {
        guard += 1;
        let count = remaining.len();
        let mut clipped = false;
        for i in 0..count {
            let (ia, ib, ic) = (
                remaining[(i + count - 1) % count],
                remaining[i],
                remaining[(i + 1) % count],
            );
            let (a, b, c) = (
                points[ia as usize],
                points[ib as usize],
                points[ic as usize],
            );
            if corner(a, b, c) * orientation <= 0.0 {
                continue;
            }
            let blocked = remaining.iter().any(|&other| {
                other != ia && other != ib && other != ic && inside(points[other as usize], a, b, c)
            });
            if blocked {
                continue;
            }
            mesh.add_triangle(ia, ib, ic);
            remaining.remove(i);
            clipped = true;
            break;
        }
        if !clipped {
            break;
        }
    }
    if let [a, b, c] = remaining[..] {
        mesh.add_triangle(a, b, c);
    }
    painter.add(Shape::mesh(mesh));
    painter.add(Shape::closed_line(points.to_vec(), Stroke::new(0.6, color)));
}

/// timeline のツールのアイコン。
pub fn paint_tool(painter: &Painter, rect: Rect, tool: TimelineTool, color: Color32) {
    let grid = Grid::new(rect);
    let pen = Pen {
        painter,
        grid,
        stroke: grid.stroke(1.4, color),
        color,
    };
    match tool {
        TimelineTool::Select => pen.fill(&[
            (4.0, 1.5),
            (4.0, 13.0),
            (6.8, 10.3),
            (8.8, 14.5),
            (10.6, 13.6),
            (8.6, 9.6),
            (12.3, 9.6),
        ]),
        TimelineTool::TrackForward | TimelineTool::TrackBackward => {
            let forward = tool == TimelineTool::TrackForward;
            let base = if forward { 2.0 } else { 14.0 };
            let direction = if forward { 1.0 } else { -1.0 };
            pen.line(&[(base, 2.0), (base, 14.0)]);
            pen.horizontal_arrow(base + 2.0 * direction, base + 9.0 * direction, 5.0);
            pen.horizontal_arrow(base + 2.0 * direction, base + 9.0 * direction, 11.0);
        }
        TimelineTool::Razor => {
            // 中心 (8, 8) で -45° 回した刃と柄。
            let (sin, cos) = (-FRAC_PI_4).sin_cos();
            let turn = |x: f32, y: f32| (8.0 + x * cos - y * sin, 8.0 + x * sin + y * cos);
            pen.closed(&[
                turn(-3.0, -7.0),
                turn(3.0, -7.0),
                turn(3.0, 3.0),
                turn(-3.0, 3.0),
            ]);
            pen.line(&[turn(0.0, -4.5), turn(0.0, 0.5)]);
            pen.fill(&[
                turn(-1.5, 3.0),
                turn(1.5, 3.0),
                turn(1.5, 7.5),
                turn(-1.5, 7.5),
            ]);
        }
        TimelineTool::Ripple => {
            pen.bracket(6.0, -1.0);
            pen.horizontal_arrow(8.5, 13.0, 8.0);
        }
        TimelineTool::Rolling => {
            pen.bracket(7.0, -1.0);
            pen.bracket(9.0, 1.0);
            pen.horizontal_arrow(5.0, 1.5, 8.0);
            pen.horizontal_arrow(11.0, 14.5, 8.0);
        }
        TimelineTool::Rate => {
            pen.bracket(2.0, 1.0);
            pen.bracket(14.0, -1.0);
            pen.circle(8.0, 8.0, 3.5);
            pen.line(&[(8.0, 5.8), (8.0, 8.0), (9.8, 8.0)]);
        }
        TimelineTool::Slip => {
            pen.bracket(2.0, 1.0);
            pen.bracket(14.0, -1.0);
            pen.horizontal_arrow(7.0, 11.0, 8.0);
            pen.horizontal_arrow(9.0, 5.0, 8.0);
        }
        TimelineTool::Slide => {
            pen.closed(&[(5.0, 4.0), (11.0, 4.0), (11.0, 12.0), (5.0, 12.0)]);
            pen.horizontal_arrow(4.0, 1.5, 8.0);
            pen.horizontal_arrow(12.0, 14.5, 8.0);
        }
        TimelineTool::Pen => {
            pen.closed(&[
                (3.0, 13.0),
                (4.2, 8.8),
                (11.0, 2.0),
                (14.0, 5.0),
                (7.2, 11.8),
            ]);
            pen.line(&[(3.0, 13.0), (5.6, 10.4)]);
        }
        TimelineTool::Hand => {
            pen.line(&[(4.5, 9.0), (4.5, 11.0)]);
            pen.quad((4.5, 11.0), (5.0, 14.5), (8.5, 14.5));
            pen.quad((8.5, 14.5), (12.5, 14.5), (12.5, 10.5));
            pen.line(&[(12.5, 10.5), (12.5, 5.5)]);
            for (x, top) in [(6.0, 4.5), (8.3, 2.5), (10.5, 3.5), (12.5, 5.5)] {
                pen.line(&[(x, 9.5), (x, top)]);
            }
            pen.line(&[(4.5, 9.0), (2.5, 7.5)]);
        }
        TimelineTool::Zoom => paint_magnifier(&pen, None),
        TimelineTool::Text => {
            let pen = Pen {
                stroke: grid.stroke(1.8, color),
                ..pen
            };
            pen.line(&[(3.0, 3.0), (13.0, 3.0)]);
            pen.line(&[(8.0, 3.0), (8.0, 14.0)]);
            pen.line(&[(6.0, 14.0), (10.0, 14.0)]);
        }
    }
}

/// 虫眼鏡。`plus` が Some なら、レンズの中に + / - を描く。
fn paint_magnifier(pen: &Pen<'_>, plus: Option<bool>) {
    pen.circle(6.5, 6.5, 4.3);
    if let Some(plus) = plus {
        pen.line(&[(4.5, 6.5), (8.5, 6.5)]);
        if plus {
            pen.line(&[(6.5, 4.5), (6.5, 8.5)]);
        }
    }
    let thick = Pen {
        stroke: pen.grid.stroke(2.0, pen.color),
        ..*pen
    };
    thick.line(&[(9.7, 9.7), (14.0, 14.0)]);
}

impl Clone for Pen<'_> {
    fn clone(&self) -> Self {
        *self
    }
}

impl Copy for Pen<'_> {}

/// video track の表示の目玉。非表示ならスラッシュを重ねる。
pub fn paint_eye(painter: &Painter, rect: Rect, hidden: bool, color: Color32) {
    let grid = Grid::new(rect);
    let pen = Pen {
        painter,
        grid,
        stroke: grid.stroke(1.4, color),
        color,
    };
    pen.quad((1.5, 8.0), (8.0, 1.0), (14.5, 8.0));
    pen.quad((14.5, 8.0), (8.0, 15.0), (1.5, 8.0));
    pen.dot(8.0, 8.0, 2.2);
    if hidden {
        let slash = Pen {
            stroke: grid.stroke(1.6, color),
            ..pen
        };
        slash.line(&[(2.5, 14.0), (13.5, 2.0)]);
    }
}

/// アニメーションの入り切りのストップウォッチ。
pub fn paint_stopwatch(painter: &Painter, rect: Rect, color: Color32) {
    let grid = Grid::new(rect);
    let pen = Pen {
        painter,
        grid,
        stroke: grid.stroke(1.4, color),
        color,
    };
    pen.circle(8.0, 9.2, 5.2);
    pen.line(&[(5.5, 1.8), (10.5, 1.8)]);
    pen.line(&[(8.0, 1.8), (8.0, 4.0)]);
    pen.line(&[(8.0, 6.2), (8.0, 9.2), (10.4, 9.2)]);
}

/// 開閉・送り・一覧の矢印の向き。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Chevron {
    Down,
    Right,
    Left,
    /// 一覧を開く ComboBox の上下の印。
    UpDown,
}

/// 「く」の字の矢印。文字（v ⌄ ▾ ‹ ›）で代用すると字形が崩れるので、必ずこの図形で描く。
pub fn paint_chevron(painter: &Painter, rect: Rect, direction: Chevron, color: Color32) {
    let grid = Grid::new(rect);
    let pen = Pen {
        painter,
        grid,
        stroke: grid.stroke(1.8, color),
        color,
    };
    match direction {
        Chevron::Down => pen.line(&[(4.0, 6.0), (8.0, 10.5), (12.0, 6.0)]),
        Chevron::Right => pen.line(&[(6.0, 3.5), (10.5, 8.0), (6.0, 12.5)]),
        Chevron::Left => pen.line(&[(10.0, 3.5), (5.5, 8.0), (10.0, 12.5)]),
        Chevron::UpDown => {
            pen.line(&[(4.5, 6.0), (8.0, 2.5), (11.5, 6.0)]);
            pen.line(&[(4.5, 10.0), (8.0, 13.5), (11.5, 10.0)]);
        }
    }
}

/// キーフレームの菱形。`filled` なら塗る（その位置にキーがある）。
pub fn paint_diamond(painter: &Painter, center: Pos2, radius: f32, filled: bool, color: Color32) {
    let points = vec![
        center + Vec2::new(0.0, -radius),
        center + Vec2::new(radius, 0.0),
        center + Vec2::new(0.0, radius),
        center + Vec2::new(-radius, 0.0),
    ];
    if filled {
        painter.add(Shape::convex_polygon(points, color, Stroke::NONE));
    } else {
        painter.add(Shape::closed_line(points, Stroke::new(1.2, color)));
    }
}

/// テロップ位置のボタン。画面の枠と、下寄りの左・中央・右に置いた帯。
pub fn paint_placement(
    painter: &Painter,
    rect: Rect,
    alignment: &str,
    frame: Color32,
    bar: Color32,
) {
    let screen = Rect::from_center_size(rect.center(), Vec2::new(18.0, 14.0));
    painter.rect_stroke(
        screen,
        1.0,
        Stroke::new(1.0, frame),
        egui::StrokeKind::Inside,
    );
    let x = match alignment {
        "left" => screen.min.x + 2.0,
        "right" => screen.max.x - 10.0,
        _ => screen.center().x - 4.0,
    };
    painter.rect_filled(
        Rect::from_min_size(Pos2::new(x, screen.max.y - 5.0), Vec2::new(8.0, 2.0)),
        0.0,
        bar,
    );
}

/// プロジェクトパネルの種別。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MediaIcon {
    Folder,
    Video,
    Audio,
    Image,
}

/// プロジェクトパネルの種別アイコン（16x14）。矩形の組み合わせで描く。
pub fn paint_media_kind(painter: &Painter, origin: Pos2, kind: MediaIcon) {
    let dark = Color32::from_rgb(0x1b, 0x1f, 0x25);
    let at = |x: f32, y: f32, w: f32, h: f32| {
        Rect::from_min_size(origin + Vec2::new(x, y), Vec2::new(w, h))
    };
    match kind {
        MediaIcon::Folder => {
            let gold = Color32::from_rgb(0xc9, 0xa5, 0x5c);
            painter.rect_filled(at(1.0, 1.0, 6.0, 3.0), 1.0, gold);
            painter.rect_filled(at(1.0, 3.0, 14.0, 10.0), 1.5, gold);
        }
        MediaIcon::Video => {
            painter.rect_filled(
                at(1.0, 1.0, 14.0, 12.0),
                1.5,
                Color32::from_rgb(0x5b, 0x8f, 0xd1),
            );
            for row in 0..3 {
                let y = 3.0 + row as f32 * 3.5;
                painter.rect_filled(at(2.5, y, 2.0, 2.0), 0.0, dark);
                painter.rect_filled(at(11.5, y, 2.0, 2.0), 0.0, dark);
            }
        }
        MediaIcon::Audio => {
            painter.rect_filled(
                at(1.0, 1.0, 14.0, 12.0),
                1.5,
                Color32::from_rgb(0x4f, 0xae, 0x7d),
            );
            for (index, height) in [4.0f32, 8.0, 5.0, 9.0, 3.0].into_iter().enumerate() {
                let x = 4.5 + index as f32 * 2.5;
                painter.rect_filled(at(x, 7.0 - height / 2.0, 1.5, height), 0.0, dark);
            }
        }
        MediaIcon::Image => {
            let frame = at(1.0, 1.0, 14.0, 12.0);
            painter.rect_filled(frame, 1.5, Color32::from_rgb(0xb6, 0x7a, 0xc9));
            let clipped = painter.with_clip_rect(frame.intersect(painter.clip_rect()));
            clipped.circle_filled(origin + Vec2::new(10.5, 4.5), 1.5, dark);
            // 45° 回した四角の上半分（山）。
            let center = origin + Vec2::new(5.0, 13.0);
            clipped.add(Shape::convex_polygon(
                vec![
                    center + Vec2::new(-5.7, 0.0),
                    center + Vec2::new(0.0, -5.7),
                    center + Vec2::new(5.7, 0.0),
                ],
                dark,
                Stroke::NONE,
            ));
        }
    }
}

/// 検索を消す ×。
pub fn paint_cross(painter: &Painter, rect: Rect, color: Color32) {
    let grid = Grid::new(rect);
    let pen = Pen {
        painter,
        grid,
        stroke: grid.stroke(1.6, color),
        color,
    };
    pen.line(&[(4.5, 4.5), (11.5, 11.5)]);
    pen.line(&[(11.5, 4.5), (4.5, 11.5)]);
}

/// 拡大・縮小のボタンの虫眼鏡。
pub fn paint_zoom(painter: &Painter, rect: Rect, zoom_in: bool, color: Color32) {
    let grid = Grid::new(rect);
    let pen = Pen {
        painter,
        grid,
        stroke: grid.stroke(1.4, color),
        color,
    };
    paint_magnifier(&pen, Some(zoom_in));
}

/// アイコンだけのボタン。読み上げ・試験からは `label` で引ける。
pub fn button(
    ui: &mut egui::Ui,
    size: Vec2,
    label: &str,
    selected: bool,
    paint: impl FnOnce(&Painter, Rect, Color32),
) -> Response {
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    response
        .widget_info(|| WidgetInfo::selected(WidgetType::Button, ui.is_enabled(), selected, label));
    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        let (fill, border) = if selected {
            (theme::SELECTED, theme::SELECTED_BORDER)
        } else if response.hovered() && ui.is_enabled() {
            (theme::TOOL_HOVER, Color32::TRANSPARENT)
        } else {
            (Color32::TRANSPARENT, Color32::TRANSPARENT)
        };
        painter.rect(
            rect,
            4.0,
            fill,
            Stroke::new(1.0, border),
            egui::StrokeKind::Inside,
        );
        let color = if !ui.is_enabled() {
            theme::TEXT_DISABLED
        } else if selected {
            Color32::WHITE
        } else {
            theme::TEXT_ICON
        };
        let side = 16.0f32.min(rect.width()).min(rect.height());
        paint(
            painter,
            Rect::from_center_size(rect.center(), Vec2::splat(side)),
            color,
        );
    }
    response
}

#[cfg(test)]
mod tests {
    use super::*;
    use mvm_project::timeline::TOOLS;

    const SQUARE: Rect = Rect::from_min_max(Pos2::ZERO, Pos2::new(16.0, 16.0));

    fn shapes_for(paint: impl FnOnce(&Painter)) -> Vec<Shape> {
        let ctx = egui::Context::default();
        let mut paint = Some(paint);
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            let painter = ui.ctx().layer_painter(egui::LayerId::background());
            if let Some(paint) = paint.take() {
                paint(&painter);
            }
        });
        // font の atlas の更新は描く先が無いので捨てる。
        output.textures_delta.clear();
        output
            .shapes
            .into_iter()
            .map(|clipped| clipped.shape)
            .collect()
    }

    fn triangles(shapes: &[Shape]) -> usize {
        shapes
            .iter()
            .map(|shape| match shape {
                Shape::Mesh(mesh) => mesh.indices.len() / 3,
                _ => 0,
            })
            .sum()
    }

    #[test]
    fn every_tool_draws_inside_its_square() {
        for spec in TOOLS {
            let shapes = shapes_for(|painter| {
                paint_tool(painter, SQUARE, spec.tool, Color32::WHITE);
            });
            assert!(!shapes.is_empty(), "{} のアイコンが空です", spec.name);
            for shape in &shapes {
                let bounds = shape.visual_bounding_rect();
                assert!(
                    SQUARE.expand(1.5).contains_rect(bounds),
                    "{} のアイコンが 16x16 からはみ出しました: {bounds:?}",
                    spec.name
                );
            }
        }
    }

    #[test]
    fn a_concave_polygon_is_split_into_all_of_its_triangles() {
        // 選択ツールの矢印は凹の 7 角形。扇形に分けると切り欠きを塗りつぶしてしまう。
        let shapes = shapes_for(|painter| {
            paint_tool(painter, SQUARE, TimelineTool::Select, Color32::WHITE);
        });
        assert_eq!(triangles(&shapes), 5);
        let mesh = shapes
            .iter()
            .find_map(|shape| match shape {
                Shape::Mesh(mesh) => Some(mesh.clone()),
                _ => None,
            })
            .unwrap();
        // 切り欠き（柄の左、(6, 12)）は塗らない。三角形のどれにも入らないことを見る。
        let notch = Pos2::new(6.0, 12.0);
        let covered = mesh.indices.chunks(3).any(|tri| {
            let [a, b, c] = [0, 1, 2].map(|k| mesh.vertices[tri[k] as usize].pos);
            let side = |p: Pos2, q: Pos2| (q - p).x * (notch - p).y - (q - p).y * (notch - p).x;
            let (d1, d2, d3) = (side(a, b), side(b, c), side(c, a));
            (d1 >= 0.0 && d2 >= 0.0 && d3 >= 0.0) || (d1 <= 0.0 && d2 <= 0.0 && d3 <= 0.0)
        });
        assert!(!covered, "矢印の切り欠きを塗りました");
    }

    #[test]
    fn the_hidden_eye_adds_a_slash() {
        let shown = shapes_for(|painter| paint_eye(painter, SQUARE, false, Color32::WHITE));
        let hidden = shapes_for(|painter| paint_eye(painter, SQUARE, true, Color32::WHITE));
        assert_eq!(hidden.len(), shown.len() + 1);
    }
}
