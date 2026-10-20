//! Manim clip（利用者が書いた任意の Scene）を mp4 に描く。数式 clip の escape hatch。
//!
//! 出力の寸法と fps は Project に合わせる。生成した動画は `<Project の directory>/cache/manim/` に、
//! 動画の中身の SHA-256 を名前にして置き、公開した file は上書きしない（[`publish_video`]）。

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use mvm_core::Rational;
use mvm_platform::{ProcessOutcome, ProcessRequest, run_process};
use sha2::{Digest, Sha256};

use crate::error::MathError;

const RENDER_TIMEOUT: Duration = Duration::from_secs(30 * 60);
const OUTPUT_NAME: &str = "mvm_manim_output";
static SNAPSHOT_SERIAL: AtomicU64 = AtomicU64::new(1);

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

/// 描いた動画を `directory` に公開する。名前は中身の SHA-256 で、公開した file は上書きしない
/// （同じ中身が既にあればそれを使う）。Project の undo が古い参照へ戻っても、その動画は変わらない。
///
/// 動画は一定の大きさずつ読み、hash を取りながら、この呼び出しだけが持つ一時 file へ写す（全体を memory に
/// 載せない）。写した file を flush して読み直し、hash が一致してから、既存を置き換えない hard link で公開する。
/// 同じ中身を同時に公開した job があれば、公開先の hash を確かめてそれを使う。
pub fn publish_video(rendered: &Path, directory: &Path) -> Result<PathBuf, MathError> {
    let partial = PartialVideo::create(directory)?;
    let (name, size) = partial.copy_from(rendered)?;
    if size == 0 {
        return Err(MathError::new("生成した動画が空です"));
    }
    if file_digest(partial.path())
        .map_err(|error| MathError::new(format!("写した動画を読み直せません: {error}")))?
        != name
    {
        return Err(MathError::new(
            "写した動画の中身が生成した動画と一致しません",
        ));
    }
    let published = directory.join(format!("{name}.mp4"));
    match std::fs::hard_link(partial.path(), &published) {
        Ok(()) => Ok(published),
        // 既にある（前に公開した、または同時に公開した job がある）なら、中身が名前どおりのときだけ使う。
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            match file_digest(&published) {
                Ok(existing) if existing == name => Ok(published),
                Ok(_) => Err(MathError::new(format!(
                    "公開済みの動画の中身が名前と一致しません: {}",
                    published.display()
                ))),
                Err(error) => Err(MathError::new(format!(
                    "公開済みの動画を読めません ({}): {error}",
                    published.display()
                ))),
            }
        }
        Err(error) => Err(MathError::new(format!(
            "生成した動画を公開できません: {error}"
        ))),
    }
}

/// 一度に読む大きさ。
const COPY_CHUNK: usize = 1024 * 1024;

static PARTIAL_SERIAL: AtomicU64 = AtomicU64::new(1);

/// この呼び出しが排他的に作った一時 file。drop で消す（公開した hard link は残る）。
struct PartialVideo {
    path: PathBuf,
    file: std::fs::File,
}

impl PartialVideo {
    fn create(directory: &Path) -> Result<Self, MathError> {
        let mut last = None;
        // 前に落ちた process の残りと名前がぶつかったときだけ次を試す。
        for _ in 0..64 {
            let serial = PARTIAL_SERIAL.fetch_add(1, Ordering::Relaxed);
            let path = directory.join(format!("manim.partial-{}-{serial}", std::process::id()));
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
            {
                Ok(file) => return Ok(Self { path, file }),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    last = Some(error);
                }
                Err(error) => {
                    return Err(MathError::new(format!(
                        "生成した動画の一時 file を作れません: {error}"
                    )));
                }
            }
        }
        Err(MathError::new(format!(
            "生成した動画の一時 file の名前を決められません: {}",
            last.map_or_else(String::new, |error| error.to_string())
        )))
    }

    fn path(&self) -> &Path {
        &self.path
    }

    /// `source` を写し、中身の SHA-256（小文字 16 進）と大きさを返す。写し終えたら媒体へ flush する。
    fn copy_from(&self, source: &Path) -> Result<(String, u64), MathError> {
        use std::io::{Read, Write};
        let mut input = std::fs::File::open(source)
            .map_err(|error| MathError::new(format!("生成した動画を読めません: {error}")))?;
        let mut output = &self.file;
        let mut hasher = Sha256::new();
        let mut buffer = vec![0u8; COPY_CHUNK];
        let mut size = 0u64;
        loop {
            let read = input
                .read(&mut buffer)
                .map_err(|error| MathError::new(format!("生成した動画を読めません: {error}")))?;
            let Some(chunk) = buffer.get(..read) else {
                return Err(MathError::new("生成した動画を読んだ大きさが不正です"));
            };
            if chunk.is_empty() {
                break;
            }
            hasher.update(chunk);
            output
                .write_all(chunk)
                .map_err(|error| MathError::new(format!("生成した動画を書けません: {error}")))?;
            size = size.saturating_add(read as u64);
        }
        self.file
            .sync_all()
            .map_err(|error| MathError::new(format!("生成した動画を flush できません: {error}")))?;
        Ok((hex(&hasher.finalize()), size))
    }
}

