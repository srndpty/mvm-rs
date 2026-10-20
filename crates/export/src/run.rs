//! 計画どおりに 1 frame ずつ合成して encode し、検証してから正規名へ置く。
//!
//! encode 中の file は job ごとに一意な名前で排他的に作り、この job が作ったものだけを消す。
//! 途中で失敗・取消したときはそれを消し、正規名には何も書かない。
//!
//! 正規名へは、計画の時点で無かったなら既存を置換しない rename で置く（書き出し中に別の job や別の tool が
//! 作った file を上書きしない）。あったなら、置き換えを承認した file のままであることを照合してから置き換える。

use std::fs::OpenOptions;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use mvm_media::{Mp4Writer, ProbedMedia, probe_media_file};
use mvm_platform::{ReplaceApproval, place_file};
use mvm_preview::{AudioMixer, FramePlan, PlannedSource, StillPreview, plan_frame};
use mvm_project::Project;

use crate::error::ExportError;
use crate::plan::{ExportPlan, reject_source_as_output};

/// encode 中の file の名前の接尾辞。`<出力>.mvmtmp.<process ID>.<連番>` にする。
pub const TEMPORARY_MARKER: &str = ".mvmtmp.";

/// 一意な名前を探す回数の上限。前に落ちた process の残りと名前がぶつかったときだけ次を試す。
const TEMPORARY_ATTEMPTS: u32 = 64;

static TEMPORARY_SERIAL: AtomicU32 = AtomicU32::new(1);

/// 書き出しの進み具合を受け取り、取消を伝える。worker thread から呼ぶ。
pub trait ExportControl {
    /// `completed` frame を encode した。
    fn progress(&self, completed: i64, total: i64);
    /// true なら、次の frame の前で止める。
    fn cancelled(&self) -> bool;
}

/// 書き終えた MP4。
#[derive(Clone, Debug, PartialEq)]
pub struct ExportSummary {
    pub output_path: std::path::PathBuf,
    pub frames: i64,
    pub audio_samples: i64,
    pub duration_seconds: f64,
}

/// 計画を実行する。`project` は計画を作った Project と同じもの。
pub fn run_export(
    project: &Project,
    plan: &ExportPlan,
    control: &dyn ExportControl,
) -> Result<ExportSummary, ExportError> {
    if let Some(parent) = plan.output_path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| {
            ExportError::failed(format!(
                "書き出し先の directory を作れません ({}): {error}",
                parent.display()
            ))
        })?;
    }
    // drop（失敗・取消・panic の巻き戻し）で、この job が作った file だけを消す。
    let temporary = TemporaryOutput::create(&plan.output_path)?;
    let summary = encode(project, plan, temporary.path(), control)
        .and_then(|encoded| verify(plan, temporary.path(), &encoded))?;
    // 最後の frame の後に取り消されたら、公開しない。
    if control.cancelled() {
        return Err(ExportError::Cancelled);
    }
    // 計画の後に素材と出力先が同じ file になっていないか、置く直前にもう一度確かめる。
    reject_source_as_output(project, &plan.output_path)?;
    temporary.publish(&plan.output_path, plan.replace.as_ref())?;
    Ok(summary)
}

/// この job が排他的に作った encode 中の file。公開しないまま drop すると消す。
struct TemporaryOutput {
    path: PathBuf,
    published: bool,
}

impl TemporaryOutput {
    fn create(output: &Path) -> Result<Self, ExportError> {
        let mut last = None;
        for _ in 0..TEMPORARY_ATTEMPTS {
            let serial = TEMPORARY_SERIAL.fetch_add(1, Ordering::Relaxed);
            let mut name = output.as_os_str().to_owned();
            name.push(format!("{TEMPORARY_MARKER}{}.{serial}", std::process::id()));
            let path = PathBuf::from(name);
            // create_new は既存の file を開かない。他の job の一時 file には触れない。
            match OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(_) => {
                    return Ok(Self {
                        path,
                        published: false,
                    });
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    last = Some(error);
                }
                Err(error) => {
                    return Err(ExportError::failed(format!(
                        "一時 file を作れません ({}): {error}",
                        path.display()
                    )));
                }
            }
        }
        Err(ExportError::failed(format!(
            "一時 file の名前を決められません ({}): {}",
            output.display(),
            last.map_or_else(String::new, |error| error.to_string())
        )))
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn publish(
        mut self,
        output: &Path,
        approval: Option<&ReplaceApproval>,
    ) -> Result<(), ExportError> {
        place_file(&self.path, output, approval).map_err(|error| {
            ExportError::failed(format!(
                "書き出した file を {} へ置けません: {error}",
                output.display()
            ))
        })?;
        self.published = true;
        Ok(())
    }
}

