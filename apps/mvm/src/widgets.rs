//! 複数の panel で使う入力部品。見た目と操作は C++ 版の QML 部品（DragNumberField・TextStyleEditor など）に揃える。

use eframe::egui::{self, Align2, Color32, FontId, Rect, Sense, Stroke, Vec2};
use mvm_project::parse_argb_color;

use crate::{icons, theme};

/// "#AARRGGBB" の色を、色見本と 16 進の欄で編集する。
pub fn color_row(ui: &mut egui::Ui, label: &str, value: &mut String) {
    ui.horizontal(|ui| {
        ui.label(label);
        if let Some(argb) = parse_argb_color(value) {
            let mut color = argb_to_color(argb);
            if egui::color_picker::color_edit_button_srgba(
                ui,
                &mut color,
                egui::color_picker::Alpha::OnlyBlend,
            )
            .changed()
            {
                *value = color_to_argb(color);
            }
        }
        ui.add(egui::TextEdit::singleline(value).desired_width(90.0));
    });
}

/// track の M / S の小さなトグル。押した時点で確定する。
pub fn track_toggle(
    ui: &mut egui::Ui,
    label: &str,
    active: bool,
    active_color: Color32,
    tip: &str,
) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(Vec2::new(20.0, 20.0), Sense::click());
    response.widget_info(|| {
        egui::WidgetInfo::selected(egui::WidgetType::Button, ui.is_enabled(), active, label)
    });
    let fill = if active {
        active_color
    } else if response.hovered() {
        theme::CONTROL_HOVER
    } else {
        theme::CONTROL
    };
    ui.painter().rect(
        rect,
        3.0,
        fill,
        Stroke::new(1.0, theme::CONTROL_BORDER),
        egui::StrokeKind::Inside,
    );
    ui.painter().text(
        rect.center(),
        Align2::CENTER_CENTER,
        label,
        theme::bold(11.0),
        if active {
            theme::TOGGLE_ON_TEXT
        } else {
            theme::TEXT_ICON
        },
    );
    response.on_hover_text(tip)
}

/// 数値欄の編集。ドラッグ・直接入力を始めたときに `Start`、ドラッグ中は `Preview`、離したとき・入力を確定
/// したときに `Commit`、入力を取り消したときに `Cancel` を返す。
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum NumberEdit {
    Start,
    Preview(f64),
    Commit(f64),
    Cancel,
}

/// 左右ドラッグで値を変える数値欄（C++ 版の DragNumberField）。ダブルクリック（`click_to_edit` なら
/// クリック）で直接入力になる。
pub struct NumberField<'a> {
    pub label: &'a str,
    /// 0 より大きいと label を左に置いて 1 行（高さ 24）にする。値は label の幅。0 なら label は上。
    pub inline_label: f32,
    pub value: f64,
    pub minimum: f64,
    pub maximum: f64,
    /// 1 px ドラッグあたりの変化量。
    pub step: f64,
    pub decimals: usize,
    pub suffix: &'a str,
    pub click_to_edit: bool,
    /// 欄全体の幅。無限なら使える幅いっぱい。
    pub width: f32,
}

impl Default for NumberField<'_> {
    fn default() -> Self {
        Self {
            label: "",
            inline_label: 0.0,
            value: 0.0,
            minimum: -100_000.0,
            maximum: 100_000.0,
            step: 0.5,
            decimals: 0,
            suffix: "",
            click_to_edit: false,
            width: f32::INFINITY,
        }
    }
}

/// 数値欄の、frame をまたいで持つ状態。egui の memory に id で置く。
#[derive(Clone, Default)]
struct NumberMemory {
    press_value: f64,
    dragging: Option<f64>,
    editing: Option<String>,
    focused: bool,
}

