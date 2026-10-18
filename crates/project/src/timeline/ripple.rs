//! 時間の挿入・削除と、それを使う編集（リップルトリム・ローリング・スリップ・スライド）。

use std::collections::{HashMap, HashSet};

use crate::error::{ProjectError, ProjectResult, fail};
use crate::model::{Project, TrackRef};
use crate::subtitles::{edit_subtitle_time, reconcile_subtitle_links};

use super::common::{
    EditResult, LinkMode, TrimEdge, adjacent_clip_index, clamp_fades_to_length, clear_math_intro,
    commit_candidate, commit_mapping_edit, edge_range, edit_targets, narrow_range,
    remap_internal_ids, shift_start, trim_clip_boundary,
};
use super::edit::{EdgeEditKind, clamped_edge_delta};
use super::mapping::{
    clip_interval, clip_source_boundary_to_timeline, clip_timeline_boundary_to_source,
};

/// 候補内でも衝突しない ID を作る。時間編集は呼び出し元の ID 生成器に依存しない。
fn time_edit_id(project: &Project, base: &str) -> String {
    let used = |id: &str| {
        project
            .timeline_clips
            .iter()
            .any(|clip| clip.id == id || clip.link_group_id == id)
            || project
                .subtitles
                .as_ref()
                .is_some_and(|track| track.cues.iter().any(|cue| cue.id == id))
    };
    (1_u64..)
        .map(|n| format!("{base}-time-{n}"))
        .find(|id| !used(id))
        .unwrap_or_default()
}

/// 全 track と字幕へ時間の挿入・削除を適用する（[start, start + removed) を消して inserted を挿入する）。
/// 区間をまたぐ素材は分割する。excluded の clip は動かさない。
pub(crate) fn time_edit_candidate(
    candidate: &mut Project,
    start: i64,
    removed: i64,
    inserted: i64,
    excluded: &HashSet<String>,
) -> ProjectResult<()> {
    let Some(end) = (start >= 0 && removed >= 0 && inserted >= 0)
        .then(|| start.checked_add(removed))
        .flatten()
    else {
        return fail("時間編集の区間が不正です");
    };
    if removed == 0 && inserted == 0 {
        return Ok(());
    }
    let inexact = || ProjectError::new("時間編集の境界を素材フレームで正確に表現できません");
    let mut output = Vec::with_capacity(candidate.timeline_clips.len());
    let mut right_groups: HashMap<String, String> = HashMap::new();
    // 分割した clip の系譜（元の ID → 右側の新しい ID と、時間編集後の右側の開始位置）。
    // 字幕は ID ではなくこの系譜で、右側の音声にあたる字幕を右側の clip へ付け替える。
    let mut lineage: Vec<(String, String, i64)> = Vec::new();
    let originals = candidate.timeline_clips.clone();
    for original in &originals {
        let (a, b) = clip_interval(candidate, original)?;
        if excluded.contains(&original.id) || b <= start {
            output.push(original.clone());
            continue;
        }
        if a < start {
            let mut left = original.clone();
            trim_clip_boundary(candidate, &mut left, TrimEdge::Right, start - b)?;
            if clip_interval(candidate, &left)? != (a, start) {
                return Err(inexact());
            }
            left.effects.fade_out_frames = 0;
            clamp_fades_to_length(&mut left);
            output.push(left);
        }
        if b <= end {
            continue;
        }
        let mut right = original.clone();
        if a < end {
            trim_clip_boundary(candidate, &mut right, TrimEdge::Left, end - a)?;
        }
        if clip_interval(candidate, &right)? != (a.max(end), b) {
            return Err(inexact());
        }
        if a < start {
            right.id = time_edit_id(candidate, &original.id);
            let mut serial = 0_u64;
            let base = right.id.clone();
            remap_internal_ids(&mut right, &mut || {
                serial += 1;
                format!("{base}-internal-{serial}")
            })?;
            // 同じリンクの両半分に同じ新規グループを割り当てる。
            if original.is_linked() {
                let group = right_groups
                    .entry(original.link_group_id.clone())
                    .or_insert_with(|| time_edit_id(candidate, &original.link_group_id));
                right.link_group_id = group.clone();
            }
            for transition in &mut candidate.timeline_transitions {
                if transition.outgoing_clip_id == original.id {
                    transition.outgoing_clip_id = right.id.clone();
                }
            }
        }
        let shifted =
            i128::from(right.timeline_start_frame) - i128::from(removed) + i128::from(inserted);
        let Some(shifted) = i64::try_from(shifted).ok().filter(|start| *start >= 0) else {
            return fail("クリップの移動先が範囲外です");
        };
        right.timeline_start_frame = shifted;
        if a < end {
            right.effects.fade_in_frames = 0;
            clear_math_intro(&mut right);
            clamp_fades_to_length(&mut right);
        }
        if right.id != original.id {
            lineage.push((
                original.id.clone(),
                right.id.clone(),
                right.timeline_start_frame,
            ));
        }
        output.push(right);
    }
    candidate.timeline_clips = output;
    // 削除区間に丸ごと入って消えた clip へのリンクを外す（字幕は残す）。字幕の時間編集は検証まで行うので、
    // その前に外さないと、区間の外へはみ出した字幕が 1 つあるだけで時間編集全体が失敗する。
    reconcile_subtitle_links(candidate);
    let mut counts: HashMap<String, usize> = HashMap::new();
    for clip in candidate
        .timeline_clips
        .iter()
        .filter(|clip| clip.is_linked())
    {
        *counts.entry(clip.link_group_id.clone()).or_default() += 1;
    }
    for clip in &mut candidate.timeline_clips {
        if clip.is_linked() && counts.get(&clip.link_group_id) != Some(&2) {
            clip.link_group_id.clear();
        }
    }
    let mut cue_serial = 0_u64;
    let snapshot = candidate.clone();
    edit_subtitle_time(candidate, start, removed, inserted, &mut || {
        cue_serial += 1;
        time_edit_id(&snapshot, &format!("subtitle-ripple-{cue_serial}"))
    })?;
    // 時間編集は境界を跨ぐ字幕も境界で分けているので、右側の clip の開始以降に始まる字幕が右側の音声にあたる。
    if let Some(track) = candidate.subtitles.as_mut() {
        for (original_id, right_id, right_start) in &lineage {
            for cue in &mut track.cues {
                if cue.link_clip_id == *original_id && cue.start_frame >= *right_start {
                    cue.link_clip_id = right_id.clone();
                }
            }
        }
    }
    Ok(())
}

