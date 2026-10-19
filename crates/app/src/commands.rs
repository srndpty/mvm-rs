//! 編集 command。判定は project が行い、ここは undo 1 段と選択の更新だけを担う。

use std::collections::HashSet;

use mvm_core::Rational;
use mvm_project::clip_effects::ClipKeyKind;
use mvm_project::timeline::{
    ClipSpeedDurationEdit, CopyPlacement, LinkMode, SelectDirection, SpeedDurationInput, TrimEdge,
    add_timeline_marker, apply_default_clip_fades, apply_default_edit_transition,
    apply_math_transform_transition, clip_ids_from_frame, clip_ids_spanning_frame, clip_interval,
    default_transition_frames, delete_clip_key, delete_timeline_clips, delete_timeline_marker,
    delete_timeline_transition, edit_clip_key, edit_text_clip, effect_edit_frame, move_clips,
    place_copied_clips, place_text_clip, rate_stretch_timeline_clip, ripple_trim_timeline_clip,
    roll_timeline_edit, set_clip_effect_at, set_clip_fades, set_clip_key_animation,
    set_clip_key_interpolation, set_clip_speed_duration, set_mark_in, set_mark_out,
    set_subtitle_visible, set_timeline_transition_span, set_track_muted, set_track_solo,
    slide_timeline_clip, slip_timeline_clip, split_timeline_clips, step_clip_volume,
    toggle_clip_key, toggle_clips_enabled, touching_clip_id, trim_timeline_clip,
};
use mvm_project::{MediaItem, Project, ProjectResult, TimelineClip, TrackKind, TrackRef};

use crate::error::AppError;
use crate::session::{Clipboard, Session};

pub(crate) struct IdFactory {
    pub(crate) serial: u64,
    used: HashSet<String>,
}

impl IdFactory {
    pub(crate) fn next(&mut self) -> String {
        for _ in 0..1_000_000 {
            let id = format!("c{}", self.serial);
            self.serial = self.serial.saturating_add(1);
            if self.used.insert(id.clone()) {
                return id;
            }
        }
        let id = format!("c-overflow-{}", self.used.len());
        self.used.insert(id.clone());
        id
    }
}

impl Session {
    pub fn undo(&mut self) -> Result<(), AppError> {
        let Some(previous) = self.history.undo(self.project.clone()) else {
            return Err(AppError::new("戻す編集がありません"));
        };
        self.project = previous;
        self.finish_edit();
        Ok(())
    }

    pub fn redo(&mut self) -> Result<(), AppError> {
        let Some(next) = self.history.redo(self.project.clone()) else {
            return Err(AppError::new("やり直す編集がありません"));
        };
        self.project = next;
        self.finish_edit();
        Ok(())
    }

    pub fn select_only(&mut self, id: String) {
        self.subtitle_selection.clear();
        self.selection = vec![id];
        self.selected_edge = None;
        self.selected_marker = None;
    }

    pub fn toggle_selected(&mut self, id: String) {
        self.subtitle_selection.clear();
        if let Some(index) = self.selection.iter().position(|item| item == &id) {
            self.selection.remove(index);
        } else {
            self.selection.push(id);
        }
        self.selected_edge = None;
        self.selected_marker = None;
    }

    pub fn select_ids(&mut self, ids: Vec<String>) {
        if !ids.is_empty() {
            self.subtitle_selection.clear();
        }
        self.selection = ids;
        self.selected_edge = None;
        self.selected_marker = None;
    }

    pub fn select_all(&mut self) {
        self.subtitle_selection.clear();
        self.selection = self
            .project
            .timeline_clips
            .iter()
            .map(|clip| clip.id.clone())
            .collect();
        self.selected_edge = None;
        self.selected_marker = None;
    }

    pub fn select_edge(&mut self, id: String, edge: TrimEdge) {
        self.selected_edge = Some((id, edge));
    }

    pub fn selected_edge(&self) -> Option<(&str, TrimEdge)> {
        self.selected_edge
            .as_ref()
            .map(|(id, edge)| (id.as_str(), *edge))
    }

