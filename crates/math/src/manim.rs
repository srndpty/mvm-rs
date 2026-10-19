//! Manim（と、Manim が起動する LaTeX・dvisvgm）による描画。外部 process は Job Object の中で走らせ、
//! 取消・timeout では子 process まで止める。
//!
//! 失敗の種類は script が error-kind.txt に書き、TeX の誤りの説明は TeX の log の `!` 行から取る。
//! Manim の console 出力は環境で折り返しが変わるので、判定に使わない。

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use mvm_platform::{ProcessOutcome, ProcessRequest, find_on_path, run_process};
use serde::Deserialize;

use crate::error::MathError;
use crate::job::{MathJob, SCRIPT_VERSION};

const SCRIPT: &str = include_str!("../assets/mvm_math.py");
const SCENE: &str = "MvmMath";
const RENDER_TIMEOUT: Duration = Duration::from_secs(300);
const VERSION_TIMEOUT: Duration = Duration::from_secs(60);
const MAXIMUM_LOG_BYTES: usize = 64 * 1024;
const MAXIMUM_TEX_ERROR_LINES: usize = 3;

/// 確認済みの描画環境。fingerprint は cache key に入る。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Toolchain {
    pub manim: PathBuf,
    pub fingerprint: String,
}

/// 描画の失敗。取消は失敗として覚えない（要求が残っていれば描き直す）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RenderFailure {
    Cancelled,
    /// 式（TeX）の誤り。直すまで描けない。
    InvalidSource {
        message: String,
        log: String,
    },
    Failed {
        message: String,
        log: String,
    },
}

impl RenderFailure {
    fn failed(message: impl Into<String>, log: String) -> Self {
        Self::Failed {
            message: message.into(),
            log,
        }
    }
}

/// 描いた連番。名前ごとに大きさと PNG の並び。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SceneOutput {
    pub sequences: BTreeMap<String, Vec<PathBuf>>,
    pub sizes: BTreeMap<String, (u32, u32)>,
}

fn first_line(text: &str) -> String {
    text.lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("")
        .to_owned()
}

fn tail(text: String) -> String {
    if text.len() <= MAXIMUM_LOG_BYTES {
        return text;
    }
    let mut start = text.len() - MAXIMUM_LOG_BYTES;
    while !text.is_char_boundary(start) {
        start += 1;
    }
    text[start..].to_owned()
}

/// Manim・LaTeX・dvisvgm を確かめ、版から fingerprint を作る。latex・dvisvgm は PATH からだけ探す
/// （Manim へ引き継ぐのと同じ PATH）。
pub fn preflight(
    manim_override: Option<&Path>,
    work: &Path,
    cancel: &AtomicBool,
) -> Result<Toolchain, MathError> {
    let manim = match manim_override.filter(|path| !path.as_os_str().is_empty()) {
        Some(path) if path.is_file() => path.to_path_buf(),
        Some(path) => {
            return Err(MathError::new(format!(
                "Manim の実行 file が見つかりません: {}",
                path.display()
            )));
        }
        None => find_on_path("manim.exe").ok_or_else(|| {
            MathError::new(
                "manim.exe が PATH に見つかりません。Manim Community を導入してから mvm を起動し直してください",
            )
        })?,
    };
    let latex = find_on_path("latex.exe").ok_or_else(|| {
        MathError::new(
            "LaTeX（latex.exe）が PATH に見つかりません。MiKTeX を導入し、PATH に追加してから mvm を起動し直してください",
        )
    })?;
    let dvisvgm = find_on_path("dvisvgm.exe").ok_or_else(|| {
        MathError::new(
            "dvisvgm.exe が PATH に見つかりません。MiKTeX を導入し、PATH に追加してから mvm を起動し直してください",
        )
    })?;
    std::fs::create_dir_all(work)
        .map_err(|error| MathError::new(format!("数式の作業 directory を作れません: {error}")))?;
    let mut fingerprint = format!("backend=manim\nscript={SCRIPT_VERSION}\n");
    for (name, executable) in [("manim", &manim), ("latex", &latex), ("dvisvgm", &dvisvgm)] {
        if cancel.load(Ordering::Relaxed) {
            return Err(MathError::new("描画環境の確認を取り消しました"));
        }
        let stdout = work.join(format!("{name}-version.stdout.txt"));
        let stderr = work.join(format!("{name}-version.stderr.txt"));
        let outcome = run_process(
            &ProcessRequest {
                executable,
                arguments: &[OsStr::new("--version")],
                working_directory: Some(work),
                stdout_path: Some(&stdout),
                stderr_path: Some(&stderr),
                timeout: Some(VERSION_TIMEOUT),
            },
            &mut || cancel.load(Ordering::Relaxed),
        )
        .map_err(|error| MathError::new(error.to_string()))?;
        let output = std::fs::read_to_string(&stdout).unwrap_or_default();
        if outcome != ProcessOutcome::Exited(0) {
            let reason = first_line(&std::fs::read_to_string(&stderr).unwrap_or_default());
            return Err(MathError::new(format!(
                "{name} --version が失敗しました（{}）: {reason}",
                executable.display()
            )));
        }
        // 版は標準出力の最初の行だけから取る。標準エラーには MiKTeX の更新の催促などが混ざる。
        let version = first_line(&output);
        if version.is_empty() {
            return Err(MathError::new(format!(
                "{name} --version が版を出力しませんでした"
            )));
        }
        fingerprint.push_str(&format!("{name}={version}\n"));
    }
    Ok(Toolchain { manim, fingerprint })
}

