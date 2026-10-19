//! オーディオミキサー。音声 track ごとに音量・定位・名前・M/S とメーター、最後にマスター（モニター音量）。
//!
//! 値は離したとき（入力欄は確定したとき）に 1 回だけ commit する。

use eframe::egui::{self, Color32, Rect, Sense, Vec2};
use mvm_app::Session;
use mvm_project::TrackRef;

/// フェーダーの下限。これ以下は無音（`-inf` と表示）。
const SILENT_DB: f64 = -96.0;
const MAXIMUM_DB: f64 = 15.0;
/// メーターの目盛りの下端。
const METER_FLOOR_DB: f32 = -60.0;
/// 表示の落ち方（dB / 秒）と、peak の保持時間。
const FALL_DB_PER_SECOND: f32 = 24.0;
const PEAK_HOLD_SECONDS: f64 = 1.0;
const METER_SIZE: Vec2 = Vec2::new(14.0, 120.0);

#[derive(Default)]
pub struct MixerState {
    drafts: Vec<StripDraft>,
    revision: u64,
    meters: Vec<MeterDisplay>,
    master: MeterDisplay,
    last_time: f64,
}

#[derive(Clone, Default)]
struct StripDraft {
    gain_db: f64,
    pan_percent: f64,
    name: String,
}

#[derive(Clone, Copy)]
struct MeterDisplay {
    level_db: f32,
    peak_db: f32,
    peak_time: f64,
    clipped: bool,
}

/// 何も鳴っていないメーター（目盛りの下端）。0 dB から始めると、開いた直後に満量を表示してしまう。
impl Default for MeterDisplay {
    fn default() -> Self {
        Self {
            level_db: METER_FLOOR_DB,
            peak_db: METER_FLOOR_DB,
            peak_time: 0.0,
            clipped: false,
        }
    }
}

impl MeterDisplay {
    fn feed(&mut self, peak: f32, now: f64, elapsed: f32) {
        let incoming = if peak > 0.0 {
            20.0 * peak.log10()
        } else {
            METER_FLOOR_DB
        };
        let fallen = (self.level_db - FALL_DB_PER_SECOND * elapsed).max(METER_FLOOR_DB);
        self.level_db = incoming.max(fallen);
        if incoming >= self.peak_db || now - self.peak_time > PEAK_HOLD_SECONDS {
            self.peak_db = incoming.max(METER_FLOOR_DB);
            self.peak_time = now;
        }
        if peak > 1.0 {
            self.clipped = true;
        }
    }
}

pub fn show(ui: &mut egui::Ui, session: &mut Session, state: &mut MixerState) {
    let now = ui.input(|input| input.time);
    let elapsed = if state.last_time > 0.0 {
        (now - state.last_time).clamp(0.0, 1.0) as f32
    } else {
        0.0
    };
    state.last_time = now;
    let peaks = session.take_meters();
    let tracks = session.project().audio_tracks.len();
    state.meters.resize(tracks, MeterDisplay::default());
    for (index, meter) in state.meters.iter_mut().enumerate() {
        meter.feed(
            peaks.tracks.get(index).copied().unwrap_or(0.0),
            now,
            elapsed,
        );
    }
    state.master.feed(peaks.master, now, elapsed);
    if state.revision != session.edit_revision() || state.drafts.len() != tracks {
        state.revision = session.edit_revision();
        state.drafts = session
            .project()
            .audio_tracks
            .iter()
            .map(|track| StripDraft {
                gain_db: track.mixer_gain_db,
                pan_percent: track.mixer_pan * 100.0,
                name: track.mixer_name.clone(),
            })
            .collect();
    }
    if session.is_playing() {
        ui.ctx().request_repaint();
    }

    egui::ScrollArea::horizontal()
        .id_salt("mixer_scroll")
        .show(ui, |ui| {
            ui.horizontal_top(|ui| {
                for index in 0..tracks {
                    ui.group(|ui| {
                        ui.vertical(|ui| strip(ui, session, state, index));
                    });
                }
                ui.group(|ui| {
                    ui.vertical(|ui| master(ui, session, state));
                });
            });
        });
}

