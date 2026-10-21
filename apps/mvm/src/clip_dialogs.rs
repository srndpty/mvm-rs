//! 速度・デュレーションの dialog と、Project の映像設定の dialog（C++ 版と同じ項目・並び）。値は「適用」で
//! 1 回だけ commit する。

use eframe::egui;
use mvm_app::{Session, VideoSettings};
use mvm_core::Rational;
use mvm_core::output_timebase::CONFIGURABLE_OUTPUT_FRAME_RATES;
use mvm_core::timecode::{format_timecode, parse_timecode};
use mvm_project::timeline::{ClipSpeedDurationEdit, SpeedDurationInput, timeline_clip_duration};
use mvm_project::{MAX_CLIP_SPEED_PERCENT, MIN_CLIP_SPEED_PERCENT};

use crate::dialog;
use crate::widgets::{self, ButtonKind, NumberEdit, NumberField};

const WARNING: egui::Color32 = egui::Color32::from_rgb(0xf0, 0xb8, 0x70);
const NOTE: egui::Color32 = egui::Color32::from_rgb(0xae, 0xb4, 0xbf);
const CAUTION: egui::Color32 = egui::Color32::from_rgb(0xf0, 0xc3, 0x6a);
const FAILURE: egui::Color32 = egui::Color32::from_rgb(0xef, 0x8b, 0x8b);

/// 速度の 1/100 %（6667 なら 66.67%）。C++ 版と同じく小数 2 桁まで入力できる。
type SpeedCenti = i64;

#[derive(Default)]
pub struct SpeedDialog {
    clip_id: Option<String>,
    /// 開いたときの速度。尺から決めた速度は小数 2 桁の % で表せないことがあるので、速度・尺のどちらも編集して
    /// いない間は丸めた % ではなくこれを使う（ピッチ・リップルだけを変えて適用しても速度と尺を変えない）。
    original_speed: Option<Rational>,
    speed_centi: SpeedCenti,
    duration_frames: i64,
    /// デュレーションの欄の文字（timecode）。
    duration_text: String,
    input: Option<SpeedDurationInput>,
    preserve_pitch: bool,
    ripple: bool,
    /// 等速以外を持てない clip（静止画など）。速度は変えられず、尺だけを変える。
    still: bool,
    /// 延びた先の clip と重なる。上書きの確認を出している。
    confirm_overwrite: bool,
    error: String,
}

impl SpeedDialog {
    /// 選んだ clip の今の値で開く。速度を持てない clip（文字・静止画・フレーム保持）は尺だけを変える
    /// （C++ 版と同じ）。
    pub fn open(&mut self, session: &mut Session, clip_id: &str) {
        let Some(clip) = session.project().clip(clip_id) else {
            session.set_alert("速度を変える clip がありません");
            return;
        };
        let still = clip.has_synthetic_source_domain();
        let duration = match timeline_clip_duration(session.project(), clip) {
            Ok(duration) => duration,
            Err(error) => {
                session.set_alert(error.to_string());
                return;
            }
        };
        let fps = session.project().timeline_fps;
        *self = Self {
            clip_id: Some(clip_id.to_owned()),
            original_speed: Some(clip.speed),
            speed_centi: speed_centi(clip.speed),
            duration_frames: duration,
            duration_text: timecode(duration, fps),
            input: still.then_some(SpeedDurationInput::Duration),
            preserve_pitch: clip.preserve_pitch && !still,
            ripple: false,
            still,
            confirm_overwrite: false,
            error: String::new(),
        };
    }

    pub fn is_open(&self) -> bool {
        self.clip_id.is_some()
    }