impl NumberField<'_> {
    fn clamp(&self, value: f64) -> f64 {
        let factor = 10f64.powi(self.decimals as i32);
        ((value * factor).round() / factor).clamp(self.minimum, self.maximum)
    }

    /// 入力した文字列全体を数として読む。末尾の単位だけは付いていてよい。読めなければ None。
    fn parse(&self, text: &str) -> Option<f64> {
        let body = text.trim();
        let unit = self.suffix.trim();
        let body = if unit.is_empty() {
            body
        } else {
            body.strip_suffix(unit).unwrap_or(body).trim()
        };
        body.parse::<f64>()
            .ok()
            .filter(|value| value.is_finite())
            .map(|value| self.clamp(value))
    }

    fn text(&self, value: f64) -> String {
        format!("{value:.*}{}", self.decimals, self.suffix)
    }

    pub fn show(self, ui: &mut egui::Ui, id: egui::Id) -> Option<NumberEdit> {
        let inline = self.inline_label > 0.0;
        let height = if inline { 24.0 } else { 38.0 };
        let width = if self.width.is_finite() {
            self.width
        } else {
            ui.available_width()
        };
        let (rect, _) = ui.allocate_exact_size(Vec2::new(width, height), Sense::hover());
        let (label_rect, box_rect) = if inline {
            let split = rect.min.x + self.inline_label;
            (
                Rect::from_min_max(rect.min, egui::pos2(split, rect.max.y)),
                Rect::from_min_max(egui::pos2(split, rect.min.y), rect.max),
            )
        } else {
            (
                Rect::from_min_max(rect.min, egui::pos2(rect.max.x, rect.min.y + 14.0)),
                Rect::from_min_max(egui::pos2(rect.min.x, rect.max.y - 24.0), rect.max),
            )
        };
        if !self.label.is_empty() {
            ui.painter_at(label_rect).text(
                label_rect.left_center() + Vec2::new(2.0, 0.0),
                Align2::LEFT_CENTER,
                self.label,
                FontId::proportional(10.5),
                theme::TEXT_SECONDARY,
            );
        }
        let mut memory: NumberMemory = ui.data(|data| data.get_temp(id)).unwrap_or_default();
        if let Some(text) = memory.editing.take() {
            let result = self.edit_text(ui, id, box_rect, text, &mut memory);
            ui.data_mut(|data| data.insert_temp(id, memory));
            return result;
        }
        let mut result = None;
        let enabled = ui.is_enabled();
        let response = ui
            .interact(box_rect, id, Sense::click_and_drag())
            .on_hover_cursor(egui::CursorIcon::ResizeHorizontal);
        if response.drag_started() {
            memory.press_value = self.value;
            result = Some(NumberEdit::Start);
        }
        if response.dragged()
            && let (Some(origin), Some(pointer)) =
                ui.input(|input| (input.pointer.press_origin(), input.pointer.interact_pos()))
        {
            let delta = f64::from(pointer.x - origin.x);
            if memory.dragging.is_some() || delta.abs() >= 3.0 {
                let value = self.clamp(memory.press_value + delta * self.step);
                memory.dragging = Some(value);
                result = Some(NumberEdit::Preview(value));
            }
        }
        if response.drag_stopped()
            && let Some(value) = memory.dragging.take()
        {
            result = Some(NumberEdit::Commit(value));
        }
        if response.double_clicked() || (self.click_to_edit && response.clicked()) {
            memory.editing = Some(format!("{:.*}", self.decimals, self.value));
            memory.focused = false;
            result = Some(NumberEdit::Start);
        }
        let fill = if !enabled {
            theme::FIELD_DISABLED
        } else if response.is_pointer_button_down_on() {
            theme::FIELD_PRESSED
        } else {
            theme::FIELD
        };
        let border = if response.hovered() {
            theme::FIELD_FOCUS
        } else {
            theme::TIMELINE_BORDER
        };
        ui.painter().rect(
            box_rect,
            3.0,
            fill,
            Stroke::new(1.0, border),
            egui::StrokeKind::Inside,
        );
        let shown = memory.dragging.unwrap_or(self.value);
        let text = self.text(shown);
        response.widget_info(|| {
            let mut info =
                egui::WidgetInfo::labeled(egui::WidgetType::DragValue, enabled, self.label);
            info.current_text_value = Some(text.clone());
            info
        });
        ui.painter_at(box_rect.shrink(1.0)).text(
            box_rect.left_center() + Vec2::new(8.0, 0.0),
            Align2::LEFT_CENTER,
            &text,
            FontId::proportional(13.0),
            if enabled {
                theme::TEXT
            } else {
                theme::FIELD_DISABLED_TEXT
            },
        );
        ui.data_mut(|data| data.insert_temp(id, memory));
        result
    }

    /// 直接入力。Enter で確定、Esc で取り消す。欄の外を押したら、パネルでは取り消し、ダイアログ
    /// （click_to_edit）では確定する。読めない入力は閉じずに直させる。
    fn edit_text(
        &self,
        ui: &mut egui::Ui,
        id: egui::Id,
        box_rect: Rect,
        mut text: String,
        memory: &mut NumberMemory,
    ) -> Option<NumberEdit> {
        let response = ui.put(
            box_rect,
            egui::TextEdit::singleline(&mut text)
                .id(id.with("editor"))
                .font(FontId::proportional(13.0))
                .margin(Vec2::new(7.0, 4.0)),
        );
        if !memory.focused {
            response.request_focus();
            memory.focused = true;
        }
        if ui.input(|input| input.key_pressed(egui::Key::Escape)) {
            *memory = NumberMemory::default();
            return Some(NumberEdit::Cancel);
        }
        if !response.lost_focus() {
            memory.editing = Some(text);
            return None;
        }
        let enter = ui.input(|input| input.key_pressed(egui::Key::Enter));
        if !enter && !self.click_to_edit {
            *memory = NumberMemory::default();
            return Some(NumberEdit::Cancel);
        }
        match self.parse(&text) {
            Some(value) => {
                *memory = NumberMemory::default();
                Some(NumberEdit::Commit(value))
            }
            None => {
                response.request_focus();
                memory.editing = Some(text);
                None
            }
        }
    }
}

