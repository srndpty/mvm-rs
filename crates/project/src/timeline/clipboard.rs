//! コピーした clip を、別の位置または別の Project へ置く。
//!
//! ファイルの有無は見ない。素材がパネルに無ければ、コピー元の bin 項目を root へ足す。
//! それも無ければ失敗し、Project は変えない。

use std::collections::{HashMap, HashSet};

use mvm_core::Rational;

use crate::clip_effects::retime_clip_keys;
use crate::error::fail;
use crate::media_bin::{add_media_item, validate_media_bin};
use crate::model::{MediaItem, Project, TimelineClip, TrackKind, TrackRef};

use super::common::{EditResult, commit_candidate, remap_internal_ids};
use super::mapping::{source_boundary_to_timeline_boundary, timeline_clip_duration};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CopyPlacement {
    /// 元の track から順に、mute されていない空きを探す。無ければ track を足す。
    FindFreeTrack,
    /// ドラッグで見せた track 差のまま置く。重なれば失敗する。
    Exact { video_delta: i64, audio_delta: i64 },
}

/// `source_fps` はコピー元 Project の timeline fps。clip の開始差と key を、貼り付け先の fps へ移す。
pub fn place_copied_clips(
    project: &mut Project,
    clips: &[TimelineClip],
    media_items: &[MediaItem],
    source_fps: Rational,
    destination_frame: i64,
    placement: CopyPlacement,
    new_id: &mut dyn FnMut() -> String,
) -> EditResult {
    if clips.is_empty() || destination_frame < 0 || !source_fps.is_positive() {
        return fail("配置する clip または位置が不正です");
    }
    let first = clips
        .iter()
        .map(|clip| clip.timeline_start_frame)
        .min()
        .unwrap_or(0);
    let mut link_counts: HashMap<String, usize> = HashMap::new();
    let mut placed_lanes: Vec<(TrackKind, usize, Vec<TimelineClip>)> = Vec::new();
    for clip in clips {
        let offset = source_boundary_to_timeline_boundary(
            clip.timeline_start_frame.saturating_sub(first),
            source_fps,
            project.timeline_fps,
        )?;
        let start = destination_frame
            .checked_add(offset)
            .ok_or_else(|| crate::ProjectError::new("貼り付け位置が範囲外です"))?;
        let delta = match (placement, clip.track.kind) {
            (CopyPlacement::Exact { video_delta, .. }, TrackKind::Video) => video_delta,
            (CopyPlacement::Exact { audio_delta, .. }, TrackKind::Audio) => audio_delta,
            (CopyPlacement::FindFreeTrack, _) => 0,
        };
        let index = i64::try_from(clip.track.index)
            .ok()
            .and_then(|index| index.checked_add(delta))
            .filter(|index| *index >= 0)
            .ok_or_else(|| crate::ProjectError::new("配置先の track が範囲外です"))?;
        let mut placed = clip.clone();
        placed.timeline_start_frame = start;
        placed.track.index = usize::try_from(index)
            .map_err(|_| crate::ProjectError::new("配置先の track が範囲外です"))?;
        if !source_fps.same_value(project.timeline_fps) {
            let duration = timeline_clip_duration(project, &placed)?;
            for channel in crate::clip_effects::EFFECT_CHANNELS {
                retime_clip_keys(
                    placed.effects.keys_mut(channel.kind),
                    source_fps,
                    project.timeline_fps,
                    duration,
                )?;
            }
        }
        if !clip.link_group_id.is_empty() {
            *link_counts.entry(clip.link_group_id.clone()).or_insert(0) += 1;
        }
        if let Some((_, _, lane)) = placed_lanes.iter_mut().find(|(kind, source_index, _)| {
            *kind == clip.track.kind && *source_index == clip.track.index
        }) {
            lane.push(placed);
        } else {
            placed_lanes.push((clip.track.kind, clip.track.index, vec![placed]));
        }
    }

    let mut issued = HashSet::new();
    let mut candidate = project.clone();
    for clip in clips {
        if !clip.kind.uses_media_item() {
            continue;
        }
        if candidate.media_items.iter().any(|item| {
            crate::path_identity::canonical_path_key(&item.media_path)
                == crate::path_identity::canonical_path_key(&clip.media_path)
        }) {
            continue;
        }
        let Some(copied) = media_items.iter().find(|item| {
            crate::path_identity::canonical_path_key(&item.media_path)
                == crate::path_identity::canonical_path_key(&clip.media_path)
        }) else {
            return fail(format!(
                "貼り付けた素材がプロジェクトパネルにありません: {}",
                clip.name
            ));
        };
        let mut item = copied.clone();
        item.id = fresh_id(&candidate, &mut issued, new_id)?;
        item.folder_id.clear();
        add_media_item(&mut candidate, item)?;
    }

    let mut reserved: HashSet<(TrackKind, usize)> = HashSet::new();
    let mut new_ids = Vec::new();
    let mut new_links: HashMap<String, String> = HashMap::new();
    for (kind, _, lane) in &mut placed_lanes {
        let chosen = choose_track(&candidate, *kind, lane, placement, &reserved)?;
        if chosen == candidate.tracks(*kind).len() {
            candidate
                .tracks_mut(*kind)
                .push(crate::model::Track::named(kind.default_track_name(chosen)));
        }
        reserved.insert((*kind, chosen));
        for copy in lane {
            copy.track = TrackRef::new(*kind, chosen);
            copy.id = fresh_id(&candidate, &mut issued, new_id)?;
            remap_internal_ids(copy, new_id)?;
            if !copy.link_group_id.is_empty() {
                let group = copy.link_group_id.clone();
                if link_counts.get(&group).copied() == Some(2) {
                    if !new_links.contains_key(&group) {
                        new_links.insert(group.clone(), fresh_id(&candidate, &mut issued, new_id)?);
                    }
                    if let Some(id) = new_links.get(&group) {
                        copy.link_group_id.clone_from(id);
                    }
                } else {
                    copy.link_group_id.clear();
                }
            }
            new_ids.push(copy.id.clone());
            candidate.timeline_clips.push(copy.clone());
        }
    }
    rebind_media(&mut candidate, &new_ids)?;
    let selected = new_ids.first().and_then(|id| candidate.clip_index(id));
    validate_media_bin(&candidate)?;
    commit_candidate(project, candidate, selected)
}

