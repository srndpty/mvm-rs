//! inspector の effect（モーション・クロップ・不透明度・音量）・フェード・文字の編集。
//!
//! 値は再生ヘッドの位置（clip の外なら先頭）で評価して見せる。ドラッグを離すか入力欄を抜けたときに 1 回だけ
//! commit する。キーのある channel を変えると、その位置のキーを置き換える。

use eframe::egui;
use mvm_app::Session;
use mvm_project::clip_effects::{ClipKeyKind, KeyInterpolation, evaluate_clip_keys};
use mvm_project::{TextClipData, TimelineClip, TimelineClipKind};

use crate::widgets::color_row;

macro_rules! report {
    ($session:ident, $call:expr $(,)?) => {{
        let result = $call;
        if let Err(error) = result {
            $session.set_alert(error.message());
        }
    }};
}

/// 映像系の clip に出す channel と表示名・単位。
const VISUAL_ROWS: [(ClipKeyKind, &str, &str); 10] = [
    (ClipKeyKind::PositionX, "位置 X", " %"),
    (ClipKeyKind::PositionY, "位置 Y", " %"),
    (ClipKeyKind::ScaleX, "拡大率 X", " %"),
    (ClipKeyKind::ScaleY, "拡大率 Y", " %"),
    (ClipKeyKind::Rotation, "回転", " °"),
    (ClipKeyKind::CropLeft, "クロップ 左", " %"),
    (ClipKeyKind::CropTop, "クロップ 上", " %"),
    (ClipKeyKind::CropRight, "クロップ 右", " %"),
    (ClipKeyKind::CropBottom, "クロップ 下", " %"),
    (ClipKeyKind::Opacity, "不透明度", " %"),
];

const AUDIO_ROWS: [(ClipKeyKind, &str, &str); 2] = [
    (ClipKeyKind::Volume, "音量", " %"),
    (ClipKeyKind::Ducking, "ダッキング", " dB"),
];

const CHANNELS: usize = 12;

pub struct MotionState {
    clip_id: Option<String>,
    revision: u64,
    frame: i64,
    values: [f64; CHANNELS],
    lock_aspect: bool,
    fade_in: i64,
    fade_out: i64,
    text: Option<TextClipData>,
}

impl Default for MotionState {
    fn default() -> Self {
        Self {
            clip_id: None,
            revision: 0,
            frame: 0,
            values: [0.0; CHANNELS],
            lock_aspect: true,
            fade_in: 0,
            fade_out: 0,
            text: None,
        }
    }
}

impl MotionState {
    /// Project・再生ヘッド・clip が変わったら、下書きを Project の値で読み直す。
    fn load(&mut self, session: &Session, clip: &TimelineClip) {
        let frame = session.effect_frame(&clip.id).unwrap_or(0);
        let same = self.clip_id.as_deref() == Some(clip.id.as_str())
            && self.revision == session.edit_revision();
        if same && self.frame == frame {
            return;
        }
        // 再生ヘッドが動いただけなら、評価し直す値だけを読み直す（入力中の文字は残す）。
        self.frame = frame;
        for (index, value) in self.values.iter_mut().enumerate() {
            if let Some(kind) = kind_at(index) {
                *value =
                    evaluate_clip_keys(clip.effects.keys(kind), clip.effects.base(kind), frame);
            }
        }
        if same {
            return;
        }
        self.clip_id = Some(clip.id.clone());
        self.revision = session.edit_revision();
        self.fade_in = clip.effects.fade_in_frames;
        self.fade_out = clip.effects.fade_out_frames;
        self.text = (clip.kind == TimelineClipKind::Text).then(|| clip.text.clone());
    }
}

fn kind_at(index: usize) -> Option<ClipKeyKind> {
    mvm_project::clip_effects::EFFECT_CHANNELS
        .get(index)
        .map(|channel| channel.kind)
}

pub fn show(
    ui: &mut egui::Ui,
    session: &mut Session,
    state: &mut MotionState,
    clip: &TimelineClip,
) {
    state.load(session, clip);
    if let Some(text) = state.text.as_mut() {
        show_text(ui, session, &clip.id, text);
    }
    let audio = clip.kind == TimelineClipKind::Audio;
    let rows: &[(ClipKeyKind, &str, &str)] = if audio { &AUDIO_ROWS } else { &VISUAL_ROWS };
    egui::CollapsingHeader::new(if audio { "音声" } else { "モーション" })
        .id_salt("effect_rows")
        .default_open(true)
        .show(ui, |ui| {
            ui.weak(format!("clip 内 {} frame の値", state.frame));
            if !audio {
                ui.checkbox(&mut state.lock_aspect, "縦横比を固定");
            }
            egui::Grid::new("effect_grid")
                .num_columns(3)
                .spacing([6.0, 3.0])
                .show(ui, |ui| {
                    for (kind, label, suffix) in rows {
                        effect_row(ui, session, state, clip, *kind, label, suffix);
                        ui.end_row();
                    }
                });
            fade_rows(ui, session, state, clip);
        });
}

