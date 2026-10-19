//! 自動音量調整の dialog。設定 → 解析（進捗と取消）→ 結果の確認 → 適用。
//!
//! 結果を適用するまで Project は変わらない。適用は 1 回の undo。

use eframe::egui;
use mvm_app::{AudioAdjustmentSettings, Session};

#[derive(Default)]
pub struct AudioAdjustDialog {
    open: bool,
    settings: Option<AudioAdjustmentSettings>,
    error: String,
}

impl AudioAdjustDialog {
    pub fn open(&mut self, session: &Session) {
        self.settings = Some(session.audio_adjustment_defaults());
        self.error.clear();
        self.open = true;
    }

    pub fn is_open(&self) -> bool {
        self.open
    }
}

pub fn show(ui: &mut egui::Ui, session: &mut Session, dialog: &mut AudioAdjustDialog) {
    if let Some(result) = session.poll_audio_adjustment()
        && let Err(error) = result
    {
        dialog.error = error.message().to_owned();
    }
    if !dialog.open {
        return;
    }
    let mut open = true;
    egui::Window::new("自動音量調整")
        .collapsible(false)
        .resizable(true)
        .open(&mut open)
        .show(ui.ctx(), |ui| {
            if let Some(progress) = session.audio_adjustment_progress() {
                ui.label("音声を解析しています");
                ui.add(egui::ProgressBar::new(progress).show_percentage());
                if ui.button("取消").clicked() {
                    session.cancel_audio_adjustment();
                }
                ui.ctx().request_repaint();
                return;
            }
            if session.audio_adjustment_result().is_some() {
                show_result(ui, session, dialog);
                return;
            }
            show_settings(ui, session, dialog);
        });
    if !open {
        session.cancel_audio_adjustment();
        session.discard_audio_adjustment();
        dialog.open = false;
    }
}

fn show_settings(ui: &mut egui::Ui, session: &mut Session, dialog: &mut AudioAdjustDialog) {
    let names: Vec<String> = session
        .project()
        .audio_tracks
        .iter()
        .map(|track| track.name.clone())
        .collect();
    let Some(settings) = dialog.settings.as_mut() else {
        return;
    };
    ui.label("BGM のトラック");
    egui::ComboBox::from_id_salt("adjust_bgm")
        .selected_text(
            usize::try_from(settings.bgm_track)
                .ok()
                .and_then(|index| names.get(index))
                .cloned()
                .unwrap_or_default(),
        )
        .show_ui(ui, |ui| {
            for (index, name) in names.iter().enumerate() {
                ui.selectable_value(&mut settings.bgm_track, index as i32, name);
            }
        });
    ui.label("声のトラック");
    ui.horizontal_wrapped(|ui| {
        for (index, name) in names.iter().enumerate() {
            let track = index as i32;
            if track == settings.bgm_track {
                continue;
            }
            let mut checked = settings.voice_tracks.contains(&track);
            if ui.checkbox(&mut checked, name).changed() {
                if checked {
                    settings.voice_tracks.push(track);
                    settings.voice_tracks.sort_unstable();
                } else {
                    settings.voice_tracks.retain(|other| *other != track);
                }
            }
        }
    });
    settings
        .voice_tracks
        .retain(|track| *track != settings.bgm_track);
    ui.separator();
    ui.checkbox(&mut settings.normalize, "ラウドネスを揃える");
    ui.add_enabled_ui(settings.normalize, |ui| {
        ui.add(
            egui::DragValue::new(&mut settings.voice_lufs)
                .range(-40.0..=-5.0)
                .speed(0.1)
                .prefix("声の目標 ")
                .suffix(" LUFS"),
        );
        ui.add(
            egui::DragValue::new(&mut settings.bgm_lufs)
                .range(-40.0..=-5.0)
                .speed(0.1)
                .prefix("BGM の目標 ")
                .suffix(" LUFS"),
        );
    });
    ui.checkbox(&mut settings.duck, "声の間だけ BGM を下げる");
    ui.add_enabled_ui(settings.duck, |ui| {
        ui.add(
            egui::DragValue::new(&mut settings.reduction_db)
                .range(0.0..=60.0)
                .speed(0.1)
                .prefix("下げる量 ")
                .suffix(" dB"),
        );
        ui.add(
            egui::DragValue::new(&mut settings.threshold_db)
                .range(-96.0..=0.0)
                .speed(0.1)
                .prefix("声とみなす音量 ")
                .suffix(" dBFS"),
        );
        ui.add(
            egui::DragValue::new(&mut settings.attack_ms)
                .range(1..=10_000)
                .prefix("下げ始め ")
                .suffix(" ms"),
        );
        ui.add(
            egui::DragValue::new(&mut settings.hold_ms)
                .range(0..=10_000)
                .prefix("保持 ")
                .suffix(" ms"),
        );
        ui.add(
            egui::DragValue::new(&mut settings.release_ms)
                .range(1..=10_000)
                .prefix("戻し ")
                .suffix(" ms"),
        );
    });
    if !dialog.error.is_empty() {
        ui.colored_label(egui::Color32::from_rgb(240, 120, 110), &dialog.error);
    }
    if ui.button("解析する").clicked() {
        let settings = settings.clone();
        dialog.error.clear();
        if let Err(error) = session.start_audio_adjustment(settings) {
            dialog.error = error.message().to_owned();
        }
    }
}

fn show_result(ui: &mut egui::Ui, session: &mut Session, dialog: &mut AudioAdjustDialog) {
    let Some(outcome) = session.audio_adjustment_result() else {
        return;
    };
    egui::ScrollArea::vertical()
        .max_height(320.0)
        .show(ui, |ui| {
            egui::Grid::new("adjust_result")
                .striped(true)
                .show(ui, |ui| {
                    ui.strong("clip");
                    ui.strong("役割");
                    ui.strong("ラウドネス");
                    ui.strong("true peak");
                    ui.strong("補正");
                    ui.end_row();
                    for clip in &outcome.clips {
                        ui.label(&clip.name);
                        ui.label(if clip.voice { "声" } else { "BGM" });
                        if clip.measurement.measurable {
                            ui.label(format!("{:.1} LUFS", clip.measurement.integrated_lufs));
                        } else {
                            ui.label("測れません（無音）");
                        }
                        ui.label(format!("{:.1} dBTP", clip.measurement.true_peak_db));
                        let mut note = format!("{:+.1} dB", clip.correction_db);
                        if clip.peak_limited {
                            note.push_str("（peak で制限）");
                        }
                        if clip.gain_limited {
                            note.push_str("（範囲で制限）");
                        }
                        ui.label(note);
                        ui.end_row();
                    }
                });
        });
    if outcome.settings.duck {
        ui.label(format!("声を検出した区間: {} 箇所", outcome.ranges.len()));
    }
    if !dialog.error.is_empty() {
        ui.colored_label(egui::Color32::from_rgb(240, 120, 110), &dialog.error);
    }
    ui.horizontal(|ui| {
        if ui.button("適用").clicked() {
            match session.apply_audio_adjustment() {
                Ok(()) => dialog.open = false,
                Err(error) => dialog.error = error.message().to_owned(),
            }
        }
        if ui.button("破棄").clicked() {
            session.discard_audio_adjustment();
        }
    });
}
