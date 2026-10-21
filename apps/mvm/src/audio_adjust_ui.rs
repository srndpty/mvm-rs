//! BGM の自動音量調整の dialog（C++ 版の AutoAudioDialog と同じ並び）。設定 → 解析（進捗と中止）→ 結果の確認・
//! 試聴 → 適用。結果を適用するまで Project は変わらない。適用は 1 回の undo。

use eframe::egui::{self, Color32, Pos2, Stroke, Vec2};
use mvm_app::{AudioAdjustmentSettings, Session};

use crate::dialog;
use crate::widgets::{self, ButtonKind};

const TEXT: Color32 = Color32::from_rgb(0xc8, 0xcb, 0xd1);
const HEADING: Color32 = Color32::from_rgb(0xe2, 0xe5, 0xeb);
const DETAIL: Color32 = Color32::from_rgb(0xb8, 0xc3, 0xd1);
const NOTE: Color32 = Color32::from_rgb(0xaa, 0xb2, 0xbd);

/// 数値の欄。見出し・範囲・小数の桁。
const FIELDS: [(&str, f64, f64, usize); 7] = [
    ("声の目標 (LUFS)", -40.0, -5.0, 1),
    ("BGM の目標 (LUFS)", -40.0, -5.0, 1),
    ("下げる量 (dB)", 0.0, 60.0, 1),
    ("検出しきい値 (dBFS)", -96.0, 0.0, 1),
    ("下げる時間 (ms)", 1.0, 10_000.0, 0),
    ("保持する時間 (ms)", 0.0, 10_000.0, 0),
    ("戻す時間 (ms)", 1.0, 10_000.0, 0),
];

#[derive(Default)]
pub struct AudioAdjustDialog {
    open: bool,
    settings: Option<AudioAdjustmentSettings>,
    /// 数値の欄の文字。読めない欄があれば解析させない。
    fields: Vec<String>,
    error: String,
}

impl AudioAdjustDialog {
    pub fn open(&mut self, session: &Session) {
        let settings = session.audio_adjustment_defaults();
        self.fields = field_values(&settings)
            .iter()
            .zip(FIELDS)
            .map(|(value, (_, _, _, decimals))| format!("{value:.decimals$}"))
            .collect();
        self.settings = Some(settings);
        self.error.clear();
        self.open = true;
    }

    pub fn is_open(&self) -> bool {
        self.open
    }
}

fn field_values(settings: &AudioAdjustmentSettings) -> [f64; 7] {
    [
        settings.voice_lufs,
        settings.bgm_lufs,
        settings.reduction_db,
        settings.threshold_db,
        f64::from(settings.attack_ms),
        f64::from(settings.hold_ms),
        f64::from(settings.release_ms),
    ]
}

fn set_field(settings: &mut AudioAdjustmentSettings, index: usize, value: f64) {
    match index {
        0 => settings.voice_lufs = value,
        1 => settings.bgm_lufs = value,
        2 => settings.reduction_db = value,
        3 => settings.threshold_db = value,
        4 => settings.attack_ms = value as i32,
        5 => settings.hold_ms = value as i32,
        _ => settings.release_ms = value as i32,
    }
}

