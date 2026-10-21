//! 基本の timeline 編集（移動・配置・削除・リンク解除・trim・分割・音量・有効/無効・フェード）。

use std::collections::{HashMap, HashSet};

use mvm_core::Rational;

use crate::clip_effects::step_volume_percent_by_db;
use crate::error::{ProjectResult, fail};
use crate::model::{Project, TimelineClip, TimelineClipKind, TrackKind, TrackRef};
use crate::subtitles::{
    erase_linked_subtitles, linked_subtitle_start, relink_subtitles_after_split,
    shift_linked_subtitles, unlink_subtitles,
};

use super::common::{
    EditResult, LinkMode, TrimEdge, adjacent_clip_index, assign_split_ids, clamp_fades_to_length,
    clear_math_intro, commit_candidate, commit_mapping_edit, edge_range, edit_targets,
    finalize_timeline_candidate, include_linked_counterparts, mark_clips, narrow_range,
    trim_clip_boundary,
};
use super::mapping::{
    clip_interval, clip_source_boundary_to_timeline, clip_timeline_boundary_to_source,
};
use super::settings::add_track;

/// track の終端（clip の終端の最大）。
pub fn timeline_track_end_frame(project: &Project, track: TrackRef) -> ProjectResult<i64> {
    if !project.is_valid_track_ref(track) {
        return fail("track が存在しません");
    }
    let mut end = 0;
    for clip in project
        .timeline_clips
        .iter()
        .filter(|clip| clip.track == track)
    {
        end = end.max(clip_interval(project, clip)?.1);
    }
    Ok(end)
}

/// track 上で frame を含む clip。
pub fn active_clip_at(project: &Project, track: TrackRef, timeline_frame: i64) -> Option<usize> {
    if !project.is_valid_track_ref(track) || timeline_frame < 0 {
        return None;
    }
    project.timeline_clips.iter().position(|clip| {
        clip.track == track
            && clip_interval(project, clip)
                .is_ok_and(|(start, end)| (start..end).contains(&timeline_frame))
    })
}

/// index が track index と一致する配列を返す。clip が無い track は `None`。合成順は index 昇順で bottom → top。
pub fn active_clips_at(
    project: &Project,
    kind: TrackKind,
    timeline_frame: i64,
) -> Vec<Option<usize>> {
    (0..project.tracks(kind).len())
        .map(|index| active_clip_at(project, TrackRef::new(kind, index), timeline_frame))
        .collect()
}

/// 一番上の video track で `timeline_frame` に掛かる有効な clip（preview で見えている clip）の index。
/// 非表示の track の clip は preview に出ないので選ばない（preview の合成と同じ判定）。
pub fn top_video_clip_at(project: &Project, timeline_frame: i64) -> Option<usize> {
    active_clips_at(project, TrackKind::Video, timeline_frame)
        .into_iter()
        .rev()
        .flatten()
        .find(|index| {
            let clip = &project.timeline_clips[*index];
            clip.enabled && project.is_track_output_enabled(clip.track)
        })
}

/// 文字のテロップ位置の左右の余白（画面幅の割合）と、下からの高さ（画面高の割合）。決め打ちの値で、
/// 安全域の規格から導いたものではない（C++ 版と同じ）。
const TEXT_PRESET_SIDE_RATIO: f64 = 0.05;
const TEXT_PRESET_BOTTOM_RATIO: f64 = 0.15;

/// 文字 block（幅・高さ、画素）を、画面の下から 15% の高さの左・中央・右へ置く左上の座標。
/// 画面より大きな文字は左上を画面内に留める。
pub fn text_preset_position(
    block_width: f64,
    block_height: f64,
    output_width: i32,
    output_height: i32,
    alignment: &str,
) -> ProjectResult<(i32, i32)> {
    if output_width <= 0 || output_height <= 0 {
        return fail("出力の寸法が不正です");
    }
    if !block_width.is_finite()
        || !block_height.is_finite()
        || block_width < 0.0
        || block_height < 0.0
    {
        return fail("文字の大きさが不正です");
    }
    let width = f64::from(output_width);
    let side = width * TEXT_PRESET_SIDE_RATIO;
    let x = match alignment {
        "left" => side,
        "center" => (width - block_width) / 2.0,
        "right" => width - side - block_width,
        other => return fail(format!("未知の揃えです: {other}")),
    };
    let y = f64::from(output_height) * (1.0 - TEXT_PRESET_BOTTOM_RATIO) - block_height;
    Ok((x.round().max(0.0) as i32, y.round().max(0.0) as i32))
}

/// track の [start, end) を上書きする。keep の clip（上に置く clip）は触らない。
/// 丸ごと覆われた clip は消し（リンク相手は未リンクにする）、端が掛かれば削り、中に置けば 2 つに分ける。
fn overwrite_track_range(
    candidate: &mut Project,
    track: TrackRef,
    start: i64,
    end: i64,
    keep: &HashSet<String>,
    new_id: &mut dyn FnMut() -> String,
) -> ProjectResult<()> {
    let mut removed: Vec<String> = Vec::new();
    let original_count = candidate.timeline_clips.len();
    for index in 0..original_count {
        let clip = &candidate.timeline_clips[index];
        if keep.contains(&clip.id) || clip.track != track {
            continue;
        }
        let (clip_start, clip_end) = clip_interval(candidate, clip)?;
        if clip_end <= start || clip_start >= end {
            continue;
        }
        if clip_start >= start && clip_end <= end {
            removed.push(clip.id.clone());
            continue;
        }
        if clip_start < start && clip_end > end {
            // 中に置いた: 左を start で止め、右を end から始まる別 clip にする。
            let mut right = clip.clone();
            assign_split_ids(&clip.id, &mut right, new_id)
                .map_err(|_| crate::ProjectError::new("上書きで分けた clip の ID を作れません"))?;
            right.link_group_id.clear();
            trim_clip_boundary(candidate, &mut right, TrimEdge::Left, end - clip_start)?;
            right.effects.fade_in_frames = 0;
            clear_math_intro(&mut right);
            clamp_fades_to_length(&mut right);
            let original_id = candidate.timeline_clips[index].id.clone();
            for transition in &mut candidate.timeline_transitions {
                if transition.outgoing_clip_id == original_id {
                    transition.outgoing_clip_id = right.id.clone();
                }
            }
            let mut left = candidate.timeline_clips[index].clone();
            trim_clip_boundary(candidate, &mut left, TrimEdge::Right, start - clip_end)?;
            left.effects.fade_out_frames = 0;
            clamp_fades_to_length(&mut left);
            candidate.timeline_clips[index] = left;
            candidate.timeline_clips.push(right);
            continue;
        }
        let mut trimmed = clip.clone();
        if clip_start < start {
            trim_clip_boundary(candidate, &mut trimmed, TrimEdge::Right, start - clip_end)?;
        } else {
            trim_clip_boundary(candidate, &mut trimmed, TrimEdge::Left, end - clip_start)?;
        }
        clamp_fades_to_length(&mut trimmed);
        candidate.timeline_clips[index] = trimmed;
    }
    remove_clips_and_unlink(candidate, &removed);
    Ok(())
}

