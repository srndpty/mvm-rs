//! timeline の表示。区間の外にある clip は widget を作らない。
//!
//! ドラッグ中は Project を変えず、離したときに command を 1 回だけ実行する。

use eframe::egui::{self, Align2, Color32, Label, Pos2, Rect, Sense, Stroke, Vec2};
use mvm_app::{
    Session, TimeWindow, display_span_frames, ranges_overlap, ruler_step, visible_window,
};
use mvm_core::timecode::format_timecode;
use mvm_project::timeline::{
    BodyAction, BodyPress, ClipSpan, DragBounds, EdgeAction, GestureModifiers, PenGeometry, PenKey,
    PenPress, TOOLS, TimelineTool, TrimEdge, body_press, body_release, clip_ids_spanning_frame,
    clip_interval, drag_snap_frames, edge_release, group_drag_offset_x, group_drag_track_index,
    marker_near_ruler_x, pen_press, pen_release, pen_snap_value, pen_value_at, pen_visible_keys,
    pen_visible_line_points, snap_drag_offset_x,
};
use mvm_project::{Project, TrackKind, TrackRef};

use crate::project_panel::MediaDrag;
use crate::subtitle_lane::{self, SubtitleGesture};

macro_rules! report {
    ($session:ident, $call:expr $(,)?) => {{
        let result = $call;
        if let Err(error) = result {
            $session.set_alert(error.message());
        }
    }};
}

const HEADER_WIDTH: f32 = 210.0;
const RULER_HEIGHT: f32 = 28.0;
pub(crate) const ROW_HEIGHT: f32 = 40.0;
pub(crate) const EDGE_PIXELS: f32 = 8.0;

/// timeline の UI 状態。ドラッグ中の操作と、shell へ渡す依頼。
#[derive(Default)]
pub struct TimelineState {
    pub gesture: Option<TimelineGesture>,
    pub subtitle_gesture: Option<SubtitleGesture>,
    /// 右クリックの「自動字幕を生成」で選んだ clip。shell が文字起こしの dialog を開く。
    pub transcribe_clip: Option<String>,
    /// 右クリックの「速度・デュレーション」で選んだ clip。shell が dialog を開く。
    pub speed_clip: Option<String>,
    /// 右クリックの「プロジェクト設定をこの素材に合わせる」で選んだ clip。
    pub match_settings_clip: Option<String>,
    /// 空白・ルーラーを右クリックした位置（track の行の上なら track も）。
    background_menu: Option<(Option<TrackRef>, i64)>,
}

pub struct TimelineGesture {
    clip_id: String,
    press_frame: i64,
    origin: Pos2,
    pointer: Pos2,
    moved: bool,
    linked: bool,
    body: Option<BodyPress>,
    edge: Option<TrimEdge>,
    pen: Option<PenPress>,
}

pub fn show(ui: &mut egui::Ui, session: &mut Session, state: &mut TimelineState) {
    ui.horizontal_wrapped(|ui| {
        ui.label("タイムライン");
        if ui.button("縮小").clicked() {
            session.zoom_by(0.5);
        }
        ui.label(format!("{}%", session.zoom_percent()));
        if ui.button("拡大").clicked() {
            session.zoom_by(2.0);
        }
        ui.monospace(session.playhead_label());
        let rate = session.shuttle_rate();
        if rate != 0 && rate != 1 {
            ui.label(format!("シャトル {rate} 倍速（音声なし）"));
        }
        ui.label(format!("ツール {}", session.tool().spec().name));
        if ui
            .button("+V")
            .on_hover_text("video トラックを追加")
            .clicked()
        {
            report!(session, session.add_track(TrackKind::Video));
        }
        if ui
            .button("+A")
            .on_hover_text("audio トラックを追加")
            .clicked()
        {
            report!(session, session.add_track(TrackKind::Audio));
        }
    });
    ui.horizontal_wrapped(|ui| {
        for spec in TOOLS {
            let button = egui::Button::new(spec.name).selected(session.tool() == spec.tool);
            let response = ui
                .add(button)
                .on_hover_text(format!("{}  {}", spec.key, spec.hint));
            if response.clicked() {
                session.set_tool(spec.tool);
            }
        }
    });

    let rows = track_rows(session.project());
    let ppf = session.pixels_per_frame();
    let span =
        display_span_frames(session.project()).max(session.playhead_frame().saturating_add(1));
    let content_width = (span as f32).mul_add(ppf, 48.0).max(1.0);

    ui.horizontal(|ui| {
        ui.vertical(|ui| {
            ui.add_space(RULER_HEIGHT);
            for (kind, index, name) in &rows {
                let track = TrackRef::new(*kind, *index);
                ui.allocate_ui_with_layout(
                    Vec2::new(HEADER_WIDTH, ROW_HEIGHT),
                    egui::Layout::left_to_right(egui::Align::Center),
                    |ui| {
                        let label = ui.add(Label::new(name).sense(Sense::click()));
                        label.context_menu(|ui| {
                            if ui.button("このトラックを削除").clicked() {
                                report!(session, session.remove_track(track));
                                ui.close();
                            }
                        });
                        track_buttons(ui, session, track, name);
                    },
                );
            }
            if let Some(visible) = session
                .project()
                .subtitles
                .as_ref()
                .map(|track| track.visible)
            {
                ui.allocate_ui_with_layout(
                    Vec2::new(HEADER_WIDTH, ROW_HEIGHT),
                    egui::Layout::left_to_right(egui::Align::Center),
                    |ui| {
                        ui.label("S1");
                        let label = if visible { "S1 表示" } else { "S1 非表示" };
                        if ui.button(label).clicked() {
                            report!(session, session.toggle_subtitle_visible());
                        }
                    },
                );
            }
        });

        egui::ScrollArea::horizontal()
            .id_salt("timeline_scroll")
            .show_viewport(ui, |ui, viewport| {
                let window = visible_window(viewport.min.x, viewport.width(), ppf);
                draw_contents(ui, session, state, &rows, content_width, window, ppf);
            });
    });
}

