//! 複数の panel で使う入力部品。

use eframe::egui::{self, Color32};
use mvm_project::parse_argb_color;

/// "#AARRGGBB" の色を、色見本と 16 進の欄で編集する。
pub fn color_row(ui: &mut egui::Ui, label: &str, value: &mut String) {
    ui.horizontal(|ui| {
        ui.label(label);
        if let Some(argb) = parse_argb_color(value) {
            let [a, r, g, b] = argb.to_be_bytes();
            let mut color = Color32::from_rgba_unmultiplied(r, g, b, a);
            if egui::color_picker::color_edit_button_srgba(
                ui,
                &mut color,
                egui::color_picker::Alpha::OnlyBlend,
            )
            .changed()
            {
                let [r, g, b, a] = color.to_srgba_unmultiplied();
                *value = format!("#{a:02X}{r:02X}{g:02X}{b:02X}");
            }
        }
        ui.add(egui::TextEdit::singleline(value).desired_width(90.0));
    });
}
