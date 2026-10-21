//! エフェクトコントロール。選択中の clip（選択が無ければ再生ヘッドの位置で preview に見えている clip）の
//! 書式・キーフレーム・フェードと、選択中の編集点・マーカー。並びは C++ 版と同じにする。
//! 値は離したとき（入力は確定したとき）に 1 回だけ commit する。

use eframe::egui;
use mvm_app::Session;
use mvm_core::timecode::format_timecode;
use mvm_project::{Project, TimelineClipKind};

use crate::theme;

macro_rules! report {
    ($session:ident, $call:expr $(,)?) => {{
        let result = $call;
        if let Err(error) = result {
            $session.set_alert(error.message());
        }
    }};
}

#[derive(Default)]
pub struct InspectorState {
    clip_id: Option<String>,
    /// 下書きを読んだときの [`Session::edit_revision`]。undo などで Project が変われば読み直す。
    revision: u64,
    spans: Vec<SpanDraft>,
    math: crate::math_ui::MathInspector,
    motion: crate::motion_ui::MotionState,
}

#[derive(Clone, Debug)]
struct SpanDraft {
    id: String,
    before: i64,
    after: i64,
}

impl InspectorState {
    /// キーの列・グラフを押した後で、選んだ channel がある（キーボードの操作をキーへ向ける）。
    pub fn wants_key_shortcuts(&self) -> bool {
        self.motion.wants_key_shortcuts()
    }
}

pub fn show(ui: &mut egui::Ui, session: &mut Session, state: &mut InspectorState) {
    ui.spacing_mut().item_spacing.y = 6.0;
    edit_point(ui, session);
    if let Some(frame) = session.selected_marker() {
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new(format!(
                    "マーカー {}",
                    frame_label(session.project(), frame)
                ))
                .size(11.0)
                .color(theme::TEXT_SECONDARY),
            );
            if ui.button("マーカーを削除").clicked() {
                report!(session, session.delete_marker(frame));
            }
        });
    }
    let Some(clip) = session.effect_target().cloned() else {
        if session.selected_edge().is_none() {
            secondary(ui, "クリップ未選択");
        }
        state.motion.forget();
        return;
    };
    if state.clip_id.as_deref() != Some(clip.id.as_str())
        || state.revision != session.edit_revision()
    {
        load(session, &clip, state);
    }
    // 文字 clip の名前は本文の先頭なので、本文の欄と重複する。出さない。
    if clip.kind == TimelineClipKind::Text {
        crate::motion_ui::show_text(ui, session, &mut state.motion, &clip);
    } else if session.selected_edge().is_none() {
        secondary(ui, &clip.name);
    }
    // 書式の欄が確定したら Project が変わる。後の欄は確定後の clip で描く（前の clip で読み直すと、確定前の
    // 値へ戻してしまう）。
    let Some(clip) = session.project().clip(&clip.id).cloned() else {
        return;
    };
    crate::math_ui::show(ui, session, &mut state.math, &clip);
    let Some(clip) = session.project().clip(&clip.id).cloned() else {
        return;
    };
    crate::motion_ui::show(ui, session, &mut state.motion, &clip);
    transitions(ui, session, state, &clip.id);
}

fn secondary(ui: &mut egui::Ui, text: &str) {
    ui.add(
        egui::Label::new(
            egui::RichText::new(text)
                .size(11.0)
                .color(theme::TEXT_SECONDARY),
        )
        .truncate(),
    );
}

/// 選択中の編集点。数式 clip の編集点では数式の変形を置ける（編集メニューと同じ操作）。
fn edit_point(ui: &mut egui::Ui, session: &mut Session) {
    let Some((clip_id, _)) = session.selected_edge() else {
        return;
    };
    let name = session
        .project()
        .clip(clip_id)
        .map(|clip| clip.name.clone())
        .unwrap_or_default();
    ui.label(
        egui::RichText::new("編集点")
            .font(theme::bold(12.0))
            .color(theme::TEXT),
    );
    secondary(ui, &name);
    if ui.button("数式の変形を適用").clicked() {
        report!(session, session.apply_math_transform());
    }
}

/// clip に掛かるトランジションの長さ（cut の前後の frame）と削除。
fn transitions(ui: &mut egui::Ui, session: &mut Session, state: &mut InspectorState, id: &str) {
    let mut span_edit = None;
    let mut span_delete = None;
    for span in &mut state.spans {
        ui.label(
            egui::RichText::new(format!("トランジション {}", span.id))
                .size(11.0)
                .color(theme::TEXT_SECONDARY),
        );
        ui.horizontal(|ui| {
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
            if ui.button("削除").clicked() {
                span_delete = Some(span.id.clone());
            }
        });
    }
    if let Some((transition_id, before, after)) = span_edit {
        report!(
            session,
            session.set_transition_span(&transition_id, before, after),
        );
    }
    if let Some(transition_id) = span_delete {
        report!(session, session.delete_transition(&transition_id));
    }
    if (state.revision != session.edit_revision())
        && let Some(clip) = session.project().clip(id).cloned()
    {
        load(session, &clip, state);
    }
}

fn load(session: &Session, clip: &mvm_project::TimelineClip, state: &mut InspectorState) {
    let project = session.project();
    state.clip_id = Some(clip.id.clone());
    state.revision = session.edit_revision();
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
