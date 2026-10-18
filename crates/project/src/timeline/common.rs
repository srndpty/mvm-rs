//! timeline 編集の間で共有する小さな部品（端の trim・動かせる範囲・編集対象・確定）。

use std::collections::{HashMap, HashSet};

use crate::clip_effects::reframe_clip_keys;
use crate::error::{ProjectError, ProjectResult, fail};
use crate::math_clip::{MathClipAnimation, MathIntroKind};
use crate::model::{Project, TimelineClip};
use crate::subtitles::{reconcile_subtitle_links, remap_linked_subtitles};

use super::mapping::{
    clip_interval, clip_source_boundary_to_timeline, clip_timeline_boundary_to_source,
    source_boundary_to_timeline_boundary, timeline_clip_duration,
};
use super::validate::{reconcile_timeline_transitions, validate_timeline};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TrimEdge {
    Left,
    Right,
}

/// リンクされた clip（video / audio の 1 組）の相手にも同じ編集を適用するか。
/// 既定の操作は Linked で、Alt を押しながらの操作は Single（操作した clip だけ）になる。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum LinkMode {
    Linked,
    Single,
}

/// 編集の結果として選択する clip の位置。
pub type EditResult = ProjectResult<Option<usize>>;

/// clip ID → timeline_clips の位置。トランジションごとに全 clip を走査しない（clip 数 × トランジション数に
/// しない）ために一度だけ作る。ID が重複していれば先頭の位置を返す。
pub struct ClipIdIndex<'a> {
    indices: HashMap<&'a str, usize>,
}

impl<'a> ClipIdIndex<'a> {
    pub fn new(project: &'a Project) -> Self {
        let mut indices = HashMap::with_capacity(project.timeline_clips.len());
        for (index, clip) in project.timeline_clips.iter().enumerate() {
            indices.entry(clip.id.as_str()).or_insert(index);
        }
        Self { indices }
    }

    pub fn find(&self, id: &str) -> Option<usize> {
        self.indices.get(id).copied()
    }
}

/// trim した clip の fade を縮めた尺に収める。先頭側を優先して残す。
pub(crate) fn clamp_fades_to_length(clip: &mut TimelineClip) {
    let length = clip.source_duration();
    clip.effects.fade_in_frames = clip.effects.fade_in_frames.min(length);
    clip.effects.fade_out_frames = clip
        .effects
        .fade_out_frames
        .min(length - clip.effects.fade_in_frames);
}

/// clip の片端を project frame 単位で動かす。検証は呼び出し側が candidate 全体に対して行う。
/// left 端を動かしても右端（clip の終端）は timeline 上で動かない。
pub(crate) fn trim_clip_boundary(
    project: &Project,
    clip: &mut TimelineClip,
    edge: TrimEdge,
    delta: i64,
) -> ProjectResult<()> {
    if delta == i64::MIN {
        return fail("trim delta の絶対値を表せません");
    }
    let original_duration = timeline_clip_duration(project, clip)?;
    let original_start = clip.timeline_start_frame;
    if clip.has_synthetic_source_domain() {
        let Some(original_end) = original_start.checked_add(original_duration) else {
            return fail("text / image clip の終端が範囲外です");
        };
        let new_start = i128::from(original_start)
            + if edge == TrimEdge::Left {
                i128::from(delta)
            } else {
                0
            };
        let new_end = i128::from(original_end)
            + if edge == TrimEdge::Right {
                i128::from(delta)
            } else {
                0
            };
        let valid = new_start >= 0
            && new_end > new_start
            && new_end <= i128::from(i64::MAX)
            && new_end - new_start <= i128::from(i64::MAX / 2);
        if !valid {
            return fail("text / image clip の trim 範囲が不正です");
        }
        let (new_start, new_end) = (new_start as i64, new_end as i64);
        // 数式の Write は clip の見えている先頭から始まる（左 trim でも先頭から書き直す）。
        // trim は素材 frame を timeline の fps へ揃え直すので、尺も同じ秒数へ換算し、縮めた尺に収める。
        let animation = &mut clip.math_animation;
        if animation.intro != MathIntroKind::None {
            if clip.source_fps != project.timeline_fps {
                let retimed = source_boundary_to_timeline_boundary(
                    animation.intro_frames,
                    clip.source_fps,
                    project.timeline_fps,
                )?;
                animation.intro_frames = retimed.max(1);
            }
            animation.intro_frames = animation.intro_frames.min(new_end - new_start);
        }
        clip.timeline_start_frame = new_start;
        clip.source_fps = project.timeline_fps;
        clip.source_in_frame = 0;
        clip.source_out_frame = new_end - new_start;
        clip.source_frame_count = clip.source_out_frame;
        let new_duration = clip.source_out_frame;
        clip.effects.for_each_keys_mut(|_, keys| {
            reframe_clip_keys(
                keys,
                original_duration,
                new_duration,
                new_start - original_start,
            );
        });
        return Ok(());
    }
    let original = match edge {
        TrimEdge::Left => clip.source_in_frame,
        TrimEdge::Right => clip.source_out_frame,
    };
    let timeline_boundary = clip_source_boundary_to_timeline(clip, original, project.timeline_fps)?;
    let Some(moved_boundary) = timeline_boundary
        .checked_add(delta)
        .filter(|boundary| *boundary >= 0)
    else {
        return fail("trim delta が範囲外です");
    };
    let source_boundary =
        clip_timeline_boundary_to_source(clip, moved_boundary, project.timeline_fps)?;
    match edge {
        TrimEdge::Left => {
            clip.source_in_frame = source_boundary;
            let new_duration = timeline_clip_duration(project, clip)?;
            let start_delta = original_duration - new_duration;
            let Some(start) = original_start
                .checked_add(start_delta)
                .filter(|start| *start >= 0)
            else {
                return fail("left trim 後の timeline start が範囲外です");
            };
            clip.timeline_start_frame = start;
        }
        TrimEdge::Right => clip.source_out_frame = source_boundary,
    }
    let adjusted_duration = timeline_clip_duration(project, clip)?;
    let local_shift = clip.timeline_start_frame - original_start;
    clip.effects.for_each_keys_mut(|_, keys| {
        reframe_clip_keys(keys, original_duration, adjusted_duration, local_shift);
    });
    Ok(())
}

