//! 左パネルの「字幕」。一覧・編集欄・共通書式・SRT の読み書き・文字起こしの入口。
//!
//! 一覧は表示する行だけを作る。編集欄の値は「更新」で 1 回だけ commit し、Esc で確定済みの値へ戻す。
//! 編集欄の下書きは、編集している字幕そのもの（区間・本文）が Project で変わったときだけ読み直す。共通書式の
//! 変更のように他の所だけが変わったときは、未確定の本文を残す。

use eframe::egui::{self, Align2, Color32, FontId, Pos2, Rect, Sense, Stroke, Vec2};
use mvm_app::Session;
use mvm_project::subtitles::subtitle_contains_frame;

use crate::text_style_ui::{self, StyleChange, StyleValues};
use crate::transcribe_ui::TranscribeDialog;
use crate::widgets::{self, ButtonKind, NumberEdit, NumberField};
use crate::{icons, theme};

const ROW_HEIGHT: f32 = 30.0;
/// 「追加」で作る字幕の既定の尺（秒）。
const DEFAULT_SECONDS: i64 = 2;

#[derive(Default)]
pub struct SubtitlePanel {
    /// 編集欄が指す字幕。None なら新規の入力。
    editing: Option<String>,
    /// 編集欄を最後に確かめたときの [`Session::edit_revision`]。
    revision: u64,
    /// 編集欄へ読んだときの字幕の区間と本文（新規の入力なら None）。Project 側のこれが変わったら読み直す。
    loaded: Option<(i64, i64, String)>,
    /// 編集欄を読み込み済みか。false なら次の描画で読む（開いた直後・一覧で選んだ・追加した）。
    current: bool,
    content: String,
    /// 開始・終了の欄の文字。数字だけを受け付ける。
    start_text: String,
    end_text: String,
    /// 本文欄に最後にあったカーソル（文字の位置）。分割の位置に使う。
    cursor: Option<usize>,
    /// 共通書式の欄を開いているか。
    style_open: bool,
    /// SRT の読み込みで今の字幕に足す（既定は置き換える。C++ 版と同じ）。
    append_on_import: bool,
}

impl SubtitlePanel {
    /// 試験が本文の欄へ入力する。
    #[cfg(test)]
    pub fn set_content(&mut self, text: &str) {
        self.content = text.to_owned();
    }
}

pub fn show(
    ui: &mut egui::Ui,
    session: &mut Session,
    panel: &mut SubtitlePanel,
    transcribe: &mut TranscribeDialog,
) {
    ui.spacing_mut().item_spacing = Vec2::new(6.0, 6.0);
    ui.horizontal_wrapped(|ui| {
        if widgets::dialog_button(ui, "自動文字起こし", ButtonKind::Prominent).clicked() {
            transcribe.open(session, None);
        }
        if widgets::dialog_button(ui, "SRT読込", ButtonKind::Normal).clicked()
            && let Some(path) = rfd::FileDialog::new()
                .set_title("SRTを読み込む")
                .add_filter("字幕", &["srt"])
                .pick_file()
        {
            match session.import_srt(&path, !panel.append_on_import) {
                Ok(count) => session.set_alert(format!("SRT から {count} 件を読み込みました")),
                Err(error) => session.set_alert(error.message()),
            }
        }
        let mode = usize::from(panel.append_on_import);
        if let Some(choice) = widgets::choice_box(
            ui,
            egui::Id::new("subtitle_import_mode"),
            &["置換", "追加"],
            mode,
            92.0,
        ) {
            panel.append_on_import = choice == 1;
        }
        if widgets::dialog_button(ui, "SRT出力", ButtonKind::Normal).clicked()
            && let Some(path) = rfd::FileDialog::new()
                .set_title("SRTを書き出す")
                .add_filter("字幕", &["srt"])
                .set_file_name("subtitles.srt")
                .save_file()
            && let Err(error) = session.export_srt(&path)
        {
            session.set_alert(error.message());
        }
    });
    let visible = session
        .project()
        .subtitles
        .as_ref()
        .map(|track| track.visible);
    let mut shown = visible.unwrap_or(true);
    let check = ui.add_enabled_ui(visible.is_some(), |ui| {
        widgets::dialog_check(ui, &mut shown, "字幕を表示")
    });
    if check.inner.clicked()
        && let Err(error) = session.toggle_subtitle_visible()
    {
        session.set_alert(error.message());
    }
    style_editor(ui, session, panel);
    sync_editor(session, panel);
    cue_list(ui, session, panel);
    editor(ui, session, panel);
}