fn fresh_id(
    project: &Project,
    issued: &mut HashSet<String>,
    new_id: &mut dyn FnMut() -> String,
) -> crate::ProjectResult<String> {
    let id = new_id();
    if id.is_empty()
        || project.clip(&id).is_some()
        || project.media_items.iter().any(|item| item.id == id)
        || !issued.insert(id.clone())
    {
        return fail("コピーの ID を作れません");
    }
    Ok(id)
}

fn choose_track(
    project: &Project,
    kind: TrackKind,
    lane: &[TimelineClip],
    placement: CopyPlacement,
    reserved: &HashSet<(TrackKind, usize)>,
) -> crate::ProjectResult<usize> {
    let Some(first) = lane.first() else {
        return fail("配置する clip がありません");
    };
    let count = project.tracks(kind).len();
    if matches!(placement, CopyPlacement::Exact { .. }) {
        if first.track.index >= count || reserved.contains(&(kind, first.track.index)) {
            return fail("配置先の track が範囲外です");
        }
        if lane_free(project, kind, first.track.index, lane)? {
            return Ok(first.track.index);
        }
        return fail("配置先の track が重なっています");
    }
    let mut order = Vec::new();
    if first.track.index < count {
        order.push(first.track.index);
    }
    for index in 0..=count {
        if !order.contains(&index) {
            order.push(index);
        }
    }
    for index in order {
        if index < count && project.tracks(kind)[index].muted {
            continue;
        }
        if reserved.contains(&(kind, index)) {
            continue;
        }
        if index == count || lane_free(project, kind, index, lane)? {
            return Ok(index);
        }
    }
    fail("空き track を確保できません")
}

fn lane_free(
    project: &Project,
    kind: TrackKind,
    index: usize,
    lane: &[TimelineClip],
) -> crate::ProjectResult<bool> {
    for copy in lane {
        let duration = timeline_clip_duration(project, copy)?;
        let end = copy
            .timeline_start_frame
            .checked_add(duration)
            .ok_or_else(|| crate::ProjectError::new("コピーした clip の尺が不正です"))?;
        for existing in &project.timeline_clips {
            if existing.track.kind != kind || existing.track.index != index {
                continue;
            }
            let existing_end = existing
                .timeline_start_frame
                .checked_add(timeline_clip_duration(project, existing)?)
                .ok_or_else(|| crate::ProjectError::new("既存 clip の尺が不正です"))?;
            if copy.timeline_start_frame < existing_end && existing.timeline_start_frame < end {
                return Ok(false);
            }
        }
    }
    Ok(true)
}

