//! Project の設定・track・空白・keyframe・Manim clip の配置。

use mvm_core::Rational;

use crate::clip_effects::{
    ClipEffects, ClipKeyKind, EFFECT_CHANNELS, insert_clip_key, remove_clip_keys, retime_clip_keys,
};
use crate::error::{ProjectResult, fail};
use crate::model::{
    ManimAsset, Project, TimelineClip, TimelineClipKind, Track, TrackKind, TrackRef,
    is_canonical_frame_rate, is_configurable_timeline_frame_rate, is_valid_project_output_size,
};

use super::common::{EditResult, commit_candidate, finalize_timeline_candidate};
use super::mapping::{
    clip_duration_at, clip_interval, source_boundary_to_timeline_boundary, timeline_clip_duration,
};
use super::ripple::edit_timeline_time;

/// 生成済みの Manim asset を timeline に置く。Manim clip は Project に 1 つだけ。
#[allow(clippy::too_many_arguments)]
pub fn append_manim_timeline_clip_at(
    project: &mut Project,
    asset: &ManimAsset,
    clip_id: String,
    source_fps: Rational,
    source_frame_count: i64,
    timeline_start_frame: i64,
    track: TrackRef,
) -> EditResult {
    if asset.generated_video_path.as_os_str().is_empty()
        || asset.scene_name.is_empty()
        || clip_id.is_empty()
    {
        return fail("timeline に配置できる生成済み Manim asset ではありません");
    }
    if !project.is_valid_track_ref(track) || track.kind != TrackKind::Video {
        return fail("Manim clip の配置先 video track が存在しません");
    }
    if project
        .timeline_clips
        .iter()
        .any(|clip| clip.kind == TimelineClipKind::Manim)
    {
        return fail("Manim asset はすでに timeline に配置されています");
    }
    let mut candidate = project.clone();
    candidate.timeline_clips.push(TimelineClip {
        kind: TimelineClipKind::Manim,
        media_path: asset.generated_video_path.clone(),
        name: asset.scene_name.clone(),
        id: clip_id,
        source_fps,
        source_frame_count,
        source_in_frame: 0,
        source_out_frame: source_frame_count,
        timeline_start_frame,
        track,
        ..TimelineClip::default()
    });
    let selected = candidate.timeline_clips.len() - 1;
    commit_candidate(project, candidate, Some(selected))
}

