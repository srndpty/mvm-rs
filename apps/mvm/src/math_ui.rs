//! inspector の数式・数式列・グラフ・Manim clip の欄。
//!
//! 値は下書きで編集し、「適用」（式の欄では Ctrl+Enter）で 1 回だけ commit する。Esc で確定済みの値へ戻す。
//! 描画の状態（描画中・完了・誤り・利用不可）と、誤りの log を見せる。

use eframe::egui;
use mvm_app::{MathClipStatus, Session};
use mvm_project::equation_sequence::{EquationOperation, PartId, StateId};
use mvm_project::graph_clip::{GraphClipData, GraphIntroKind};
use mvm_project::math_clip::{MathClipAnimation, MathClipData, MathIntroKind};
use mvm_project::{TimelineClip, TimelineClipKind};

use crate::widgets::color_row;

macro_rules! report {
    ($session:ident, $call:expr $(,)?) => {{
        let result = $call;
        if let Err(error) = result {
            $session.set_alert(error.message());
        }
    }};
}

const ERROR_COLOR: egui::Color32 = egui::Color32::from_rgb(240, 120, 110);

#[derive(Default)]
pub struct MathInspector {
    clip_id: Option<String>,
    revision: u64,
    math: Option<MathClipData>,
    animation: MathClipAnimation,
    sources: Vec<String>,
    /// 式の欄で選んでいる byte 範囲（状態ごと）。部分式を足すときに使う。
    selections: Vec<Option<(usize, usize)>>,
    part_label: String,
    graph: Option<GraphClipData>,
    graph_length: i64,
    action_state: usize,
    action_part: Option<PartId>,
    action_start: i64,
    action_duration: i64,
    action_pulse: bool,
    pair_from: Option<PartId>,
    pair_to: Option<PartId>,
}

impl MathInspector {
    fn load(&mut self, session: &Session, clip: &TimelineClip) {
        self.clip_id = Some(clip.id.clone());
        self.revision = session.edit_revision();
        self.math = Some(clip.math.clone());
        self.animation = clip.math_animation;
        self.sources = clip
            .equation_sequence
            .states
            .iter()
            .map(|state| state.equation.source.clone())
            .collect();
        self.selections = vec![None; self.sources.len()];
        self.graph = Some(clip.graph.clone());
        self.graph_length = clip.source_frame_count;
        if self.action_duration <= 0 {
            self.action_duration = 30;
        }
    }
}

/// 数式系の clip なら欄を出す。
pub fn show(
    ui: &mut egui::Ui,
    session: &mut Session,
    state: &mut MathInspector,
    clip: &TimelineClip,
) {
    if state.clip_id.as_deref() != Some(clip.id.as_str())
        || state.revision != session.edit_revision()
    {
        state.load(session, clip);
    }
    match clip.kind {
        TimelineClipKind::Math => {
            status(ui, session, clip);
            math(ui, session, state, clip);
        }
        TimelineClipKind::EquationSequence => {
            status(ui, session, clip);
            equation(ui, session, state, clip);
        }
        TimelineClipKind::Graph => {
            status(ui, session, clip);
            graph(ui, session, state, clip);
        }
        TimelineClipKind::Manim => manim(ui, session, clip),
        _ => {}
    }
}

fn status(ui: &mut egui::Ui, session: &mut Session, clip: &TimelineClip) {
    ui.separator();
    match session.math_status(clip) {
        MathClipStatus::Idle | MathClipStatus::Ready => {
            ui.label("描画: 完了");
        }
        MathClipStatus::Checking => {
            ui.label("描画: 描画環境を確かめています");
        }
        MathClipStatus::Rendering => {
            ui.label("描画: 描画中");
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(250));
        }
        MathClipStatus::Unavailable(reason) => {
            ui.colored_label(ERROR_COLOR, format!("描画できません: {reason}"));
            if ui.button("再試行").clicked() {
                session.retry_math();
            }
        }
        MathClipStatus::InvalidSource { message, log } => {
            ui.colored_label(ERROR_COLOR, format!("式の誤り: {message}"));
            log_view(ui, &log);
        }
        MathClipStatus::Failed { message, log } => {
            ui.colored_label(ERROR_COLOR, format!("描画に失敗しました: {message}"));
            log_view(ui, &log);
            if ui.button("再試行").clicked() {
                session.retry_math();
            }
        }
        MathClipStatus::Invalid(reason) => {
            ui.colored_label(ERROR_COLOR, reason);
        }
    }
}

