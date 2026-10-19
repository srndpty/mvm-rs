//! 書き出しの計画。始める前に、書き出せない Project をここで全部落とす。GPU と file は触らない。
//!
//! 各 frame の layer と音声区間は preview と同じ関数（[`mvm_preview::plan_frame`]・
//! [`mvm_preview::audio_spans`]）で決める。ここで持つのは、書き出し全体の尺・出力形式と、
//! preview では「描かない」で済ませている clip を書き出しでは失敗にする検査だけ。

use std::path::{Path, PathBuf};

use mvm_core::Rational;
use mvm_core::output_timebase::OutputTimebase;
use mvm_media::Mp4Settings;
use mvm_preview::{AudioSpan, audio_spans, output_size, output_timebase};
use mvm_project::timeline::validate_timeline;
use mvm_project::{Project, TimelineClipKind};

use crate::error::ExportError;

/// 画質の 3 段階。CRF の値は C++ 版の書き出しと同じ。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ExportQuality {
    High,
    #[default]
    Standard,
    Compact,
}

impl ExportQuality {
    pub const ALL: [Self; 3] = [Self::High, Self::Standard, Self::Compact];

    /// libx264 の constant rate factor。
    pub const fn crf(self) -> u8 {
        match self {
            Self::High => 18,
            Self::Standard => 23,
            Self::Compact => 28,
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::High => "高品質",
            Self::Standard => "標準",
            Self::Compact => "容量優先",
        }
    }

    pub const fn detail(self) -> &'static str {
        match self {
            Self::High => "CRF 18・容量大",
            Self::Standard => "CRF 23",
            Self::Compact => "CRF 28・画質低下",
        }
    }
}

/// 利用者が書き出しの dialog で決める値。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExportRequest {
    pub output_path: PathBuf,
    pub quality: ExportQuality,
    /// 字幕を映像へ焼き込む。焼き込まなくても、字幕の終端までは書き出す。
    pub burn_subtitles: bool,
}

/// 検査を通った書き出し。[`crate::run_export`] と [`crate::ExportJob`] へ渡す。
#[derive(Clone, Debug)]
pub struct ExportPlan {
    pub output_path: PathBuf,
    /// encode 中に書く file。検証を通ってから `output_path` へ rename する。
    pub temporary_path: PathBuf,
    pub width: u32,
    pub height: u32,
    pub fps: Rational,
    pub crf: u8,
    pub burn_subtitles: bool,
    /// 書き出す frame 数（clip と字幕の終端）。
    pub total_frames: i64,
    /// 書き出す 48 kHz の sample 数。音声が無ければ 0。
    pub total_samples: i64,
    pub timebase: OutputTimebase,
    /// 鳴らす音声区間。空なら音声の stream を作らない。
    pub audio_spans: Vec<AudioSpan>,
}

impl ExportPlan {
    pub fn has_audio(&self) -> bool {
        !self.audio_spans.is_empty()
    }

    pub fn mp4_settings(&self) -> Mp4Settings {
        Mp4Settings {
            width: self.width,
            height: self.height,
            fps: self.fps,
            crf: self.crf,
            audio: self.has_audio(),
        }
    }
}

/// 書き出しを計画する。書き出せない clip が出力に含まれていれば、始める前に失敗させる。
pub fn plan_export(project: &Project, request: &ExportRequest) -> Result<ExportPlan, ExportError> {
    let output_path = check_output_path(&request.output_path)?;
    let validation =
        validate_timeline(project).map_err(|error| ExportError::failed(error.to_string()))?;
    let (width, height) =
        output_size(project).map_err(|error| ExportError::failed(error.to_string()))?;
    let fps = project
        .timeline_fps
        .reduced()
        .ok_or_else(|| ExportError::failed("timeline の fps が不正です"))?;
    reject_unsupported_clips(project)?;
    if !has_output(project) {
        return Err(ExportError::failed(
            "書き出す有効な clip または字幕がありません",
        ));
    }
    let total_frames = validation.total_frames;
    if total_frames <= 0 {
        return Err(ExportError::failed("書き出す尺がありません"));
    }
    let timebase =
        output_timebase(project).map_err(|error| ExportError::failed(error.to_string()))?;
    let spans = audio_spans(project).map_err(|error| ExportError::failed(error.to_string()))?;
    let total_samples = if spans.is_empty() {
        0
    } else {
        timebase
            .first_audio_sample(total_frames)
            .map_err(|error| ExportError::failed(error.to_string()))?
    };
    Ok(ExportPlan {
        temporary_path: temporary_path(&output_path),
        output_path,
        width,
        height,
        fps,
        crf: request.quality.crf(),
        burn_subtitles: request.burn_subtitles,
        total_frames,
        total_samples,
        timebase,
        audio_spans: spans,
    })
}