/// 解像度と timeline fps を 1 つの transaction で変更する。既存 clip の素材 domain は変えず、timeline 上の位置
/// （開始・マーカー・イン/アウト・トランジションの長さ・字幕・clip 内の effect key）だけを、秒の位置が保たれるよう
/// 新 fps へ換算する。key は新しい clip 尺の末尾 frame へ寄せ、同じ frame に重なったものは先の 1 つを残す。
pub fn set_project_video_settings(
    project: &mut Project,
    width: i32,
    height: i32,
    fps: Rational,
) -> EditResult {
    if !is_valid_project_output_size(width, height) {
        return fail("Project output size が不正です");
    }
    if !is_configurable_timeline_frame_rate(fps) || !is_canonical_frame_rate(fps) {
        return fail("対応していない timeline frame rate です");
    }
    if project.output_width == width
        && project.output_height == height
        && project.timeline_fps == fps
    {
        return Ok(None);
    }
    let mut candidate = project.clone();
    let old_fps = project.timeline_fps;
    if old_fps != fps {
        let convert = |frame: &mut i64| -> bool {
            match source_boundary_to_timeline_boundary(*frame, old_fps, fps) {
                Ok(converted) => {
                    *frame = converted;
                    true
                }
                Err(_) => false,
            }
        };
        if let Some(track) = candidate.subtitles.as_mut() {
            // 字幕は最も近い frame へ丸める（境界の ceil ではない）。
            let round = |frame: &mut i64| -> bool {
                let num = i128::from(*frame) * i128::from(old_fps.den) * i128::from(fps.num);
                let den = i128::from(old_fps.num) * i128::from(fps.den);
                match i64::try_from((num + den / 2) / den) {
                    Ok(value) => {
                        *frame = value;
                        true
                    }
                    Err(_) => false,
                }
            };
            for cue in &mut track.cues {
                if !round(&mut cue.start_frame) || !round(&mut cue.end_frame) {
                    return fail("字幕を新しいFPSへ換算できません");
                }
            }
        }
        if !candidate.timeline_markers.iter_mut().all(&convert) {
            return fail("マーカーを新しいtimeline frame rateへ変換できません");
        }
        candidate.timeline_markers.dedup();
        let in_ok = candidate.in_frame.as_mut().is_none_or(convert);
        let out_ok = candidate.out_frame.as_mut().is_none_or(convert);
        if !in_ok || !out_ok {
            return fail("イン・アウトを新しいtimeline frame rateへ変換できません");
        }
        if matches!((candidate.in_frame, candidate.out_frame), (Some(start), Some(end)) if start >= end)
        {
            return fail("frame rate変更でイン・アウトの範囲が空になります");
        }
        for clip in &mut candidate.timeline_clips {
            if !convert(&mut clip.timeline_start_frame) {
                return fail(format!(
                    "clip開始位置を新しいtimeline frame rateへ変換できません: {}",
                    clip.name
                ));
            }
            // 尺は素材 domain と新しい timeline fps から決まる。開始位置の換算とは独立。
            let new_duration = match clip_duration_at(clip, fps) {
                Ok(duration) => duration,
                Err(error) => {
                    return fail(format!(
                        "clipの尺を新しいtimeline frame rateへ変換できません: {}: {error}",
                        clip.name
                    ));
                }
            };
            for channel in &EFFECT_CHANNELS {
                if let Err(error) = retime_clip_keys(
                    clip.effects.keys_mut(channel.kind),
                    old_fps,
                    fps,
                    new_duration,
                ) {
                    return fail(format!(
                        "キーフレームを新しいtimeline frame rateへ変換できません: {}: {error}",
                        clip.name
                    ));
                }
            }
        }
        for transition in &mut candidate.timeline_transitions {
            if !convert(&mut transition.frames_before_cut)
                || !convert(&mut transition.frames_after_cut)
            {
                return fail("トランジションを新しいtimeline frame rateへ変換できません");
            }
        }
    }
    candidate.output_width = width;
    candidate.output_height = height;
    candidate.timeline_fps = fps;
    finalize_timeline_candidate(&mut candidate).map_err(|error| {
        crate::ProjectError::new(format!("Project設定変更後のtimelineが不正です: {error}"))
    })?;
    *project = candidate;
    Ok(None)
}

/// fps だけを変更する。換算の規則は [`set_project_video_settings`] と同じ。
pub fn set_timeline_frame_rate(project: &mut Project, fps: Rational) -> EditResult {
    let (width, height) = (project.output_width, project.output_height);
    set_project_video_settings(project, width, height, fps)
}

pub fn add_track(project: &mut Project, kind: TrackKind) -> EditResult {
    let mut candidate = project.clone();
    let index = candidate.tracks(kind).len();
    candidate
        .tracks_mut(kind)
        .push(Track::named(kind.default_track_name(index)));
    commit_candidate(project, candidate, Some(index))
}