    pub fn select_marker(&mut self, frame: Option<i64>) {
        self.selected_marker = frame;
        if frame.is_some() {
            self.selection.clear();
            self.selected_edge = None;
        }
    }

    pub fn selected_marker(&self) -> Option<i64> {
        self.selected_marker
    }

    pub fn has_clipboard(&self) -> bool {
        self.clipboard
            .as_ref()
            .is_some_and(|clipboard| !clipboard.clips.is_empty())
            || self
                .subtitle_clipboard
                .as_ref()
                .is_some_and(|cues| !cues.is_empty())
    }

    pub fn copy_selection(&mut self) -> Result<(), AppError> {
        if !self.subtitle_selection.is_empty() {
            return self.copy_subtitles();
        }
        let clips = self.ordered_selection()?;
        let media = self.media_for(&clips);
        self.clipboard = Some(Clipboard {
            clips,
            media,
            fps: self.project.timeline_fps,
        });
        self.subtitle_clipboard = None;
        Ok(())
    }

    pub fn cut_selection(&mut self) -> Result<(), AppError> {
        self.copy_selection()?;
        self.delete_selection()
    }

    pub fn paste(&mut self) -> Result<(), AppError> {
        if self.subtitle_clipboard.is_some() {
            return self.paste_subtitles();
        }
        let Some(clipboard) = self.clipboard.clone() else {
            return Err(AppError::new("貼り付ける clip がありません"));
        };
        self.ensure_media_files(&clipboard.media, &clipboard.clips)?;
        let frame = self.playhead_frame;
        self.edit_with_ids(|project, new_id| {
            place_copied_clips(
                project,
                &clipboard.clips,
                &clipboard.media,
                clipboard.fps,
                frame,
                CopyPlacement::FindFreeTrack,
                new_id,
            )
        })
    }

    pub fn duplicate_selection(&mut self) -> Result<(), AppError> {
        if !self.subtitle_selection.is_empty() {
            return self.duplicate_subtitles();
        }
        let clips = self.ordered_selection()?;
        let media = self.media_for(&clips);
        self.ensure_media_files(&media, &clips)?;
        let frame = self.playhead_frame;
        let fps = self.project.timeline_fps;
        self.edit_with_ids(|project, new_id| {
            place_copied_clips(
                project,
                &clips,
                &media,
                fps,
                frame,
                CopyPlacement::FindFreeTrack,
                new_id,
            )
        })
    }

    /// ドラッグで見せた位置へ複製する。clipboard は変えない。
    pub fn duplicate_at(
        &mut self,
        anchor_id: &str,
        destination: TrackRef,
        timeline_start_frame: i64,
    ) -> Result<(), AppError> {
        let Some(anchor) = self.project.clip(anchor_id).cloned() else {
            return Err(AppError::new("複製する clip がありません"));
        };
        if destination.kind != anchor.track.kind {
            return Err(AppError::new("異なる種別の track へ複製できません"));
        }
        let clips = self.clips_with_anchor(anchor_id);
        if clips.is_empty() {
            return Err(AppError::new("複製する clip がありません"));
        }
        let first = clips
            .iter()
            .map(|clip| clip.timeline_start_frame)
            .min()
            .unwrap_or(anchor.timeline_start_frame);
        let destination_first = timeline_start_frame - (anchor.timeline_start_frame - first);
        if destination_first < 0 {
            return Err(AppError::new("複製先が timeline の先頭より前です"));
        }
        let anchor_index = i64::try_from(anchor.track.index)
            .map_err(|_| AppError::new("複製元の track が範囲外です"))?;
        let dest_index = i64::try_from(destination.index)
            .map_err(|_| AppError::new("複製先の track が範囲外です"))?;
        let delta = dest_index - anchor_index;
        let (video_delta, audio_delta) = match destination.kind {
            TrackKind::Video => (delta, 0),
            TrackKind::Audio => (0, delta),
        };
        let media = self.media_for(&clips);
        self.ensure_media_files(&media, &clips)?;
        let fps = self.project.timeline_fps;
        self.edit_with_ids(|project, new_id| {
            place_copied_clips(
                project,
                &clips,
                &media,
                fps,
                destination_first,
                CopyPlacement::Exact {
                    video_delta,
                    audio_delta,
                },
                new_id,
            )
        })
    }

