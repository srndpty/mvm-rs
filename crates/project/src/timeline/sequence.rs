//! 内部の時間軸を持つ clip（数式 sequence・グラフ）の timeline 上の操作と評価。

use mvm_core::Rational;
use mvm_core::source_frame_mapping::convert_frame_boundary;

use crate::equation_sequence::{
    ActionId, BindingStatus, EquationEvaluation, EquationSequenceClipData, EquationTargetStatus,
};
use crate::error::{ProjectError, ProjectResult, fail};
use crate::graph_clip::{GraphClipData, GraphFunctionId, GraphIntroKind, GraphValidationStatus};
use crate::model::{Project, TimelineClip, TimelineClipKind, TrackRef, default_still_clip_frames};

use super::common::EditResult;
use super::edit::{place_still_clip_at, place_timeline_clip_at};
use super::mapping::clip_source_frame_at;
use super::validate::validate_timeline;

fn unit_speed(clip: &TimelineClip) -> bool {
    clip.speed == Rational::new(1, 1)
}

/// 数式 sequence の data を `edit` で変え、timeline 全体を検証してから置き換える。全体の末尾まで表示していた
/// clip だけ、新しい末尾へ追従する。
pub fn edit_equation_sequence(
    project: &mut Project,
    clip_id: &str,
    edit: impl FnOnce(&mut EquationSequenceClipData) -> ProjectResult<()>,
) -> EditResult {
    let mut candidate = project.clone();
    let Some(index) = candidate.clip_index(clip_id) else {
        return fail("数式 sequence が存在しません");
    };
    let clip = &mut candidate.timeline_clips[index];
    if clip.kind != TimelineClipKind::EquationSequence {
        return fail("数式 sequence の編集対象が不正です");
    }
    let was_full_tail = clip.source_out_frame == clip.source_frame_count;
    edit(&mut clip.equation_sequence)?;
    let (_, length) = clip.equation_sequence.intervals()?;
    clip.source_frame_count = length;
    if was_full_tail {
        clip.source_out_frame = length;
    }
    validate_timeline(&candidate)?;
    *project = candidate;
    Ok(Some(index))
}

pub fn add_equation_sequence(
    project: &mut Project,
    data: EquationSequenceClipData,
    clip_id: String,
    name: String,
    track: TrackRef,
    start: i64,
) -> EditResult {
    let (_, length) = data.intervals()?;
    let clip = TimelineClip {
        kind: TimelineClipKind::EquationSequence,
        id: clip_id,
        name,
        track,
        timeline_start_frame: start,
        source_fps: project.timeline_fps,
        source_frame_count: length,
        source_out_frame: length,
        equation_sequence: data,
        ..TimelineClip::default()
    };
    let mut candidate = project.clone();
    candidate.timeline_clips.push(clip);
    validate_timeline(&candidate)?;
    *project = candidate;
    Ok(Some(project.timeline_clips.len() - 1))
}

/// clip の複製。clip の ID と、数式 sequence・グラフの内部 ID を全て発行し直す。
pub fn copy_internal_clip(
    source: &TimelineClip,
    new_id: &mut dyn FnMut() -> String,
) -> ProjectResult<TimelineClip> {
    if !matches!(
        source.kind,
        TimelineClipKind::EquationSequence | TimelineClipKind::Graph
    ) {
        return fail("数式 sequence・Graph のコピー対象が不正です");
    }
    let mut copy = source.clone();
    copy.id = new_id();
    if copy.id.is_empty() || copy.id == source.id {
        return fail("コピーの ID を作れません");
    }
    super::common::remap_internal_ids(&mut copy, new_id)?;
    Ok(copy)
}

/// clip 先頭からの output frame での数式 sequence の評価。
pub fn evaluate_equation_clip(
    clip: &TimelineClip,
    output: Rational,
    clip_local_output_frame: i64,
) -> ProjectResult<EquationEvaluation> {
    if clip.kind != TimelineClipKind::EquationSequence || !unit_speed(clip) {
        return fail("数式 sequence の種類または速度が不正です");
    }
    let source = clip_source_frame_at(clip, output, clip_local_output_frame)?;
    clip.equation_sequence.evaluate(source)
}