/// 色の選択の操作。選んでいる途中は `Preview`、OK で `Commit`、それ以外（キャンセル・Esc・外側の押下）で
/// 閉じたら `Cancel`（C++ 版の ModernColorPicker と同じ）。色は "#AARRGGBB"。
#[derive(Clone, Debug, PartialEq)]
pub enum ColorEdit {
    Preview(String),
    Commit(String),
    Cancel,
}

/// 色の見本（34x20）。押すと色の選択を開く。
pub fn color_swatch(
    ui: &mut egui::Ui,
    id: egui::Id,
    value: &str,
    title: &str,
) -> Option<ColorEdit> {
    let (rect, response) = ui.allocate_exact_size(Vec2::new(34.0, 20.0), Sense::click());
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::ColorButton, true, title));
    let response = response.on_hover_cursor(egui::CursorIcon::PointingHand);
    let current = parse_argb_color(value).map(argb_to_color);
    let border = if response.hovered() {
        theme::FIELD_FOCUS
    } else {
        Color32::from_rgb(0x5a, 0x61, 0x6c)
    };
    let painter = ui.painter();
    painter.rect(
        rect,
        2.0,
        Color32::from_rgb(0x2a, 0x2f, 0x37),
        Stroke::new(1.0, border),
        egui::StrokeKind::Inside,
    );
    if let Some(color) = current {
        painter.rect_filled(rect.shrink(2.0), 0.0, color);
        // 完全に透明な色は見本が背景と区別できないので斜線で示す。
        if color.a() == 0 {
            painter.line_segment(
                [
                    rect.left_bottom() + Vec2::new(4.0, -4.0),
                    rect.right_top() + Vec2::new(-4.0, 4.0),
                ],
                Stroke::new(1.0, Color32::from_rgb(0xd0, 0x50, 0x50)),
            );
        }
    }
    let draft_id = id.with("draft");
    let popup_id = id.with("popup");
    let mut draft: Option<Color32> = ui.data(|data| data.get_temp(draft_id)).flatten();
    if response.clicked() && draft.is_none() {
        draft = current;
    }
    let before = draft;
    let mut accepted = false;
    let mut canceled = false;
    let shown = egui::Popup::from_toggle_button_response(&response)
        .id(popup_id)
        .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
        .width(300.0)
        .show(|ui| {
            ui.label(
                egui::RichText::new(title)
                    .font(theme::bold(12.0))
                    .color(theme::TEXT),
            );
            if let Some(color) = draft.as_mut() {
                egui::color_picker::color_picker_color32(
                    ui,
                    color,
                    egui::color_picker::Alpha::OnlyBlend,
                );
            }
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if dialog_button(ui, "OK", ButtonKind::Prominent).clicked() {
                        accepted = true;
                    }
                    if dialog_button(ui, "キャンセル", ButtonKind::Normal).clicked() {
                        canceled = true;
                    }
                });
            });
        });
    if shown.is_some() && !accepted && !canceled {
        ui.data_mut(|data| data.insert_temp(draft_id, draft));
        if draft != before
            && let Some(color) = draft
        {
            return Some(ColorEdit::Preview(color_to_argb(color)));
        }
        return None;
    }
    if shown.is_some() {
        egui::Popup::close_id(ui.ctx(), popup_id);
    }
    ui.data_mut(|data| data.remove::<Option<Color32>>(draft_id));
    // 開いていなければ何もしない。閉じたときは OK だけを確定し、他は取り消す。
    let color = draft?;
    if accepted {
        return Some(if Some(color) == current {
            ColorEdit::Cancel
        } else {
            ColorEdit::Commit(color_to_argb(color))
        });
    }
    Some(ColorEdit::Cancel)
}