impl Drop for PartialVideo {
    fn drop(&mut self) {
        // 自分が作った一時 file だけを消す。公開した hard link は別の名前なので残る。
        let _ = std::fs::remove_file(&self.path);
    }
}

/// file の中身の SHA-256（小文字 16 進）。一定の大きさずつ読む。
fn file_digest(path: &Path) -> std::io::Result<String> {
    use std::io::Read;
    let mut input = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; COPY_CHUNK];
    loop {
        let read = input.read(&mut buffer)?;
        match buffer.get(..read) {
            Some([]) | None => break,
            Some(chunk) => hasher.update(chunk),
        }
    }
    Ok(hex(&hasher.finalize()))
}

/// `request` の Scene を描く。script は描く前に同じ directory の一時 file へ写し、その写しの hash を
/// fingerprint にして Manim にも写しを渡す（hash を取った後に script を書き換えても食い違わない）。
/// 同じ directory に置くので、script の隣の module の import はそのまま動く。ただし import する module や
/// 画像などの依存 file は fingerprint に入らない。
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
    let content = std::fs::read(&request.script).map_err(|error| {
        MathError::new(format!(
            "Manim script を読めません ({}): {error}",
            request.script.display()
        ))
    })?;
    let fingerprint = hex(&Sha256::digest(&content));
    let serial = SNAPSHOT_SERIAL.fetch_add(1, Ordering::Relaxed);
    let script_directory = request
        .script
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_default();
    let snapshot =
        script_directory.join(format!(".mvm-snapshot-{}-{serial}.py", std::process::id()));
    std::fs::create_dir_all(&request.output_directory)
        .map_err(|error| MathError::new(format!("Manim の出力先を作れません: {error}")))?;
    let job = request
        .output_directory
        .join("jobs")
        .join(format!("{}-{serial}", std::process::id()));
    let _ = std::fs::remove_dir_all(&job);
    std::fs::create_dir_all(&job)
        .map_err(|error| MathError::new(format!("Manim の作業 directory を作れません: {error}")))?;
    let result = std::fs::write(&snapshot, &content)
        .map_err(|error| MathError::new(format!("Manim script の写しを書けません: {error}")))
        .and_then(|()| run_scene(request, &snapshot, &job, cancel));
    let _ = std::fs::remove_file(&snapshot);
    let outcome = result.and_then(|(rendered, log)| {
        publish_video(&rendered, &request.output_directory).map(|video| (video, log))
    });
    let _ = std::fs::remove_dir_all(&job);
    outcome.map(|(video, log)| ManimClipOutput {
        video,
        fingerprint,
        log,
    })
}