impl Drop for TemporaryOutput {
    fn drop(&mut self) {
        if !self.published {
            // 失敗の理由は元の error。片付けの失敗で上書きしない。
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

/// encode が書いた中身。検証と結果の報告に使う。
struct Encoded {
    frames: i64,
    audio_samples: i64,
}

fn encode(
    project: &Project,
    plan: &ExportPlan,
    temporary: &Path,
    control: &dyn ExportControl,
) -> Result<Encoded, ExportError> {
    let mut preview =
        StillPreview::new().map_err(|error| ExportError::failed(error.to_string()))?;
    let mut mixer = if plan.has_audio() {
        Some(
            AudioMixer::open(plan.audio_spans.clone(), plan.timebase, plan.fps)
                .map_err(|error| ExportError::failed(error.to_string()))?,
        )
    } else {
        None
    };
    let mut writer = Mp4Writer::create(temporary, &plan.mp4_settings())
        .map_err(|error| ExportError::failed(error.to_string()))?;
    let mut interleaved = Vec::new();
    let mut left = Vec::new();
    let mut right = Vec::new();
    for frame in 0..plan.total_frames {
        if control.cancelled() {
            return Err(ExportError::Cancelled);
        }
        let layers = frame_layers(project, plan, frame)?;
        let image = preview
            .render(project, frame, &layers)
            .map_err(|error| frame_error(frame, plan, error))?;
        writer
            .write_frame(&image.rgba)
            .map_err(|error| frame_error(frame, plan, error))?;
        if let Some(mixer) = &mut mixer {
            let start = first_sample(plan, frame)?;
            let end = first_sample(plan, frame + 1)?;
            let count = usize::try_from(end - start)
                .map_err(|_| ExportError::failed("frame の sample 数が不正です"))?;
            interleaved.clear();
            interleaved.resize(count * 2, 0.0);
            mixer
                .mix(start, &mut interleaved)
                .map_err(|error| frame_error(frame, plan, error))?;
            left.clear();
            right.clear();
            for pair in interleaved.as_chunks::<2>().0 {
                left.push(pair[0]);
                right.push(pair[1]);
            }
            writer
                .write_audio(&left, &right)
                .map_err(|error| frame_error(frame, plan, error))?;
        }
        control.progress(frame + 1, plan.total_frames);
    }
    let summary = writer
        .finish()
        .map_err(|error| ExportError::failed(error.to_string()))?;
    Ok(Encoded {
        frames: summary.video_frames,
        audio_samples: summary.audio_samples,
    })
}

/// preview と同じ計画。焼き込まない字幕だけを外す。preview が描かずに済ませる clip が残っていれば
/// 失敗させる（計画の検査を抜けたものを黙って落とさない）。
fn frame_layers(
    project: &Project,
    plan: &ExportPlan,
    frame: i64,
) -> Result<FramePlan, ExportError> {
    let mut layers =
        plan_frame(project, frame, &plan.math).map_err(|error| frame_error(frame, plan, error))?;
    if layers.skipped_clips > 0 {
        let reason = layers
            .notes
            .first()
            .cloned()
            .unwrap_or_else(|| "書き出せない clip が含まれています".to_owned());
        return Err(frame_error(frame, plan, reason));
    }
    if !plan.burn_subtitles {
        layers
            .layers
            .retain(|layer| !matches!(layer.source, PlannedSource::Subtitle(_)));
    }
    Ok(layers)
}

fn first_sample(plan: &ExportPlan, frame: i64) -> Result<i64, ExportError> {
    plan.timebase
        .first_audio_sample(frame)
        .map_err(|error| ExportError::failed(error.to_string()))
}

fn frame_error(frame: i64, plan: &ExportPlan, error: impl std::fmt::Display) -> ExportError {
    ExportError::failed(format!(
        "frame {frame}（{}）を書き出せません: {error}",
        seconds_label(frame, plan)
    ))
}

fn seconds_label(frame: i64, plan: &ExportPlan) -> String {
    let seconds = frame as f64 * plan.fps.den as f64 / plan.fps.num as f64;
    format!("{seconds:.2} 秒")
}

/// 書いた file を開き直し、寸法・fps・frame 数が計画と一致することを確かめる。
fn verify(
    plan: &ExportPlan,
    temporary: &Path,
    encoded: &Encoded,
) -> Result<ExportSummary, ExportError> {
    if encoded.frames != plan.total_frames || encoded.audio_samples != plan.total_samples {
        return Err(ExportError::failed(format!(
            "encode した量が計画と違います（映像 {} / {} frame、音声 {} / {} sample）",
            encoded.frames, plan.total_frames, encoded.audio_samples, plan.total_samples
        )));
    }
    let probed = probe_media_file(temporary).map_err(|error| {
        ExportError::failed(format!("書き出した file を検証できません: {error}"))
    })?;
    let ProbedMedia::Video(video) = probed else {
        return Err(ExportError::failed(
            "書き出した file が映像として読めません",
        ));
    };
    let size_matches = u32::try_from(video.width).ok() == Some(plan.width)
        && u32::try_from(video.height).ok() == Some(plan.height);
    let fps_matches =
        i64::from(video.fps_num) * plan.fps.den == i64::from(video.fps_den) * plan.fps.num;
    if !size_matches || !fps_matches || video.frame_count != plan.total_frames {
        return Err(ExportError::failed(format!(
            "書き出した file が計画と違います（{}x{}・{}/{} fps・{} frame、計画は {}x{}・{}/{} fps・{} frame）",
            video.width,
            video.height,
            video.fps_num,
            video.fps_den,
            video.frame_count,
            plan.width,
            plan.height,
            plan.fps.num,
            plan.fps.den,
            plan.total_frames
        )));
    }
    if video.has_audio != plan.has_audio() {
        return Err(ExportError::failed(
            "書き出した file の音声の有無が計画と違います",
        ));
    }
    Ok(ExportSummary {
        output_path: plan.output_path.clone(),
        frames: encoded.frames,
        audio_samples: encoded.audio_samples,
        duration_seconds: plan.total_frames as f64 * plan.fps.den as f64 / plan.fps.num as f64,
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn each_job_owns_a_distinct_temporary_and_removes_only_its_own() {
        let dir = std::env::temp_dir().join(format!("mvm-export-temp-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let output = dir.join("out.mp4");
        // 別の process が書いている途中の一時 file。
        let foreign = dir.join(format!("out.mp4{TEMPORARY_MARKER}999999.1"));
        std::fs::write(&foreign, b"other job").unwrap();

        let first = TemporaryOutput::create(&output).unwrap();
        let second = TemporaryOutput::create(&output).unwrap();
        assert_ne!(first.path(), second.path());
        assert!(first.path().is_file() && second.path().is_file());
        let first_path = first.path().to_path_buf();
        drop(first);
        assert!(
            !first_path.exists(),
            "取り消した job の一時 file が残りました"
        );
        assert!(second.path().is_file(), "別の job の一時 file を消しました");
        assert_eq!(std::fs::read(&foreign).unwrap(), b"other job");

        std::fs::write(second.path(), b"encoded").unwrap();
        second.publish(&output, None).unwrap();
        assert_eq!(std::fs::read(&output).unwrap(), b"encoded");

        // 計画の時点で無かった正規名に、書き出し中に別の job が置いた file は上書きしない。
        let late = TemporaryOutput::create(&output).unwrap();
        std::fs::write(late.path(), b"late").unwrap();
        let late_path = late.path().to_path_buf();
        assert!(late.publish(&output, None).is_err());
        assert_eq!(std::fs::read(&output).unwrap(), b"encoded");
        assert!(
            !late_path.exists(),
            "公開できなかった一時 file が残りました"
        );
        let mut names: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        assert_eq!(
            names,
            ["out.mp4", &format!("out.mp4{TEMPORARY_MARKER}999999.1")]
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