fn argb_to_color(argb: u32) -> Color32 {
    let [a, r, g, b] = argb.to_be_bytes();
    Color32::from_rgba_unmultiplied(r, g, b, a)
}

fn color_to_argb(color: Color32) -> String {
    let [r, g, b, a] = color.to_srgba_unmultiplied();
    format!("#{a:02X}{r:02X}{g:02X}{b:02X}")
}

/// 色の 16 進の直接入力。確定（Enter・欄の外）で読めた値だけを返し、読めなければ元へ戻す。Esc で取り消す。
pub fn hex_field(ui: &mut egui::Ui, id: egui::Id, value: &str, width: f32) -> Option<String> {
    let mut text: String = ui
        .data(|data| data.get_temp(id))
        .unwrap_or_else(|| value.to_owned());
    let response = ui.add_sized(
        Vec2::new(width, 24.0),
        egui::TextEdit::singleline(&mut text)
            .id(id)
            .font(FontId::monospace(12.0))
            .margin(Vec2::new(8.0, 4.0)),
    );
    if response.has_focus() {
        if ui.input(|input| input.key_pressed(egui::Key::Escape)) {
            ui.data_mut(|data| data.remove::<String>(id));
            response.surrender_focus();
        } else {
            ui.data_mut(|data| data.insert_temp(id, text));
        }
        return None;
    }
    ui.data_mut(|data| data.remove::<String>(id));
    if !response.lost_focus() {
        return None;
    }
    let upper = text.trim().to_uppercase();
    (parse_argb_color(&upper).is_some() && upper != value).then_some(upper)
}

/// 書体の選択の操作。一覧で指している書体は `Hover`（preview へ仮に反映する）、選んだら `Commit`、選ばずに
/// 閉じたら `Cancel`（C++ 版と同じ）。
#[derive(Clone, Debug, PartialEq)]
pub enum FontEdit {
    Hover(String),
    Commit(String),
    Cancel,
}

/// 字幕向きの太く読みやすい書体。この PC に入っているものを一覧の先頭に置く（C++ 版と同じ候補）。
const RECOMMENDED_FONTS: [&str; 13] = [
    "源真ゴシックP Bold",
    "源真ゴシックP Heavy",
    "LINE Seed JP_OTF Bold",
    "LINE Seed JP OTF ExtraBold",
    "BIZ UDPGothic",
    "BIZ UDPゴシック",
    "Noto Sans JP Black",
    "Noto Sans JP Medium",
    "源暎ゴシックN Bold",
    "UD Digi Kyokasho NP-B",
    "UD デジタル 教科書体 NP",
    "Meiryo",
    "メイリオ",
];
/// 一覧の右に出す見本の文字。
const FONT_SAMPLE: &str = "文字もじモジ";
const FONT_ROW: f32 = 26.0;

/// 書体の一覧と見本の texture。全ての選択欄で共有する（OS の書体は起動中に変わらない扱い）。
#[derive(Clone, Default)]
struct FontCache {
    families: Option<Result<std::sync::Arc<Vec<String>>, String>>,
    samples: std::collections::HashMap<String, Option<egui::TextureHandle>>,
}

/// 選択欄ごとの、開いている間の状態。
#[derive(Clone, Default)]
struct FontBoxMemory {
    open: bool,
    filter: String,
    hovered: Option<String>,
}