/// TeX の log の `!` で始まる行（誤りの要約）を集める。
fn tex_error_lines(media: &Path) -> String {
    let Ok(entries) = std::fs::read_dir(media.join("Tex")) else {
        return String::new();
    };
    let mut lines = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension() != Some(OsStr::new("log")) {
            continue;
        }
        let text = std::fs::read(&path)
            .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
            .unwrap_or_default();
        for line in text.lines() {
            if lines.len() >= MAXIMUM_TEX_ERROR_LINES {
                break;
            }
            if let Some(rest) = line.strip_prefix('!') {
                lines.push(rest.trim().to_owned());
            }
        }
    }
    lines.join("\n")
}

#[derive(Deserialize)]
struct ScriptManifest {
    sequences: BTreeMap<String, ScriptSequence>,
}

#[derive(Deserialize)]
struct ScriptSequence {
    width: u32,
    height: u32,
    frames: usize,
}

/// `job` を `directory`（空の作業 directory）で描く。
pub fn render(
    toolchain: &Toolchain,
    job: &MathJob,
    directory: &Path,
    cancel: &AtomicBool,
) -> Result<SceneOutput, RenderFailure> {
    let request = job.request().map_err(|error| RenderFailure::Failed {
        message: error.to_string(),
        log: String::new(),
    })?;
    let failed = |message: String| RenderFailure::failed(message, String::new());
    std::fs::create_dir_all(directory)
        .map_err(|error| failed(format!("数式の作業 directory を作れません: {error}")))?;
    let script = directory.join("mvm_math.py");
    std::fs::write(&script, SCRIPT)
        .and_then(|()| std::fs::write(directory.join("request.json"), request.to_string()))
        .map_err(|error| failed(format!("数式の描画 script を書けません: {error}")))?;
    let media = directory.join("media");
    let stdout = directory.join("stdout.txt");
    let stderr = directory.join("stderr.txt");
    let arguments = [
        OsStr::new("render"),
        OsStr::new("-s"),
        OsStr::new("--format"),
        OsStr::new("png"),
        OsStr::new("--transparent"),
        OsStr::new("--progress_bar"),
        OsStr::new("none"),
        OsStr::new("--media_dir"),
        media.as_os_str(),
        script.as_os_str(),
        OsStr::new(SCENE),
    ];
    let outcome = run_process(
        &ProcessRequest {
            executable: &toolchain.manim,
            arguments: &arguments,
            working_directory: Some(directory),
            stdout_path: Some(&stdout),
            stderr_path: Some(&stderr),
            timeout: Some(RENDER_TIMEOUT),
        },
        &mut || cancel.load(Ordering::Relaxed),
    );
    let log = tail(format!(
        "{}{}",
        std::fs::read_to_string(&stdout).unwrap_or_default(),
        std::fs::read_to_string(&stderr).unwrap_or_default()
    ));
    let outcome = match outcome {
        Ok(outcome) => outcome,
        Err(error) => return Err(RenderFailure::failed(error.to_string(), log)),
    };
    match outcome {
        ProcessOutcome::Cancelled => return Err(RenderFailure::Cancelled),
        ProcessOutcome::TimedOut => {
            return Err(RenderFailure::failed(
                format!(
                    "数式の描画が {} 秒で終わりませんでした",
                    RENDER_TIMEOUT.as_secs()
                ),
                log,
            ));
        }
        ProcessOutcome::Exited(0) => {}
        ProcessOutcome::Exited(code) => {
            let kind =
                std::fs::read_to_string(directory.join("error-kind.txt")).unwrap_or_default();
            let reason = first_line(
                &std::fs::read_to_string(directory.join("error.txt")).unwrap_or_default(),
            );
            if kind.trim() == "latex" {
                let mut message = tex_error_lines(&media);
                if message.is_empty() {
                    message = "LaTeX が式を処理できませんでした".to_owned();
                }
                return Err(RenderFailure::InvalidSource { message, log });
            }
            let message = if reason.is_empty() {
                format!("Manim が終了コード {code} で失敗しました")
            } else {
                reason
            };
            return Err(RenderFailure::failed(message, log));
        }
    }
    let out = directory.join("out");
    let manifest: ScriptManifest = std::fs::read_to_string(out.join("manifest.json"))
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .ok_or_else(|| {
            RenderFailure::failed("Manim が描画の結果を書きませんでした", log.clone())
        })?;
    let expected = job.expected_sequences();
    if expected.len() != manifest.sequences.len() {
        return Err(RenderFailure::failed(
            format!(
                "描いた連番の数が要求と違います（要求 {}、結果 {}）",
                expected.len(),
                manifest.sequences.len()
            ),
            log,
        ));
    }
    let mut output = SceneOutput::default();
    for (name, frames) in expected {
        let Some(entry) = manifest.sequences.get(&name) else {
            return Err(RenderFailure::failed(
                format!("{name} を描いていません"),
                log,
            ));
        };
        // 枚数が違えば進み具合と frame 番号の対応が崩れる。黙って詰めたり補ったりしない。
        if entry.frames != frames || entry.width == 0 || entry.height == 0 {
            return Err(RenderFailure::failed(
                format!(
                    "{name} の枚数または大きさが要求と違います（要求 {frames} 枚、結果 {} 枚）",
                    entry.frames
                ),
                log,
            ));
        }
        let mut paths = Vec::with_capacity(frames);
        for index in 0..frames {
            let path = out.join(format!("{}-{index:05}.png", name.replace('/', "_")));
            match std::fs::metadata(&path) {
                Ok(metadata) if metadata.len() > 0 => paths.push(path),
                _ => {
                    return Err(RenderFailure::failed(
                        format!("{name} の {index} 枚目がありません"),
                        log,
                    ));
                }
            }
        }
        output
            .sizes
            .insert(name.clone(), (entry.width, entry.height));
        output.sequences.insert(name, paths);
    }
    Ok(output)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_override_is_reported_with_its_path() {
        let error = preflight(
            Some(Path::new("C:\\mvm-surely-missing\\manim.exe")),
            &std::env::temp_dir(),
            &AtomicBool::new(false),
        )
        .unwrap_err();
        assert!(error.message().contains("mvm-surely-missing"), "{error}");
    }

    #[test]
    fn log_tail_keeps_the_end_on_a_char_boundary() {
        let text = format!("{}あ", "x".repeat(MAXIMUM_LOG_BYTES));
        let kept = tail(text);
        assert!(kept.ends_with('あ'));
        assert!(kept.len() <= MAXIMUM_LOG_BYTES);
    }

    #[test]
    fn tex_error_lines_are_read_from_the_tex_log() {
        let directory = std::env::temp_dir().join(format!("mvm-texlog-{}", std::process::id()));
        std::fs::create_dir_all(directory.join("Tex")).unwrap();
        std::fs::write(
            directory.join("Tex").join("a.log"),
            "This is pdfTeX\n! Undefined control sequence.\nl.5 \\fracc\n",
        )
        .unwrap();
        assert_eq!(tex_error_lines(&directory), "Undefined control sequence.");
        std::fs::remove_dir_all(&directory).unwrap();
    }
}
