//! 数式・数式列・グラフの clip の作成と編集。値の判定は project が行い、ここは ID の発行と undo 1 段を担う。

use mvm_project::equation_sequence::{
    ActionId, EquationAction, EquationOperation, EquationSequenceClipData, EquationState,
    EquationStepTransition, EquationTargetStatus, PartId, PartPair, SemanticPart, SourceBinding,
    StateId, TransitionId,
};
use mvm_project::graph_clip::{GraphClipData, GraphFunctionId};
use mvm_project::math_clip::{MathClipAnimation, MathClipData};
use mvm_project::timeline::{
    edit_equation_sequence, edit_graph, edit_math_clip, place_new_equation_sequence,
    place_new_graph, place_new_math, set_graph_source_duration,
};
use mvm_project::{ProjectResult, default_still_clip_frames};

use crate::error::AppError;
use crate::session::Session;

/// 新しい数式 clip の式。
const DEFAULT_MATH_SOURCE: &str = "a^2 + b^2 = c^2";

impl Session {
    /// 再生ヘッドの位置へ数式 clip を置き、選ぶ。
    pub fn add_math_clip(&mut self) -> Result<(), AppError> {
        let frame = self.playhead_frame;
        self.edit_with_ids(|project, new_id| {
            place_new_math(
                project,
                new_id(),
                MathClipData {
                    source: DEFAULT_MATH_SOURCE.to_owned(),
                    ..MathClipData::default()
                },
                frame,
            )
        })
    }

    pub fn set_math_clip(
        &mut self,
        clip_id: &str,
        data: MathClipData,
        animation: MathClipAnimation,
    ) -> Result<(), AppError> {
        self.transact(|project| edit_math_clip(project, clip_id, data, animation))
    }

    /// 再生ヘッドの位置へ数式列（状態 1 つ、5 秒）を置く。
    pub fn add_equation_sequence_clip(&mut self) -> Result<(), AppError> {
        let frame = self.playhead_frame;
        let hold = default_still_clip_frames(self.project.timeline_fps);
        self.edit_with_ids(|project, new_id| {
            let data = EquationSequenceClipData {
                states: vec![EquationState {
                    id: StateId::new(new_id()),
                    equation: MathClipData {
                        source: "x + x = 2x".to_owned(),
                        ..MathClipData::default()
                    },
                    revision: new_id(),
                    hold_frames: hold,
                    parts: Vec::new(),
                }],
                transitions: Vec::new(),
                actions: Vec::new(),
            };
            place_new_equation_sequence(project, new_id(), "数式列".to_owned(), data, frame)
        })
    }

    /// 数式列の data を変える。`edit` は ID の発行と出力の高さを受け取る。
    pub fn edit_equation(
        &mut self,
        clip_id: &str,
        edit: impl FnOnce(
            &mut EquationSequenceClipData,
            &mut dyn FnMut() -> String,
            i32,
        ) -> ProjectResult<()>,
    ) -> Result<(), AppError> {
        let height = self.project.output_height;
        let clip_id = clip_id.to_owned();
        self.edit_with_ids(move |project, new_id| {
            edit_equation_sequence(project, &clip_id, |data| edit(data, new_id, height))
        })
    }

    /// 最後の状態の後ろへ、同じ式の状態と変形（各 1 秒）を足す。
    pub fn append_equation_state(&mut self, clip_id: &str) -> Result<(), AppError> {
        let second = default_still_clip_frames(self.project.timeline_fps) / 5;
        self.edit_equation(clip_id, |data, new_id, height| {
            let Some(last) = data.states.last().cloned() else {
                return Err(mvm_project::ProjectError::new("数式列に状態がありません"));
            };
            let state = EquationState {
                id: StateId::new(new_id()),
                revision: new_id(),
                hold_frames: second.max(1),
                parts: Vec::new(),
                ..last.clone()
            };
            let edge = EquationStepTransition {
                id: TransitionId::new(new_id()),
                from: last.id.clone(),
                to: state.id.clone(),
                frames: second.max(1),
                correspondence: Vec::new(),
            };
            let position = data.states.len();
            data.insert_state(position, state, &[edge], height)
        })
    }

    /// 状態を消す。前後の状態は新しい変形（1 秒）でつなぐ。
    pub fn delete_equation_state(
        &mut self,
        clip_id: &str,
        state: &StateId,
    ) -> Result<(), AppError> {
        let second = default_still_clip_frames(self.project.timeline_fps) / 5;
        let state = state.clone();
        self.edit_equation(clip_id, move |data, new_id, height| {
            let Some(index) = data.states.iter().position(|item| item.id == state) else {
                return Err(mvm_project::ProjectError::new("状態が存在しません"));
            };
            let bridge =
                (index > 0 && index + 1 < data.states.len()).then(|| EquationStepTransition {
                    id: TransitionId::new(new_id()),
                    from: data.states[index - 1].id.clone(),
                    to: data.states[index + 1].id.clone(),
                    frames: second.max(1),
                    correspondence: Vec::new(),
                });
            data.delete_state(&state, bridge, height)
        })
    }