fn track_buttons(ui: &mut egui::Ui, session: &mut Session, track: TrackRef, name: &str) {
    let Some(state) = session.project().tracks(track.kind).get(track.index) else {
        return;
    };
    let muted = state.muted;
    let solo = state.solo;
    match track.kind {
        TrackKind::Video => {
            let label = if muted {
                format!("{name} 非表示")
            } else {
                format!("{name} 表示")
            };
            if ui.button(label).clicked() {
                report!(session, session.toggle_track_muted(track));
            }
        }
        TrackKind::Audio => {
            let mute_label = if muted {
                format!("{name} 消音中")
            } else {
                format!("{name} 消音")
            };
            let solo_label = if solo {
                format!("{name} ソロ中")
            } else {
                format!("{name} ソロ")
            };
            if ui.button(mute_label).clicked() {
                report!(session, session.toggle_track_muted(track));
            }
            if ui.button(solo_label).clicked() {
                report!(session, session.toggle_track_solo(track));
            }
        }
    }
}

fn draw_contents(
    ui: &mut egui::Ui,
    session: &mut Session,
    state: &mut TimelineState,
    rows: &[(TrackKind, usize, String)],
    content_width: f32,
    window: TimeWindow,
    ppf: f32,
) {
    let lanes = rows.len() + usize::from(session.project().subtitles.is_some());
    let height = RULER_HEIGHT + lanes as f32 * ROW_HEIGHT;
    let (rect, response) = ui.allocate_exact_size(
        Vec2::new(content_width, height.max(1.0)),
        Sense::click_and_drag(),
    );
    background(ui, session, &response, rect, ppf);
    if response.secondary_clicked()
        && let Some(pointer) = response.interact_pointer_pos()
    {
        let frame = frame_at(pointer.x, rect.min.x, ppf);
        let track = drop_target(pointer, rect, rows, ppf).map(|(track, _)| track);
        state.background_menu = Some((track, frame));
    }
    response.context_menu(|ui| background_menu(ui, session, state));

    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 0.0, Color32::from_rgb(22, 24, 28));
    draw_ruler(ui, session, rect, window, ppf);

    let mut interval_failed = false;
    let mut visible = Vec::new();
    for (row_index, (kind, track_index, _)) in rows.iter().enumerate() {
        let y = rect.min.y + RULER_HEIGHT + row_index as f32 * ROW_HEIGHT;
        let row_rect = Rect::from_min_size(
            Pos2::new(rect.min.x, y),
            Vec2::new(rect.width(), ROW_HEIGHT),
        );
        if row_index % 2 == 0 {
            painter.rect_filled(row_rect, 0.0, Color32::from_rgb(30, 32, 36));
        }
        for clip in &session.project().timeline_clips {
            if clip.track.kind != *kind || clip.track.index != *track_index {
                continue;
            }
            match clip_interval(session.project(), clip) {
                Ok((start, end)) if ranges_overlap(start, end, window) => {
                    visible.push(VisibleClip {
                        id: clip.id.clone(),
                        name: clip.name.clone(),
                        kind: clip.track.kind,
                        track_index: clip.track.index,
                        y,
                        start,
                        end,
                    });
                }
                Ok(_) => {}
                Err(_) => interval_failed = true,
            }
        }
    }
    for clip in visible {
        draw_clip(ui, session, state, &clip, rect.min.x, ppf, rows);
    }
    if session.project().subtitles.is_some() {
        let y = rect.min.y + RULER_HEIGHT + rows.len() as f32 * ROW_HEIGHT;
        let lane = Rect::from_min_size(
            Pos2::new(rect.min.x, y),
            Vec2::new(rect.width(), ROW_HEIGHT),
        );
        subtitle_lane::draw(ui, session, &mut state.subtitle_gesture, lane, window, ppf);
    }
    draw_playhead(ui, rect, session.playhead_frame(), ppf);
    receive_media(ui, session, rect, rows, ppf);
    if interval_failed {
        ui.label("区間を計算できない clip があります");
    }
}