    pub fn delete_selection(&mut self) -> Result<(), AppError> {
        if !self.subtitle_selection.is_empty() {
            let ids = self.subtitle_selection.clone();
            return self.delete_subtitles(&ids);
        }
        if self.selection.is_empty() {
            let Some(frame) = self.selected_marker else {
                return Err(AppError::new("削除する clip がありません"));
            };
            return self.transact(move |project| delete_timeline_marker(project, frame));
        }
        let ids = self.selection.clone();
        self.transact(move |project| delete_timeline_clips(project, &ids))
    }

    pub fn split_selection_at_playhead(&mut self) -> Result<(), AppError> {
        let ids = self.selection.clone();
        let frame = self.playhead_frame;
        self.split_at(&ids, frame, true)
    }

    pub fn split_all_at_playhead(&mut self) -> Result<(), AppError> {
        let frame = self.playhead_frame;
        let ids = clip_ids_spanning_frame(&self.project, frame);
        self.split_at(&ids, frame, true)
    }

    pub fn split_at(&mut self, ids: &[String], frame: i64, linked: bool) -> Result<(), AppError> {
        let ids = ids.to_vec();
        let mode = link_mode(linked);
        self.edit_with_ids(move |project, new_id| {
            split_timeline_clips(project, &ids, frame, new_id, mode)
        })
    }

    pub fn step_volume(&mut self, step_db: f64) -> Result<(), AppError> {
        let ids = self.selection.clone();
        self.transact(move |project| step_clip_volume(project, &ids, step_db))
    }

    pub fn toggle_enabled(&mut self) -> Result<(), AppError> {
        let ids = self.selection.clone();
        self.transact(move |project| toggle_clips_enabled(project, &ids))
    }

    pub fn apply_default_transition(&mut self) -> Result<(), AppError> {
        let frames = default_transition_frames(self.project.timeline_fps);
        if let Some((id, edge)) = self.selected_edge.clone()
            && let Some(other) = touching_clip_id(&self.project, &id, edge)
        {
            let (outgoing, incoming) = match edge {
                TrimEdge::Right => (id, other),
                TrimEdge::Left => (other, id),
            };
            return self.edit_with_ids(move |project, new_id| {
                apply_default_edit_transition(
                    project,
                    &outgoing,
                    &incoming,
                    frames,
                    LinkMode::Linked,
                    new_id,
                )
                .map(|_| None)
            });
        }
        let ids = self.selection.clone();
        self.transact(move |project| apply_default_clip_fades(project, &ids, frames))
    }

    pub fn apply_math_transform(&mut self) -> Result<(), AppError> {
        let Some((id, edge)) = self.selected_edge.clone() else {
            return Err(AppError::new("数式の変形を置く編集点がありません"));
        };
        let Some(other) = touching_clip_id(&self.project, &id, edge) else {
            return Err(AppError::new("数式の変形を置く隣の clip がありません"));
        };
        let frames = default_transition_frames(self.project.timeline_fps);
        let (outgoing, incoming) = match edge {
            TrimEdge::Right => (id, other),
            TrimEdge::Left => (other, id),
        };
        self.edit_with_ids(move |project, new_id| {
            apply_math_transform_transition(project, &outgoing, &incoming, frames, new_id)
                .map(|_| None)
        })
    }

    pub fn move_selection(
        &mut self,
        anchor_id: &str,
        destination: TrackRef,
        new_start_frame: i64,
        linked: bool,
    ) -> Result<(), AppError> {
        let ids = if self.selection.iter().any(|id| id == anchor_id) {
            self.selection.clone()
        } else {
            vec![anchor_id.to_owned()]
        };
        let anchor_id = anchor_id.to_owned();
        let mode = link_mode(linked);
        self.edit_with_ids(move |project, new_id| {
            move_clips(
                project,
                &ids,
                &anchor_id,
                destination,
                new_start_frame,
                mode,
                Some(new_id),
            )
        })
    }