    /// 状態の式を置き換える。部分式の範囲は古い式のものとして無効になる（付け直す）。
    pub fn set_equation_source(
        &mut self,
        clip_id: &str,
        state: &StateId,
        source: &str,
    ) -> Result<(), AppError> {
        let state = state.clone();
        let source = source.to_owned();
        self.edit_equation(clip_id, move |data, new_id, height| {
            data.replace_source(&state, source, new_id(), height)
        })
    }

    pub fn set_equation_hold(
        &mut self,
        clip_id: &str,
        state: &StateId,
        frames: i64,
    ) -> Result<(), AppError> {
        let state = state.clone();
        self.edit_equation(clip_id, move |data, _, height| {
            data.change_hold(&state, frames, height)
        })
    }

    pub fn set_equation_transition(
        &mut self,
        clip_id: &str,
        transition: &TransitionId,
        frames: i64,
    ) -> Result<(), AppError> {
        let transition = transition.clone();
        self.edit_equation(clip_id, move |data, _, height| {
            data.change_transition(&transition, frames, height)
        })
    }

    /// 式の byte 範囲 [begin, end) を部分式にする。`selected_source` は範囲を選んだ式（編集欄の文字列）で、
    /// 確定済みの式と一致するときだけ受け付ける（未確定の式の位置で別の文字へ結び付けない）。
    pub fn add_equation_part(
        &mut self,
        clip_id: &str,
        state: &StateId,
        selected_source: &str,
        (begin, end): (usize, usize),
        label: &str,
    ) -> Result<(), AppError> {
        let state = state.clone();
        let label = label.trim().to_owned();
        let selected_source = selected_source.to_owned();
        self.edit_equation(clip_id, move |data, new_id, height| {
            let Some(target) = data.states.iter().find(|item| item.id == state) else {
                return Err(mvm_project::ProjectError::new("状態が存在しません"));
            };
            if target.equation.source != selected_source {
                return Err(mvm_project::ProjectError::new(
                    "式が確定していません。式を確定してから範囲を選んでください",
                ));
            }
            let Some(text) = target.equation.source.get(begin..end) else {
                return Err(mvm_project::ProjectError::new("部分式の範囲が式の外です"));
            };
            let part = SemanticPart {
                id: PartId::new(new_id()),
                label: if label.is_empty() {
                    text.trim().to_owned()
                } else {
                    label
                },
                binding: SourceBinding {
                    revision: target.revision.clone(),
                    begin: begin as i64,
                    end: end as i64,
                    expected_text: text.to_owned(),
                    status: mvm_project::equation_sequence::BindingStatus::Bound,
                },
            };
            data.add_part(&state, part, height)
        })
    }

    pub fn delete_equation_part(
        &mut self,
        clip_id: &str,
        state: &StateId,
        part: &PartId,
    ) -> Result<(), AppError> {
        let state = state.clone();
        let part = part.clone();
        self.edit_equation(clip_id, move |data, _, height| {
            data.delete_part(&state, &part, height)
        })
    }

    pub fn add_equation_correspondence(
        &mut self,
        clip_id: &str,
        transition: &TransitionId,
        from: &PartId,
        to: &PartId,
    ) -> Result<(), AppError> {
        let transition = transition.clone();
        let pair = PartPair {
            from: from.clone(),
            to: to.clone(),
        };
        self.edit_equation(clip_id, move |data, _, height| {
            data.add_correspondence(&transition, pair, height)
        })
    }

    pub fn remove_equation_correspondence(
        &mut self,
        clip_id: &str,
        transition: &TransitionId,
        pair: &PartPair,
    ) -> Result<(), AppError> {
        let transition = transition.clone();
        let pair = pair.clone();
        self.edit_equation(clip_id, move |data, _, height| {
            data.remove_correspondence(&transition, &pair, height)
        })
    }

    /// 状態の hold の `start` から `duration` frame、部分式を強調する。
    pub fn add_equation_action(
        &mut self,
        clip_id: &str,
        state: &StateId,
        target: &PartId,
        start: i64,
        duration: i64,
        operation: EquationOperation,
    ) -> Result<(), AppError> {
        let state = state.clone();
        let target = target.clone();
        self.edit_equation(clip_id, move |data, new_id, height| {
            data.add_action(
                EquationAction {
                    id: ActionId::new(new_id()),
                    state,
                    target,
                    target_status: EquationTargetStatus::Present,
                    start,
                    duration,
                    operation,
                },
                height,
            )
        })
    }

    pub fn delete_equation_action(
        &mut self,
        clip_id: &str,
        action: &ActionId,
    ) -> Result<(), AppError> {
        let action = action.clone();
        self.edit_equation(clip_id, move |data, _, height| {
            data.delete_action(&action, height)
        })
    }

    /// 再生ヘッドの位置へグラフ（y = x^2、5 秒）を置く。
    pub fn add_graph_clip(&mut self) -> Result<(), AppError> {
        let frame = self.playhead_frame;
        self.edit_with_ids(|project, new_id| {
            let clip_id = new_id();
            let function = GraphFunctionId(new_id());
            place_new_graph(project, clip_id, function, "グラフ".to_owned(), frame)
        })
    }