/// clip を消し、消した clip のリンク相手を未リンクにする（片方だけのリンクを残さない）。
pub(crate) fn remove_clips_and_unlink(candidate: &mut Project, removed: &[String]) {
    let groups: HashSet<String> = candidate
        .timeline_clips
        .iter()
        .filter(|clip| removed.contains(&clip.id) && clip.is_linked())
        .map(|clip| clip.link_group_id.clone())
        .collect();
    for clip in &mut candidate.timeline_clips {
        if groups.contains(&clip.link_group_id) {
            clip.link_group_id.clear();
        }
    }
    candidate
        .timeline_clips
        .retain(|clip| !removed.contains(&clip.id));
}

pub fn move_clip(
    project: &mut Project,
    clip_id: &str,
    destination: TrackRef,
    new_start_frame: i64,
) -> EditResult {
    move_clips(
        project,
        &[clip_id.to_owned()],
        clip_id,
        destination,
        new_start_frame,
        LinkMode::Linked,
        None,
    )
}

/// anchor clip の移動量を選択 clip 全体へ適用する。Linked ならリンク相手と字幕も同じ時間差で動く。
/// `new_id` を渡すと上書きで置く（動かした clip の下になる他の clip は、丸ごと覆われれば消し、端が掛かれば
/// 削り、中に置けば 2 つに分ける）。`None` なら重なりを拒否する。
pub fn move_clips(
    project: &mut Project,
    clip_ids: &[String],
    anchor_clip_id: &str,
    destination: TrackRef,
    new_start_frame: i64,
    link_mode: LinkMode,
    new_id: Option<&mut dyn FnMut() -> String>,
) -> EditResult {
    if clip_ids.is_empty() || !project.is_valid_track_ref(destination) || new_start_frame < 0 {
        return fail("timeline clip の移動先 track または start frame が不正です");
    }
    let mut candidate = project.clone();
    let Some(anchor_index) = candidate.clip_index(anchor_clip_id) else {
        return fail("移動する timeline clip がありません");
    };
    let anchor = candidate.timeline_clips[anchor_index].clone();
    if !anchor.kind.fits_track(destination.kind) {
        return fail("この clip はその種別の track へ移動できません");
    }
    let mut moved_ids: HashSet<String> = HashSet::new();
    let mut link_groups: HashSet<String> = HashSet::new();
    for id in clip_ids {
        let Some(selected) = candidate.clip(id) else {
            return fail("移動する timeline clip がありません");
        };
        moved_ids.insert(selected.id.clone());
        if link_mode == LinkMode::Linked && selected.is_linked() {
            link_groups.insert(selected.link_group_id.clone());
        }
    }
    if !moved_ids.contains(anchor_clip_id) {
        return fail("anchor clip が選択に含まれていません");
    }
    for clip in &candidate.timeline_clips {
        if clip.is_linked() && link_groups.contains(&clip.link_group_id) {
            moved_ids.insert(clip.id.clone());
        }
    }
    let mut minimum_start = candidate
        .timeline_clips
        .iter()
        .filter(|clip| moved_ids.contains(&clip.id))
        .map(|clip| clip.timeline_start_frame)
        .min()
        .unwrap_or(i64::MAX);
    // リンクした字幕も同じ量だけ動くので、字幕が 0 より前へ出ない位置で止める。
    // Alt（Single）の移動では clip のリンク相手と同じく字幕も追従させない。
    if link_mode == LinkMode::Linked
        && let Some(cue_start) = linked_subtitle_start(&candidate, &moved_ids)
    {
        minimum_start = minimum_start.min(cue_start);
    }
    let requested_delta = new_start_frame - anchor.timeline_start_frame;
    // anchor だけを 0 へ丸めると、より左にある選択 clip やリンク相手が負になる。
    // グループ全体の最左端が 0 に接する位置で止め、全 clip へ同じ delta を適用する。
    let delta = if requested_delta < 0 && minimum_start < -requested_delta {
        -minimum_start
    } else {
        requested_delta
    };
    let track_delta = destination.index as i64 - anchor.track.index as i64;
    let track_counts = (candidate.video_tracks.len(), candidate.audio_tracks.len());
    for clip in &mut candidate.timeline_clips {
        if !moved_ids.contains(&clip.id) {
            continue;
        }
        let Some(start) = clip.timeline_start_frame.checked_add(delta) else {
            return fail("選択clipの移動先が範囲外です");
        };
        clip.timeline_start_frame = start;
        if clip.track.kind == destination.kind {
            let count = match clip.track.kind {
                TrackKind::Video => track_counts.0,
                TrackKind::Audio => track_counts.1,
            };
            let translated = clip.track.index as i64 + track_delta;
            if translated < 0 || translated >= count as i64 {
                return fail("選択clipの移動先trackが範囲外です");
            }
            clip.track = TrackRef::new(clip.track.kind, translated as usize);
        }
    }
    if link_mode == LinkMode::Linked {
        shift_linked_subtitles(&mut candidate, &moved_ids, delta)?;
    }
    if let Some(new_id) = new_id {
        let spans: Vec<(TrackRef, i64, i64)> = candidate
            .timeline_clips
            .iter()
            .filter(|clip| moved_ids.contains(&clip.id))
            .map(|clip| {
                clip_interval(&candidate, clip).map(|(start, end)| (clip.track, start, end))
            })
            .collect::<ProjectResult<_>>()?;
        for (track, start, end) in spans {
            overwrite_track_range(&mut candidate, track, start, end, &moved_ids, new_id)?;
        }
    }
    finalize_timeline_candidate(&mut candidate)?;
    *project = candidate;
    Ok(project.clip_index(anchor_clip_id))
}