    pub fn trim_edge(
        &mut self,
        clip_id: &str,
        edge: TrimEdge,
        delta: i64,
        linked: bool,
    ) -> Result<(), AppError> {
        let clip_id = clip_id.to_owned();
        let mode = link_mode(linked);
        self.transact(move |project| trim_timeline_clip(project, &clip_id, edge, delta, mode))
    }

    pub fn ripple_edge(
        &mut self,
        clip_id: &str,
        edge: TrimEdge,
        delta: i64,
        linked: bool,
    ) -> Result<(), AppError> {
        let clip_id = clip_id.to_owned();
        let mode = link_mode(linked);
        self.transact(move |project| {
            ripple_trim_timeline_clip(project, &clip_id, edge, delta, mode)
        })
    }

    pub fn roll_edge(
        &mut self,
        clip_id: &str,
        edge: TrimEdge,
        delta: i64,
        linked: bool,
    ) -> Result<(), AppError> {
        let clip_id = clip_id.to_owned();
        let mode = link_mode(linked);
        self.transact(move |project| roll_timeline_edit(project, &clip_id, edge, delta, mode))
    }

    pub fn rate_edge(
        &mut self,
        clip_id: &str,
        edge: TrimEdge,
        delta: i64,
        linked: bool,
    ) -> Result<(), AppError> {
        let clip_id = clip_id.to_owned();
        let mode = link_mode(linked);
        self.transact(move |project| {
            rate_stretch_timeline_clip(project, &clip_id, edge, delta, mode)
        })
    }

    pub fn slip_clip(&mut self, clip_id: &str, delta: i64, linked: bool) -> Result<(), AppError> {
        let clip_id = clip_id.to_owned();
        let mode = link_mode(linked);
        self.transact(move |project| slip_timeline_clip(project, &clip_id, delta, mode))
    }

    pub fn slide_clip(&mut self, clip_id: &str, delta: i64, linked: bool) -> Result<(), AppError> {
        let clip_id = clip_id.to_owned();
        let mode = link_mode(linked);
        self.transact(move |project| slide_timeline_clip(project, &clip_id, delta, mode))
    }

    pub fn edit_key(
        &mut self,
        clip_id: &str,
        kind: ClipKeyKind,
        original_frame: Option<i64>,
        frame: i64,
        percent: f64,
    ) -> Result<(), AppError> {
        let clip_id = clip_id.to_owned();
        self.transact(move |project| {
            edit_clip_key(project, &clip_id, kind, original_frame, frame, percent)
        })
    }

    pub fn delete_key(
        &mut self,
        clip_id: &str,
        kind: ClipKeyKind,
        frame: i64,
    ) -> Result<(), AppError> {
        let clip_id = clip_id.to_owned();
        self.transact(move |project| delete_clip_key(project, &clip_id, kind, frame))
    }

    /// effect を編集する clip 内の frame（再生ヘッドの位置。clip の外なら先頭）。
    pub fn effect_frame(&self, clip_id: &str) -> Result<i64, AppError> {
        effect_edit_frame(&self.project, clip_id, self.playhead_frame)
            .map_err(|error| AppError::new(error.to_string()))
    }

    /// effect の値を変える。キーのある channel は再生ヘッドの位置のキーを置き換える（無ければ足す）。
    pub fn set_effect(
        &mut self,
        clip_id: &str,
        kind: ClipKeyKind,
        value: f64,
    ) -> Result<(), AppError> {
        self.set_effects(clip_id, &[(kind, value)])
    }

    /// 複数の effect（縦横比を固定した拡大率など）を 1 つの undo で変える。どれかが失敗すれば何も変えない。
    pub fn set_effects(
        &mut self,
        clip_id: &str,
        values: &[(ClipKeyKind, f64)],
    ) -> Result<(), AppError> {
        let frame = self.effect_frame(clip_id)?;
        self.set_effects_at(clip_id, frame, values)
    }

