//! 数式の描画の service。worker は 1 本で、要求された描画を優先順に cache から読むか Manim で描く。
//!
//! - UI は要る描画の並びを [`MathService::request`] で丸ごと渡す。要らなくなった描画は process ごと止める。
//! - 最初に描画環境を確かめる（[`crate::manim::preflight`]）。環境が無ければ全て「利用不可」になる。
//! - 失敗は覚えて描き直さない。[`MathService::retry`] で環境を確かめ直し、失敗を忘れる
//!   （disk の成功した結果は使い続ける）。
//! - 描けた artifact は immutable な [`MathLibrary`] の snapshot で渡す。
//! - `Drop` で取消を頼み、worker の終了を待つ。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread::JoinHandle;

use crate::artifact::{MathArtifact, load_artifact, store_artifact};
use crate::error::MathError;
use crate::job::MathJob;
use crate::library::MathLibrary;
use crate::manim::{RenderFailure, Toolchain, preflight, render};

pub type Wake = Arc<dyn Fn() + Send + Sync>;

/// 1 つの描画の状態。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MathStatus {
    /// 描画環境を確かめている。
    Checking,
    /// 描画環境が無い（理由）。
    Unavailable(String),
    Pending,
    Rendering,
    Ready,
    /// 式（TeX）の誤り。
    InvalidSource {
        message: String,
        log: String,
    },
    Failed {
        message: String,
        log: String,
    },
}

enum Toolchains {
    Checking,
    Available(Toolchain),
    Unavailable(String),
}

struct State {
    toolchain: Toolchains,
    wanted: Vec<MathJob>,
    failed: HashMap<MathJob, MathStatus>,
    rendering: Option<(MathJob, Arc<AtomicBool>)>,
    library: Arc<MathLibrary>,
    artifacts: HashMap<MathJob, Arc<MathArtifact>>,
    retry: bool,
    shutdown: bool,
    revision: u64,
}

struct Shared {
    state: Mutex<State>,
    changed: Condvar,
    wake: Mutex<Option<Wake>>,
    stop: Arc<AtomicBool>,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }

    fn notify(&self) {
        self.changed.notify_all();
        let wake = self
            .wake
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .clone();
        if let Some(wake) = wake {
            wake();
        }
    }
}

pub struct MathService {
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
}

enum Task {
    Preflight,
    Render(Box<(MathJob, Arc<AtomicBool>, Toolchain)>),
    Exit,
}

impl MathService {
    /// `cache` に artifact を置く。`manim` が None なら PATH から探す。
    pub fn start(cache: PathBuf, manim: Option<PathBuf>) -> Result<Self, MathError> {
        let shared = Arc::new(Shared {
            state: Mutex::new(State {
                toolchain: Toolchains::Checking,
                wanted: Vec::new(),
                failed: HashMap::new(),
                rendering: None,
                library: Arc::new(MathLibrary::default()),
                artifacts: HashMap::new(),
                retry: false,
                shutdown: false,
                revision: 1,
            }),
            changed: Condvar::new(),
            wake: Mutex::new(None),
            stop: Arc::new(AtomicBool::new(false)),
        });
        let worker = Arc::clone(&shared);
        let thread = std::thread::Builder::new()
            .name("mvm-math".into())
            .spawn(move || run(&worker, &cache, manim.as_deref()))
            .map_err(|error| MathError::new(format!("数式の描画を起動できません: {error}")))?;
        Ok(Self {
            shared,
            thread: Some(thread),
        })
    }

    pub fn set_wake(&self, wake: Wake) {
        *self
            .shared
            .wake
            .lock()
            .unwrap_or_else(|poison| poison.into_inner()) = Some(wake);
    }

