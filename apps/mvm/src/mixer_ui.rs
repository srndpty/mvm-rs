//! オーディオミキサー。音声 track ごとに定位・M/S・音量・左右のメーター・名前、最後にマスター（モニター音量）。
//! preview の右にも、同じマスターを細い帯で常に出す。
//!
//! track の値は離したとき（数値は確定したとき）に 1 回だけ commit する。マスターは再生の出口の音量で、
//! Project に入らないので動かした分だけすぐ反映する。

use eframe::egui::{self, Align2, Color32, FontId, Pos2, Rect, Sense, Stroke, Vec2};
use mvm_app::Session;
use mvm_project::TrackRef;

use crate::{theme, widgets};

/// フェーダーの下限。これ以下は無音（-∞ と表示）。
const SILENT_DB: f64 = -96.0;
const MAXIMUM_DB: f64 = 15.0;
/// マスターはモニター音量（倍率 4 まで）なので、+12 dB で止める。
const MASTER_MAXIMUM_DB: f64 = 12.0;
/// メーターの目盛りの下端。
const METER_FLOOR_DB: f32 = -60.0;
/// 表示の落ち方（dB / 秒）と、peak の保持時間。
const FALL_DB_PER_SECOND: f32 = 36.0;
const PEAK_HOLD_SECONDS: f64 = 1.0;

/// フェーダーの目盛り。dB と、レールの上端からの割合。0 dB 付近を広く取る。
const FADER_DB: [f64; 8] = [15.0, 6.0, 0.0, -6.0, -12.0, -24.0, -48.0, -96.0];
const FADER_POSITION: [f64; 8] = [0.0, 0.14, 0.25, 0.38, 0.5, 0.65, 0.82, 1.0];
const METER_TICKS: [f32; 7] = [0.0, -6.0, -12.0, -24.0, -36.0, -48.0, -60.0];

const STRIP_WIDTH: f32 = 111.0;
const FADER_WIDTH: f32 = 43.0;

#[derive(Default)]
pub struct MixerState {
    drafts: Vec<StripDraft>,
    revision: u64,
    meters: Vec<StereoMeter>,
    master: StereoMeter,
    last_time: f64,
    /// 数値を入力している欄と、その文字列。
    editing: Option<(ValueField, String)>,
    /// ドラッグ中のフェーダー・パンの位置（0〜1 / -1〜1）。離したときに commit する。
    dragging: Option<f64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ValueField {
    Gain(usize),
    Pan(usize),
    Master,
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

/// 左右のメーター。クリップのランプは左右で 1 つ。
#[derive(Clone, Copy, Default)]
struct StereoMeter([MeterDisplay; 2]);

impl StereoMeter {
    fn feed(&mut self, peak: [f32; 2], now: f64, elapsed: f32) {
        for (channel, value) in self.0.iter_mut().zip(peak) {
            channel.feed(value, now, elapsed);
        }
    }

    fn clipped(&self) -> bool {
        self.0.iter().any(|channel| channel.clipped)
    }

    fn clear_clip(&mut self) {
        for channel in &mut self.0 {
            channel.clipped = false;
        }
    }

    fn loudest_db(&self) -> f32 {
        self.0[0].level_db.max(self.0[1].level_db)
    }
}

impl MixerState {
    /// 再生した音のメーターを進め、Project が変わっていれば下書きを読み直す。毎 frame 1 回だけ呼ぶ
    /// （ミキサーのタブとマスターの帯が同じ値を読む）。
    pub fn update(&mut self, ui: &egui::Ui, session: &mut Session) {
        let now = ui.input(|input| input.time);
        let elapsed = if self.last_time > 0.0 {
            (now - self.last_time).clamp(0.0, 1.0) as f32
        } else {
            0.0
        };
        self.last_time = now;
        let peaks = session.take_meters();
        let tracks = session.project().audio_tracks.len();
        self.meters.resize(tracks, StereoMeter::default());
        for (index, meter) in self.meters.iter_mut().enumerate() {
            meter.feed(
                peaks.tracks.get(index).copied().unwrap_or_default(),
                now,
                elapsed,
            );
        }
        self.master.feed(peaks.master, now, elapsed);
        if self.revision != session.edit_revision() || self.drafts.len() != tracks {
            self.revision = session.edit_revision();
            self.drafts = session
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
        let falling = self
            .meters
            .iter()
            .chain(std::iter::once(&self.master))
            .any(|meter| meter.loudest_db() > METER_FLOOR_DB);
        if session.is_playing() || falling {
            ui.ctx().request_repaint();
        }
    }
}

/// ミキサーのタブ。track ごとの帯と、最後にマスターの帯。
pub fn show(ui: &mut egui::Ui, session: &mut Session, state: &mut MixerState) {
    let tracks = session.project().audio_tracks.len();
    egui::ScrollArea::horizontal()
        .id_salt("mixer_scroll")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            let height = ui.available_height().max(300.0);
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = 1.0;
                for index in 0..tracks {
                    strip_frame(ui, theme::PANEL, height, |ui| {
                        strip(ui, session, state, index);
                    });
                }
                strip_frame(
                    ui,
                    egui::Color32::from_rgb(0x24, 0x29, 0x31),
                    height,
                    |ui| {
                        master(ui, session, state, false);
                    },
                );
            });
        });
}

