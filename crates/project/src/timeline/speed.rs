//! 速度を変える編集（レート調整・速度と尺の指定・フレーム保持）。
//!
//! 尺 D にしたい clip の速度は s = (out - in) R / D（R = timeline fps / 素材 fps）とする。このとき実効 fps は
//! (out - in) timeline fps / D で、尺はちょうど D になる。

use mvm_core::Rational;

use crate::clip_effects::{
    ClipKeyKind, evaluate_clip_opacity, reframe_clip_keys, rescale_clip_keys,
};
use crate::error::{ProjectResult, fail};
use crate::model::{
    FrameHold, MAX_CLIP_SPEED_PERCENT, MIN_CLIP_SPEED_PERCENT, Project, TimelineClipKind, TrackRef,
};
use crate::subtitles::edit_subtitle_time;

use super::common::{
    EditResult, LinkMode, TrimEdge, commit_candidate, commit_mapping_edit, edit_targets,
    finalize_timeline_candidate, shift_start, trim_clip_boundary,
};
use super::edit::{clip_ids_spanning_frame, remove_clips_and_unlink, split_timeline_clips};
use super::mapping::{
    clip_fade_source_frame_at, clip_interval, clip_source_frame_at, timeline_clip_duration,
};
use super::ripple::shift_following_clips;

/// 尺を `new_duration` にする速度 (out - in) R / D を、約分した有理数で返す。
fn speed_for_duration(
    project: &Project,
    clip: &crate::model::TimelineClip,
    new_duration: i64,
) -> Option<Rational> {
    if new_duration <= 0 {
        return None;
    }
    // (out - in) tlNum srcDen / (tlDen srcNum D)
    let num = i128::from(clip.source_duration())
        .checked_mul(i128::from(project.timeline_fps.num))?
        .checked_mul(i128::from(clip.source_fps.den))?;
    let den = i128::from(project.timeline_fps.den)
        .checked_mul(i128::from(clip.source_fps.num))?
        .checked_mul(i128::from(new_duration))?;
    if num <= 0 || den <= 0 {
        return None;
    }
    let (mut a, mut b) = (num, den);
    while b != 0 {
        (a, b) = (b, a % b);
    }
    Some(Rational::new(
        i64::try_from(num / a).ok()?,
        i64::try_from(den / a).ok()?,
    ))
}

const LINKED_SPEED_MISMATCH: &str =
    "リンク相手と速度が違うため、一緒にレート調整できません (Alt で片方だけ調整できます)";

/// 対象 clip へ速度を適用する。left 端なら各 clip の終端を、right 端なら開始位置を保つ。key は尺に合わせて伸縮する。
fn apply_clip_speed(
    candidate: &mut Project,
    index: usize,
    edge: TrimEdge,
    speed: Rational,
    link_mode: LinkMode,
) -> ProjectResult<()> {
    let original = candidate.timeline_clips[index].speed;
    for target in edit_targets(candidate, index, link_mode) {
        let mut clip = candidate.timeline_clips[target].clone();
        if clip.speed != original {
            return fail(LINKED_SPEED_MISMATCH);
        }
        let before = timeline_clip_duration(candidate, &clip)?;
        clip.speed = speed;
        // 等速では伸縮しないのでピッチ保持は意味を持たない（検証が拒否する）。
        if speed.num == speed.den {
            clip.preserve_pitch = false;
        }
        let after = timeline_clip_duration(candidate, &clip)?;
        if edge == TrimEdge::Left {
            clip.timeline_start_frame = clip.timeline_start_frame + before - after;
        }
        clip.effects
            .for_each_keys_mut(|_, keys| rescale_clip_keys(keys, before, after));
        candidate.timeline_clips[target] = clip;
    }
    Ok(())
}

fn apply_rate_stretch(
    candidate: &mut Project,
    index: usize,
    edge: TrimEdge,
    new_duration: i64,
    link_mode: LinkMode,
) -> ProjectResult<()> {
    let Some(speed) = speed_for_duration(candidate, &candidate.timeline_clips[index], new_duration)
    else {
        return fail("この尺にする速度を表せません");
    };
    apply_clip_speed(candidate, index, edge, speed, link_mode)
}

/// 尺 D で伸縮した candidate が成り立つか。判定は検証に一本化する
/// （速度の範囲、1 frame 以上の尺、timeline 先頭、同じ track の重なり）。
fn rate_stretch_feasible(
    project: &Project,
    index: usize,
    edge: TrimEdge,
    new_duration: i64,
    link_mode: LinkMode,
) -> bool {
    let mut candidate = project.clone();
    apply_rate_stretch(&mut candidate, index, edge, new_duration, link_mode).is_ok()
        && finalize_timeline_candidate(&mut candidate).is_ok()
}

