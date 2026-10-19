//! OS に触れる file 操作。製品コードの unsafe はこの crate だけに置く（media・audio を除く）。

#[allow(unsafe_code)]
mod atomic_write;
mod error;
#[allow(unsafe_code)]
mod file_id;
#[allow(unsafe_code)]
mod lock;
mod wide;

pub use atomic_write::atomic_write;
pub use error::PlatformError;
pub use file_id::{FileProbe, FileProbeStatus, probe_file};
pub use lock::{ProjectLock, acquire_project_lock};