/// 選択が変わったか、編集している字幕が Project で変わったら（undo・更新・削除）、編集欄を読み直す。
fn sync_editor(session: &Session, panel: &mut SubtitlePanel) {
    let selected = session.subtitle_selection().first().cloned();
    let revision = session.edit_revision();
    if panel.current && selected == panel.editing {
        if panel.revision == revision {
            return;
        }
        // Project は変わったが、編集している字幕は読んだときのまま（共通書式・他の字幕の編集など）。
        if committed(session, panel.editing.as_deref()) == panel.loaded {
            panel.revision = revision;
            return;
        }
    }
    panel.current = true;
    panel.revision = revision;
    panel.editing = selected;
    load_editor(session, panel);
}

/// Project にある字幕の区間と本文。
fn committed(session: &Session, id: Option<&str>) -> Option<(i64, i64, String)> {
    let id = id?;
    session
        .project()
        .subtitles
        .as_ref()?
        .cues
        .iter()
        .find(|cue| cue.id == id)
        .map(|cue| (cue.start_frame, cue.end_frame, cue.content.clone()))
}

fn load_editor(session: &Session, panel: &mut SubtitlePanel) {
    panel.cursor = None;
    panel.loaded = committed(session, panel.editing.as_deref());
    if let Some((start, end, content)) = panel.loaded.clone() {
        panel.start_text = start.to_string();
        panel.end_text = end.to_string();
        panel.content = content;
    } else {
        let fps = session.project().timeline_fps;
        let frames = if fps.den > 0 {
            DEFAULT_SECONDS * fps.num / fps.den
        } else {
            1
        };
        panel.editing = None;
        let start = session.playhead_frame();
        panel.start_text = start.to_string();
        panel.end_text = (start + frames.max(1)).to_string();
        panel.content.clear();
    }
}

/// 開始・終了の欄の値。読めない・空・終了が開始以前なら理由を返す（最後に読めた古い値では確定しない）。
fn entered_range(start: &str, end: &str) -> Result<(i64, i64), &'static str> {
    match (start.parse::<i64>(), end.parse::<i64>()) {
        (Ok(start), Ok(end)) if start >= 0 && end > start => Ok((start, end)),
        (Ok(_), Ok(_)) => Err("終了フレームは開始フレームより後にしてください"),
        _ => Err("開始・終了フレームを数字で入力してください"),
    }
}

fn seconds(session: &Session, frame: i64) -> String {
    let fps = session.project().timeline_fps;
    if fps.num <= 0 {
        return String::new();
    }
    let value = frame as f64 * fps.den as f64 / fps.num as f64;
    if value >= 60.0 {
        let minutes = (value / 60.0).floor();
        format!("{}:{:04.1}", minutes as i64, value - minutes * 60.0)
    } else {
        format!("{value:.1}")
    }
}