    pub fn set_graph_clip(&mut self, clip_id: &str, data: GraphClipData) -> Result<(), AppError> {
        self.transact(|project| {
            edit_graph(project, clip_id, |graph| {
                *graph = data;
                Ok(())
            })
        })
    }

    /// 関数を 1 本足す（上限は project が決める）。
    pub fn add_graph_function(&mut self, clip_id: &str) -> Result<(), AppError> {
        let clip_id = clip_id.to_owned();
        self.edit_with_ids(move |project, new_id| {
            let length = project
                .clip(&clip_id)
                .map_or(0, |clip| clip.source_frame_count);
            let id = GraphFunctionId(new_id());
            edit_graph(project, &clip_id, |graph| {
                graph.add_function(
                    mvm_project::graph_clip::GraphFunction {
                        id,
                        expression: "sin(x)".to_owned(),
                        color: "#FF55AAFF".to_owned(),
                        ..mvm_project::graph_clip::GraphFunction::default()
                    },
                    length,
                )
            })
        })
    }

    pub fn set_graph_duration(&mut self, clip_id: &str, frames: i64) -> Result<(), AppError> {
        self.transact(|project| set_graph_source_duration(project, clip_id, frames))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use mvm_project::math_clip::MathIntroKind;
    use mvm_project::{Project, TimelineClipKind};

    use super::*;

    fn selected_clip(session: &Session) -> mvm_project::TimelineClip {
        let id = session.selected_clip().unwrap().to_owned();
        session.project().clip(&id).unwrap().clone()
    }

    #[test]
    fn math_clips_are_placed_edited_and_undone_in_one_step_each() {
        let mut session = Session::from_project(Project::default());
        session.add_math_clip().unwrap();
        let clip = selected_clip(&session);
        assert_eq!(clip.kind, TimelineClipKind::Math);
        assert_eq!(clip.math.source, DEFAULT_MATH_SOURCE);
        let data = MathClipData {
            source: "\\frac{1}{2}".to_owned(),
            ..clip.math.clone()
        };
        let write = MathClipAnimation {
            intro: MathIntroKind::Write,
            intro_frames: 30,
        };
        session.set_math_clip(&clip.id, data, write).unwrap();
        assert_eq!(selected_clip(&session).math.source, "\\frac{1}{2}");
        // 空の式・尺を超える Write は確定しない。
        let empty = MathClipData {
            source: "  ".to_owned(),
            ..clip.math.clone()
        };
        assert!(session.set_math_clip(&clip.id, empty, write).is_err());
        let long = MathClipAnimation {
            intro: MathIntroKind::Write,
            intro_frames: 100_000,
        };
        assert!(
            session
                .set_math_clip(&clip.id, clip.math.clone(), long)
                .is_err()
        );
        session.undo().unwrap();
        assert_eq!(selected_clip(&session).math.source, DEFAULT_MATH_SOURCE);
    }

    #[test]
    fn an_equation_sequence_grows_by_states_parts_and_actions() {
        let mut session = Session::from_project(Project::default());
        session.add_equation_sequence_clip().unwrap();
        let clip = selected_clip(&session);
        session.append_equation_state(&clip.id).unwrap();
        let data = selected_clip(&session).equation_sequence;
        assert_eq!(data.states.len(), 2);
        assert_eq!(data.transitions.len(), 1);
        let second = data.states[1].id.clone();
        session
            .set_equation_source(&clip.id, &second, "x + x + x = 3x")
            .unwrap();
        // "3x" は byte 12..14。未確定の式（編集欄だけ書き換えた式）で選んだ範囲は受け付けない。
        assert!(
            session
                .add_equation_part(&clip.id, &second, "x + x + x = 4x", (12, 14), "")
                .is_err()
        );
        session
            .add_equation_part(&clip.id, &second, "x + x + x = 3x", (12, 14), "")
            .unwrap();
        let data = selected_clip(&session).equation_sequence;
        let part = data.states[1].parts[0].clone();
        assert_eq!(part.label, "3x");
        session
            .add_equation_action(&clip.id, &second, &part.id, 0, 10, EquationOperation::Pulse)
            .unwrap();
        let action = selected_clip(&session).equation_sequence.actions[0]
            .id
            .clone();
        // 範囲外の部分式は拒否する。
        assert!(
            session
                .add_equation_part(&clip.id, &second, "x + x + x = 3x", (12, 99), "")
                .is_err()
        );
        session.delete_equation_action(&clip.id, &action).unwrap();
        session.delete_equation_state(&clip.id, &second).unwrap();
        let data = selected_clip(&session).equation_sequence;
        assert_eq!((data.states.len(), data.transitions.len()), (1, 0));
    }

    #[test]
    fn graph_clips_get_a_second_function_and_a_new_length() {
        let mut session = Session::from_project(Project::default());
        session.add_graph_clip().unwrap();
        let clip = selected_clip(&session);
        session.add_graph_function(&clip.id).unwrap();
        assert_eq!(selected_clip(&session).graph.functions.len(), 2);
        session.set_graph_duration(&clip.id, 120).unwrap();
        assert_eq!(selected_clip(&session).source_frame_count, 120);
    }
}