fn rebind_media(project: &mut Project, new_ids: &[String]) -> crate::ProjectResult<()> {
    let mut updates = Vec::new();
    for clip in &project.timeline_clips {
        if !new_ids.iter().any(|id| id == &clip.id) || !clip.kind.uses_media_item() {
            continue;
        }
        let path = crate::path_identity::canonical_path_key(&clip.media_path);
        let Some(item) = project
            .media_items
            .iter()
            .find(|item| crate::path_identity::canonical_path_key(&item.media_path) == path)
        else {
            return fail(format!(
                "貼り付けた素材がプロジェクトパネルにありません: {}",
                clip.name
            ));
        };
        updates.push((clip.id.clone(), item.id.clone(), item.media_path.clone()));
    }
    for (clip_id, item_id, media_path) in updates {
        if let Some(clip) = project
            .timeline_clips
            .iter_mut()
            .find(|clip| clip.id == clip_id)
        {
            clip.media_item_id = item_id;
            clip.media_path = media_path;
        }
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use mvm_core::Rational;

    use super::*;
    use crate::model::{TimelineClipKind, TrackRef};

    fn text(id: &str, start: i64, track: usize) -> TimelineClip {
        TimelineClip {
            kind: TimelineClipKind::Text,
            name: id.to_owned(),
            id: id.to_owned(),
            source_fps: Rational::new(30, 1),
            source_frame_count: 30,
            source_in_frame: 0,
            source_out_frame: 30,
            timeline_start_frame: start,
            track: TrackRef::video(track),
            text: crate::model::TextClipData {
                content: id.to_owned(),
                ..crate::model::TextClipData::default()
            },
            ..TimelineClip::default()
        }
    }

    fn ids() -> impl FnMut() -> String {
        let mut n = 0;
        move || {
            n += 1;
            format!("copy-{n}")
        }
    }

    #[test]
    fn paste_lands_on_the_next_free_track_and_keeps_the_original() {
        let mut project = Project {
            timeline_fps: Rational::new(30, 1),
            timeline_clips: vec![text("a", 0, 0)],
            ..Project::default()
        };
        let before = project.timeline_clips.len();
        place_copied_clips(
            &mut project,
            &[text("a", 0, 0)],
            &[],
            Rational::new(30, 1),
            0,
            CopyPlacement::FindFreeTrack,
            &mut ids(),
        )
        .expect("貼り付け");
        assert_eq!(project.timeline_clips.len(), before + 1);
        let copy = project
            .timeline_clips
            .iter()
            .find(|clip| clip.id != "a")
            .unwrap();
        assert_eq!(copy.track, TrackRef::video(1));
        assert_eq!(copy.timeline_start_frame, 0);
        assert_ne!(copy.id, "a");
        assert_eq!(copy.text.content, "a");
    }

    #[test]
    fn exact_overlap_is_rejected_and_the_project_stays() {
        let mut project = Project {
            timeline_fps: Rational::new(30, 1),
            timeline_clips: vec![text("a", 10, 0)],
            ..Project::default()
        };
        let error = place_copied_clips(
            &mut project,
            &[text("a", 10, 0)],
            &[],
            Rational::new(30, 1),
            10,
            CopyPlacement::Exact {
                video_delta: 0,
                audio_delta: 0,
            },
            &mut ids(),
        )
        .expect_err("重なり");
        assert!(error.to_string().contains("重な"), "{error}");
        assert_eq!(project.timeline_clips.len(), 1);
        assert_eq!(project.timeline_clips[0].id, "a");
    }

    #[test]
    fn empty_paste_does_not_change_the_project() {
        let mut project = Project::default();
        let error = place_copied_clips(
            &mut project,
            &[],
            &[],
            Rational::new(30, 1),
            0,
            CopyPlacement::FindFreeTrack,
            &mut ids(),
        )
        .expect_err("空");
        assert!(error.to_string().contains("不正"), "{error}");
        assert!(project.timeline_clips.is_empty());
    }
}