/// 端のドラッグ量（project frame）を、伸縮できる範囲で止めた値にする。止める条件は速度の範囲、1 frame 以上の尺、
/// 同じ track の隣の clip、timeline 先頭で、Linked ならリンク相手の条件も含める。確定と drag 中の表示の両方が使う。
pub fn clamp_rate_edit(
    project: &Project,
    clip_id: &str,
    edge: TrimEdge,
    delta: i64,
    link_mode: LinkMode,
) -> ProjectResult<i64> {
    let Some(index) = project.clip_index(clip_id) else {
        return fail("レート調整する timeline clip がありません");
    };
    let clip = &project.timeline_clips[index];
    if clip.has_synthetic_source_domain() {
        return fail("text / image clip の尺は trim で変更してください");
    }
    if edit_targets(project, index, link_mode)
        .iter()
        .any(|target| project.timeline_clips[*target].speed != clip.speed)
    {
        return fail(LINKED_SPEED_MISMATCH);
    }
    let duration = timeline_clip_duration(project, clip)?;
    // right 端は右へ動かすと伸び、left 端は左へ動かすと伸びる。
    let duration_for = |delta: i64| -> i64 {
        let signed = match edge {
            TrimEdge::Right => i128::from(delta),
            TrimEdge::Left => -i128::from(delta),
        };
        let value = i128::from(duration) + signed;
        if value < 1 {
            0
        } else {
            i64::try_from(value.min(i128::from(i64::MAX / 4))).unwrap_or(i64::MAX / 4)
        }
    };
    let feasible = |delta: i64| {
        let new_duration = duration_for(delta);
        new_duration > 0 && rate_stretch_feasible(project, index, edge, new_duration, link_mode)
    };
    // 伸縮できる範囲は現在の尺（delta 0）から要求した向きに連続している（各条件は尺の上限か下限なので）。
    // 成り立つ最も遠い量を二分探索で求める。
    if delta != 0 && feasible(delta) {
        return Ok(delta);
    }
    let (mut reachable, mut blocked) = (0_i64, delta);
    while reachable.abs_diff(blocked) > 1 {
        let middle = reachable + (blocked - reachable) / 2;
        if feasible(middle) {
            reachable = middle;
        } else {
            blocked = middle;
        }
    }
    Ok(reachable)
}

/// clamp_rate_edit で止めた量で伸縮した candidate を作る。表示と確定の両方がこれを使う。
fn rate_stretch_candidate(
    project: &Project,
    clip_id: &str,
    edge: TrimEdge,
    delta: i64,
    link_mode: LinkMode,
) -> ProjectResult<(Project, usize, i64)> {
    let applied = clamp_rate_edit(project, clip_id, edge, delta, link_mode)?;
    let mut candidate = project.clone();
    let Some(index) = candidate.clip_index(clip_id) else {
        return fail("レート調整する timeline clip がありません");
    };
    if applied == 0 {
        return Ok((candidate, index, applied));
    }
    let duration = timeline_clip_duration(&candidate, &candidate.timeline_clips[index])?;
    let new_duration = match edge {
        TrimEdge::Right => duration + applied,
        TrimEdge::Left => duration - applied,
    };
    apply_rate_stretch(&mut candidate, index, edge, new_duration, link_mode)?;
    Ok((candidate, index, applied))
}

/// drag 中の表示の 1 clip 分。確定と同じ candidate から作るので、リンク相手の尺が操作した clip と違っても
/// 表示と確定が食い違わない。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RateStretchPreviewClip {
    pub clip_id: String,
    pub timeline_start_frame: i64,
    pub duration_frames: i64,
    pub speed: Rational,
    /// 開始 / 終端が今の位置から動く frame 数。
    pub start_delta: i64,
    pub end_delta: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RateStretchPreview {
    /// 操作した clip の端を実際に動かす量（clamp_rate_edit の結果）。
    pub applied_delta: i64,
    pub clips: Vec<RateStretchPreviewClip>,
}