/// clip が載っている track は削除させない（暗黙に clip を消さない）。
pub fn remove_track(project: &mut Project, track: TrackRef) -> EditResult {
    if !project.is_valid_track_ref(track) {
        return fail("削除する track が存在しません");
    }
    if track.kind == TrackKind::Video && project.video_tracks.len() <= 1 {
        return fail("video track は最低 1 本必要です");
    }
    if let Some(clip) = project
        .timeline_clips
        .iter()
        .find(|clip| clip.track == track)
    {
        return fail(format!(
            "clip が載っている track は削除できません: {}",
            clip.name
        ));
    }
    let mut candidate = project.clone();
    let tracks = candidate.tracks_mut(track.kind);
    tracks.remove(track.index);
    // 削除位置より後ろの track index を詰め、既定名も振り直す。
    for (index, remaining) in tracks.iter_mut().enumerate() {
        remaining.name = track.kind.default_track_name(index);
    }
    for clip in &mut candidate.timeline_clips {
        if clip.track.kind == track.kind && clip.track.index > track.index {
            clip.track.index -= 1;
        }
    }
    commit_candidate(project, candidate, Some(track.index))
}

pub fn set_track_muted(project: &mut Project, track: TrackRef, muted: bool) -> EditResult {
    set_tracks_muted(project, track.kind, &[track.index], muted)
}

/// 同じ kind の複数 track の mute をまとめて変える（目玉のドラッグ塗り）。1 つでも不正な index があれば何も
/// 変えずに失敗する。
pub fn set_tracks_muted(
    project: &mut Project,
    kind: TrackKind,
    indices: &[usize],
    muted: bool,
) -> EditResult {
    let Some(first) = indices.first() else {
        return fail("track が指定されていません");
    };
    if indices
        .iter()
        .any(|index| !project.is_valid_track_ref(TrackRef::new(kind, *index)))
    {
        return fail("track が存在しません");
    }
    let tracks = project.tracks_mut(kind);
    for index in indices {
        tracks[*index].muted = muted;
    }
    Ok(Some(*first))
}

/// audio track だけが対象。video track を渡すと失敗する。
pub fn set_track_solo(project: &mut Project, track: TrackRef, solo: bool) -> EditResult {
    if !project.is_valid_track_ref(track) {
        return fail("track が存在しません");
    }
    if track.kind != TrackKind::Audio {
        return fail("solo は audio track だけに設定できます");
    }
    project.audio_tracks[track.index].solo = solo;
    Ok(Some(track.index))
}

/// track 上の空白 [start, end)。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TimelineGap {
    pub start: i64,
    pub end: i64,
}

/// track 上の timeline_frame を含む「clip が無く、後ろに clip がある」区間を返す。終端より後ろの空白は詰める
/// 対象が無いので失敗にする。
pub fn gap_at(
    project: &Project,
    track: TrackRef,
    timeline_frame: i64,
) -> ProjectResult<TimelineGap> {
    if !project.is_valid_track_ref(track) || timeline_frame < 0 {
        return fail("track または frame が不正です");
    }
    let mut gap_start = 0;
    let mut gap_end = None::<i64>;
    for clip in project
        .timeline_clips
        .iter()
        .filter(|clip| clip.track == track)
    {
        let (start, end) = clip_interval(project, clip)?;
        if (start..end).contains(&timeline_frame) {
            return fail("その位置には clip があります");
        }
        if end <= timeline_frame {
            gap_start = gap_start.max(end);
        }
        if start > timeline_frame {
            gap_end = Some(gap_end.map_or(start, |gap_end| gap_end.min(start)));
        }
    }
    let Some(gap_end) = gap_end else {
        return fail("後続 clip が無いため詰める gap がありません");
    };
    Ok(TimelineGap {
        start: gap_start,
        end: gap_end,
    })
}

/// gap_at が返す区間を閉じ、後続 clip を左へ詰める（ripple delete）。全 track と字幕に同じ時間削除を適用する。
pub fn ripple_delete_gap(
    project: &mut Project,
    track: TrackRef,
    timeline_frame: i64,
) -> EditResult {
    let gap = gap_at(project, track, timeline_frame)?;
    if gap.end <= gap.start {
        return fail("詰める空白がありません");
    }
    edit_timeline_time(project, gap.start, gap.end - gap.start, 0)
}