fn log_view(ui: &mut egui::Ui, log: &str) {
    if log.is_empty() {
        return;
    }
    egui::CollapsingHeader::new("log").show(ui, |ui| {
        egui::ScrollArea::vertical()
            .max_height(160.0)
            .show(ui, |ui| {
                ui.monospace(log);
            });
    });
}

fn math(ui: &mut egui::Ui, session: &mut Session, state: &mut MathInspector, clip: &TimelineClip) {
    let Some(draft) = state.math.as_mut() else {
        return;
    };
    ui.label("式（LaTeX）");
    let source = ui.add(
        egui::TextEdit::multiline(&mut draft.source)
            .code_editor()
            .desired_rows(3)
            .desired_width(f32::INFINITY),
    );
    let mut apply = source.has_focus()
        && ui.input(|input| input.key_pressed(egui::Key::Enter) && input.modifiers.ctrl);
    if source.has_focus() && ui.input(|input| input.key_pressed(egui::Key::Escape)) {
        *draft = clip.math.clone();
        state.animation = clip.math_animation;
    }
    ui.add(
        egui::DragValue::new(&mut draft.font_size)
            .range(1..=session.project().output_height.max(1))
            .prefix("文字サイズ ")
            .suffix(" px"),
    );
    color_row(ui, "文字色", &mut draft.color);
    color_row(ui, "背景", &mut draft.background_color);
    let mut write = state.animation.intro == MathIntroKind::Write;
    if ui
        .checkbox(&mut write, "先頭で書いていく（Write）")
        .changed()
    {
        state.animation = if write {
            MathClipAnimation {
                intro: MathIntroKind::Write,
                intro_frames: clip.source_duration().clamp(1, 60),
            }
        } else {
            MathClipAnimation::default()
        };
    }
    if state.animation.intro == MathIntroKind::Write {
        ui.add(
            egui::DragValue::new(&mut state.animation.intro_frames)
                .range(1..=clip.source_duration().max(1))
                .prefix("Write の尺 ")
                .suffix(" frame"),
        );
    }
    let changed = *draft != clip.math || state.animation != clip.math_animation;
    ui.horizontal(|ui| {
        apply |= ui
            .add_enabled(changed, egui::Button::new("適用"))
            .on_hover_text("Ctrl+Enter でも適用します")
            .clicked();
        if ui
            .add_enabled(changed, egui::Button::new("元に戻す"))
            .clicked()
        {
            *draft = clip.math.clone();
            state.animation = clip.math_animation;
        }
    });
    if apply && changed {
        let data = draft.clone();
        let animation = state.animation;
        report!(session, session.set_math_clip(&clip.id, data, animation));
    }
}