/// preview の右に置く、マスターの細い帯（フェーダーとメーターだけ）。
pub fn show_master_strip(ui: &mut egui::Ui, session: &mut Session, state: &mut MixerState) {
    master(ui, session, state, true);
}

fn strip_frame(ui: &mut egui::Ui, fill: Color32, height: f32, add: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::new()
        .fill(fill)
        .stroke(Stroke::new(1.0, Color32::from_rgb(0x34, 0x3b, 0x45)))
        .inner_margin(egui::Margin::same(5))
        .show(ui, |ui| {
            ui.set_width(STRIP_WIDTH - 10.0);
            ui.set_height(height - 12.0);
            ui.vertical_centered(add);
        });
}

fn strip(ui: &mut egui::Ui, session: &mut Session, state: &mut MixerState, index: usize) {
    let Some(track) = session.project().audio_tracks.get(index).cloned() else {
        return;
    };
    let Some(mut draft) = state.drafts.get(index).cloned() else {
        return;
    };
    let mut commit = false;

    let pan = pan_knob(ui, draft.pan_percent / 100.0, &mut state.dragging);
    if let Some(edit) = pan {
        draft.pan_percent = edit.value * 100.0;
        commit |= edit.commit;
    }
    if let Some(value) = value_field(
        ui,
        &mut state.editing,
        ValueField::Pan(index),
        draft.pan_percent,
        ui.available_width(),
        pan_text,
        parse_pan,
    ) {
        draft.pan_percent = value;
        commit = true;
    }

    ui.add_space(4.0);
    ui.horizontal(|ui| {
        ui.add_space((ui.available_width() - 54.0).max(0.0) / 2.0);
        let mute = widgets::track_toggle(ui, "M", track.muted, theme::MUTE_ON, "ミュート");
        if mute.clicked()
            && let Err(error) = session.toggle_track_muted(TrackRef::audio(index))
        {
            session.set_alert(error.message());
        }
        let solo = widgets::track_toggle(
            ui,
            "S",
            track.solo,
            theme::SOLO_ON,
            "ソロ（この track だけを鳴らす）",
        );
        if solo.clicked()
            && let Err(error) = session.toggle_track_solo(TrackRef::audio(index))
        {
            session.set_alert(error.message());
        }
    });
    ui.add_space(4.0);

    let name_height = 24.0 + 16.0 + 26.0;
    let body = (ui.available_height() - name_height).max(90.0);
    let meter = state.meters.get_mut(index);
    let fader = Fader {
        id: egui::Id::new(("mixer_track", index)),
        gain_db: draft.gain_db,
        maximum_db: MAXIMUM_DB,
        height: body,
        show_scale: true,
    };
    let edit = fader_and_meter(ui, &fader, meter, &mut state.dragging);
    if let Some(edit) = edit {
        draft.gain_db = edit.value;
        commit |= edit.commit;
    }
    let loudest = state
        .meters
        .get(index)
        .map_or(METER_FLOOR_DB, StereoMeter::loudest_db);
    if let Some(value) = gain_row(
        ui,
        &mut state.editing,
        ValueField::Gain(index),
        draft.gain_db,
        loudest,
    ) {
        draft.gain_db = value;
        commit = true;
    }

    let name = ui.add(
        egui::TextEdit::singleline(&mut draft.name)
            // 名前が空なら「音声 N」と見せる（C++ 版と同じ。確定するまで Project には書かない）。
            .hint_text(format!("音声 {}", index + 1))
            .horizontal_align(egui::Align::Center)
            .font(FontId::proportional(11.0))
            .desired_width(f32::INFINITY),
    );
    if name.has_focus() && ui.input(|input| input.key_pressed(egui::Key::Escape)) {
        draft.name = track.mixer_name.clone();
        name.surrender_focus();
    } else {
        commit |= name.lost_focus();
    }
    ui.label(
        egui::RichText::new(&track.name)
            .size(10.0)
            .color(theme::SCALE_TEXT),
    );

    if let Some(slot) = state.drafts.get_mut(index) {
        *slot = draft.clone();
    }
    if commit {
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

fn master(ui: &mut egui::Ui, session: &mut Session, state: &mut MixerState, compact: bool) {
    let gain_db = if session.monitor_volume() > 0.0 {
        f64::from(20.0 * session.monitor_volume().log10())
    } else {
        SILENT_DB
    };
    let mut edited = None;
    if !compact {
        ui.add_space(30.0);
        ui.label(
            egui::RichText::new("マスター")
                .size(11.0)
                .color(theme::TEXT_ICON),
        );
        ui.add_space(70.0);
    }
    let reserved = if compact { 26.0 } else { 26.0 + 24.0 + 16.0 };
    let body = (ui.available_height() - reserved).max(90.0);
    let fader = Fader {
        id: egui::Id::new(("mixer_master", compact)),
        gain_db,
        maximum_db: MASTER_MAXIMUM_DB,
        height: body,
        show_scale: !compact,
    };
    if let Some(edit) = fader_and_meter(ui, &fader, Some(&mut state.master), &mut state.dragging) {
        edited = Some(edit.value);
    }
    if let Some(value) = gain_row(
        ui,
        &mut state.editing,
        ValueField::Master,
        gain_db,
        state.master.loudest_db(),
    ) {
        edited = Some(value.min(MASTER_MAXIMUM_DB));
    }
    if !compact {
        ui.add_space(24.0);
        ui.label(
            egui::RichText::new("再生の出口")
                .size(10.0)
                .color(theme::SCALE_TEXT),
        );
    }
    if let Some(value) = edited {
        let gain = if value <= SILENT_DB {
            0.0
        } else {
            10f64.powf(value / 20.0) as f32
        };
        if let Err(error) = session.set_monitor_volume(gain) {
            session.set_alert(error.message());
        }
    }
}

/// 動かしている値と、離して確定したか。
struct Edit {
    value: f64,
    commit: bool,
}

/// フェーダーとその右のメーターを横に並べる。フェーダーを動かしたら値を返す。
/// フェーダーの帯の形。`show_scale` が false なら、メーターの目盛りを省いて細くする。
struct Fader {
    id: egui::Id,
    gain_db: f64,
    maximum_db: f64,
    height: f32,
    show_scale: bool,
}

fn fader_and_meter(
    ui: &mut egui::Ui,
    fader_spec: &Fader,
    meter: Option<&mut StereoMeter>,
    dragging: &mut Option<f64>,
) -> Option<Edit> {
    let Fader {
        id,
        gain_db,
        maximum_db,
        height,
        show_scale,
    } = *fader_spec;
    let meter_width = if show_scale { 57.0 } else { 29.0 };
    let width = FADER_WIDTH + 4.0 + meter_width;
    let (rect, _) = ui.allocate_exact_size(Vec2::new(width, height), Sense::hover());
    let fader_rect = Rect::from_min_size(rect.min, Vec2::new(FADER_WIDTH, height));
    let meter_rect = Rect::from_min_max(Pos2::new(fader_rect.max.x + 4.0, rect.min.y), rect.max);
    let edit = fader(
        ui,
        id.with("fader"),
        fader_rect,
        gain_db,
        maximum_db,
        dragging,
    );
    if let Some(meter) = meter
        && draw_meter(ui, id.with("clip_lamp"), meter_rect, meter, show_scale)
    {
        meter.clear_clip();
    }
    edit
}

/// フェーダーのレール上の位置（上端 0、下端 1）。
fn fader_position(db: f64) -> f64 {
    let value = db.clamp(SILENT_DB, MAXIMUM_DB);
    for i in 1..FADER_DB.len() {
        if value >= FADER_DB[i] {
            let span = FADER_DB[i - 1] - FADER_DB[i];
            return FADER_POSITION[i - 1]
                + (FADER_DB[i - 1] - value) / span * (FADER_POSITION[i] - FADER_POSITION[i - 1]);
        }
    }
    1.0
}

/// [`fader_position`] の逆。
fn fader_db(position: f64) -> f64 {
    let p = position.clamp(0.0, 1.0);
    for i in 1..FADER_POSITION.len() {
        if p <= FADER_POSITION[i] {
            let span = FADER_POSITION[i] - FADER_POSITION[i - 1];
            return FADER_DB[i - 1]
                + (p - FADER_POSITION[i - 1]) / span * (FADER_DB[i] - FADER_DB[i - 1]);
        }
    }
    SILENT_DB
}

/// 上下ドラッグで音量（Ctrl で微調整）、ダブルクリックで 0 dB。
fn fader(
    ui: &mut egui::Ui,
    id: egui::Id,
    rect: Rect,
    gain_db: f64,
    maximum_db: f64,
    dragging: &mut Option<f64>,
) -> Option<Edit> {
    let response = ui
        .interact(rect, id, Sense::click_and_drag())
        .on_hover_text("上下ドラッグで音量、Ctrl で微調整、ダブルクリックで 0 dB")
        .on_hover_cursor(egui::CursorIcon::ResizeVertical);
    let rail = Rect::from_min_max(
        Pos2::new(rect.min.x + 25.0, rect.min.y + 10.0),
        Pos2::new(rect.min.x + 40.0, rect.max.y - 10.0),
    );
    let mut edit = None;
    if response.double_clicked() {
        *dragging = None;
        edit = Some(Edit {
            value: 0.0,
            commit: true,
        });
    } else if response.drag_started() {
        *dragging = Some(fader_position(gain_db));
    } else if response.dragged()
        && let Some(position) = dragging.as_mut()
    {
        let scale = if ui.input(|input| input.modifiers.ctrl) {
            0.1
        } else {
            1.0
        };
        *position = (*position
            + f64::from(response.drag_delta().y / rail.height().max(1.0)) * scale)
            .clamp(fader_position(maximum_db), 1.0);
        edit = Some(Edit {
            value: fader_db(*position),
            commit: false,
        });
    } else if response.drag_stopped()
        && let Some(position) = dragging.take()
    {
        edit = Some(Edit {
            value: fader_db(position),
            commit: true,
        });
    }

    let shown = edit.as_ref().map_or(gain_db, |edit| edit.value);
    let painter = ui.painter_at(rect.expand(2.0));
    painter.rect(
        Rect::from_min_size(
            Pos2::new(rail.min.x + 6.0, rail.min.y),
            Vec2::new(3.0, rail.height()),
        ),
        0.0,
        Color32::from_rgb(0x0d, 0x10, 0x14),
        Stroke::new(1.0, Color32::from_rgb(0x45, 0x4b, 0x55)),
        egui::StrokeKind::Inside,
    );
    let font = FontId::proportional(8.0);
    for db in FADER_DB {
        let y = rail.min.y + fader_position(db) as f32 * rail.height();
        painter.line_segment(
            [
                Pos2::new(rail.min.x - 6.0, y),
                Pos2::new(rail.min.x - 1.0, y),
            ],
            Stroke::new(1.0, theme::SCALE_TICK),
        );
        let label = if db <= SILENT_DB {
            "−∞".to_owned()
        } else {
            format!("{db}")
        };
        painter.text(
            Pos2::new(rail.min.x - 7.0, y),
            Align2::RIGHT_CENTER,
            label,
            font.clone(),
            theme::SCALE_TEXT,
        );
    }
    let knob_y = rail.min.y + fader_position(shown) as f32 * rail.height();
    let knob = Rect::from_center_size(Pos2::new(rail.center().x, knob_y), Vec2::new(15.0, 25.0));
    let fill = if response.hovered() || response.dragged() {
        Color32::from_rgb(0x74, 0x7d, 0x89)
    } else {
        Color32::from_rgb(0x50, 0x57, 0x61)
    };
    painter.rect(
        knob,
        2.0,
        fill,
        Stroke::new(1.0, Color32::from_rgb(0xab, 0xb3, 0xbd)),
        egui::StrokeKind::Inside,
    );
    painter.rect_filled(
        Rect::from_center_size(knob.center(), Vec2::new(9.0, 2.0)),
        0.0,
        Color32::from_rgb(0xe5, 0xe9, 0xee),
    );
    edit
}

/// 上下・左右ドラッグで定位（Ctrl で微調整）、ダブルクリックで中央。値は -1（左）〜 1（右）。
fn pan_knob(ui: &mut egui::Ui, pan: f64, dragging: &mut Option<f64>) -> Option<Edit> {
    let (rect, response) = ui.allocate_exact_size(Vec2::new(78.0, 62.0), Sense::click_and_drag());
    let response = response
        .on_hover_text("上下・左右ドラッグでパン、Ctrl で微調整、ダブルクリックで中央")
        .on_hover_cursor(egui::CursorIcon::AllScroll);
    let mut edit = None;
    if response.double_clicked() {
        *dragging = None;
        edit = Some(Edit {
            value: 0.0,
            commit: true,
        });
    } else if response.drag_started() {
        *dragging = Some(pan);
    } else if response.dragged()
        && let Some(value) = dragging.as_mut()
    {
        let scale = if ui.input(|input| input.modifiers.ctrl) {
            0.001
        } else {
            0.01
        };
        let delta = response.drag_delta();
        *value = (*value + f64::from(delta.x - delta.y) * scale).clamp(-1.0, 1.0);
        edit = Some(Edit {
            value: *value,
            commit: false,
        });
    } else if response.drag_stopped()
        && let Some(value) = dragging.take()
    {
        edit = Some(Edit {
            value,
            commit: true,
        });
    }
    let shown = edit.as_ref().map_or(pan, |edit| edit.value) as f32;
    let painter = ui.painter_at(rect);
    let center = rect.center() - Vec2::new(0.0, 4.0);
    let fill = if response.hovered() {
        Color32::from_rgb(0x36, 0x3b, 0x43)
    } else {
        Color32::from_rgb(0x29, 0x2d, 0x34)
    };
    painter.circle(center, 20.0, fill, Stroke::new(2.0, theme::SCALE_TEXT));
    // 中央で真上、±1 で ±135°。
    let angle = shown * 135f32.to_radians();
    let direction = Vec2::new(angle.sin(), -angle.cos());
    painter.line_segment(
        [center + direction * 3.0, center + direction * 17.0],
        Stroke::new(2.0, Color32::from_rgb(0xcd, 0xd4, 0xdd)),
    );
    let font = FontId::proportional(10.0);
    painter.text(
        rect.left_bottom() + Vec2::new(2.0, 0.0),
        Align2::LEFT_BOTTOM,
        "L",
        font.clone(),
        theme::MIXER_VALUE,
    );
    painter.text(
        rect.right_bottom() - Vec2::new(2.0, 0.0),
        Align2::RIGHT_BOTTOM,
        "R",
        font,
        theme::MIXER_VALUE,
    );
    edit
}

/// 左右のメーターと目盛り。クリップのランプを押したら true（解除する）。
fn draw_meter(
    ui: &mut egui::Ui,
    lamp_id: egui::Id,
    rect: Rect,
    meter: &StereoMeter,
    show_scale: bool,
) -> bool {
    let painter = ui.painter_at(rect);
    let lamp = Rect::from_min_size(rect.min + Vec2::new(1.0, 0.0), Vec2::new(27.0, 6.0));
    let lamp_response = ui
        .interact(lamp.expand(3.0), lamp_id, Sense::click())
        .on_hover_text("0 dB 超過。クリックでクリップ表示を解除")
        .on_hover_cursor(egui::CursorIcon::PointingHand);
    painter.rect_filled(
        lamp,
        1.0,
        if meter.clipped() {
            Color32::from_rgb(0xff, 0x45, 0x45)
        } else {
            Color32::from_rgb(0x48, 0x29, 0x2d)
        },
    );
    let bars = Rect::from_min_max(
        Pos2::new(rect.min.x, rect.min.y + 10.0),
        Pos2::new(rect.max.x, rect.max.y - 16.0),
    );
    let fraction = |db: f32| ((db - METER_FLOOR_DB) / -METER_FLOOR_DB).clamp(0.0, 1.0);
    for (index, channel) in meter.0.iter().enumerate() {
        let bar = Rect::from_min_size(
            Pos2::new(bars.min.x + index as f32 * 15.0, bars.min.y),
            Vec2::new(12.0, bars.height()),
        );
        painter.rect(
            bar,
            0.0,
            Color32::from_rgb(0x0c, 0x11, 0x15),
            Stroke::new(1.0, Color32::from_rgb(0x45, 0x4b, 0x55)),
            egui::StrokeKind::Inside,
        );
        let inner = bar.shrink(1.0);
        let level = fraction(channel.level_db);
        if level > 0.0 {
            let top = inner.max.y - inner.height() * level;
            let lit = Rect::from_min_max(Pos2::new(inner.min.x, top), inner.max);
            let upper = if channel.level_db > -6.0 {
                Color32::from_rgb(0xe5, 0xc9, 0x47)
            } else {
                Color32::from_rgb(0x5f, 0xd6, 0x5e)
            };
            let mut mesh = egui::Mesh::default();
            mesh.colored_vertex(lit.left_top(), upper);
            mesh.colored_vertex(lit.right_top(), upper);
            mesh.colored_vertex(lit.right_bottom(), Color32::from_rgb(0x36, 0xa8, 0x55));
            mesh.colored_vertex(lit.left_bottom(), Color32::from_rgb(0x36, 0xa8, 0x55));
            mesh.add_triangle(0, 1, 2);
            mesh.add_triangle(0, 2, 3);
            painter.add(egui::Shape::mesh(mesh));
        }
        let held = fraction(channel.peak_db);
        let y = (inner.max.y - inner.height() * held).min(inner.max.y - 2.0);
        painter.rect_filled(
            Rect::from_min_size(Pos2::new(inner.min.x, y), Vec2::new(inner.width(), 2.0)),
            0.0,
            Color32::from_rgb(0xf3, 0xcf, 0x43),
        );
    }
    let small = FontId::proportional(8.0);
    if show_scale {
        for db in METER_TICKS {
            let y = bars.max.y - bars.height() * fraction(db);
            let x = bars.min.x + 29.0;
            painter.line_segment(
                [Pos2::new(x, y), Pos2::new(x + 4.0, y)],
                Stroke::new(1.0, theme::SCALE_TICK),
            );
            painter.text(
                Pos2::new(x + 6.0, y),
                Align2::LEFT_CENTER,
                format!("{db}"),
                small.clone(),
                theme::SCALE_TEXT,
            );
        }
    }
    painter.text(
        Pos2::new(rect.min.x + 14.0, rect.max.y),
        Align2::CENTER_BOTTOM,
        "L   R",
        FontId::proportional(9.0),
        theme::SCALE_TEXT,
    );
    lamp_response.clicked()
}

/// フェーダーの下の行。左が音量の数値（押すと入力）、右が今のメーターの値。
fn gain_row(
    ui: &mut egui::Ui,
    editing: &mut Option<(ValueField, String)>,
    field: ValueField,
    gain_db: f64,
    loudest: f32,
) -> Option<f64> {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        let value = value_field(
            ui,
            editing,
            field,
            gain_db,
            FADER_WIDTH,
            gain_text,
            parse_gain,
        );
        let level = if loudest <= METER_FLOOR_DB {
            "−∞".to_owned()
        } else {
            format!("{loudest:.1}")
        };
        ui.add_sized(
            Vec2::new(ui.available_width().min(57.0), 20.0),
            egui::Label::new(
                egui::RichText::new(level)
                    .size(10.0)
                    .color(theme::SCALE_TEXT),
            ),
        );
        value
    })
    .inner
}