    /// 要る描画を優先順に渡す。前の要求を置き換える。要らなくなった描画は止め、memory からも外す。
    pub fn request(&self, jobs: Vec<MathJob>) {
        let mut state = self.shared.lock();
        if state.wanted == jobs {
            return;
        }
        if let Some((job, cancel)) = &state.rendering
            && !jobs.contains(job)
        {
            cancel.store(true, Ordering::Release);
        }
        let before = state.artifacts.len();
        state.artifacts.retain(|job, _| jobs.contains(job));
        if state.artifacts.len() != before {
            state.library = Arc::new(MathLibrary::new(state.artifacts.clone()));
            state.revision += 1;
        }
        state.wanted = jobs;
        drop(state);
        self.shared.notify();
    }

    /// 描画環境を確かめ直し、覚えている失敗を忘れて描き直す。
    pub fn retry(&self) {
        let mut state = self.shared.lock();
        state.retry = true;
        state.failed.clear();
        state.revision += 1;
        drop(state);
        self.shared.notify();
    }

    pub fn library(&self) -> Arc<MathLibrary> {
        Arc::clone(&self.shared.lock().library)
    }

    /// 状態が変わるたびに増える。UI はこれで描き直しを決める。
    pub fn revision(&self) -> u64 {
        self.shared.lock().revision
    }

    pub fn status(&self, job: &MathJob) -> MathStatus {
        let state = self.shared.lock();
        match &state.toolchain {
            Toolchains::Checking => return MathStatus::Checking,
            Toolchains::Unavailable(reason) => return MathStatus::Unavailable(reason.clone()),
            Toolchains::Available(_) => {}
        }
        if state.artifacts.contains_key(job) {
            return MathStatus::Ready;
        }
        if let Some(failure) = state.failed.get(job) {
            return failure.clone();
        }
        if state
            .rendering
            .as_ref()
            .is_some_and(|(rendering, _)| rendering == job)
        {
            return MathStatus::Rendering;
        }
        MathStatus::Pending
    }

    /// 描画環境が使えるか（理由付き）。確認中は None。
    pub fn availability(&self) -> Option<Result<(), String>> {
        match &self.shared.lock().toolchain {
            Toolchains::Checking => None,
            Toolchains::Available(_) => Some(Ok(())),
            Toolchains::Unavailable(reason) => Some(Err(reason.clone())),
        }
    }
}

