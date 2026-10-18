//! playhead で描く layer。decode と GPU はここから先。

use mvm_project::clip_effects::{ClipEffects, evaluate_clip_keys};
use mvm_project::render::{
    TimelineRenderSegment, render_segment_opacity, timeline_render_segments,
};
use mvm_project::timeline::{clip_duration_at, clip_interval, clip_source_frame_at};
use mvm_project::{Project, TimelineClipKind, TrackKind, is_valid_project_output_size};

use crate::error::PreviewError;

/// この frame で decode する 1 layer。track の下から順。
#[derive(Clone, Debug)]
pub struct PlannedLayer {
    pub media_path: std::path::PathBuf,
    pub source: PlannedSource,
    pub opacity: f32,
    pub effects: ClipEffects,
    pub track_index: usize,
    pub lane: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlannedSource {
    Video { frame: i64 },
    Image,
}

#[derive(Clone, Debug)]
pub struct FramePlan {
    pub layers: Vec<PlannedLayer>,
    /// この frame にあるが、文字・数式・グラフ・Manim なので描かない clip の数。
    pub skipped_clips: usize,
}

/// Project の出力寸法。偶数かつ 2..=16384 でなければ失敗する。
pub fn output_size(project: &Project) -> Result<(u32, u32), PreviewError> {
    if !is_valid_project_output_size(project.output_width, project.output_height) {
        return Err(PreviewError::new("Project の出力寸法が不正です"));
    }
    Ok((project.output_width as u32, project.output_height as u32))
}

/// 停止中の preview の 1 枚。`rgba` は premultiplied、行は隙間なく詰める。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PreviewImage {
    pub frame: i64,
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// 不透明な黒。layer が無い frame と、合成の下地に使う。
pub fn black_image(frame: i64, project: &Project) -> Result<PreviewImage, PreviewError> {
    let (width, height) = output_size(project)?;
    let pixels = usize::try_from(width)
        .ok()
        .and_then(|width| {
            usize::try_from(height)
                .ok()
                .and_then(|height| width.checked_mul(height))
        })
        .ok_or_else(|| PreviewError::new("出力の画素数が overflow しました"))?;
    let bytes = pixels
        .checked_mul(4)
        .ok_or_else(|| PreviewError::new("出力の画素数が overflow しました"))?;
    let mut rgba = vec![0u8; bytes];
    for pixel in rgba.as_chunks_mut::<4>().0 {
        pixel[3] = 255;
    }
    Ok(PreviewImage {
        frame,
        width,
        height,
        rgba,
    })
}

/// `frame` に重なる映像・静止画。mute と無効な clip は含めない。
pub fn plan_frame(project: &Project, frame: i64) -> Result<FramePlan, PreviewError> {
    if frame < 0 {
        return Err(PreviewError::new("frame 番号が不正です"));
    }
    let segments = timeline_render_segments(project, TrackKind::Video)
        .map_err(|error| PreviewError::new(error.to_string()))?;
    let mut layers = Vec::new();
    let mut skipped_clips = 0usize;
    for segment in &segments {
        let (start, end) = clip_interval(project, &segment.clip)
            .map_err(|error| PreviewError::new(error.to_string()))?;
        if frame < start || frame >= end {
            continue;
        }
        if !project.is_track_output_enabled(segment.clip.track) {
            continue;
        }
        let source = match segment.clip.kind {
            TimelineClipKind::Video => {
                let local = frame
                    .checked_sub(start)
                    .ok_or_else(|| PreviewError::new("frame 番号が overflow しました"))?;
                let source = clip_source_frame_at(&segment.clip, project.timeline_fps, local)
                    .map_err(|error| PreviewError::new(error.to_string()))?;
                PlannedSource::Video { frame: source }
            }
            TimelineClipKind::Image => PlannedSource::Image,
            TimelineClipKind::Text
            | TimelineClipKind::Math
            | TimelineClipKind::EquationSequence
            | TimelineClipKind::Graph
            | TimelineClipKind::Manim
            | TimelineClipKind::Audio => {
                skipped_clips += 1;
                continue;
            }
        };
        let Some(opacity) = opacity_at(project, segment, frame)? else {
            continue;
        };
        layers.push(PlannedLayer {
            media_path: segment.clip.media_path.clone(),
            source,
            opacity,
            effects: effects_at(
                &segment.original.effects,
                effect_local(project, segment, frame)?,
            ),
            track_index: segment.clip.track.index,
            lane: segment.lane,
        });
    }
    layers.sort_by(|left, right| {
        left.track_index
            .cmp(&right.track_index)
            .then(left.lane.cmp(&right.lane))
    });
    Ok(FramePlan {
        layers,
        skipped_clips,
    })
}

fn opacity_at(
    project: &Project,
    segment: &TimelineRenderSegment,
    frame: i64,
) -> Result<Option<f32>, PreviewError> {
    let opacity =
        render_segment_opacity(segment, project.timeline_fps, frame).ok_or_else(|| {
            PreviewError::new(format!("{} の不透明度を計算できません", segment.clip.name))
        })?;
    if !opacity.is_finite() || opacity < 0.0 {
        return Err(PreviewError::new(format!(
            "{} の不透明度が不正です",
            segment.clip.name
        )));
    }
    if opacity == 0.0 {
        return Ok(None);
    }
    Ok(Some(opacity.min(1.0) as f32))
}

fn effect_local(
    project: &Project,
    segment: &TimelineRenderSegment,
    frame: i64,
) -> Result<i64, PreviewError> {
    let duration = clip_duration_at(&segment.original, project.timeline_fps)
        .map_err(|error| PreviewError::new(error.to_string()))?;
    if duration <= 0 {
        return Err(PreviewError::new("clip の尺が不正です"));
    }
    Ok((frame - segment.original.timeline_start_frame).clamp(0, duration - 1))
}

fn effects_at(effects: &ClipEffects, local: i64) -> ClipEffects {
    let mut at = effects.clone();
    at.position_x_percent =
        evaluate_clip_keys(&effects.position_x_keys, effects.position_x_percent, local);
    at.position_y_percent =
        evaluate_clip_keys(&effects.position_y_keys, effects.position_y_percent, local);
    at.scale_x_percent = evaluate_clip_keys(&effects.scale_x_keys, effects.scale_x_percent, local);
    at.scale_y_percent = evaluate_clip_keys(&effects.scale_y_keys, effects.scale_y_percent, local);
    at.rotation_degrees =
        evaluate_clip_keys(&effects.rotation_keys, effects.rotation_degrees, local);
    at.crop_left_percent =
        evaluate_clip_keys(&effects.crop_left_keys, effects.crop_left_percent, local);
    at.crop_top_percent =
        evaluate_clip_keys(&effects.crop_top_keys, effects.crop_top_percent, local);
    at.crop_right_percent =
        evaluate_clip_keys(&effects.crop_right_keys, effects.crop_right_percent, local);
    at.crop_bottom_percent = evaluate_clip_keys(
        &effects.crop_bottom_keys,
        effects.crop_bottom_percent,
        local,
    );
    at
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use std::path::PathBuf;

    use mvm_core::Rational;
    use mvm_project::{Project, TimelineClip, TimelineClipKind, TrackRef};

    use super::*;

    fn video(path: &str, track: usize, start: i64) -> TimelineClip {
        TimelineClip {
            kind: TimelineClipKind::Video,
            media_path: PathBuf::from(path),
            name: path.to_owned(),
            id: format!("id-{path}"),
            source_fps: Rational::new(60, 1),
            source_frame_count: 300,
            source_in_frame: 0,
            source_out_frame: 300,
            timeline_start_frame: start,
            track: TrackRef::video(track),
            ..TimelineClip::default()
        }
    }

    #[test]
    fn plans_the_visible_video_frame_and_leaves_the_others_out() {
        let mut project = Project::default();
        let mut hidden = video("off.mp4", 0, 0);
        hidden.enabled = false;
        let mut silent = video("silent.mp4", 0, 0);
        silent.effects.opacity_percent = 0.0;
        let mut later = video("later.mp4", 0, 100);
        later.id = "later".to_owned();
        project.timeline_clips = vec![
            video("bottom.mp4", 0, 0),
            video("top.mp4", 1, 0),
            hidden,
            silent,
            later,
            TimelineClip {
                kind: TimelineClipKind::Text,
                name: "字幕ではない文字".to_owned(),
                id: "text".to_owned(),
                source_fps: Rational::new(60, 1),
                source_frame_count: 30,
                source_in_frame: 0,
                source_out_frame: 30,
                timeline_start_frame: 0,
                track: TrackRef::video(0),
                ..TimelineClip::default()
            },
            TimelineClip {
                kind: TimelineClipKind::Image,
                media_path: PathBuf::from("still.png"),
                name: "still".to_owned(),
                id: "still".to_owned(),
                source_fps: Rational::new(60, 1),
                source_frame_count: 30,
                source_in_frame: 0,
                source_out_frame: 30,
                timeline_start_frame: 0,
                track: TrackRef::video(0),
                ..TimelineClip::default()
            },
        ];

        let plan = plan_frame(&project, 5).expect("plan");
        let described: Vec<_> = plan
            .layers
            .iter()
            .map(|layer| {
                (
                    layer.media_path.display().to_string(),
                    match layer.source {
                        PlannedSource::Video { frame } => frame,
                        PlannedSource::Image => -1,
                    },
                )
            })
            .collect();
        assert_eq!(
            described,
            [
                ("bottom.mp4".to_owned(), 5),
                ("still.png".to_owned(), -1),
                ("top.mp4".to_owned(), 5),
            ]
        );
        assert_eq!(plan.skipped_clips, 1, "文字 clip は描かない");
        assert!(
            described
                .iter()
                .all(|(path, _)| path != "off.mp4" && path != "silent.mp4" && path != "later.mp4"),
            "無効・透明・範囲外の clip が残っています: {described:?}"
        );

        project.video_tracks[0].muted = true;
        project.video_tracks[1].muted = true;
        let muted = plan_frame(&project, 5).expect("mute");
        assert!(
            muted.layers.is_empty(),
            "mute した track を描いています: {:?}",
            muted.layers.len()
        );
    }

    #[test]
    fn black_image_is_opaque() {
        let image = black_image(3, &Project::default()).expect("黒");
        assert_eq!((image.width, image.height, image.frame), (1920, 1080, 3));
        assert_eq!(image.rgba.len(), 1920 * 1080 * 4);
        assert!(
            image
                .rgba
                .as_chunks::<4>()
                .0
                .iter()
                .all(|pixel| *pixel == [0, 0, 0, 255])
        );
    }
}