    /// 欄の値から編集を作る。尺を入力した側は欄の文字をその場で読み直し、読めなければ拒否する（最後に
    /// 読めた古い尺では適用しない）。
    fn edit(&self, fps: Rational, overwrite: bool) -> Result<ClipSpeedDurationEdit, String> {
        let input = self.input.unwrap_or(SpeedDurationInput::Speed);
        let duration_frames = match input {
            SpeedDurationInput::Duration => parse_duration(&self.duration_text, fps)?,
            SpeedDurationInput::Speed => self.duration_frames,
        };
        let speed = match (self.input, self.original_speed) {
            (None, Some(original)) => original,
            _ => Rational::new(self.speed_centi, 10_000)
                .reduced()
                .ok_or_else(|| "速度を表せません".to_owned())?,
        };
        Ok(ClipSpeedDurationEdit {
            input,
            speed,
            duration_frames,
            preserve_pitch: self.preserve_pitch && !self.still,
            ripple: self.ripple,
            overwrite,
        })
    }

    /// 入力した側から、もう一方の値を project の計算で埋める。
    fn sync(&mut self, session: &Session) {
        let Some(id) = self.clip_id.as_deref() else {
            return;
        };
        let edit = match self.edit(session.project().timeline_fps, false) {
            Ok(edit) => edit,
            Err(error) => {
                self.error = error;
                return;
            }
        };
        match session.preview_speed_duration(id, &edit) {
            Ok(preview) => {
                if self.input != Some(SpeedDurationInput::Duration) {
                    self.duration_frames = preview.duration_frames;
                    self.duration_text =
                        timecode(preview.duration_frames, session.project().timeline_fps);
                }
                if self.input != Some(SpeedDurationInput::Speed) {
                    self.speed_centi = speed_centi(preview.speed);
                }
                self.error.clear();
            }
            Err(error) => {
                // 重なりは「適用」のときに上書きを確かめる。ここでは理由だけを出す。
                if !error.overlaps_following {
                    self.error = error.error.to_string();
                }
            }
        }
    }
}

fn speed_centi(speed: Rational) -> SpeedCenti {
    if speed.den <= 0 {
        return 10_000;
    }
    let scaled = i128::from(speed.num) * 10_000;
    let den = i128::from(speed.den);
    i64::try_from((scaled + den / 2) / den).unwrap_or(10_000)
}

const DURATION_FORMAT: &str = "デュレーションを 00:00:05:00 の形で入力してください";

/// デュレーションの欄を frame 数にする。1 frame 未満と読めない文字は拒否する。
fn parse_duration(text: &str, fps: Rational) -> Result<i64, String> {
    match parse_timecode(text.trim(), fps.num, fps.den) {
        Some(frames) if frames > 0 => Ok(frames),
        _ => Err(DURATION_FORMAT.to_owned()),
    }
}

fn timecode(frames: i64, fps: Rational) -> String {
    format_timecode(frames, fps.num, fps.den).unwrap_or_else(|| "--:--:--:--".to_owned())
}

