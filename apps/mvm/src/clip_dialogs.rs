//! 速度・デュレーションの dialog と、Project の映像設定の dialog。値は「適用」で 1 回だけ commit する。

use eframe::egui;
use mvm_app::{Session, VideoSettings};
use mvm_core::Rational;
use mvm_core::output_timebase::CONFIGURABLE_OUTPUT_FRAME_RATES;
use mvm_core::timecode::format_timecode;
use mvm_project::timeline::{ClipSpeedDurationEdit, SpeedDurationInput, timeline_clip_duration};
use mvm_project::{MAX_CLIP_SPEED_PERCENT, MIN_CLIP_SPEED_PERCENT};

const ERROR: egui::Color32 = egui::Color32::from_rgb(230, 120, 120);

#[derive(Default)]
pub struct SpeedDialog {
    clip_id: Option<String>,
    /// 開いたときの速度。尺から決めた速度は整数 % で表せないことがあるので、速度・尺のどちらも編集していない間は
    /// 丸めた % ではなくこれを使う（ピッチ・リップルだけを変えて適用しても速度と尺を変えない）。
    original_speed: Option<Rational>,
    speed_percent: i64,
    duration_frames: i64,
    input: Option<SpeedDurationInput>,
    preserve_pitch: bool,
    ripple: bool,
    /// 延びた先の clip と重なる。「上書きする」を出す。
    overlaps: bool,
    error: String,
}

impl SpeedDialog {
    /// 選んだ clip の今の値で開く。速度を持てない clip（文字・静止画・フレーム保持）は断る。
    pub fn open(&mut self, session: &mut Session, clip_id: &str) {
        let Some(clip) = session.project().clip(clip_id) else {
            session.set_alert("速度を変える clip がありません");
            return;
        };
        if clip.has_synthetic_source_domain() {
            session.set_alert("この clip は速度を変えられません");
            return;
        }
        let duration = match timeline_clip_duration(session.project(), clip) {
            Ok(duration) => duration,
            Err(error) => {
                session.set_alert(error.to_string());
                return;
            }
        };
        *self = Self {
            clip_id: Some(clip_id.to_owned()),
            original_speed: Some(clip.speed),
            speed_percent: speed_percent(clip.speed),
            duration_frames: duration,
            input: None,
            preserve_pitch: clip.preserve_pitch,
            ripple: false,
            overlaps: false,
            error: String::new(),
        };
    }

    pub fn is_open(&self) -> bool {
        self.clip_id.is_some()
    }

    fn edit(&self, overwrite: bool) -> Option<ClipSpeedDurationEdit> {
        Some(ClipSpeedDurationEdit {
            input: self.input.unwrap_or(SpeedDurationInput::Speed),
            speed: match (self.input, self.original_speed) {
                (None, Some(original)) => original,
                _ => Rational::new(self.speed_percent, 100).reduced()?,
            },
            duration_frames: self.duration_frames,
            preserve_pitch: self.preserve_pitch,
            ripple: self.ripple,
            overwrite,
        })
    }

    /// 入力した側から、もう一方の値を project の計算で埋める。
    fn sync(&mut self, session: &Session) {
        let (Some(id), Some(edit)) = (self.clip_id.as_deref(), self.edit(false)) else {
            return;
        };
        match session.preview_speed_duration(id, &edit) {
            Ok(preview) => {
                self.duration_frames = preview.duration_frames;
                self.speed_percent = speed_percent(preview.speed);
                self.overlaps = false;
                self.error.clear();
            }
            Err(error) => {
                self.overlaps = error.overlaps_following;
                self.error = error.error.to_string();
            }
        }
    }
}

fn speed_percent(speed: Rational) -> i64 {
    if speed.den <= 0 {
        return 100;
    }
    let scaled = i128::from(speed.num) * 100;
    let den = i128::from(speed.den);
    i64::try_from((scaled + den / 2) / den).unwrap_or(100)
}