fn cue_list(ui: &mut egui::Ui, session: &mut Session, panel: &mut SubtitlePanel) {
    let Some(track) = session.project().subtitles.as_ref() else {
        ui.label(
            egui::RichText::new("字幕がありません")
                .size(12.0)
                .color(theme::TEXT_SECONDARY),
        );
        return;
    };
    let rows: Vec<(String, String, String)> = track
        .cues
        .iter()
        .map(|cue| {
            (
                cue.id.clone(),
                format!(
                    "{}–{}",
                    seconds(session, cue.start_frame),
                    seconds(session, cue.end_frame)
                ),
                cue.content.replace('\n', " / "),
            )
        })
        .collect();
    let starts: Vec<i64> = track.cues.iter().map(|cue| cue.start_frame).collect();
    let playhead = session.playhead_frame();
    let mut clicked = None;
    let height = (ui.available_height() * 0.5).max(180.0);
    egui::Frame::new()
        .fill(Color32::from_rgb(0x18, 0x1c, 0x21))
        .stroke(Stroke::new(1.0, Color32::from_rgb(0x2c, 0x32, 0x3b)))
        .corner_radius(4)
        .inner_margin(egui::Margin::same(4))
        .show(ui, |ui| {
            egui::ScrollArea::vertical()
                .id_salt("subtitle_list")
                .max_height(height)
                .auto_shrink([false, true])
                .show_rows(ui, ROW_HEIGHT, rows.len(), |ui, range| {
                    ui.spacing_mut().item_spacing.y = 0.0;
                    for index in range {
                        let (id, time, text) = &rows[index];
                        let selected = session.subtitle_selection().contains(id);
                        let active = track_contains(session, id, playhead);
                        let (rect, response) = ui.allocate_exact_size(
                            Vec2::new(ui.available_width(), ROW_HEIGHT),
                            Sense::click(),
                        );
                        response.widget_info(|| {
                            egui::WidgetInfo::labeled(egui::WidgetType::Button, true, text)
                        });
                        let fill = if selected {
                            Color32::from_rgb(0x30, 0x45, 0x5c)
                        } else if response.hovered() {
                            Color32::from_rgb(0x26, 0x2c, 0x34)
                        } else {
                            Color32::TRANSPARENT
                        };
                        let painter = ui.painter_at(rect);
                        painter.rect_filled(rect, 3.0, fill);
                        if active {
                            painter.rect_filled(
                                Rect::from_min_size(rect.min, Vec2::new(3.0, rect.height())),
                                0.0,
                                Color32::from_rgb(0x5f, 0xb3, 0x7a),
                            );
                        }
                        painter.text(
                            rect.left_center() + Vec2::new(10.0, 0.0),
                            Align2::LEFT_CENTER,
                            time,
                            FontId::monospace(12.0),
                            if selected {
                                Color32::from_rgb(0xd6, 0xe6, 0xf7)
                            } else {
                                theme::TEXT_SECONDARY
                            },
                        );
                        painter.text(
                            rect.left_center() + Vec2::new(110.0, 0.0),
                            Align2::LEFT_CENTER,
                            text,
                            FontId::proportional(12.0),
                            Color32::from_rgb(0xee, 0xee, 0xee),
                        );
                        if response.clicked() {
                            let additive =
                                ui.input(|input| input.modifiers.ctrl || input.modifiers.shift);
                            clicked = Some((id.clone(), starts[index], additive));
                        }
                    }
                });
        });
    if let Some((id, start, additive)) = clicked {
        session.select_subtitle(&id, additive);
        if !session.is_playing() {
            session.seek(start);
        }
        panel.current = false;
    }
}

fn track_contains(session: &Session, id: &str, frame: i64) -> bool {
    session
        .project()
        .subtitles
        .as_ref()
        .and_then(|track| track.cues.iter().find(|cue| cue.id == id))
        .is_some_and(|cue| subtitle_contains_frame(cue, frame))
}

/// 開始・終了の frame の欄。数字だけを受け付ける。
fn frame_field(ui: &mut egui::Ui, text: &mut String, width: f32) -> egui::Response {
    let response = ui.add_sized(
        Vec2::new(width, 34.0),
        egui::TextEdit::singleline(text)
            .font(FontId::proportional(12.0))
            .margin(Vec2::new(10.0, 8.0)),
    );
    text.retain(|character| character.is_ascii_digit());
    response
}

