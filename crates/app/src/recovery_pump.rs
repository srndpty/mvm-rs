//! recovery の書き込みと削除を、積んだ順に 1 つずつ worker で行う。
//!
//! 古い書き込みが後から新しい recovery を上書きしない。Drop は積んだ job を全て終えてから返す。

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread::JoinHandle;

use mvm_platform::atomic_write;
use mvm_project::Project;
use mvm_project::recovery::write_recovery_envelope;

pub(crate) struct RecoveryWrite {
    pub revision: u64,
    pub project: Arc<Project>,
    pub recovery_path: PathBuf,
    pub canonical_path: PathBuf,
    pub canonical_sha256: String,
    pub saved_at: String,
    pub session_id: String,
}

enum Job {
    Write(RecoveryWrite),
    Delete { recovery_path: PathBuf },
}

struct Running {
    revision: u64,
    write: bool,
    handle: JoinHandle<Result<(), String>>,
}

pub struct RecoveryPump {
    queue: VecDeque<Job>,
    running: Option<Running>,
}

pub struct PumpEvent {
    pub revision: u64,
    pub write: bool,
    pub result: Result<(), String>,
}

impl RecoveryPump {
    pub fn new() -> Self {
        Self {
            queue: VecDeque::new(),
            running: None,
        }
    }

    /// 同じ recovery への積んだ書き込みは、新しいもので置き換える。他の file の job は残す。
    pub fn enqueue_write(&mut self, write: RecoveryWrite) {
        let path = write.recovery_path.clone();
        self.queue
            .retain(|job| !(job.is_write() && job.recovery_path() == path.as_path()));
        self.queue.push_back(Job::Write(write));
        self.start();
    }

    /// 同じ recovery への積んだ書き込みと削除は、この削除 1 つにまとめる。他の file の job は残す。
    pub fn enqueue_delete(&mut self, recovery_path: PathBuf) {
        self.queue
            .retain(|job| job.recovery_path() != recovery_path.as_path());
        self.queue.push_back(Job::Delete { recovery_path });
        self.start();
    }

    pub fn poll(&mut self) -> Option<PumpEvent> {
        let finished = self
            .running
            .as_ref()
            .is_some_and(|running| running.handle.is_finished());
        if !finished {
            return None;
        }
        let running = self.running.take()?;
        let result = join(running.handle);
        self.start();
        Some(PumpEvent {
            revision: running.revision,
            write: running.write,
            result,
        })
    }

    fn start(&mut self) {
        if self.running.is_some() {
            return;
        }
        let Some(job) = self.queue.pop_front() else {
            return;
        };
        let (revision, write) = match &job {
            Job::Write(write) => (write.revision, true),
            Job::Delete { .. } => (0, false),
        };
        let handle = std::thread::spawn(move || run(job));
        self.running = Some(Running {
            revision,
            write,
            handle,
        });
    }
}

impl RecoveryPump {
    /// 動いている job と積んだ job を、この thread で全て終える。最初の失敗を返す。
    ///
    /// Project を切り替える前と終了前に呼ぶ。保存後の削除を積んだまま捨てると、次に開いたときに
    /// 古い recovery が出てくる。
    pub fn flush(&mut self) -> Result<(), String> {
        let mut outcome = Ok(());
        if let Some(running) = self.running.take() {
            outcome = join(running.handle);
        }
        while let Some(job) = self.queue.pop_front() {
            let result = run(job);
            if outcome.is_ok() {
                outcome = result;
            }
        }
        outcome
    }
}

impl Default for RecoveryPump {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for RecoveryPump {
    fn drop(&mut self) {
        // 失敗を知らせる先はもう無い。session は切り替えと終了の前に flush して失敗を表示する。
        let _ = self.flush();
    }
}

fn join(handle: JoinHandle<Result<(), String>>) -> Result<(), String> {
    match handle.join() {
        Ok(result) => result,
        Err(_) => Err("自動復旧の書き込みが失敗しました".to_owned()),
    }
}

impl Job {
    fn is_write(&self) -> bool {
        matches!(self, Self::Write(_))
    }