/// 一覧の行。見出しか書体。
enum FontRow<'a> {
    Caption(&'static str),
    Font(&'a str),
}

/// 一覧に並べる行。絞り込みが無ければ、おすすめ（この PC にあるもの）の後に全ての書体を見出し付きで並べる。
fn font_rows<'a>(families: &'a [String], filter: &str) -> Vec<FontRow<'a>> {
    let filter = filter.trim().to_lowercase();
    if !filter.is_empty() {
        return families
            .iter()
            .filter(|name| name.to_lowercase().contains(&filter))
            .map(|name| FontRow::Font(name.as_str()))
            .collect();
    }
    let recommended: Vec<&str> = RECOMMENDED_FONTS
        .iter()
        .copied()
        .filter(|name| families.iter().any(|family| family == name))
        .collect();
    let mut rows = Vec::new();
    if !recommended.is_empty() {
        rows.push(FontRow::Caption("おすすめ (字幕向き)"));
        rows.extend(recommended.into_iter().map(FontRow::Font));
        rows.push(FontRow::Caption("すべてのフォント"));
    }
    rows.extend(families.iter().map(|name| FontRow::Font(name.as_str())));
    rows
}

fn font_sample_texture(ui: &egui::Ui, family: &str) -> Option<egui::TextureHandle> {
    let (width, height, pixels) =
        mvm_app::font_sample(family, FONT_SAMPLE, 14, 0xFFC8_CED6).ok()?;
    let image = egui::ColorImage::from_rgba_unmultiplied(
        [usize::try_from(width).ok()?, usize::try_from(height).ok()?],
        &pixels,
    );
    Some(ui.ctx().load_texture(
        format!("font_sample:{family}"),
        image,
        egui::TextureOptions::LINEAR,
    ))
}