fn strip(ui: &mut egui::Ui, session: &mut Session, state: &mut MixerState, index: usize) {
    let Some(track) = session.project().audio_tracks.get(index).cloned() else {
        return;
    };
    let Some(draft) = state.drafts.get_mut(index) else {
        return;
    };
    let mut commit = false;
    ui.label(egui::RichText::new(&track.name).strong());

    let pan = ui
        .add(
            egui::DragValue::new(&mut draft.pan_percent)
                .range(-100.0..=100.0)
                .speed(1.0)
                .custom_formatter(|value, _| pan_text(value)),
        )
        .on_hover_text("定位。-100 が左、100 が右。ダブルクリックで中央");
    if pan.double_clicked() {
        draft.pan_percent = 0.0;
        commit = true;
    }
    commit |= pan.drag_stopped() || pan.lost_focus();

    ui.horizontal(|ui| {
        let meter = state.meters.get_mut(index);
        if let Some(meter) = meter
            && draw_meter(ui, meter)
        {
            meter.clipped = false;
        }
        let fader = ui
            .add(
                egui::Slider::new(&mut draft.gain_db, SILENT_DB..=MAXIMUM_DB)
                    .vertical()
                    .show_value(false),
            )
            .on_hover_text("音量。ダブルクリックで 0 dB");
        if fader.double_clicked() {
            draft.gain_db = 0.0;
            commit = true;
        }
        commit |= fader.drag_stopped();
    });
    let value = ui.add(
        egui::DragValue::new(&mut draft.gain_db)
            .range(SILENT_DB..=MAXIMUM_DB)
            .speed(0.1)
            .custom_formatter(|value, _| gain_text(value))
            .custom_parser(parse_gain),
    );
    commit |= value.drag_stopped() || value.lost_focus();

    ui.horizontal(|ui| {
        let mute = egui::Button::new("消音").selected(track.muted);
        if ui.add(mute).clicked()
            && let Err(error) = session.toggle_track_muted(TrackRef::audio(index))
        {
            session.set_alert(error.message());
        }
        let solo = egui::Button::new("ソロ").selected(track.solo);
        if ui.add(solo).clicked()
            && let Err(error) = session.toggle_track_solo(TrackRef::audio(index))
        {
            session.set_alert(error.message());
        }
    });
    let name = ui.add(
        egui::TextEdit::singleline(&mut draft.name)
            .hint_text(track.name.as_str())
            .desired_width(80.0),
    );
    if name.has_focus() && ui.input(|input| input.key_pressed(egui::Key::Escape)) {
        draft.name = track.mixer_name.clone();
        name.surrender_focus();
    } else {
        commit |= name.lost_focus();
    }

    if commit {
        let draft = draft.clone();
        let changed = draft.gain_db != track.mixer_gain_db
            || draft.pan_percent / 100.0 != track.mixer_pan
            || draft.name.trim() != track.mixer_name;
        if changed
            && let Err(error) =
                session.set_track_mix(index, draft.gain_db, draft.pan_percent / 100.0, &draft.name)
        {
            session.set_alert(error.message());
            state.revision = 0;
        }
    }
}

fn master(ui: &mut egui::Ui, session: &mut Session, state: &mut MixerState) {
    ui.label(egui::RichText::new("マスター").strong());
    ui.label("モニター");
    let mut gain_db = if session.monitor_volume() > 0.0 {
        f64::from(20.0 * session.monitor_volume().log10())
    } else {
        SILENT_DB
    };
    ui.horizontal(|ui| {
        if draw_meter(ui, &mut state.master) {
            state.master.clipped = false;
        }
        let fader = ui
            .add(
                egui::Slider::new(&mut gain_db, SILENT_DB..=12.0)
                    .vertical()
                    .show_value(false),
            )
            .on_hover_text("再生の出口の音量（書き出しには掛からない）。ダブルクリックで 0 dB");
        if fader.double_clicked() {
            gain_db = 0.0;
        }
        if fader.changed() || fader.double_clicked() {
            let gain = if gain_db <= SILENT_DB {
                0.0
            } else {
                10f64.powf(gain_db / 20.0) as f32
            };
            if let Err(error) = session.set_monitor_volume(gain) {
                session.set_alert(error.message());
            }
        }
    });
    ui.label(gain_text(gain_db));
}

