//! 書き出しを worker thread で走らせる。UI thread は状態を読み、取消を頼むだけ。
//!
//! worker は Project の snapshot（`Arc`）と計画を所有する。drop すると取り消して join する。

use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use mvm_core::export_eta::estimate_export_remaining_seconds;
use mvm_project::Project;

use crate::error::ExportError;
use crate::plan::ExportPlan;
use crate::run::{ExportControl, ExportSummary, run_export};

/// UI を起こす間隔の下限。frame ごとに起こすと描画が encode の邪魔になる。
const WAKE_INTERVAL: Duration = Duration::from_millis(100);

type Wake = Arc<dyn Fn() + Send + Sync>;

/// その時点の書き出しの状態。
#[derive(Clone, Debug, PartialEq)]
pub enum ExportStatus {
    Running(ExportProgress),
    Finished(Result<ExportSummary, ExportError>),
}

#[derive(Clone, Debug, PartialEq)]
pub struct ExportProgress {
    pub completed: i64,
    pub total: i64,
    /// 推定に足る観測が無い間は `None`。
    pub remaining_seconds: Option<f64>,
    /// 取消を頼んだ後、worker が止まるまで true。
    pub cancelling: bool,
}

struct Shared {
    cancel: AtomicBool,
    state: Mutex<State>,
    wake: Option<Wake>,
}

struct State {
    completed: i64,
    total: i64,
    /// 最初に進捗が届いた時点。準備（素材を開くなど）の時間を速度に含めない。
    baseline: Option<(i64, Instant)>,
    last_wake: Option<Instant>,
    result: Option<Result<ExportSummary, ExportError>>,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }

    fn wake(&self) {
        if let Some(wake) = &self.wake {
            wake();
        }
    }
}

impl ExportControl for Shared {
    fn progress(&self, completed: i64, total: i64) {
        let now = Instant::now();
        let mut state = self.lock();
        state.completed = completed;
        state.total = total;
        if state.baseline.is_none() {
            state.baseline = Some((completed, now));
        }
        let due = state
            .last_wake
            .is_none_or(|last| now.duration_since(last) >= WAKE_INTERVAL);
        if due {
            state.last_wake = Some(now);
        }
        drop(state);
        if due {
            self.wake();
        }
    }

    fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Acquire)
    }
}

pub struct ExportJob {
    shared: Arc<Shared>,
    handle: Option<JoinHandle<()>>,
    output_path: std::path::PathBuf,
}

impl ExportJob {
    /// worker を起こす。`wake` は状態が変わったときに UI を起こす（egui の `request_repaint`）。
    pub fn start(
        project: Arc<Project>,
        plan: ExportPlan,
        wake: Option<Wake>,
    ) -> Result<Self, ExportError> {
        let shared = Arc::new(Shared {
            cancel: AtomicBool::new(false),
            state: Mutex::new(State {
                completed: 0,
                total: plan.total_frames,
                baseline: None,
                last_wake: None,
                result: None,
            }),
            wake,
        });
        let output_path = plan.output_path.clone();
        let worker = Arc::clone(&shared);
        let handle = std::thread::Builder::new()
            .name("mvm-export".to_owned())
            .spawn(move || {
                let result = std::panic::catch_unwind(AssertUnwindSafe(|| {
                    run_export(&project, &plan, &*worker)
                }))
                // 一時 file は run_export が持つ guard が巻き戻しの間に消す。
                .unwrap_or_else(|_| {
                    Err(ExportError::failed("書き出しの worker が異常終了しました"))
                });
                worker.lock().result = Some(result);
                worker.wake();
            })
            .map_err(|error| {
                ExportError::failed(format!("書き出しの thread を起こせません: {error}"))
            })?;
        Ok(Self {
            shared,
            handle: Some(handle),
            output_path,
        })
    }

    pub fn output_path(&self) -> &std::path::Path {
        &self.output_path
    }

    /// 次の frame の前で止めるよう頼む。止まったら [`ExportStatus::Finished`] になる。
    pub fn cancel(&self) {
        self.shared.cancel.store(true, Ordering::Release);
    }

    pub fn status(&self) -> ExportStatus {
        let state = self.shared.lock();
        if let Some(result) = &state.result {
            return ExportStatus::Finished(result.clone());
        }
        let remaining_seconds = state.baseline.and_then(|(baseline, at)| {
            estimate_export_remaining_seconds(
                baseline,
                state.completed,
                state.total,
                at.elapsed().as_secs_f64(),
            )
        });
        ExportStatus::Running(ExportProgress {
            completed: state.completed,
            total: state.total,
            remaining_seconds,
            cancelling: self.shared.cancel.load(Ordering::Acquire),
        })
    }

    /// 終わるまで待つ。試験と、終了時の片付けに使う。
    pub fn wait(mut self) -> Result<ExportSummary, ExportError> {
        self.join();
        self.shared
            .lock()
            .result
            .clone()
            .unwrap_or_else(|| Err(ExportError::failed("書き出しの結果がありません")))
    }

    fn join(&mut self) {
        if let Some(handle) = self.handle.take() {
            // worker の panic は catch_unwind で結果にしてある。join の失敗はそれ以外の異常。
            if handle.join().is_err() {
                let mut state = self.shared.lock();
                if state.result.is_none() {
                    state.result = Some(Err(ExportError::failed(
                        "書き出しの worker が異常終了しました",
                    )));
                }
            }
        }
    }
}

impl Drop for ExportJob {
    fn drop(&mut self) {
        self.cancel();
        self.join();
    }
}