/// 書体の選択欄（C++ 版の TextStyleEditor の ComboBox）。
pub fn font_box(ui: &mut egui::Ui, id: egui::Id, value: &str, width: f32) -> Option<FontEdit> {
    let (rect, response) = ui.allocate_exact_size(Vec2::new(width, 26.0), Sense::click());
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::ComboBox, true, value));
    let popup_id = id.with("popup");
    let open = egui::Popup::is_id_open(ui.ctx(), popup_id);
    let border = if response.hovered() || open {
        theme::FIELD_FOCUS
    } else {
        theme::TIMELINE_BORDER
    };
    ui.painter().rect(
        rect,
        3.0,
        if response.is_pointer_button_down_on() {
            Color32::from_rgb(0x2a, 0x2f, 0x37)
        } else {
            theme::FIELD
        },
        Stroke::new(1.0, border),
        egui::StrokeKind::Inside,
    );
    ui.painter_at(rect.shrink2(Vec2::new(1.0, 1.0))).text(
        rect.left_center() + Vec2::new(8.0, 0.0),
        Align2::LEFT_CENTER,
        value,
        FontId::proportional(12.0),
        theme::TEXT,
    );
    icons::paint_chevron(
        ui.painter(),
        Rect::from_center_size(
            egui::pos2(rect.max.x - 13.0, rect.center().y),
            Vec2::splat(12.0),
        ),
        icons::Chevron::UpDown,
        theme::TEXT_ICON,
    );
    let mut memory: FontBoxMemory = ui.data(|data| data.get_temp(id)).unwrap_or_default();
    let cache_id = egui::Id::new("font_cache");
    let mut result = None;
    let mut chosen = None;
    let shown = egui::Popup::from_toggle_button_response(&response)
        .id(popup_id)
        .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
        .width(width.max(320.0))
        .show(|ui| {
            let search = ui.add(
                egui::TextEdit::singleline(&mut memory.filter)
                    .id(id.with("search"))
                    .hint_text("フォントを検索")
                    .font(FontId::proportional(12.0))
                    .margin(Vec2::new(8.0, 5.0))
                    .desired_width(f32::INFINITY),
            );
            if !memory.open {
                search.request_focus();
                memory.filter.clear();
            }
            let mut cache: FontCache = ui
                .data_mut(|data| data.get_temp(cache_id))
                .unwrap_or_default();
            let families = cache
                .families
                .get_or_insert_with(|| {
                    mvm_app::font_families()
                        .map(std::sync::Arc::new)
                        .map_err(|error| error.to_string())
                })
                .clone();
            let families = match families {
                Ok(families) => families,
                Err(error) => {
                    ui.label(egui::RichText::new(error).color(theme::ERROR));
                    return;
                }
            };
            let rows = font_rows(&families, &memory.filter);
            if rows.is_empty() {
                ui.label(
                    egui::RichText::new(format!(
                        "「{}」に一致するフォントはありません",
                        memory.filter.trim()
                    ))
                    .size(12.0)
                    .color(theme::TEXT_SECONDARY),
                );
            }
            let mut hovered = None;
            egui::ScrollArea::vertical()
                .id_salt(id.with("list"))
                .max_height(420.0)
                .show_rows(ui, FONT_ROW, rows.len(), |ui, range| {
                    ui.spacing_mut().item_spacing.y = 0.0;
                    for row in &rows[range] {
                        let (rect, response) = ui.allocate_exact_size(
                            Vec2::new(ui.available_width(), FONT_ROW),
                            Sense::click(),
                        );
                        match row {
                            FontRow::Caption(caption) => {
                                ui.painter().text(
                                    rect.left_center() + Vec2::new(8.0, 2.0),
                                    Align2::LEFT_CENTER,
                                    *caption,
                                    FontId::proportional(11.0),
                                    Color32::from_rgb(0x7d, 0x85, 0x90),
                                );
                            }
                            FontRow::Font(name) => {
                                response.widget_info(|| {
                                    egui::WidgetInfo::labeled(egui::WidgetType::Button, true, *name)
                                });
                                if response.hovered() {
                                    hovered = Some((*name).to_owned());
                                }
                                if response.hovered() || *name == value {
                                    ui.painter().rect_filled(
                                        rect,
                                        3.0,
                                        Color32::from_rgb(0x30, 0x45, 0x5c),
                                    );
                                }
                                ui.painter_at(rect).text(
                                    rect.left_center() + Vec2::new(8.0, 0.0),
                                    Align2::LEFT_CENTER,
                                    *name,
                                    FontId::proportional(12.0),
                                    theme::TEXT,
                                );
                                let sample = cache
                                    .samples
                                    .entry((*name).to_owned())
                                    .or_insert_with(|| font_sample_texture(ui, name));
                                if let Some(texture) = sample {
                                    let size = texture.size_vec2();
                                    let at = Rect::from_min_size(
                                        egui::pos2(
                                            rect.max.x - size.x - 8.0,
                                            rect.center().y - size.y / 2.0,
                                        ),
                                        size,
                                    );
                                    ui.painter().image(
                                        texture.id(),
                                        at,
                                        Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0)),
                                        Color32::WHITE,
                                    );
                                }
                                if response.clicked() {
                                    chosen = Some((*name).to_owned());
                                }
                            }
                        }
                    }
                });
            ui.data_mut(|data| data.insert_temp(cache_id, cache));
            if ui.input(|input| input.key_pressed(egui::Key::Enter))
                && let Some(FontRow::Font(name)) =
                    rows.iter().find(|row| matches!(row, FontRow::Font(_)))
            {
                chosen = Some((*name).to_owned());
            }
            if hovered.is_some() && hovered != memory.hovered {
                memory.hovered.clone_from(&hovered);
                result = hovered.map(FontEdit::Hover);
            }
        });
    if let Some(name) = chosen {
        egui::Popup::close_id(ui.ctx(), popup_id);
        memory = FontBoxMemory::default();
        result = Some(if name == value {
            FontEdit::Cancel
        } else {
            FontEdit::Commit(name)
        });
    } else if shown.is_some() {
        memory.open = true;
    } else if memory.open {
        // 選ばずに閉じた。仮に見せた書体を元へ戻す。
        memory = FontBoxMemory::default();
        result = Some(FontEdit::Cancel);
    }
    ui.data_mut(|data| data.insert_temp(id, memory));
    result
}

/// 書式のトグル（太字の B など）。押した時点で確定する。
pub fn style_toggle(ui: &mut egui::Ui, label: &str, checked: bool, tip: &str) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(Vec2::new(28.0, 24.0), Sense::click());
    response
        .widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::Checkbox, true, checked, tip));
    let (fill, border) = if checked {
        (theme::STYLE_ON, theme::FIELD_FOCUS)
    } else if response.hovered() {
        (Color32::from_rgb(0x2a, 0x2f, 0x37), theme::FIELD_FOCUS)
    } else {
        (Color32::TRANSPARENT, theme::TIMELINE_BORDER)
    };
    ui.painter().rect(
        rect,
        3.0,
        fill,
        Stroke::new(1.0, border),
        egui::StrokeKind::Inside,
    );
    ui.painter().text(
        rect.center(),
        Align2::CENTER_CENTER,
        label,
        theme::bold(13.0),
        if checked {
            Color32::WHITE
        } else {
            theme::TEXT_ICON
        },
    );
    response.on_hover_text(tip)
}