/// clip の edge を timeline 上で動かせる範囲 [lower, upper]（project frame）。
/// 素材の範囲を超えず、clip の尺を 1 frame 以上に保つ。
pub(crate) fn edge_range(
    project: &Project,
    clip: &TimelineClip,
    edge: TrimEdge,
) -> ProjectResult<(i64, i64)> {
    if clip.has_synthetic_source_domain() {
        let duration = timeline_clip_duration(project, clip)?;
        return Ok(match edge {
            TrimEdge::Left => (-clip.timeline_start_frame, duration - 1),
            TrimEdge::Right => (
                -(duration - 1),
                i64::MAX / 2 - clip.timeline_start_frame - duration,
            ),
        });
    }
    let fps = project.timeline_fps;
    let in_frame = clip_source_boundary_to_timeline(clip, clip.source_in_frame, fps)?;
    let out_frame = clip_source_boundary_to_timeline(clip, clip.source_out_frame, fps)?;
    let end_frame = clip_source_boundary_to_timeline(clip, clip.source_frame_count, fps)?;
    let duration = out_frame - in_frame;
    Ok(match edge {
        TrimEdge::Left => (-in_frame, duration - 1),
        TrimEdge::Right => (-(duration - 1), end_frame - out_frame),
    })
}

pub(crate) fn narrow_range(range: &mut (i64, i64), other: (i64, i64)) {
    range.0 = range.0.max(other.0);
    range.1 = range.1.min(other.1);
}

/// 編集の対象になる clip。clip 自身を先頭に置き、Linked ならリンク相手を後ろへ足す。
pub(crate) fn edit_targets(project: &Project, index: usize, link_mode: LinkMode) -> Vec<usize> {
    let mut targets = vec![index];
    let group = &project.timeline_clips[index].link_group_id;
    if link_mode == LinkMode::Single || group.is_empty() {
        return targets;
    }
    targets.extend(
        project
            .timeline_clips
            .iter()
            .enumerate()
            .filter(|(other, clip)| *other != index && clip.link_group_id == *group)
            .map(|(other, _)| other),
    );
    targets
}

/// clip の edge と接している同じ track の clip。
pub(crate) fn adjacent_clip_index(
    project: &Project,
    index: usize,
    edge: TrimEdge,
) -> ProjectResult<Option<usize>> {
    let clip = &project.timeline_clips[index];
    let (start, end) = clip_interval(project, clip)?;
    for (other, candidate) in project.timeline_clips.iter().enumerate() {
        if other == index || candidate.track != clip.track {
            continue;
        }
        let (other_start, other_end) = clip_interval(project, candidate)?;
        let touching = match edge {
            TrimEdge::Right => other_start == end,
            TrimEdge::Left => other_end == start,
        };
        if touching {
            return Ok(Some(other));
        }
    }
    Ok(None)
}