/// track 末尾へ追加する。clip の track と開始位置はここで決める。
pub fn append_timeline_clip(
    project: &mut Project,
    mut clip: TimelineClip,
    track: TrackRef,
) -> EditResult {
    if !project.is_valid_track_ref(track) {
        return fail("追加先の track が存在しません");
    }
    if !clip.kind.fits_track(track.kind) {
        return fail("clip 種別と track 種別が一致しません");
    }
    clip.track = track;
    clip.timeline_start_frame = timeline_track_end_frame(project, track)?;
    let mut candidate = project.clone();
    candidate.timeline_clips.push(clip);
    let selected = candidate.timeline_clips.len() - 1;
    commit_candidate(project, candidate, Some(selected))
}

/// 指定位置へ配置する。既存 clip と重なる場合は失敗する。
pub fn place_timeline_clip_at(
    project: &mut Project,
    mut clip: TimelineClip,
    track: TrackRef,
    timeline_start_frame: i64,
) -> EditResult {
    if !project.is_valid_track_ref(track) || timeline_start_frame < 0 {
        return fail("追加先の track または start frame が不正です");
    }
    if !clip.kind.fits_track(track.kind) {
        return fail("clip 種別と track 種別が一致しません");
    }
    clip.track = track;
    clip.timeline_start_frame = timeline_start_frame;
    let mut candidate = project.clone();
    candidate.timeline_clips.push(clip);
    let selected = candidate.timeline_clips.len() - 1;
    commit_candidate(project, candidate, Some(selected))
}

pub fn delete_timeline_clip(project: &mut Project, selected_index: usize) -> EditResult {
    let Some(selected) = project.timeline_clips.get(selected_index) else {
        return fail("削除する timeline clip がありません");
    };
    let mut candidate = project.clone();
    let group = selected.link_group_id.clone();
    let mut erased: HashSet<String> = HashSet::from([selected.id.clone()]);
    if group.is_empty() {
        candidate.timeline_clips.remove(selected_index);
    } else {
        erased.extend(
            candidate
                .timeline_clips
                .iter()
                .filter(|clip| clip.link_group_id == group)
                .map(|clip| clip.id.clone()),
        );
        candidate
            .timeline_clips
            .retain(|clip| clip.link_group_id != group);
    }
    erase_linked_subtitles(&mut candidate, &erased);
    finalize_timeline_candidate(&mut candidate)?;
    *project = candidate;
    Ok(project
        .timeline_clips
        .len()
        .checked_sub(1)
        .map(|last| selected_index.min(last)))
}

/// 選択した clip を一度に消す。リンク相手も同じ link group なら一緒に消す（単体削除と同じ）。
pub fn delete_timeline_clips(project: &mut Project, clip_ids: &[String]) -> EditResult {
    if clip_ids.is_empty() {
        return fail("削除する timeline clip がありません");
    }
    let mut candidate = project.clone();
    let mut erased: HashSet<String> = HashSet::new();
    for id in clip_ids {
        let Some(clip) = candidate.clip(id) else {
            return fail("削除する timeline clip がありません");
        };
        erased.insert(clip.id.clone());
        if !clip.link_group_id.is_empty() {
            let group = clip.link_group_id.clone();
            erased.extend(
                candidate
                    .timeline_clips
                    .iter()
                    .filter(|other| other.link_group_id == group)
                    .map(|other| other.id.clone()),
            );
        }
    }
    candidate
        .timeline_clips
        .retain(|clip| !erased.contains(&clip.id));
    erase_linked_subtitles(&mut candidate, &erased);
    finalize_timeline_candidate(&mut candidate)?;
    *project = candidate;
    Ok(project.timeline_clips.len().checked_sub(1))
}

/// 文字 clip を指定位置へ置く。中身が空、または既存 clip と重なるときは Project を変えない。
/// 文字 clip の表示名。本文の 1 行目を 24 文字までにする。
fn text_clip_name(content: &str) -> String {
    let line = content.trim().lines().next().unwrap_or_default();
    if line.chars().count() > 24 {
        let mut short: String = line.chars().take(24).collect();
        short.push('…');
        short
    } else {
        line.to_owned()
    }
}

/// 文字 clip の本文と書式を置き換える。表示名は本文から作り直す。値の範囲は timeline の検証が決める。
pub fn edit_text_clip(
    project: &mut Project,
    clip_id: &str,
    text: crate::model::TextClipData,
) -> EditResult {
    let Some(index) = project.clip_index(clip_id) else {
        return fail("編集する文字 clip がありません");
    };
    if project.timeline_clips[index].kind != TimelineClipKind::Text {
        return fail("編集する文字 clip がありません");
    }
    if text.content.trim().is_empty() {
        return fail("文字 clip の本文を空にはできません");
    }
    let mut candidate = project.clone();
    let clip = &mut candidate.timeline_clips[index];
    clip.name = text_clip_name(&text.content);
    clip.text = text;
    if candidate == *project {
        return Ok(None);
    }
    commit_candidate(project, candidate, Some(index))
}