fn equation(
    ui: &mut egui::Ui,
    session: &mut Session,
    state: &mut MathInspector,
    clip: &TimelineClip,
) {
    let data = clip.equation_sequence.clone();
    if state.sources.len() != data.states.len() {
        state.load(session, clip);
    }
    for (index, item) in data.states.iter().enumerate() {
        ui.separator();
        ui.label(egui::RichText::new(format!("状態 {}", index + 1)).strong());
        let Some(source) = state.sources.get_mut(index) else {
            continue;
        };
        let output = egui::TextEdit::multiline(source)
            .code_editor()
            .desired_rows(2)
            .desired_width(f32::INFINITY)
            .show(ui);
        if let Some(range) = output.cursor_range {
            let (from, to) = (range.primary.index.0, range.secondary.index.0);
            let (low, high) = (from.min(to), from.max(to));
            let byte = |chars: usize| {
                source
                    .char_indices()
                    .nth(chars)
                    .map_or(source.len(), |(at, _)| at)
            };
            state.selections[index] = (high > low).then(|| (byte(low), byte(high)));
        }
        let apply = output.response.lost_focus()
            || (output.response.has_focus()
                && ui.input(|input| input.key_pressed(egui::Key::Enter) && input.modifiers.ctrl));
        if apply && *source != item.equation.source {
            let text = source.clone();
            report!(
                session,
                session.set_equation_source(&clip.id, &item.id, &text)
            );
        }
        let mut hold = item.hold_frames;
        let response = ui.add(
            egui::DragValue::new(&mut hold)
                .range(1..=100_000)
                .prefix("表示 ")
                .suffix(" frame"),
        );
        if (response.drag_stopped() || response.lost_focus()) && hold != item.hold_frames {
            report!(session, session.set_equation_hold(&clip.id, &item.id, hold));
        }
        ui.horizontal_wrapped(|ui| {
            for part in &item.parts {
                let bound =
                    part.binding.status == mvm_project::equation_sequence::BindingStatus::Bound;
                let label = if bound {
                    format!("部分式 {}", part.label)
                } else {
                    format!("部分式 {}（式の変更で外れた）", part.label)
                };
                ui.label(label);
                if ui.small_button("外す").clicked() {
                    report!(
                        session,
                        session.delete_equation_part(&clip.id, &item.id, &part.id)
                    );
                }
            }
        });
        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut state.part_label)
                    .hint_text("部分式の名前")
                    .desired_width(100.0),
            );
            let selection = state.selections.get(index).copied().flatten();
            if ui
                .add_enabled(selection.is_some(), egui::Button::new("選択範囲を部分式に"))
                .clicked()
                && let Some((begin, end)) = selection
            {
                let label = std::mem::take(&mut state.part_label);
                report!(
                    session,
                    session.add_equation_part(&clip.id, &item.id, begin, end, &label)
                );
            }
        });
        if data.states.len() > 1 && ui.button("この状態を消す").clicked() {
            report!(session, session.delete_equation_state(&clip.id, &item.id));
        }
        if let Some(transition) = data.transitions.get(index) {
            ui.separator();
            ui.label(format!("状態 {} → {} の変形", index + 1, index + 2));
            let mut frames = transition.frames;
            let response = ui.add(
                egui::DragValue::new(&mut frames)
                    .range(1..=9_999)
                    .prefix("変形 ")
                    .suffix(" frame"),
            );
            if (response.drag_stopped() || response.lost_focus()) && frames != transition.frames {
                report!(
                    session,
                    session.set_equation_transition(&clip.id, &transition.id, frames)
                );
            }
            for pair in &transition.correspondence {
                let name = |states: &[mvm_project::equation_sequence::EquationState],
                            id: &PartId| {
                    states
                        .iter()
                        .flat_map(|state| state.parts.iter())
                        .find(|part| part.id == *id)
                        .map(|part| part.label.clone())
                        .unwrap_or_default()
                };
                ui.horizontal(|ui| {
                    ui.label(format!(
                        "対応 {} → {}",
                        name(&data.states, &pair.from),
                        name(&data.states, &pair.to)
                    ));
                    if ui.small_button("外す").clicked() {
                        report!(
                            session,
                            session.remove_equation_correspondence(&clip.id, &transition.id, pair)
                        );
                    }
                });
            }
            let next = &data.states[index + 1];
            ui.horizontal(|ui| {
                part_combo(ui, ("pair_from", index), &item.parts, &mut state.pair_from);
                ui.label("→");
                part_combo(ui, ("pair_to", index), &next.parts, &mut state.pair_to);
                let ready = state.pair_from.is_some() && state.pair_to.is_some();
                if ui
                    .add_enabled(ready, egui::Button::new("対応を足す"))
                    .clicked()
                    && let (Some(from), Some(to)) = (state.pair_from.take(), state.pair_to.take())
                {
                    report!(
                        session,
                        session.add_equation_correspondence(&clip.id, &transition.id, &from, &to)
                    );
                }
            });
        }
    }
    ui.separator();
    if ui.button("状態を後ろに足す").clicked() {
        report!(session, session.append_equation_state(&clip.id));
    }
    actions(ui, session, state, clip);
}

fn part_combo(
    ui: &mut egui::Ui,
    salt: (&str, usize),
    parts: &[mvm_project::equation_sequence::SemanticPart],
    selected: &mut Option<PartId>,
) {
    let text = selected
        .as_ref()
        .and_then(|id| parts.iter().find(|part| part.id == *id))
        .map_or_else(|| "部分式".to_owned(), |part| part.label.clone());
    egui::ComboBox::from_id_salt(salt)
        .selected_text(text)
        .show_ui(ui, |ui| {
            for part in parts {
                ui.selectable_value(selected, Some(part.id.clone()), &part.label);
            }
        });
}