/// 描けるかどうか。描けない理由（標本の無い action・未解決の action）は個別に返す。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EquationRenderability {
    pub renderable: bool,
    pub unsampled_actions: Vec<ActionId>,
    pub unresolved_actions: Vec<ActionId>,
    pub error: Option<ProjectError>,
}

/// 数式 sequence を output の時間軸で描けるか。部分式の参照が全て解決し、全ての action が見える範囲で
/// 少なくとも 1 つの出力標本を持つこと。
pub fn equation_renderability(
    clip: &TimelineClip,
    output: Rational,
    output_height: i32,
) -> EquationRenderability {
    match equation_renderability_checked(clip, output, output_height) {
        Ok(result) => result,
        Err(error) => EquationRenderability {
            error: Some(error),
            ..EquationRenderability::default()
        },
    }
}

fn equation_renderability_checked(
    clip: &TimelineClip,
    output: Rational,
    output_height: i32,
) -> ProjectResult<EquationRenderability> {
    if clip.kind != TimelineClipKind::EquationSequence || !unit_speed(clip) {
        return fail("数式 sequence の種類または速度が不正です");
    }
    let data = &clip.equation_sequence;
    data.validate(output_height)?;
    let (intervals, length) = data.intervals()?;
    if length != clip.source_frame_count
        || clip.source_in_frame < 0
        || clip.source_out_frame <= clip.source_in_frame
        || clip.source_out_frame > length
    {
        return fail("数式 sequence の素材範囲が不正です");
    }
    let source = clip.source_fps;
    let visible = convert_frame_boundary(clip.source_in_frame, source, output, true)
        .zip(convert_frame_boundary(
            clip.source_out_frame,
            source,
            output,
            true,
        ))
        .filter(|(begin, end)| end > begin);
    let Some((visible_begin, visible_end)) = visible else {
        return fail("出力時間軸の換算が不成立または overflow です");
    };
    let mut result = EquationRenderability::default();
    for action in &data.actions {
        // 検証済みなので、action の状態とその hold の区間は必ずある。
        let Some(state_index) = data
            .states
            .iter()
            .position(|state| state.id == action.state)
        else {
            return fail("action の StateId が存在しません");
        };
        let state = &data.states[state_index];
        let Some(interval) = intervals
            .iter()
            .find(|interval| !interval.transition && interval.index == state_index)
        else {
            return fail("action の状態の区間がありません");
        };
        let span = interval
            .begin
            .checked_add(action.start)
            .and_then(|begin| begin.checked_add(action.duration).map(|end| (begin, end)));
        let Some((begin, end)) = span else {
            return fail("action の絶対区間が overflow です");
        };
        let resolved = action.target_status != EquationTargetStatus::Missing
            && state.parts.iter().any(|part| {
                part.id == action.target && part.binding.status == BindingStatus::Bound
            });
        if !resolved {
            result.unresolved_actions.push(action.id.clone());
        }
        if end <= clip.source_in_frame || begin >= clip.source_out_frame {
            continue;
        }
        let first = convert_frame_boundary(begin, source, output, true);
        let after = convert_frame_boundary(end, source, output, true);
        let (Some(first), Some(after)) = (first, after) else {
            return fail("action の出力区間が overflow です");
        };
        if first.max(visible_begin) >= after.min(visible_end) {
            result.unsampled_actions.push(action.id.clone());
        }
    }
    let invalid_binding = data.states.iter().any(|state| {
        state
            .parts
            .iter()
            .any(|part| part.binding.status != BindingStatus::Bound)
    });
    result.renderable = !invalid_binding
        && result.unsampled_actions.is_empty()
        && result.unresolved_actions.is_empty();
    if !result.renderable {
        result.error = Some(ProjectError::new(
            "部分式の参照が未解決、または action の出力標本がありません",
        ));
    }
    Ok(result)
}