/// 数値の欄。押すと入力になり、Enter・欄の外で確定、Esc で取り消す。範囲外なら入力を続けさせる。
fn value_field(
    ui: &mut egui::Ui,
    editing: &mut Option<(ValueField, String)>,
    field: ValueField,
    value: f64,
    width: f32,
    format: fn(f64) -> String,
    parse: fn(&str) -> Option<f64>,
) -> Option<f64> {
    let size = Vec2::new(width, 20.0);
    if let Some((_, text)) = editing.as_mut().filter(|(active, _)| *active == field) {
        let response = ui.add_sized(
            size,
            egui::TextEdit::singleline(text)
                .horizontal_align(egui::Align::Center)
                .font(FontId::proportional(11.0)),
        );
        if !response.has_focus() && !response.lost_focus() {
            response.request_focus();
        }
        let parsed = response.lost_focus().then(|| parse(text));
        if ui.input(|input| input.key_pressed(egui::Key::Escape)) {
            *editing = None;
            return None;
        }
        return match parsed {
            Some(Some(value)) => {
                *editing = None;
                Some(value)
            }
            Some(None) => {
                // 範囲外・数でない入力は確定させず、入力を続けさせる。
                response.request_focus();
                None
            }
            None => None,
        };
    }
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    let text = format(value);
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, &text));
    let color = if response.hovered() {
        theme::MIXER_VALUE_HOVER
    } else {
        theme::MIXER_VALUE
    };
    ui.painter().text(
        rect.center(),
        Align2::CENTER_CENTER,
        &text,
        FontId::proportional(11.0),
        color,
    );
    let response = response.on_hover_cursor(egui::CursorIcon::Text);
    if response.clicked() {
        let initial =
            if value <= SILENT_DB && matches!(field, ValueField::Gain(_) | ValueField::Master) {
                "-inf".to_owned()
            } else {
                format!("{value:.1}")
            };
        *editing = Some((field, initial));
    }
    None
}