pub fn place_text_clip(
    project: &mut Project,
    track: TrackRef,
    timeline_start_frame: i64,
    content: &str,
    new_id: &mut dyn FnMut() -> String,
) -> EditResult {
    let content = content.trim();
    if content.is_empty() || !project.is_valid_track_ref(track) || timeline_start_frame < 0 {
        return fail("文字 clip の内容または位置が不正です");
    }
    if track.kind != TrackKind::Video {
        return fail("文字 clip は video track に置きます");
    }
    let length = crate::model::default_still_clip_frames(project.timeline_fps);
    let id = new_id();
    if id.is_empty() {
        return fail("文字 clip の ID を作れません");
    }
    let clip = TimelineClip {
        kind: TimelineClipKind::Text,
        name: text_clip_name(content),
        id,
        source_fps: project.timeline_fps,
        source_frame_count: length,
        source_in_frame: 0,
        source_out_frame: length,
        timeline_start_frame,
        track,
        text: crate::model::TextClipData {
            content: content.to_owned(),
            ..crate::model::TextClipData::default()
        },
        ..TimelineClip::default()
    };
    place_timeline_clip_at(project, clip, track, timeline_start_frame)
}

/// kind の track を `first_index` から順に試し、mute されていない track で `place` が成功したら確定する。
/// 既存 track に空きが無ければ track を上へ足していく。足した track は空で mute もされていないので、
/// そこへ置けなければ別の理由の失敗としてそのまま返す。
fn place_on_first_free_track(
    project: &mut Project,
    kind: TrackKind,
    first_index: usize,
    mut place: impl FnMut(&mut Project, usize) -> EditResult,
) -> EditResult {
    let mut candidate = project.clone();
    let mut track_index = first_index;
    loop {
        let added_track = track_index >= candidate.tracks(kind).len();
        if added_track {
            add_track(&mut candidate, kind)?;
        }
        if !candidate.tracks(kind)[track_index].muted {
            match place(&mut candidate, track_index) {
                Ok(selected) => {
                    *project = candidate;
                    return Ok(selected);
                }
                Err(error) if added_track => return Err(error),
                Err(_) => {}
            }
        }
        track_index += 1;
    }
}

/// 再生ヘッドで最上位の active clip より上の非 mute 映像 track に文字・静止画・数式 sequence・グラフを置く。
/// 空きが無ければ映像 track を上へ足して置く。
pub fn place_still_clip_at(
    project: &mut Project,
    clip: TimelineClip,
    timeline_start_frame: i64,
) -> EditResult {
    let placeable = clip.kind.is_still()
        || matches!(
            clip.kind,
            TimelineClipKind::EquationSequence | TimelineClipKind::Graph
        );
    if !placeable || timeline_start_frame < 0 {
        return fail("配置する text / image clip または開始位置が不正です");
    }
    let highest_active = active_clips_at(project, TrackKind::Video, timeline_start_frame)
        .iter()
        .rposition(Option::is_some);
    let first = highest_active.map_or(0, |index| index + 1);
    place_on_first_free_track(project, TrackKind::Video, first, |candidate, index| {
        place_timeline_clip_at(
            candidate,
            clip.clone(),
            TrackRef::video(index),
            timeline_start_frame,
        )
    })
}

/// 指定位置で空いている非 mute の audio track（A1 から順）に音声 clip を置く。空きが無ければ audio track を足す。
pub fn place_audio_clip_at(
    project: &mut Project,
    clip: TimelineClip,
    timeline_start_frame: i64,
) -> EditResult {
    if clip.kind != TimelineClipKind::Audio || timeline_start_frame < 0 {
        return fail("配置する audio clip または開始位置が不正です");
    }
    place_on_first_free_track(project, TrackKind::Audio, 0, |candidate, index| {
        place_timeline_clip_at(
            candidate,
            clip.clone(),
            TrackRef::audio(index),
            timeline_start_frame,
        )
    })
}

/// 動画素材を再生ヘッドの位置へ置く。V1 から順に、置く区間が空いている非 mute の video track へ置き、
/// どこにも置けなければ video track を上へ足す（C++ 版 `addVideoClip` は毎回新しい track を足すが、空きのある
/// track を使う通常の NLE の配置にした）。リンク音声も A1 から順に空いている非 mute の audio track へ置き、
/// 無ければ audio track を足す。既存の clip は動かさない。
pub fn place_video_media_at(
    project: &mut Project,
    primary: TimelineClip,
    linked_audio: Option<TimelineClip>,
    timeline_start_frame: i64,
) -> EditResult {
    if primary.kind != TimelineClipKind::Video || timeline_start_frame < 0 {
        return fail("配置する video clip または開始位置が不正です");
    }
    place_on_first_free_track(project, TrackKind::Video, 0, |attempt, video_index| {
        let video_track = TrackRef::video(video_index);
        match &linked_audio {
            None => {
                place_timeline_clip_at(attempt, primary.clone(), video_track, timeline_start_frame)
            }
            Some(audio) => {
                place_on_first_free_track(attempt, TrackKind::Audio, 0, |pair, audio_index| {
                    place_linked_av_pair_at(
                        pair,
                        primary.clone(),
                        video_track,
                        audio.clone(),
                        TrackRef::audio(audio_index),
                        timeline_start_frame,
                    )
                })
            }
        }
    })
}

