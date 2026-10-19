//! 計画どおりに 1 frame ずつ合成して encode し、検証してから正規名へ置く。
//!
//! 途中で失敗・取消したときは一時 file を消し、正規名には何も書かない。

use std::path::Path;

use mvm_media::{Mp4Writer, ProbedMedia, probe_media_file};
use mvm_preview::{AudioMixer, FramePlan, PlannedSource, StillPreview, plan_frame};
use mvm_project::Project;

use crate::error::ExportError;
use crate::plan::ExportPlan;

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
    remove_if_present(&plan.temporary_path)?;
    let result = encode(project, plan, control)
        .and_then(|encoded| verify(plan, &encoded))
        .and_then(|summary| {
            // 最後の frame の後に取り消されたら、公開しない。
            if control.cancelled() {
                Err(ExportError::Cancelled)
            } else {
                Ok(summary)
            }
        })
        .and_then(|summary| publish(plan).map(|()| summary));
    if result.is_err() {
        // 失敗の理由は元の error。片付けの失敗で上書きしない。
        let _ = std::fs::remove_file(&plan.temporary_path);
    }
    result
}

/// encode が書いた中身。検証と結果の報告に使う。
struct Encoded {
    frames: i64,
    audio_samples: i64,
}

fn encode(
    project: &Project,
    plan: &ExportPlan,
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
    let mut writer = Mp4Writer::create(&plan.temporary_path, &plan.mp4_settings())
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
    let mut layers = plan_frame(project, frame).map_err(|error| frame_error(frame, plan, error))?;
    if layers.skipped_clips > 0 {
        return Err(frame_error(
            frame,
            plan,
            "書き出せない clip が含まれています",
        ));
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
fn verify(plan: &ExportPlan, encoded: &Encoded) -> Result<ExportSummary, ExportError> {
    if encoded.frames != plan.total_frames || encoded.audio_samples != plan.total_samples {
        return Err(ExportError::failed(format!(
            "encode した量が計画と違います（映像 {} / {} frame、音声 {} / {} sample）",
            encoded.frames, plan.total_frames, encoded.audio_samples, plan.total_samples
        )));
    }
    let probed = probe_media_file(&plan.temporary_path).map_err(|error| {
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

fn publish(plan: &ExportPlan) -> Result<(), ExportError> {
    std::fs::rename(&plan.temporary_path, &plan.output_path).map_err(|error| {
        ExportError::failed(format!(
            "書き出した file を {} へ置けません: {error}",
            plan.output_path.display()
        ))
    })
}

fn remove_if_present(path: &Path) -> Result<(), ExportError> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(ExportError::failed(format!(
            "前回の一時 file を消せません ({}): {error}",
            path.display()
        ))),
    }
}