pub fn show_speed(ui: &mut egui::Ui, session: &mut Session, dialog: &mut SpeedDialog) {
    let Some(id) = dialog.clip_id.clone() else {
        return;
    };
    let fps = session.project().timeline_fps;
    let mut apply = false;
    let mut cancel = false;
    let width = (ui.ctx().content_rect().width() - 40.0).clamp(300.0, 440.0);
    let ((), close) = dialog::show(
        ui.ctx(),
        "speed_duration",
        "速度・デュレーション",
        width,
        |ui| {
            let speed = ui
                .add_enabled_ui(!dialog.still, |ui| {
                    NumberField {
                        label: "速度",
                        value: dialog.speed_centi as f64 / 100.0,
                        minimum: MIN_CLIP_SPEED_PERCENT as f64,
                        maximum: MAX_CLIP_SPEED_PERCENT as f64,
                        decimals: 2,
                        suffix: "%",
                        click_to_edit: true,
                        ..NumberField::default()
                    }
                    .show(ui, egui::Id::new("speed_field"))
                })
                .inner;
            if let Some(NumberEdit::Preview(value) | NumberEdit::Commit(value)) = speed {
                dialog.speed_centi = (value * 100.0).round() as i64;
                dialog.input = Some(SpeedDurationInput::Speed);
                dialog.sync(session);
            }
            ui.label("デュレーション");
            let duration = ui.add_sized(
                egui::vec2(ui.available_width(), 34.0),
                egui::TextEdit::singleline(&mut dialog.duration_text)
                    .hint_text("00:00:05:00")
                    .margin(egui::vec2(10.0, 8.0)),
            );
            if duration.changed() {
                dialog.input = Some(SpeedDurationInput::Duration);
                // 入力途中の読めない文字は理由を出すだけにする。適用は欄を読み直して拒否する。
                match parse_duration(&dialog.duration_text, fps) {
                    Ok(frames) => {
                        dialog.duration_frames = frames;
                        dialog.sync(session);
                    }
                    Err(error) => dialog.error = error,
                }
            }
            let pitch_enabled = !dialog.still && dialog.speed_centi != 10_000;
            let pitch = ui.add_enabled_ui(pitch_enabled, |ui| {
                widgets::dialog_check(ui, &mut dialog.preserve_pitch, "オーディオのピッチを維持")
            });
            let ripple = widgets::dialog_check(
                ui,
                &mut dialog.ripple,
                "リップル編集 (後続クリップをシフト)",
            );
            if pitch.inner.clicked() || ripple.clicked() {
                dialog.sync(session);
            }
            if !dialog.error.is_empty() {
                ui.colored_label(WARNING, &dialog.error);
            }
        },
        |ui| {
            apply = widgets::dialog_button(ui, "適用", ButtonKind::Prominent).clicked();
            cancel = widgets::dialog_button(ui, "キャンセル", ButtonKind::Normal).clicked();
        },
    );
    if cancel || close {
        dialog.clip_id = None;
        return;
    }
    if dialog.confirm_overwrite {
        confirm_overwrite(ui, session, dialog, &id);
        return;
    }
    if apply {
        apply_speed(session, dialog, &id, false);
    }
}

/// 速度・尺の変更で延びた先の clip と重なるとき、上書きしてよいかを確かめる。
fn confirm_overwrite(ui: &mut egui::Ui, session: &mut Session, dialog: &mut SpeedDialog, id: &str) {
    let mut overwrite = false;
    let mut cancel = false;
    let width = (ui.ctx().content_rect().width() - 40.0).clamp(300.0, 420.0);
    let ((), close) = dialog::show(
        ui.ctx(),
        "speed_overwrite",
        "後続のクリップを上書き",
        width,
        |ui| {
            ui.label(
                "変更後のクリップが後ろのクリップと重なります。\n重なった部分を上書き (後ろのクリップを削除または短縮) してよいですか？",
            );
        },
        |ui| {
            overwrite = widgets::dialog_button(ui, "上書きする", ButtonKind::Prominent).clicked();
            cancel = widgets::dialog_button(ui, "キャンセル", ButtonKind::Normal).clicked();
        },
    );
    if cancel || close {
        dialog.confirm_overwrite = false;
    } else if overwrite {
        dialog.confirm_overwrite = false;
        apply_speed(session, dialog, id, true);
    }
}

fn apply_speed(session: &mut Session, dialog: &mut SpeedDialog, id: &str, overwrite: bool) {
    let edit = match dialog.edit(session.project().timeline_fps, overwrite) {
        Ok(edit) => edit,
        Err(error) => {
            dialog.error = error;
            return;
        }
    };
    // リップルしない変更が後続 clip と重なるなら、適用の前に上書きを確かめる。
    if !overwrite
        && !dialog.ripple
        && let Err(error) = session.preview_speed_duration(id, &edit)
        && error.overlaps_following
    {
        dialog.confirm_overwrite = true;
        return;
    }
    match session.set_speed_duration(id, &edit) {
        Ok(()) => dialog.clip_id = None,
        // ダイアログの陰になる status ではなく、ダイアログの中に理由を出す。
        Err(error) => dialog.error = error.message().to_owned(),
    }
}

