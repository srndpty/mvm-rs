//! Manim clip の生成。利用者の script の Scene を Project の寸法・fps の mp4 にし、動画の clip として置く。
//!
//! 生成した動画は `<Project の directory>/cache/manim/` に置く（保存していない Project では生成できない）。
//! 同じ script・Scene をもう一度生成すると asset と clip の動画を置き換える（1 回の undo）。

use std::path::PathBuf;

use mvm_core::Rational;
use mvm_math::{ManimClipRequest, render_manim_clip};
use mvm_media::{ProbedMedia, probe_media_file};
use mvm_platform::find_on_path;
use mvm_project::timeline::{append_manim_timeline_clip_at, clip_interval, install_manim_asset};
use mvm_project::{ManimAsset, TimelineClipKind, TrackKind, TrackRef};

use crate::background::{BackgroundJob, JobControl};
use crate::error::AppError;
use crate::preferences::Preferences;
use crate::session::Session;

#[derive(Clone, Debug)]
pub struct ManimClipOutcome {
    asset: ManimAsset,
    fps: Rational,
    frames: i64,
}

fn manim_executable() -> Result<PathBuf, AppError> {
    let configured = Preferences::load()?.manim_executable;
    if !configured.trim().is_empty() {
        return Ok(PathBuf::from(configured.trim()));
    }
    find_on_path("manim.exe").ok_or_else(|| {
        AppError::new("manim.exe が PATH に見つかりません。Manim Community を導入してください")
    })
}

fn generate(
    request: &ManimClipRequest,
    control: &JobControl,
) -> Result<ManimClipOutcome, AppError> {
    let cancel = control.cancel_flag();
    let output =
        render_manim_clip(request, &cancel).map_err(|error| AppError::new(error.to_string()))?;
    let ProbedMedia::Video(video) = probe_media_file(&output.video)
        .map_err(|error| AppError::new(format!("生成した動画を読めません: {error}")))?
    else {
        return Err(AppError::new("生成した file が動画として読めません"));
    };
    let fps = Rational::new(i64::from(video.fps_num), i64::from(video.fps_den))
        .reduced()
        .ok_or_else(|| AppError::new("生成した動画の fps が不正です"))?;
    let asset = ManimAsset::ready(
        request.script.clone(),
        request.scene.clone(),
        output.video,
        output.fingerprint,
    )
    .map_err(|error| AppError::new(error.to_string()))?;
    control.set_progress(1.0);
    Ok(ManimClipOutcome {
        asset,
        fps,
        frames: video.frame_count,
    })
}

impl Session {
    /// `script` の `scene` を生成する。終わったら [`Self::poll_manim_clip`] が Project へ入れる。
    pub fn start_manim_clip(&mut self, script: PathBuf, scene: String) -> Result<(), AppError> {
        if self.background.manim.is_some() {
            return Err(AppError::new("Manim を生成中です"));
        }
        let Some(project_path) = self.path() else {
            return Err(AppError::new(
                "Manim clip は Project を保存してから生成してください",
            ));
        };
        let output_directory = project_path
            .parent()
            .map(PathBuf::from)
            .unwrap_or_default()
            .join("cache")
            .join("manim");
        let request = ManimClipRequest {
            manim: manim_executable()?,
            script,
            scene: scene.trim().to_owned(),
            output_directory,
            width: self.project.output_width,
            height: self.project.output_height,
            fps: self.project.timeline_fps,
        };
        let job = BackgroundJob::start(
            "mvm-manim-clip",
            self.preview_wake.clone(),
            move |control| generate(&request, control),
        )?;
        self.background.manim = Some(job);
        Ok(())
    }

    pub fn is_generating_manim(&self) -> bool {
        self.background.manim.is_some()
    }

    pub fn cancel_manim_clip(&mut self) {
        if let Some(job) = &self.background.manim {
            job.cancel();
        }
    }

