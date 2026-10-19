//! 左パネルの「字幕」。一覧・編集欄・共通書式・SRT の読み書き・文字起こしの入口。
//!
//! 一覧は表示する行だけを作る。編集欄の値は「更新」で 1 回だけ commit し、Esc で確定済みの値へ戻す。

use eframe::egui::{self, Color32};
use mvm_app::Session;
use mvm_project::SubtitleStyle;
use mvm_project::subtitles::subtitle_contains_frame;

use crate::transcribe_ui::TranscribeDialog;
use crate::widgets::color_row;

const ROW_HEIGHT: f32 = 22.0;
/// 「追加」で作る字幕の既定の尺（秒）。
const DEFAULT_SECONDS: i64 = 2;

#[derive(Default)]
pub struct SubtitlePanel {
    /// 編集欄が指す字幕。None なら新規の入力。
    editing: Option<String>,
    revision: u64,
    start: i64,
    end: i64,
    content: String,
    /// 本文欄に最後にあったカーソル（文字の位置）。分割の位置に使う。
    cursor: Option<usize>,
    style: Option<SubtitleStyle>,
    replace_on_import: bool,
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
    ui.horizontal_wrapped(|ui| {
        if ui.button("自動文字起こし").clicked() {
            transcribe.open(session, None);
        }
        if ui.button("SRT 読み込み").clicked()
            && let Some(path) = rfd::FileDialog::new()
                .add_filter("SRT", &["srt"])
                .pick_file()
        {
            match session.import_srt(&path, panel.replace_on_import) {
                Ok(count) => session.set_alert(format!("SRT から {count} 件を読み込みました")),
                Err(error) => session.set_alert(error.message()),
            }
        }
        ui.checkbox(&mut panel.replace_on_import, "置換");
        if ui.button("SRT 書き出し").clicked()
            && let Some(path) = rfd::FileDialog::new()
                .add_filter("SRT", &["srt"])
                .set_file_name("subtitles.srt")
                .save_file()
            && let Err(error) = session.export_srt(&path)
        {
            session.set_alert(error.message());
        }
    });
    style_editor(ui, session, panel);
    ui.separator();
    sync_editor(session, panel);
    cue_list(ui, session, panel);
    ui.separator();
    editor(ui, session, panel);
}

/// 選択が変わったか Project が変わったら、編集欄を選択中の字幕で読み直す。
fn sync_editor(session: &Session, panel: &mut SubtitlePanel) {
    let selected = session.subtitle_selection().first().cloned();
    if selected == panel.editing && panel.revision == session.edit_revision() {
        return;
    }
    panel.revision = session.edit_revision();
    panel.editing = selected;
    load_editor(session, panel);
}