pub fn preview_rate_stretch(
    project: &Project,
    clip_id: &str,
    edge: TrimEdge,
    delta: i64,
    link_mode: LinkMode,
) -> ProjectResult<RateStretchPreview> {
    let (candidate, index, applied_delta) =
        rate_stretch_candidate(project, clip_id, edge, delta, link_mode)?;
    let mut clips = Vec::new();
    for target in edit_targets(&candidate, index, link_mode) {
        let clip = &candidate.timeline_clips[target];
        let before = &project.timeline_clips[target];
        let duration = timeline_clip_duration(&candidate, clip)?;
        let before_duration = timeline_clip_duration(project, before)?;
        clips.push(RateStretchPreviewClip {
            clip_id: clip.id.clone(),
            timeline_start_frame: clip.timeline_start_frame,
            duration_frames: duration,
            speed: clip.speed,
            start_delta: clip.timeline_start_frame - before.timeline_start_frame,
            end_delta: (clip.timeline_start_frame + duration)
                - (before.timeline_start_frame + before_duration),
        });
    }
    Ok(RateStretchPreview {
        applied_delta,
        clips,
    })
}

/// レート調整。端をドラッグして、素材範囲（in/out）を変えずに速度を変えて尺を伸縮する。反対側の端は動かさず、
/// リップルも上書きもしない。Linked ならリンク相手にも同じ速度を適用する（相手の速度が違えば失敗する）。
pub fn rate_stretch_timeline_clip(
    project: &mut Project,
    clip_id: &str,
    edge: TrimEdge,
    delta: i64,
    link_mode: LinkMode,
) -> EditResult {
    let (candidate, index, applied) =
        rate_stretch_candidate(project, clip_id, edge, delta, link_mode)?;
    if applied == 0 {
        return fail(format!(
            "速度の範囲 ({MIN_CLIP_SPEED_PERCENT}%〜{MAX_CLIP_SPEED_PERCENT}%) または隣の clip に達しているため、これ以上伸縮できません"
        ));
    }
    commit_mapping_edit(project, candidate, Some(index))
}

/// 速度と尺の指定のどちらで決めるか。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SpeedDurationInput {
    Speed,
    Duration,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ClipSpeedDurationEdit {
    pub input: SpeedDurationInput,
    pub speed: Rational,
    pub duration_frames: i64,
    pub preserve_pitch: bool,
    pub ripple: bool,
    /// ripple しないで延びた先にある clip を上書きする。開始位置は固定なので、重なるのは元の終端以降に始まる
    /// clip だけであり、丸ごと覆えば削除、はみ出せば左端を削る。
    pub overwrite: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ClipSpeedDurationPreview {
    pub duration_frames: i64,
    pub speed: Rational,
}

/// 速度・尺の変更の失敗。`overlaps_following` は ripple も overwrite もしないとき、延びた先の clip と重なるために
/// 失敗したか（UI はこれを見て上書きの確認を出す）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SpeedDurationError {
    pub error: crate::ProjectError,
    pub overlaps_following: bool,
}

impl From<crate::ProjectError> for SpeedDurationError {
    fn from(error: crate::ProjectError) -> Self {
        Self {
            error,
            overlaps_following: false,
        }
    }
}

/// 伸ばした targets の区間に掛かる、同じ track の targets 以外の clip の ID。
fn clips_covered_by_targets(candidate: &Project, targets: &[usize]) -> ProjectResult<Vec<String>> {
    let mut covered = Vec::new();
    for target in targets {
        let clip = &candidate.timeline_clips[*target];
        let (start, end) = clip_interval(candidate, clip)?;
        for (other, following) in candidate.timeline_clips.iter().enumerate() {
            if targets.contains(&other) || following.track != clip.track {
                continue;
            }
            let (other_start, other_end) = clip_interval(candidate, following)?;
            if other_start < end && other_end > start {
                covered.push(following.id.clone());
            }
        }
    }
    Ok(covered)
}

/// covered を targets の終端まで上書きする。丸ごと覆われた clip は消し、リンク相手は未リンクにする。
/// はみ出す clip は左端を targets の終端まで削る（effect は残す。縮めた尺に収まらない fade だけ詰める）。
fn overwrite_covered_clips(
    candidate: &mut Project,
    targets: &[usize],
    covered: &[String],
) -> ProjectResult<()> {
    let ends: Vec<(TrackRef, i64)> = targets
        .iter()
        .map(|target| {
            let clip = &candidate.timeline_clips[*target];
            clip_interval(candidate, clip).map(|(_, end)| (clip.track, end))
        })
        .collect::<ProjectResult<_>>()?;
    let mut removed = Vec::new();
    for id in covered {
        let Some(index) = candidate.clip_index(id) else {
            continue;
        };
        let mut clip = candidate.timeline_clips[index].clone();
        let (start, end) = clip_interval(candidate, &clip)?;
        let Some((_, cover_end)) = ends.iter().find(|(track, _)| *track == clip.track) else {
            continue;
        };
        if end <= *cover_end {
            removed.push(id.clone());
            continue;
        }
        trim_clip_boundary(candidate, &mut clip, TrimEdge::Left, cover_end - start)?;
        // 上書きは尺を縮めるだけで effect は消さない。末尾は動かないので fade out を優先して残す。
        let length = clip.source_duration();
        clip.effects.fade_out_frames = clip.effects.fade_out_frames.min(length);
        clip.effects.fade_in_frames = clip
            .effects
            .fade_in_frames
            .min(length - clip.effects.fade_out_frames);
        candidate.timeline_clips[index] = clip;
    }
    remove_clips_and_unlink(candidate, &removed);
    Ok(())
}

