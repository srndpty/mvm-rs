//! 書き出し。書き出し設定のダイアログで書き出し先と品質を決めて始める（OS のダイアログは「参照…」を押したとき
//! だけ開く）。書き出し中は進み具合と取消、失敗したら理由を出す。

use std::path::{Path, PathBuf};

use eframe::egui;
use mvm_app::{
    ExportQuality, ExportRequest, ExportSettingsSummary, ReplaceApproval, Session,
    export_settings_summary, replace_approval,
};
use mvm_core::export_eta::format_export_remaining;

use crate::dialog;
use crate::widgets::{self, ButtonKind};

/// dialog の入力。閉じても次に開くまで値を覚える。
pub struct ExportDialog {
    open: bool,
    /// 書き出し先の欄の文字。
    path: String,
    quality: ExportQuality,
    burn_subtitles: bool,
    summary: Option<ExportSettingsSummary>,
    error: String,
    /// 書き出し先に既にある file を置き換えるかの確認中。確認を出した時点の実体を持つ。
    overwrite: Option<ReplaceApproval>,
    /// 失敗した書き出しの理由。閉じるまでダイアログで見せる。
    failure: Option<String>,
    /// 直前の書き出しの結果。
    pub finished: String,
}

impl Default for ExportDialog {
    fn default() -> Self {
        Self {
            open: false,
            path: String::new(),
            quality: ExportQuality::default(),
            burn_subtitles: true,
            summary: None,
            error: String::new(),
            overwrite: None,
            failure: None,
            finished: String::new(),
        }
    }
}

impl ExportDialog {
    /// 書き出し設定のダイアログを開く。書き出し先が空なら Project の隣を勧める。
    pub fn open(&mut self, session: &Session) {
        if session.is_exporting() {
            return;
        }
        if self.path.is_empty() {
            self.path = session.default_export_path().display().to_string();
        }
        self.error.clear();
        self.overwrite = None;
        // 品質は書き出しごとに選ぶ（開くたびに既定へ戻す）。
        self.quality = ExportQuality::default();
        self.summary = Some(export_settings_summary(session.project()));
        self.open = true;
    }

    pub fn is_open(&self) -> bool {
        self.open
    }
}

/// 書き出しの終わりを拾い、dialog と進捗を出す。
pub fn show(ui: &mut egui::Ui, session: &mut Session, dialog: &mut ExportDialog) {
    if let Some(result) = session.poll_export() {
        match result {
            Ok(summary) => {
                dialog.finished = format!(
                    "書き出しました: {}（{} frame・{:.2} 秒）",
                    summary.output_path.display(),
                    summary.frames,
                    summary.duration_seconds
                );
            }
            Err(error) => {
                dialog.finished.clear();
                dialog.failure = Some(error.to_string());
            }
        }
    }
    if session.is_exporting() {
        show_progress(ui, session);
    } else if dialog.failure.is_some() {
        show_failure(ui, dialog);
    } else if dialog.open {
        show_settings(ui, session, dialog);
    }
}