/// 写しの script を描き、作業 directory の中の動画と log を返す。
fn run_scene(
    request: &ManimClipRequest,
    snapshot: &Path,
    job: &Path,
    cancel: &AtomicBool,
) -> Result<(PathBuf, String), MathError> {
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
        snapshot.as_os_str(),
        OsStr::new(request.scene.as_str()),
    ];
    let stdout = job.join("stdout.txt");
    let stderr = job.join("stderr.txt");
    let outcome = run_process(
        &ProcessRequest {
            executable: &request.manim,
            arguments: &arguments,
            working_directory: snapshot.parent(),
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
    match outcome {
        Err(error) => Err(MathError::new(error.to_string())),
        Ok(ProcessOutcome::Cancelled) => Err(MathError::new("Manim の生成を取り消しました")),
        Ok(ProcessOutcome::TimedOut) => {
            Err(MathError::new("Manim の生成が時間内に終わりませんでした"))
        }
        Ok(ProcessOutcome::Exited(0)) => find_output(&media)
            .map(|path| (path, log))
            .ok_or_else(|| MathError::new("Manim が動画を出力しませんでした")),
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
    }
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
    fn published_videos_are_never_overwritten() {
        let directory = std::env::temp_dir().join(format!("mvm-manim-pub-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory).unwrap();
        let first = directory.join("first.mp4");
        std::fs::write(&first, b"video one").unwrap();
        let published = publish_video(&first, &directory).unwrap();
        // 同じ中身は同じ file を使い、bytes は変わらない。
        assert_eq!(publish_video(&first, &directory).unwrap(), published);
        assert_eq!(std::fs::read(&published).unwrap(), b"video one");
        // 違う中身は別の file になり、前の file はそのまま。
        let second = directory.join("second.mp4");
        std::fs::write(&second, b"video two").unwrap();
        let other = publish_video(&second, &directory).unwrap();
        assert_ne!(other, published);
        assert_eq!(std::fs::read(&published).unwrap(), b"video one");
        assert_eq!(std::fs::read(&other).unwrap(), b"video two");
        // 名前と中身が食い違う既存の file は上書きせずに失敗する。
        std::fs::write(&published, b"tampered").unwrap();
        assert!(publish_video(&first, &directory).is_err());
        assert_eq!(std::fs::read(&published).unwrap(), b"tampered");
        std::fs::remove_dir_all(&directory).unwrap();
    }

    #[test]
    fn concurrent_jobs_publishing_the_same_video_both_get_the_intact_file() {
        let directory = std::env::temp_dir().join(format!("mvm-manim-race-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory).unwrap();
        // 一時 file の書き込み途中を公開すれば中身が欠けるよう、copy の 1 回分より大きくする。
        let content: Vec<u8> = (0..(3 * COPY_CHUNK + 17))
            .map(|index| (index % 251) as u8)
            .collect();
        let expected = hex(&Sha256::digest(&content));
        for round in 0..8 {
            let rendered = directory.join(format!("rendered-{round}.mp4"));
            std::fs::write(&rendered, &content).unwrap();
            let published = directory.join(format!("{expected}.mp4"));
            let _ = std::fs::remove_file(&published);
            let barrier = std::sync::Barrier::new(2);
            let results: Vec<_> = std::thread::scope(|scope| {
                let jobs: Vec<_> = (0..2)
                    .map(|_| {
                        scope.spawn(|| {
                            barrier.wait();
                            publish_video(&rendered, &directory)
                        })
                    })
                    .collect();
                jobs.into_iter().map(|job| job.join().unwrap()).collect()
            });
            for result in results {
                assert_eq!(result.unwrap(), published, "round {round}");
            }
            assert_eq!(std::fs::read(&published).unwrap(), content, "round {round}");
        }
        let leftovers: Vec<_> = std::fs::read_dir(&directory)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|name| name.contains("partial"))
            .collect();
        assert!(
            leftovers.is_empty(),
            "一時 file が残りました: {leftovers:?}"
        );
        std::fs::remove_dir_all(&directory).unwrap();
    }

    #[test]
    fn an_empty_render_is_not_published() {
        let directory =
            std::env::temp_dir().join(format!("mvm-manim-empty-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory).unwrap();
        let rendered = directory.join("empty.mp4");
        std::fs::write(&rendered, b"").unwrap();
        let error = publish_video(&rendered, &directory).unwrap_err();
        assert!(error.message().contains("空"), "{error}");
        let names: Vec<_> = std::fs::read_dir(&directory)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, ["empty.mp4"]);
        std::fs::remove_dir_all(&directory).unwrap();
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

    /// 実 Manim: 生成を始めた直後に script を書き換えても、fingerprint と描いた動画は始めたときの script の
    /// ものになる（0.5 秒の Scene を描き始め、すぐに 2 秒の Scene へ書き換える）。同じ中身の再生成は同じ
    /// file を使い、公開済みの動画は変わらない。
    #[test]
    #[ignore = "workstation"]
    fn the_rendered_video_matches_the_fingerprinted_script_even_if_it_changes() {
        let directory =
            std::env::temp_dir().join(format!("mvm-manim-change-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory).unwrap();
        let script = directory.join("scene.py");
        let short = "from manim import *\n\nclass Hold(Scene):\n    def construct(self):\n        self.add(Dot())\n        self.wait(0.5)\n";
        let long = short.replace("self.wait(0.5)", "self.wait(2)");
        std::fs::write(&script, short).unwrap();
        let request = ManimClipRequest {
            manim: mvm_platform::find_on_path("manim.exe").expect("manim.exe が PATH にありません"),
            script: script.clone(),
            scene: "Hold".to_owned(),
            output_directory: directory.join("cache"),
            width: 320,
            height: 180,
            fps: Rational::new(30, 1),
        };
        let worker = {
            let request = request.clone();
            std::thread::spawn(move || render_manim_clip(&request, &AtomicBool::new(false)))
        };
        std::thread::sleep(std::time::Duration::from_millis(150));
        std::fs::write(&script, &long).unwrap();
        let output = worker.join().unwrap().unwrap();
        assert_eq!(output.fingerprint, hex(&Sha256::digest(short.as_bytes())));
        let frames = |path: &Path| match mvm_media::probe_media_file(path).unwrap() {
            mvm_media::ProbedMedia::Video(video) => video.frame_count,
            _ => panic!("動画として読めない"),
        };
        let short_frames = frames(&output.video);
        assert!(
            (10..=20).contains(&short_frames),
            "0.5 秒の Scene ではない: {short_frames} frame"
        );
        let first_bytes = std::fs::read(&output.video).unwrap();

        // 書き換えた script を描くと別の file になり、前の動画は変わらない。
        let again = render_manim_clip(&request, &AtomicBool::new(false)).unwrap();
        assert_ne!(again.video, output.video);
        assert_eq!(again.fingerprint, hex(&Sha256::digest(long.as_bytes())));
        assert!(frames(&again.video) > short_frames * 2);
        assert_eq!(std::fs::read(&output.video).unwrap(), first_bytes);
        // 写しの script は残らない。
        let leftovers = std::fs::read_dir(&directory)
            .unwrap()
            .flatten()
            .filter(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".mvm-snapshot")
            })
            .count();
        assert_eq!(leftovers, 0);
        std::fs::remove_dir_all(&directory).unwrap();
    }
}
