//! OS に触れる file 操作。製品コードの unsafe はこの crate だけに置く（media・audio を除く）。

#[allow(unsafe_code)]
mod atomic_write;
mod error;

pub use atomic_write::atomic_write;
pub use error::PlatformError;