fn actions(
    ui: &mut egui::Ui,
    session: &mut Session,
    state: &mut MathInspector,
    clip: &TimelineClip,
) {
    let data = &clip.equation_sequence;
    ui.separator();
    ui.label(egui::RichText::new("強調").strong());
    for action in &data.actions {
        let state_index = data
            .states
            .iter()
            .position(|item| item.id == action.state)
            .unwrap_or(0);
        let label = data
            .states
            .get(state_index)
            .and_then(|item| item.parts.iter().find(|part| part.id == action.target))
            .map_or_else(String::new, |part| part.label.clone());
        let operation = match action.operation {
            EquationOperation::Outline => "枠",
            EquationOperation::Pulse => "拡大",
        };
        ui.horizontal(|ui| {
            ui.label(format!(
                "状態 {} の {label} を{operation}（{} frame から {} frame）",
                state_index + 1,
                action.start,
                action.duration
            ));
            if ui.small_button("消す").clicked() {
                report!(
                    session,
                    session.delete_equation_action(&clip.id, &action.id)
                );
            }
        });
    }
    let Some(target_state) = data
        .states
        .get(state.action_state.min(data.states.len().saturating_sub(1)))
    else {
        return;
    };
    ui.horizontal(|ui| {
        egui::ComboBox::from_id_salt("action_state")
            .selected_text(format!("状態 {}", state.action_state + 1))
            .show_ui(ui, |ui| {
                for index in 0..data.states.len() {
                    ui.selectable_value(
                        &mut state.action_state,
                        index,
                        format!("状態 {}", index + 1),
                    );
                }
            });
        part_combo(
            ui,
            ("action_part", 0),
            &target_state.parts,
            &mut state.action_part,
        );
    });
    ui.horizontal(|ui| {
        ui.add(
            egui::DragValue::new(&mut state.action_start)
                .range(0..=100_000)
                .prefix("開始 "),
        );
        ui.add(
            egui::DragValue::new(&mut state.action_duration)
                .range(1..=9_999)
                .prefix("尺 "),
        );
        ui.checkbox(&mut state.action_pulse, "拡大（外すと枠）");
    });
    let target_id: StateId = target_state.id.clone();
    if ui
        .add_enabled(state.action_part.is_some(), egui::Button::new("強調を足す"))
        .clicked()
        && let Some(part) = state.action_part.clone()
    {
        let operation = if state.action_pulse {
            EquationOperation::Pulse
        } else {
            EquationOperation::Outline
        };
        report!(
            session,
            session.add_equation_action(
                &clip.id,
                &target_id,
                &part,
                state.action_start,
                state.action_duration,
                operation,
            )
        );
    }
}

fn graph(ui: &mut egui::Ui, session: &mut Session, state: &mut MathInspector, clip: &TimelineClip) {
    let Some(draft) = state.graph.as_mut() else {
        return;
    };
    ui.label("表示範囲");
    ui.horizontal(|ui| {
        ui.add(
            egui::DragValue::new(&mut draft.viewport.x_min)
                .speed(0.1)
                .prefix("x "),
        );
        ui.add(
            egui::DragValue::new(&mut draft.viewport.x_max)
                .speed(0.1)
                .prefix("〜 "),
        );
    });
    ui.horizontal(|ui| {
        ui.add(
            egui::DragValue::new(&mut draft.viewport.y_min)
                .speed(0.1)
                .prefix("y "),
        );
        ui.add(
            egui::DragValue::new(&mut draft.viewport.y_max)
                .speed(0.1)
                .prefix("〜 "),
        );
    });
    ui.horizontal(|ui| {
        ui.checkbox(&mut draft.axes.show_axes, "軸");
        ui.checkbox(&mut draft.axes.show_grid, "格子");
    });
    ui.horizontal(|ui| {
        ui.label("x 軸のラベル");
        ui.text_edit_singleline(&mut draft.axes.x_label);
    });
    ui.horizontal(|ui| {
        ui.label("y 軸のラベル");
        ui.text_edit_singleline(&mut draft.axes.y_label);
    });
    let mut remove = None;
    for (index, function) in draft.functions.iter_mut().enumerate() {
        ui.separator();
        ui.label(format!("関数 {}", index + 1));
        ui.horizontal(|ui| {
            ui.label("y =");
            ui.add(egui::TextEdit::singleline(&mut function.expression).code_editor());
        });
        ui.horizontal(|ui| {
            ui.label("ラベル（LaTeX）");
            ui.text_edit_singleline(&mut function.label);
        });
        color_row(ui, "色", &mut function.color);
        ui.add(
            egui::DragValue::new(&mut function.stroke_width)
                .range(0.5..=40.0)
                .speed(0.1)
                .prefix("太さ "),
        );
        if ui.small_button("この関数を消す").clicked() {
            remove = Some(index);
        }
    }
    if let Some(index) = remove {
        draft.functions.remove(index);
    }
    let mut draw = draft.intro.kind == GraphIntroKind::Draw;
    if ui.checkbox(&mut draw, "先頭で描いていく（Draw）").changed() {
        draft.intro.kind = if draw {
            GraphIntroKind::Draw
        } else {
            GraphIntroKind::None
        };
        draft.intro.frames = if draw {
            clip.source_frame_count.clamp(1, 60)
        } else {
            0
        };
    }
    if draft.intro.kind == GraphIntroKind::Draw {
        ui.add(
            egui::DragValue::new(&mut draft.intro.frames)
                .range(1..=clip.source_frame_count.max(1))
                .prefix("Draw の尺 ")
                .suffix(" frame"),
        );
    }
    let changed = *draft != clip.graph;
    ui.horizontal(|ui| {
        if ui.add_enabled(changed, egui::Button::new("適用")).clicked() {
            let data = draft.clone();
            report!(session, session.set_graph_clip(&clip.id, data));
        }
        if ui
            .add_enabled(changed, egui::Button::new("元に戻す"))
            .clicked()
        {
            *draft = clip.graph.clone();
        }
    });
    if ui.button("関数を足す").clicked() {
        report!(session, session.add_graph_function(&clip.id));
    }
    let response = ui.add(
        egui::DragValue::new(&mut state.graph_length)
            .range(1..=1_000_000)
            .prefix("内部の尺 ")
            .suffix(" frame"),
    );
    if (response.drag_stopped() || response.lost_focus())
        && state.graph_length != clip.source_frame_count
    {
        report!(
            session,
            session.set_graph_duration(&clip.id, state.graph_length)
        );
    }
}