/// 欄の文字を数として読む。範囲の外・読めない文字は None。
fn parse_field(text: &str, index: usize) -> Option<f64> {
    let (_, minimum, maximum, decimals) = FIELDS.get(index)?;
    let value = text.trim().parse::<f64>().ok()?;
    let integral = *decimals == 0 && value.fract() != 0.0;
    (value.is_finite() && (*minimum..=*maximum).contains(&value) && !integral).then_some(value)
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
    // 試聴が終わった（末尾まで再生した・Space で止めた）ら、元の音へ戻す。
    if !session.is_auditioning() {
        session.stop_audition();
    }
    let names: Vec<String> = session
        .project()
        .audio_tracks
        .iter()
        .map(|track| track.name.clone())
        .collect();
    let progress = session.audio_adjustment_progress();
    let adjusting = progress.is_some();
    let has_result = session.audio_adjustment_result().is_some();
    let stale = session.audio_adjustment_is_stale();
    let auditioning = session.is_auditioning();
    let invalid = dialog
        .fields
        .iter()
        .enumerate()
        .any(|(index, text)| parse_field(text, index).is_none());
    let mut changed = false;
    let mut analyze = false;
    let mut cancel = false;
    let mut apply = false;
    let mut audition = false;
    let screen = ui.ctx().content_rect();
    let width = (screen.width() - 24.0).clamp(300.0, 680.0);
    let height = (screen.height() - 24.0 - 48.0 - 58.0 - 34.0).clamp(200.0, 760.0 - 140.0);
    let Some(settings) = dialog.settings.as_mut() else {
        return;
    };
    let ((), close) = dialog::show(
        ui.ctx(),
        "audio_adjust",
        "BGM の自動音量調整",
        width,
        |ui| {
            egui::ScrollArea::vertical()
                .id_salt("audio_adjust_scroll")
                .max_height(height)
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 10.0;
                    ui.label(egui::RichText::new(
                        "声の音量を検出して BGM を下げます。声トラックの効果音や雑音にも反応します。試聴は現在の再生位置から、調整後の音で再生します。",
                    ).color(TEXT));
                    if stale {
                        ui.colored_label(
                            Color32::from_rgb(0xe3, 0xbc, 0x7c),
                            "音声の編集後です。自動調整を再生成してください。",
                        );
                    }
                    ui.label(egui::RichText::new("声トラック（複数選択可）").color(HEADING));
                    ui.add_enabled_ui(!adjusting, |ui| {
                        egui::ScrollArea::vertical()
                            .id_salt("voice_tracks")
                            .max_height(130.0)
                            .show(ui, |ui| {
                                ui.spacing_mut().item_spacing.y = 0.0;
                                for (index, name) in names.iter().enumerate() {
                                    let track = index as i32;
                                    let mut checked = settings.voice_tracks.contains(&track);
                                    if widgets::dialog_check(ui, &mut checked, name).clicked() {
                                        if checked {
                                            settings.voice_tracks.push(track);
                                            settings.voice_tracks.sort_unstable();
                                        } else {
                                            settings.voice_tracks.retain(|other| *other != track);
                                        }
                                        changed = true;
                                    }
                                }
                            });
                    });
                    ui.label(egui::RichText::new("BGM トラック").color(HEADING));
                    let options: Vec<&str> = names.iter().map(String::as_str).collect();
                    let current = usize::try_from(settings.bgm_track).unwrap_or(0);
                    let chosen = ui
                        .add_enabled_ui(!adjusting, |ui| {
                            widgets::choice_box(
                                ui,
                                egui::Id::new("adjust_bgm"),
                                &options,
                                current,
                                ui.available_width(),
                            )
                        })
                        .inner;
                    if let Some(index) = chosen {
                        settings.bgm_track = index as i32;
                        changed = true;
                    }
                    ui.add_enabled_ui(!adjusting, |ui| {
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 12.0;
                            changed |= widgets::dialog_check(
                                ui,
                                &mut settings.normalize,
                                "ラウドネス正規化",
                            )
                            .clicked();
                            changed |=
                                widgets::dialog_check(ui, &mut settings.duck, "自動ダッキング")
                                    .clicked();
                        });
                        for (index, (label, _, _, _)) in FIELDS.iter().enumerate() {
                            ui.spacing_mut().item_spacing.y = 3.0;
                            ui.label(egui::RichText::new(*label).color(TEXT));
                            let Some(text) = dialog.fields.get_mut(index) else {
                                continue;
                            };
                            let parsed = parse_field(text, index);
                            let mut field = egui::TextEdit::singleline(text)
                                .margin(Vec2::new(10.0, 8.0))
                                .desired_width(f32::INFINITY);
                            if parsed.is_none() {
                                field = field.text_color(Color32::from_rgb(0xff, 0xb5, 0xac));
                            }
                            if ui.add(field).changed() {
                                changed = true;
                                if let Some(value) = parse_field(text, index) {
                                    set_field(settings, index, value);
                                }
                            }
                            ui.spacing_mut().item_spacing.y = 10.0;
                        }
                    });
                    ui.label(egui::RichText::new(
                        "正規化はクリップごとに測定します。−1 dBTP を超える増幅は制限します。複数トラックを合成した最終ピークは別途確認してください。",
                    ).color(NOTE));
                    ui.horizontal(|ui| {
                        let label = if adjusting {
                            "解析を中止"
                        } else {
                            "解析・カーブ生成"
                        };
                        let button = ui.add_enabled_ui(adjusting || !invalid, |ui| {
                            widgets::dialog_button(ui, label, ButtonKind::Normal)
                        });
                        if button.inner.clicked() {
                            if adjusting {
                                session.cancel_audio_adjustment();
                            } else {
                                analyze = true;
                            }
                        }
                        if let Some(progress) = progress {
                            ui.label(
                                egui::RichText::new(format!("{} %", (progress * 100.0).round()))
                                    .color(TEXT),
                            );
                        }
                    });
                    if let Some(progress) = progress {
                        dialog::progress_bar(ui, Some(progress));
                        ui.ctx().request_repaint();
                    }
                    if !dialog.error.is_empty() {
                        ui.colored_label(Color32::from_rgb(0xff, 0xb5, 0xac), &dialog.error);
                    }
                    results(ui, session, settings.reduction_db);
                });
        },
        |ui| {
            let button = ui.add_enabled_ui(has_result && !stale, |ui| {
                widgets::dialog_button(ui, "適用", ButtonKind::Prominent)
            });
            apply = button.inner.clicked();
            let label = if auditioning {
                "試聴を停止"
            } else {
                "音声を試聴"
            };
            let button = ui.add_enabled_ui((has_result && !stale) || auditioning, |ui| {
                widgets::dialog_button(ui, label, ButtonKind::Normal)
            });
            audition = button.inner.clicked();
            cancel = widgets::dialog_button(ui, "キャンセル", ButtonKind::Normal).clicked();
        },
    );
    if changed {
        // 設定を変えたら、前の解析の結果は使わない（C++ 版と同じ）。
        session.stop_audition();
        session.cancel_audio_adjustment();
        session.discard_audio_adjustment();
        settings
            .voice_tracks
            .retain(|track| *track != settings.bgm_track);
    }
    if analyze {
        dialog.error.clear();
        if let Err(error) = session.start_audio_adjustment(settings.clone()) {
            dialog.error = error.message().to_owned();
        }
    }
    if audition {
        if auditioning {
            session.stop_audition();
        } else if let Err(error) = session.audition_audio_adjustment() {
            dialog.error = error.message().to_owned();
        }
    }
    if apply {
        session.stop_audition();
        match session.apply_audio_adjustment() {
            Ok(()) => dialog.open = false,
            Err(error) => dialog.error = error.message().to_owned(),
        }
    }
    if cancel || close {
        session.stop_audition();
        session.cancel_audio_adjustment();
        session.discard_audio_adjustment();
        dialog.open = false;
    }
}