fn editor(ui: &mut egui::Ui, session: &mut Session, panel: &mut SubtitlePanel) {
    let playing = session.is_playing();
    ui.add_enabled_ui(!playing, |ui| {
        let parsed = entered_range(&panel.start_text, &panel.end_text);
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("開始／終了フレーム").size(12.0));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if let Ok((start, end)) = parsed {
                    ui.label(
                        egui::RichText::new(format!(
                            "{}–{} 秒",
                            seconds(session, start),
                            seconds(session, end)
                        ))
                        .monospace()
                        .size(12.0)
                        .color(theme::TEXT_SECONDARY),
                    );
                }
            });
        });
        ui.horizontal(|ui| {
            let half = ((ui.available_width() - 6.0) / 2.0).max(40.0);
            frame_field(ui, &mut panel.start_text, half);
            frame_field(ui, &mut panel.end_text, half);
        });
        // 入力した後の値で確かめる。読めなければ「更新」「追加」を押せなくし、理由を出す。
        let range = entered_range(&panel.start_text, &panel.end_text);
        if let Err(reason) = range {
            ui.colored_label(Color32::from_rgb(0xef, 0x8b, 0x8b), reason);
        }
        let text = ui.add_sized(
            Vec2::new(ui.available_width(), 70.0),
            egui::TextEdit::multiline(&mut panel.content)
                .font(FontId::proportional(12.0))
                .margin(Vec2::splat(10.0))
                .hint_text("字幕本文"),
        );
        if text.has_focus() && ui.input(|input| input.key_pressed(egui::Key::Escape)) {
            load_editor(session, panel);
        }
        if text.has_focus() {
            panel.cursor = egui::TextEdit::load_state(ui.ctx(), text.id)
                .and_then(|state| state.cursor.char_range())
                .map(|range| range.primary.index.0);
        }
        let cursor = panel.cursor;
        let editing = panel.editing.clone();
        ui.horizontal_wrapped(|ui| {
            let update = ui.add_enabled_ui(editing.is_some() && range.is_ok(), |ui| {
                widgets::dialog_button(ui, "更新", ButtonKind::Prominent)
            });
            if update.inner.clicked()
                && let Some(id) = editing.as_deref()
                && let Ok((start, end)) = range
                && let Err(error) = session.update_subtitle(id, start, end, &panel.content)
            {
                session.set_alert(error.message());
            }
            let add = ui.add_enabled_ui(range.is_ok(), |ui| {
                widgets::dialog_button(ui, "追加", ButtonKind::Normal)
            });
            if add.inner.clicked()
                && let Ok((start, end)) = range
            {
                match session.add_subtitle(start, end, &panel.content) {
                    Ok(_) => panel.current = false,
                    Err(error) => session.set_alert(error.message()),
                }
            }
            let Some(id) = editing else {
                return;
            };
            if widgets::dialog_button(ui, "削除", ButtonKind::Destructive).clicked()
                && let Err(error) = session.delete_subtitles(std::slice::from_ref(&id))
            {
                session.set_alert(error.message());
            }
            let split = widgets::dialog_button(ui, "再生位置で分割", ButtonKind::Normal)
                .on_hover_text(
                    "本文は、本文欄に置いたカーソルの位置で分けます。置いていなければ再生位置に近い句読点で分けます",
                );
            if split.clicked()
                && let Err(error) = session.split_subtitle_at_playhead(&id, cursor)
            {
                session.set_alert(error.message());
            }
            if widgets::dialog_button(ui, "次と結合", ButtonKind::Normal).clicked()
                && let Err(error) = session.merge_subtitle_with_next(&id)
            {
                session.set_alert(error.message());
            }
        });
    });
    if playing {
        ui.weak("再生中は字幕を編集できません");
    }
}

