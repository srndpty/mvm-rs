//! Manim clip（利用者が書いた任意の Scene）を mp4 に描く。数式 clip の escape hatch。
//!
//! 出力の寸法と fps は Project に合わせる。生成した動画は `<Project の directory>/cache/manim/` に、
//! script の内容・Scene 名・寸法・fps の hash を名前にして置く（同じ指定なら同じ file）。

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use mvm_core::Rational;
use mvm_platform::{ProcessOutcome, ProcessRequest, run_process};
use sha2::{Digest, Sha256};

use crate::error::MathError;

const RENDER_TIMEOUT: Duration = Duration::from_secs(30 * 60);
const OUTPUT_NAME: &str = "mvm_manim_output";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ManimClipRequest {
    pub manim: PathBuf,
    pub script: PathBuf,
    pub scene: String,
    pub output_directory: PathBuf,
    pub width: i32,
    pub height: i32,
    pub fps: Rational,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ManimClipOutput {
    pub video: PathBuf,
    /// script の内容の SHA-256（小文字 16 進）。script が変わったかの判定に使う。
    pub fingerprint: String,
    pub log: String,
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// script の内容の SHA-256。
pub fn script_fingerprint(script: &Path) -> Result<String, MathError> {
    let bytes = std::fs::read(script).map_err(|error| {
        MathError::new(format!(
            "Manim script を読めません ({}): {error}",
            script.display()
        ))
    })?;
    Ok(hex(&Sha256::digest(&bytes)))
}

fn find_output(directory: &Path) -> Option<PathBuf> {
    let entries = std::fs::read_dir(directory).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if let Some(found) = find_output(&path) {
                return Some(found);
            }
        } else if path.file_stem() == Some(OsStr::new(OUTPUT_NAME))
            && path.extension() == Some(OsStr::new("mp4"))
        {
            return Some(path);
        }
    }
    None
}

