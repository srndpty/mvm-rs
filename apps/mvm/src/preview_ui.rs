//! preview。停止中は playhead の frame、再生中は worker が届けた frame を出す。
//!
//! 停止中に動画・画像の clip を選ぶと、見えている枠を重ねる。枠の中を掴むと移動、角を掴むと拡縮し、離したときに
//! 1 回だけ commit する。

use std::sync::Arc;

use eframe::egui::{
    self, Color32, ColorImage, Pos2, Rect, Sense, Stroke, TextureHandle, TextureOptions,
};

use mvm_app::{PreviewImage, Session};
use mvm_project::timeline::TimelineTool;

/// preview の枠のドラッグ。Project は変えず、離したときに 1 回だけ commit する。
pub struct TransformDrag {
    clip_id: String,
    /// `None` は移動、`Some(i)` は角 i（左上から時計回り）の拡縮。
    corner: Option<usize>,
    origin: Pos2,
    /// 掴んだときの矩形（出力画素の x, y, 幅, 高さ）。
    start: [f64; 4],
}

pub fn show(
    ui: &mut egui::Ui,
    session: &mut Session,
    texture: &mut Option<(u64, TextureHandle)>,
    draft: &mut String,
    notice: &mut String,
    drag: &mut Option<TransformDrag>,
) {
    let ctx = ui.ctx().clone();
    session.set_preview_wake(Arc::new(move || ctx.request_repaint()));
    session.refresh_preview();
    session.poll_preview();
    ui.horizontal(|ui| {
        let verb = if session.is_playing() {
            "一時停止"
        } else {
            "再生"
        };
        if ui.button(verb).clicked() {
            session.toggle_playback();
        }
        ui.label(if session.is_playing() {
            "再生中"
        } else {
            "停止"
        });
        ui.monospace(session.playhead_label());
    });
    if session.tool() == TimelineTool::Text {
        ui.label("置く文字");
        ui.add(egui::TextEdit::multiline(draft).desired_rows(3));
        let commit = ui.button("文字を置く").clicked()
            || ui.input_mut(|input| input.consume_key(egui::Modifiers::COMMAND, egui::Key::Enter));
        if ui.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::Escape)) {
            draft.clear();
        }
        if commit {
            match session.place_text(draft) {
                Ok(()) => {
                    draft.clear();
                    notice.clear();
                }
                Err(error) => *notice = error.message().to_owned(),
            }
        }
    }
    if let Some(note) = session.skipped_note() {
        ui.label(note);
    }
    if let Some(error) = session.preview_error() {
        ui.colored_label(Color32::from_rgb(230, 120, 120), error);
        if ui.button("再表示").clicked() {
            session.invalidate_preview();
        }
    }
    let generation = session.preview_generation();
    let Some((size, image)) = session
        .preview_image()
        .and_then(|image| image_size(image).map(|size| (size, image)))
    else {
        // 失敗した frame に、前の frame の画像を残さない。
        *texture = None;
        *drag = None;
        return;
    };
    let handle = match texture {
        Some((cached, handle)) if *cached == generation => handle,
        _ => {
            let color = ColorImage::from_rgba_premultiplied(size, &image.rgba);
            let handle = ui
                .ctx()
                .load_texture("preview", color, TextureOptions::LINEAR);
            &texture.insert((generation, handle)).1
        }
    };
    let available = ui.available_size();
    if available.x > 1.0 && available.y > 1.0 {
        let image = ui.add(
            egui::Image::from_texture(egui::load::SizedTexture::from_handle(handle))
                .max_size(available)
                .maintain_aspect_ratio(true),
        );
        transform_overlay(ui, session, drag, image.rect);
    } else {
        *drag = None;
    }
}

/// 画素列の長さが寸法と一致するときだけ、egui に渡す寸法を返す。
fn image_size(image: &PreviewImage) -> Option<[usize; 2]> {
    let width = usize::try_from(image.width).ok()?;
    let height = usize::try_from(image.height).ok()?;
    let bytes = width.checked_mul(height)?.checked_mul(4)?;
    (bytes == image.rgba.len()).then_some([width, height])
}

const HANDLE: f32 = 5.0;

