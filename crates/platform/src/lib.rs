//! OS に触れる file 操作と外部 process の実行。製品コードの unsafe はこの crate だけに置く（media・audio を除く）。

#[allow(unsafe_code)]
mod atomic_write;
mod command_line;
mod error;
#[allow(unsafe_code)]
mod file_id;
#[allow(unsafe_code)]
mod lock;
#[allow(unsafe_code)]
mod process;
mod wide;

pub use atomic_write::atomic_write;
pub use command_line::{find_on_path, quote_argument};
pub use error::PlatformError;
pub use file_id::{FileProbe, FileProbeStatus, probe_file};
pub use lock::{ProjectLock, acquire_project_lock};
pub use process::{ProcessOutcome, ProcessRequest, run_process};
