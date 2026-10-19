//! track・フレーム保持・リンク・リップル削除・速度と尺・Project の映像設定の command。判定は project が行う。

use mvm_core::Rational;
use mvm_media::{ProbedMedia, probe_media_file};
use mvm_project::clip_effects::{
    ClipKeyKind, ClipVisualGeometry, clip_visual_geometry, set_effects_for_visual_rect,
};
use mvm_project::timeline::{
    ClipSpeedDurationEdit, ClipSpeedDurationPreview, LinkMode, SpeedDurationError, add_track,
    insert_frame_hold, preview_clip_speed_duration, remove_track, ripple_delete_clips,
    ripple_delete_gap, set_clip_speed_duration, set_project_video_settings, unlink_timeline_clip,
    video_settings_for_media,
};
use mvm_project::{
    Project, TimelineClip, TimelineClipKind, TrackKind, TrackRef, default_frame_hold_frames,
};

use crate::error::AppError;
use crate::session::Session;

/// Project の映像設定（出力の幅・高さと timeline の fps）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VideoSettings {
    pub width: i32,
    pub height: i32,
    pub fps: Rational,
}

impl Session {
    pub fn add_track(&mut self, kind: TrackKind) -> Result<(), AppError> {
        self.transact(|project| add_track(project, kind).map(|_| None))
    }

    /// clip の載っている track は消さない（project が断る）。
    pub fn remove_track(&mut self, track: TrackRef) -> Result<(), AppError> {
        self.transact(|project| remove_track(project, track).map(|_| None))
    }

    /// 再生ヘッドの位置でフレーム保持を挿入できるか。判定を別に書かず、複製した Project で実際に試す
    /// （押せるのに実行すると失敗するメニューにしない）。
    pub fn can_insert_frame_hold(&self, clip_id: &str) -> bool {
        let mut candidate = self.project.clone();
        let mut factory = self.id_factory();
        let frames = default_frame_hold_frames(candidate.timeline_fps);
        insert_frame_hold(
            &mut candidate,
            clip_id,
            self.playhead_frame,
            frames,
            &mut || factory.next(),
        )
        .is_ok()
    }

    /// 再生ヘッドの位置の frame を 2 秒保持する。
    pub fn insert_frame_hold(&mut self, clip_id: &str) -> Result<(), AppError> {
        let frame = self.playhead_frame;
        let frames = default_frame_hold_frames(self.project.timeline_fps);
        self.edit_with_ids(|project, new_id| {
            insert_frame_hold(project, clip_id, frame, frames, new_id)
        })
    }

    pub fn unlink_clip(&mut self, clip_id: &str) -> Result<(), AppError> {
        self.transact(|project| unlink_timeline_clip(project, clip_id))
    }

    /// track 上の frame を含む空白を詰める（全 track と字幕に同じ時間削除を適用する）。
    pub fn ripple_delete_gap(&mut self, track: TrackRef, frame: i64) -> Result<(), AppError> {
        self.transact(|project| ripple_delete_gap(project, track, frame))
    }

    /// 選んだ clip の区間を全 track から取り除いて詰める。
    pub fn ripple_delete_selection(&mut self) -> Result<(), AppError> {
        let ids = self.selection.clone();
        self.transact(|project| ripple_delete_clips(project, &ids))?;
        self.selection.clear();
        Ok(())
    }

    /// 速度・尺の dialog の見込み。`overlaps_following` なら上書きの確認を出す。
    pub fn preview_speed_duration(
        &self,
        clip_id: &str,
        edit: &ClipSpeedDurationEdit,
    ) -> Result<ClipSpeedDurationPreview, SpeedDurationError> {
        preview_clip_speed_duration(&self.project, clip_id, edit, LinkMode::Linked)
    }

    pub fn set_speed_duration(
        &mut self,
        clip_id: &str,
        edit: &ClipSpeedDurationEdit,
    ) -> Result<(), AppError> {
        self.transact(|project| set_clip_speed_duration(project, clip_id, edit, LinkMode::Linked))
    }

    pub fn video_settings(&self) -> VideoSettings {
        VideoSettings {
            width: self.project.output_width,
            height: self.project.output_height,
            fps: self.project.timeline_fps,
        }
    }

    /// 解像度と fps を変える。既存 clip の開始位置などは秒の位置を保つよう換算する。
    pub fn set_video_settings(&mut self, settings: VideoSettings) -> Result<(), AppError> {
        self.transact(|project| {
            set_project_video_settings(project, settings.width, settings.height, settings.fps)
        })
    }

