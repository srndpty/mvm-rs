//! 文字起こしの dialog。対象・whisper-cli・model・言語・GPU・語彙ヒント → 認識 → 候補の確認と修正 → 適用。
//!
//! 指定した tool の場所・言語・GPU は端末の設定に保存し、次に開いたときの初期値にする。

use std::path::PathBuf;

use eframe::egui;
use mvm_app::{
    MAXIMUM_PROMPT_CHARS, Preferences, Session, TranscriptionRequest, TranscriptionSource,
};
use mvm_project::{MediaKind, TimelineClipKind};

#[derive(Clone, Debug, PartialEq, Eq)]
enum Choice {
    Clip(String),
    Media(String),
}

#[derive(Clone)]
struct CandidateDraft {
    start: i64,
    end: i64,
    content: String,
}

#[derive(Default)]
pub struct TranscribeDialog {
    open: bool,
    choice: Option<Choice>,
    place_frame: i64,
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
        self.place_frame = session.playhead_frame();
        self.drafts_loaded = false;
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
    let mut open = true;
    egui::Window::new("自動文字起こし")
        .collapsible(false)
        .resizable(true)
        .default_width(520.0)
        .open(&mut open)
        .show(ui.ctx(), |ui| {
            if let Some(progress) = session.transcription_progress() {
                ui.label("認識しています");
                ui.add(egui::ProgressBar::new(progress).show_percentage());
                if ui.button("取消").clicked() {
                    session.cancel_transcription();
                }
                ui.ctx().request_repaint();
                return;
            }
            if session.transcription_candidates().is_some() {
                show_candidates(ui, session, dialog);
            } else {
                show_settings(ui, session, dialog);
            }
        });
    if !open {
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

fn show_settings(ui: &mut egui::Ui, session: &mut Session, dialog: &mut TranscribeDialog) {
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
    let selected = dialog
        .choice
        .as_ref()
        .map(|choice| choice_label(session, choice))
        .unwrap_or_else(|| "対象を選んでください".to_owned());
    egui::ComboBox::from_label("対象")
        .selected_text(selected)
        .width(360.0)
        .show_ui(ui, |ui| {
            for choice in &choices {
                let label = choice_label(session, choice);
                ui.selectable_value(&mut dialog.choice, Some(choice.clone()), label);
            }
        });
    if matches!(dialog.choice, Some(Choice::Media(_))) {
        ui.add(
            egui::DragValue::new(&mut dialog.place_frame)
                .range(0..=i64::MAX)
                .prefix("素材の配置 ")
                .suffix(" frame"),
        );
    }
    path_row(
        ui,
        "whisper-cli",
        &mut dialog.preferences.whisper_cli,
        "exe",
    );
    path_row(
        ui,
        "認識 model",
        &mut dialog.preferences.whisper_model,
        "bin",
    );
    ui.horizontal(|ui| {
        ui.label("言語");
        ui.add(
            egui::TextEdit::singleline(&mut dialog.preferences.whisper_language)
                .desired_width(60.0),
        )
        .on_hover_text("ja・en など。自動検出は auto");
        ui.checkbox(&mut dialog.preferences.whisper_gpu, "GPU（Vulkan）で認識")
            .on_hover_text("使えなければ失敗します（CPU へ切り替えません）");
    });
    ui.label(format!(
        "語彙ヒント（固有名詞など、{MAXIMUM_PROMPT_CHARS} 文字まで）"
    ));
    ui.add(
        egui::TextEdit::multiline(&mut dialog.prompt)
            .desired_rows(2)
            .desired_width(f32::INFINITY),
    );
    if !dialog.error.is_empty() {
        ui.colored_label(egui::Color32::from_rgb(240, 120, 110), &dialog.error);
    }
    if !dialog.preferences_loaded {
        ui.colored_label(
            egui::Color32::from_rgb(240, 120, 110),
            "端末の設定を読めません。file を直して開き直すか、既定に戻してください（他の設定も既定に戻ります）",
        );
        if ui.button("設定を既定に戻す").clicked() {
            dialog.preferences = Preferences::default();
            dialog.preferences_loaded = true;
            dialog.error.clear();
        }
    }
    let ready = dialog.choice.is_some() && !session.is_exporting() && dialog.preferences_loaded;
    if ui
        .add_enabled(ready, egui::Button::new("認識開始"))
        .clicked()
    {
        dialog.error.clear();
        if let Err(error) = dialog.preferences.save() {
            dialog.error = error.message().to_owned();
            return;
        }
        let source = match dialog.choice.clone() {
            Some(Choice::Clip(clip_id)) => TranscriptionSource::Clip { clip_id },
            Some(Choice::Media(item_id)) => TranscriptionSource::Media {
                item_id,
                place_frame: dialog.place_frame,
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
}

fn path_row(ui: &mut egui::Ui, label: &str, value: &mut String, extension: &str) {
    ui.horizontal(|ui| {
        ui.label(label);
        ui.add(egui::TextEdit::singleline(value).desired_width(300.0));
        if ui.button("選ぶ").clicked()
            && let Some(path) = rfd::FileDialog::new()
                .add_filter(label, &[extension])
                .pick_file()
        {
            *value = path.display().to_string();
        }
    });
}

fn show_candidates(ui: &mut egui::Ui, session: &mut Session, dialog: &mut TranscribeDialog) {
    if !dialog.drafts_loaded {
        dialog.drafts = session
            .transcription_candidates()
            .unwrap_or_default()
            .iter()
            .map(|cue| CandidateDraft {
                start: cue.start_frame,
                end: cue.end_frame,
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
    egui::ScrollArea::vertical()
        .max_height(360.0)
        .show(ui, |ui| {
            for (index, draft) in dialog.drafts.iter_mut().enumerate() {
                ui.horizontal(|ui| {
                    ui.add(egui::DragValue::new(&mut draft.start).range(0..=i64::MAX));
                    ui.label(format!("{:.2}", seconds(draft.start)));
                    ui.add(egui::DragValue::new(&mut draft.end).range(0..=i64::MAX));
                    ui.label(format!("{:.2} 秒", seconds(draft.end)));
                });
                ui.add(egui::TextEdit::multiline(&mut draft.content).desired_rows(1));
                ui.horizontal(|ui| {
                    if ui.button("更新").clicked() {
                        update = Some(index);
                    }
                    if ui.button("削除").clicked() {
                        remove = Some(index);
                    }
                });
                ui.separator();
            }
        });
    if let Some(index) = update
        && let Some(draft) = dialog.drafts.get(index).cloned()
        && let Err(error) =
            session.edit_transcription_candidate(index, draft.start, draft.end, &draft.content)
    {
        dialog.error = error.message().to_owned();
    }
    if let Some(index) = remove {
        session.remove_transcription_candidate(index);
        dialog.drafts_loaded = false;
    }
    if !dialog.error.is_empty() {
        ui.colored_label(egui::Color32::from_rgb(240, 120, 110), &dialog.error);
    }
    ui.horizontal(|ui| {
        ui.radio_value(&mut dialog.replace, true, "字幕を置換");
        ui.radio_value(&mut dialog.replace, false, "字幕へ追加");
        if ui.button("候補を適用").clicked() {
            match session.apply_transcription(dialog.replace) {
                Ok(()) => dialog.open = false,
                Err(error) => dialog.error = error.message().to_owned(),
            }
        }
        if ui.button("破棄").clicked() {
            session.discard_transcription();
        }
    });
}