pub fn show_speed(ui: &mut egui::Ui, session: &mut Session, dialog: &mut SpeedDialog) {
    let Some(id) = dialog.clip_id.clone() else {
        return;
    };
    let fps = session.project().timeline_fps;
    let mut apply = None;
    let mut close = false;
    egui::Modal::new(egui::Id::new("speed_duration")).show(ui.ctx(), |ui| {
        ui.heading("速度・デュレーション");
        let speed = ui.add(
            egui::DragValue::new(&mut dialog.speed_percent)
                .range(MIN_CLIP_SPEED_PERCENT..=MAX_CLIP_SPEED_PERCENT)
                .suffix(" % 速度"),
        );
        if speed.changed() {
            dialog.input = Some(SpeedDurationInput::Speed);
            dialog.sync(session);
        }
        ui.horizontal(|ui| {
            let duration = ui.add(
                egui::DragValue::new(&mut dialog.duration_frames)
                    .range(1..=i64::MAX)
                    .suffix(" frame"),
            );
            if duration.changed() {
                dialog.input = Some(SpeedDurationInput::Duration);
                dialog.sync(session);
            }
            ui.label(seconds_label(dialog.duration_frames, fps));
        });
        ui.label("デュレーション");
        if ui
            .checkbox(&mut dialog.preserve_pitch, "オーディオのピッチを維持")
            .changed()
        {
            dialog.sync(session);
        }
        if ui
            .checkbox(&mut dialog.ripple, "リップル編集 (後続クリップをシフト)")
            .changed()
        {
            dialog.sync(session);
        }
        if !dialog.error.is_empty() {
            ui.colored_label(ERROR, &dialog.error);
        }
        ui.horizontal(|ui| {
            if ui.button("キャンセル").clicked() {
                close = true;
            }
            if dialog.overlaps {
                if ui.button("上書きする").clicked() {
                    apply = Some(true);
                }
            } else if ui.button("適用").clicked() {
                apply = Some(false);
            }
        });
    });
    if close {
        dialog.clip_id = None;
        return;
    }
    let Some(overwrite) = apply else {
        return;
    };
    let Some(edit) = dialog.edit(overwrite) else {
        dialog.error = "速度を表せません".to_owned();
        return;
    };
    match session.set_speed_duration(&id, &edit) {
        Ok(()) => dialog.clip_id = None,
        Err(error) => {
            dialog.error = error.message().to_owned();
            dialog.sync(session);
        }
    }
}

fn seconds_label(frames: i64, fps: Rational) -> String {
    format_timecode(frames, fps.num, fps.den).unwrap_or_else(|| "--:--:--:--".to_owned())
}

/// Project の映像設定。素材に合わせるときは、その値を入れた状態で開く。
pub struct SettingsDialog {
    open: bool,
    width: i32,
    height: i32,
    fps: Rational,
    /// 素材に合わせて開いたときの説明。
    source: String,
    error: String,
}

impl Default for SettingsDialog {
    fn default() -> Self {
        Self {
            open: false,
            width: 0,
            height: 0,
            fps: Rational::new(60, 1),
            source: String::new(),
            error: String::new(),
        }
    }
}

impl SettingsDialog {
    pub fn open(&mut self, session: &Session) {
        let settings = session.video_settings();
        self.fill(settings, String::new());
    }

    /// clip の素材に合わせた値で開く。既に同じなら知らせるだけにする。
    pub fn open_for_clip(&mut self, session: &mut Session, clip_id: &str) {
        let settings = match session.video_settings_for_clip(clip_id) {
            Ok(settings) => settings,
            Err(error) => {
                session.set_alert(error.message());
                return;
            }
        };
        if settings == session.video_settings() {
            session.set_alert("プロジェクト設定はすでにこの素材と一致しています");
            return;
        }
        let name = session
            .project()
            .clip(clip_id)
            .map(|clip| clip.name.clone())
            .unwrap_or_default();
        self.fill(settings, format!("素材: {name}"));
    }

    fn fill(&mut self, settings: VideoSettings, source: String) {
        *self = Self {
            open: true,
            width: settings.width,
            height: settings.height,
            fps: settings.fps,
            source,
            error: String::new(),
        };
    }

    pub fn is_open(&self) -> bool {
        self.open
    }
}

