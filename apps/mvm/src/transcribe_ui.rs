//! 文字起こしの dialog（C++ 版の TranscribeDialog と同じ並び）。対象・model・whisper-cli・実行経路・言語・
//! 語彙ヒント → 認識 → 候補の確認と修正 → 適用。
//!
//! 指定した tool の場所・言語・GPU は端末の設定に保存し、次に開いたときの初期値にする。

use std::path::PathBuf;

use eframe::egui::{self, Color32, Vec2};
use mvm_app::{
    MAXIMUM_PROMPT_CHARS, Preferences, Session, TranscriptionRequest, TranscriptionSource,
};
use mvm_project::{MediaKind, TimelineClipKind};

use crate::dialog;
use crate::theme;
use crate::widgets::{self, ButtonKind};

#[derive(Clone, Debug, PartialEq, Eq)]
enum Choice {
    Clip(String),
    Media(String),
}

/// 候補の欄の文字。開始・終了は数字だけを受け付ける。
#[derive(Clone)]
struct CandidateDraft {
    start: String,
    end: String,
    content: String,
}

#[derive(Default)]
pub struct TranscribeDialog {
    open: bool,
    choice: Option<Choice>,
    /// 素材を置く frame（素材を対象にしたとき）。
    place_text: String,
    preferences: Preferences,
    prompt: String,
    replace: bool,
    error: String,
    /// 端末の設定を読めたか。読めなければ「既定に戻す」まで保存（と認識）をさせない。
    preferences_loaded: bool,
    drafts: Vec<CandidateDraft>,
    drafts_loaded: bool,
}

impl TranscribeDialog {
    /// `clip_id` を指定すると、その clip を対象にして開く。
    pub fn open(&mut self, session: &Session, clip_id: Option<String>) {
        self.error.clear();
        match Preferences::load() {
            Ok(preferences) => {
                self.preferences = preferences;
                self.preferences_loaded = true;
            }
            Err(error) => {
                self.error = error.message().to_owned();
                self.preferences_loaded = false;
            }
        }
        self.choice = clip_id.map(Choice::Clip).or_else(|| self.choice.take());
        self.place_text = session.playhead_frame().to_string();
        self.drafts_loaded = false;
        // 適用は既定で字幕を置き換える（C++ 版と同じ）。
        self.replace = true;
        self.open = true;
    }
}

pub fn show(ui: &mut egui::Ui, session: &mut Session, dialog: &mut TranscribeDialog) {
    if let Some(result) = session.poll_transcription() {
        dialog.drafts_loaded = false;
        if let Err(error) = result {
            dialog.error = error.message().to_owned();
        }
    }
    if !dialog.open {
        return;
    }
    let transcribing = session.transcription_progress().is_some();
    let has_candidates = session.transcription_candidates().is_some();
    let mut close_requested = false;
    let mut apply = false;
    let mut replace = dialog.replace;
    let screen = ui.ctx().content_rect();
    let width = (screen.width() - 24.0).clamp(300.0, 650.0);
    let height = (screen.height() - 24.0 - 48.0 - 58.0 - 34.0).clamp(200.0, 640.0 - 140.0);
    let ((), close) = dialog::show(
        ui.ctx(),
        "transcribe",
        "ローカル文字起こし",
        width,
        |ui| {
            egui::ScrollArea::vertical()
                .id_salt("transcribe_scroll")
                .max_height(height)
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    ui.add_enabled_ui(!transcribing, |ui| settings(ui, session, dialog));
                    progress_row(ui, session, dialog, transcribing);
                    if !dialog.error.is_empty() {
                        ui.colored_label(Color32::from_rgb(0xe8, 0x9a, 0x96), &dialog.error);
                    }
                    ui.label(
                        egui::RichText::new(
                            "候補の開始・終了フレームと本文を修正し、適用してください。",
                        )
                        .color(theme::TEXT_SECONDARY),
                    );
                    ui.add_enabled_ui(has_candidates, |ui| candidates(ui, session, dialog));
                });
        },
        |ui| {
            let button = ui.add_enabled_ui(has_candidates, |ui| {
                widgets::dialog_button(ui, "候補を適用", ButtonKind::Prominent)
            });
            apply = button.inner.clicked();
            close_requested = widgets::dialog_button(ui, "閉じる", ButtonKind::Normal).clicked();
            let mode = usize::from(!replace);
            let chosen = ui
                .add_enabled_ui(has_candidates, |ui| {
                    widgets::choice_box(
                        ui,
                        egui::Id::new("transcribe_mode"),
                        &["字幕を置換", "字幕へ追加"],
                        mode,
                        120.0,
                    )
                })
                .inner;
            if let Some(index) = chosen {
                replace = index == 0;
            }
        },
    );
    dialog.replace = replace;
    if apply {
        match session.apply_transcription(dialog.replace) {
            Ok(()) => dialog.open = false,
            Err(error) => dialog.error = error.message().to_owned(),
        }
    }
    if close_requested || close {
        session.cancel_transcription();
        session.discard_transcription();
        dialog.open = false;
    }
}

