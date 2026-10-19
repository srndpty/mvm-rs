//! マーカー、イン・アウト、effect の基準値、字幕 track の表示。

use crate::clip_effects::ClipKeyKind;
use crate::error::fail;
use crate::model::Project;

use super::common::{EditResult, commit_candidate};

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

pub fn set_clip_effect_base(
    project: &mut Project,
    clip_id: &str,
    kind: ClipKeyKind,
    value: f64,
) -> EditResult {
    let Some(index) = project.clip_index(clip_id) else {
        return fail("effect を変える clip がありません");
    };
    if kind.is_audio()
        != (project.timeline_clips[index].kind == crate::model::TimelineClipKind::Audio)
        || !value.is_finite()
    {
        return fail("effect の種別または値が不正です");
    }
    let channel = kind.channel();
    if !channel.accepts(value) {
        return fail(format!(
            "値は {} から {} の範囲です",
            channel.minimum, channel.maximum
        ));
    }
    if project.timeline_clips[index].effects.base(kind) == value {
        return Ok(None);
    }
    let mut candidate = project.clone();
    *candidate.timeline_clips[index].effects.base_mut(kind) = value;
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