/// clip の右クリック。選択中の clip に対して働く（右クリックした clip が選択に無ければ、それだけを選ぶ）。
fn clip_menu(ui: &mut egui::Ui, session: &mut Session, state: &mut TimelineState, id: &str) {
    let Some(clip) = session.project().clip(id).cloned() else {
        return;
    };
    if ui.button("コピー").clicked() {
        report!(session, session.copy_selection());
        ui.close();
    }
    if ui.button("カット").clicked() {
        report!(session, session.cut_selection());
        ui.close();
    }
    if ui.button("複製").clicked() {
        report!(session, session.duplicate_selection());
        ui.close();
    }
    ui.separator();
    if !clip.has_synthetic_source_domain() && ui.button("速度・デュレーション…").clicked()
    {
        state.speed_clip = Some(clip.id.clone());
        ui.close();
    }
    if ui
        .add_enabled(
            session.can_insert_frame_hold(&clip.id),
            egui::Button::new("フレーム保持を挿入"),
        )
        .clicked()
    {
        report!(session, session.insert_frame_hold(&clip.id));
        ui.close();
    }
    if matches!(
        clip.kind,
        mvm_project::TimelineClipKind::Audio
            | mvm_project::TimelineClipKind::Video
            | mvm_project::TimelineClipKind::Manim
    ) && ui.button("自動字幕を生成").clicked()
    {
        state.transcribe_clip = Some(clip.id.clone());
        ui.close();
    }
    if matches!(
        clip.kind,
        mvm_project::TimelineClipKind::Video | mvm_project::TimelineClipKind::Manim
    ) && ui.button("プロジェクト設定をこの素材に合わせる").clicked()
    {
        state.match_settings_clip = Some(clip.id.clone());
        ui.close();
    }
    if clip.is_linked() && ui.button("リンクを解除").clicked() {
        report!(session, session.unlink_clip(&clip.id));
        ui.close();
    }
    ui.separator();
    if ui.button("削除").clicked() {
        report!(session, session.delete_selection());
        ui.close();
    }
    if ui.button("リップル削除（全トラック）").clicked() {
        report!(session, session.ripple_delete_selection());
        ui.close();
    }
}

/// 空白・ルーラーの右クリック。空白は詰められ、マーカーとイン・アウトを消せる。
fn background_menu(ui: &mut egui::Ui, session: &mut Session, state: &mut TimelineState) {
    let Some((track, frame)) = state.background_menu else {
        ui.close();
        return;
    };
    let mut shown = false;
    if let Some(track) = track {
        shown = true;
        if ui.button("リップル削除（空白を詰める）").clicked() {
            report!(session, session.ripple_delete_gap(track, frame));
            ui.close();
        }
    }
    if session.has_clipboard() {
        shown = true;
        if ui.button("ペースト").clicked() {
            session.seek(frame);
            report!(session, session.paste());
            ui.close();
        }
    }
    if let Some(marker) = session.selected_marker() {
        shown = true;
        if ui.button("マーカーを削除").clicked() {
            report!(session, session.delete_marker(marker));
            ui.close();
        }
    }
    let (has_in, has_out) = {
        let project = session.project();
        (project.in_frame.is_some(), project.out_frame.is_some())
    };
    if has_in || has_out {
        shown = true;
        if has_in && ui.button("インを消去").clicked() {
            report!(session, session.clear_mark_in());
            ui.close();
        }
        if has_out && ui.button("アウトを消去").clicked() {
            report!(session, session.clear_mark_out());
            ui.close();
        }
        if ui.button("イン・アウトを消去").clicked() {
            report!(session, session.clear_marks());
            ui.close();
        }
    }
    if !shown {
        ui.label("操作できる項目がありません");
    }
}

/// track の行の上の位置を、置き先の track と frame にする。ruler・字幕の行・外側は `None`。
fn drop_target(
    pointer: Pos2,
    rect: Rect,
    rows: &[(TrackKind, usize, String)],
    ppf: f32,
) -> Option<(TrackRef, i64)> {
    if !rect.contains(pointer) {
        return None;
    }
    let local = pointer.y - rect.min.y - RULER_HEIGHT;
    if local < 0.0 {
        return None;
    }
    let row = (local / ROW_HEIGHT) as usize;
    let (kind, index, _) = rows.get(row)?;
    Some((
        TrackRef::new(*kind, *index),
        frame_at(pointer.x, rect.min.x, ppf),
    ))
}

/// プロジェクトパネルからのドラッグを受ける。ドラッグ中は置く位置を枠で見せる。
/// OS から落とされたファイルは受けない（ドラッグ中の pointer の位置が届かず、置く行を確かめられないため）。
fn receive_media(
    ui: &mut egui::Ui,
    session: &mut Session,
    rect: Rect,
    rows: &[(TrackKind, usize, String)],
    ppf: f32,
) {
    if !egui::DragAndDrop::has_payload_of_type::<MediaDrag>(ui.ctx()) {
        return;
    }
    let pointer = ui.input(|input| input.pointer.latest_pos());
    let target = pointer.and_then(|pointer| drop_target(pointer, rect, rows, ppf));
    let Some((track, frame)) = target else {
        return;
    };
    let row = rows
        .iter()
        .position(|(kind, index, _)| *kind == track.kind && *index == track.index)
        .unwrap_or_default();
    let x = rect.min.x + frame as f32 * ppf;
    let y = rect.min.y + RULER_HEIGHT + row as f32 * ROW_HEIGHT;
    ui.painter().rect_stroke(
        Rect::from_min_size(Pos2::new(x, y + 2.0), Vec2::new(48.0, ROW_HEIGHT - 4.0)),
        3.0,
        Stroke::new(2.0, Color32::from_rgb(98, 182, 255)),
        egui::StrokeKind::Inside,
    );
    if ui.input(|input| input.pointer.any_released())
        && let Some(drag) = egui::DragAndDrop::take_payload::<MediaDrag>(ui.ctx())
    {
        report!(session, session.place_media_items_at(&drag.0, track, frame));
    }
}