fn effect_row(
    ui: &mut egui::Ui,
    session: &mut Session,
    state: &mut MotionState,
    clip: &TimelineClip,
    kind: ClipKeyKind,
    label: &str,
    suffix: &str,
) {
    let channel = kind.channel();
    let keys = clip.effects.keys(kind);
    let animated = !keys.is_empty();
    let at_key = keys.iter().any(|key| key.frame == state.frame);
    let index = kind as usize;
    ui.label(label);
    let Some(value) = state.values.get_mut(index) else {
        return;
    };
    let response = ui.add(
        egui::DragValue::new(value)
            .range(channel.minimum..=channel.maximum)
            .speed(0.5)
            .max_decimals(2)
            .suffix(suffix),
    );
    if response.drag_stopped() || response.lost_focus() {
        let value = *value;
        let pair = match kind {
            ClipKeyKind::ScaleX if state.lock_aspect => Some(ClipKeyKind::ScaleY),
            ClipKeyKind::ScaleY if state.lock_aspect => Some(ClipKeyKind::ScaleX),
            _ => None,
        };
        match pair {
            Some(other) => {
                report!(
                    session,
                    session.set_effects(&clip.id, &[(kind, value), (other, value)])
                );
            }
            None => report!(session, session.set_effect(&clip.id, kind, value)),
        }
    }
    ui.horizontal(|ui| {
        if ui
            .selectable_label(animated, "アニメ")
            .on_hover_text("アニメーションを切り替える")
            .clicked()
        {
            report!(
                session,
                session.set_key_animation(&clip.id, kind, !animated)
            );
        }
        if animated {
            if ui
                .small_button("前")
                .on_hover_text("前のキーへ移動")
                .clicked()
            {
                session.seek_key(&clip.id, kind, false);
            }
            if ui
                .selectable_label(at_key, "キー")
                .on_hover_text("現在位置のキーを追加・削除")
                .clicked()
            {
                report!(session, session.toggle_key(&clip.id, kind));
            }
            if ui
                .small_button("次")
                .on_hover_text("次のキーへ移動")
                .clicked()
            {
                session.seek_key(&clip.id, kind, true);
            }
            if let Some(current) = keys
                .iter()
                .find(|key| key.frame == state.frame)
                .map(|key| key.interpolation)
            {
                let mut chosen = current;
                egui::ComboBox::from_id_salt(("interpolation", index))
                    .selected_text(interpolation_name(current))
                    .show_ui(ui, |ui| {
                        for option in KeyInterpolation::ALL {
                            ui.selectable_value(&mut chosen, option, interpolation_name(option));
                        }
                    });
                if chosen != current {
                    report!(
                        session,
                        session.set_key_interpolation(&clip.id, kind, chosen)
                    );
                }
            }
        }
    });
}

fn interpolation_name(interpolation: KeyInterpolation) -> &'static str {
    match interpolation {
        KeyInterpolation::Linear => "リニア",
        KeyInterpolation::EaseIn => "イーズイン",
        KeyInterpolation::EaseOut => "イーズアウト",
        KeyInterpolation::EaseInOut => "イーズインアウト",
        KeyInterpolation::Spline => "スプライン",
    }
}

fn fade_rows(
    ui: &mut egui::Ui,
    session: &mut Session,
    state: &mut MotionState,
    clip: &TimelineClip,
) {
    let limit = clip.source_duration().max(0);
    let mut commit = false;
    ui.horizontal(|ui| {
        ui.label("フェードイン");
        let response = ui.add(
            egui::DragValue::new(&mut state.fade_in)
                .range(0..=limit)
                .suffix(" 素材f"),
        );
        commit |= response.drag_stopped() || response.lost_focus();
    });
    ui.horizontal(|ui| {
        ui.label("フェードアウト");
        let response = ui.add(
            egui::DragValue::new(&mut state.fade_out)
                .range(0..=limit)
                .suffix(" 素材f"),
        );
        commit |= response.drag_stopped() || response.lost_focus();
    });
    if commit {
        report!(
            session,
            session.set_fades(&clip.id, state.fade_in, state.fade_out)
        );
    }
}

/// 文字 clip の本文と書式。「文字を適用」で 1 回だけ commit する。
fn show_text(ui: &mut egui::Ui, session: &mut Session, clip_id: &str, text: &mut TextClipData) {
    egui::CollapsingHeader::new("文字")
        .id_salt("text_clip")
        .default_open(true)
        .show(ui, |ui| {
            ui.add(
                egui::TextEdit::multiline(&mut text.content)
                    .hint_text("文字を入力")
                    .desired_rows(3),
            );
            ui.horizontal(|ui| {
                ui.label("フォント");
                ui.text_edit_singleline(&mut text.font_family);
            });
            ui.horizontal(|ui| {
                ui.add(
                    egui::DragValue::new(&mut text.font_size)
                        .range(1..=2000)
                        .suffix(" px"),
                );
                ui.checkbox(&mut text.bold, "太字");
            });
            ui.horizontal(|ui| {
                ui.label("位置");
                ui.add(egui::DragValue::new(&mut text.x).prefix("X "));
                ui.add(egui::DragValue::new(&mut text.y).prefix("Y "));
            });
            ui.horizontal(|ui| {
                ui.label("揃え");
                for (value, label) in [("left", "左"), ("center", "中央"), ("right", "右")] {
                    if ui
                        .selectable_label(text.alignment == value, label)
                        .clicked()
                    {
                        text.alignment = value.to_owned();
                    }
                }
            });
            color_row(ui, "文字色", &mut text.color);
            color_row(ui, "縁の色", &mut text.outline_color);
            ui.add(
                egui::DragValue::new(&mut text.outline_width)
                    .range(0..=32)
                    .prefix("縁の太さ "),
            );
            color_row(ui, "背景色", &mut text.background_color);
            if ui.button("文字を適用").clicked() {
                report!(session, session.edit_text(clip_id, text.clone()));
            }
        });
}