    /// ドラッグ・入力で編集した effect を、編集を始めたときの clip 内 `frame` へ確定する。編集の途中で Project が
    /// 変わっていれば（undo・別の編集）、古い値を新しい Project へ書かずに失敗する。
    pub fn commit_effects(
        &mut self,
        clip_id: &str,
        frame: i64,
        revision: u64,
        values: &[(ClipKeyKind, f64)],
    ) -> Result<(), AppError> {
        self.expect_revision(revision)?;
        self.set_effects_at(clip_id, frame, values)
    }

    /// 編集を始めたときの revision と今が違えば失敗する。
    pub(crate) fn expect_revision(&self, revision: u64) -> Result<(), AppError> {
        if revision == self.edit_revision {
            Ok(())
        } else {
            Err(AppError::new(
                "編集中に Project が変わったため、この変更は取り消しました",
            ))
        }
    }

    fn set_effects_at(
        &mut self,
        clip_id: &str,
        frame: i64,
        values: &[(ClipKeyKind, f64)],
    ) -> Result<(), AppError> {
        self.transact(|project| {
            let mut selected = None;
            for (kind, value) in values {
                selected = set_clip_effect_at(project, clip_id, *kind, frame, *value)?.or(selected);
            }
            Ok(selected)
        })
    }

    /// 文字 clip の本文と書式を置き換える。
    pub fn edit_text(
        &mut self,
        clip_id: &str,
        text: mvm_project::TextClipData,
    ) -> Result<(), AppError> {
        self.transact(|project| edit_text_clip(project, clip_id, text))
    }

    /// 再生ヘッドの位置のキーを足す・消す。
    pub fn toggle_key(&mut self, clip_id: &str, kind: ClipKeyKind) -> Result<(), AppError> {
        let frame = self.effect_frame(clip_id)?;
        self.transact(|project| toggle_clip_key(project, clip_id, kind, frame))
    }

    pub fn set_key_animation(
        &mut self,
        clip_id: &str,
        kind: ClipKeyKind,
        enabled: bool,
    ) -> Result<(), AppError> {
        let frame = self.effect_frame(clip_id)?;
        self.transact(|project| set_clip_key_animation(project, clip_id, kind, frame, enabled))
    }

    /// 再生ヘッドの位置のキーから始まる区間の補間を変える。
    pub fn set_key_interpolation(
        &mut self,
        clip_id: &str,
        kind: ClipKeyKind,
        interpolation: mvm_project::clip_effects::KeyInterpolation,
    ) -> Result<(), AppError> {
        let frame = self.effect_frame(clip_id)?;
        self.transact(|project| {
            set_clip_key_interpolation(project, clip_id, kind, frame, interpolation)
        })
    }

    /// キーへ再生ヘッドを動かす。`forward` なら次、そうでなければ前のキー。無ければ動かさない。
    pub fn seek_key(&mut self, clip_id: &str, kind: ClipKeyKind, forward: bool) {
        let Some(clip) = self.project.clip(clip_id) else {
            return;
        };
        let start = clip.timeline_start_frame;
        let local = self.playhead_frame.saturating_sub(start);
        let keys = clip.effects.keys(kind);
        let target = if forward {
            keys.iter().find(|key| key.frame > local)
        } else {
            keys.iter().rev().find(|key| key.frame < local)
        };
        if let Some(key) = target {
            let frame = start.saturating_add(key.frame);
            self.seek(frame);
        }
    }

    pub fn set_fades(
        &mut self,
        clip_id: &str,
        fade_in_frames: i64,
        fade_out_frames: i64,
    ) -> Result<(), AppError> {
        self.transact(|project| set_clip_fades(project, clip_id, fade_in_frames, fade_out_frames))
    }

