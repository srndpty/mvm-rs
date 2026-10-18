//! 再生する音声区間。sample の対応は output timebase の整数だけで決める。

use std::path::PathBuf;

use mvm_core::output_timebase::{OutputTimebase, QUALIFIED_AUDIO_SAMPLE_RATE};
use mvm_project::render::TimelineRenderSegment;
use mvm_project::timeline::clip_interval;
use mvm_project::{Project, TrackKind};

use crate::error::PreviewError;

#[derive(Clone, Debug)]
pub struct AudioSpan {
    pub path: PathBuf,
    pub segment: TimelineRenderSegment,
    pub timeline_start_sample: i64,
    pub timeline_end_sample: i64,
    pub source_start_sample: i64,
    pub speed_num: i64,
    pub speed_den: i64,
}

pub fn output_timebase(project: &Project) -> Result<OutputTimebase, PreviewError> {
    OutputTimebase::configured(project.timeline_fps, QUALIFIED_AUDIO_SAMPLE_RATE)
        .map_err(|error| PreviewError::new(error.to_string()))
}

/// mute / solo を外した音声 clip。ピッチ保持はまだ鳴らせないので、あれば失敗する。
pub fn audio_spans(project: &Project) -> Result<Vec<AudioSpan>, PreviewError> {
    let timebase = output_timebase(project)?;
    let segments = mvm_project::render::timeline_render_segments(project, TrackKind::Audio)
        .map_err(|error| PreviewError::new(error.to_string()))?;
    let mut spans = Vec::new();
    for segment in segments {
        if !project.is_track_output_enabled(segment.clip.track) {
            continue;
        }
        if segment.original.preserve_pitch {
            return Err(PreviewError::new("ピッチ保持の再生はまだできません"));
        }
        if segment.original.speed.den <= 0 || segment.original.speed.num <= 0 {
            return Err(PreviewError::new("clip の速度が不正です"));
        }
        let (start_frame, end_frame) = clip_interval(project, &segment.clip)
            .map_err(|error| PreviewError::new(error.to_string()))?;
        let source = OutputTimebase::new(segment.clip.source_fps, QUALIFIED_AUDIO_SAMPLE_RATE)
            .map_err(|error| PreviewError::new(error.to_string()))?;
        spans.push(AudioSpan {
            path: segment.clip.media_path.clone(),
            timeline_start_sample: timebase
                .first_audio_sample(start_frame)
                .map_err(|error| PreviewError::new(error.to_string()))?,
            timeline_end_sample: timebase
                .first_audio_sample(end_frame)
                .map_err(|error| PreviewError::new(error.to_string()))?,
            source_start_sample: source
                .first_audio_sample(segment.clip.source_in_frame)
                .map_err(|error| PreviewError::new(error.to_string()))?,
            speed_num: segment.original.speed.num,
            speed_den: segment.original.speed.den,
            segment,
        });
    }
    Ok(spans)
}

pub fn source_sample(span: &AudioSpan, timeline_sample: i64) -> Result<Option<i64>, PreviewError> {
    if timeline_sample < span.timeline_start_sample || timeline_sample >= span.timeline_end_sample {
        return Ok(None);
    }
    if span.speed_den <= 0 {
        return Err(PreviewError::new("clip の速度が不正です"));
    }
    let elapsed = timeline_sample - span.timeline_start_sample;
    let advanced = i128::from(elapsed) * i128::from(span.speed_num) / i128::from(span.speed_den);
    let source = i128::from(span.source_start_sample) + advanced;
    i64::try_from(source)
        .map(Some)
        .map_err(|_| PreviewError::new("素材 sample が overflow しました"))
}

/// 有効・無効を問わず、clip と字幕が占める終端。再生を止める frame（含まない）。
pub fn playback_end(project: &Project) -> Result<i64, PreviewError> {
    let mut end = 0i64;
    for clip in &project.timeline_clips {
        let (_, clip_end) =
            clip_interval(project, clip).map_err(|error| PreviewError::new(error.to_string()))?;
        end = end.max(clip_end);
    }
    if let Some(track) = &project.subtitles {
        for cue in &track.cues {
            end = end.max(cue.end_frame);
        }
    }
    Ok(end)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use std::path::PathBuf;

    use mvm_core::Rational;
    use mvm_project::{Project, TimelineClip, TimelineClipKind, Track, TrackKind, TrackRef};

    use super::*;

    fn audio(name: &str, track: usize, start: i64) -> TimelineClip {
        TimelineClip {
            kind: TimelineClipKind::Audio,
            media_path: format!("{name}.wav").into(),
            name: name.to_owned(),
            id: format!("id-{name}"),
            source_fps: Rational::new(60, 1),
            source_frame_count: 300,
            source_in_frame: 0,
            source_out_frame: 300,
            timeline_start_frame: start,
            track: TrackRef::audio(track),
            ..TimelineClip::default()
        }
    }

    #[test]
    fn maps_a_60fps_clip_onto_800_sample_frames_and_drops_muted_tracks() {
        let mut project = Project::default();
        project
            .audio_tracks
            .push(Track::named(TrackKind::Audio.default_track_name(1)));
        project.audio_tracks[1].solo = true;
        let mut pitched = audio("pitched", 1, 0);
        pitched.preserve_pitch = true;
        pitched.id = "pitched".to_owned();
        project.timeline_clips = vec![audio("quiet", 0, 0), audio("solo", 1, 0)];
        let spans = audio_spans(&project).expect("区間");
        assert_eq!(spans.len(), 1, "solo 以外が残っています");
        assert_eq!(spans[0].path, PathBuf::from("solo.wav"));
        assert_eq!(spans[0].timeline_start_sample, 0);
        assert_eq!(spans[0].timeline_end_sample, 300 * 800);
        assert_eq!(spans[0].source_start_sample, 0);
        assert_eq!(source_sample(&spans[0], 800).unwrap(), Some(800));
        assert_eq!(source_sample(&spans[0], 300 * 800).unwrap(), None);

        let half = AudioSpan {
            speed_num: 1,
            speed_den: 2,
            source_start_sample: 100,
            timeline_start_sample: 0,
            timeline_end_sample: 10_000,
            ..spans[0].clone()
        };
        assert_eq!(source_sample(&half, 800).unwrap(), Some(100 + 400));

        project.timeline_clips.push(pitched);
        let error = audio_spans(&project).unwrap_err();
        assert!(error.to_string().contains("ピッチ保持"), "{error}");
    }

    #[test]
    fn empty_project_has_no_playback_range() {
        assert_eq!(playback_end(&Project::default()).unwrap(), 0);
    }
}