fn background(
    ui: &mut egui::Ui,
    session: &mut Session,
    response: &egui::Response,
    rect: Rect,
    ppf: f32,
) {
    let tool = session.tool();
    if tool == TimelineTool::Hand && response.dragged() {
        ui.scroll_with_delta(Vec2::new(-response.drag_delta().x, 0.0));
        return;
    }
    if tool == TimelineTool::Zoom && response.clicked() {
        let alt = ui.input(|input| input.modifiers.alt);
        session.zoom_by(if alt { 0.5 } else { 2.0 });
        return;
    }
    if matches!(
        tool,
        TimelineTool::Text | TimelineTool::Pen | TimelineTool::Hand
    ) {
        return;
    }
    let Some(pointer) = response.interact_pointer_pos() else {
        if response.drag_stopped() {
            session.set_scrubbing(false);
        }
        return;
    };
    if !(response.dragged() || response.clicked()) {
        if response.drag_stopped() {
            session.set_scrubbing(false);
        }
        return;
    }
    let frame = frame_at(pointer.x, rect.min.x, ppf);
    if pointer.y - rect.min.y < RULER_HEIGHT {
        let markers = session.project().timeline_markers.clone();
        let local = f64::from(pointer.x - rect.min.x);
        if let Some(marker) = marker_near_ruler_x(local, f64::from(ppf), &markers, 8.0) {
            session.select_marker(Some(marker));
            session.set_scrubbing(false);
            return;
        }
    }
    if tool == TimelineTool::Razor && response.clicked() && ui.input(|input| input.modifiers.shift)
    {
        let ids = clip_ids_spanning_frame(session.project(), frame);
        let linked = !ui.input(|input| input.modifiers.alt);
        report!(session, session.split_at(&ids, frame, linked));
        return;
    }
    session.set_scrubbing(response.dragged());
    session.seek(frame);
    if response.drag_stopped() {
        session.set_scrubbing(false);
    }
}

fn draw_ruler(ui: &mut egui::Ui, session: &Session, rect: Rect, window: TimeWindow, ppf: f32) {
    let project = session.project();
    let painter = ui.painter_at(rect);
    let ruler = Rect::from_min_size(rect.min, Vec2::new(rect.width(), RULER_HEIGHT));
    painter.rect_filled(ruler, 0.0, Color32::from_rgb(28, 30, 34));
    for frame in &project.timeline_markers {
        let x = rect.min.x + *frame as f32 * ppf;
        let selected = session.selected_marker() == Some(*frame);
        painter.circle_filled(
            Pos2::new(x, ruler.center().y),
            4.0,
            if selected {
                Color32::from_rgb(240, 200, 80)
            } else {
                Color32::from_rgb(180, 150, 60)
            },
        );
    }
    let fps = project.timeline_fps;
    let nominal = if fps.den > 0 { fps.num / fps.den } else { 1 };
    let step = ruler_step(ppf, nominal.max(1));
    if step <= 0 {
        return;
    }
    let mut tick = window.first.saturating_div(step).saturating_mul(step);
    if tick < window.first {
        tick = tick.saturating_add(step);
    }
    let font = egui::TextStyle::Small.resolve(ui.style());
    while tick < window.last {
        let x = rect.min.x + tick as f32 * ppf;
        painter.line_segment(
            [Pos2::new(x, ruler.max.y - 8.0), Pos2::new(x, ruler.max.y)],
            Stroke::new(1.0, Color32::from_rgb(140, 144, 150)),
        );
        let label = format_timecode(tick, fps.num, fps.den).unwrap_or_else(|| tick.to_string());
        painter.text(
            Pos2::new(x + 2.0, ruler.min.y + 2.0),
            Align2::LEFT_TOP,
            label,
            font.clone(),
            Color32::from_rgb(180, 184, 190),
        );
        let next = tick.saturating_add(step);
        if next <= tick {
            break;
        }
        tick = next;
    }
}

struct VisibleClip {
    id: String,
    name: String,
    kind: TrackKind,
    track_index: usize,
    y: f32,
    start: i64,
    end: i64,
}

fn draw_clip(
    ui: &mut egui::Ui,
    session: &mut Session,
    state: &mut TimelineState,
    clip: &VisibleClip,
    origin_x: f32,
    ppf: f32,
    rows: &[(TrackKind, usize, String)],
) {
    let x = origin_x + clip.start as f32 * ppf;
    let width = ((clip.end - clip.start) as f32 * ppf).max(2.0);
    let rect = Rect::from_min_size(
        Pos2::new(x, clip.y + 4.0),
        Vec2::new(width, ROW_HEIGHT - 8.0),
    );
    let selected = session.selection().iter().any(|id| id == &clip.id);
    ui.painter()
        .rect_filled(rect, 3.0, clip_color(clip.kind, selected));
    if clip.kind == TrackKind::Audio {
        draw_waveform(ui, session, &clip.id, rect, ppf);
    }
    let response = ui.put(
        rect,
        Label::new(clip.name.as_str())
            .truncate()
            .sense(Sense::click_and_drag()),
    );
    if response.secondary_clicked() && !selected {
        session.select_only(clip.id.clone());
    }
    response.context_menu(|ui| clip_menu(ui, session, state, &clip.id));
    let gesture = &mut state.gesture;
    draw_transitions(ui, session, clip, origin_x, ppf);
    draw_keys(ui, session, clip, rect, ppf);
    if let Some((id, edge)) = session.selected_edge()
        && id == clip.id
    {
        let x = match edge {
            TrimEdge::Left => rect.min.x,
            TrimEdge::Right => rect.max.x,
        };
        ui.painter().line_segment(
            [Pos2::new(x, rect.min.y), Pos2::new(x, rect.max.y)],
            Stroke::new(3.0, Color32::from_rgb(240, 200, 80)),
        );
    }
    if let Some(active) = gesture
        .as_ref()
        .filter(|active| active.moved && active.clip_id == clip.id)
    {
        draw_ghost(ui, active, rect);
    }
    // 押した瞬間に始める。drag_started はクリックの閾値を超えるまで来ないので、
    // それを待つとクリック（選択・レーザー）が gesture を持たず、ドラッグの起点もずれる。
    if response.is_pointer_button_down_on()
        && ui.input(|input| input.pointer.primary_pressed())
        && let Some(pointer) = ui.input(|input| input.pointer.press_origin())
    {
        *gesture = begin_gesture(ui, session, clip, pointer, origin_x, ppf, rect);
    }
    if response.dragged()
        && let Some(active) = gesture.as_mut().filter(|active| active.clip_id == clip.id)
        && let Some(pointer) = response.interact_pointer_pos()
    {
        active.pointer = pointer;
        if active.origin.distance(pointer) > 4.0 {
            active.moved = true;
        }
    }
    // 離した frame は全 clip で真になるので、自分の gesture だけを取り出す。
    if ui.input(|input| input.pointer.primary_released())
        && let Some(active) = gesture.take_if(|active| active.clip_id == clip.id)
    {
        commit_gesture(ui, session, active, origin_x, ppf, rows, rect);
    }
}