fn choice_label(session: &Session, choice: &Choice) -> String {
    let project = session.project();
    match choice {
        Choice::Clip(id) => project
            .clip(id)
            .map(|clip| {
                let track = project
                    .track(clip.track)
                    .map(|track| track.name.clone())
                    .unwrap_or_default();
                format!(
                    "クリップ: {} （{track}・{} frame）",
                    clip.name, clip.timeline_start_frame
                )
            })
            .unwrap_or_else(|| "クリップ: （無し）".to_owned()),
        Choice::Media(id) => project
            .media_items
            .iter()
            .find(|item| item.id == *id)
            .map(|item| format!("素材: {}", item.name))
            .unwrap_or_else(|| "素材: （無し）".to_owned()),
    }
}

/// 対象・モデル・実行経路・言語・語彙ヒント・配置。
fn settings(ui: &mut egui::Ui, session: &Session, dialog: &mut TranscribeDialog) {
    let project = session.project();
    let mut choices: Vec<Choice> = project
        .timeline_clips
        .iter()
        .filter(|clip| matches!(clip.kind, TimelineClipKind::Audio | TimelineClipKind::Video))
        .map(|clip| Choice::Clip(clip.id.clone()))
        .collect();
    choices.extend(
        project
            .media_items
            .iter()
            .filter(|item| matches!(item.kind, MediaKind::Audio | MediaKind::Video))
            .map(|item| Choice::Media(item.id.clone())),
    );
    let labels: Vec<String> = choices
        .iter()
        .map(|choice| choice_label(session, choice))
        .collect();
    ui.horizontal(|ui| {
        ui.label("対象");
        let options: Vec<&str> = labels.iter().map(String::as_str).collect();
        let current = dialog
            .choice
            .as_ref()
            .and_then(|choice| choices.iter().position(|other| other == choice));
        if current.is_none() && !choices.is_empty() {
            dialog.choice = choices.first().cloned();
        }
        if let Some(index) = widgets::choice_box(
            ui,
            egui::Id::new("transcribe_source"),
            &options,
            current.unwrap_or(0),
            ui.available_width(),
        ) {
            dialog.choice = choices.get(index).cloned();
        }
    });
    path_row(
        ui,
        "モデルを選択",
        &mut dialog.preferences.whisper_model,
        "認識モデル",
        "bin",
        "多言語モデルを指定してください (精度重視なら ggml-large-v3.bin と Vulkan)",
    );
    path_row(
        ui,
        "whisper-cli を選択",
        &mut dialog.preferences.whisper_cli,
        "whisper-cli",
        "exe",
        "whisper.cpp の whisper-cli.exe を指定してください",
    );
    egui::Grid::new("transcribe_grid")
        .num_columns(2)
        .spacing([12.0, 8.0])
        .show(ui, |ui| {
            let field_width = ui.available_width().max(240.0);
            ui.label("実行経路");
            let backend = usize::from(dialog.preferences.whisper_gpu);
            if let Some(index) = widgets::choice_box(
                ui,
                egui::Id::new("transcribe_backend"),
                &["CPU", "Vulkan"],
                backend,
                field_width,
            ) {
                dialog.preferences.whisper_gpu = index == 1;
            }
            ui.end_row();
            ui.label("言語コード");
            ui.add_sized(
                Vec2::new(field_width, 34.0),
                egui::TextEdit::singleline(&mut dialog.preferences.whisper_language)
                    .hint_text("ja / auto / en")
                    .margin(Vec2::new(10.0, 8.0)),
            );
            ui.end_row();
            ui.label("語彙ヒント");
            let prompt = ui.add_sized(
                Vec2::new(field_width, 34.0),
                egui::TextEdit::singleline(&mut dialog.prompt)
                    .char_limit(MAXIMUM_PROMPT_CHARS)
                    .hint_text(format!(
                        "固有名詞・専門用語など (任意、{MAXIMUM_PROMPT_CHARS}文字まで)"
                    ))
                    .margin(Vec2::new(10.0, 8.0)),
            );
            let _ = prompt;
            ui.end_row();
            ui.label("素材の配置フレーム");
            let placement = ui.add_sized(
                Vec2::new(field_width, 34.0),
                egui::TextEdit::singleline(&mut dialog.place_text).margin(Vec2::new(10.0, 8.0)),
            );
            if placement.changed() {
                dialog
                    .place_text
                    .retain(|character| character.is_ascii_digit());
            }
            ui.end_row();
        });
    ui.label(
        "クリップはトリム・速度・配置を反映し、作った字幕はクリップの移動に追従します。素材は全音声を処理します。",
    );
    if !dialog.preferences_loaded {
        ui.colored_label(
            Color32::from_rgb(0xe8, 0x9a, 0x96),
            "端末の設定を読めません。file を直して開き直すか、既定に戻してください（他の設定も既定に戻ります）",
        );
        if widgets::dialog_button(ui, "設定を既定に戻す", ButtonKind::Normal).clicked() {
            dialog.preferences = Preferences::default();
            dialog.preferences_loaded = true;
            dialog.error.clear();
        }
    }
}