/// メーターを描く。クリップのランプを押したら true（解除する）。
fn draw_meter(ui: &mut egui::Ui, meter: &mut MeterDisplay) -> bool {
    let (rect, response) = ui.allocate_exact_size(METER_SIZE, Sense::click());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 2.0, Color32::from_gray(24));
    let fraction = |db: f32| ((db - METER_FLOOR_DB) / -METER_FLOOR_DB).clamp(0.0, 1.0);
    let lamp_height = 8.0;
    let bar = Rect::from_min_max(rect.min + Vec2::new(0.0, lamp_height + 2.0), rect.max);
    let level = fraction(meter.level_db);
    if level > 0.0 {
        let top = bar.max.y - bar.height() * level;
        let color = if meter.level_db > -6.0 {
            Color32::from_rgb(230, 80, 60)
        } else if meter.level_db > -18.0 {
            Color32::from_rgb(220, 200, 70)
        } else {
            Color32::from_rgb(80, 190, 110)
        };
        painter.rect_filled(
            Rect::from_min_max(egui::pos2(bar.min.x, top), bar.max),
            1.0,
            color,
        );
    }
    let peak = fraction(meter.peak_db);
    if peak > 0.0 {
        let y = bar.max.y - bar.height() * peak;
        painter.line_segment(
            [egui::pos2(bar.min.x, y), egui::pos2(bar.max.x, y)],
            egui::Stroke::new(1.5, Color32::from_rgb(240, 220, 80)),
        );
    }
    let lamp = Rect::from_min_size(rect.min, Vec2::new(rect.width(), lamp_height));
    painter.rect_filled(
        lamp,
        1.0,
        if meter.clipped {
            Color32::from_rgb(220, 40, 40)
        } else {
            Color32::from_gray(50)
        },
    );
    response
        .on_hover_text("0 dB を超えると赤く残る。クリックで解除")
        .clicked()
}

fn gain_text(value: f64) -> String {
    if value <= SILENT_DB {
        "-inf dB".to_owned()
    } else {
        format!("{value:+.1} dB")
    }
}

fn parse_gain(text: &str) -> Option<f64> {
    let trimmed = text.trim().trim_end_matches("dB").trim();
    if matches!(trimmed, "-inf" | "-∞") {
        return Some(SILENT_DB);
    }
    trimmed
        .parse::<f64>()
        .ok()
        .filter(|value| value.is_finite() && (SILENT_DB..=MAXIMUM_DB).contains(value))
}

fn pan_text(value: f64) -> String {
    let rounded = value.round() as i64;
    match rounded {
        0 => "定位 中央".to_owned(),
        value if value < 0 => format!("定位 左 {}", -value),
        value => format!("定位 右 {value}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gain_text_and_parser_agree_on_silence_and_range() {
        assert_eq!(gain_text(-96.0), "-inf dB");
        assert_eq!(gain_text(-6.0), "-6.0 dB");
        assert_eq!(parse_gain("-inf"), Some(SILENT_DB));
        assert_eq!(parse_gain("-∞"), Some(SILENT_DB));
        assert_eq!(parse_gain(" 3.5 dB"), Some(3.5));
        assert_eq!(parse_gain("16"), None);
        assert_eq!(parse_gain("abc"), None);
    }

    #[test]
    fn a_fresh_meter_shows_silence_until_something_plays() {
        let mut meter = MeterDisplay::default();
        meter.feed(0.0, 0.0, 0.0);
        assert_eq!(meter.level_db, METER_FLOOR_DB);
        assert_eq!(meter.peak_db, METER_FLOOR_DB);
        assert!(!meter.clipped);
    }

    #[test]
    fn meter_falls_over_time_and_latches_clipping() {
        let mut meter = MeterDisplay {
            level_db: METER_FLOOR_DB,
            peak_db: METER_FLOOR_DB,
            ..MeterDisplay::default()
        };
        meter.feed(1.0, 0.0, 0.0);
        assert!((meter.level_db - 0.0).abs() < 1e-6);
        assert!(!meter.clipped);
        meter.feed(0.0, 0.5, 0.5);
        assert!((meter.level_db + 12.0).abs() < 1e-4, "{}", meter.level_db);
        assert!(
            (meter.peak_db - 0.0).abs() < 1e-6,
            "保持中の peak が落ちました"
        );
        meter.feed(0.0, 2.0, 1.5);
        assert_eq!(meter.peak_db, METER_FLOOR_DB);
        meter.feed(1.5, 2.1, 0.1);
        assert!(meter.clipped);
        meter.feed(0.0, 3.0, 0.9);
        assert!(meter.clipped, "ランプが自然に消えました");
    }
}