/// 全 track と字幕へ時間の挿入・削除を適用して確定する。
pub fn edit_timeline_time(
    project: &mut Project,
    start: i64,
    removed: i64,
    inserted: i64,
) -> EditResult {
    let mut candidate = project.clone();
    time_edit_candidate(&mut candidate, start, removed, inserted, &HashSet::new())?;
    commit_candidate(project, candidate, None)
}

/// 操作した clip の元の終端 `boundary` で、targets 以外の全 track を `shift` だけずらす。
pub(crate) fn shift_following_clips(
    candidate: &mut Project,
    boundary: i64,
    targets: &[usize],
    shift: i64,
) -> ProjectResult<()> {
    if shift == 0 {
        return Ok(());
    }
    let excluded: HashSet<String> = targets
        .iter()
        .map(|target| candidate.timeline_clips[*target].id.clone())
        .collect();
    if shift < 0 {
        time_edit_candidate(candidate, boundary + shift, -shift, 0, &excluded)
    } else {
        time_edit_candidate(candidate, boundary, 0, shift, &excluded)
    }
}

/// リップルトリム。尺の増減を全 track と字幕へ波及させる。left 端を trim しても clip の開始位置は動かない。
/// Linked ならリンク相手も同じ量 trim し、相手の track の後ろもずらす。
pub fn ripple_trim_timeline_clip(
    project: &mut Project,
    clip_id: &str,
    edge: TrimEdge,
    delta: i64,
    link_mode: LinkMode,
) -> EditResult {
    if project.clip_index(clip_id).is_none() {
        return fail("リップルトリムする timeline clip がありません");
    }
    let mut candidate = project.clone();
    let delta = clamped_edge_delta(
        &candidate,
        clip_id,
        edge,
        EdgeEditKind::Ripple,
        delta,
        link_mode,
    )?;
    let Some(index) = candidate.clip_index(clip_id) else {
        return fail("リップルトリムする timeline clip がありません");
    };
    let targets = edit_targets(&candidate, index, link_mode);
    // 操作対象の尺を変えてから、元の終端で全 track へ時間の増減を波及させる。
    let mut boundary = None;
    let mut shift = 0;
    for (order, target) in targets.iter().enumerate() {
        let mut clip = candidate.timeline_clips[*target].clone();
        let (original_start, original_end) = clip_interval(&candidate, &clip)?;
        trim_clip_boundary(&candidate, &mut clip, edge, delta)?;
        // left 端を動かしても clip は元の開始位置に留め、尺の変化を後ろへ波及させる。
        clip.timeline_start_frame = original_start;
        let (_, new_end) = clip_interval(&candidate, &clip)?;
        // リンク相手と後ろのずれ方が違うと同期が崩れる。素材 fps の丸めで食い違うなら拒否する。
        if order == 0 {
            shift = new_end - original_end;
            boundary = Some(original_end);
        } else if new_end - original_end != shift {
            return fail("リンク相手と尺の変化量が一致しないためリップルトリムできません");
        }
        candidate.timeline_clips[*target] = clip;
    }
    if let Some(boundary) = boundary {
        shift_following_clips(&mut candidate, boundary, &targets, shift)?;
    }
    let selected = candidate.clip_index(clip_id);
    commit_mapping_edit(project, candidate, selected)
}