fn check_output_path(path: &Path) -> Result<PathBuf, ExportError> {
    if path.as_os_str().is_empty() {
        return Err(ExportError::failed("書き出し先が指定されていません"));
    }
    if path.file_name().is_none() {
        return Err(ExportError::failed(format!(
            "書き出し先のパスが不正です: {}",
            path.display()
        )));
    }
    let is_mp4 = path
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("mp4"));
    if !is_mp4 {
        return Err(ExportError::failed(format!(
            "書き出し先は .mp4 にしてください: {}",
            path.display()
        )));
    }
    std::path::absolute(path).map_err(|error| {
        ExportError::failed(format!(
            "書き出し先のパスを解決できません ({}): {error}",
            path.display()
        ))
    })
}

/// 出力の正規名の隣に置く、encode 中の file。
pub fn temporary_path(output: &Path) -> PathBuf {
    let mut name = output.as_os_str().to_owned();
    name.push(".mvmtmp");
    PathBuf::from(name)
}

/// 数式・グラフ・Manim はまだ合成できない。preview は描かずに済ませるが、書き出しで黙って
/// 抜くと見た目の違う動画ができるので失敗させる。出力しない clip（無効・非表示の track）は問わない。
fn reject_unsupported_clips(project: &Project) -> Result<(), ExportError> {
    for clip in &project.timeline_clips {
        if !clip.enabled || !project.is_track_output_enabled(clip.track) {
            continue;
        }
        if matches!(
            clip.kind,
            TimelineClipKind::Math
                | TimelineClipKind::EquationSequence
                | TimelineClipKind::Graph
                | TimelineClipKind::Manim
        ) {
            return Err(ExportError::failed(format!(
                "数式・グラフ・Manim の clip はまだ書き出せません: {}",
                clip.name
            )));
        }
    }
    Ok(())
}

