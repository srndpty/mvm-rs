//! timeline の S1（字幕）レーン。表示範囲の字幕だけを描く。
//!
//! クリックで選択（Ctrl / Shift で足し引き）、本体のドラッグで移動（Alt で元を残して複製）、
//! 端のドラッグでトリム。ドラッグ中は Project を変えず、離したときに 1 回だけ commit する。Esc で取り消す。

use eframe::egui::{self, Color32, Pos2, Rect, Sense, Stroke, Vec2};
use mvm_app::{Session, TimeWindow, ranges_overlap};

use crate::timeline_ui::{EDGE_PIXELS, ROW_HEIGHT};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Grip {
    Body,
    Left,
    Right,
}

pub struct SubtitleGesture {
    cue_id: String,
    grip: Grip,
    origin: Pos2,
    pointer: Pos2,
    moved: bool,
}

struct VisibleCue {
    id: String,
    start: i64,
    end: i64,
    content: String,
    linked: bool,
}

pub fn draw(
    ui: &mut egui::Ui,
    session: &mut Session,
    gesture: &mut Option<SubtitleGesture>,
    lane: Rect,
    window: TimeWindow,
    ppf: f32,
) {
    ui.painter()
        .rect_filled(lane, 0.0, Color32::from_rgb(34, 30, 40));
    let Some(track) = session.project().subtitles.as_ref() else {
        return;
    };
    let visible: Vec<VisibleCue> = track
        .cues
        .iter()
        .filter(|cue| ranges_overlap(cue.start_frame, cue.end_frame, window))
        .map(|cue| VisibleCue {
            id: cue.id.clone(),
            start: cue.start_frame,
            end: cue.end_frame,
            content: cue.content.lines().next().unwrap_or("").to_owned(),
            linked: !cue.link_clip_id.is_empty(),
        })
        .collect();
    let dimmed = !track.visible;
    if gesture.is_some() && ui.input(|input| input.key_pressed(egui::Key::Escape)) {
        *gesture = None;
    }
    for cue in visible {
        draw_cue(ui, session, gesture, &cue, lane, ppf, dimmed);
    }
}

fn draw_cue(
    ui: &mut egui::Ui,
    session: &mut Session,
    gesture: &mut Option<SubtitleGesture>,
    cue: &VisibleCue,
    lane: Rect,
    ppf: f32,
    dimmed: bool,
) {
    let x = lane.min.x + cue.start as f32 * ppf;
    let width = ((cue.end - cue.start) as f32 * ppf).max(2.0);
    let rect = Rect::from_min_size(
        Pos2::new(x, lane.min.y + 4.0),
        Vec2::new(width, ROW_HEIGHT - 8.0),
    );
    let selected = session.subtitle_selection().iter().any(|id| id == &cue.id);
    let fill = match (selected, dimmed) {
        (true, _) => Color32::from_rgb(168, 120, 196),
        (false, false) => Color32::from_rgb(112, 78, 140),
        (false, true) => Color32::from_rgb(70, 60, 80),
    };
    let painter = ui.painter();
    painter.rect_filled(rect, 3.0, fill);
    if cue.linked {
        painter.line_segment(
            [
                Pos2::new(rect.min.x, rect.max.y - 1.0),
                Pos2::new(rect.max.x, rect.max.y - 1.0),
            ],
            Stroke::new(2.0, Color32::from_rgb(220, 200, 240)),
        );
    }
    let response = ui.put(
        rect,
        egui::Label::new(cue.content.as_str())
            .truncate()
            .sense(Sense::click_and_drag()),
    );
    let response = response.on_hover_text(&cue.content);

    if response.is_pointer_button_down_on()
        && ui.input(|input| input.pointer.primary_pressed())
        && let Some(pointer) = ui.input(|input| input.pointer.press_origin())
    {
        let grip = if pointer.x - rect.min.x <= EDGE_PIXELS {
            Grip::Left
        } else if rect.max.x - pointer.x <= EDGE_PIXELS {
            Grip::Right
        } else {
            Grip::Body
        };
        let modifiers = ui.input(|input| input.modifiers);
        if modifiers.ctrl || modifiers.shift {
            session.select_subtitle(&cue.id, true);
        } else if !selected {
            session.select_subtitle(&cue.id, false);
        }
        *gesture = Some(SubtitleGesture {
            cue_id: cue.id.clone(),
            grip,
            origin: pointer,
            pointer,
            moved: false,
        });
    }
    if response.dragged()
        && let Some(active) = gesture.as_mut().filter(|active| active.cue_id == cue.id)
        && let Some(pointer) = response.interact_pointer_pos()
    {
        active.pointer = pointer;
        if active.origin.distance(pointer) > 4.0 {
            active.moved = true;
        }
    }
    if let Some(active) = gesture
        .as_ref()
        .filter(|active| active.moved && active.cue_id == cue.id)
    {
        let delta = active.pointer.x - active.origin.x;
        let ghost = match active.grip {
            Grip::Body => rect.translate(Vec2::new(delta, 0.0)),
            Grip::Left => Rect::from_min_max(Pos2::new(rect.min.x + delta, rect.min.y), rect.max),
            Grip::Right => Rect::from_min_max(rect.min, Pos2::new(rect.max.x + delta, rect.max.y)),
        };
        let alt = ui.input(|input| input.modifiers.alt);
        ui.painter().rect_stroke(
            ghost,
            3.0,
            Stroke::new(
                2.0,
                if alt && active.grip == Grip::Body {
                    Color32::from_rgba_unmultiplied(240, 220, 120, 140)
                } else {
                    Color32::from_rgb(240, 220, 120)
                },
            ),
            egui::StrokeKind::Outside,
        );
    }
    if ui.input(|input| input.pointer.primary_released())
        && let Some(active) = gesture.take_if(|active| active.cue_id == cue.id)
        && active.moved
    {
        let delta = ((active.pointer.x - active.origin.x) / ppf.max(0.01)).round() as i64;
        let alt = ui.input(|input| input.modifiers.alt);
        let result = match active.grip {
            Grip::Body => {
                let ids = session.subtitle_selection().to_vec();
                session.move_subtitles(&ids, delta, alt)
            }
            Grip::Left => {
                let full = full_content(session, &cue.id);
                session.update_subtitle(&cue.id, (cue.start + delta).max(0), cue.end, &full)
            }
            Grip::Right => {
                let full = full_content(session, &cue.id);
                session.update_subtitle(&cue.id, cue.start, cue.end + delta, &full)
            }
        };
        if let Err(error) = result {
            session.set_alert(error.message());
        }
    }
}

/// 表示は 1 行目だけなので、更新には全文を使う。
fn full_content(session: &Session, id: &str) -> String {
    session
        .project()
        .subtitles
        .as_ref()
        .and_then(|track| track.cues.iter().find(|cue| cue.id == id))
        .map(|cue| cue.content.clone())
        .unwrap_or_default()
}
