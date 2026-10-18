//! 試験専用の補助。製品コードから依存しない。

mod marker;

pub use marker::{
    CELL_COUNT, CELL_SIZE, MARKER_HEIGHT, MARKER_WIDTH, MarkerError, read_frame_marker,
    read_frame_marker_at,
};

use std::path::PathBuf;

/// リポジトリルート。試験素材の場所をここから決める。
pub fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// `dev testmedia` が Smoke 素材を書く directory。
pub fn smoke_dir() -> PathBuf {
    repo_root().join("tests/assets/smoke")
}