fn gain_text(value: f64) -> String {
    if value <= SILENT_DB {
        "−∞".to_owned()
    } else {
        format!("{value:.1}")
    }
}

fn parse_gain(text: &str) -> Option<f64> {
    let trimmed = text.trim().trim_end_matches("dB").trim();
    if matches!(trimmed, "-inf" | "-∞" | "−∞") {
        return Some(SILENT_DB);
    }
    trimmed
        .parse::<f64>()
        .ok()
        .filter(|value| value.is_finite() && (SILENT_DB..=MAXIMUM_DB).contains(value))
}

fn pan_text(value: f64) -> String {
    format!("{:.1}", value)
}

fn parse_pan(text: &str) -> Option<f64> {
    text.trim()
        .parse::<f64>()
        .ok()
        .filter(|value| value.is_finite() && (-100.0..=100.0).contains(value))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gain_text_and_parser_agree_on_silence_and_range() {
        assert_eq!(gain_text(-96.0), "−∞");
        assert_eq!(gain_text(-6.0), "-6.0");
        assert_eq!(parse_gain("-inf"), Some(SILENT_DB));
        assert_eq!(parse_gain("-∞"), Some(SILENT_DB));
        assert_eq!(parse_gain("−∞"), Some(SILENT_DB));
        assert_eq!(parse_gain(" 3.5 dB"), Some(3.5));
        assert_eq!(parse_gain("16"), None);
        assert_eq!(parse_gain("abc"), None);
        assert_eq!(parse_pan("-100"), Some(-100.0));
        assert_eq!(parse_pan("100.5"), None);
    }

    #[test]
    fn the_fader_scale_spreads_the_range_around_zero_db() {
        // 目盛りの点はそのまま、間は直線で補う。期待値は C++ 版の AudioFader の表から写した。
        assert_eq!(fader_position(15.0), 0.0);
        assert_eq!(fader_position(0.0), 0.25);
        assert_eq!(fader_position(-12.0), 0.5);
        assert_eq!(fader_position(-96.0), 1.0);
        assert!((fader_position(-3.0) - 0.315).abs() < 1e-9);
        assert!((fader_position(-36.0) - 0.735).abs() < 1e-9);
        // 範囲の外は端へ寄せる。
        assert_eq!(fader_position(40.0), 0.0);
        assert_eq!(fader_position(-200.0), 1.0);
        for db in [12.0, 6.0, 1.5, 0.0, -3.0, -20.0, -60.0, -96.0] {
            assert!(
                (fader_db(fader_position(db)) - db).abs() < 1e-9,
                "{db} dB が往復しません"
            );
        }
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
        let mut meter = MeterDisplay::default();
        meter.feed(1.0, 0.0, 0.0);
        assert!((meter.level_db - 0.0).abs() < 1e-6);
        assert!(!meter.clipped);
        meter.feed(0.0, 0.5, 0.5);
        assert!((meter.level_db + 18.0).abs() < 1e-4, "{}", meter.level_db);
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

    #[test]
    fn the_stereo_meter_keeps_left_and_right_apart() {
        let mut meter = StereoMeter::default();
        meter.feed([1.0, 0.0], 0.0, 0.0);
        assert!((meter.0[0].level_db - 0.0).abs() < 1e-6);
        assert_eq!(meter.0[1].level_db, METER_FLOOR_DB);
        assert!(!meter.clipped());
        meter.feed([0.0, 2.0], 0.1, 0.1);
        assert!(meter.clipped());
        meter.clear_clip();
        assert!(!meter.clipped());
    }
}