pub fn show_settings(ui: &mut egui::Ui, session: &mut Session, dialog: &mut SettingsDialog) {
    if !dialog.open {
        return;
    }
    let mut apply = false;
    let mut close = false;
    egui::Modal::new(egui::Id::new("project_settings")).show(ui.ctx(), |ui| {
        ui.heading("プロジェクト設定");
        if !dialog.source.is_empty() {
            ui.label(&dialog.source);
        }
        egui::Grid::new("project_settings_grid").show(ui, |ui| {
            ui.label("幅");
            ui.add(egui::DragValue::new(&mut dialog.width).range(2..=16384));
            ui.end_row();
            ui.label("高さ");
            ui.add(egui::DragValue::new(&mut dialog.height).range(2..=16384));
            ui.end_row();
            ui.label("フレームレート");
            egui::ComboBox::from_id_salt("project_fps")
                .selected_text(fps_label(dialog.fps))
                .show_ui(ui, |ui| {
                    for fps in CONFIGURABLE_OUTPUT_FRAME_RATES {
                        ui.selectable_value(&mut dialog.fps, *fps, fps_label(*fps));
                    }
                });
            ui.end_row();
        });
        ui.label(
            "fps を変更すると、既存クリップの開始位置を秒位置が保たれるよう換算します。素材の in/out は変更しません。",
        );
        ui.label("幅と高さは MP4（yuv420p）で扱える 2〜16384 の偶数を指定してください。");
        if !dialog.error.is_empty() {
            ui.colored_label(ERROR, &dialog.error);
        }
        ui.horizontal(|ui| {
            if ui.button("キャンセル").clicked() {
                close = true;
            }
            if ui.button("適用").clicked() {
                apply = true;
            }
        });
    });
    if close {
        dialog.open = false;
    } else if apply {
        let settings = VideoSettings {
            width: dialog.width,
            height: dialog.height,
            fps: dialog.fps,
        };
        match session.set_video_settings(settings) {
            Ok(()) => dialog.open = false,
            Err(error) => dialog.error = error.message().to_owned(),
        }
    }
}

pub fn fps_label(fps: Rational) -> String {
    if fps.den == 1 {
        format!("{} fps", fps.num)
    } else if fps.den > 0 {
        format!(
            "{:.3} fps ({}/{})",
            fps.num as f64 / fps.den as f64,
            fps.num,
            fps.den
        )
    } else {
        "-- fps".to_owned()
    }
}

#[cfg(test)]
mod tests {
    use mvm_project::{Project, TimelineClip, TimelineClipKind, TrackRef};

    use super::*;

    /// 尺から決めた 2/3 倍（66.666…%）の clip。
    fn session() -> Session {
        Session::from_project(Project {
            timeline_clips: vec![TimelineClip {
                kind: TimelineClipKind::Video,
                media_path: "v.mp4".into(),
                name: "v".to_owned(),
                id: "v".to_owned(),
                source_fps: Rational::new(60, 1),
                source_frame_count: 200,
                source_out_frame: 200,
                speed: Rational::new(2, 3),
                track: TrackRef::video(0),
                ..TimelineClip::default()
            }],
            ..Project::default()
        })
    }

    #[test]
    fn an_unedited_fractional_speed_is_kept_as_is() {
        let mut session = session();
        let mut dialog = SpeedDialog::default();
        dialog.open(&mut session, "v");
        assert_eq!(dialog.speed_percent, 67);
        // ピッチだけを変えても、速度は丸めた 67% ではなく元の 2/3 のまま。
        dialog.preserve_pitch = true;
        let edit = dialog.edit(false).expect("編集");
        assert_eq!(edit.speed, Rational::new(2, 3));
    }

    #[test]
    fn an_edited_speed_uses_the_entered_percentage() {
        let mut session = session();
        let mut dialog = SpeedDialog::default();
        dialog.open(&mut session, "v");
        dialog.speed_percent = 50;
        dialog.input = Some(SpeedDurationInput::Speed);
        let edit = dialog.edit(false).expect("編集");
        assert_eq!(edit.speed, Rational::new(1, 2));
    }
}