/// 認識開始・キャンセルと進み具合。
fn progress_row(
    ui: &mut egui::Ui,
    session: &mut Session,
    dialog: &mut TranscribeDialog,
    transcribing: bool,
) {
    let ready = dialog.choice.is_some()
        && !session.is_exporting()
        && dialog.preferences_loaded
        && !dialog.preferences.whisper_model.trim().is_empty();
    let mut start = false;
    ui.horizontal(|ui| {
        let label = if transcribing {
            "認識中…"
        } else {
            "認識開始"
        };
        let button = ui.add_enabled_ui(!transcribing && ready, |ui| {
            widgets::dialog_button(ui, label, ButtonKind::Prominent)
        });
        start = button.inner.clicked();
        let cancel = ui.add_enabled_ui(transcribing, |ui| {
            widgets::dialog_button(ui, "キャンセル", ButtonKind::Normal)
        });
        if cancel.inner.clicked() {
            session.cancel_transcription();
        }
        ui.vertical_centered_justified(|ui| {
            ui.add_space(12.0);
            dialog::progress_bar(ui, Some(session.transcription_progress().unwrap_or(0.0)));
        });
    });
    if transcribing {
        ui.ctx().request_repaint();
    }
    if !start {
        return;
    }
    dialog.error.clear();
    if let Err(error) = dialog.preferences.save() {
        dialog.error = error.message().to_owned();
        return;
    }
    let source = match dialog.choice.clone() {
        Some(Choice::Clip(clip_id)) => TranscriptionSource::Clip { clip_id },
        Some(Choice::Media(item_id)) => TranscriptionSource::Media {
            item_id,
            place_frame: dialog.place_text.parse().unwrap_or(0),
        },
        None => return,
    };
    let request = TranscriptionRequest {
        source,
        whisper_cli: PathBuf::from(dialog.preferences.whisper_cli.trim()),
        model: PathBuf::from(dialog.preferences.whisper_model.trim()),
        language: dialog.preferences.whisper_language.trim().to_owned(),
        gpu: dialog.preferences.whisper_gpu,
        prompt: dialog.prompt.clone(),
    };
    if let Err(error) = session.start_transcription(request) {
        dialog.error = error.message().to_owned();
    }
}