    /// 生成した Manim clip の asset。
    pub fn manim_asset(&self, clip_id: &str) -> Option<&ManimAsset> {
        let clip = self.project.clip(clip_id)?;
        self.project
            .manim_assets
            .iter()
            .find(|asset| asset.generated_video_path == clip.media_path)
    }

    /// 同じ script・Scene をもう一度生成する（script を直した後など）。
    pub fn regenerate_manim_clip(&mut self, clip_id: &str) -> Result<(), AppError> {
        let Some(asset) = self.manim_asset(clip_id).cloned() else {
            return Err(AppError::new("この clip の Manim の生成元がありません"));
        };
        self.start_manim_clip(asset.script_path, asset.scene_name)
    }

    /// 生成が終わっていれば Project へ入れる（1 回の undo）。clip が無ければ再生ヘッドへ置く。
    pub fn poll_manim_clip(&mut self) -> Option<Result<(), AppError>> {
        let finished = self.background.manim.as_mut()?.take_finished()?;
        self.background.manim = None;
        let outcome = match finished.and_then(|result| result) {
            Ok(outcome) => outcome,
            Err(error) => return Some(Err(error)),
        };
        let start = self.playhead_frame;
        let mut factory = self.id_factory();
        let clip_id = factory.next();
        self.id_serial = factory.serial;
        Some(self.transact(move |project| {
            let video = outcome.asset.generated_video_path.clone();
            let replaced =
                install_manim_asset(project, outcome.asset.clone(), outcome.fps, outcome.frames)?;
            let placed = project
                .timeline_clips
                .iter()
                .any(|clip| clip.kind == TimelineClipKind::Manim && clip.media_path == video);
            if placed {
                return Ok(replaced);
            }
            // 置く位置で空いている最初の映像 track。無ければ足す。
            let duration_end = start + outcome.frames;
            let free = (0..project.video_tracks.len()).find(|index| {
                !project.timeline_clips.iter().any(|clip| {
                    clip.track == TrackRef::video(*index)
                        && clip_interval(project, clip)
                            .is_ok_and(|(begin, end)| begin < duration_end && start < end)
                })
            });
            let track = match free {
                Some(index) => index,
                None => {
                    mvm_project::timeline::add_track(project, TrackKind::Video)?;
                    project.video_tracks.len() - 1
                }
            };
            let asset = outcome.asset.clone();
            append_manim_timeline_clip_at(
                project,
                &asset,
                clip_id,
                outcome.fps,
                outcome.frames,
                start,
                TrackRef::video(track),
            )
        }))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn an_unsaved_project_cannot_generate() {
        let mut session = Session::new();
        let error = session
            .start_manim_clip(PathBuf::from("scene.py"), "Scene1".to_owned())
            .unwrap_err();
        assert!(error.message().contains("保存"), "{error}");
        assert!(!session.is_generating_manim());
    }

    #[test]
    fn a_finished_generation_installs_the_asset_and_places_one_clip() {
        let mut session = Session::new();
        let asset = ManimAsset::ready(
            PathBuf::from("C:\\work\\scene.py"),
            "Intro".to_owned(),
            PathBuf::from("C:\\work\\cache\\manim\\a.mp4"),
            "a".repeat(64),
        )
        .unwrap();
        let outcome = ManimClipOutcome {
            asset: asset.clone(),
            fps: Rational::new(60, 1),
            frames: 120,
        };
        session.background.manim =
            Some(BackgroundJob::start("試験", None, move |_| Ok(outcome)).unwrap());
        let result = loop {
            if let Some(result) = session.poll_manim_clip() {
                break result;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        };
        result.unwrap();
        let project = session.project();
        assert_eq!(project.manim_assets, vec![asset]);
        let clips: Vec<_> = project
            .timeline_clips
            .iter()
            .filter(|clip| clip.kind == TimelineClipKind::Manim)
            .collect();
        assert_eq!(clips.len(), 1);
        assert_eq!(clips[0].source_frame_count, 120);
        session.undo().unwrap();
        assert!(
            session.project().manim_assets.is_empty(),
            "1 回の undo で戻りません"
        );
    }
}
