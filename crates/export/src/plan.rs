//! 書き出しの計画。始める前に、書き出せない Project をここで全部落とす。GPU は触らない。
//! file は、出力先が素材と同じ実体でないかを確かめるために識別子を読むだけで、書かない。
//!
//! 各 frame の layer と音声区間は preview と同じ関数（[`mvm_preview::plan_frame`]・
//! [`mvm_preview::audio_spans`]）で決める。ここで持つのは、書き出し全体の尺・出力形式と、
//! preview では「描かない」で済ませている clip を書き出しでは失敗にする検査だけ。

use std::path::{Path, PathBuf};
use std::sync::Arc;

use mvm_core::Rational;
use mvm_core::output_timebase::OutputTimebase;
use mvm_math::require_ready;
use mvm_media::Mp4Settings;
use mvm_platform::{FileProbeStatus, ReplaceApproval, probe_file};
use mvm_preview::{AudioSpan, MathLibrary, audio_spans, output_size, output_timebase};
use mvm_project::timeline::validate_timeline;
use mvm_project::{ManimGenerationState, Project, TimelineClipKind};

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
    /// 書き出し先に既にある file の置き換えの承認。無ければ既存の file へは書き出さない。
    pub replace: Option<ReplaceApproval>,
}

/// 検査を通った書き出し。[`crate::run_export`] と [`crate::ExportJob`] へ渡す。
#[derive(Clone, Debug)]
pub struct ExportPlan {
    pub output_path: PathBuf,
    /// 計画の時点で書き出し先にあった file の置き換えの承認。`None` なら書き出し先は新しい file で、
    /// 書き終えたときに別の file ができていれば置き換えずに失敗する。
    pub replace: Option<ReplaceApproval>,
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
    /// 描けた数式。出力する数式・数式列・Graph のラベルは全てここにある（計画の時点で確かめる）。
    pub math: Arc<MathLibrary>,
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
pub fn plan_export(
    project: &Project,
    request: &ExportRequest,
    math: Arc<MathLibrary>,
) -> Result<ExportPlan, ExportError> {
    let output_path = check_output_path(&request.output_path)?;
    let replace = check_existing_output(&output_path, request.replace.as_ref())?;
    reject_source_as_output(project, &output_path)?;
    let validation =
        validate_timeline(project).map_err(|error| ExportError::failed(error.to_string()))?;
    let (width, height) =
        output_size(project).map_err(|error| ExportError::failed(error.to_string()))?;
    let fps = project
        .timeline_fps
        .reduced()
        .ok_or_else(|| ExportError::failed("timeline の fps が不正です"))?;
    reject_unrendered_clips(project, &math)?;
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
        output_path,
        replace,
        width,
        height,
        fps,
        crf: request.quality.crf(),
        burn_subtitles: request.burn_subtitles,
        total_frames,
        total_samples,
        timebase,
        audio_spans: spans,
        math,
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

/// 書き出し先に今ある file と承認を突き合わせる。無ければ新しい file として書く（古い承認は使わない）。
/// あれば、同じ実体への承認があるときだけ置き換える。承認の無い既存の file には書き出さない。
fn check_existing_output(
    output: &Path,
    approval: Option<&ReplaceApproval>,
) -> Result<Option<ReplaceApproval>, ExportError> {
    let current =
        ReplaceApproval::capture(output).map_err(|error| ExportError::failed(error.to_string()))?;
    match (current, approval) {
        (None, _) => Ok(None),
        (Some(current), Some(approval)) if current == *approval => Ok(Some(current)),
        (Some(_), Some(_)) => Err(ExportError::failed(format!(
            "置き換えを承認した後に書き出し先の file が変わりました: {}",
            output.display()
        ))),
        (Some(_), None) => Err(ExportError::failed(format!(
            "書き出し先に file があります。置き換えるか確かめてください: {}",
            output.display()
        ))),
    }
}

/// 出力先が Project の参照する file（素材・Manim の script と生成物）と同じ実体なら失敗させる。
/// 置き換えると元の素材を失う。パスの綴りではなく volume と file ID で比べるので、大文字小文字・
/// 別の綴り・hard link でも同じ file を見分ける。出力先がまだ無ければ、どの素材とも同じではない。
/// 素材の実体を確かめられなければ（開けない・directory など）、同じでないと言えないので失敗させる。
/// 素材が無いだけなら、出力先と同じ file ではない。
pub(crate) fn reject_source_as_output(project: &Project, output: &Path) -> Result<(), ExportError> {
    let target = probe_file(output);
    match target.status {
        FileProbeStatus::Missing => return Ok(()),
        FileProbeStatus::Unavailable => {
            return Err(ExportError::failed(format!(
                "書き出し先が素材と同じ file か確かめられません: {}",
                output.display()
            )));
        }
        FileProbeStatus::File => {}
    }
    let same = |path: &Path| -> Result<bool, ExportError> {
        let source = probe_file(path);
        match source.status {
            FileProbeStatus::Missing => Ok(false),
            FileProbeStatus::File => Ok(
                source.volume_serial == target.volume_serial && source.file_id == target.file_id
            ),
            FileProbeStatus::Unavailable => Err(ExportError::failed(format!(
                "素材が書き出し先と同じ file か確かめられません: {}",
                path.display()
            ))),
        }
    };
    let sources = project
        .timeline_clips
        .iter()
        .map(|clip| clip.media_path.as_path())
        .chain(
            project
                .media_items
                .iter()
                .map(|item| item.media_path.as_path()),
        )
        .chain(project.manim_assets.iter().flat_map(|asset| {
            [
                asset.script_path.as_path(),
                asset.generated_video_path.as_path(),
            ]
        }))
        .filter(|path| !path.as_os_str().is_empty());
    for source in sources {
        if same(source)? {
            return Err(ExportError::failed(format!(
                "書き出し先が Project の素材と同じ file です（素材を上書きします）: {}",
                source.display()
            )));
        }
    }
    Ok(())
}

/// 数式・数式列・Graph のラベルの描画が済んでいない、または Manim の動画を生成していない clip が出力に
/// 含まれていれば失敗させる。preview は描かずに済ませるが、書き出しで黙って抜くと見た目の違う動画ができる。
/// 出力しない clip（無効・非表示の track）は問わない。
fn reject_unrendered_clips(project: &Project, math: &MathLibrary) -> Result<(), ExportError> {
    require_ready(project, math).map_err(|error| ExportError::failed(error.to_string()))?;
    for clip in &project.timeline_clips {
        if !clip.enabled
            || !project.is_track_output_enabled(clip.track)
            || clip.kind != TimelineClipKind::Manim
        {
            continue;
        }
        let ready = project.manim_assets.iter().any(|asset| {
            asset.generated_video_path == clip.media_path
                && asset.generation_state == ManimGenerationState::Ready
        });
        if !ready {
            return Err(ExportError::failed(format!(
                "Manim の動画が生成されていません: {}",
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
            replace: None,
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
        match plan_export(project, request, Arc::new(MathLibrary::default())) {
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
        let plan = plan_export(
            &project,
            &request("out.mp4"),
            Arc::new(MathLibrary::default()),
        )
        .expect("計画");
        assert_eq!(plan.total_frames, 120);
        // 60 fps の 1 frame は 48 kHz の 800 sample。
        assert_eq!(plan.total_samples, 120 * 800);
        assert!(plan.has_audio());
        assert_eq!(plan.audio_spans.len(), 1);
        assert_eq!((plan.width, plan.height), (1920, 1080));
        assert_eq!(plan.fps, Rational::new(60, 1));
        assert_eq!(plan.crf, 23);
        assert!(plan.output_path.is_absolute());

        project.audio_tracks[0].muted = true;
        let silent = plan_export(
            &project,
            &request("out.mp4"),
            Arc::new(MathLibrary::default()),
        )
        .expect("無音");
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
        assert!(
            plan_export(
                &project,
                &request("OUT.MP4"),
                Arc::new(MathLibrary::default())
            )
            .is_ok()
        );
    }

    /// 既にある `path` を置き換える承認を付けた依頼。
    fn approved(path: &Path) -> ExportRequest {
        ExportRequest {
            replace: Some(ReplaceApproval::capture(path).unwrap().unwrap()),
            ..request(&path.to_string_lossy())
        }
    }

    #[test]
    fn rejects_an_output_that_is_a_source_file_under_any_spelling() {
        let dir = std::env::temp_dir().join(format!("mvm-plan-source-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let source = dir.join("source.mp4");
        std::fs::write(&source, b"source").unwrap();
        let linked = dir.join("linked.mp4");
        std::fs::hard_link(&source, &linked).unwrap();
        let other = dir.join("other.mp4");
        std::fs::write(&other, b"other").unwrap();

        let mut clip = video("v", 10);
        clip.media_path = source.clone();
        let project = Project {
            timeline_clips: vec![clip],
            ..Project::default()
        };
        let upper = PathBuf::from(source.to_string_lossy().to_uppercase());
        // 置き換えを承認していても、素材は置き換えない。
        for output in [&source, &upper, &linked] {
            let message = error_of(&project, &approved(output));
            assert!(
                message.contains("素材と同じ"),
                "{}: {message}",
                output.display()
            );
        }

        // media bin だけにある素材と Manim の生成物も守る。
        let bin = Project {
            timeline_clips: vec![video("v", 10)],
            media_items: vec![mvm_project::MediaItem {
                media_path: other.clone(),
                ..mvm_project::MediaItem::default()
            }],
            ..Project::default()
        };
        assert!(error_of(&bin, &approved(&other)).contains("素材と同じ"));
        let manim = Project {
            timeline_clips: vec![video("v", 10)],
            manim_assets: vec![mvm_project::ManimAsset {
                script_path: dir.join("scene.py"),
                scene_name: "Scene".to_owned(),
                generated_video_path: other.clone(),
                generation_state: ManimGenerationState::Ready,
                source_fingerprint: String::new(),
            }],
            ..Project::default()
        };
        assert!(error_of(&manim, &approved(&other)).contains("素材と同じ"));

        // 実体を確かめられない素材（ここでは directory）があれば、同じでないと言えないので失敗する。
        let unknown = Project {
            timeline_clips: vec![video("v", 10)],
            media_items: vec![mvm_project::MediaItem {
                media_path: dir.clone(),
                ..mvm_project::MediaItem::default()
            }],
            ..Project::default()
        };
        assert!(error_of(&unknown, &approved(&other)).contains("確かめられません"));

        // 素材でない既存 file は承認があれば、まだ無い file はそのまま書き出せる（負の対照）。
        let plan = plan_export(
            &project,
            &approved(&other),
            Arc::new(MathLibrary::default()),
        )
        .expect("承認した既存の file");
        assert!(plan.replace.is_some());
        let plan = plan_export(
            &project,
            &request(&dir.join("new.mp4").to_string_lossy()),
            Arc::new(MathLibrary::default()),
        )
        .expect("新しい file");
        assert!(plan.replace.is_none());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn an_existing_output_needs_an_approval_of_that_very_file() {
        let dir = std::env::temp_dir().join(format!("mvm-plan-existing-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let output = dir.join("out.mp4");
        std::fs::write(&output, b"earlier export").unwrap();
        let project = Project {
            timeline_clips: vec![video("v", 10)],
            ..Project::default()
        };
        let message = error_of(&project, &request(&output.to_string_lossy()));
        assert!(message.contains("確かめてください"), "{message}");

        let stale = approved(&output);
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(&output, b"rewritten by another tool").unwrap();
        let message = error_of(&project, &stale);
        assert!(message.contains("変わりました"), "{message}");

        // 承認の後に消えた file は新しい file として書く（承認は持ち越さない）。
        std::fs::remove_file(&output).unwrap();
        let plan = plan_export(&project, &stale, Arc::new(MathLibrary::default())).unwrap();
        assert!(plan.replace.is_none());
        std::fs::remove_dir_all(&dir).unwrap();
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
        let plan =
            plan_export(&project, &unburned, Arc::new(MathLibrary::default())).expect("字幕だけ");
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
            shown.contains("描画が済んでいません") && shown.contains("式"),
            "{shown}"
        );

        project.video_tracks[1].muted = true;
        assert!(
            plan_export(
                &project,
                &request("out.mp4"),
                Arc::new(MathLibrary::default())
            )
            .is_ok(),
            "非表示の track の数式で失敗しました"
        );
        project.video_tracks[1].muted = false;
        project.timeline_clips[1].enabled = false;
        assert!(
            plan_export(
                &project,
                &request("out.mp4"),
                Arc::new(MathLibrary::default())
            )
            .is_ok(),
            "無効にした数式で失敗しました"
        );

        // 描画が済んでいれば計画できる。
        project.timeline_clips[1].enabled = true;
        let mut artifact = mvm_math::MathArtifact::default();
        artifact.sequences.insert(
            "static".to_owned(),
            vec![Arc::new(mvm_math::Mask::new(4, 2, vec![255; 8]).unwrap())],
        );
        let library = MathLibrary::new(std::collections::HashMap::from([(
            mvm_math::MathJob::Static(mvm_math::TexSpec {
                source: "x^2".to_owned(),
                font_size: project.timeline_clips[1].math.font_size,
            }),
            Arc::new(artifact),
        )]));
        assert!(plan_export(&project, &request("out.mp4"), Arc::new(library)).is_ok());
    }

    #[test]
    fn pitch_preserved_audio_is_planned_like_other_audio() {
        // 60 frame の素材を 200% で 30 frame にする（等速の clip はピッチ保持を持てない）。
        let mut audio = clip(TimelineClipKind::Audio, "a", TrackRef::audio(0), 60);
        audio.speed = Rational::new(2, 1);
        audio.preserve_pitch = true;
        let project = Project {
            timeline_clips: vec![audio],
            ..Project::default()
        };
        let plan = plan_export(
            &project,
            &request("out.mp4"),
            Arc::new(MathLibrary::default()),
        )
        .expect("ピッチ保持の音声");
        assert!(plan.has_audio());
        assert_eq!(plan.total_frames, 30);
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
