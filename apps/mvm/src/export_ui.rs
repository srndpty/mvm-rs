//! 書き出しの設定 dialog と、書き出し中の進捗・取消。

use std::path::PathBuf;

use eframe::egui;
use mvm_app::{ExportQuality, ExportRequest, Session};
use mvm_core::Rational;
use mvm_core::export_eta::format_export_remaining;
use mvm_project::timeline::timeline_end_frame;

/// dialog の入力。閉じても次に開くまで値を覚える。
#[derive(Default)]
pub struct ExportDialog {
    open: bool,
    path: String,
    quality: ExportQuality,
    burn_subtitles: bool,
    error: String,
    /// 直前の書き出しの結果。成功は通常の色、失敗は shell の notice に出す。
    pub finished: String,
}

impl ExportDialog {
    /// dialog を開く。書き出し先が空なら Project の隣を勧める。
    pub fn open(&mut self, session: &Session) {
        if session.is_exporting() {
            return;
        }
        if self.path.is_empty() {
            self.path = session.default_export_path().display().to_string();
            self.burn_subtitles = true;
        }
        self.error.clear();
        self.open = true;
    }

    pub fn is_open(&self) -> bool {
        self.open
    }
}

/// 書き出しの終わりを拾い、dialog と進捗を出す。失敗は `notice` へ書く。
pub fn show(
    ui: &mut egui::Ui,
    session: &mut Session,
    dialog: &mut ExportDialog,
    notice: &mut String,
) {
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
                *notice = error.to_string();
            }
        }
    }
    if session.is_exporting() {
        show_progress(ui, session);
    } else if dialog.open {
        show_settings(ui, session, dialog);
    }
}

fn show_settings(ui: &mut egui::Ui, session: &mut Session, dialog: &mut ExportDialog) {
    let mut start = false;
    let mut cancel = false;
    let project = session.project();
    let fps = project.timeline_fps;
    let has_subtitles = project
        .subtitles
        .as_ref()
        .is_some_and(|track| !track.cues.is_empty());
    let length = timeline_end_frame(project);
    let (width, height) = (project.output_width, project.output_height);
    egui::Modal::new(egui::Id::new("export")).show(ui.ctx(), |ui| {
        ui.heading("書き出し");
        match length {
            Ok(frames) => {
                ui.label(format!(
                    "{width}×{height}・{} fps・{}",
                    fps_label(fps),
                    length_label(frames, fps)
                ));
            }
            Err(error) => {
                ui.colored_label(egui::Color32::from_rgb(230, 120, 120), error.to_string());
            }
        }
        ui.add_space(6.0);
        ui.label("書き出し先（MP4）");
        ui.horizontal(|ui| {
            ui.add(egui::TextEdit::singleline(&mut dialog.path).desired_width(360.0));
            if ui.button("参照").clicked()
                && let Some(path) = pick_output(&dialog.path)
            {
                dialog.path = path.display().to_string();
            }
        });
        ui.add_space(6.0);
        ui.label("画質");
        for quality in ExportQuality::ALL {
            ui.radio_value(
                &mut dialog.quality,
                quality,
                format!("{}（{}）", quality.label(), quality.detail()),
            );
        }
        ui.add_space(6.0);
        ui.add_enabled(
            has_subtitles,
            egui::Checkbox::new(&mut dialog.burn_subtitles, "字幕を焼き込む"),
        );
        if !dialog.error.is_empty() {
            ui.colored_label(egui::Color32::from_rgb(230, 120, 120), &dialog.error);
        }
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            if ui.button("書き出す").clicked() {
                start = true;
            }
            if ui.button("キャンセル").clicked() {
                cancel = true;
            }
        });
    });
    if cancel {
        dialog.open = false;
        return;
    }
    if start {
        let request = ExportRequest {
            output_path: ensure_mp4(PathBuf::from(dialog.path.trim())),
            quality: dialog.quality,
            burn_subtitles: dialog.burn_subtitles,
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
}

fn show_progress(ui: &mut egui::Ui, session: &mut Session) {
    let Some(progress) = session.export_progress() else {
        return;
    };
    let fps = session.project().timeline_fps;
    let mut cancel = false;
    egui::Modal::new(egui::Id::new("export-progress")).show(ui.ctx(), |ui| {
        ui.heading("書き出し中");
        let fraction = if progress.total > 0 {
            progress.completed as f32 / progress.total as f32
        } else {
            0.0
        };
        ui.add(
            egui::ProgressBar::new(fraction)
                .desired_width(360.0)
                .show_percentage(),
        );
        ui.label(format!(
            "{} / {}",
            length_label(progress.completed, fps),
            length_label(progress.total, fps)
        ));
        ui.label(match progress.remaining_seconds {
            Some(seconds) => format_export_remaining(seconds),
            None => "残り時間を計測しています".to_owned(),
        });
        let label = if progress.cancelling {
            "取り消しています"
        } else {
            "取り消す"
        };
        if ui
            .add_enabled(!progress.cancelling, egui::Button::new(label))
            .clicked()
        {
            cancel = true;
        }
    });
    if cancel {
        session.cancel_export();
    }
}

fn length_label(frames: i64, fps: Rational) -> String {
    if fps.is_positive() {
        let seconds = frames as f64 * fps.den as f64 / fps.num as f64;
        format!("{frames} frame（{seconds:.2} 秒）")
    } else {
        format!("{frames} frame")
    }
}

fn fps_label(fps: Rational) -> String {
    if fps.den == 1 {
        fps.num.to_string()
    } else if fps.is_positive() {
        format!("{:.3}", fps.num as f64 / fps.den as f64)
    } else {
        "?".to_owned()
    }
}

fn pick_output(current: &str) -> Option<PathBuf> {
    let mut dialog = rfd::FileDialog::new()
        .set_title("書き出し先")
        .add_filter("MP4", &["mp4"]);
    let current = PathBuf::from(current);
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
    fn labels_show_frames_and_seconds() {
        assert_eq!(
            length_label(90, Rational::new(60, 1)),
            "90 frame（1.50 秒）"
        );
        assert_eq!(fps_label(Rational::new(30000, 1001)), "29.970");
        assert_eq!(fps_label(Rational::new(60, 1)), "60");
        assert_eq!(ensure_mp4(PathBuf::from("a")), PathBuf::from("a.mp4"));
        assert_eq!(ensure_mp4(PathBuf::from("a.mov")), PathBuf::from("a.mov"));
        assert_eq!(ensure_mp4(PathBuf::new()), PathBuf::new());
    }
}