impl Drop for MathService {
    fn drop(&mut self) {
        {
            let mut state = self.shared.lock();
            state.shutdown = true;
            if let Some((_, cancel)) = &state.rendering {
                cancel.store(true, Ordering::Release);
            }
        }
        self.shared.stop.store(true, Ordering::Release);
        self.shared.changed.notify_all();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn next_task(state: &mut State) -> Option<Task> {
    if state.shutdown {
        return Some(Task::Exit);
    }
    if state.retry || matches!(state.toolchain, Toolchains::Checking) {
        state.retry = false;
        state.toolchain = Toolchains::Checking;
        return Some(Task::Preflight);
    }
    let Toolchains::Available(toolchain) = &state.toolchain else {
        return None;
    };
    let job = state
        .wanted
        .iter()
        .find(|job| !state.artifacts.contains_key(*job) && !state.failed.contains_key(*job))?
        .clone();
    let cancel = Arc::new(AtomicBool::new(false));
    let toolchain = toolchain.clone();
    state.rendering = Some((job.clone(), Arc::clone(&cancel)));
    state.revision += 1;
    Some(Task::Render(Box::new((job, cancel, toolchain))))
}

fn run(shared: &Shared, cache: &Path, manim: Option<&Path>) {
    let jobs_directory = cache.join("jobs");
    let mut serial = 0u64;
    loop {
        let task = {
            let mut state = shared.lock();
            loop {
                if let Some(task) = next_task(&mut state) {
                    break task;
                }
                state = shared
                    .changed
                    .wait(state)
                    .unwrap_or_else(|poison| poison.into_inner());
            }
        };
        match task {
            Task::Exit => return,
            Task::Preflight => {
                // 前の強制終了などで残った作業 directory を消してから確かめる。
                let _ = std::fs::remove_dir_all(&jobs_directory);
                let result = preflight(manim, &jobs_directory, &shared.stop);
                let mut state = shared.lock();
                state.toolchain = match result {
                    Ok(toolchain) => Toolchains::Available(toolchain),
                    Err(error) => Toolchains::Unavailable(error.to_string()),
                };
                state.revision += 1;
                drop(state);
                shared.notify();
            }
            Task::Render(render_task) => {
                let (job, cancel, toolchain) = *render_task;
                serial += 1;
                let directory = jobs_directory.join(format!("{}-{serial}", std::process::id()));
                let result = produce(&job, &toolchain, cache, &directory, &cancel);
                let _ = std::fs::remove_dir_all(&directory);
                let mut state = shared.lock();
                state.rendering = None;
                match result {
                    Ok(artifact) => {
                        if state.wanted.contains(&job) {
                            state.artifacts.insert(job, Arc::new(artifact));
                            state.library = Arc::new(MathLibrary::new(state.artifacts.clone()));
                        }
                    }
                    Err(RenderFailure::Cancelled) => {}
                    Err(RenderFailure::InvalidSource { message, log }) => {
                        state
                            .failed
                            .insert(job, MathStatus::InvalidSource { message, log });
                    }
                    Err(RenderFailure::Failed { message, log }) => {
                        state
                            .failed
                            .insert(job, MathStatus::Failed { message, log });
                    }
                }
                state.revision += 1;
                drop(state);
                shared.notify();
            }
        }
    }
}

/// cache にあれば読み、無い（壊れている）なら描いて cache へ置く。
fn produce(
    job: &MathJob,
    toolchain: &Toolchain,
    cache: &Path,
    directory: &Path,
    cancel: &AtomicBool,
) -> Result<MathArtifact, RenderFailure> {
    let failed = |error: MathError| RenderFailure::Failed {
        message: error.to_string(),
        log: String::new(),
    };
    let key = job.key(&toolchain.fingerprint).map_err(failed)?;
    let stored = cache.join(&key);
    match load_artifact(&stored, &key) {
        Ok(Some(artifact)) => return Ok(artifact),
        Ok(None) => {}
        Err(_) => {
            // 壊れた結果は消して描き直す。
            let _ = std::fs::remove_dir_all(&stored);
        }
    }
    let output = render(toolchain, job, directory, cancel)?;
    if cancel.load(Ordering::Acquire) {
        return Err(RenderFailure::Cancelled);
    }
    store_artifact(&stored, &key, &output.sequences, &output.sizes).map_err(failed)?;
    match load_artifact(&stored, &key).map_err(failed)? {
        Some(artifact) => Ok(artifact),
        None => Err(failed(MathError::new("書いた数式の cache を読めません"))),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use std::time::{Duration, Instant};

    use super::*;
    use crate::job::TexSpec;

    fn wait_for(
        service: &MathService,
        job: &MathJob,
        done: impl Fn(&MathStatus) -> bool,
    ) -> MathStatus {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let status = service.status(job);
            if done(&status) || Instant::now() > deadline {
                return status;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn a_missing_manim_makes_every_job_unavailable_and_drop_joins() {
        let cache = std::env::temp_dir().join(format!("mvm-math-service-{}", std::process::id()));
        let service = MathService::start(
            cache.clone(),
            Some(PathBuf::from("C:\\mvm-surely-missing\\manim.exe")),
        )
        .unwrap();
        let job = MathJob::Static(TexSpec {
            source: "x".to_owned(),
            font_size: 64,
        });
        service.request(vec![job.clone()]);
        let status = wait_for(&service, &job, |status| {
            !matches!(status, MathStatus::Checking)
        });
        let MathStatus::Unavailable(reason) = status else {
            panic!("利用不可にならない: {status:?}");
        };
        assert!(reason.contains("mvm-surely-missing"), "{reason}");
        assert!(service.library().is_empty());
        drop(service);
        let _ = std::fs::remove_dir_all(&cache);
    }

    /// 実 Manim と MiKTeX で、静止・Write・変形を描き、cache から読み直す。
    #[test]
    #[ignore = "workstation"]
    fn renders_with_the_real_manim_and_reads_the_cache_back() {
        let cache = std::env::temp_dir().join(format!("mvm-math-real-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&cache);
        let still = MathJob::Static(TexSpec {
            source: "x^2 + \\frac{b}{a}x = 0".to_owned(),
            font_size: 96,
        });
        let write = MathJob::Write {
            tex: TexSpec {
                source: "a + b".to_owned(),
                font_size: 96,
            },
            frames: 6,
        };
        let broken = MathJob::Static(TexSpec {
            source: "\\fracc{a}{b}".to_owned(),
            font_size: 96,
        });
        let service = MathService::start(cache.clone(), None).unwrap();
        service.request(vec![still.clone(), write.clone(), broken.clone()]);
        let finished = |status: &MathStatus| {
            matches!(
                status,
                MathStatus::Ready
                    | MathStatus::Failed { .. }
                    | MathStatus::InvalidSource { .. }
                    | MathStatus::Unavailable(_)
            )
        };
        for job in [&still, &write, &broken] {
            let deadline = Instant::now() + Duration::from_secs(240);
            while !finished(&service.status(job)) && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(100));
            }
        }
        assert_eq!(
            service.status(&still),
            MathStatus::Ready,
            "{:?}",
            service.status(&still)
        );
        assert_eq!(service.status(&write), MathStatus::Ready);
        let MathStatus::InvalidSource { message, .. } = service.status(&broken) else {
            panic!("TeX の誤りが分類されない: {:?}", service.status(&broken));
        };
        assert!(message.contains("Undefined control sequence"), "{message}");
        let library = service.library();
        let mask = library
            .get(&still)
            .unwrap()
            .frame("static", 0)
            .unwrap()
            .clone();
        // 四隅は透明、glyph の画素がある。
        assert_eq!(mask.coverage[0], 0);
        assert!(mask.coverage.iter().filter(|value| **value > 128).count() > 100);
        let frames = library.get(&write).unwrap().sequence("write").unwrap();
        assert_eq!(frames.len(), 6);
        let ink = |mask: &Mask| {
            mask.coverage
                .iter()
                .map(|value| u64::from(*value))
                .sum::<u64>()
        };
        assert!(
            ink(&frames[1]) < ink(&frames[5]),
            "Write が進むほど描かれる"
        );
        assert!(
            frames
                .iter()
                .all(|frame| frame.width == mask.width || frame.width > 0)
        );
        drop(service);

        // 2 回目は cache から読む（Manim を起動しても同じ結果だが、ここでは速さで区別しない）。
        let again = MathService::start(cache.clone(), None).unwrap();
        again.request(vec![still.clone()]);
        let status = wait_for(&again, &still, |status| finished(status));
        assert_eq!(status, MathStatus::Ready);
        assert_eq!(
            again
                .library()
                .get(&still)
                .unwrap()
                .frame("static", 0)
                .unwrap()
                .coverage,
            mask.coverage
        );
        drop(again);
        let _ = std::fs::remove_dir_all(&cache);
    }

    /// 実 Manim で変形と Equation Sequence（変形・outline・pulse）を描く。端点は静止の描画と同じ位置に来る。
    #[test]
    #[ignore = "workstation"]
    fn renders_transforms_and_equation_sequences_with_the_real_manim() {
        use crate::job::{
            ActionOperation, EquationActionJob, EquationJob, EquationStateJob,
            EquationTransitionJob, Parity,
        };
        use crate::segments::{match_segments, segment_tex};

        let cache = std::env::temp_dir().join(format!("mvm-math-real2-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&cache);
        let source = "x + x = 2x";
        let target = "x + x + x = 3x";
        let a = segment_tex(source);
        let b = segment_tex(target);
        let matching = match_segments(&a, &b);
        let parity = Parity::of(1920, 1080);
        let transform = MathJob::Transform {
            source: TexSpec {
                source: source.to_owned(),
                font_size: 96,
            },
            target: TexSpec {
                source: target.to_owned(),
                font_size: 96,
            },
            frames: 8,
            pairs: matching
                .pairs
                .iter()
                .map(|pair| (pair.source, pair.target))
                .collect(),
            unmatched_source: matching.unmatched_source.clone(),
            unmatched_target: matching.unmatched_target.clone(),
            source_segments: a.iter().map(|segment| segment.text.clone()).collect(),
            target_segments: b.iter().map(|segment| segment.text.clone()).collect(),
            parity,
        };
        let still = MathJob::Static(TexSpec {
            source: source.to_owned(),
            font_size: 96,
        });
        let equation = MathJob::Equation(EquationJob {
            states: vec![
                EquationStateJob {
                    segments: a.iter().map(|segment| segment.text.clone()).collect(),
                    font_size: 96,
                },
                EquationStateJob {
                    segments: b.iter().map(|segment| segment.text.clone()).collect(),
                    font_size: 96,
                },
            ],
            transitions: vec![EquationTransitionJob {
                frames: 6,
                pairs: matching
                    .pairs
                    .iter()
                    .map(|pair| (pair.source, pair.target))
                    .collect(),
                unmatched_source: matching.unmatched_source.clone(),
                unmatched_target: matching.unmatched_target.clone(),
            }],
            actions: vec![
                EquationActionJob {
                    state: 0,
                    segment: 4,
                    frames: 4,
                    operation: ActionOperation::Outline,
                },
                EquationActionJob {
                    state: 1,
                    segment: 6,
                    frames: 4,
                    operation: ActionOperation::Pulse,
                },
            ],
            parity,
        });
        let service = MathService::start(cache.clone(), None).unwrap();
        service.request(vec![still.clone(), transform.clone(), equation.clone()]);
        for job in [&still, &transform, &equation] {
            let status = {
                let deadline = Instant::now() + Duration::from_secs(300);
                loop {
                    let status = service.status(job);
                    if matches!(
                        status,
                        MathStatus::Ready
                            | MathStatus::Failed { .. }
                            | MathStatus::InvalidSource { .. }
                            | MathStatus::Unavailable(_)
                    ) || Instant::now() > deadline
                    {
                        break status;
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
            };
            assert_eq!(status, MathStatus::Ready, "{job:?}");
        }
        let library = service.library();
        let still_mask = library
            .get(&still)
            .unwrap()
            .frame("static", 0)
            .unwrap()
            .clone();
        let frames = library
            .get(&transform)
            .unwrap()
            .sequence("transform")
            .unwrap();
        assert_eq!(frames.len(), 8);
        // frame 0 は前の式の静止と同じ画素（canvas の中の、静止を中央に置いた位置）。
        let canvas = &frames[0];
        assert_eq!(canvas.width % 2, 0, "出力と偶奇が揃わない");
        let left = (canvas.width - still_mask.width) / 2;
        let top = (canvas.height - still_mask.height) / 2;
        let mut different = 0usize;
        for y in 0..still_mask.height {
            for x in 0..still_mask.width {
                let a = still_mask.coverage[(y * still_mask.width + x) as usize];
                let b = canvas.coverage[((top + y) * canvas.width + left + x) as usize];
                if a.abs_diff(b) > 8 {
                    different += 1;
                }
            }
        }
        let ink = still_mask
            .coverage
            .iter()
            .filter(|value| **value > 0)
            .count();
        assert!(ink > 100);
        assert!(
            different * 50 < ink,
            "端点が静止とずれています（違う画素 {different} / {ink}）"
        );
        let artifact = library.get(&equation).unwrap();
        for name in ["s0", "s1", "t0", "a0", "a0/base", "a1", "a1/base"] {
            assert!(artifact.sequence(name).is_some(), "{name} がありません");
        }
        assert_eq!(artifact.sequence("t0").unwrap().len(), 6);
        assert_eq!(artifact.sequence("a1").unwrap().len(), 4);
        drop(service);
        let _ = std::fs::remove_dir_all(&cache);
    }

    use crate::artifact::Mask;
}