/// 全字幕の共通書式。文字 clip のエフェクトコントロールと同じ書式の欄で編集し、1 つ変えるごとに確定する。
fn style_editor(ui: &mut egui::Ui, session: &mut Session, panel: &mut SubtitlePanel) {
    let style = session
        .project()
        .subtitles
        .as_ref()
        .map(|track| track.style.clone())
        .unwrap_or_default();
    let (rect, response) =
        ui.allocate_exact_size(Vec2::new(ui.available_width(), 26.0), Sense::click());
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, "共通書式"));
    if response.hovered() {
        ui.painter()
            .rect_filled(rect, 3.0, Color32::from_rgb(0x25, 0x2b, 0x33));
    }
    icons::paint_chevron(
        ui.painter(),
        Rect::from_center_size(rect.left_center() + Vec2::new(10.0, 0.0), Vec2::splat(11.0)),
        if panel.style_open {
            icons::Chevron::Down
        } else {
            icons::Chevron::Right
        },
        theme::TEXT_ICON,
    );
    let title = ui.painter().text(
        rect.left_center() + Vec2::new(22.0, 0.0),
        Align2::LEFT_CENTER,
        "共通書式",
        theme::bold(12.0),
        theme::TEXT,
    );
    ui.painter().text(
        Pos2::new(title.max.x + 8.0, rect.center().y),
        Align2::LEFT_CENTER,
        format!("{}・{}px", style.font_family, style.font_size),
        FontId::proportional(11.0),
        Color32::from_rgb(0x7d, 0x85, 0x90),
    );
    if response.clicked() {
        panel.style_open = !panel.style_open;
    }
    if !panel.style_open {
        return;
    }
    let values = StyleValues {
        font_family: &style.font_family,
        font_size: style.font_size,
        bold: style.bold,
        color: &style.color,
        outline_color: &style.outline_color,
        outline_width: style.outline_width,
        background_color: &style.background_color,
    };
    let id = egui::Id::new("subtitle_style");
    let mut alignment = None;
    let mut margins: (Option<NumberEdit>, Option<NumberEdit>) = (None, None);
    let mut edit = text_style_ui::show(
        ui,
        id,
        &values,
        session.project().output_height,
        |ui| {
            for (value, tip) in [
                ("left", "左揃え"),
                ("center", "中央揃え"),
                ("right", "右揃え"),
            ] {
                if align_button(ui, value, style.alignment == value, tip).clicked()
                    && style.alignment != value
                {
                    alignment = Some(value);
                }
            }
        },
        |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                text_style_ui::section_label(ui, "余白", 38.0);
                let half = ((ui.available_width() - 6.0) / 2.0).max(40.0);
                let side = NumberField {
                    label: "左右",
                    inline_label: 28.0,
                    value: (style.side_margin * 100.0).round(),
                    minimum: 0.0,
                    maximum: 49.0,
                    step: 0.2,
                    suffix: " %",
                    width: half,
                    ..NumberField::default()
                }
                .show(ui, id.with("side"));
                let bottom = NumberField {
                    label: "下",
                    inline_label: 20.0,
                    value: (style.bottom_margin * 100.0).round(),
                    minimum: 0.0,
                    maximum: 99.0,
                    step: 0.2,
                    suffix: " %",
                    width: half,
                    ..NumberField::default()
                }
                .show(ui, id.with("bottom"));
                margins = (side, bottom);
            });
        },
    );
    let mut next = style.clone();
    let mut previewed = style.clone();
    let mut preview = false;
    if let Some(value) = alignment {
        next.alignment = value.to_owned();
    }
    // 余白は % で見せ、0..1 で持つ。ドラッグ中は preview だけ、離したら確定する。
    for (index, margin) in [margins.0, margins.1].into_iter().enumerate() {
        let target = |style: &mut mvm_project::SubtitleStyle, value: f64| {
            if index == 0 {
                style.side_margin = value / 100.0;
            } else {
                style.bottom_margin = value / 100.0;
            }
        };
        match margin {
            Some(NumberEdit::Commit(value)) => target(&mut next, value),
            Some(NumberEdit::Preview(value)) => {
                target(&mut previewed, value);
                preview = true;
            }
            Some(NumberEdit::Cancel) => edit.cancel = true,
            Some(NumberEdit::Start) | None => {}
        }
    }
    for change in &edit.commits {
        apply_style(&mut next, change);
    }
    for change in &edit.previews {
        apply_style(&mut previewed, change);
        preview = true;
    }
    let result = if next != style {
        session.set_subtitle_style(next)
    } else if preview {
        session.preview_subtitle_style(previewed)
    } else {
        if edit.cancel {
            session.cancel_preview();
        }
        Ok(())
    };
    if let Err(error) = result {
        session.set_alert(error.message());
    }
}

/// 書式の変更を字幕の共通書式へ当てる。
fn apply_style(style: &mut mvm_project::SubtitleStyle, change: &StyleChange) {
    match change {
        StyleChange::FontFamily(value) => style.font_family = value.clone(),
        StyleChange::FontSize(value) => style.font_size = *value,
        StyleChange::Bold(value) => style.bold = *value,
        StyleChange::Color(value) => style.color = value.clone(),
        StyleChange::OutlineColor(value) => style.outline_color = value.clone(),
        StyleChange::OutlineWidth(value) => style.outline_width = *value,
        StyleChange::BackgroundColor(value) => style.background_color = value.clone(),
    }
}