/// プロジェクトパネルや外部からのドロップで、指定した track・位置へ素材を置く。
/// - target.index が track 数と等しければ、その種別の track を末尾に足して置く
/// - linked_audio があれば、ドロップした行の種別に合う側を target へ置き、相手はもう一方の種別で空いている
///   最初の非 mute track（無ければ新しい track）へ置く
/// - target に置けない（種別が違う・既存の clip と重なる）ときは上書きせず、同じ種別で空いている最初の
///   非 mute track（無ければ新しい track）へ置く。時刻は変えない
/// - 開始位置が負なら 0 へ寄せる。target.index が範囲外なら失敗する
pub fn place_media_at_drop(
    project: &mut Project,
    primary: TimelineClip,
    linked_audio: Option<TimelineClip>,
    mut target: TrackRef,
    timeline_start_frame: i64,
) -> EditResult {
    let track_count = project.tracks(target.kind).len();
    if target.index > track_count {
        return fail("ドロップ先の track が不正です");
    }
    let start = timeline_start_frame.max(0);
    // リンク対は、ドロップした行の種別に合う側をその track へ置く。
    let on_target = match (&linked_audio, target.kind) {
        (Some(audio), TrackKind::Audio) => audio,
        _ => &primary,
    };
    let own_kind = if on_target.kind == TimelineClipKind::Audio {
        TrackKind::Audio
    } else {
        TrackKind::Video
    };
    let mut alone = on_target.clone();
    alone.link_group_id.clear();
    let fits_at = |base: &Project, track: TrackRef| {
        let mut probe = base.clone();
        place_timeline_clip_at(&mut probe, alone.clone(), track, start).is_ok()
    };
    let mut candidate = project.clone();
    let kind_fits = on_target.kind.fits_track(target.kind);
    if kind_fits && target.index == track_count {
        add_track(&mut candidate, target.kind)?;
    }
    // ドロップした行に置けないときは、上書きせずに同じ種別で空いている track へ回す。
    if !kind_fits || !fits_at(&candidate, target) {
        let mut chosen = 0;
        let mut scratch = candidate.clone();
        place_on_first_free_track(&mut scratch, own_kind, 0, |attempt, index| {
            chosen = index;
            place_timeline_clip_at(
                attempt,
                alone.clone(),
                TrackRef::new(own_kind, index),
                start,
            )
        })?;
        while candidate.tracks(own_kind).len() <= chosen {
            add_track(&mut candidate, own_kind)?;
        }
        target = TrackRef::new(own_kind, chosen);
    }
    let result = match linked_audio {
        None => place_timeline_clip_at(&mut candidate, primary, target, start),
        Some(audio) => {
            // リンク相手は、もう一方の種別で空いている最初の track（無ければ新しい track）へ置く。
            let partner_kind = match target.kind {
                TrackKind::Video => TrackKind::Audio,
                TrackKind::Audio => TrackKind::Video,
            };
            place_on_first_free_track(&mut candidate, partner_kind, 0, |attempt, partner_index| {
                let partner = TrackRef::new(partner_kind, partner_index);
                let (video_track, audio_track) = match target.kind {
                    TrackKind::Video => (target, partner),
                    TrackKind::Audio => (partner, target),
                };
                place_linked_av_pair_at(
                    attempt,
                    primary.clone(),
                    video_track,
                    audio.clone(),
                    audio_track,
                    start,
                )
            })
        }
    };
    let selected = result?;
    *project = candidate;
    Ok(selected)
}

/// リンク済みの video / audio を 1 つの candidate へ追加し、不変条件を満たしたときだけ確定する。
pub fn place_linked_av_pair_at(
    project: &mut Project,
    mut video: TimelineClip,
    video_track: TrackRef,
    mut audio: TimelineClip,
    audio_track: TrackRef,
    timeline_start_frame: i64,
) -> EditResult {
    if !project.is_valid_track_ref(video_track)
        || !project.is_valid_track_ref(audio_track)
        || timeline_start_frame < 0
    {
        return fail("リンクclipの追加先trackまたはstart frameが不正です");
    }
    if video.kind == TimelineClipKind::Audio
        || audio.kind != TimelineClipKind::Audio
        || video_track.kind != TrackKind::Video
        || audio_track.kind != TrackKind::Audio
        || !video.is_linked()
        || video.link_group_id != audio.link_group_id
    {
        return fail("リンクclipは同じlink IDを持つvideo/audioの1組である必要があります");
    }
    video.track = video_track;
    video.timeline_start_frame = timeline_start_frame;
    audio.track = audio_track;
    audio.timeline_start_frame = timeline_start_frame;
    let mut candidate = project.clone();
    let video_index = candidate.timeline_clips.len();
    candidate.timeline_clips.push(video);
    candidate.timeline_clips.push(audio);
    commit_candidate(project, candidate, Some(video_index))
}

/// clip のリンク（相手の clip と、文字起こしで作った字幕とのリンク）を外す。
pub fn unlink_timeline_clip(project: &mut Project, clip_id: &str) -> EditResult {
    let Some(index) = project.clip_index(clip_id) else {
        return fail("リンク解除する timeline clip がありません");
    };
    let mut candidate = project.clone();
    let group = candidate.timeline_clips[index].link_group_id.clone();
    let mut unlinked: HashSet<String> = HashSet::from([clip_id.to_owned()]);
    if !group.is_empty() {
        for clip in &mut candidate.timeline_clips {
            if clip.link_group_id == group {
                unlinked.insert(clip.id.clone());
                clip.link_group_id.clear();
            }
        }
    }
    if unlink_subtitles(&mut candidate, &unlinked) == 0 && group.is_empty() {
        return fail("選択した clip はリンクされていません");
    }
    commit_candidate(project, candidate, Some(index))
}

/// clip の端のドラッグの種類。動かせる範囲の決め方が違う。
/// - Trim: 素材の範囲と 1 frame 以上の尺。left 端は timeline 先頭より前へ出さない
/// - Ripple: Trim と同じだが、left 端でも clip の開始位置は動かない
/// - Roll: 操作した clip と接している clip の両方で素材の範囲と 1 frame 以上の尺
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EdgeEditKind {
    Trim,
    Ripple,
    Roll,
}