fn show_settings(ui: &mut egui::Ui, session: &mut Session, dialog: &mut ExportDialog) {
    let mut start = false;
    let mut cancel = false;
    let mut replace = None;
    let has_subtitles = session
        .project()
        .subtitles
        .as_ref()
        .is_some_and(|track| !track.cues.is_empty());
    let summary = dialog.summary.clone();
    let confirming = dialog.overwrite.is_some();
    let width = (ui.ctx().content_rect().width() - 40.0).clamp(300.0, 640.0);
    let ((), close) = dialog::show(
        ui.ctx(),
        "export",
        "書き出し設定",
        width,
        |ui| {
            if let Some(summary) = &summary {
                ui.label(&summary.input);
                ui.label(&summary.output);
                if !summary.warning.is_empty() {
                    egui::Frame::new()
                        .fill(egui::Color32::from_rgb(0x3a, 0x30, 0x20))
                        .stroke(egui::Stroke::new(
                            1.0,
                            egui::Color32::from_rgb(0xb5, 0x8a, 0x43),
                        ))
                        .corner_radius(4)
                        .inner_margin(egui::Margin::same(10))
                        .show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            ui.label(
                                egui::RichText::new(&summary.warning)
                                    .color(egui::Color32::from_rgb(0xf0, 0xc3, 0x6a)),
                            );
                        });
                }
            }
            ui.label(egui::RichText::new("書き出し先（MP4）").strong());
            ui.horizontal(|ui| {
                let field_width = (ui.available_width() - 96.0).max(120.0);
                let field = ui.add_sized(
                    egui::vec2(field_width, 34.0),
                    egui::TextEdit::singleline(&mut dialog.path).margin(egui::vec2(10.0, 8.0)),
                );
                if field.changed() {
                    dialog.overwrite = None;
                }
                if widgets::dialog_button(ui, "参照…", ButtonKind::Normal).clicked()
                    && let Some(path) = pick_output(Path::new(dialog.path.trim()))
                {
                    dialog.path = path.display().to_string();
                    dialog.overwrite = None;
                }
            });
            ui.label(egui::RichText::new("エンコード品質").strong());
            ui.add_enabled_ui(has_subtitles, |ui| {
                widgets::dialog_check(ui, &mut dialog.burn_subtitles, "字幕を焼き込む");
            });
            let options: Vec<String> = ExportQuality::ALL
                .iter()
                .map(|quality| format!("{}（{}）", quality.label(), quality.detail()))
                .collect();
            let labels: Vec<&str> = options.iter().map(String::as_str).collect();
            let current = ExportQuality::ALL
                .iter()
                .position(|quality| *quality == dialog.quality)
                .unwrap_or(0);
            if let Some(index) = widgets::choice_box(
                ui,
                egui::Id::new("export_quality"),
                &labels,
                current,
                ui.available_width(),
            ) && let Some(quality) = ExportQuality::ALL.get(index)
            {
                dialog.quality = *quality;
            }
            ui.label(
                egui::RichText::new(
                    "品質は圧縮率だけを変更します。出力の解像度とfpsはプロジェクト設定のままです。",
                )
                .color(egui::Color32::from_rgb(0xae, 0xb4, 0xbf)),
            );
            if !dialog.error.is_empty() {
                ui.colored_label(egui::Color32::from_rgb(0xef, 0x8b, 0x8b), &dialog.error);
            }
            if let Some(approval) = &dialog.overwrite {
                ui.colored_label(
                    egui::Color32::from_rgb(0xf0, 0xc3, 0x6a),
                    format!(
                        "{} は既にあります。置き換えて書き出しますか？",
                        approval.path().display()
                    ),
                );
            }
        },
        |ui| {
            if confirming {
                if widgets::dialog_button(ui, "置き換えて書き出す", ButtonKind::Destructive)
                    .clicked()
                {
                    replace = Some(true);
                }
                if widgets::dialog_button(ui, "やめる", ButtonKind::Normal).clicked() {
                    replace = Some(false);
                }
            } else {
                if widgets::dialog_button(ui, "書き出す", ButtonKind::Prominent).clicked() {
                    start = true;
                }
                if widgets::dialog_button(ui, "キャンセル", ButtonKind::Normal).clicked() {
                    cancel = true;
                }
            }
        },
    );
    if cancel || close {
        dialog.open = false;
        dialog.overwrite = None;
        return;
    }
    let output_path = ensure_mp4(PathBuf::from(dialog.path.trim()));
    let approval = match (start, replace) {
        // 空の書き出し先は計画が理由を付けて拒む。
        (true, _) if output_path.as_os_str().is_empty() => None,
        // 既にある file には書かずに、置き換えの確認を出す。確認した時点の実体だけを置き換える。
        (true, _) => match replace_approval(&output_path) {
            Ok(None) => None,
            Ok(Some(approval)) => {
                dialog.overwrite = Some(approval);
                return;
            }
            Err(error) => {
                dialog.error = error.message().to_owned();
                return;
            }
        },
        (_, Some(true)) => dialog.overwrite.take(),
        (_, Some(false)) => {
            dialog.overwrite = None;
            return;
        }
        (false, None) => return,
    };
    let request = ExportRequest {
        output_path,
        quality: dialog.quality,
        burn_subtitles: dialog.burn_subtitles && has_subtitles,
        replace: approval,
    };
    match session.start_export(&request) {
        Ok(()) => {
            dialog.path = request.output_path.display().to_string();
            dialog.error.clear();
            dialog.finished.clear();
            dialog.open = false;
        }
        Err(error) => dialog.error = error.message().to_owned(),
    }
}