/// グラフの data を `edit` で変え、timeline 全体を検証してから置き換える。
pub fn edit_graph(
    project: &mut Project,
    clip_id: &str,
    edit: impl FnOnce(&mut GraphClipData) -> ProjectResult<()>,
) -> EditResult {
    let mut candidate = project.clone();
    let Some(index) = candidate.clip_index(clip_id) else {
        return fail("Graph が存在しません");
    };
    let clip = &mut candidate.timeline_clips[index];
    if clip.kind != TimelineClipKind::Graph {
        return fail("Graph の編集対象が不正です");
    }
    edit(&mut clip.graph)?;
    validate_timeline(&candidate)?;
    *project = candidate;
    Ok(Some(index))
}

fn new_graph_clip(
    project: &Project,
    clip_id: String,
    function: GraphFunctionId,
    name: String,
) -> TimelineClip {
    let frames = default_still_clip_frames(project.timeline_fps);
    TimelineClip {
        kind: TimelineClipKind::Graph,
        id: clip_id,
        name,
        graph: GraphClipData::with_default_function(function),
        source_fps: project.timeline_fps,
        source_frame_count: frames,
        source_out_frame: frames,
        ..TimelineClip::default()
    }
}

pub fn add_graph(
    project: &mut Project,
    clip_id: String,
    function: GraphFunctionId,
    name: String,
    track: TrackRef,
    start: i64,
) -> EditResult {
    let clip = new_graph_clip(project, clip_id, function, name);
    place_timeline_clip_at(project, clip, track, start)
}

/// 作成メニューの入口。文字・数式と同じく、start で空いている映像 track へ置く。
pub fn place_new_graph(
    project: &mut Project,
    clip_id: String,
    function: GraphFunctionId,
    name: String,
    start: i64,
) -> EditResult {
    let clip = new_graph_clip(project, clip_id, function, name);
    place_still_clip_at(project, clip, start)
}

/// グラフの内部の尺を変える。全体の末尾まで表示していた clip だけ、新しい末尾へ追従する。
pub fn set_graph_source_duration(project: &mut Project, clip_id: &str, frames: i64) -> EditResult {
    let mut candidate = project.clone();
    let Some(index) = candidate.clip_index(clip_id) else {
        return fail("Graph が存在しません");
    };
    let clip = &mut candidate.timeline_clips[index];
    if clip.kind != TimelineClipKind::Graph {
        return fail("Graph ではありません");
    }
    let full = clip.source_out_frame == clip.source_frame_count;
    clip.source_frame_count = frames;
    if full {
        clip.source_out_frame = frames;
    }
    validate_timeline(&candidate)?;
    *project = candidate;
    Ok(Some(index))
}

/// グラフの 1 frame の評価。Draw の途中なら描き終えた割合を有理数で持つ。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GraphFrame {
    pub source_frame: i64,
    pub progress_numerator: i64,
    pub progress_denominator: i64,
}

pub fn evaluate_graph_clip(
    clip: &TimelineClip,
    output: Rational,
    local_output_frame: i64,
) -> ProjectResult<GraphFrame> {
    if clip.kind != TimelineClipKind::Graph
        || !unit_speed(clip)
        || clip.graph.validate(clip.source_frame_count) != GraphValidationStatus::Valid
    {
        return fail("Graph の種類・構造・速度が不正です");
    }
    let source_frame = clip_source_frame_at(clip, output, local_output_frame)?;
    let intro = clip.graph.intro;
    let drawing = intro.kind == GraphIntroKind::Draw && source_frame < intro.frames;
    Ok(GraphFrame {
        source_frame,
        progress_numerator: if drawing { source_frame } else { 1 },
        progress_denominator: if drawing { intro.frames } else { 1 },
    })
}