/// 出力する clip か字幕が 1 つでもあるか。字幕は焼き込まなくても尺を決めるので数える。
fn has_output(project: &Project) -> bool {
    project
        .timeline_clips
        .iter()
        .any(|clip| clip.enabled && project.is_track_output_enabled(clip.track))
        || project
            .subtitles
            .as_ref()
            .is_some_and(|track| !track.cues.is_empty())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use mvm_project::{SubtitleCue, SubtitleStyle, SubtitleTrack, TimelineClip, TrackRef};

    use super::*;

    fn request(path: &str) -> ExportRequest {
        ExportRequest {
            output_path: PathBuf::from(path),
            quality: ExportQuality::Standard,
            burn_subtitles: true,
        }
    }

    fn clip(kind: TimelineClipKind, id: &str, track: TrackRef, frames: i64) -> TimelineClip {
        TimelineClip {
            kind,
            media_path: PathBuf::from(format!("{id}.media")),
            name: id.to_owned(),
            id: id.to_owned(),
            source_fps: Rational::new(60, 1),
            source_frame_count: frames,
            source_in_frame: 0,
            source_out_frame: frames,
            timeline_start_frame: 0,
            track,
            ..TimelineClip::default()
        }
    }

    fn video(id: &str, frames: i64) -> TimelineClip {
        clip(TimelineClipKind::Video, id, TrackRef::video(0), frames)
    }

    fn error_of(project: &Project, request: &ExportRequest) -> String {
        match plan_export(project, request) {
            Ok(plan) => panic!("計画できてしまいました: {} frame", plan.total_frames),
            Err(error) => error.to_string(),
        }
    }

    #[test]
    fn quality_maps_to_the_crf_of_the_cpp_export() {
        let crf: Vec<_> = ExportQuality::ALL
            .iter()
            .map(|quality| quality.crf())
            .collect();
        assert_eq!(crf, [18, 23, 28]);
        assert_eq!(ExportQuality::default(), ExportQuality::Standard);
    }

    #[test]
    fn plans_the_whole_timeline_with_audio_samples_on_the_output_timebase() {
        let mut project = Project {
            timeline_clips: vec![
                video("v", 120),
                clip(TimelineClipKind::Audio, "a", TrackRef::audio(0), 90),
            ],
            ..Project::default()
        };
        project.timeline_clips[1].media_path = PathBuf::from("a.wav");
        let plan = plan_export(&project, &request("out.mp4")).expect("計画");
        assert_eq!(plan.total_frames, 120);
        // 60 fps の 1 frame は 48 kHz の 800 sample。
        assert_eq!(plan.total_samples, 120 * 800);
        assert!(plan.has_audio());
        assert_eq!(plan.audio_spans.len(), 1);
        assert_eq!((plan.width, plan.height), (1920, 1080));
        assert_eq!(plan.fps, Rational::new(60, 1));
        assert_eq!(plan.crf, 23);
        assert!(plan.output_path.is_absolute());
        assert_eq!(
            plan.temporary_path.file_name().unwrap().to_string_lossy(),
            "out.mp4.mvmtmp"
        );

        project.audio_tracks[0].muted = true;
        let silent = plan_export(&project, &request("out.mp4")).expect("無音");
        assert!(!silent.has_audio(), "mute した track の音を書き出します");
        assert_eq!(silent.total_samples, 0);
        assert!(!silent.mp4_settings().audio);
    }

    #[test]
    fn rejects_a_path_that_is_not_an_mp4() {
        let project = Project {
            timeline_clips: vec![video("v", 10)],
            ..Project::default()
        };
        assert!(error_of(&project, &request("")).contains("指定されていません"));
        assert!(error_of(&project, &request("out.mov")).contains(".mp4"));
        assert!(error_of(&project, &request("out")).contains(".mp4"));
        assert!(plan_export(&project, &request("OUT.MP4")).is_ok());
    }

    #[test]
    fn rejects_a_timeline_with_nothing_to_output() {
        assert!(error_of(&Project::default(), &request("out.mp4")).contains("有効な clip"));

        let mut disabled = Project {
            timeline_clips: vec![video("v", 10)],
            ..Project::default()
        };
        disabled.timeline_clips[0].enabled = false;
        assert!(error_of(&disabled, &request("out.mp4")).contains("有効な clip"));

        let mut hidden = Project {
            timeline_clips: vec![video("v", 10)],
            ..Project::default()
        };
        hidden.video_tracks[0].muted = true;
        assert!(error_of(&hidden, &request("out.mp4")).contains("有効な clip"));
    }

    #[test]
    fn subtitles_alone_set_the_length_even_when_not_burned() {
        let project = Project {
            subtitles: Some(SubtitleTrack {
                visible: true,
                style: SubtitleStyle::default(),
                cues: vec![SubtitleCue {
                    id: "cue".to_owned(),
                    start_frame: 5,
                    end_frame: 20,
                    content: "字幕".to_owned(),
                    link_clip_id: String::new(),
                }],
            }),
            ..Project::default()
        };
        let mut unburned = request("out.mp4");
        unburned.burn_subtitles = false;
        let plan = plan_export(&project, &unburned).expect("字幕だけ");
        assert_eq!(plan.total_frames, 20);
        assert!(!plan.burn_subtitles);
        assert!(!plan.has_audio());
    }

    #[test]
    fn math_on_an_output_track_fails_but_a_hidden_one_is_ignored() {
        let mut math = clip(TimelineClipKind::Math, "式", TrackRef::video(1), 30);
        math.media_path = PathBuf::new();
        math.math.source = "x^2".to_owned();
        let mut project = Project {
            timeline_clips: vec![video("v", 30), math],
            ..Project::default()
        };
        let shown = error_of(&project, &request("out.mp4"));
        assert!(
            shown.contains("まだ書き出せません") && shown.contains("式"),
            "{shown}"
        );

        project.video_tracks[1].muted = true;
        assert!(
            plan_export(&project, &request("out.mp4")).is_ok(),
            "非表示の track の数式で失敗しました"
        );
        project.video_tracks[1].muted = false;
        project.timeline_clips[1].enabled = false;
        assert!(
            plan_export(&project, &request("out.mp4")).is_ok(),
            "無効にした数式で失敗しました"
        );
    }

    #[test]
    fn pitch_preserved_audio_fails_before_encoding() {
        let mut audio = clip(TimelineClipKind::Audio, "a", TrackRef::audio(0), 30);
        audio.preserve_pitch = true;
        let project = Project {
            timeline_clips: vec![audio],
            ..Project::default()
        };
        assert!(error_of(&project, &request("out.mp4")).contains("ピッチ保持"));
    }

    #[test]
    fn an_invalid_timeline_is_reported_as_it_is() {
        let mut project = Project {
            timeline_clips: vec![video("dup", 10), video("dup", 10)],
            ..Project::default()
        };
        project.timeline_clips[1].timeline_start_frame = 20;
        assert!(error_of(&project, &request("out.mp4")).contains("ID"));
    }
}
