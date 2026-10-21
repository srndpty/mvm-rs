//! 文字の書式（フォント・サイズ・太字・文字色・縁・背景）。文字 clip のエフェクトコントロールと字幕の
//! 共通書式が同じ部品を使い、見た目と操作を揃える（C++ 版の TextStyleEditor）。
//! 値は読むだけにし、変更は [`StyleEdit`] で返す。保存先の違いは呼び出し側が吸収する。

use eframe::egui::{self, Align2, FontId, Sense, Vec2};

use crate::theme;
use crate::widgets::{self, ColorEdit, FontEdit, NumberEdit, NumberField};

/// 書式の今の値。
pub struct StyleValues<'a> {
    pub font_family: &'a str,
    pub font_size: i32,
    pub bold: bool,
    pub color: &'a str,
    pub outline_color: &'a str,
    pub outline_width: i32,
    pub background_color: &'a str,
}

/// 書式の 1 つの値。
#[derive(Clone, Debug, PartialEq)]
pub enum StyleChange {
    FontFamily(String),
    FontSize(i32),
    Bold(bool),
    Color(String),
    OutlineColor(String),
    OutlineWidth(i32),
    BackgroundColor(String),
}

/// 書式の欄の操作。`commits` は確定（1 つごとに 1 回の undo）、`previews` は確定前に preview だけで見せる値
/// （ドラッグ中・色を選んでいる途中・書体の一覧で指している書体）、`cancel` は仮の値を元へ戻す。
#[derive(Debug, Default, PartialEq)]
pub struct StyleEdit {
    pub commits: Vec<StyleChange>,
    pub previews: Vec<StyleChange>,
    pub cancel: bool,
}

impl StyleEdit {
    fn number(&mut self, edit: Option<NumberEdit>, current: i32, change: fn(i32) -> StyleChange) {
        match edit {
            Some(NumberEdit::Preview(value)) => self.previews.push(change(value as i32)),
            Some(NumberEdit::Commit(value)) if value as i32 != current => {
                self.commits.push(change(value as i32));
            }
            Some(NumberEdit::Commit(_) | NumberEdit::Cancel) => self.cancel = true,
            Some(NumberEdit::Start) | None => {}
        }
    }

    fn color(&mut self, edit: Option<ColorEdit>, change: fn(String) -> StyleChange) {
        match edit {
            Some(ColorEdit::Preview(value)) => self.previews.push(change(value)),
            Some(ColorEdit::Commit(value)) => self.commits.push(change(value)),
            Some(ColorEdit::Cancel) => self.cancel = true,
            None => {}
        }
    }
}

/// 書式の欄を描く。`bold_row` は太字の右に並べる操作、`middle` は太字の行と色の間に置く行。
pub fn show(
    ui: &mut egui::Ui,
    id: egui::Id,
    values: &StyleValues<'_>,
    maximum_font_size: i32,
    bold_row: impl FnOnce(&mut egui::Ui),
    middle: impl FnOnce(&mut egui::Ui),
) -> StyleEdit {
    let mut edit = StyleEdit::default();
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        let font_width = (ui.available_width() - 62.0).max(80.0);
        match widgets::font_box(ui, id.with("font"), values.font_family, font_width) {
            Some(FontEdit::Hover(font)) => edit.previews.push(StyleChange::FontFamily(font)),
            Some(FontEdit::Commit(font)) => edit.commits.push(StyleChange::FontFamily(font)),
            Some(FontEdit::Cancel) => edit.cancel = true,
            None => {}
        }
        let size = NumberField {
            inline_label: 1.0,
            value: f64::from(values.font_size),
            minimum: 1.0,
            maximum: f64::from(maximum_font_size.max(1)),
            step: 0.5,
            width: 58.0,
            ..NumberField::default()
        }
        .show(ui, id.with("size"));
        edit.number(size, values.font_size, StyleChange::FontSize);
    });
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        if widgets::style_toggle(ui, "B", values.bold, "太字").clicked() {
            edit.commits.push(StyleChange::Bold(!values.bold));
        }
        ui.add_space(6.0);
        bold_row(ui);
    });
    middle(ui);
    // 色の行。見出し・見本・16 進の欄（縁は太さも）。幅は panel の幅から決める。
    let total = ui.available_width();
    for (key, label, title, value) in [
        ("color", "文字色", "文字色", values.color),
        ("outline", "縁", "縁の色", values.outline_color),
        ("background", "背景", "背景色", values.background_color),
    ] {
        let change: fn(String) -> StyleChange = match key {
            "color" => StyleChange::Color,
            "outline" => StyleChange::OutlineColor,
            _ => StyleChange::BackgroundColor,
        };
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            section_label(ui, label, 44.0);
            let swatch = widgets::color_swatch(ui, id.with((key, "swatch")), value, title);
            edit.color(swatch, change);
            let outline = key == "outline";
            let hex_width =
                (total - 44.0 - 34.0 - 12.0 - if outline { 76.0 } else { 0.0 }).max(60.0);
            if let Some(typed) = widgets::hex_field(ui, id.with((key, "hex")), value, hex_width) {
                edit.commits.push(change(typed));
            }
            if outline {
                let field = NumberField {
                    label: "幅",
                    inline_label: 20.0,
                    value: f64::from(values.outline_width),
                    minimum: 0.0,
                    maximum: 32.0,
                    step: 0.2,
                    width: 70.0,
                    ..NumberField::default()
                };
                let width = field.show(ui, id.with("outline_width"));
                edit.number(width, values.outline_width, StyleChange::OutlineWidth);
            }
        });
    }
    edit
}

/// 行の左の見出し（11 px の灰色、左寄せ）。
pub fn section_label(ui: &mut egui::Ui, text: &str, width: f32) {
    let (cell, _) = ui.allocate_exact_size(Vec2::new(width, 20.0), Sense::hover());
    ui.painter().text(
        cell.left_center(),
        Align2::LEFT_CENTER,
        text,
        FontId::proportional(11.0),
        theme::TEXT_SECONDARY,
    );
}