/// keyframe 編集の候補（ドラッグ表示と確定の両方が使う）。
#[derive(Clone, Debug, PartialEq)]
pub struct ClipKeyEditPreview {
    pub effects: ClipEffects,
    pub frame: i64,
}

/// keyframe の追加（`original_frame` が `None`）または移動。値は channel の範囲へ、frame は clip の尺と隣の key
/// の間へ収める。
pub fn preview_clip_key_edit(
    project: &Project,
    clip_id: &str,
    kind: ClipKeyKind,
    original_frame: Option<i64>,
    requested_frame: i64,
    requested_percent: f64,
) -> ProjectResult<ClipKeyEditPreview> {
    let Some(index) = project.clip_index(clip_id) else {
        return fail("キーフレームを編集する clip がありません");
    };
    let clip = &project.timeline_clips[index];
    if kind.is_audio() != (clip.kind == TimelineClipKind::Audio) || !requested_percent.is_finite() {
        return fail("キーフレームの種別または値が不正です");
    }
    let duration = timeline_clip_duration(project, clip)?;
    let mut effects = clip.effects.clone();
    let channel = kind.channel();
    let value = requested_percent.clamp(channel.minimum, channel.maximum);
    let keys = effects.keys_mut(kind);
    let frame = match original_frame {
        None => {
            let frame = requested_frame.clamp(0, duration - 1);
            match keys.iter_mut().find(|key| key.frame == frame) {
                Some(key) => key.value = value,
                None => insert_clip_key(keys, frame, value),
            }
            keys.sort_by_key(|key| key.frame);
            frame
        }
        Some(original) => {
            let Some(position) = keys.iter().position(|key| key.frame == original) else {
                return fail("移動元のキーフレームがありません");
            };
            let lower = position
                .checked_sub(1)
                .map_or(0, |previous| keys[previous].frame + 1);
            let upper = keys
                .get(position + 1)
                .map_or(duration - 1, |next| next.frame - 1);
            let frame = requested_frame.clamp(lower, upper.max(lower));
            keys[position].frame = frame;
            keys[position].value = value;
            frame
        }
    };
    let mut candidate = project.clone();
    candidate.timeline_clips[index].effects = effects.clone();
    finalize_timeline_candidate(&mut candidate)?;
    Ok(ClipKeyEditPreview { effects, frame })
}

pub fn edit_clip_key(
    project: &mut Project,
    clip_id: &str,
    kind: ClipKeyKind,
    original_frame: Option<i64>,
    requested_frame: i64,
    requested_percent: f64,
) -> EditResult {
    let preview = preview_clip_key_edit(
        project,
        clip_id,
        kind,
        original_frame,
        requested_frame,
        requested_percent,
    )?;
    let Some(index) = project.clip_index(clip_id) else {
        return fail("キーフレームを編集する clip がありません");
    };
    let mut candidate = project.clone();
    candidate.timeline_clips[index].effects = preview.effects;
    commit_candidate(project, candidate, Some(index))
}

/// frame の key を消す。最後の key を消すと線は effects の基準値（消した key の値）へ戻る。
pub fn delete_clip_key(
    project: &mut Project,
    clip_id: &str,
    kind: ClipKeyKind,
    frame: i64,
) -> EditResult {
    let Some(index) = project.clip_index(clip_id) else {
        return fail("キーフレームを削除する clip がありません");
    };
    let mut candidate = project.clone();
    let clip = &mut candidate.timeline_clips[index];
    if kind.is_audio() != (clip.kind == TimelineClipKind::Audio) {
        return fail("キーフレームの種別が不正です");
    }
    let keys = clip.effects.keys(kind);
    let Some(found) = keys.iter().find(|key| key.frame == frame).copied() else {
        return fail("削除するキーフレームがありません");
    };
    if keys.len() == 1 {
        *clip.effects.base_mut(kind) = found.value;
    }
    remove_clip_keys(clip.effects.keys_mut(kind), &[frame]);
    commit_candidate(project, candidate, Some(index))
}