    /// 映像 clip の素材に合わせた Project の設定。素材は調べ直す。
    pub fn video_settings_for_clip(&self, clip_id: &str) -> Result<VideoSettings, AppError> {
        let Some(clip) = self.project.clip(clip_id) else {
            return Err(AppError::new("対象の clip がありません"));
        };
        if !matches!(clip.kind, TimelineClipKind::Video | TimelineClipKind::Manim) {
            return Err(AppError::new("動画の clip ではありません"));
        }
        let ProbedMedia::Video(video) =
            probe_media_file(&clip.media_path).map_err(|error| AppError::new(error.to_string()))?
        else {
            return Err(AppError::new("素材が動画ではありません"));
        };
        let (width, height, fps) = video_settings_for_media(
            video.width,
            video.height,
            Rational::new(i64::from(video.sar_num), i64::from(video.sar_den)),
            Rational::new(i64::from(video.fps_num), i64::from(video.fps_den)),
        )
        .map_err(|error| AppError::new(error.to_string()))?;
        Ok(VideoSettings { width, height, fps })
    }
}

/// preview で枠を動かせる clip の素材の寸法（動画・画像の素材だけ。文字・数式は raster の大きさが要るので扱わない）。
fn source_size(project: &Project, clip: &TimelineClip) -> Option<(i32, i32)> {
    if !matches!(clip.kind, TimelineClipKind::Video | TimelineClipKind::Image) {
        return None;
    }
    let item = project
        .media_items
        .iter()
        .find(|item| item.id == clip.media_item_id)?;
    (item.width > 0 && item.height > 0).then_some((item.width, item.height))
}

impl Session {
    /// 再生ヘッドの位置で clip が見えている矩形（出力画素）。preview の枠に使う。動かせない clip は `None`。
    pub fn clip_visual_rect(&self, clip_id: &str) -> Option<ClipVisualGeometry> {
        let clip = self.project.clip(clip_id)?;
        let (width, height) = source_size(&self.project, clip)?;
        let frame = self.effect_frame(clip_id).ok()?;
        clip_visual_geometry(
            &clip.effects.evaluated_at(frame),
            width,
            height,
            self.project.output_width,
            self.project.output_height,
        )
    }

    /// 見えている矩形を (x, y, width, height)（出力画素）にする位置と拡大率を、1 つの undo で設定する。
    /// キーのある channel は再生ヘッドの位置のキーを置き換える。
    pub fn place_clip_visual_rect(
        &mut self,
        clip_id: &str,
        rect: [f64; 4],
    ) -> Result<(), AppError> {
        let Some(clip) = self.project.clip(clip_id) else {
            return Err(AppError::new("動かす clip がありません"));
        };
        let Some((width, height)) = source_size(&self.project, clip) else {
            return Err(AppError::new("この clip は preview で動かせません"));
        };
        let frame = self.effect_frame(clip_id)?;
        let mut effects = clip.effects.evaluated_at(frame);
        let [x, y, w, h] = rect;
        set_effects_for_visual_rect(
            &mut effects,
            width,
            height,
            self.project.output_width,
            self.project.output_height,
            x,
            y,
            w,
            h,
        )
        .map_err(|error| AppError::new(error.to_string()))?;
        self.set_effects(
            clip_id,
            &[
                (ClipKeyKind::PositionX, effects.position_x_percent),
                (ClipKeyKind::PositionY, effects.position_y_percent),
                (ClipKeyKind::ScaleX, effects.scale_x_percent),
                (ClipKeyKind::ScaleY, effects.scale_y_percent),
            ],
        )
    }
}

#[cfg(test)]
mod tests {
    use mvm_project::{Project, TimelineClip};

    use super::*;

    fn clip(id: &str, track: TrackRef, start: i64) -> TimelineClip {
        TimelineClip {
            kind: if track.kind == TrackKind::Audio {
                TimelineClipKind::Audio
            } else {
                TimelineClipKind::Video
            },
            media_path: format!("{id}.mp4").into(),
            name: id.to_owned(),
            id: id.to_owned(),
            source_fps: Rational::new(60, 1),
            source_frame_count: 120,
            source_out_frame: 120,
            timeline_start_frame: start,
            track,
            ..TimelineClip::default()
        }
    }

    fn session(clips: Vec<TimelineClip>) -> Session {
        Session::from_project(Project {
            timeline_clips: clips,
            ..Project::default()
        })
    }

    #[test]
    fn tracks_are_added_and_only_empty_ones_removed() {
        let mut session = session(vec![clip("a", TrackRef::audio(0), 0)]);
        session.add_track(TrackKind::Audio).expect("追加");
        assert_eq!(session.project().audio_tracks.len(), 2);
        assert_eq!(session.project().audio_tracks[1].name, "A2");
        assert!(session.remove_track(TrackRef::audio(0)).is_err());
        session
            .remove_track(TrackRef::audio(1))
            .expect("空の track");
        assert_eq!(session.project().audio_tracks.len(), 1);
        session.undo().expect("戻す");
        assert_eq!(session.project().audio_tracks.len(), 2);
    }

