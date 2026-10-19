//! 選択中の clip・編集点・マーカー。値は離したときに 1 回だけ commit する。

use eframe::egui;
use mvm_app::Session;
use mvm_core::timecode::format_timecode;
use mvm_project::clip_effects::ClipKeyKind;
use mvm_project::timeline::clip_interval;
use mvm_project::{Project, TimelineClipKind};

macro_rules! report {
    ($session:ident, $call:expr $(,)?) => {{
        let result = $call;
        if let Err(error) = result {
            $session.set_alert(error.message());
        }
    }};
}

pub struct InspectorState {
    clip_id: Option<String>,
    /// 下書きを読んだときの [`Session::edit_revision`]。undo などで Project が変われば読み直す。
    revision: u64,
    opacity: f64,
    volume: f64,
    speed_percent: f64,
    spans: Vec<SpanDraft>,
    math: crate::math_ui::MathInspector,
}

#[derive(Clone, Debug)]
struct SpanDraft {
    id: String,
    before: i64,
    after: i64,
}

impl Default for InspectorState {
    fn default() -> Self {
        Self {
            clip_id: None,
            revision: 0,
            opacity: 100.0,
            volume: 100.0,
            speed_percent: 100.0,
            spans: Vec::new(),
            math: crate::math_ui::MathInspector::default(),
        }
    }
}

pub fn show(ui: &mut egui::Ui, session: &mut Session, state: &mut InspectorState) {
    ui.label("インスペクタ");
    if let Some(frame) = session.selected_marker() {
        ui.label(format!(
            "マーカー {}",
            frame_label(session.project(), frame)
        ));
        if ui.button("マーカーを削除").clicked() {
            report!(session, session.delete_marker(frame));
        }
    }
    let Some(id) = session.selected_clip().map(str::to_owned) else {
        if session.selected_edge().is_none() && session.selected_marker().is_none() {
            ui.label("選択なし");
        }
        math_button(ui, session);
        return;
    };
    let Some(clip) = session.project().clip(&id).cloned() else {
        ui.label("選択なし");
        return;
    };
    if state.clip_id.as_deref() != Some(id.as_str()) || state.revision != session.edit_revision() {
        load(session, &clip, state);
    }
    ui.label(format!("名前 {}", clip.name));
    ui.label(kind_label(clip.kind));
    match clip_interval(session.project(), &clip) {
        Ok((start, end)) => {
            ui.label(format!("イン {}", frame_label(session.project(), start)));
            ui.label(format!("アウト {}", frame_label(session.project(), end)));
        }
        Err(error) => {
            ui.label(error.to_string());
        }
    }
    if clip.kind == TimelineClipKind::Audio {
        let response = ui.add(
            egui::DragValue::new(&mut state.volume)
                .range(0.0..=200.0)
                .speed(1.0)
                .suffix(" % 音量"),
        );
        if response.drag_stopped() || response.lost_focus() {
            report!(
                session,
                session.set_effect(&id, ClipKeyKind::Volume, state.volume)
            );
        }
        key_list(ui, session, &clip, ClipKeyKind::Volume);
    } else {
        let response = ui.add(
            egui::DragValue::new(&mut state.opacity)
                .range(0.0..=100.0)
                .speed(1.0)
                .suffix(" % 不透明度"),
        );
        if response.drag_stopped() || response.lost_focus() {
            report!(
                session,
                session.set_effect(&id, ClipKeyKind::Opacity, state.opacity),
            );
        }
        key_list(ui, session, &clip, ClipKeyKind::Opacity);
    }
    if !clip.has_synthetic_source_domain() {
        let response = ui.add(
            egui::DragValue::new(&mut state.speed_percent)
                .range(10.0..=1000.0)
                .speed(1.0)
                .suffix(" % 速度"),
        );
        if response.drag_stopped() || response.lost_focus() {
            let percent = state.speed_percent.round() as i64;
            report!(session, session.set_speed_percent(&id, percent));
        }
    }
    let mut span_edit = None;
    let mut span_delete = None;
    for span in &mut state.spans {
        ui.label(format!("トランジション {}", span.id));
        let before = ui.add(
            egui::DragValue::new(&mut span.before)
                .range(0..=10_000)
                .suffix(" frame 前"),
        );
        let after = ui.add(
            egui::DragValue::new(&mut span.after)
                .range(0..=10_000)
                .suffix(" frame 後"),
        );
        if before.drag_stopped()
            || before.lost_focus()
            || after.drag_stopped()
            || after.lost_focus()
        {
            span_edit = Some((span.id.clone(), span.before, span.after));
        }
        if ui.button(format!("{} を削除", span.id)).clicked() {
            span_delete = Some(span.id.clone());
        }
    }
    if let Some((transition_id, before, after)) = span_edit {
        report!(
            session,
            session.set_transition_span(&transition_id, before, after),
        );
        if let Some(clip) = session.project().clip(&id).cloned() {
            load(session, &clip, state);
        }
    }
    if let Some(transition_id) = span_delete {
        report!(session, session.delete_transition(&transition_id));
        if let Some(clip) = session.project().clip(&id).cloned() {
            load(session, &clip, state);
        }
    }
    crate::math_ui::show(ui, session, &mut state.math, &clip);
    math_button(ui, session);
}