fn speed_duration_candidate(
    project: &Project,
    clip_id: &str,
    edit: &ClipSpeedDurationEdit,
    link_mode: LinkMode,
) -> Result<(Project, usize), SpeedDurationError> {
    let mut candidate = project.clone();
    let Some(index) = candidate.clip_index(clip_id) else {
        return Err(crate::ProjectError::new("速度を変更する clip がありません").into());
    };
    let targets = edit_targets(&candidate, index, link_mode);
    let old_ends: Vec<i64> = targets
        .iter()
        .map(|target| {
            clip_interval(&candidate, &candidate.timeline_clips[*target]).map(|(_, end)| end)
        })
        .collect::<ProjectResult<_>>()?;
    let operated = candidate.timeline_clips[index].clone();
    if operated.has_synthetic_source_domain() {
        if edit.input != SpeedDurationInput::Duration
            || edit.preserve_pitch
            || edit.duration_frames < 1
        {
            return Err(crate::ProjectError::new("静止 clip は尺だけ変更できます").into());
        }
        let before = timeline_clip_duration(&candidate, &operated)?;
        let mut clip = operated.clone();
        trim_clip_boundary(
            &candidate,
            &mut clip,
            TrimEdge::Right,
            edit.duration_frames - before,
        )?;
        candidate.timeline_clips[index] = clip;
    } else {
        let speed = match edit.input {
            SpeedDurationInput::Duration => {
                speed_for_duration(&candidate, &operated, edit.duration_frames)
                    .ok_or_else(|| crate::ProjectError::new("この尺にする速度を表せません"))?
            }
            SpeedDurationInput::Speed => edit.speed,
        };
        let Some(speed) = speed.reduced() else {
            return Err(crate::ProjectError::new("速度が不正です").into());
        };
        apply_clip_speed(&mut candidate, index, TrimEdge::Right, speed, link_mode)?;
        // 等速ではピッチ保持を持たせない。preview と書き出しの両方が 1/1 では伸縮しない。
        for target in &targets {
            candidate.timeline_clips[*target].preserve_pitch =
                edit.preserve_pitch && speed.num != speed.den;
        }
    }
    let mut shift = 0;
    for (order, target) in targets.iter().enumerate() {
        let (_, end) = clip_interval(&candidate, &candidate.timeline_clips[*target])?;
        let delta = end - old_ends[order];
        if order == 0 {
            shift = delta;
        } else if shift != delta {
            return Err(crate::ProjectError::new("リンク相手と尺の変化量が一致しません").into());
        }
    }
    if edit.ripple && shift != 0 {
        shift_following_clips(&mut candidate, old_ends[0], &targets, shift)?;
    }
    if !edit.ripple && shift > 0 {
        let covered = clips_covered_by_targets(&candidate, &targets)?;
        if !covered.is_empty() {
            if !edit.overwrite {
                return Err(SpeedDurationError {
                    error: crate::ProjectError::new("後続の clip と重なります"),
                    overlaps_following: true,
                });
            }
            overwrite_covered_clips(&mut candidate, &targets, &covered)?;
        }
    }
    // 削除で後ろの index がずれるので、操作した clip を ID で引き直す。
    let Some(index) = candidate.clip_index(clip_id) else {
        return Err(crate::ProjectError::new("速度を変更する clip がありません").into());
    };
    if candidate == *project {
        return Err(crate::ProjectError::new("変更がありません").into());
    }
    finalize_timeline_candidate(&mut candidate)?;
    Ok((candidate, index))
}

pub fn preview_clip_speed_duration(
    project: &Project,
    clip_id: &str,
    edit: &ClipSpeedDurationEdit,
    link_mode: LinkMode,
) -> Result<ClipSpeedDurationPreview, SpeedDurationError> {
    let (candidate, index) = speed_duration_candidate(project, clip_id, edit, link_mode)?;
    let clip = &candidate.timeline_clips[index];
    Ok(ClipSpeedDurationPreview {
        duration_frames: timeline_clip_duration(&candidate, clip)?,
        speed: clip.speed,
    })
}