/// 音声 clip の波形。見えている列だけを 2 pixel ごとに描く。素材の時刻は clip の素材 in と速度から求める。
fn draw_waveform(ui: &egui::Ui, session: &mut Session, clip_id: &str, rect: Rect, ppf: f32) {
    let Some(clip) = session.project().clip(clip_id) else {
        return;
    };
    let timeline_fps = session.project().timeline_fps;
    let (source_fps, speed) = (clip.source_fps, clip.speed);
    if !timeline_fps.is_positive() || !source_fps.is_positive() || !speed.is_positive() {
        return;
    }
    let source_in = clip.source_in_frame as f64 * source_fps.den as f64 / source_fps.num as f64;
    let speed = speed.num as f64 / speed.den as f64;
    let seconds_per_frame = timeline_fps.den as f64 / timeline_fps.num as f64;
    let path = clip.media_path.clone();
    let Some(peaks) = session.waveform(&path) else {
        return;
    };
    let visible = rect.intersect(ui.clip_rect());
    if visible.width() <= 0.0 {
        return;
    }
    let painter = ui.painter_at(rect);
    let color = Color32::from_rgba_unmultiplied(170, 220, 190, 150);
    let middle = rect.center().y;
    let half = rect.height() / 2.0 - 2.0;
    let mut x = visible.min.x.floor();
    while x < visible.max.x {
        let local = |px: f32| f64::from((px - rect.min.x) / ppf) * seconds_per_frame;
        let begin = source_in + local(x) * speed;
        let end = source_in + local(x + 2.0) * speed;
        let mut column: Option<(f32, f32)> = None;
        for channel in 0..peaks.channels {
            if let Some(found) =
                mvm_core::waveform_peaks::waveform_column(&peaks, channel, begin, end)
            {
                let (low, high) = column.unwrap_or((found.minimum, found.maximum));
                column = Some((low.min(found.minimum), high.max(found.maximum)));
            }
        }
        if let Some((low, high)) = column {
            painter.line_segment(
                [
                    Pos2::new(x, middle - high * half),
                    Pos2::new(x, middle - low * half),
                ],
                Stroke::new(1.5, color),
            );
        }
        x += 2.0;
    }
}

/// ドラッグ中の行き先。Project は変えず、離したときの command と同じ形を枠で見せる。
fn draw_ghost(ui: &egui::Ui, active: &TimelineGesture, rect: Rect) {
    let delta = active.pointer - active.origin;
    let ghost = if active.pen.is_some() {
        ui.painter()
            .circle_filled(active.pointer, 4.0, Color32::from_rgb(240, 200, 80));
        return;
    } else if let Some(edge) = active.edge {
        match edge {
            TrimEdge::Left => {
                Rect::from_min_max(Pos2::new(rect.min.x + delta.x, rect.min.y), rect.max)
            }
            TrimEdge::Right => {
                Rect::from_min_max(rect.min, Pos2::new(rect.max.x + delta.x, rect.max.y))
            }
        }
    } else {
        rect.translate(delta)
    };
    ui.painter().rect_stroke(
        ghost,
        3.0,
        Stroke::new(1.5, Color32::from_rgba_unmultiplied(230, 230, 230, 200)),
        egui::StrokeKind::Inside,
    );
}

/// 出ていく clip の行に、切れ目の前後の区間を重ねる。
fn draw_transitions(ui: &egui::Ui, session: &Session, clip: &VisibleClip, origin_x: f32, ppf: f32) {
    for transition in &session.project().timeline_transitions {
        if transition.outgoing_clip_id != clip.id {
            continue;
        }
        let first = clip.end.saturating_sub(transition.frames_before_cut);
        let last = clip.end.saturating_add(transition.frames_after_cut);
        let band = Rect::from_min_max(
            Pos2::new(origin_x + first as f32 * ppf, clip.y + 4.0),
            Pos2::new(origin_x + last as f32 * ppf, clip.y + ROW_HEIGHT - 4.0),
        );
        ui.painter().rect_filled(
            band,
            3.0,
            Color32::from_rgba_unmultiplied(200, 170, 230, 70),
        );
        ui.painter().line_segment(
            [band.left_top(), band.right_bottom()],
            Stroke::new(1.0, Color32::from_rgb(200, 170, 230)),
        );
    }
}