/// 同じ link group の相手。
pub(crate) fn link_partner_index(project: &Project, index: usize) -> Option<usize> {
    let group = &project.timeline_clips[index].link_group_id;
    if group.is_empty() {
        return None;
    }
    project
        .timeline_clips
        .iter()
        .enumerate()
        .position(|(other, clip)| other != index && clip.link_group_id == *group)
}

/// marked に印を付けた clip のリンク相手にも印を付ける。link は横移動を同期する契約である。
pub(crate) fn include_linked_counterparts(project: &Project, marked: &mut [bool]) {
    let groups: HashSet<&str> = project
        .timeline_clips
        .iter()
        .zip(marked.iter())
        .filter(|(clip, marked)| **marked && clip.is_linked())
        .map(|(clip, _)| clip.link_group_id.as_str())
        .collect();
    for (clip, marked) in project.timeline_clips.iter().zip(marked.iter_mut()) {
        if clip.is_linked() && groups.contains(clip.link_group_id.as_str()) {
            *marked = true;
        }
    }
}

/// ID で選んだ clip の位置に印を付ける。存在しない ID があれば `missing` で失敗する。
pub(crate) fn mark_clips(
    project: &Project,
    ids: &[String],
    missing: &str,
) -> ProjectResult<Vec<bool>> {
    let mut marked = vec![false; project.timeline_clips.len()];
    for id in ids {
        let index = project
            .clip_index(id)
            .ok_or_else(|| ProjectError::new(missing))?;
        marked[index] = true;
    }
    Ok(marked)
}

pub(crate) fn shift_start(clip: &mut TimelineClip, delta: i64) -> ProjectResult<()> {
    let Some(start) = clip
        .timeline_start_frame
        .checked_add(delta)
        .filter(|start| *start >= 0)
    else {
        return fail(format!("clip の移動先が範囲外です: {}", clip.name));
    };
    clip.timeline_start_frame = start;
    Ok(())
}

/// 編集後の candidate のトランジションと字幕のリンクを clip に合わせてから、timeline を検証する。
pub fn finalize_timeline_candidate(candidate: &mut Project) -> ProjectResult<i64> {
    reconcile_timeline_transitions(candidate);
    reconcile_subtitle_links(candidate);
    validate_timeline(candidate).map(|validation| validation.total_frames)
}

/// candidate を確定する。検証に失敗したら project を変えない。
pub(crate) fn commit_candidate(
    project: &mut Project,
    mut candidate: Project,
    selected: Option<usize>,
) -> EditResult {
    finalize_timeline_candidate(&mut candidate)?;
    *project = candidate;
    Ok(selected)
}

/// 時間の対応を変える clip の編集の確定。リンクした字幕を音声に合わせて置き直してから確定する。
pub(crate) fn commit_mapping_edit(
    project: &mut Project,
    mut candidate: Project,
    selected: Option<usize>,
) -> EditResult {
    remap_linked_subtitles(project, &mut candidate)?;
    commit_candidate(project, candidate, selected)
}

/// 右半分のように続きとして作った clip は Write を持たない（fade in と同じ）。
pub(crate) fn clear_math_intro(clip: &mut TimelineClip) {
    clip.math_animation = MathClipAnimation::default();
}

/// 分割・上書きで作った右側の clip に新しい ID と内部 ID を付ける。
pub(crate) fn assign_split_ids(
    original_id: &str,
    right: &mut TimelineClip,
    new_id: &mut dyn FnMut() -> String,
) -> ProjectResult<()> {
    right.id = new_id();
    if right.id.is_empty() || right.id == original_id {
        return fail("分割後の clip ID を作れません");
    }
    remap_internal_ids(right, new_id)
}

/// 数式 sequence・グラフの内部 ID を発行し直す（複製の両方が同じ ID を持たないように）。
pub(crate) fn remap_internal_ids(
    clip: &mut TimelineClip,
    new_id: &mut dyn FnMut() -> String,
) -> ProjectResult<()> {
    use crate::model::TimelineClipKind;
    match clip.kind {
        TimelineClipKind::EquationSequence => clip.equation_sequence.remap_ids(new_id),
        TimelineClipKind::Graph => clip.graph.remap_ids(new_id),
        _ => Ok(()),
    }
}