pub fn set_clip_speed_duration(
    project: &mut Project,
    clip_id: &str,
    edit: &ClipSpeedDurationEdit,
    link_mode: LinkMode,
) -> EditResult {
    let (candidate, index) =
        speed_duration_candidate(project, clip_id, edit, link_mode).map_err(|error| error.error)?;
    commit_mapping_edit(project, candidate, Some(index))
}

/// 再生ヘッドの位置にフレーム保持を挿入する。保持中もモーションは継続し、保持後の右側 clip は保持尺だけ進んだ
/// 位置から再開する（動きは timeline の時間に付き、映像だけが止まる）。不透明度は挿入位置の値（key と fade 込み）
/// を保持 clip の基本値へ焼き込む。
pub fn insert_frame_hold(
    project: &mut Project,
    clip_id: &str,
    frame: i64,
    hold_frames: i64,
    new_id: &mut dyn FnMut() -> String,
) -> EditResult {
    let Some(original) = project.clip(clip_id).filter(|_| hold_frames >= 1).cloned() else {
        return fail("保持する映像 clip または尺が不正です");
    };
    let (start, end) = clip_interval(project, &original)?;
    if original.kind != TimelineClipKind::Video
        || original.frame_hold.is_some()
        || frame <= start
        || frame >= end
    {
        return fail("再生ヘッドは通常の映像 clip の内側に置いてください");
    }
    let fps = project.timeline_fps;
    let source_frame = clip_source_frame_at(&original, fps, frame - start)?;
    let fade_frame = clip_fade_source_frame_at(&original, fps, frame - start)?;
    let held_opacity_percent = 100.0
        * evaluate_clip_opacity(
            &original.effects,
            frame - start,
            fade_frame,
            original.source_duration(),
        );
    let mut candidate = project.clone();
    let spanning = clip_ids_spanning_frame(&candidate, frame);
    split_timeline_clips(&mut candidate, &spanning, frame, new_id, LinkMode::Linked)?;
    // 右側は分割で元の動きを切り出してある。その続きを保持が消費するので、右側は元の動きを保持尺だけ先へ進めて
    // 取り直す。そうしないと保持の終わりから右側の先頭へ戻る。
    let Some(right) = candidate.timeline_clips.iter_mut().find(|clip| {
        clip.id != clip_id && clip.track == original.track && clip.timeline_start_frame == frame
    }) else {
        return fail("保持位置の右側の clip がありません");
    };
    let motion_kinds = || {
        crate::clip_effects::EFFECT_CHANNELS
            .iter()
            .map(|channel| channel.kind)
            .filter(|kind| *kind != ClipKeyKind::Opacity && !kind.is_audio())
    };
    for kind in motion_kinds() {
        let keys = right.effects.keys_mut(kind);
        *keys = original.effects.keys(kind).clone();
        reframe_clip_keys(keys, end - start, end - frame, frame - start + hold_frames);
    }
    for clip in &mut candidate.timeline_clips {
        if clip.timeline_start_frame >= frame {
            shift_start(clip, hold_frames)?;
        }
    }
    // 保持は保持位置への時間の挿入でもある。字幕も clip と同じだけ後ろへ送る。
    edit_subtitle_time(&mut candidate, frame, 0, hold_frames, new_id)?;
    let Some(mut hold) = candidate.clip(clip_id).cloned() else {
        return fail("保持する映像 clip がありません");
    };
    hold.id = new_id();
    hold.name.push_str(" (保持)");
    hold.link_group_id.clear();
    hold.timeline_start_frame = frame;
    hold.source_fps = fps;
    hold.source_frame_count = hold_frames;
    hold.source_in_frame = 0;
    hold.source_out_frame = hold_frames;
    hold.speed = Rational::new(1, 1);
    hold.preserve_pitch = false;
    hold.frame_hold = Some(FrameHold {
        source_frame,
        source_fps: original.source_fps,
        source_frame_count: original.source_frame_count,
        speed: original.speed,
    });
    for kind in motion_kinds() {
        let keys = hold.effects.keys_mut(kind);
        *keys = original.effects.keys(kind).clone();
        reframe_clip_keys(keys, end - start, hold_frames, frame - start);
    }
    hold.effects.opacity_percent = held_opacity_percent;
    hold.effects.opacity_keys.clear();
    hold.effects.volume_keys.clear();
    hold.effects.fade_in_frames = 0;
    hold.effects.fade_out_frames = 0;
    candidate.timeline_clips.push(hold);
    let hold_index = candidate.timeline_clips.len() - 1;
    commit_candidate(project, candidate, Some(hold_index))
}