/// outgoing（edge = Right なら clip 自身）と incoming の境界を動かす。接している clip が無ければ `false`。
fn roll_edit_point(
    candidate: &mut Project,
    index: usize,
    edge: TrimEdge,
    delta: i64,
) -> ProjectResult<bool> {
    let Some(neighbor) = adjacent_clip_index(candidate, index, edge)? else {
        return Ok(false);
    };
    let (outgoing_index, incoming_index) = match edge {
        TrimEdge::Right => (index, neighbor),
        TrimEdge::Left => (neighbor, index),
    };
    let mut outgoing = candidate.timeline_clips[outgoing_index].clone();
    let mut incoming = candidate.timeline_clips[incoming_index].clone();
    trim_clip_boundary(candidate, &mut outgoing, TrimEdge::Right, delta)?;
    trim_clip_boundary(candidate, &mut incoming, TrimEdge::Left, delta)?;
    let (_, outgoing_end) = clip_interval(candidate, &outgoing)?;
    let (incoming_start, _) = clip_interval(candidate, &incoming)?;
    // 素材 fps が timeline と異なると frame 境界の丸めで隙間ができうる。合計尺を保つという契約を守れないので
    // 黙って受理しない。
    if outgoing_end != incoming_start {
        return fail("境界を素材 frame に揃えられないためローリング編集できません");
    }
    candidate.timeline_clips[outgoing_index] = outgoing;
    candidate.timeline_clips[incoming_index] = incoming;
    Ok(true)
}

/// ローリング編集。clip の edge と接している隣の clip の境界を一緒に動かし、2 clip の合計尺を保つ。
/// 接している clip が無ければ失敗する。Linked ならリンク相手の編集点も動かす（相手が編集点を持たない
/// L / J カットなら動かさない）。
pub fn roll_timeline_edit(
    project: &mut Project,
    clip_id: &str,
    edge: TrimEdge,
    delta: i64,
    link_mode: LinkMode,
) -> EditResult {
    let Some(index) = project.clip_index(clip_id) else {
        return fail("ローリング編集する timeline clip がありません");
    };
    let mut candidate = project.clone();
    let delta = clamped_edge_delta(
        &candidate,
        clip_id,
        edge,
        EdgeEditKind::Roll,
        delta,
        link_mode,
    )?;
    for (order, target) in edit_targets(&candidate, index, link_mode)
        .into_iter()
        .enumerate()
    {
        let found = roll_edit_point(&mut candidate, target, edge, delta)?;
        if order == 0 && !found {
            return fail("ローリング編集には接している隣の clip が必要です");
        }
    }
    commit_mapping_edit(project, candidate, Some(index))
}