/// 解析の結果。clip ごとの測定と補正、BGM の ducking のカーブ、声を検出した区間。
fn results(ui: &mut egui::Ui, session: &Session, reduction_db: f64) {
    let Some(outcome) = session.audio_adjustment_result() else {
        return;
    };
    let fps = session.project().timeline_fps;
    ui.label(
        egui::RichText::new("解析結果（測定不能な無音・短い音声は正規化を適用しません）")
            .color(TEXT),
    );
    egui::ScrollArea::vertical()
        .id_salt("adjust_results")
        .max_height(240.0)
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 6.0;
            for clip in &outcome.clips {
                egui::Frame::new()
                    .fill(Color32::from_rgb(0x20, 0x24, 0x2b))
                    .stroke(Stroke::new(1.0, Color32::from_rgb(0x41, 0x47, 0x50)))
                    .corner_radius(5)
                    .inner_margin(egui::Margin::same(8))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.spacing_mut().item_spacing.y = 4.0;
                        ui.label(egui::RichText::new(&clip.name).color(HEADING));
                        let detail = if clip.measurement.measurable {
                            format!(
                                "{:.1} LUFS / ピーク {:.1} dBTP / 補正 {:.1} dB{}{}",
                                clip.measurement.integrated_lufs,
                                clip.measurement.true_peak_db,
                                clip.correction_db,
                                if clip.peak_limited {
                                    "（ピーク制限で目標未達）"
                                } else {
                                    ""
                                },
                                if clip.gain_limited {
                                    "（補正量の上限に到達）"
                                } else {
                                    ""
                                }
                            )
                        } else {
                            "測定不能：正規化は未適用".to_owned()
                        };
                        ui.label(egui::RichText::new(detail).color(DETAIL));
                        let keys = outcome.ducking_keys(&clip.clip_id);
                        if keys.is_empty() {
                            return;
                        }
                        let (rect, _) = ui.allocate_exact_size(
                            Vec2::new(ui.available_width(), 55.0),
                            egui::Sense::hover(),
                        );
                        let last = keys.last().map_or(1, |key| key.frame).max(1) as f32;
                        let points: Vec<Pos2> = keys
                            .iter()
                            .map(|key| {
                                let x = rect.min.x + key.frame as f32 / last * rect.width();
                                let depth = (-key.value / reduction_db.max(1.0)).min(1.0) as f32;
                                Pos2::new(x, rect.min.y + 4.0 + depth * (rect.height() - 8.0))
                            })
                            .collect();
                        ui.painter().add(egui::Shape::line(
                            points,
                            Stroke::new(1.5, Color32::from_rgb(0x72, 0xb4, 0xf4)),
                        ));
                        let seconds = keys.last().map_or(0.0, |key| {
                            if fps.is_positive() {
                                key.frame as f64 * fps.den as f64 / fps.num as f64
                            } else {
                                0.0
                            }
                        });
                        ui.label(
                            egui::RichText::new(format!(
                                "{} キー / {seconds:.2} 秒（クリップ内）",
                                keys.len()
                            ))
                            .color(NOTE),
                        );
                    });
            }
        });
    let ranges = outcome.range_seconds();
    if ranges.is_empty() {
        return;
    }
    ui.label(egui::RichText::new("声の検出区間").color(TEXT));
    egui::ScrollArea::vertical()
        .id_salt("adjust_ranges")
        .max_height(100.0)
        .show(ui, |ui| {
            for (start, end) in ranges {
                ui.label(egui::RichText::new(format!("{start:.2}〜{end:.2} 秒")).color(DETAIL));
            }
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fields_accept_only_numbers_in_range() {
        assert_eq!(parse_field("-16", 0), Some(-16.0));
        assert_eq!(parse_field("-4", 0), None, "範囲の外を受け付けました");
        assert_eq!(parse_field("150", 4), Some(150.0));
        assert_eq!(parse_field("150.5", 4), None, "ms に小数を受け付けました");
        assert_eq!(parse_field("abc", 2), None);
        assert_eq!(parse_field("0", 5), Some(0.0));
        assert_eq!(parse_field("0", 6), None);
    }
}
