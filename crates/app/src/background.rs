//! UI を止めない 1 本の作業（自動音量調整の解析・文字起こし・Manim の生成）。
//!
//! 作業は自分の入力（Project の snapshot など）を所有し、結果は終わったときに 1 回だけ渡す。
//! `Drop` で取消を頼み、thread の終了を待つ。

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::thread::JoinHandle;

use crate::audio_adjust::AudioAdjustmentOutcome;
use crate::error::AppError;
use crate::manim_clip::{ManimClipOutcome, ManimTarget};
use crate::transcribe::TranscriptionOutcome;

/// session が持つ背景の作業と、その結果のうち UI がまだ確かめていないもの。
#[derive(Default)]
pub(crate) struct BackgroundWork {
    pub audio_adjust: Option<BackgroundJob<Result<AudioAdjustmentOutcome, AppError>>>,
    pub audio_adjust_result: Option<AudioAdjustmentOutcome>,
    pub transcription: Option<BackgroundJob<Result<TranscriptionOutcome, AppError>>>,
    pub transcription_result: Option<TranscriptionOutcome>,
    pub manim: Option<(
        BackgroundJob<Result<ManimClipOutcome, AppError>>,
        ManimTarget,
    )>,
}

/// 作業の側から見た取消と進捗。
#[derive(Clone, Default)]
pub struct JobControl {
    cancel: Arc<AtomicBool>,
    /// 0..=10000（0.01% 単位）。
    progress: Arc<AtomicU32>,
}

impl JobControl {
    pub fn is_cancelled(&self) -> bool {
        self.cancel.load(Ordering::Acquire)
    }

    /// 0..=1 の進み具合。範囲外は丸める。
    pub fn set_progress(&self, fraction: f64) {
        let value = if fraction.is_finite() {
            (fraction.clamp(0.0, 1.0) * 10_000.0).round() as u32
        } else {
            0
        };
        self.progress.store(value, Ordering::Release);
    }

    pub fn progress(&self) -> f32 {
        self.progress.load(Ordering::Acquire) as f32 / 10_000.0
    }

    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Release);
    }

    /// 外部 process の取消に渡す flag。
    pub fn cancel_flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.cancel)
    }
}

pub struct BackgroundJob<T: Send + 'static> {
    control: JobControl,
    handle: Option<JoinHandle<T>>,
}

impl<T: Send + 'static> BackgroundJob<T> {
    /// `work` を名前付きの thread で走らせる。終わったら `wake` で UI を起こす。
    pub fn start(
        name: &str,
        wake: Option<Arc<dyn Fn() + Send + Sync>>,
        work: impl FnOnce(&JobControl) -> T + Send + 'static,
    ) -> Result<Self, AppError> {
        let control = JobControl::default();
        let worker = control.clone();
        let handle = std::thread::Builder::new()
            .name(name.to_owned())
            .spawn(move || {
                let result = work(&worker);
                if let Some(wake) = wake {
                    wake();
                }
                result
            })
            .map_err(|error| AppError::new(format!("{name} を起動できません: {error}")))?;
        Ok(Self {
            control,
            handle: Some(handle),
        })
    }

    pub fn progress(&self) -> f32 {
        self.control.progress()
    }

    pub fn cancel(&self) {
        self.control.cancel();
    }

    /// 終わっていれば結果を返す。作業が panic したら失敗として返す（黙って止まった作業を残さない）。
    pub fn take_finished(&mut self) -> Option<Result<T, AppError>> {
        if !self.handle.as_ref()?.is_finished() {
            return None;
        }
        let handle = self.handle.take()?;
        Some(
            handle
                .join()
                .map_err(|_| AppError::new("作業が異常終了しました")),
        )
    }

    /// 試験用: 作業が終わったか（結果は受け取らない）。
    #[cfg(test)]
    pub fn is_finished(&self) -> bool {
        self.handle.as_ref().is_none_or(JoinHandle::is_finished)
    }

    /// 試験用: 終わるまで待って結果を返す。
    #[cfg(test)]
    pub fn wait(mut self) -> Result<T, AppError> {
        let handle = self
            .handle
            .take()
            .ok_or_else(|| AppError::new("作業はもう終わっています"))?;
        handle
            .join()
            .map_err(|_| AppError::new("作業が異常終了しました"))
    }
}

impl<T: Send + 'static> Drop for BackgroundJob<T> {
    fn drop(&mut self) {
        self.control.cancel();
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[test]
    fn result_is_delivered_once_and_progress_is_clamped() {
        let mut job = BackgroundJob::start("試験", None, |control| {
            control.set_progress(2.0);
            7
        })
        .unwrap();
        let mut result = None;
        for _ in 0..500 {
            if let Some(value) = job.take_finished() {
                result = Some(value);
                break;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(result, Some(Ok(7)));
        assert_eq!(job.progress(), 1.0);
        assert!(job.take_finished().is_none(), "結果を 2 回渡しました");
    }

    #[test]
    fn dropping_cancels_and_joins() {
        let observed = Arc::new(AtomicBool::new(false));
        let seen = Arc::clone(&observed);
        let job = BackgroundJob::start("試験", None, move |control| {
            while !control.is_cancelled() {
                std::thread::sleep(Duration::from_millis(1));
            }
            seen.store(true, Ordering::Release);
        })
        .unwrap();
        drop(job);
        assert!(observed.load(Ordering::Acquire), "取消を見ずに終わりました");
    }

    #[test]
    #[allow(clippy::panic)]
    fn a_panicking_job_is_reported_as_a_failure() {
        let job = BackgroundJob::start("試験", None, |_| -> i32 {
            panic!("試験の異常終了")
        })
        .unwrap();
        assert!(job.wait().is_err());
    }
}