fn show_progress(ui: &mut egui::Ui, session: &mut Session) {
    let Some(progress) = session.export_progress() else {
        return;
    };
    let mut cancel = false;
    let text = if progress.cancelling {
        "キャンセルしています…".to_owned()
    } else if progress.total <= 0 || progress.completed == 0 && progress.remaining_seconds.is_none()
    {
        "準備しています…".to_owned()
    } else {
        format!(
            "{} / {} frame ・ {}",
            progress.completed,
            progress.total,
            match progress.remaining_seconds {
                Some(seconds) => format_export_remaining(seconds),
                None => "残り時間を計算中…".to_owned(),
            }
        )
    };
    let fraction = (progress.total > 0 && progress.completed > 0)
        .then(|| progress.completed as f32 / progress.total as f32);
    let width = (ui.ctx().content_rect().width() - 40.0).clamp(300.0, 460.0);
    dialog::show(
        ui.ctx(),
        "export-progress",
        "動画を書き出しています",
        width,
        |ui| {
            ui.vertical_centered(|ui| ui.label(&text));
            dialog::progress_bar(ui, fraction);
        },
        |ui| {
            let label = if progress.cancelling {
                "キャンセル中…"
            } else {
                "キャンセル"
            };
            let button = ui.add_enabled_ui(!progress.cancelling, |ui| {
                widgets::dialog_button(ui, label, ButtonKind::Normal)
            });
            if button.inner.clicked() {
                cancel = true;
            }
        },
    );
    if cancel {
        session.cancel_export();
    }
}

fn show_failure(ui: &mut egui::Ui, dialog: &mut ExportDialog) {
    let message = dialog.failure.clone().unwrap_or_default();
    let width = (ui.ctx().content_rect().width() - 40.0).clamp(300.0, 560.0);
    let mut ok = false;
    let ((), close) = dialog::show(
        ui.ctx(),
        "export-failure",
        "書き出しに失敗しました",
        width,
        |ui| {
            ui.label(&message);
        },
        |ui| {
            ok = widgets::dialog_button(ui, "OK", ButtonKind::Prominent).clicked();
        },
    );
    if ok || close {
        dialog.failure = None;
    }
}

fn pick_output(current: &Path) -> Option<PathBuf> {
    let mut dialog = rfd::FileDialog::new()
        .set_title("書き出し先を指定")
        .add_filter("MP4", &["mp4"]);
    if let Some(parent) = current.parent().filter(|parent| parent.is_dir()) {
        dialog = dialog.set_directory(parent);
    }
    if let Some(name) = current.file_name() {
        dialog = dialog.set_file_name(name.to_string_lossy());
    }
    dialog.save_file().map(ensure_mp4)
}

/// 拡張子が無ければ `.mp4` を付ける。別の拡張子は書き出しの計画が拒む。
fn ensure_mp4(path: PathBuf) -> PathBuf {
    if path.as_os_str().is_empty() || path.extension().is_some() {
        path
    } else {
        path.with_extension("mp4")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_output_gets_the_mp4_extension_only_when_it_has_none() {
        assert_eq!(ensure_mp4(PathBuf::from("a")), PathBuf::from("a.mp4"));
        assert_eq!(ensure_mp4(PathBuf::from("a.mov")), PathBuf::from("a.mov"));
        assert_eq!(ensure_mp4(PathBuf::new()), PathBuf::new());
    }
}