    #[test]
    fn a_frame_hold_is_offered_only_inside_the_clip() {
        let mut session = session(vec![clip("a", TrackRef::video(0), 0)]);
        session.seek(30);
        assert!(session.can_insert_frame_hold("a"));
        assert!(!session.can_insert_frame_hold("無い"));
        session.insert_frame_hold("a").expect("挿入");
        let holds = session
            .project()
            .timeline_clips
            .iter()
            .filter(|clip| clip.frame_hold.is_some())
            .count();
        assert_eq!(holds, 1);
        session.seek(10_000);
        assert!(!session.can_insert_frame_hold("a"));
    }

    #[test]
    fn ripple_delete_of_the_selection_pulls_later_clips_in() {
        let mut session = session(vec![
            clip("a", TrackRef::video(0), 0),
            clip("b", TrackRef::video(0), 120),
        ]);
        session.select_only("a".to_owned());
        session.ripple_delete_selection().expect("リップル削除");
        assert_eq!(session.project().timeline_clips.len(), 1);
        assert_eq!(
            session.project().clip("b").expect("b").timeline_start_frame,
            0
        );
        assert!(session.selection().is_empty());
        assert!(session.ripple_delete_selection().is_err());
    }

    #[test]
    fn a_gap_is_closed_and_unlinking_keeps_both_clips() {
        let mut first = clip("v", TrackRef::video(0), 60);
        let mut second = clip("s", TrackRef::audio(0), 60);
        first.link_group_id = "g".to_owned();
        second.link_group_id = "g".to_owned();
        let mut session = session(vec![first, second]);
        session.unlink_clip("v").expect("リンク解除");
        assert!(
            session
                .project()
                .timeline_clips
                .iter()
                .all(|clip| !clip.is_linked())
        );
        session
            .ripple_delete_gap(TrackRef::video(0), 10)
            .expect("空白を詰める");
        assert!(
            session
                .project()
                .timeline_clips
                .iter()
                .all(|clip| clip.timeline_start_frame == 0)
        );
    }

    #[test]
    fn moving_the_visual_rect_sets_position_and_scale_in_one_undo() {
        let mut video = clip("v", TrackRef::video(0), 0);
        video.media_item_id = "item".to_owned();
        let mut session = Session::from_project(Project {
            timeline_clips: vec![video],
            media_items: vec![mvm_project::MediaItem {
                id: "item".to_owned(),
                kind: mvm_project::MediaKind::Video,
                media_path: "v.mp4".into(),
                name: "v.mp4".to_owned(),
                fps: Rational::new(60, 1),
                frame_count: 120,
                width: 1920,
                height: 1080,
                ..mvm_project::MediaItem::default()
            }],
            ..Project::default()
        });
        let full = session.clip_visual_rect("v").expect("枠");
        assert_eq!(
            (full.x, full.y, full.width, full.height),
            (0.0, 0.0, 1920.0, 1080.0)
        );
        // 右下 1/4 へ縮める: 拡大率 50%・中心は (1440, 810)。
        session
            .place_clip_visual_rect("v", [960.0, 540.0, 960.0, 540.0])
            .expect("移動");
        let effects = &session.project().timeline_clips[0].effects;
        assert!((effects.scale_x_percent - 50.0).abs() < 1e-6);
        assert!((effects.scale_y_percent - 50.0).abs() < 1e-6);
        assert!((effects.position_x_percent - 25.0).abs() < 1e-6);
        assert!((effects.position_y_percent - 25.0).abs() < 1e-6);
        let moved = session.clip_visual_rect("v").expect("枠");
        assert!((moved.x - 960.0).abs() < 1e-6 && (moved.width - 960.0).abs() < 1e-6);
        session.undo().expect("1 回で戻る");
        assert_eq!(
            session.project().timeline_clips[0].effects.scale_x_percent,
            100.0
        );
        // 素材の寸法を持たない clip は動かせない。
        assert!(session.clip_visual_rect("無い").is_none());
    }

    #[test]
    fn video_settings_change_in_one_step() {
        let mut session = Session::new();
        let settings = VideoSettings {
            width: 1280,
            height: 720,
            fps: Rational::new(30, 1),
        };
        session.set_video_settings(settings).expect("設定");
        assert_eq!(session.video_settings(), settings);
        assert!(
            session
                .set_video_settings(VideoSettings {
                    width: 1281,
                    ..settings
                })
                .is_err()
        );
        session.undo().expect("戻す");
        assert_eq!(session.video_settings().width, 1920);
    }
}