    pub fn set_speed_percent(&mut self, clip_id: &str, percent: i64) -> Result<(), AppError> {
        if !(10..=1000).contains(&percent) {
            return Err(AppError::new("速度は 10% から 1000% です"));
        }
        let speed = Rational::new(percent, 100)
            .reduced()
            .ok_or_else(|| AppError::new("速度を表せません"))?;
        let preserve = self
            .project
            .clip(clip_id)
            .is_some_and(|clip| clip.preserve_pitch);
        let clip_id = clip_id.to_owned();
        self.transact(move |project| {
            set_clip_speed_duration(
                project,
                &clip_id,
                &ClipSpeedDurationEdit {
                    input: SpeedDurationInput::Speed,
                    speed,
                    duration_frames: 1,
                    preserve_pitch: preserve,
                    ripple: false,
                    overwrite: false,
                },
                LinkMode::Linked,
            )
        })
    }

    pub fn set_transition_span(
        &mut self,
        transition_id: &str,
        frames_before_cut: i64,
        frames_after_cut: i64,
    ) -> Result<(), AppError> {
        let transition_id = transition_id.to_owned();
        self.transact(move |project| {
            set_timeline_transition_span(
                project,
                &transition_id,
                frames_before_cut,
                frames_after_cut,
                LinkMode::Linked,
            )
            .map(|_| None)
        })
    }

    pub fn delete_transition(&mut self, transition_id: &str) -> Result<(), AppError> {
        let transition_id = transition_id.to_owned();
        self.transact(move |project| {
            delete_timeline_transition(project, &transition_id).map(|_| None)
        })
    }

    pub fn add_marker(&mut self) -> Result<(), AppError> {
        let frame = self.playhead_frame;
        self.transact(move |project| add_timeline_marker(project, frame))
    }

    pub fn delete_marker(&mut self, frame: i64) -> Result<(), AppError> {
        self.transact(move |project| delete_timeline_marker(project, frame))
    }

    pub fn jump_marker(&mut self, forward: bool) {
        let current = self.playhead_frame;
        let frame = if forward {
            self.project
                .timeline_markers
                .iter()
                .copied()
                .find(|frame| *frame > current)
        } else {
            self.project
                .timeline_markers
                .iter()
                .copied()
                .rev()
                .find(|frame| *frame < current)
        };
        if let Some(frame) = frame {
            self.seek(frame);
        }
    }

    pub fn mark_in(&mut self) -> Result<(), AppError> {
        let frame = self.playhead_frame;
        self.transact(move |project| set_mark_in(project, Some(frame)))
    }

    pub fn mark_out(&mut self) -> Result<(), AppError> {
        let frame = self.playhead_frame;
        self.transact(move |project| set_mark_out(project, Some(frame)))
    }

    pub fn clear_marks(&mut self) -> Result<(), AppError> {
        self.transact(|project| {
            set_mark_in(project, None)?;
            set_mark_out(project, None)
        })
    }

    pub fn clear_mark_in(&mut self) -> Result<(), AppError> {
        self.transact(|project| set_mark_in(project, None))
    }

    pub fn clear_mark_out(&mut self) -> Result<(), AppError> {
        self.transact(|project| set_mark_out(project, None))
    }

    pub fn jump_to_in(&mut self) {
        if let Some(frame) = self.project.in_frame {
            self.seek(frame);
        }
    }

    pub fn jump_to_out(&mut self) {
        if let Some(frame) = self.project.out_frame {
            self.seek(frame);
        }
    }

    pub fn jump_edit_point(&mut self, forward: bool) {
        let mut points = Vec::new();
        for clip in &self.project.timeline_clips {
            if let Ok((start, end)) = clip_interval(&self.project, clip) {
                points.push(start);
                points.push(end);
            }
        }
        points.sort_unstable();
        points.dedup();
        let current = self.playhead_frame;
        let frame = if forward {
            points.into_iter().find(|point| *point > current)
        } else {
            points.into_iter().rev().find(|point| *point < current)
        };
        if let Some(frame) = frame {
            self.seek(frame);
        }
    }