/// 端のドラッグ量（project frame）を、動かせる範囲で止めた値にする。Linked ならリンク相手も同じ量を動かせる
/// 範囲に止める。素材の端を越えるドラッグは失敗させずに端で止める。確定と drag 中の表示の両方が使う。
pub fn clamp_edge_edit(
    project: &Project,
    clip_id: &str,
    edge: TrimEdge,
    kind: EdgeEditKind,
    delta: i64,
    link_mode: LinkMode,
) -> ProjectResult<i64> {
    let Some(index) = project.clip_index(clip_id) else {
        return fail("伸縮する timeline clip がありません");
    };
    let mut range = (i64::MIN, i64::MAX);
    for target in edit_targets(project, index, link_mode) {
        let clip = &project.timeline_clips[target];
        if kind == EdgeEditKind::Roll {
            let Some(neighbor) = adjacent_clip_index(project, target, edge)? else {
                // リンク相手が編集点を持たない（L / J カット）なら、相手は範囲を縛らない。
                if target != index {
                    continue;
                }
                return fail("ローリング編集には接している隣の clip が必要です");
            };
            let (outgoing, incoming) = match edge {
                TrimEdge::Right => (target, neighbor),
                TrimEdge::Left => (neighbor, target),
            };
            narrow_range(
                &mut range,
                edge_range(project, &project.timeline_clips[outgoing], TrimEdge::Right)?,
            );
            narrow_range(
                &mut range,
                edge_range(project, &project.timeline_clips[incoming], TrimEdge::Left)?,
            );
            continue;
        }
        narrow_range(&mut range, edge_range(project, clip, edge)?);
        // 通常の trim は left 端を動かすと clip の開始位置も動く。timeline 先頭より前へは出さない。
        // リップルは開始位置を保つので、この制約を受けない。
        if kind == EdgeEditKind::Trim {
            if edge == TrimEdge::Left {
                range.0 = range.0.max(-clip.timeline_start_frame);
            }
            // 接している隣の clip の方へは延ばさない（延ばすと重なる）。縮める向きは自由で、
            // 離れればトランジションは消える。
            if adjacent_clip_index(project, target, edge)?.is_some() {
                match edge {
                    TrimEdge::Right => range.1 = range.1.min(0),
                    TrimEdge::Left => range.0 = range.0.max(0),
                }
            }
        }
    }
    Ok(if range.0 > range.1 {
        0
    } else {
        delta.clamp(range.0, range.1)
    })
}

/// [`clamp_edge_edit`] で止めた量を返す。1 frame も動かせなければ失敗にする。
pub(crate) fn clamped_edge_delta(
    project: &Project,
    clip_id: &str,
    edge: TrimEdge,
    kind: EdgeEditKind,
    delta: i64,
    link_mode: LinkMode,
) -> ProjectResult<i64> {
    match clamp_edge_edit(project, clip_id, edge, kind, delta, link_mode)? {
        0 => fail("素材の端または clip の最小尺に達しているため、これ以上伸縮できません"),
        clamped => Ok(clamped),
    }
}

/// Linked ならリンク相手の同じ側の端も同じ量だけ動かす。動かせる範囲を越える量は止め、
/// 1 frame も動かせなければ失敗する。
pub fn trim_timeline_clip(
    project: &mut Project,
    clip_id: &str,
    edge: TrimEdge,
    delta: i64,
    link_mode: LinkMode,
) -> EditResult {
    let Some(index) = project.clip_index(clip_id) else {
        return fail("trim する timeline clip がありません");
    };
    let mut candidate = project.clone();
    let delta = if candidate.timeline_clips[index].kind == TimelineClipKind::EquationSequence {
        delta
    } else {
        clamped_edge_delta(
            &candidate,
            clip_id,
            edge,
            EdgeEditKind::Trim,
            delta,
            link_mode,
        )?
    };
    for target in edit_targets(&candidate, index, link_mode) {
        let mut clip = candidate.timeline_clips[target].clone();
        trim_clip_boundary(&candidate, &mut clip, edge, delta)?;
        candidate.timeline_clips[target] = clip;
    }
    commit_mapping_edit(project, candidate, Some(index))
}

/// clip の片端を timeline frame の位置へ動かした clip（Project は変えない）。素材範囲は trim・分割と同じ規則で
/// 決める。端がちょうどその位置に来ない（速度や fps の違いで丸まる）、または素材の範囲を超えるなら失敗する。
pub fn clip_with_edge_at(
    project: &Project,
    clip: &TimelineClip,
    edge: TrimEdge,
    timeline_frame: i64,
) -> ProjectResult<TimelineClip> {
    let (start, end) = clip_interval(project, clip)?;
    let mut moved = clip.clone();
    let delta = timeline_frame
        - match edge {
            TrimEdge::Left => start,
            TrimEdge::Right => end,
        };
    if delta != 0 {
        trim_clip_boundary(project, &mut moved, edge, delta)?;
    }
    let (moved_start, moved_end) = clip_interval(project, &moved)?;
    let exact = match edge {
        TrimEdge::Left => moved_start == timeline_frame && moved_end == end,
        TrimEdge::Right => moved_start == start && moved_end == timeline_frame,
    };
    if !exact
        || moved.source_in_frame < 0
        || moved.source_out_frame <= moved.source_in_frame
        || moved.source_out_frame > moved.source_frame_count
    {
        return fail(format!(
            "clip の端を素材 frame へ一意に換算できません: {}",
            clip.name
        ));
    }
    Ok(moved)
}