/// Project の映像設定。素材に合わせるときは、その値で別のダイアログを開く。
pub struct SettingsDialog {
    open: bool,
    width: String,
    height: String,
    fps: Rational,
    /// 素材に合わせて開いたとき、その素材の名前と仕様、合わせた後の設定。
    matching: Option<(String, String, VideoSettings)>,
    error: String,
}

impl Default for SettingsDialog {
    fn default() -> Self {
        Self {
            open: false,
            width: String::new(),
            height: String::new(),
            fps: Rational::new(60, 1),
            matching: None,
            error: String::new(),
        }
    }
}

impl SettingsDialog {
    pub fn open(&mut self, session: &Session) {
        let settings = session.video_settings();
        *self = Self {
            open: true,
            width: settings.width.to_string(),
            height: settings.height.to_string(),
            fps: settings.fps,
            matching: None,
            error: String::new(),
        };
    }

    /// clip の素材に合わせる確認を開く（C++ 版の「プロジェクト設定を素材に合わせる」）。
    pub fn open_for_clip(&mut self, session: &mut Session, clip_id: &str) {
        let Some(clip) = session.project().clip(clip_id) else {
            session.set_alert("素材に合わせる clip がありません");
            return;
        };
        let name = clip.name.clone();
        let (source, settings, error) = match session.video_settings_for_clip(clip_id) {
            Ok(settings) => (
                format!(
                    "{}×{} / {}",
                    settings.width,
                    settings.height,
                    fps_label(settings.fps)
                ),
                settings,
                String::new(),
            ),
            Err(error) => (
                String::new(),
                session.video_settings(),
                error.message().to_owned(),
            ),
        };
        *self = Self {
            open: true,
            width: settings.width.to_string(),
            height: settings.height.to_string(),
            fps: settings.fps,
            matching: Some((name, source, settings)),
            error,
        };
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    /// 入力した幅と高さ。MP4（yuv420p）で扱える 2〜16384 の偶数でなければ None。
    fn requested(&self) -> Option<VideoSettings> {
        let parse = |text: &str| {
            text.trim()
                .parse::<i32>()
                .ok()
                .filter(|value| (2..=16384).contains(value) && value % 2 == 0)
        };
        Some(VideoSettings {
            width: parse(&self.width)?,
            height: parse(&self.height)?,
            fps: self.fps,
        })
    }
}

pub fn show_settings(ui: &mut egui::Ui, session: &mut Session, dialog: &mut SettingsDialog) {
    if !dialog.open {
        return;
    }
    if dialog.matching.is_some() {
        show_match_settings(ui, session, dialog);
        return;
    }
    let mut apply = false;
    let mut cancel = false;
    let has_clips = !session.project().timeline_clips.is_empty();
    let requested = dialog.requested();
    let width = (ui.ctx().content_rect().width() - 40.0).clamp(300.0, 520.0);
    let ((), close) = dialog::show(
        ui.ctx(),
        "project_settings",
        "プロジェクト設定",
        width,
        |ui| {
            egui::Grid::new("project_settings_grid")
                .num_columns(2)
                .spacing([12.0, 8.0])
                .show(ui, |ui| {
                    let field_width = ui.available_width().max(200.0);
                    for (label, text, hint) in [
                        ("幅", &mut dialog.width, "1920"),
                        ("高さ", &mut dialog.height, "1080"),
                    ] {
                        ui.label(label);
                        let response = ui.add_sized(
                            egui::vec2(field_width, 34.0),
                            egui::TextEdit::singleline(text)
                                .hint_text(hint)
                                .margin(egui::vec2(10.0, 8.0)),
                        );
                        if response.changed() {
                            text.retain(|character| character.is_ascii_digit());
                        }
                        ui.end_row();
                    }
                    ui.label("フレームレート");
                    let labels: Vec<String> = CONFIGURABLE_OUTPUT_FRAME_RATES
                        .iter()
                        .map(|fps| fps_label(*fps))
                        .collect();
                    let options: Vec<&str> = labels.iter().map(String::as_str).collect();
                    let current = CONFIGURABLE_OUTPUT_FRAME_RATES
                        .iter()
                        .position(|fps| *fps == dialog.fps)
                        .unwrap_or(0);
                    if let Some(index) = widgets::choice_box(
                        ui,
                        egui::Id::new("project_fps"),
                        &options,
                        current,
                        field_width,
                    ) && let Some(fps) = CONFIGURABLE_OUTPUT_FRAME_RATES.get(index)
                    {
                        dialog.fps = *fps;
                    }
                    ui.end_row();
                });
            if has_clips {
                ui.colored_label(
                    CAUTION,
                    "fpsを変更すると、既存クリップの開始位置を秒位置が保たれるよう換算します。素材のin/outは変更しません。",
                );
            }
            ui.colored_label(
                NOTE,
                "幅と高さはMP4（yuv420p）で扱える2〜16384の偶数を指定してください。",
            );
            if !dialog.error.is_empty() {
                ui.colored_label(FAILURE, &dialog.error);
            }
        },
        |ui| {
            let button = ui.add_enabled_ui(requested.is_some(), |ui| {
                widgets::dialog_button(ui, "適用", ButtonKind::Prominent)
            });
            apply = button.inner.clicked();
            cancel = widgets::dialog_button(ui, "キャンセル", ButtonKind::Normal).clicked();
        },
    );
    if cancel || close {
        dialog.open = false;
    } else if apply && let Some(settings) = requested {
        match session.set_video_settings(settings) {
            Ok(()) => dialog.open = false,
            Err(error) => dialog.error = error.message().to_owned(),
        }
    }
}

fn show_match_settings(ui: &mut egui::Ui, session: &mut Session, dialog: &mut SettingsDialog) {
    let Some((name, source, target)) = dialog.matching.clone() else {
        return;
    };
    let current = session.video_settings();
    let valid = dialog.error.is_empty();
    let changes = target != current;
    let has_clips = !session.project().timeline_clips.is_empty();
    let mut apply = false;
    let mut cancel = false;
    let width = (ui.ctx().content_rect().width() - 40.0).clamp(300.0, 580.0);
    let ((), close) = dialog::show(
        ui.ctx(),
        "match_settings",
        "プロジェクト設定を素材に合わせる",
        width,
        |ui| {
            if valid {
                ui.label(format!(
                    "素材: {name}\n{source}\n\n現在: {}×{} / {}\n変更後: {}×{} / {}",
                    current.width,
                    current.height,
                    fps_label(current.fps),
                    target.width,
                    target.height,
                    fps_label(target.fps)
                ));
                if has_clips {
                    ui.colored_label(
                        CAUTION,
                        "既存クリップの開始位置は秒位置を維持して換算します。素材のin/outは変更しません。続行しますか？",
                    );
                }
                if !changes {
                    ui.colored_label(NOTE, "プロジェクト設定はすでにこの素材と一致しています。");
                }
            } else {
                ui.colored_label(FAILURE, &dialog.error);
            }
        },
        |ui| {
            let button = ui.add_enabled_ui(valid && changes, |ui| {
                widgets::dialog_button(ui, "変更する", ButtonKind::Prominent)
            });
            apply = button.inner.clicked();
            cancel = widgets::dialog_button(ui, "キャンセル", ButtonKind::Normal).clicked();
        },
    );
    if cancel || close {
        dialog.open = false;
    } else if apply {
        match session.set_video_settings(target) {
            Ok(()) => dialog.open = false,
            Err(error) => dialog.error = error.message().to_owned(),
        }
    }
}

/// fps の表示。整数はそのまま、それ以外は小数 2 桁（C++ 版と同じ）。
pub fn fps_label(fps: Rational) -> String {
    if fps.den == 1 {
        format!("{} fps", fps.num)
    } else if fps.den > 0 {
        format!("{:.2} fps", fps.num as f64 / fps.den as f64)
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
        assert_eq!(dialog.speed_centi, 6667);
        // ピッチだけを変えても、速度は丸めた 66.67% ではなく元の 2/3 のまま。
        dialog.preserve_pitch = true;
        let edit = dialog.edit(Rational::new(60, 1), false).expect("編集");
        assert_eq!(edit.speed, Rational::new(2, 3));
    }

    #[test]
    fn an_edited_speed_uses_the_entered_percentage() {
        let mut session = session();
        let mut dialog = SpeedDialog::default();
        dialog.open(&mut session, "v");
        dialog.speed_centi = 5025;
        dialog.input = Some(SpeedDurationInput::Speed);
        let edit = dialog.edit(Rational::new(60, 1), false).expect("編集");
        assert_eq!(edit.speed, Rational::new(201, 400));
    }

    #[test]
    fn a_bad_duration_text_is_refused_instead_of_the_last_good_one() {
        let mut session = session();
        let mut dialog = SpeedDialog::default();
        dialog.open(&mut session, "v");
        let fps = Rational::new(60, 1);
        // 尺を 1 秒にしたあと、欄を読めない文字に書き換えた（最後に読めた尺は 60 frame）。
        dialog.input = Some(SpeedDurationInput::Duration);
        dialog.duration_text = "00:00:01:00".to_owned();
        assert_eq!(dialog.edit(fps, false).expect("編集").duration_frames, 60);
        dialog.duration_frames = 60;
        for text in ["00:00:0", "", "00:00:00:00", "abc"] {
            dialog.duration_text = text.to_owned();
            assert_eq!(
                dialog.edit(fps, false),
                Err(DURATION_FORMAT.to_owned()),
                "{text:?} を受け付けました"
            );
        }
        // 適用しても clip は変わらず、理由を出してダイアログは開いたまま。
        let before = session.project().clone();
        apply_speed(&mut session, &mut dialog, "v", false);
        assert_eq!(session.project(), &before);
        assert_eq!(dialog.error, DURATION_FORMAT);
        assert!(dialog.is_open());
    }

    #[test]
    fn a_still_clip_opens_to_change_only_its_duration() {
        let mut session = Session::from_project(Project {
            timeline_clips: vec![TimelineClip {
                kind: TimelineClipKind::Image,
                media_path: "still.png".into(),
                name: "静止画".to_owned(),
                id: "still".to_owned(),
                source_fps: Rational::new(60, 1),
                source_out_frame: 120,
                track: TrackRef::video(0),
                ..TimelineClip::default()
            }],
            ..Project::default()
        });
        let mut dialog = SpeedDialog::default();
        dialog.open(&mut session, "still");
        assert!(dialog.is_open(), "{:?}", session.alert());
        assert!(dialog.still);
        // 開いた直後から尺の入力として扱い、ピッチは持たせない。
        dialog.preserve_pitch = true;
        dialog.duration_text = "00:00:03:00".to_owned();
        let edit = dialog.edit(Rational::new(60, 1), false).expect("編集");
        assert_eq!(edit.input, SpeedDurationInput::Duration);
        assert_eq!(edit.duration_frames, 180);
        assert!(!edit.preserve_pitch);
        apply_speed(&mut session, &mut dialog, "still", false);
        assert!(!dialog.is_open(), "{}", dialog.error);
        let clip = session.project().clip("still").expect("clip");
        assert_eq!(
            timeline_clip_duration(session.project(), clip).expect("尺"),
            180
        );
        assert_eq!(clip.speed, Rational::new(1, 1));
    }

    #[test]
    fn the_size_must_be_an_even_number_in_range() {
        let mut dialog = SettingsDialog {
            width: "1280".to_owned(),
            height: "720".to_owned(),
            ..SettingsDialog::default()
        };
        assert!(dialog.requested().is_some());
        dialog.width = "1281".to_owned();
        assert!(dialog.requested().is_none(), "奇数を受け付けました");
        dialog.width = "20000".to_owned();
        assert!(dialog.requested().is_none());
        dialog.width = String::new();
        assert!(dialog.requested().is_none());
    }
}
