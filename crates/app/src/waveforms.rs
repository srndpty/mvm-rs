//! timeline に描く音声波形の cache。素材 file ごとに 1 度だけ背景で decode し、peak を `Arc` で共有する。
//!
//! decode は 1 本ずつ行い、要求された順に進める。読めなかった file は失敗として覚え、同じ session では
//! 読み直さない（毎 frame 失敗を繰り返さない）。

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use mvm_core::waveform_peaks::WaveformPeaks;
use mvm_media::decode_waveform;

use crate::background::BackgroundJob;
use crate::error::AppError;
use crate::session::Session;

enum Entry {
    Pending,
    Ready(Arc<WaveformPeaks>),
    Failed,
}

type Decode = BackgroundJob<Result<WaveformPeaks, AppError>>;

#[derive(Default)]
pub(crate) struct Waveforms {
    entries: HashMap<PathBuf, Entry>,
    queue: VecDeque<PathBuf>,
    job: Option<(PathBuf, Decode)>,
}

impl Waveforms {
    /// 読めていれば peak を返す。まだなら decode を予約する。
    fn get(&mut self, path: &Path) -> Option<Arc<WaveformPeaks>> {
        match self.entries.get(path) {
            Some(Entry::Ready(peaks)) => Some(Arc::clone(peaks)),
            Some(Entry::Pending | Entry::Failed) => None,
            None => {
                self.entries.insert(path.to_path_buf(), Entry::Pending);
                self.queue.push_back(path.to_path_buf());
                None
            }
        }
    }

    /// 終わった decode を受け取り、次を始める。結果が届いたら true（再描画が要る）。
    fn poll(&mut self, wake: Option<Arc<dyn Fn() + Send + Sync>>) -> bool {
        let mut changed = false;
        if let Some((path, job)) = self.job.as_mut()
            && let Some(result) = job.take_finished()
        {
            let entry = match result.and_then(|decoded| decoded) {
                Ok(peaks) => Entry::Ready(Arc::new(peaks)),
                Err(_) => Entry::Failed,
            };
            self.entries.insert(path.clone(), entry);
            self.job = None;
            changed = true;
        }
        if self.job.is_none()
            && let Some(path) = self.queue.pop_front()
        {
            let work_path = path.clone();
            match BackgroundJob::start("waveform", wake, move |control| {
                let cancel = control.cancel_flag();
                decode_waveform(&work_path, Some(&cancel))
                    .map_err(|error| AppError::new(error.to_string()))
            }) {
                Ok(job) => self.job = Some((path, job)),
                Err(_) => {
                    self.entries.insert(path, Entry::Failed);
                    changed = true;
                }
            }
        }
        changed
    }
}

impl Session {
    /// 素材の波形。まだ読めていなければ `None` を返し、背景で読み始める。
    pub fn waveform(&mut self, path: &Path) -> Option<Arc<WaveformPeaks>> {
        if path.as_os_str().is_empty() {
            return None;
        }
        self.waveforms.get(path)
    }

    /// 波形の decode を進める。新しい波形が届いたら true。
    pub fn poll_waveforms(&mut self) -> bool {
        let wake = self.preview_wake.clone();
        self.waveforms.poll(wake)
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[test]
    fn a_missing_file_is_requested_once_and_remembered_as_failed() {
        let mut waveforms = Waveforms::default();
        let path = Path::new("Z:/無い/音.wav");
        assert!(waveforms.get(path).is_none());
        assert!(waveforms.get(path).is_none());
        assert_eq!(waveforms.queue.len(), 1);
        let mut changed = false;
        for _ in 0..500 {
            changed |= waveforms.poll(None);
            if matches!(waveforms.entries.get(path), Some(Entry::Failed)) {
                break;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(changed);
        assert!(matches!(waveforms.entries.get(path), Some(Entry::Failed)));
        // 失敗した file は読み直さない。
        assert!(waveforms.get(path).is_none());
        assert!(waveforms.queue.is_empty());
        assert!(waveforms.job.is_none());
    }
}