fn manim(ui: &mut egui::Ui, session: &mut Session, clip: &TimelineClip) {
    ui.separator();
    let Some(asset) = session.manim_asset(&clip.id).cloned() else {
        ui.colored_label(ERROR_COLOR, "生成元の script が Project にありません");
        return;
    };
    ui.label(format!("script {}", asset.script_path.display()));
    ui.label(format!("Scene {}", asset.scene_name));
    ui.label(format!("状態 {}", asset.generation_state.name()));
    if session.is_generating_manim() {
        ui.label("生成中");
        if ui.button("生成を取り消す").clicked() {
            session.cancel_manim_clip();
        }
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_millis(250));
    } else if ui
        .button("再生成")
        .on_hover_text("script を直した後に、同じ Scene を描き直す")
        .clicked()
    {
        report!(session, session.regenerate_manim_clip(&clip.id));
    }
}

/// 「挿入 > Manim clip」の dialog。script と Scene 名を指定して生成する。
#[derive(Default)]
pub struct ManimDialog {
    open: bool,
    script: String,
    scene: String,
    error: String,
}

impl ManimDialog {
    pub fn open(&mut self) {
        self.error.clear();
        self.open = true;
    }
}

pub fn show_manim_dialog(ui: &mut egui::Ui, session: &mut Session, dialog: &mut ManimDialog) {
    if let Some(result) = session.poll_manim_clip() {
        match result {
            Ok(()) => dialog.open = false,
            Err(error) => {
                dialog.error = error.message().to_owned();
                dialog.open = true;
            }
        }
    }
    if !dialog.open {
        return;
    }
    let mut open = true;
    egui::Window::new("Manim clip")
        .collapsible(false)
        .open(&mut open)
        .show(ui.ctx(), |ui| {
            ui.horizontal(|ui| {
                ui.label("script");
                ui.add(egui::TextEdit::singleline(&mut dialog.script).desired_width(300.0));
                if ui.button("選ぶ").clicked()
                    && let Some(path) = rfd::FileDialog::new()
                        .add_filter("Python", &["py"])
                        .pick_file()
                {
                    dialog.script = path.display().to_string();
                }
            });
            ui.horizontal(|ui| {
                ui.label("Scene");
                ui.add(egui::TextEdit::singleline(&mut dialog.scene).desired_width(200.0));
            });
            ui.label("Project の寸法と fps で生成し、再生ヘッドの位置へ置きます");
            if !dialog.error.is_empty() {
                ui.colored_label(ERROR_COLOR, &dialog.error);
            }
            if session.is_generating_manim() {
                ui.label("生成中");
                if ui.button("取り消す").clicked() {
                    session.cancel_manim_clip();
                }
                ui.ctx()
                    .request_repaint_after(std::time::Duration::from_millis(250));
            } else if ui.button("生成").clicked() {
                dialog.error.clear();
                let script = std::path::PathBuf::from(dialog.script.trim());
                let scene = dialog.scene.trim().to_owned();
                if let Err(error) = session.start_manim_clip(script, scene) {
                    dialog.error = error.message().to_owned();
                }
            }
        });
    if !open {
        dialog.open = false;
    }
}