fn math_button(ui: &mut egui::Ui, session: &mut Session) {
    if session.selected_edge().is_some() && ui.button("数式の変形").clicked() {
        report!(session, session.apply_math_transform());
    }
}

fn key_list(
    ui: &mut egui::Ui,
    session: &mut Session,
    clip: &mvm_project::TimelineClip,
    kind: ClipKeyKind,
) {
    let mut remove = None;
    for key in clip.effects.keys(kind) {
        let label = format!("{} frame のキーを削除", key.frame);
        if ui.button(label).clicked() {
            remove = Some(key.frame);
        }
    }
    if let Some(frame) = remove {
        report!(session, session.delete_key(&clip.id, kind, frame));
    }
}

fn load(session: &Session, clip: &mvm_project::TimelineClip, state: &mut InspectorState) {
    let project = session.project();
    state.clip_id = Some(clip.id.clone());
    state.revision = session.edit_revision();
    state.opacity = clip.effects.base(ClipKeyKind::Opacity);
    state.volume = clip.effects.base(ClipKeyKind::Volume);
    state.speed_percent = if clip.speed.den == 0 {
        100.0
    } else {
        clip.speed.num as f64 * 100.0 / clip.speed.den as f64
    };
    state.spans = project
        .timeline_transitions
        .iter()
        .filter(|transition| {
            transition.outgoing_clip_id == clip.id || transition.incoming_clip_id == clip.id
        })
        .map(|transition| SpanDraft {
            id: transition.id.clone(),
            before: transition.frames_before_cut,
            after: transition.frames_after_cut,
        })
        .collect();
}

fn kind_label(kind: TimelineClipKind) -> &'static str {
    match kind {
        TimelineClipKind::Video => "映像",
        TimelineClipKind::Manim => "Manim",
        TimelineClipKind::Audio => "音声",
        TimelineClipKind::Image => "画像",
        TimelineClipKind::Text => "文字",
        TimelineClipKind::Math => "数式",
        TimelineClipKind::EquationSequence => "数式列",
        TimelineClipKind::Graph => "グラフ",
    }
}

fn frame_label(project: &Project, frame: i64) -> String {
    let fps = project.timeline_fps;
    let timecode = format_timecode(frame, fps.num, fps.den).unwrap_or_else(|| frame.to_string());
    let seconds = if fps.is_positive() {
        frame as f64 * fps.den as f64 / fps.num as f64
    } else {
        0.0
    };
    format!("{timecode}  ({frame} frame / {seconds:.3} 秒)")
}