/// clip の in / out を timeline 上で動かせる範囲 [lower, upper]（project frame）。
fn slip_range(project: &Project, clip: &crate::model::TimelineClip) -> ProjectResult<(i64, i64)> {
    let fps = project.timeline_fps;
    let in_frame = clip_source_boundary_to_timeline(clip, clip.source_in_frame, fps)?;
    let out_frame = clip_source_boundary_to_timeline(clip, clip.source_out_frame, fps)?;
    let end_frame = clip_source_boundary_to_timeline(clip, clip.source_frame_count, fps)?;
    Ok((-in_frame, end_frame - out_frame))
}

/// in / out を timeline 上で delta だけずらす。素材上の長さ（out - in）は保ち、素材の範囲を超える分は端で止める。
/// in が動いたら `true`。
fn slip_clip_source(
    project: &Project,
    clip: &mut crate::model::TimelineClip,
    delta: i64,
) -> ProjectResult<bool> {
    let in_boundary =
        clip_source_boundary_to_timeline(clip, clip.source_in_frame, project.timeline_fps)?;
    let Some(shifted) = in_boundary.checked_add(delta) else {
        return fail("スリップ量が範囲外です");
    };
    // 素材の先頭より前は存在しないので、timeline 境界の段階で 0 に丸めておく。
    let shifted_in = clip_timeline_boundary_to_source(clip, shifted.max(0), project.timeline_fps)?;
    // 素材 fps が timeline と異なる場合、timeline 上の尺は丸めで 1 frame 変わりうる。重なりは検証が拒否する。
    let length = clip.source_duration();
    let new_in = shifted_in.clamp(0, clip.source_frame_count - length);
    let moved = new_in != clip.source_in_frame;
    clip.source_in_frame = new_in;
    clip.source_out_frame = new_in + length;
    Ok(moved)
}

/// スリップ。timeline 上の位置と素材上の長さを保ったまま in/out を同じだけずらす。素材の範囲を超える分は端で
/// 止める。1 frame もずらせなければ失敗する。Linked ならリンク相手も同じ量ずらし、全員がずらせる範囲で止める。
pub fn slip_timeline_clip(
    project: &mut Project,
    clip_id: &str,
    delta: i64,
    link_mode: LinkMode,
) -> EditResult {
    let Some(index) = project.clip_index(clip_id) else {
        return fail("スリップする timeline clip がありません");
    };
    let mut candidate = project.clone();
    if candidate.timeline_clips[index].has_synthetic_source_domain() {
        return fail("text / image clip は素材位置を持たないためスリップできません");
    }
    let targets = edit_targets(&candidate, index, link_mode);
    // リンク相手と同じ量だけずらさないと同期が崩れる。全員がずらせる範囲へ先に丸める。
    let mut range = (i64::MIN, i64::MAX);
    for target in &targets {
        narrow_range(
            &mut range,
            slip_range(&candidate, &candidate.timeline_clips[*target])?,
        );
    }
    let delta = delta.clamp(range.0.min(range.1), range.1);
    let mut any_moved = false;
    for target in &targets {
        let mut clip = candidate.timeline_clips[*target].clone();
        any_moved |= slip_clip_source(&candidate, &mut clip, delta)?;
        candidate.timeline_clips[*target] = clip;
    }
    if !any_moved {
        return fail("素材の端に達しているためスリップできません");
    }
    commit_mapping_edit(project, candidate, Some(index))
}

