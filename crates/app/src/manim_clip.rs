//! Manim clip の生成。利用者の script の Scene を Project の寸法・fps の mp4 にし、動画の clip として置く。
//!
//! 生成した動画は `<Project の directory>/cache/manim/` に置く（保存していない Project では生成できない）。
//! 同じ script・Scene をもう一度生成すると asset と clip の動画を置き換える（1 回の undo）。
//! 生成した動画は上書きしないので、undo で前の参照へ戻ると前の動画がそのまま使われる。
//! 生成の開始時の Project（読み込みの番号・保存先・寸法・fps）と置く位置を覚え、適用の直前に照合する。
//! 生成中に Project を入れ替えた・寸法や fps を変えた場合は適用しない。

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

/// 生成を始めたときの Project。結果はこれと同じ Project にだけ適用する。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ManimTarget {
    load_serial: u64,
    path: PathBuf,
    width: i32,
    height: i32,
    fps: Rational,
    /// 新しい clip を置く位置（開始時の再生ヘッド）。
    start_frame: i64,
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
        let target = ManimTarget {
            load_serial: self.load_serial,
            path: project_path.to_path_buf(),
            width: request.width,
            height: request.height,
            fps: request.fps,
            start_frame: self.playhead_frame,
        };
        let job = BackgroundJob::start(
            "mvm-manim-clip",
            self.preview_wake.clone(),
            move |control| generate(&request, control),
        )?;
        self.background.manim = Some((job, target));
        Ok(())
    }

    pub fn is_generating_manim(&self) -> bool {
        self.background.manim.is_some()
    }

    pub fn cancel_manim_clip(&mut self) {
        if let Some((job, _)) = &self.background.manim {
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

    /// 生成を始めたときと同じ Project・保存先・寸法・fps か。
    fn check_manim_target(&self, target: &ManimTarget) -> Result<(), AppError> {
        if target.load_serial != self.load_serial || self.path() != Some(target.path.as_path()) {
            return Err(AppError::new(
                "生成中に Project が変わったので、生成した Manim clip を置きませんでした",
            ));
        }
        if target.width != self.project.output_width
            || target.height != self.project.output_height
            || target.fps != self.project.timeline_fps
        {
            return Err(AppError::new(
                "生成中に Project の寸法か fps が変わったので、生成した Manim clip を置きませんでした。もう一度生成してください",
            ));
        }
        Ok(())
    }

    /// 生成が終わっていれば Project へ入れる（1 回の undo）。clip が無ければ再生ヘッドへ置く。
    pub fn poll_manim_clip(&mut self) -> Option<Result<(), AppError>> {
        let finished = self.background.manim.as_mut()?.0.take_finished()?;
        let target = self.background.manim.take()?.1;
        let outcome = match finished.and_then(|result| result) {
            Ok(outcome) => outcome,
            Err(error) => return Some(Err(error)),
        };
        if let Err(error) = self.check_manim_target(&target) {
            return Some(Err(error));
        }
        let start = target.start_frame;
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
    use std::path::Path;

    use super::*;

    fn saved_session(directory: &Path, name: &str) -> Session {
        std::fs::create_dir_all(directory).unwrap();
        let mut session = Session::new();
        session.create_project(&directory.join(name)).unwrap();
        session
    }

    fn target_of(session: &Session) -> ManimTarget {
        ManimTarget {
            load_serial: session.load_serial,
            path: session.path().unwrap().to_path_buf(),
            width: session.project.output_width,
            height: session.project.output_height,
            fps: session.project.timeline_fps,
            start_frame: 30,
        }
    }

    fn outcome(video: &str) -> ManimClipOutcome {
        ManimClipOutcome {
            asset: ManimAsset::ready(
                PathBuf::from("C:\\work\\scene.py"),
                "Intro".to_owned(),
                PathBuf::from(video),
                "a".repeat(64),
            )
            .unwrap(),
            fps: Rational::new(60, 1),
            frames: 120,
        }
    }

    /// 生成が `outcome` で終わったことにして、`target` の Project へ適用する。
    fn finish(
        session: &mut Session,
        outcome: ManimClipOutcome,
        target: ManimTarget,
    ) -> Result<(), AppError> {
        session.background.manim = Some((
            BackgroundJob::start("試験", None, move |_| Ok(outcome)).unwrap(),
            target,
        ));
        loop {
            if let Some(result) = session.poll_manim_clip() {
                return result;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
    }

    fn manim_videos(session: &Session) -> Vec<PathBuf> {
        session
            .project()
            .timeline_clips
            .iter()
            .filter(|clip| clip.kind == TimelineClipKind::Manim)
            .map(|clip| clip.media_path.clone())
            .collect()
    }

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
    fn regeneration_switches_the_reference_and_undo_redo_follow_it() {
        let directory = std::env::temp_dir().join(format!("mvm-manim-app-{}", std::process::id()));
        let mut session = saved_session(&directory, "a.mvm");
        let target = target_of(&session);
        finish(
            &mut session,
            outcome("C:\\work\\cache\\manim\\one.mp4"),
            target.clone(),
        )
        .unwrap();
        assert_eq!(
            manim_videos(&session),
            vec![PathBuf::from("C:\\work\\cache\\manim\\one.mp4")]
        );
        // clip は生成を始めたときの再生ヘッドに置く。
        assert_eq!(session.project().timeline_clips[0].timeline_start_frame, 30);
        // 同じ script・Scene の再生成は、別の動画へ参照を切り替える（1 回の undo）。
        finish(
            &mut session,
            outcome("C:\\work\\cache\\manim\\two.mp4"),
            target,
        )
        .unwrap();
        assert_eq!(
            manim_videos(&session),
            vec![PathBuf::from("C:\\work\\cache\\manim\\two.mp4")]
        );
        assert_eq!(session.project().manim_assets.len(), 1);
        session.undo().unwrap();
        assert_eq!(
            manim_videos(&session),
            vec![PathBuf::from("C:\\work\\cache\\manim\\one.mp4")]
        );
        session.redo().unwrap();
        assert_eq!(
            manim_videos(&session),
            vec![PathBuf::from("C:\\work\\cache\\manim\\two.mp4")]
        );
        session.undo().unwrap();
        session.undo().unwrap();
        assert!(session.project().manim_assets.is_empty());
        drop(session);
        std::fs::remove_dir_all(&directory).unwrap();
    }

    #[test]
    fn a_result_for_another_project_is_not_applied() {
        let directory =
            std::env::temp_dir().join(format!("mvm-manim-switch-{}", std::process::id()));
        let mut session = saved_session(&directory, "a.mvm");
        let stale = target_of(&session);
        session.create_project(&directory.join("b.mvm")).unwrap();
        let error = finish(&mut session, outcome("C:\\work\\one.mp4"), stale).unwrap_err();
        assert!(error.message().contains("Project が変わった"), "{error}");
        assert!(manim_videos(&session).is_empty());
        assert!(session.project().manim_assets.is_empty());
        drop(session);
        std::fs::remove_dir_all(&directory).unwrap();
    }

    #[test]
    fn a_result_rendered_for_another_size_or_rate_is_not_applied() {
        let directory = std::env::temp_dir().join(format!("mvm-manim-size-{}", std::process::id()));
        let mut session = saved_session(&directory, "a.mvm");
        let mut stale = target_of(&session);
        stale.width = 1280;
        stale.height = 720;
        let error = finish(&mut session, outcome("C:\\work\\one.mp4"), stale).unwrap_err();
        assert!(error.message().contains("寸法か fps"), "{error}");
        let mut stale = target_of(&session);
        stale.fps = Rational::new(30, 1);
        assert!(finish(&mut session, outcome("C:\\work\\one.mp4"), stale).is_err());
        assert!(session.project().manim_assets.is_empty());
        drop(session);
        std::fs::remove_dir_all(&directory).unwrap();
    }

    #[test]
    fn switching_projects_drops_the_running_generation() {
        let directory = std::env::temp_dir().join(format!("mvm-manim-drop-{}", std::process::id()));
        let mut session = saved_session(&directory, "a.mvm");
        let target = target_of(&session);
        session.background.manim = Some((
            BackgroundJob::start("試験", None, |control| {
                while !control.is_cancelled() {
                    std::thread::sleep(std::time::Duration::from_millis(1));
                }
                Err(AppError::new("取り消しました"))
            })
            .unwrap(),
            target,
        ));
        session.open_project(&directory.join("a.mvm")).unwrap();
        assert!(
            !session.is_generating_manim(),
            "読み込み直しで生成が残りました"
        );
        drop(session);
        std::fs::remove_dir_all(&directory).unwrap();
    }
}