/// 不透明度（音声は音量）の線とキー。画面に入っている区間だけを描く。
fn draw_keys(ui: &egui::Ui, session: &Session, clip: &VisibleClip, rect: Rect, ppf: f32) {
    let Some(item) = session.project().clip(&clip.id) else {
        return;
    };
    let kind = key_kind(item);
    let stored = item.effects.keys(kind);
    if stored.is_empty() && session.tool() != TimelineTool::Pen {
        return;
    }
    let keys: Vec<PenKey> = stored
        .iter()
        .map(|key| PenKey {
            frame: key.frame,
            value: key.value,
            samples: Vec::new(),
        })
        .collect();
    let geometry = pen_geometry(&rect, ppf, kind);
    let visible = ui.clip_rect();
    let left = f64::from(visible.min.x - rect.min.x);
    let right = f64::from(visible.max.x - rect.min.x);
    let to_screen = |x: f64, y: f64| Pos2::new(rect.min.x + x as f32, rect.min.y + y as f32);
    let line: Vec<Pos2> =
        pen_visible_line_points(&keys, item.effects.base(kind), &geometry, left, right)
            .iter()
            .map(|point| to_screen(point.x, point.y))
            .collect();
    let color = Color32::from_rgb(240, 200, 80);
    ui.painter()
        .add(egui::Shape::line(line, Stroke::new(1.0, color)));
    for key in pen_visible_keys(&keys, &geometry, left, right) {
        let x = key.frame as f64 * geometry.pixels_per_frame;
        let y = mvm_project::timeline::pen_y(&geometry, key.value);
        ui.painter().circle_filled(to_screen(x, y), 3.0, color);
    }
}

fn key_kind(clip: &mvm_project::TimelineClip) -> mvm_project::clip_effects::ClipKeyKind {
    if clip.kind == mvm_project::TimelineClipKind::Audio {
        mvm_project::clip_effects::ClipKeyKind::Volume
    } else {
        mvm_project::clip_effects::ClipKeyKind::Opacity
    }
}

fn begin_gesture(
    ui: &egui::Ui,
    session: &mut Session,
    clip: &VisibleClip,
    pointer: Pos2,
    origin_x: f32,
    ppf: f32,
    rect: Rect,
) -> Option<TimelineGesture> {
    let tool = session.tool();
    let mods = gesture_modifiers(ui);
    let frame = frame_at(pointer.x, origin_x, ppf);
    if matches!(
        tool,
        TimelineTool::TrackForward | TimelineTool::TrackBackward
    ) {
        let track = mods
            .shift
            .then_some(TrackRef::new(clip.kind, clip.track_index));
        session.select_track_from_playhead(frame, tool == TimelineTool::TrackForward, track);
        return None;
    }
    if matches!(
        tool,
        TimelineTool::Hand | TimelineTool::Zoom | TimelineTool::Text
    ) {
        return None;
    }
    if tool == TimelineTool::Pen {
        return begin_pen(session, clip, pointer, rect, ppf, mods);
    }
    let edge = edge_tool(tool).then(|| edge_at(pointer.x, rect)).flatten();
    let body = edge.is_none().then(|| body_press(tool, mods, frame));
    Some(TimelineGesture {
        clip_id: clip.id.clone(),
        press_frame: frame,
        origin: pointer,
        pointer,
        moved: false,
        linked: !mods.alt,
        body,
        edge,
        pen: None,
    })
}

fn begin_pen(
    session: &mut Session,
    clip: &VisibleClip,
    pointer: Pos2,
    rect: Rect,
    ppf: f32,
    mods: GestureModifiers,
) -> Option<TimelineGesture> {
    let item = session.project().clip(&clip.id).cloned()?;
    let kind = key_kind(&item);
    let geometry = pen_geometry(&rect, ppf, kind);
    let keys: Vec<PenKey> = item
        .effects
        .keys(kind)
        .iter()
        .map(|key| PenKey {
            frame: key.frame,
            value: key.value,
            samples: Vec::new(),
        })
        .collect();
    let local_x = f64::from(pointer.x - rect.min.x);
    let local_y = f64::from(pointer.y - rect.min.y);
    let frame = (local_x / f64::from(ppf.max(0.25))).floor() as i64;
    let press = pen_press(
        &keys,
        item.effects.base(kind),
        frame.max(0),
        local_x,
        local_y,
        &geometry,
        mods,
    );
    match press {
        PenPress::DeleteKey { frame } => {
            report!(session, session.delete_key(&clip.id, kind, frame));
            None
        }
        PenPress::Select => {
            session.select_only(clip.id.clone());
            None
        }
        PenPress::None => None,
        PenPress::Edit { .. } => Some(TimelineGesture {
            clip_id: clip.id.clone(),
            press_frame: frame.max(0),
            origin: pointer,
            pointer,
            moved: false,
            linked: !mods.alt,
            body: None,
            edge: None,
            pen: Some(press),
        }),
    }
}