/// 選んだ動画・画像 clip の見えている枠。中を掴むと移動、角を掴むと縦横比を保って拡縮する（Shift で縦横別）。
/// 回転した clip は枠と画像が一致しないので出さない。
fn transform_overlay(
    ui: &mut egui::Ui,
    session: &mut Session,
    drag: &mut Option<TransformDrag>,
    image_rect: Rect,
) {
    let output_width = f64::from(session.project().output_width);
    let target = session
        .selected_clip()
        .map(str::to_owned)
        .filter(|_| !session.is_playing())
        .and_then(|id| session.clip_visual_rect(&id).map(|geometry| (id, geometry)))
        .filter(|(_, geometry)| geometry.rotation_degrees == 0.0);
    let Some((clip_id, geometry)) = target else {
        *drag = None;
        return;
    };
    if output_width <= 0.0 || image_rect.width() <= 0.0 {
        return;
    }
    let scale = f64::from(image_rect.width()) / output_width;
    let to_screen = |rect: [f64; 4]| {
        Rect::from_min_size(
            Pos2::new(
                image_rect.min.x + (rect[0] * scale) as f32,
                image_rect.min.y + (rect[1] * scale) as f32,
            ),
            egui::vec2((rect[2] * scale) as f32, (rect[3] * scale) as f32),
        )
    };
    let start = [geometry.x, geometry.y, geometry.width, geometry.height];
    let response = ui.interact(image_rect, ui.id().with("preview_transform"), Sense::drag());
    if response.drag_started()
        && let Some(origin) = response.interact_pointer_pos()
    {
        let screen = to_screen(start);
        let corner = corners(screen)
            .iter()
            .position(|point| point.distance(origin) <= HANDLE * 2.0);
        if corner.is_some() || screen.contains(origin) {
            *drag = Some(TransformDrag {
                clip_id: clip_id.clone(),
                corner,
                origin,
                start,
            });
        }
    }
    let free = ui.input(|input| input.modifiers.shift);
    let pointer = ui.input(|input| input.pointer.latest_pos());
    let current = match (drag.as_ref(), pointer) {
        (Some(active), Some(pointer)) if active.clip_id == clip_id => {
            dragged_rect(active, pointer, scale, free)
        }
        _ => start,
    };
    let painter = ui.painter_at(image_rect);
    let screen = to_screen(current);
    let color = Color32::from_rgb(98, 182, 255);
    painter.rect_stroke(
        screen,
        0.0,
        Stroke::new(1.5, color),
        egui::StrokeKind::Middle,
    );
    for point in corners(screen) {
        painter.rect_filled(
            Rect::from_center_size(point, egui::vec2(HANDLE * 2.0, HANDLE * 2.0)),
            1.0,
            color,
        );
    }
    if response.drag_stopped()
        && let Some(active) = drag.take()
        && active.clip_id == clip_id
        && current != active.start
        && let Err(error) = session.place_clip_visual_rect(&clip_id, current)
    {
        session.set_alert(error.message());
    }
}

/// 左上・右上・右下・左下。
fn corners(rect: Rect) -> [Pos2; 4] {
    [
        rect.left_top(),
        rect.right_top(),
        rect.right_bottom(),
        rect.left_bottom(),
    ]
}

/// ドラッグ中の矩形（出力画素）。角の拡縮は向かいの角を固定する。
fn dragged_rect(active: &TransformDrag, pointer: Pos2, scale: f64, free: bool) -> [f64; 4] {
    let [x, y, width, height] = active.start;
    let dx = f64::from(pointer.x - active.origin.x) / scale;
    let dy = f64::from(pointer.y - active.origin.y) / scale;
    let Some(corner) = active.corner else {
        return [x + dx, y + dy, width, height];
    };
    // 掴んだ角の向き（右・下へ伸ばすと正）。
    let (sx, sy) = match corner {
        0 => (-1.0, -1.0),
        1 => (1.0, -1.0),
        2 => (1.0, 1.0),
        _ => (-1.0, 1.0),
    };
    let mut new_width = (width + sx * dx).max(1.0);
    let mut new_height = (height + sy * dy).max(1.0);
    if !free && width > 0.0 && height > 0.0 {
        let factor = ((new_width / width) + (new_height / height)) / 2.0;
        new_width = (width * factor).max(1.0);
        new_height = (height * factor).max(1.0);
    }
    let new_x = if sx < 0.0 { x + width - new_width } else { x };
    let new_y = if sy < 0.0 { y + height - new_height } else { y };
    [new_x, new_y, new_width, new_height]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn active(corner: Option<usize>) -> TransformDrag {
        TransformDrag {
            clip_id: "c".to_owned(),
            corner,
            origin: Pos2::new(100.0, 100.0),
            start: [100.0, 50.0, 400.0, 200.0],
        }
    }

    #[test]
    fn dragging_inside_moves_by_the_pointer_in_output_pixels() {
        // 表示は出力の半分の大きさなので、10 pixel の移動は出力の 20 pixel。
        let rect = dragged_rect(&active(None), Pos2::new(110.0, 95.0), 0.5, false);
        assert_eq!(rect, [120.0, 40.0, 400.0, 200.0]);
    }

    #[test]
    fn a_corner_keeps_the_aspect_and_pins_the_opposite_corner() {
        // 右下の角を右へ 100: 幅は 1.25 倍・高さは 1.0 倍なので、平均の 1.125 倍。
        let rect = dragged_rect(&active(Some(2)), Pos2::new(200.0, 100.0), 1.0, false);
        assert_eq!(rect, [100.0, 50.0, 450.0, 225.0]);
        // Shift の左上は縦横別に縮め、右下（500, 250）を固定する。
        let rect = dragged_rect(&active(Some(0)), Pos2::new(140.0, 120.0), 1.0, true);
        assert_eq!(rect, [140.0, 70.0, 360.0, 180.0]);
    }

    #[test]
    fn a_corner_never_collapses_the_rect() {
        let rect = dragged_rect(&active(Some(2)), Pos2::new(-900.0, -900.0), 1.0, true);
        assert_eq!((rect[2], rect[3]), (1.0, 1.0));
    }
}