/// テロップ位置へ置くボタン。状態を持たないので、押している間だけ強調する。
pub fn placement_button(ui: &mut egui::Ui, alignment: &str, tip: &str) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(Vec2::new(30.0, 24.0), Sense::click());
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, tip));
    let fill = if response.is_pointer_button_down_on() {
        theme::STYLE_ON
    } else if response.hovered() {
        Color32::from_rgb(0x2a, 0x2f, 0x37)
    } else {
        Color32::TRANSPARENT
    };
    let border = if response.hovered() {
        theme::FIELD_FOCUS
    } else {
        theme::TIMELINE_BORDER
    };
    ui.painter().rect(
        rect,
        3.0,
        fill,
        Stroke::new(1.0, border),
        egui::StrokeKind::Inside,
    );
    icons::paint_placement(
        ui.painter(),
        rect,
        alignment,
        Color32::from_rgb(0x8b, 0x93, 0x9e),
        if response.hovered() {
            Color32::from_rgb(0x8c, 0xc4, 0xff)
        } else {
            theme::TEXT
        },
    );
    response.on_hover_text(tip)
}

/// ダイアログ・パネルのボタンの種類（C++ 版の ModernDialogButton）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ButtonKind {
    Normal,
    /// 主な操作。青い地。
    Prominent,
    /// 消す操作。hover で赤みを帯びる。
    Destructive,
}

pub fn dialog_button(ui: &mut egui::Ui, text: &str, kind: ButtonKind) -> egui::Response {
    let font = FontId::proportional(12.0);
    let text_width = ui
        .painter()
        .layout_no_wrap(text.to_owned(), font.clone(), theme::TEXT)
        .size()
        .x;
    let size = Vec2::new((text_width + 28.0).max(88.0), 31.0);
    let enabled = ui.is_enabled();
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, enabled, text));
    let down = response.is_pointer_button_down_on();
    let hovered = response.hovered();
    let fill = if !enabled {
        Color32::from_rgb(0x32, 0x36, 0x3c)
    } else {
        match kind {
            ButtonKind::Prominent if down => Color32::from_rgb(0x2e, 0x5a, 0x8b),
            ButtonKind::Prominent if hovered => Color32::from_rgb(0x47, 0x7f, 0xb8),
            ButtonKind::Prominent => Color32::from_rgb(0x37, 0x6f, 0xa8),
            ButtonKind::Destructive if hovered => Color32::from_rgb(0x68, 0x41, 0x43),
            _ if down => Color32::from_rgb(0x34, 0x3a, 0x42),
            _ if hovered => Color32::from_rgb(0x4a, 0x51, 0x5a),
            _ => Color32::from_rgb(0x3a, 0x40, 0x48),
        }
    };
    let border = if kind == ButtonKind::Prominent {
        Color32::from_rgb(0x5a, 0x8f, 0xc6)
    } else {
        Color32::from_rgb(0x56, 0x5d, 0x66)
    };
    ui.painter().rect(
        rect,
        4.0,
        fill,
        Stroke::new(1.0, border),
        egui::StrokeKind::Inside,
    );
    ui.painter().text(
        rect.center(),
        Align2::CENTER_CENTER,
        text,
        font,
        if enabled {
            Color32::from_rgb(0xf0, 0xf1, 0xf3)
        } else {
            Color32::from_rgb(0x80, 0x84, 0x8b)
        },
    );
    response
}