fn commit_gesture(
    ui: &egui::Ui,
    session: &mut Session,
    gesture: TimelineGesture,
    origin_x: f32,
    ppf: f32,
    rows: &[(TrackKind, usize, String)],
    rect: Rect,
) {
    let Some(clip) = session.project().clip(&gesture.clip_id).cloned() else {
        return;
    };
    let mods = gesture_modifiers(ui);
    if let Some(pen) = &gesture.pen {
        commit_pen(session, &clip, pen, &gesture, rect, ppf, mods);
        return;
    }
    if let Some(edge) = gesture.edge {
        let release = frame_at(gesture.pointer.x, origin_x, ppf);
        let delta = release.saturating_sub(gesture.press_frame);
        match edge_release(session.tool(), edge, delta, gesture.linked) {
            EdgeAction::SelectEdit { edge } => session.select_edge(clip.id, edge),
            EdgeAction::Trim {
                edge,
                delta,
                linked,
            } => {
                report!(session, session.trim_edge(&clip.id, edge, delta, linked));
            }
            EdgeAction::RippleTrim {
                edge,
                delta,
                linked,
            } => {
                report!(session, session.ripple_edge(&clip.id, edge, delta, linked));
            }
            EdgeAction::Roll {
                edge,
                delta,
                linked,
            } => {
                report!(session, session.roll_edge(&clip.id, edge, delta, linked));
            }
            EdgeAction::RateStretch {
                edge,
                delta,
                linked,
            } => {
                report!(session, session.rate_edge(&clip.id, edge, delta, linked));
            }
            EdgeAction::None => {}
        }
        return;
    }
    let Some(body) = gesture.body else {
        return;
    };
    let release = frame_at(gesture.pointer.x, origin_x, ppf);
    let drag = release.saturating_sub(gesture.press_frame);
    let (destination, start) = moved_destination(session, &clip, &gesture, ppf, rows);
    let action = body_release(&body, gesture.moved, start, release, drag);
    match action {
        BodyAction::Split {
            frame,
            all_tracks,
            linked,
        } => {
            let ids = if all_tracks {
                clip_ids_spanning_frame(session.project(), frame)
            } else {
                vec![clip.id.clone()]
            };
            report!(session, session.split_at(&ids, frame, linked));
        }
        BodyAction::Slip { delta, linked } => {
            report!(session, session.slip_clip(&clip.id, delta, linked));
        }
        BodyAction::Slide { delta, linked } => {
            report!(session, session.slide_clip(&clip.id, delta, linked));
        }
        BodyAction::Move { frame, linked } => {
            report!(
                session,
                session.move_selection(&clip.id, destination, frame, linked),
            );
        }
        BodyAction::Duplicate { frame, .. } => {
            report!(session, session.duplicate_at(&clip.id, destination, frame));
        }
        BodyAction::Toggle { .. } => session.toggle_selected(clip.id),
        BodyAction::Select { linked, .. } => {
            if linked && !clip.link_group_id.is_empty() {
                let ids = session
                    .project()
                    .timeline_clips
                    .iter()
                    .filter(|other| other.link_group_id == clip.link_group_id)
                    .map(|other| other.id.clone())
                    .collect();
                session.select_ids(ids);
            } else {
                session.select_only(clip.id);
            }
        }
        BodyAction::None => {}
    }
}

fn commit_pen(
    session: &mut Session,
    clip: &mvm_project::TimelineClip,
    pen: &PenPress,
    gesture: &TimelineGesture,
    rect: Rect,
    ppf: f32,
    mods: GestureModifiers,
) {
    let PenPress::Edit {
        grab_offset_x,
        grab_offset_y,
        ..
    } = pen
    else {
        return;
    };
    let kind = key_kind(clip);
    let geometry = pen_geometry(&rect, ppf, kind);
    let x = f64::from(gesture.pointer.x - rect.min.x) + grab_offset_x;
    let y = f64::from(gesture.pointer.y - rect.min.y) + grab_offset_y;
    let frame = (x / f64::from(ppf.max(0.25))).round() as i64;
    let value = pen_snap_value(pen_value_at(&geometry, y), mods);
    if let Some(release) = pen_release(pen, frame, value) {
        report!(
            session,
            session.edit_key(
                &clip.id,
                kind,
                release.original_frame,
                release.frame,
                release.value,
            ),
        );
    }
}

fn moved_destination(
    session: &Session,
    clip: &mvm_project::TimelineClip,
    gesture: &TimelineGesture,
    ppf: f32,
    rows: &[(TrackKind, usize, String)],
) -> (TrackRef, i64) {
    let offset_x = f64::from(gesture.pointer.x - gesture.origin.x);
    let bounds = selection_bounds(session, &clip.id);
    let bounded = group_drag_offset_x(offset_x, f64::from(ppf), bounds);
    let spans = clip_spans(session);
    let moving = moving_ids(session, &clip.id);
    let moving_refs: Vec<&str> = moving.iter().map(String::as_str).collect();
    let snap = drag_snap_frames(&spans, &moving_refs, session.playhead_frame());
    let snapped = snap_drag_offset_x(bounded, f64::from(ppf), &snap, 8.0);
    let delta = if ppf.is_finite() && ppf > 0.0 {
        (snapped.offset_x / f64::from(ppf)).round() as i64
    } else {
        0
    };
    let start = clip.timeline_start_frame.saturating_add(delta).max(0);
    let Some(anchor_row) = rows
        .iter()
        .position(|(kind, index, _)| *kind == clip.track.kind && *index == clip.track.index)
    else {
        return (clip.track, start);
    };
    let row_delta = ((gesture.pointer.y - gesture.origin.y) / ROW_HEIGHT).round() as isize;
    let target = anchor_row as isize + row_delta;
    let Some((kind, snapped_index, _)) = target
        .try_into()
        .ok()
        .and_then(|index: usize| rows.get(index))
        .filter(|(kind, _, _)| *kind == clip.track.kind)
    else {
        return (clip.track, start);
    };
    let count = i64::try_from(session.project().tracks(*kind).len()).unwrap_or(0);
    let anchor = i64::try_from(clip.track.index).unwrap_or(0);
    let snapped_index = i64::try_from(*snapped_index).unwrap_or(anchor);
    let index = group_drag_track_index(
        *kind == TrackKind::Video,
        anchor,
        snapped_index,
        count,
        bounds,
    );
    let index = usize::try_from(index).unwrap_or(clip.track.index);
    let kind = *kind;
    if session
        .project()
        .is_valid_track_ref(TrackRef::new(kind, index))
    {
        (TrackRef::new(kind, index), start)
    } else {
        (clip.track, start)
    }
}