/// frame で clip を 2 つに分割する（レーザーツール）。frame は clip の内側（start < frame < end）でなければ
/// ならない。Linked でリンク相手も frame を内側に含むなら同時に分割し、右半分どうしを新しい link group で結ぶ。
/// 片方だけを切ったら右半分は未リンク。フェードは左半分が in、右半分が out を引き継ぐ。
/// 選択は最初に指定した clip の左半分。
pub fn split_timeline_clips(
    project: &mut Project,
    clip_ids: &[String],
    frame: i64,
    new_id: &mut dyn FnMut() -> String,
    link_mode: LinkMode,
) -> EditResult {
    let Some(first_id) = clip_ids.first() else {
        return fail("分割する timeline clip がありません");
    };
    let mut candidate = project.clone();
    let original_count = candidate.timeline_clips.len();
    let requested = mark_clips(&candidate, clip_ids, "分割する timeline clip がありません")?;
    for (index, clip) in candidate.timeline_clips.iter().enumerate() {
        if !requested[index] {
            continue;
        }
        let (start, end) = clip_interval(&candidate, clip)?;
        if frame <= start || frame >= end {
            return fail(format!("分割位置が clip の内側にありません: {}", clip.name));
        }
    }
    // リンク相手は、Linked で、かつ分割位置を内側に含むときだけ一緒に切る。
    let mut split = requested.clone();
    if link_mode == LinkMode::Linked {
        include_linked_counterparts(&candidate, &mut split);
    }
    for index in 0..original_count {
        if split[index] && !requested[index] {
            let (start, end) = clip_interval(&candidate, &candidate.timeline_clips[index])?;
            split[index] = frame > start && frame < end;
        }
    }
    // link group の両方を切ったときだけ、右半分どうしを新しい group で結ぶ。
    let mut split_per_group: HashMap<String, usize> = HashMap::new();
    for (index, clip) in candidate.timeline_clips.iter().enumerate() {
        if split[index] && clip.is_linked() {
            *split_per_group
                .entry(clip.link_group_id.clone())
                .or_default() += 1;
        }
    }
    let mut right_groups: HashMap<String, String> = HashMap::new();
    let mut paired: Vec<&String> = split_per_group
        .iter()
        .filter(|(_, count)| **count == 2)
        .map(|(group, _)| group)
        .collect();
    paired.sort();
    for group in paired {
        right_groups.insert(group.clone(), new_id());
    }
    for (index, _) in split.iter().enumerate().filter(|(_, split)| **split) {
        let mut left = candidate.timeline_clips[index].clone();
        let mut right = left.clone();
        let (start, end) = clip_interval(&candidate, &left)?;
        trim_clip_boundary(&candidate, &mut left, TrimEdge::Right, frame - end)?;
        trim_clip_boundary(&candidate, &mut right, TrimEdge::Left, frame - start)?;
        // 両半分とも同じ timeline 境界から換算するので、素材上の境界は一致するはず。文字・静止画は素材の
        // 時間軸を持たず、trim が両半分とも in = 0 に振り直すので、境界の一致は timeline 上で見る。
        let consistent = if left.has_synthetic_source_domain() {
            left.timeline_start_frame + left.source_out_frame == right.timeline_start_frame
        } else {
            left.source_out_frame == right.source_in_frame
        };
        if !consistent {
            return fail(format!(
                "分割位置を素材 frame へ一意に換算できません: {}",
                left.name
            ));
        }
        if matches!(
            left.kind,
            TimelineClipKind::EquationSequence | TimelineClipKind::Graph
        ) && right.timeline_start_frame != frame
        {
            return fail("分割位置を sequence の整数素材境界で正確に表現できません");
        }
        left.effects.fade_out_frames = 0;
        left.effects.fade_in_frames = left.effects.fade_in_frames.min(left.source_duration());
        right.effects.fade_in_frames = 0;
        clear_math_intro(&mut right);
        right.effects.fade_out_frames = right.effects.fade_out_frames.min(right.source_duration());
        assign_split_ids(&left.id, &mut right, new_id)?;
        relink_subtitles_after_split(&mut candidate, &left.id, &right.id, frame, new_id);
        if right.is_linked() {
            right.link_group_id = right_groups
                .get(&right.link_group_id)
                .cloned()
                .unwrap_or_default();
        }
        for transition in &mut candidate.timeline_transitions {
            if transition.outgoing_clip_id == left.id {
                transition.outgoing_clip_id = right.id.clone();
            }
        }
        candidate.timeline_clips[index] = left;
        candidate.timeline_clips.push(right);
    }
    let selected = project.clip_index(first_id);
    commit_candidate(project, candidate, selected)
}

/// frame を内側に含む clip（全 track）。Shift+クリックの全 track 分割に使う。
pub fn clip_ids_spanning_frame(project: &Project, frame: i64) -> Vec<String> {
    project
        .timeline_clips
        .iter()
        .filter(|clip| {
            clip_interval(project, clip).is_ok_and(|(start, end)| frame > start && frame < end)
        })
        .map(|clip| clip.id.clone())
        .collect()
}

/// among のうち frame を内側に含む clip。順序は among のまま。存在しない ID は含めない。
pub fn clip_ids_spanning_frame_among(
    project: &Project,
    frame: i64,
    among: &[String],
) -> Vec<String> {
    among
        .iter()
        .filter(|id| {
            project.clip(id).is_some_and(|clip| {
                clip_interval(project, clip).is_ok_and(|(start, end)| frame > start && frame < end)
            })
        })
        .cloned()
        .collect()
}

/// clip の音量を step_db だけ上げ下げする。音量を持つのは audio clip だけなので、映像側はリンク相手の audio clip
/// を変える。音量 key があれば base と全 key を同じ規則で変え、0% の key（無音の形）は変えない。
/// 対象が無い、または全対象が既に上限・下限で何も変わらないなら失敗する。
pub fn step_clip_volume(project: &mut Project, clip_ids: &[String], step_db: f64) -> EditResult {
    let mut candidate = project.clone();
    let mut marked = mark_clips(
        &candidate,
        clip_ids,
        "音量を変える timeline clip がありません",
    )?;
    include_linked_counterparts(&candidate, &mut marked);
    let mut first_target = None;
    let mut changed = false;
    for (index, clip) in candidate.timeline_clips.iter_mut().enumerate() {
        if !marked[index] || clip.kind != TimelineClipKind::Audio {
            continue;
        }
        first_target.get_or_insert(index);
        let Some(stepped) = step_volume_percent_by_db(clip.effects.volume_percent, step_db) else {
            return fail(format!("音量を段階的に変えられません: {}", clip.name));
        };
        changed |= stepped != clip.effects.volume_percent;
        clip.effects.volume_percent = stepped;
        for key in &mut clip.effects.volume_keys {
            if key.value == 0.0 {
                continue;
            }
            let Some(stepped) = step_volume_percent_by_db(key.value, step_db) else {
                return fail(format!("音量キーを段階的に変えられません: {}", clip.name));
            };
            changed |= stepped != key.value;
            key.value = stepped;
        }
    }
    let Some(first_target) = first_target else {
        return fail("音量を変えられる audio clip がありません");
    };
    if !changed {
        return fail(if step_db > 0.0 {
            "音量は既に上限です"
        } else {
            "音量は既に下限です"
        });
    }
    commit_candidate(project, candidate, Some(first_target))
}

