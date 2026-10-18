//! timeline の表示。区間の外にある clip は widget を作らない。

use eframe::egui::{self, Align2, Color32, Label, Pos2, Rect, Sense, Stroke, Vec2};

use mvm_app::{TimeWindow, display_span_frames, ranges_overlap, ruler_step, visible_window};
use mvm_core::timecode::format_timecode;
use mvm_project::timeline::clip_interval;
use mvm_project::{Project, TrackKind};

use mvm_app::Session;

const HEADER_WIDTH: f32 = 72.0;
const RULER_HEIGHT: f32 = 28.0;
const ROW_HEIGHT: f32 = 36.0;

pub fn show(ui: &mut egui::Ui, session: &mut Session) {
    ui.horizontal(|ui| {
        ui.label("タイムライン");
        if ui.button("縮小").clicked() {
            session.zoom_by(0.5);
        }
        ui.label(format!("{}%", session.zoom_percent()));
        if ui.button("拡大").clicked() {
            session.zoom_by(2.0);
        }
        ui.monospace(session.playhead_label());
    });

    let rows = track_rows(session.project());
    let ppf = session.pixels_per_frame();
    let span =
        display_span_frames(session.project()).max(session.playhead_frame().saturating_add(1));
    let content_width = (span as f32).mul_add(ppf, 48.0).max(1.0);

    ui.horizontal(|ui| {
        ui.vertical(|ui| {
            ui.add_space(RULER_HEIGHT);
            for (_, _, name) in &rows {
                ui.allocate_ui_with_layout(
                    Vec2::new(HEADER_WIDTH, ROW_HEIGHT),
                    egui::Layout::left_to_right(egui::Align::Center),
                    |ui| {
                        ui.label(name);
                    },
                );
            }
        });

        // この frame で見えている範囲（content 座標）から、描く clip を決める。
        egui::ScrollArea::horizontal()
            .id_salt("timeline_scroll")
            .show_viewport(ui, |ui, viewport| {
                let window = visible_window(viewport.min.x, viewport.width(), ppf);
                draw_contents(ui, session, &rows, content_width, window, ppf);
            });
    });
}

fn draw_contents(
    ui: &mut egui::Ui,
    session: &mut Session,
    rows: &[(TrackKind, usize, String)],
    content_width: f32,
    window: TimeWindow,
    ppf: f32,
) {
    let height = RULER_HEIGHT + rows.len() as f32 * ROW_HEIGHT;
    let (rect, response) = ui.allocate_exact_size(
        Vec2::new(content_width, height.max(1.0)),
        Sense::click_and_drag(),
    );
    if (response.dragged() || response.clicked())
        && let Some(pointer) = response.interact_pointer_pos()
    {
        session.set_scrubbing(response.dragged());
        session.seek(frame_at(pointer.x, rect.min.x, ppf));
    }
    if response.drag_stopped() {
        session.set_scrubbing(false);
    }

    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 0.0, Color32::from_rgb(22, 24, 28));
    draw_ruler(ui, session.project(), rect, window, ppf);

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
        draw_clip(ui, session, &clip, rect.min.x, ppf);
    }
    draw_playhead(ui, rect, session.playhead_frame(), ppf);
    if interval_failed {
        ui.label("区間を計算できない clip があります");
    }
}

fn draw_ruler(ui: &mut egui::Ui, project: &Project, rect: Rect, window: TimeWindow, ppf: f32) {
    let painter = ui.painter_at(rect);
    let ruler = Rect::from_min_size(rect.min, Vec2::new(rect.width(), RULER_HEIGHT));
    painter.rect_filled(ruler, 0.0, Color32::from_rgb(28, 30, 34));
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
    y: f32,
    start: i64,
    end: i64,
}

fn draw_clip(
    ui: &mut egui::Ui,
    session: &mut Session,
    clip: &VisibleClip,
    origin_x: f32,
    ppf: f32,
) {
    let x = origin_x + clip.start as f32 * ppf;
    let width = ((clip.end - clip.start) as f32 * ppf).max(2.0);
    let rect = Rect::from_min_size(
        Pos2::new(x, clip.y + 4.0),
        Vec2::new(width, ROW_HEIGHT - 8.0),
    );
    let selected = session.selected_clip() == Some(clip.id.as_str());
    let color = clip_color(clip.kind, selected);
    ui.painter().rect_filled(rect, 3.0, color);
    let response = ui.put(
        rect,
        Label::new(clip.name.as_str())
            .truncate()
            .sense(Sense::click()),
    );
    if response.clicked() {
        session.select_clip(Some(clip.id.clone()));
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
