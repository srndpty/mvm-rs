//! preview。停止中は playhead の frame、再生中は worker が届けた frame を出す。

use std::sync::Arc;

use eframe::egui::{self, Color32, ColorImage, TextureHandle, TextureOptions};

use mvm_app::{PreviewImage, Session};

pub fn show(ui: &mut egui::Ui, session: &mut Session, texture: &mut Option<(u64, TextureHandle)>) {
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
        ui.add(
            egui::Image::from_texture(egui::load::SizedTexture::from_handle(handle))
                .max_size(available)
                .maintain_aspect_ratio(true),
        );
    }
}

/// 画素列の長さが寸法と一致するときだけ、egui に渡す寸法を返す。
fn image_size(image: &PreviewImage) -> Option<[usize; 2]> {
    let width = usize::try_from(image.width).ok()?;
    let height = usize::try_from(image.height).ok()?;
    let bytes = width.checked_mul(height)?.checked_mul(4)?;
    (bytes == image.rgba.len()).then_some([width, height])
}
