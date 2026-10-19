//! timeline に描く音声波形の cache。素材 file ごとに背景で decode し、peak を `Arc` で共有する。
//!
//! entry は file の実体（volume・file ID・大きさ・更新時刻）と組で持つ。実体が変われば読み直し、読めなかった file
//! も実体が変われば読み直す。decode 中に実体が変わったら、その結果は捨てる（古い波形で新しい entry を上書きしない）。
//! 実体の確認と、timeline から消えた素材の entry の解放は、[`CHECK_INTERVAL_MS`] ごとに行う。

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use mvm_core::waveform_peaks::WaveformPeaks;
use mvm_media::decode_waveform;
use mvm_platform::{FileProbe, probe_file};

use crate::background::BackgroundJob;
use crate::error::AppError;
use crate::session::Session;

/// 素材の実体を確かめる間隔。毎 frame file を開かない。
const CHECK_INTERVAL_MS: u64 = 1_000;

enum State {
    Pending,
    Ready(Arc<WaveformPeaks>),
    Failed,
}

struct Entry {
    identity: FileProbe,
    state: State,
}

type Decode = BackgroundJob<Result<WaveformPeaks, AppError>>;

#[derive(Default)]
pub(crate) struct Waveforms {
    entries: HashMap<PathBuf, Entry>,
    queue: VecDeque<PathBuf>,
    /// decode 中の file と、decode を始めたときの実体。
    job: Option<(PathBuf, FileProbe, Decode)>,
    next_check_ms: u64,
}

impl Waveforms {
    /// 読めていれば peak を返す。まだなら decode を予約する。
    fn get(&mut self, path: &Path) -> Option<Arc<WaveformPeaks>> {
        match self.entries.get(path) {
            Some(Entry {
                state: State::Ready(peaks),
                ..
            }) => Some(Arc::clone(peaks)),
            Some(_) => None,
            None => {
                self.entries.insert(
                    path.to_path_buf(),
                    Entry {
                        identity: probe_file(path),
                        state: State::Pending,
                    },
                );
                self.queue.push_back(path.to_path_buf());
                None
            }
        }
    }

    /// 終わった decode を受け取り、実体の変化を確かめ、次の decode を始める。`in_use` に無い file の entry は
    /// 捨てる。表示が変わり得るときは true（再描画が要る）。
    fn poll(
        &mut self,
        now_ms: u64,
        in_use: &HashSet<PathBuf>,
        wake: Option<Arc<dyn Fn() + Send + Sync>>,
    ) -> bool {
        let mut changed = self.take_finished();
        if now_ms >= self.next_check_ms {
            self.next_check_ms = now_ms.saturating_add(CHECK_INTERVAL_MS);
            changed |= self.check(in_use);
        }
        changed |= self.start_next(wake);
        changed
    }

    fn take_finished(&mut self) -> bool {
        let Some((path, identity, job)) = self.job.as_mut() else {
            return false;
        };
        let Some(result) = job.take_finished() else {
            return false;
        };
        let (path, identity) = (path.clone(), *identity);
        self.job = None;
        // 始めたときと実体が違えば（decode 中に書き換わった・entry を捨てた）結果を捨てる。
        let Some(entry) = self
            .entries
            .get_mut(&path)
            .filter(|entry| entry.identity == identity && matches!(entry.state, State::Pending))
        else {
            return false;
        };
        entry.state = match result.and_then(|decoded| decoded) {
            Ok(peaks) => State::Ready(Arc::new(peaks)),
            Err(_) => State::Failed,
        };
        true
    }

    /// 使われなくなった entry を捨て、実体が変わった file を読み直しに回す。
    fn check(&mut self, in_use: &HashSet<PathBuf>) -> bool {
        let before = self.entries.len();
        self.entries.retain(|path, _| in_use.contains(path));
        self.queue.retain(|path| self.entries.contains_key(path));
        let mut changed = self.entries.len() != before;
        for (path, entry) in &mut self.entries {
            let identity = probe_file(path);
            if identity != entry.identity {
                entry.identity = identity;
                entry.state = State::Pending;
                if !self.queue.contains(path) {
                    self.queue.push_back(path.clone());
                }
                changed = true;
            }
        }
        changed
    }