fn load_editor(session: &Session, panel: &mut SubtitlePanel) {
    panel.cursor = None;
    let cue = panel.editing.as_ref().and_then(|id| {
        session
            .project()
            .subtitles
            .as_ref()
            .and_then(|track| track.cues.iter().find(|cue| cue.id == *id))
    });
    if let Some(cue) = cue {
        panel.start = cue.start_frame;
        panel.end = cue.end_frame;
        panel.content = cue.content.clone();
    } else {
        let fps = session.project().timeline_fps;
        let frames = if fps.den > 0 {
            DEFAULT_SECONDS * fps.num / fps.den
        } else {
            1
        };
        panel.editing = None;
        panel.start = session.playhead_frame();
        panel.end = panel.start + frames.max(1);
        panel.content.clear();
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
        ui.label("字幕がありません");
        return;
    };
    let rows: Vec<(String, i64, i64, String)> = track
        .cues
        .iter()
        .map(|cue| {
            (
                cue.id.clone(),
                cue.start_frame,
                cue.end_frame,
                cue.content.replace('\n', " / "),
            )
        })
        .collect();
    let playhead = session.playhead_frame();
    let mut clicked = None;
    let height = (ui.available_height() * 0.5).max(180.0);
    egui::ScrollArea::vertical()
        .id_salt("subtitle_list")
        .max_height(height)
        .auto_shrink([false, true])
        .show_rows(ui, ROW_HEIGHT, rows.len(), |ui, range| {
            for (id, start, end, text) in &rows[range] {
                let selected = session.subtitle_selection().contains(id);
                let active = track_contains(session, id, playhead);
                ui.horizontal(|ui| {
                    let (bar, _) = ui.allocate_exact_size(
                        egui::vec2(4.0, ROW_HEIGHT - 4.0),
                        egui::Sense::hover(),
                    );
                    if active {
                        ui.painter()
                            .rect_filled(bar, 1.0, Color32::from_rgb(90, 200, 120));
                    }
                    let label = format!(
                        "{}–{}  {}",
                        seconds(session, *start),
                        seconds(session, *end),
                        text
                    );
                    if ui
                        .add(egui::Button::selectable(selected, label).truncate())
                        .clicked()
                    {
                        let additive =
                            ui.input(|input| input.modifiers.ctrl || input.modifiers.shift);
                        clicked = Some((id.clone(), *start, additive));
                    }
                });
            }
        });
    if let Some((id, start, additive)) = clicked {
        session.select_subtitle(&id, additive);
        if !session.is_playing() {
            session.seek(start);
        }
        panel.revision = 0;
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

fn editor(ui: &mut egui::Ui, session: &mut Session, panel: &mut SubtitlePanel) {
    let playing = session.is_playing();
    ui.add_enabled_ui(!playing, |ui| {
        ui.horizontal(|ui| {
            ui.label("開始");
            ui.add(
                egui::DragValue::new(&mut panel.start)
                    .range(0..=i64::MAX)
                    .suffix(" frame"),
            );
            ui.label(format!("{} 秒", seconds(session, panel.start)));
        });
        ui.horizontal(|ui| {
            ui.label("終了");
            ui.add(
                egui::DragValue::new(&mut panel.end)
                    .range(0..=i64::MAX)
                    .suffix(" frame"),
            );
            ui.label(format!("{} 秒", seconds(session, panel.end)));
        });
        let text = ui.add(
            egui::TextEdit::multiline(&mut panel.content)
                .desired_rows(3)
                .desired_width(f32::INFINITY)
                .hint_text("本文"),
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
        ui.horizontal_wrapped(|ui| {
            if ui.button("追加").clicked() {
                match session.add_subtitle(panel.start, panel.end, &panel.content) {
                    Ok(_) => panel.revision = 0,
                    Err(error) => session.set_alert(error.message()),
                }
            }
            let Some(id) = panel.editing.clone() else {
                return;
            };
            if ui.button("更新").clicked()
                && let Err(error) =
                    session.update_subtitle(&id, panel.start, panel.end, &panel.content)
            {
                session.set_alert(error.message());
            }
            if ui.button("削除").clicked()
                && let Err(error) = session.delete_subtitles(std::slice::from_ref(&id))
            {
                session.set_alert(error.message());
            }
            if ui.button("再生位置で分割").clicked() {
                // 本文欄にカーソルを置いていればその位置、無ければ句読点などで分ける。
                if let Err(error) = session.split_subtitle_at_playhead(&id, cursor) {
                    session.set_alert(error.message());
                }
            }
            if ui.button("次と結合").clicked()
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

fn style_editor(ui: &mut egui::Ui, session: &mut Session, panel: &mut SubtitlePanel) {
    egui::CollapsingHeader::new("共通書式")
        .id_salt("subtitle_style")
        .show(ui, |ui| {
            let current = session
                .project()
                .subtitles
                .as_ref()
                .map(|track| track.style.clone())
                .unwrap_or_default();
            let style = panel.style.get_or_insert_with(|| current.clone());
            ui.horizontal(|ui| {
                ui.label("フォント");
                ui.text_edit_singleline(&mut style.font_family);
            });
            ui.add(
                egui::DragValue::new(&mut style.font_size)
                    .range(8..=400)
                    .prefix("大きさ ")
                    .suffix(" px"),
            );
            ui.checkbox(&mut style.bold, "太字");
            color_row(ui, "文字色", &mut style.color);
            color_row(ui, "縁の色", &mut style.outline_color);
            ui.add(
                egui::DragValue::new(&mut style.outline_width)
                    .range(0..=40)
                    .prefix("縁の幅 ")
                    .suffix(" px"),
            );
            color_row(ui, "背景", &mut style.background_color);
            ui.horizontal(|ui| {
                ui.label("揃え");
                for (value, label) in [("left", "左"), ("center", "中央"), ("right", "右")] {
                    ui.selectable_value(&mut style.alignment, value.to_owned(), label);
                }
            });
            let mut side = style.side_margin * 100.0;
            if ui
                .add(
                    egui::DragValue::new(&mut side)
                        .range(0.0..=45.0)
                        .speed(0.1)
                        .prefix("左右の余白 ")
                        .suffix(" %"),
                )
                .changed()
            {
                style.side_margin = side / 100.0;
            }
            let mut bottom = style.bottom_margin * 100.0;
            if ui
                .add(
                    egui::DragValue::new(&mut bottom)
                        .range(0.0..=90.0)
                        .speed(0.1)
                        .prefix("下の余白 ")
                        .suffix(" %"),
                )
                .changed()
            {
                style.bottom_margin = bottom / 100.0;
            }
            let changed = *style != current;
            let draft = style.clone();
            let mut reset = false;
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(changed, egui::Button::new("書式を適用"))
                    .clicked()
                {
                    if let Err(error) = session.set_subtitle_style(draft) {
                        session.set_alert(error.message());
                    }
                    reset = true;
                } else if ui
                    .add_enabled(changed, egui::Button::new("元に戻す"))
                    .clicked()
                {
                    reset = true;
                }
            });
            if reset {
                panel.style = None;
            }
        });
}