    pub fn toggle_track_muted(&mut self, track: TrackRef) -> Result<(), AppError> {
        let Some(state) = self.project.tracks(track.kind).get(track.index) else {
            return Err(AppError::new("track がありません"));
        };
        let muted = !state.muted;
        self.transact(move |project| set_track_muted(project, track, muted))
    }

    pub fn toggle_track_solo(&mut self, track: TrackRef) -> Result<(), AppError> {
        if track.kind != TrackKind::Audio {
            return Err(AppError::new("solo は audio track だけが持ちます"));
        }
        let Some(state) = self.project.tracks(track.kind).get(track.index) else {
            return Err(AppError::new("track がありません"));
        };
        let solo = !state.solo;
        self.transact(move |project| set_track_solo(project, track, solo))
    }

    pub fn toggle_subtitle_visible(&mut self) -> Result<(), AppError> {
        let Some(visible) = self.project.subtitles.as_ref().map(|track| !track.visible) else {
            return Err(AppError::new("字幕 track がありません"));
        };
        self.transact(move |project| set_subtitle_visible(project, visible))
    }

    pub fn place_text(&mut self, content: &str) -> Result<(), AppError> {
        let content = content.to_owned();
        let frame = self.playhead_frame;
        self.edit_with_ids(move |project, new_id| {
            place_text_clip(project, TrackRef::video(0), frame, &content, new_id)
        })
    }

    pub fn select_track_from_playhead(
        &mut self,
        frame: i64,
        forward: bool,
        track: Option<TrackRef>,
    ) {
        let direction = if forward {
            SelectDirection::Forward
        } else {
            SelectDirection::Backward
        };
        let ids = clip_ids_from_frame(&self.project, frame, direction, track);
        self.select_ids(ids);
    }

    pub fn set_alert(&mut self, message: impl Into<String>) {
        self.alert = message.into();
    }

    pub(crate) fn transact(
        &mut self,
        edit: impl FnOnce(&mut Project) -> ProjectResult<Option<usize>>,
    ) -> Result<(), AppError> {
        let before = self.project.clone();
        let selected = match edit(&mut self.project) {
            Ok(selected) => selected,
            Err(error) => {
                // 複数の編集を続けて行う command は、途中で失敗しても前半を残さない。
                self.project = before;
                return Err(AppError::new(error.to_string()));
            }
        };
        if self.project == before {
            return Ok(());
        }
        self.history.push_undo(before);
        if let Some(index) = selected
            && let Some(clip) = self.project.timeline_clips.get(index)
        {
            self.subtitle_selection.clear();
            self.selection = vec![clip.id.clone()];
            self.selected_edge = None;
            self.selected_marker = None;
        }
        self.finish_edit();
        Ok(())
    }

    pub(crate) fn edit_with_ids(
        &mut self,
        edit: impl FnOnce(&mut Project, &mut dyn FnMut() -> String) -> ProjectResult<Option<usize>>,
    ) -> Result<(), AppError> {
        let mut factory = self.id_factory();
        let result = self.transact(|project| {
            let mut next = || factory.next();
            edit(project, &mut next)
        });
        self.id_serial = factory.serial;
        result
    }

    pub(crate) fn id_factory(&self) -> IdFactory {
        let mut used = HashSet::new();
        for clip in &self.project.timeline_clips {
            used.insert(clip.id.clone());
        }
        for item in &self.project.media_items {
            used.insert(item.id.clone());
        }
        for transition in &self.project.timeline_transitions {
            used.insert(transition.id.clone());
        }
        for folder in &self.project.media_folders {
            used.insert(folder.id.clone());
        }
        if let Some(track) = &self.project.subtitles {
            for cue in &track.cues {
                used.insert(cue.id.clone());
            }
        }
        IdFactory {
            serial: self.id_serial,
            used,
        }
    }

    fn ordered_selection(&self) -> Result<Vec<TimelineClip>, AppError> {
        let clips = self
            .project
            .timeline_clips
            .iter()
            .filter(|clip| self.selection.iter().any(|id| id == &clip.id))
            .cloned()
            .collect::<Vec<_>>();
        if clips.is_empty() {
            Err(AppError::new("選択している clip がありません"))
        } else {
            Ok(clips)
        }
    }