/// 選択のボタンと、選んだ file の場所。
fn path_row(
    ui: &mut egui::Ui,
    button: &str,
    value: &mut String,
    filter: &str,
    extension: &str,
    empty: &str,
) {
    ui.horizontal(|ui| {
        if widgets::dialog_button(ui, button, ButtonKind::Normal).clicked()
            && let Some(path) = rfd::FileDialog::new()
                .add_filter(filter, &[extension])
                .pick_file()
        {
            *value = path.display().to_string();
        }
        let text = if value.trim().is_empty() {
            empty
        } else {
            value.as_str()
        };
        ui.add(egui::Label::new(text).truncate());
    });
}

fn candidates(ui: &mut egui::Ui, session: &mut Session, dialog: &mut TranscribeDialog) {
    if !dialog.drafts_loaded {
        dialog.drafts = session
            .transcription_candidates()
            .unwrap_or_default()
            .iter()
            .map(|cue| CandidateDraft {
                start: cue.start_frame.to_string(),
                end: cue.end_frame.to_string(),
                content: cue.content.clone(),
            })
            .collect();
        dialog.drafts_loaded = true;
    }
    let fps = session.project().timeline_fps;
    let seconds = |frame: i64| {
        if fps.num > 0 {
            frame as f64 * fps.den as f64 / fps.num as f64
        } else {
            0.0
        }
    };
    let mut update = None;
    let mut remove = None;
    egui::Frame::new()
        .stroke(egui::Stroke::new(1.0, Color32::from_rgb(0x41, 0x45, 0x4b)))
        .corner_radius(4)
        .inner_margin(egui::Margin::same(6))
        .show(ui, |ui| {
            egui::ScrollArea::vertical()
                .id_salt("transcribe_candidates")
                .min_scrolled_height(180.0)
                .max_height(260.0)
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    for (index, draft) in dialog.drafts.iter_mut().enumerate() {
                        ui.horizontal(|ui| {
                            for text in [&mut draft.start, &mut draft.end] {
                                let field = ui.add_sized(
                                    Vec2::new(78.0, 34.0),
                                    egui::TextEdit::singleline(text).margin(Vec2::new(10.0, 8.0)),
                                );
                                if field.changed() {
                                    text.retain(|character| character.is_ascii_digit());
                                }
                            }
                            if let (Ok(start), Ok(end)) =
                                (draft.start.parse::<i64>(), draft.end.parse::<i64>())
                            {
                                ui.label(
                                    egui::RichText::new(format!(
                                        "{:.1}–{:.1} 秒",
                                        seconds(start),
                                        seconds(end)
                                    ))
                                    .monospace()
                                    .size(12.0)
                                    .color(theme::TEXT_SECONDARY),
                                );
                            }
                        });
                        ui.add_sized(
                            Vec2::new(ui.available_width(), 34.0),
                            egui::TextEdit::singleline(&mut draft.content)
                                .margin(Vec2::new(10.0, 8.0)),
                        );
                        ui.horizontal(|ui| {
                            if widgets::dialog_button(ui, "更新", ButtonKind::Normal).clicked() {
                                update = Some(index);
                            }
                            if widgets::dialog_button(ui, "削除", ButtonKind::Destructive).clicked()
                            {
                                remove = Some(index);
                            }
                        });
                        ui.add_space(4.0);
                    }
                });
        });
    if let Some(index) = update
        && let Some(draft) = dialog.drafts.get(index).cloned()
    {
        let (Ok(start), Ok(end)) = (draft.start.parse::<i64>(), draft.end.parse::<i64>()) else {
            dialog.error = "開始・終了フレームを数字で入力してください".to_owned();
            return;
        };
        if let Err(error) = session.edit_transcription_candidate(index, start, end, &draft.content)
        {
            dialog.error = error.message().to_owned();
        }
    }
    if let Some(index) = remove {
        session.remove_transcription_candidate(index);
        dialog.drafts_loaded = false;
    }
}
