//! マーカー、イン・アウト、effect の基準値、字幕 track の表示。

use crate::clip_effects::{
    ClipKeyKind, KeyInterpolation, clip_key_spline_controls, evaluate_clip_keys, insert_clip_key,
    remove_clip_keys,
};
use crate::error::{ProjectResult, fail};
use crate::model::{Project, TimelineClipKind};

use super::common::{EditResult, commit_candidate};
use super::mapping::timeline_clip_duration;

pub fn add_timeline_marker(project: &mut Project, frame: i64) -> EditResult {
    if frame < 0 {
        return fail("マーカーの frame が不正です");
    }
    if project.timeline_markers.contains(&frame) {
        return Ok(None);
    }
    let mut candidate = project.clone();
    let index = candidate
        .timeline_markers
        .iter()
        .position(|marker| *marker > frame)
        .unwrap_or(candidate.timeline_markers.len());
    candidate.timeline_markers.insert(index, frame);
    commit_candidate(project, candidate, None)
}

pub fn delete_timeline_marker(project: &mut Project, frame: i64) -> EditResult {
    if !project.timeline_markers.contains(&frame) {
        return fail("削除するマーカーがありません");
    }
    let mut candidate = project.clone();
    candidate.timeline_markers.retain(|marker| *marker != frame);
    commit_candidate(project, candidate, None)
}

pub fn set_mark_in(project: &mut Project, frame: Option<i64>) -> EditResult {
    set_mark(project, frame, project.out_frame, true)
}

pub fn set_mark_out(project: &mut Project, frame: Option<i64>) -> EditResult {
    set_mark(project, project.in_frame, frame, false)
}

fn set_mark(
    project: &mut Project,
    in_frame: Option<i64>,
    out_frame: Option<i64>,
    changing_in: bool,
) -> EditResult {
    if let (Some(start), Some(end)) = (in_frame, out_frame)
        && start >= end
    {
        return fail("インはアウトより前にしてください");
    }
    let unchanged = if changing_in {
        project.in_frame == in_frame
    } else {
        project.out_frame == out_frame
    };
    if unchanged {
        return Ok(None);
    }
    let mut candidate = project.clone();
    candidate.in_frame = in_frame;
    candidate.out_frame = out_frame;
    commit_candidate(project, candidate, None)
}

/// effect を編集する clip 内の frame。再生ヘッドが clip の外なら先頭（選んだ clip は区間外でも編集できる）。
pub fn effect_edit_frame(project: &Project, clip_id: &str, playhead: i64) -> ProjectResult<i64> {
    let Some(clip) = project.clip(clip_id) else {
        return fail("effect を変える clip がありません");
    };
    let duration = timeline_clip_duration(project, clip)?;
    let local = playhead.saturating_sub(clip.timeline_start_frame);
    Ok(if (0..duration).contains(&local) {
        local
    } else {
        0
    })
}

/// effect の candidate。clip の種別に合わない channel（映像の音量・音声の不透明度）は断る。
fn effect_candidate(
    project: &Project,
    clip_id: &str,
    kind: ClipKeyKind,
) -> ProjectResult<(Project, usize)> {
    let Some(index) = project.clip_index(clip_id) else {
        return fail("effect を変える clip がありません");
    };
    if kind.is_audio() != (project.timeline_clips[index].kind == TimelineClipKind::Audio) {
        return fail("この clip にはその effect がありません");
    }
    Ok((project.clone(), index))
}

/// effect の値を変える。キーがある channel は `local_frame` のキーを置き換える（無ければ足す）。キーが無ければ
/// 固定値を変える。値は channel の範囲に無ければ失敗する。
pub fn set_clip_effect_at(
    project: &mut Project,
    clip_id: &str,
    kind: ClipKeyKind,
    local_frame: i64,
    value: f64,
) -> EditResult {
    let channel = kind.channel();
    if !value.is_finite() || !channel.accepts(value) {
        return fail(format!(
            "値は {} から {} の範囲です",
            channel.minimum, channel.maximum
        ));
    }
    let (mut candidate, index) = effect_candidate(project, clip_id, kind)?;
    let effects = &mut candidate.timeline_clips[index].effects;
    let keys = effects.keys_mut(kind);
    if keys.is_empty() {
        *effects.base_mut(kind) = value;
    } else if let Some(key) = keys.iter_mut().find(|key| key.frame == local_frame) {
        key.value = value;
    } else {
        insert_clip_key(keys, local_frame, value);
    }
    if candidate == *project {
        return Ok(None);
    }
    commit_candidate(project, candidate, Some(index))
}