    fn recovery_path(&self) -> &Path {
        match self {
            Self::Write(write) => &write.recovery_path,
            Self::Delete { recovery_path } => recovery_path,
        }
    }
}

fn run(job: Job) -> Result<(), String> {
    match job {
        Job::Write(RecoveryWrite {
            project,
            recovery_path,
            canonical_path,
            canonical_sha256,
            saved_at,
            session_id,
            ..
        }) => {
            let text = write_recovery_envelope(
                &project,
                &canonical_path,
                &canonical_sha256,
                &saved_at,
                &session_id,
            )
            .map_err(|error| error.to_string())?;
            atomic_write(&recovery_path, text.as_bytes()).map_err(|error| error.to_string())
        }
        Job::Delete { recovery_path } => match std::fs::remove_file(&recovery_path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(format!(
                "自動復旧データを削除できません ({}): {error}",
                recovery_path.display()
            )),
        },
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use std::sync::mpsc;

    use super::*;

    fn scratch_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("mvm-pump-{}-{name}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// `release` を drop するまで終わらない job を走らせ、後の job を積んだままにする。
    fn blocked(pump: &mut RecoveryPump) -> mpsc::Sender<()> {
        let (release, wait) = mpsc::channel::<()>();
        pump.running = Some(Running {
            revision: 0,
            write: false,
            handle: std::thread::spawn(move || {
                let _ = wait.recv();
                Ok(())
            }),
        });
        release
    }

    fn write(dir: &Path, name: &str) -> RecoveryWrite {
        let canonical = dir.join(format!("{name}.mvm"));
        RecoveryWrite {
            revision: 1,
            project: Arc::new(Project::default()),
            recovery_path: crate::files::recovery_path(&canonical),
            canonical_path: canonical,
            canonical_sha256: String::new(),
            saved_at: crate::files::utc_timestamp(std::time::SystemTime::now()),
            session_id: "test".to_owned(),
        }
    }

    #[test]
    fn deletes_of_different_files_are_all_kept() {
        let dir = scratch_dir("deletes");
        let a = dir.join("a.mvm.recovery");
        let b = dir.join("b.mvm.recovery");
        std::fs::write(&a, b"a").unwrap();
        std::fs::write(&b, b"b").unwrap();
        let mut pump = RecoveryPump::new();
        let release = blocked(&mut pump);
        pump.enqueue_delete(a.clone());
        pump.enqueue_delete(b.clone());
        pump.enqueue_delete(a.clone());
        let queued: Vec<_> = pump
            .queue
            .iter()
            .map(|job| job.recovery_path().to_path_buf())
            .collect();
        assert_eq!(
            queued,
            [b.clone(), a.clone()],
            "同じ file の削除はまとめ、別の file は残す"
        );
        drop(release);
        pump.flush().unwrap();
        assert!(!a.exists() && !b.exists(), "積んだ削除を落としました");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_later_write_of_another_file_does_not_drop_an_earlier_delete() {
        let dir = scratch_dir("order");
        let first = write(&dir, "a");
        let first_path = first.recovery_path.clone();
        let second = write(&dir, "b");
        let second_path = second.recovery_path.clone();
        let mut pump = RecoveryPump::new();
        let release = blocked(&mut pump);
        pump.enqueue_write(first);
        pump.enqueue_delete(first_path.clone());
        pump.enqueue_write(second);
        drop(release);
        pump.flush().unwrap();
        assert!(!first_path.exists(), "削除した recovery が残りました");
        assert!(second_path.is_file(), "後の書き込みが消えました");

        // 削除の後の同じ file への書き込みは残る（負の対照）。
        let release = blocked(&mut pump);
        pump.enqueue_delete(second_path.clone());
        pump.enqueue_write(write(&dir, "b"));
        drop(release);
        pump.flush().unwrap();
        assert!(second_path.is_file(), "削除の後の書き込みを落としました");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