    fn clips_with_anchor(&self, anchor_id: &str) -> Vec<TimelineClip> {
        let use_selection = self.selection.iter().any(|id| id == anchor_id);
        self.project
            .timeline_clips
            .iter()
            .filter(|clip| {
                if use_selection {
                    self.selection.iter().any(|id| id == &clip.id)
                } else {
                    clip.id == anchor_id
                }
            })
            .cloned()
            .collect()
    }

    fn media_for(&self, clips: &[TimelineClip]) -> Vec<MediaItem> {
        let mut items = Vec::new();
        for clip in clips {
            if clip.media_item_id.is_empty()
                || items
                    .iter()
                    .any(|item: &MediaItem| item.id == clip.media_item_id)
            {
                continue;
            }
            if let Some(item) = self
                .project
                .media_items
                .iter()
                .find(|item| item.id == clip.media_item_id)
            {
                items.push(item.clone());
            }
        }
        items
    }

    fn ensure_media_files(
        &self,
        items: &[MediaItem],
        clips: &[TimelineClip],
    ) -> Result<(), AppError> {
        for item in items {
            if !item.media_path.as_os_str().is_empty() && !item.media_path.is_file() {
                return Err(AppError::new(format!(
                    "素材が見つかりません ({})",
                    item.media_path.display()
                )));
            }
        }
        for clip in clips {
            if !clip.media_path.as_os_str().is_empty() && !clip.media_path.is_file() {
                return Err(AppError::new(format!(
                    "素材が見つかりません ({})",
                    clip.media_path.display()
                )));
            }
        }
        Ok(())
    }
}

fn link_mode(linked: bool) -> LinkMode {
    if linked {
        LinkMode::Linked
    } else {
        LinkMode::Single
    }
}

#[cfg(test)]
mod tests {
    use mvm_core::Rational;
    use mvm_project::clip_effects::ClipKeyframe;
    use mvm_project::{Project, TimelineClip, TimelineClipKind, TrackRef};

    use super::*;

    fn animated() -> Session {
        let mut clip = TimelineClip {
            kind: TimelineClipKind::Video,
            media_path: "v.mp4".into(),
            name: "v".to_owned(),
            id: "v".to_owned(),
            source_fps: Rational::new(60, 1),
            source_frame_count: 120,
            source_out_frame: 120,
            track: TrackRef::video(0),
            ..TimelineClip::default()
        };
        clip.effects.position_x_keys =
            vec![ClipKeyframe::new(0, 0.0), ClipKeyframe::new(100, 50.0)];
        Session::from_project(Project {
            timeline_clips: vec![clip],
            ..Project::default()
        })
    }

    #[test]
    fn an_effect_edit_lands_on_the_frame_where_it_started() {
        let mut session = animated();
        session.seek(30);
        let revision = session.edit_revision();
        // 編集の途中で再生ヘッドが動いても、始めた frame のキーへ書く。
        session.seek(80);
        session
            .commit_effects("v", 30, revision, &[(ClipKeyKind::PositionX, 10.0)])
            .expect("確定");
        let keys = &session.project().timeline_clips[0].effects.position_x_keys;
        let frames: Vec<_> = keys.iter().map(|key| (key.frame, key.value)).collect();
        assert_eq!(frames, [(0, 0.0), (30, 10.0), (100, 50.0)]);
    }

    #[test]
    fn an_effect_edit_started_before_an_undo_is_rejected() {
        let mut session = animated();
        session
            .set_effect("v", ClipKeyKind::PositionY, 5.0)
            .expect("編集");
        let revision = session.edit_revision();
        session.undo().expect("戻す");
        let before = session.project().clone();
        assert!(
            session
                .commit_effects("v", 0, revision, &[(ClipKeyKind::PositionX, 10.0)])
                .is_err()
        );
        assert_eq!(session.project(), &before);
    }
}