/// clip（とリンク相手）の有効/無効を切り換える（Shift+E）。対象に 1 つでも有効な clip があれば全部を無効にし、
/// 全部が無効なら全部を有効にする。
pub fn toggle_clips_enabled(project: &mut Project, clip_ids: &[String]) -> EditResult {
    let Some(first_id) = clip_ids.first() else {
        return fail("有効/無効を切り換える timeline clip がありません");
    };
    let mut candidate = project.clone();
    let mut marked = mark_clips(
        &candidate,
        clip_ids,
        "有効/無効を切り換える timeline clip がありません",
    )?;
    include_linked_counterparts(&candidate, &mut marked);
    let any_enabled = candidate
        .timeline_clips
        .iter()
        .zip(&marked)
        .any(|(clip, marked)| *marked && clip.enabled);
    for (clip, marked) in candidate.timeline_clips.iter_mut().zip(&marked) {
        if *marked {
            clip.enabled = !any_enabled;
        }
    }
    let selected = project.clip_index(first_id);
    commit_candidate(project, candidate, selected)
}

/// 既定のトランジションの長さ（1 秒）を timeline frame で表した値。最低 1 frame。
pub fn default_transition_frames(timeline_fps: Rational) -> i64 {
    if !timeline_fps.is_positive() {
        return 1;
    }
    ((timeline_fps.num + timeline_fps.den / 2) / timeline_fps.den).max(1)
}

/// clip（とリンク相手）の先頭と末尾に `timeline_frames` の長さのフェードを付ける（Shift+D を clip 選択で押したとき）。
/// フェードは素材 frame で持つので、clip ごとに速度込みで換算する。尺が足りなければ前後で分け合う（先頭が半分を
/// 切り上げで取る）。トランジションのある端は変えない。何も変わらなければ失敗する。
pub fn apply_default_clip_fades(
    project: &mut Project,
    clip_ids: &[String],
    timeline_frames: i64,
) -> EditResult {
    let Some(first_id) = clip_ids.first().filter(|_| timeline_frames >= 1) else {
        return fail("フェードを付ける timeline clip がありません");
    };
    let mut candidate = project.clone();
    let mut marked = mark_clips(
        &candidate,
        clip_ids,
        "フェードを付ける timeline clip がありません",
    )?;
    include_linked_counterparts(&candidate, &mut marked);
    let heads: HashSet<String> = candidate
        .timeline_transitions
        .iter()
        .map(|transition| transition.incoming_clip_id.clone())
        .collect();
    let tails: HashSet<String> = candidate
        .timeline_transitions
        .iter()
        .map(|transition| transition.outgoing_clip_id.clone())
        .collect();
    let fps = candidate.timeline_fps;
    for (index, clip) in candidate.timeline_clips.iter_mut().enumerate() {
        if !marked[index] {
            continue;
        }
        let named = |error: crate::ProjectError| {
            crate::ProjectError::new(format!("{}: {error}", clip.name))
        };
        let duration = clip.source_duration();
        // timeline 上の境界から timeline_frames だけ内側の位置を素材 frame へ戻した差が、その端のフェードの
        // 素材 frame 数になる（速度 2 倍なら素材は 2 倍進む）。
        let in_boundary =
            clip_source_boundary_to_timeline(clip, clip.source_in_frame, fps).map_err(named)?;
        let out_boundary =
            clip_source_boundary_to_timeline(clip, clip.source_out_frame, fps).map_err(named)?;
        // 内側の位置は clip の中に収める（1 秒より短い clip で反対側の端を越えない）。
        let inner = timeline_frames.min(out_boundary - in_boundary);
        let head_source =
            clip_timeline_boundary_to_source(clip, in_boundary + inner, fps).map_err(named)?;
        let tail_source =
            clip_timeline_boundary_to_source(clip, out_boundary - inner, fps).map_err(named)?;
        let wanted_in = (head_source - clip.source_in_frame).clamp(0, duration);
        let wanted_out = (clip.source_out_frame - tail_source).clamp(0, duration);
        let keep_head = heads.contains(&clip.id);
        let keep_tail = tails.contains(&clip.id);
        let mut fade_in = if keep_head {
            clip.effects.fade_in_frames
        } else {
            wanted_in
        };
        let mut fade_out = if keep_tail {
            clip.effects.fade_out_frames
        } else {
            wanted_out
        };
        if !keep_head && !keep_tail {
            fade_in = fade_in.min((duration + 1) / 2);
            fade_out = fade_out.min(duration - fade_in);
        } else if keep_head {
            fade_out = fade_out.min(duration - fade_in);
        } else {
            fade_in = fade_in.min(duration - fade_out);
        }
        clip.effects.fade_in_frames = fade_in;
        clip.effects.fade_out_frames = fade_out;
    }
    if candidate == *project {
        return fail("フェードは既に付いています");
    }
    let selected = project.clip_index(first_id);
    commit_candidate(project, candidate, selected)
}

/// clip の edge 側で接している同じ track の clip の ID。
pub fn touching_clip_id(project: &Project, clip_id: &str, edge: TrimEdge) -> Option<String> {
    let index = project.clip_index(clip_id)?;
    let neighbor = adjacent_clip_index(project, index, edge).ok()??;
    Some(project.timeline_clips[neighbor].id.clone())
}
