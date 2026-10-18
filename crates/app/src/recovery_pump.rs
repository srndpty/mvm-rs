//! recovery の書き込みと削除を、積んだ順に 1 つずつ worker で行う。
//!
//! 古い書き込みが後から新しい recovery を上書きしない。Drop は積んだ job を全て終えてから返す。

use std::collections::VecDeque;
use std::path::PathBuf;
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

    pub fn enqueue_write(&mut self, write: RecoveryWrite) {
        self.queue.retain(Job::is_delete);
        self.queue.push_back(Job::Write(write));
        self.start();
    }

    pub fn enqueue_delete(&mut self, recovery_path: PathBuf) {
        self.queue.retain(Job::is_delete);
        if !self.queue.iter().any(Job::is_delete) {
            self.queue.push_back(Job::Delete { recovery_path });
        }
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
    fn is_delete(&self) -> bool {
        matches!(self, Self::Delete { .. })
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