/// 字幕の行揃えのボタン。行の長さの違う 3 本の線で揃えを示す。
fn align_button(ui: &mut egui::Ui, alignment: &str, current: bool, tip: &str) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(Vec2::new(30.0, 24.0), Sense::click());
    response
        .widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::Button, true, current, tip));
    let lit = current || response.hovered();
    let fill = if current {
        theme::STYLE_ON
    } else if response.hovered() {
        Color32::from_rgb(0x2a, 0x2f, 0x37)
    } else {
        Color32::TRANSPARENT
    };
    ui.painter().rect(
        rect,
        3.0,
        fill,
        Stroke::new(
            1.0,
            if lit {
                theme::FIELD_FOCUS
            } else {
                theme::TIMELINE_BORDER
            },
        ),
        egui::StrokeKind::Inside,
    );
    for (row, width) in [14.0f32, 9.0, 12.0].into_iter().enumerate() {
        let x = match alignment {
            "left" => 8.0,
            "right" => 22.0 - width,
            _ => 15.0 - width / 2.0,
        };
        ui.painter().rect_filled(
            Rect::from_min_size(
                rect.min + Vec2::new(x, 7.0 + row as f32 * 4.0),
                Vec2::new(width, 2.0),
            ),
            0.0,
            if lit {
                theme::TEXT
            } else {
                theme::TEXT_SECONDARY
            },
        );
    }
    response.on_hover_text(tip)
}

#[cfg(test)]
mod tests {
    use mvm_project::Project;

    use super::*;

    #[test]
    fn the_range_comes_from_the_fields_as_they_are_now() {
        assert_eq!(entered_range("10", "40"), Ok((10, 40)));
        assert_eq!(entered_range("0", "1"), Ok((0, 1)));
        // 空・読めない欄は、前に読めた値で確定させない。
        for (start, end) in [("", "40"), ("10", ""), ("", "")] {
            assert_eq!(
                entered_range(start, end),
                Err("開始・終了フレームを数字で入力してください"),
                "{start:?}–{end:?}"
            );
        }
        for (start, end) in [("40", "40"), ("40", "10")] {
            assert_eq!(
                entered_range(start, end),
                Err("終了フレームは開始フレームより後にしてください"),
                "{start:?}–{end:?}"
            );
        }
    }

    fn session_with_cue() -> (Session, String) {
        let mut session = Session::from_project(Project::default());
        let id = session.add_subtitle(0, 30, "元の本文").expect("字幕");
        (session, id)
    }

    #[test]
    fn a_style_change_keeps_the_unsaved_text() {
        let (mut session, _id) = session_with_cue();
        let mut panel = SubtitlePanel::default();
        sync_editor(&session, &mut panel);
        assert_eq!(panel.content, "元の本文");
        // 本文を直したが「更新」は押さず、共通書式を変えた。
        panel.content = "直した本文".to_owned();
        panel.start_text = "5".to_owned();
        let mut style = session
            .project()
            .subtitles
            .as_ref()
            .map(|track| track.style.clone())
            .unwrap_or_default();
        style.font_size += 4;
        session.set_subtitle_style(style).expect("書式");
        sync_editor(&session, &mut panel);
        assert_eq!(panel.content, "直した本文");
        assert_eq!(panel.start_text, "5");
    }

    #[test]
    fn an_undo_of_the_edited_cue_reloads_the_fields() {
        let (mut session, id) = session_with_cue();
        let mut panel = SubtitlePanel::default();
        sync_editor(&session, &mut panel);
        session
            .update_subtitle(&id, 0, 30, "確定した本文")
            .expect("更新");
        sync_editor(&session, &mut panel);
        assert_eq!(panel.content, "確定した本文");
        // 未確定の下書きがあっても、字幕そのものが undo で変われば Project の値へ読み直す。
        panel.content = "下書き".to_owned();
        session.undo().expect("戻す");
        sync_editor(&session, &mut panel);
        assert_eq!(panel.content, "元の本文");
        assert_eq!(
            (panel.start_text.as_str(), panel.end_text.as_str()),
            ("0", "30")
        );
    }
}