/// `local_frame` にキーがあれば消し、無ければその位置の値でキーを足す。最後のキーを消すと、その値が固定値になる。
pub fn toggle_clip_key(
    project: &mut Project,
    clip_id: &str,
    kind: ClipKeyKind,
    local_frame: i64,
) -> EditResult {
    let (mut candidate, index) = effect_candidate(project, clip_id, kind)?;
    let effects = &mut candidate.timeline_clips[index].effects;
    let value = evaluate_clip_keys(effects.keys(kind), effects.base(kind), local_frame);
    let keys = effects.keys_mut(kind);
    if keys.iter().any(|key| key.frame == local_frame) {
        remove_clip_keys(keys, &[local_frame]);
        if keys.is_empty() {
            *effects.base_mut(kind) = value;
        }
    } else {
        insert_clip_key(keys, local_frame, value);
    }
    commit_candidate(project, candidate, Some(index))
}

/// アニメーションの入り切り。入れると `local_frame` の今の値でキーを 1 つ置き、切るとその位置の値を固定値にして
/// キーを全て消す。
pub fn set_clip_key_animation(
    project: &mut Project,
    clip_id: &str,
    kind: ClipKeyKind,
    local_frame: i64,
    enabled: bool,
) -> EditResult {
    let (mut candidate, index) = effect_candidate(project, clip_id, kind)?;
    let effects = &mut candidate.timeline_clips[index].effects;
    if enabled == !effects.keys(kind).is_empty() {
        return Ok(None);
    }
    let value = evaluate_clip_keys(effects.keys(kind), effects.base(kind), local_frame);
    if enabled {
        insert_clip_key(effects.keys_mut(kind), local_frame, value);
    } else {
        *effects.base_mut(kind) = value;
        effects.keys_mut(kind).clear();
    }
    commit_candidate(project, candidate, Some(index))
}

/// `local_frame` のキーから始まる区間の補間を変える。同じ補間の再選択は何もしない（trim・分割で残した部分曲線を
/// 0..1 へ戻すと、補間を変えていないのに動きが変わる）。スプラインへ変えるときは今の曲線に近い制御点から始める。
pub fn set_clip_key_interpolation(
    project: &mut Project,
    clip_id: &str,
    kind: ClipKeyKind,
    local_frame: i64,
    interpolation: KeyInterpolation,
) -> EditResult {
    let (mut candidate, index) = effect_candidate(project, clip_id, kind)?;
    let keys = candidate.timeline_clips[index].effects.keys_mut(kind);
    let Some(key) = keys.iter_mut().find(|key| key.frame == local_frame) else {
        return fail("補間を変えるキーがありません");
    };
    if key.interpolation == interpolation {
        return Ok(None);
    }
    if interpolation == KeyInterpolation::Spline {
        let (control1, control2) = clip_key_spline_controls(key);
        key.control1 = control1.clamp(0.0, 1.0);
        key.control2 = control2.clamp(0.0, 1.0);
    }
    key.interpolation = interpolation;
    key.curve_start = 0.0;
    key.curve_end = 1.0;
    commit_candidate(project, candidate, Some(index))
}

/// フェードイン・アウトの長さ（素材の frame で数える）。clip の素材範囲を超える値は検証で断る。
pub fn set_clip_fades(
    project: &mut Project,
    clip_id: &str,
    fade_in_frames: i64,
    fade_out_frames: i64,
) -> EditResult {
    let Some(index) = project.clip_index(clip_id) else {
        return fail("フェードを変える clip がありません");
    };
    if fade_in_frames < 0 || fade_out_frames < 0 {
        return fail("フェードの長さが負です");
    }
    let mut candidate = project.clone();
    let effects = &mut candidate.timeline_clips[index].effects;
    effects.fade_in_frames = fade_in_frames;
    effects.fade_out_frames = fade_out_frames;
    if candidate == *project {
        return Ok(None);
    }
    commit_candidate(project, candidate, Some(index))
}

pub fn set_subtitle_visible(project: &mut Project, visible: bool) -> EditResult {
    let Some(track) = project.subtitles.as_ref() else {
        return fail("字幕 track がありません");
    };
    if track.visible == visible {
        return Ok(None);
    }
    let mut candidate = project.clone();
    if let Some(track) = candidate.subtitles.as_mut() {
        track.visible = visible;
    }
    commit_candidate(project, candidate, None)
}
