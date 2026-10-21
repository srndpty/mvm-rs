//! ダイアログの枠（C++ 版の ModernDialog / ModernDialogFooter）。暗い幕の上に、見出し・本文・ボタンの帯を置く。
//! 本文の中の egui の部品（入力欄・選択欄・チェックボックス）も、C++ 版のダイアログと同じ配色にする。

use eframe::egui::{self, Align2, Color32, CornerRadius, Margin, Stroke, Vec2};

use crate::theme;

const FILL: Color32 = Color32::from_rgb(0x29, 0x2b, 0x2f);
const BORDER: Color32 = Color32::from_rgb(0x55, 0x59, 0x60);
const RULE: Color32 = Color32::from_rgb(0x41, 0x45, 0x4b);
const FOOTER: Color32 = Color32::from_rgb(0x25, 0x27, 0x2b);
const TITLE: Color32 = Color32::from_rgb(0xf0, 0xf1, 0xf3);
const BACKDROP: Color32 = Color32::from_rgba_premultiplied(0, 0, 0, 0xa8);
const HEADER_HEIGHT: f32 = 48.0;
const FOOTER_HEIGHT: f32 = 58.0;

/// ダイアログを出す。`body` の戻り値と、Esc・幕の押下で閉じるよう求められたかを返す。`footer` は右寄せの
/// ボタンの帯（右から順に足す。主な操作を最初に足す）。
pub fn show<R>(
    ctx: &egui::Context,
    id: &str,
    title: &str,
    width: f32,
    body: impl FnOnce(&mut egui::Ui) -> R,
    footer: impl FnOnce(&mut egui::Ui),
) -> (R, bool) {
    let frame = egui::Frame::new()
        .fill(FILL)
        .stroke(Stroke::new(1.0, BORDER))
        .corner_radius(CornerRadius::same(8));
    let response = egui::Modal::new(egui::Id::new(id))
        .frame(frame)
        .backdrop_color(BACKDROP)
        .show(ctx, |ui| {
            ui.set_width(width);
            ui.spacing_mut().item_spacing.y = 0.0;
            header(ui, title);
            let result = egui::Frame::new()
                .inner_margin(Margin {
                    left: 18,
                    right: 18,
                    top: 16,
                    bottom: 18,
                })
                .show(ui, |ui| {
                    apply_visuals(ui);
                    ui.spacing_mut().item_spacing = Vec2::new(8.0, 8.0);
                    body(ui)
                })
                .inner;
            footer_bar(ui, footer);
            result
        });
    let close = response.should_close();
    (response.inner, close)
}

fn header(ui: &mut egui::Ui, title: &str) {
    let (rect, response) = ui.allocate_exact_size(
        Vec2::new(ui.available_width(), HEADER_HEIGHT),
        egui::Sense::hover(),
    );
    // 見出しは読み上げ・試験からも引ける。
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Label, true, title));
    ui.painter().text(
        rect.left_center() + Vec2::new(18.0, 0.0),
        Align2::LEFT_CENTER,
        title,
        theme::bold(14.0),
        TITLE,
    );
    ui.painter()
        .hline(rect.x_range(), rect.max.y - 0.5, Stroke::new(1.0, RULE));
}

fn footer_bar(ui: &mut egui::Ui, footer: impl FnOnce(&mut egui::Ui)) {
    let (rect, _) = ui.allocate_exact_size(
        Vec2::new(ui.available_width(), FOOTER_HEIGHT),
        egui::Sense::hover(),
    );
    ui.painter().rect_filled(
        rect,
        CornerRadius {
            nw: 0,
            ne: 0,
            sw: 8,
            se: 8,
        },
        FOOTER,
    );
    ui.painter()
        .hline(rect.x_range(), rect.min.y + 0.5, Stroke::new(1.0, RULE));
    let buttons = rect.shrink2(Vec2::new(16.0, 0.0));
    ui.scope_builder(
        egui::UiBuilder::new()
            .max_rect(buttons)
            .layout(egui::Layout::right_to_left(egui::Align::Center)),
        |ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            apply_visuals(ui);
            footer(ui);
        },
    );
}

/// 本文の部品の配色（ModernDialogField・ComboBox・CheckBox と同じ）。
fn apply_visuals(ui: &mut egui::Ui) {
    let visuals = ui.visuals_mut();
    visuals.override_text_color = Some(Color32::from_rgb(0xe6, 0xe8, 0xec));
    visuals.extreme_bg_color = Color32::from_rgb(0x1d, 0x21, 0x27);
    visuals.text_edit_bg_color = Some(Color32::from_rgb(0x1d, 0x21, 0x27));
    visuals.selection.bg_fill = Color32::from_rgb(0x37, 0x6f, 0xa8);
    let radius = CornerRadius::same(4);
    let widgets = &mut visuals.widgets;
    for (state, fill, border) in [
        (&mut widgets.inactive, 0x3a_40_48, 0x56_5d_66),
        (&mut widgets.hovered, 0x4a_51_5a, 0x6c_a9_e6),
        (&mut widgets.active, 0x34_3a_42, 0x6c_a9_e6),
        (&mut widgets.open, 0x35_3b_43, 0x6c_a9_e6),
    ] {
        let fill = Color32::from_rgb((fill >> 16) as u8, (fill >> 8) as u8, fill as u8);
        let border = Color32::from_rgb((border >> 16) as u8, (border >> 8) as u8, border as u8);
        state.bg_fill = fill;
        state.weak_bg_fill = fill;
        state.bg_stroke = Stroke::new(1.0, border);
        state.corner_radius = radius;
    }
    widgets.noninteractive.bg_stroke = Stroke::new(1.0, Color32::from_rgb(0x51, 0x56, 0x5e));
    ui.spacing_mut().interact_size.y = 30.0;
    ui.spacing_mut().button_padding = Vec2::new(10.0, 6.0);
}

/// 進み具合の棒（ModernDialogProgressBar）。`fraction` が None なら、行き来する帯で動いていることを示す。
pub fn progress_bar(ui: &mut egui::Ui, fraction: Option<f32>) {
    let (rect, _) =
        ui.allocate_exact_size(Vec2::new(ui.available_width(), 7.0), egui::Sense::hover());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 4.0, Color32::from_rgb(0x1d, 0x21, 0x27));
    let fill = Color32::from_rgb(0x5a, 0x9c, 0xda);
    match fraction {
        Some(fraction) => {
            let width = rect.width() * fraction.clamp(0.0, 1.0);
            painter.rect_filled(
                egui::Rect::from_min_size(rect.min, Vec2::new(width, rect.height())),
                4.0,
                fill,
            );
        }
        None => {
            let phase = (ui.input(|input| input.time) / 1.4).fract() as f32 * 1.35 - 0.35;
            let x = rect.min.x + phase * rect.width();
            painter.rect_filled(
                egui::Rect::from_min_size(
                    egui::pos2(x, rect.min.y),
                    Vec2::new(rect.width() * 0.35, rect.height()),
                ),
                4.0,
                fill,
            );
            ui.ctx().request_repaint();
        }
    }
}