    fn start_next(&mut self, wake: Option<Arc<dyn Fn() + Send + Sync>>) -> bool {
        if self.job.is_some() {
            return false;
        }
        while let Some(path) = self.queue.pop_front() {
            let Some(entry) = self
                .entries
                .get_mut(&path)
                .filter(|entry| matches!(entry.state, State::Pending))
            else {
                continue;
            };
            let identity = entry.identity;
            let work_path = path.clone();
            match BackgroundJob::start("waveform", wake, move |control| {
                let cancel = control.cancel_flag();
                decode_waveform(&work_path, Some(&cancel))
                    .map_err(|error| AppError::new(error.to_string()))
            }) {
                Ok(job) => {
                    self.job = Some((path, identity, job));
                    return false;
                }
                Err(_) => {
                    entry.state = State::Failed;
                    return true;
                }
            }
        }
        false
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

    /// 波形の decode を進め、素材の書き換えを確かめる。新しい波形が届いたか、捨てたら true。
    pub fn poll_waveforms(&mut self, now_ms: u64) -> bool {
        let in_use = self
            .project
            .timeline_clips
            .iter()
            .map(|clip| clip.media_path.clone())
            .collect();
        let wake = self.preview_wake.clone();
        self.waveforms.poll(now_ms, &in_use, wake)
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    /// 結果が届くまで進める。時計は毎回 1 秒進め、実体の確認も毎回行う。
    fn settle(waveforms: &mut Waveforms, in_use: &HashSet<PathBuf>, clock: &mut u64) {
        for _ in 0..1_000 {
            *clock += CHECK_INTERVAL_MS;
            waveforms.poll(*clock, in_use, None);
            if waveforms.job.is_none() && waveforms.queue.is_empty() {
                return;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        panic!("波形の decode が終わりません");
    }

    fn state(waveforms: &Waveforms, path: &Path) -> &'static str {
        match waveforms.entries.get(path).map(|entry| &entry.state) {
            Some(State::Pending) => "pending",
            Some(State::Ready(_)) => "ready",
            Some(State::Failed) => "failed",
            None => "none",
        }
    }

    fn tone(path: &Path, amplitude: f32) {
        let samples: Vec<f32> = (0..4_800)
            .flat_map(|index| {
                let value = if index % 2 == 0 {
                    amplitude
                } else {
                    -amplitude
                };
                [value, value]
            })
            .collect();
        mvm_media::write_pcm16_wav(path, 48_000, 2, &samples).expect("WAV");
    }

    fn loudness(peaks: &WaveformPeaks) -> f32 {
        mvm_core::waveform_peaks::waveform_column(peaks, 0, 0.0, 0.05)
            .expect("列")
            .maximum
    }

    fn scratch(name: &str) -> PathBuf {
        let directory = std::env::temp_dir().join(format!("mvm-waveform-{}", std::process::id()));
        std::fs::create_dir_all(&directory).expect("作業 directory");
        directory.join(name)
    }

    #[test]
    fn a_rewritten_file_is_decoded_again() {
        let path = scratch("rewritten.wav");
        tone(&path, 0.1);
        let in_use = HashSet::from([path.clone()]);
        let mut waveforms = Waveforms::default();
        let mut clock = 0;
        assert!(waveforms.get(&path).is_none());
        settle(&mut waveforms, &in_use, &mut clock);
        let quiet = waveforms.get(&path).expect("1 回目の波形");
        assert!(loudness(&quiet) < 0.2);
        // 同じ path を別の中身で書き換える（更新時刻と大きさが変わる）。
        std::thread::sleep(Duration::from_millis(20));
        let samples: Vec<f32> = (0..9_600)
            .map(|index| if index % 2 == 0 { 0.9 } else { -0.9 })
            .collect();
        mvm_media::write_pcm16_wav(&path, 48_000, 2, &samples).expect("書き換え");
        settle(&mut waveforms, &in_use, &mut clock);
        let loud = waveforms.get(&path).expect("書き換えた後の波形");
        assert!(loudness(&loud) > 0.8, "古い波形のままです");
    }

    #[test]
    fn a_failed_file_is_retried_after_it_is_repaired() {
        let path = scratch("repaired.wav");
        std::fs::write(&path, b"not audio").expect("壊れた file");
        let in_use = HashSet::from([path.clone()]);
        let mut waveforms = Waveforms::default();
        let mut clock = 0;
        assert!(waveforms.get(&path).is_none());
        settle(&mut waveforms, &in_use, &mut clock);
        assert_eq!(state(&waveforms, &path), "failed");
        // 実体が同じ間は読み直さない。
        settle(&mut waveforms, &in_use, &mut clock);
        assert_eq!(state(&waveforms, &path), "failed");
        std::thread::sleep(Duration::from_millis(20));
        tone(&path, 0.5);
        settle(&mut waveforms, &in_use, &mut clock);
        assert_eq!(state(&waveforms, &path), "ready");
    }

    #[test]
    fn a_result_for_an_older_file_does_not_overwrite_the_entry() {
        let path = scratch("stale.wav");
        tone(&path, 0.1);
        let mut waveforms = Waveforms::default();
        assert!(waveforms.get(&path).is_none());
        // decode を始めた後で、entry の実体を別のものにする（decode 中の書き換えに相当）。
        waveforms.poll(0, &HashSet::from([path.clone()]), None);
        assert!(waveforms.job.is_some());
        if let Some(entry) = waveforms.entries.get_mut(&path) {
            entry.identity.size += 1;
        }
        for _ in 0..1_000 {
            waveforms.take_finished();
            if waveforms.job.is_none() {
                break;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(waveforms.job.is_none());
        assert_eq!(
            state(&waveforms, &path),
            "pending",
            "古い decode の結果を使いました"
        );
    }

    #[test]
    fn entries_of_files_no_longer_on_the_timeline_are_released() {
        let path = scratch("released.wav");
        tone(&path, 0.3);
        let mut waveforms = Waveforms::default();
        let mut clock = 0;
        assert!(waveforms.get(&path).is_none());
        settle(&mut waveforms, &HashSet::from([path.clone()]), &mut clock);
        assert_eq!(state(&waveforms, &path), "ready");
        settle(&mut waveforms, &HashSet::new(), &mut clock);
        assert_eq!(state(&waveforms, &path), "none");
    }
}