fn selection_bounds(session: &Session, anchor_id: &str) -> DragBounds {
    let use_selection = session.selection().iter().any(|id| id == anchor_id);
    let mut min_start = None;
    let mut video: Option<(i64, i64)> = None;
    let mut audio: Option<(i64, i64)> = None;
    for clip in &session.project().timeline_clips {
        let included = if use_selection {
            session.selection().iter().any(|id| id == &clip.id)
        } else {
            clip.id == anchor_id
        };
        if !included {
            continue;
        }
        min_start = Some(
            min_start
                .unwrap_or(clip.timeline_start_frame)
                .min(clip.timeline_start_frame),
        );
        let index = i64::try_from(clip.track.index).unwrap_or(0);
        let range = match clip.track.kind {
            TrackKind::Video => &mut video,
            TrackKind::Audio => &mut audio,
        };
        *range = Some(match *range {
            Some((low, high)) => (low.min(index), high.max(index)),
            None => (index, index),
        });
    }
    DragBounds {
        min_start_frame: min_start,
        video,
        audio,
    }
}

fn clip_spans(session: &Session) -> Vec<ClipSpan> {
    session
        .project()
        .timeline_clips
        .iter()
        .filter_map(|clip| {
            let (start, end) = clip_interval(session.project(), clip).ok()?;
            Some(ClipSpan {
                clip_id: clip.id.clone(),
                start,
                end,
            })
        })
        .collect()
}

fn moving_ids(session: &Session, anchor_id: &str) -> Vec<String> {
    if session.selection().iter().any(|id| id == anchor_id) {
        session.selection().to_vec()
    } else {
        vec![anchor_id.to_owned()]
    }
}

fn pen_geometry(
    rect: &Rect,
    ppf: f32,
    kind: mvm_project::clip_effects::ClipKeyKind,
) -> PenGeometry {
    let maximum = if kind == mvm_project::clip_effects::ClipKeyKind::Volume {
        200.0
    } else {
        100.0
    };
    PenGeometry {
        pixels_per_frame: f64::from(ppf),
        maximum,
        width: f64::from(rect.width()),
        height: f64::from(rect.height()),
        inset: 4.0,
        key_pixels: 8.0,
        line_pixels: 12.0,
    }
}

fn edge_tool(tool: TimelineTool) -> bool {
    matches!(
        tool,
        TimelineTool::Select | TimelineTool::Ripple | TimelineTool::Rolling | TimelineTool::Rate
    )
}

fn edge_at(x: f32, rect: Rect) -> Option<TrimEdge> {
    let left = (x - rect.min.x).abs();
    let right = (x - rect.max.x).abs();
    if left <= EDGE_PIXELS && left <= right {
        Some(TrimEdge::Left)
    } else if right <= EDGE_PIXELS {
        Some(TrimEdge::Right)
    } else {
        None
    }
}

fn gesture_modifiers(ui: &egui::Ui) -> GestureModifiers {
    let modifiers = ui.input(|input| input.modifiers);
    GestureModifiers {
        alt: modifiers.alt,
        shift: modifiers.shift,
    }
}

fn draw_playhead(ui: &mut egui::Ui, rect: Rect, frame: i64, ppf: f32) {
    let x = rect.min.x + frame as f32 * ppf;
    ui.painter().line_segment(
        [Pos2::new(x, rect.min.y), Pos2::new(x, rect.max.y)],
        Stroke::new(1.5, Color32::from_rgb(230, 90, 90)),
    );
}

fn clip_color(kind: TrackKind, selected: bool) -> Color32 {
    match (kind, selected) {
        (TrackKind::Video, false) => Color32::from_rgb(58, 92, 140),
        (TrackKind::Video, true) => Color32::from_rgb(96, 140, 196),
        (TrackKind::Audio, false) => Color32::from_rgb(46, 110, 86),
        (TrackKind::Audio, true) => Color32::from_rgb(78, 158, 124),
    }
}

fn track_rows(project: &Project) -> Vec<(TrackKind, usize, String)> {
    let mut rows = Vec::new();
    for (index, track) in project.video_tracks.iter().enumerate().rev() {
        rows.push((TrackKind::Video, index, track.name.clone()));
    }
    for (index, track) in project.audio_tracks.iter().enumerate() {
        rows.push((TrackKind::Audio, index, track.name.clone()));
    }
    rows
}

fn frame_at(x: f32, origin: f32, pixels_per_frame: f32) -> i64 {
    let local = f64::from((x - origin).max(0.0));
    let frame = local / f64::from(pixels_per_frame.max(0.25));
    if !frame.is_finite() || frame <= 0.0 {
        0
    } else if frame >= i64::MAX as f64 {
        i64::MAX
    } else {
        frame.floor() as i64
    }
}