/// スライド量を、前の clip の out・後ろの clip の in を動かせる範囲（素材の範囲と 1 frame 以上の尺）、
/// timeline 先頭、接していない clip との空白に止めた値にする。Linked ならリンク相手の制約も含める。
/// 操作した clip に前後の clip が無ければ失敗する。確定と drag 中の表示の両方が使う。
pub fn clamp_slide_edit(
    project: &Project,
    clip_id: &str,
    delta: i64,
    link_mode: LinkMode,
) -> ProjectResult<i64> {
    let Some(index) = project.clip_index(clip_id) else {
        return fail("スライドする timeline clip がありません");
    };
    let targets = edit_targets(project, index, link_mode);
    let mut range = (i64::MIN, i64::MAX);
    for target in &targets {
        let clip = &project.timeline_clips[*target];
        let (start, end) = clip_interval(project, clip)?;
        let previous = adjacent_clip_index(project, *target, TrimEdge::Left)?;
        let next = adjacent_clip_index(project, *target, TrimEdge::Right)?;
        // スライドは前後の編集点を保ったまま中身だけを動かす編集である。操作した clip に接している前後の clip が
        // 無ければ単なる移動になるので拒否する。リンク相手は L / J カットで編集点を持たないことがある。
        if *target == index && (previous.is_none() || next.is_none()) {
            return fail(
                "スライドには前後に接している clip が必要です (移動は選択ツールで行ってください)",
            );
        }
        range.0 = range.0.max(-start);
        // 前の clip は out を、後ろの clip は in を同じ量だけ動かす。
        if let Some(previous) = previous {
            narrow_range(
                &mut range,
                edge_range(project, &project.timeline_clips[previous], TrimEdge::Right)?,
            );
        }
        if let Some(next) = next {
            narrow_range(
                &mut range,
                edge_range(project, &project.timeline_clips[next], TrimEdge::Left)?,
            );
        }
        // 接していない同じ track の clip とは、間の空白の分だけしか動けない。
        for (other, blocker) in project.timeline_clips.iter().enumerate() {
            if Some(other) == previous
                || Some(other) == next
                || targets.contains(&other)
                || blocker.track != clip.track
            {
                continue;
            }
            let (blocker_start, blocker_end) = clip_interval(project, blocker)?;
            if blocker_end <= start {
                range.0 = range.0.max(blocker_end - start);
            } else if blocker_start >= end {
                range.1 = range.1.min(blocker_start - end);
            }
        }
    }
    Ok(if range.0 > range.1 {
        0
    } else {
        delta.clamp(range.0, range.1)
    })
}

/// スライド。clip を尺を変えずに横へ動かし、接している前後の clip の out / in を追従させる。
pub fn slide_timeline_clip(
    project: &mut Project,
    clip_id: &str,
    delta: i64,
    link_mode: LinkMode,
) -> EditResult {
    let Some(index) = project.clip_index(clip_id) else {
        return fail("スライドする timeline clip がありません");
    };
    let mut candidate = project.clone();
    let delta = match clamp_slide_edit(&candidate, clip_id, delta, link_mode)? {
        0 => return fail("前後の clip の素材の端に達しているため、これ以上スライドできません"),
        clamped => clamped,
    };
    let targets = edit_targets(&candidate, index, link_mode);
    // 隣接関係は移動前の配置で決める。全部決めてから書き換える。
    let mut neighbors = Vec::with_capacity(targets.len());
    for target in &targets {
        neighbors.push((
            *target,
            adjacent_clip_index(&candidate, *target, TrimEdge::Left)?,
            adjacent_clip_index(&candidate, *target, TrimEdge::Right)?,
        ));
    }
    neighbors.sort_by_key(|(target, _, _)| *target);
    for (target, previous, next) in neighbors {
        let mut clip = candidate.timeline_clips[target].clone();
        shift_start(&mut clip, delta)?;
        candidate.timeline_clips[target] = clip;
        if let Some(previous) = previous.filter(|previous| !targets.contains(previous)) {
            let mut clip = candidate.timeline_clips[previous].clone();
            trim_clip_boundary(&candidate, &mut clip, TrimEdge::Right, delta)?;
            candidate.timeline_clips[previous] = clip;
        }
        if let Some(next) = next.filter(|next| !targets.contains(next)) {
            let mut clip = candidate.timeline_clips[next].clone();
            trim_clip_boundary(&candidate, &mut clip, TrimEdge::Left, delta)?;
            candidate.timeline_clips[next] = clip;
        }
    }
    commit_mapping_edit(project, candidate, Some(index))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SelectDirection {
    Forward,
    Backward,
}

/// トラックの選択ツール。Forward は frame 以降に掛かる clip（end > frame）、Backward は frame 以前に掛かる clip
/// （start <= frame）。track を指定するとその track だけに絞る。
pub fn clip_ids_from_frame(
    project: &Project,
    frame: i64,
    direction: SelectDirection,
    track: Option<TrackRef>,
) -> Vec<String> {
    project
        .timeline_clips
        .iter()
        .filter(|clip| track.is_none_or(|track| clip.track == track))
        .filter(|clip| {
            clip_interval(project, clip).is_ok_and(|(start, end)| match direction {
                SelectDirection::Forward => end > frame,
                SelectDirection::Backward => start <= frame,
            })
        })
        .map(|clip| clip.id.clone())
        .collect()
}