/// チェックボックス（C++ 版の ModernDialogCheckBox）。押したら値を反転して返す。
pub fn dialog_check(ui: &mut egui::Ui, checked: &mut bool, text: &str) -> egui::Response {
    let font = FontId::proportional(12.0);
    let text_width = ui
        .painter()
        .layout_no_wrap(text.to_owned(), font.clone(), theme::TEXT)
        .size()
        .x;
    let (rect, response) =
        ui.allocate_exact_size(Vec2::new(18.0 + 8.0 + text_width, 30.0), Sense::click());
    response.widget_info(|| {
        egui::WidgetInfo::selected(egui::WidgetType::Checkbox, ui.is_enabled(), *checked, text)
    });
    if response.clicked() {
        *checked = !*checked;
    }
    let indicator = Rect::from_min_size(
        egui::pos2(rect.min.x, rect.center().y - 9.0),
        Vec2::splat(18.0),
    );
    ui.painter().rect(
        indicator,
        4.0,
        if *checked {
            Color32::from_rgb(0x37, 0x6f, 0xa8)
        } else {
            Color32::from_rgb(0x1d, 0x21, 0x27)
        },
        Stroke::new(1.0, Color32::from_rgb(0x56, 0x5d, 0x66)),
        egui::StrokeKind::Inside,
    );
    if *checked {
        ui.painter().add(egui::Shape::line(
            vec![
                indicator.min + Vec2::new(4.5, 9.0),
                indicator.min + Vec2::new(7.5, 12.5),
                indicator.min + Vec2::new(13.5, 5.5),
            ],
            Stroke::new(1.8, Color32::WHITE),
        ));
    }
    ui.painter().text(
        egui::pos2(indicator.max.x + 8.0, rect.center().y),
        Align2::LEFT_CENTER,
        text,
        font,
        Color32::from_rgb(0xc8, 0xcb, 0xd1),
    );
    response
}

/// 選択肢の一覧（C++ 版の ModernDialogComboBox）。選び直したら、その index を返す。
pub fn choice_box(
    ui: &mut egui::Ui,
    id: egui::Id,
    options: &[&str],
    current: usize,
    width: f32,
) -> Option<usize> {
    let shown = options.get(current).copied().unwrap_or_default();
    let (rect, response) = ui.allocate_exact_size(Vec2::new(width, 31.0), Sense::click());
    response.widget_info(|| {
        let mut info = egui::WidgetInfo::labeled(egui::WidgetType::ComboBox, true, shown);
        info.current_text_value = Some(shown.to_owned());
        info
    });
    let border = if response.hovered() {
        Color32::from_rgb(0x6c, 0xa9, 0xe6)
    } else {
        Color32::from_rgb(0x51, 0x56, 0x5e)
    };
    ui.painter().rect(
        rect,
        4.0,
        Color32::from_rgb(0x1d, 0x21, 0x27),
        Stroke::new(1.0, border),
        egui::StrokeKind::Inside,
    );
    ui.painter().text(
        rect.left_center() + Vec2::new(10.0, 0.0),
        Align2::LEFT_CENTER,
        shown,
        FontId::proportional(12.0),
        Color32::from_rgb(0xf0, 0xf1, 0xf3),
    );
    icons::paint_chevron(
        ui.painter(),
        Rect::from_center_size(
            egui::pos2(rect.max.x - 14.0, rect.center().y),
            Vec2::splat(12.0),
        ),
        icons::Chevron::UpDown,
        theme::TEXT_ICON,
    );
    let mut chosen = None;
    egui::Popup::from_toggle_button_response(&response)
        .id(id.with("popup"))
        .width(width)
        .show(|ui| {
            for (index, option) in options.iter().enumerate() {
                if ui.selectable_label(index == current, *option).clicked() {
                    chosen = Some(index);
                }
            }
        });
    chosen.filter(|index| *index != current)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn field(suffix: &str, decimals: usize) -> NumberField<'_> {
        NumberField {
            minimum: -10.0,
            maximum: 200.0,
            decimals,
            suffix,
            ..NumberField::default()
        }
    }

    #[test]
    fn a_typed_number_may_end_with_its_unit_but_nothing_else() {
        assert_eq!(field(" %", 0).parse("85"), Some(85.0));
        assert_eq!(field(" %", 0).parse(" 85 % "), Some(85.0));
        assert_eq!(field(" %", 1).parse("12.34"), Some(12.3));
        // 範囲の外は端へ寄せる。
        assert_eq!(field(" %", 0).parse("500"), Some(200.0));
        assert_eq!(field(" %", 0).parse("50foo"), None);
        assert_eq!(field(" %", 0).parse(""), None);
        assert_eq!(field(" %", 0).parse("inf"), None);
    }

    #[test]
    fn colors_round_trip_through_the_stored_argb_text() {
        let color = argb_to_color(0x80FF_1018);
        assert_eq!(color_to_argb(color), "#80FF1018");
        assert_eq!(color_to_argb(argb_to_color(0xFF10_1830)), "#FF101830");
    }
}