pub fn render_manim_clip(
    request: &ManimClipRequest,
    cancel: &AtomicBool,
) -> Result<ManimClipOutput, MathError> {
    if request.scene.trim().is_empty()
        || !request
            .scene
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_')
    {
        return Err(MathError::new("Scene 名は英数字と _ で指定してください"));
    }
    if request.width <= 0 || request.height <= 0 || !request.fps.is_positive() {
        return Err(MathError::new("Manim の出力の寸法または fps が不正です"));
    }
    let fingerprint = script_fingerprint(&request.script)?;
    let mut hasher = Sha256::new();
    for part in [
        fingerprint.as_str(),
        request.scene.as_str(),
        &format!(
            "{}x{}@{}/{}",
            request.width, request.height, request.fps.num, request.fps.den
        ),
    ] {
        hasher.update((part.len() as u64).to_le_bytes());
        hasher.update(part.as_bytes());
    }
    let key = hex(&hasher.finalize());
    std::fs::create_dir_all(&request.output_directory)
        .map_err(|error| MathError::new(format!("Manim の出力先を作れません: {error}")))?;
    let video = request.output_directory.join(format!("{key}.mp4"));
    let job = request
        .output_directory
        .join("jobs")
        .join(format!("{}-{key}", std::process::id()));
    let _ = std::fs::remove_dir_all(&job);
    std::fs::create_dir_all(&job)
        .map_err(|error| MathError::new(format!("Manim の作業 directory を作れません: {error}")))?;
    let resolution = format!("{},{}", request.width, request.height);
    let fps = format!("{}", request.fps.num as f64 / request.fps.den as f64);
    let media = job.join("media");
    let arguments = [
        OsStr::new("render"),
        OsStr::new("--format"),
        OsStr::new("mp4"),
        OsStr::new("--progress_bar"),
        OsStr::new("none"),
        OsStr::new("--resolution"),
        OsStr::new(resolution.as_str()),
        OsStr::new("--fps"),
        OsStr::new(fps.as_str()),
        OsStr::new("--media_dir"),
        media.as_os_str(),
        OsStr::new("--output_file"),
        OsStr::new(OUTPUT_NAME),
        request.script.as_os_str(),
        OsStr::new(request.scene.as_str()),
    ];
    let stdout = job.join("stdout.txt");
    let stderr = job.join("stderr.txt");
    let outcome = run_process(
        &ProcessRequest {
            executable: &request.manim,
            arguments: &arguments,
            working_directory: request.script.parent(),
            stdout_path: Some(&stdout),
            stderr_path: Some(&stderr),
            timeout: Some(RENDER_TIMEOUT),
        },
        &mut || cancel.load(Ordering::Relaxed),
    );
    let log = format!(
        "{}{}",
        std::fs::read_to_string(&stdout).unwrap_or_default(),
        std::fs::read_to_string(&stderr).unwrap_or_default()
    );
    let result = match outcome {
        Err(error) => Err(MathError::new(error.to_string())),
        Ok(ProcessOutcome::Cancelled) => Err(MathError::new("Manim の生成を取り消しました")),
        Ok(ProcessOutcome::TimedOut) => {
            Err(MathError::new("Manim の生成が時間内に終わりませんでした"))
        }
        Ok(ProcessOutcome::Exited(0)) => match find_output(&media) {
            Some(path) => std::fs::rename(&path, &video)
                .or_else(|_| std::fs::copy(&path, &video).map(|_| ()))
                .map_err(|error| MathError::new(format!("生成した動画を置けません: {error}"))),
            None => Err(MathError::new("Manim が動画を出力しませんでした")),
        },
        Ok(ProcessOutcome::Exited(code)) => {
            let reason = log
                .lines()
                .rev()
                .find(|line| line.contains("Error"))
                .unwrap_or("")
                .trim()
                .to_owned();
            Err(MathError::new(format!(
                "Manim が終了コード {code} で失敗しました {reason}"
            )))
        }
    };
    let _ = std::fs::remove_dir_all(&job);
    result.map(|()| ManimClipOutput {
        video,
        fingerprint,
        log,
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn scene_names_are_checked_before_running_anything() {
        let request = ManimClipRequest {
            manim: PathBuf::from("C:\\mvm-surely-missing\\manim.exe"),
            script: PathBuf::from("C:\\mvm-surely-missing\\scene.py"),
            scene: "Bad Name".to_owned(),
            output_directory: std::env::temp_dir(),
            width: 1920,
            height: 1080,
            fps: Rational::new(60, 1),
        };
        let error = render_manim_clip(&request, &AtomicBool::new(false)).unwrap_err();
        assert!(error.message().contains("Scene 名"), "{error}");
        let missing = ManimClipRequest {
            scene: "Good".to_owned(),
            ..request
        };
        let error = render_manim_clip(&missing, &AtomicBool::new(false)).unwrap_err();
        assert!(error.message().contains("読めません"), "{error}");
    }

    #[test]
    fn the_fingerprint_follows_the_script_content() {
        let directory = std::env::temp_dir().join(format!("mvm-manim-fp-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let script = directory.join("scene.py");
        std::fs::write(&script, "a").unwrap();
        // SHA-256("a") は既知の値。
        assert_eq!(
            script_fingerprint(&script).unwrap(),
            "ca978112ca1bbdcafac231b39a23dc4da786eff8147c4e72b9807785afee48bb"
        );
        std::fs::write(&script, "b").unwrap();
        assert_ne!(
            script_fingerprint(&script).unwrap(),
            "ca978112ca1bbdcafac231b39a23dc4da786eff8147c4e72b9807785afee48bb"
        );
        std::fs::remove_dir_all(&directory).unwrap();
    }

    /// 実 Manim で小さな Scene を mp4 にする。
    #[test]
    #[ignore = "workstation"]
    fn renders_a_scene_to_an_mp4_with_the_real_manim() {
        let directory = std::env::temp_dir().join(format!("mvm-manim-clip-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory).unwrap();
        let script = directory.join("scene.py");
        std::fs::write(
            &script,
            "from manim import *\n\nclass Dot1(Scene):\n    def construct(self):\n        self.add(Dot())\n        self.wait(0.5)\n",
        )
        .unwrap();
        let manim =
            mvm_platform::find_on_path("manim.exe").expect("manim.exe が PATH にありません");
        let output = render_manim_clip(
            &ManimClipRequest {
                manim,
                script,
                scene: "Dot1".to_owned(),
                output_directory: directory.join("cache"),
                width: 320,
                height: 180,
                fps: Rational::new(30, 1),
            },
            &AtomicBool::new(false),
        )
        .unwrap();
        assert!(output.video.is_file());
        let probed = mvm_media::probe_media_file(&output.video).unwrap();
        let mvm_media::ProbedMedia::Video(video) = probed else {
            panic!("動画として読めない");
        };
        assert_eq!((video.width, video.height), (320, 180));
        assert_eq!(video.fps_num, 30 * video.fps_den);
        std::fs::remove_dir_all(&directory).unwrap();
    }
}
