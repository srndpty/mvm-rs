//! 画面の背後にある session。Project の読み書き、playhead、preview の再生を持つ。
//!
//! 編集の判定は project、画素の合成は preview が行う。ここは command と、その結果の状態だけを持つ。

mod autosave;
mod commands;
mod error;
mod files;
mod history;
mod recovery_pump;
mod session;
mod timeline_view;

pub use error::AppError;
pub use mvm_preview::PreviewImage;
pub use session::{RecoveryPrompt, Session};
pub use timeline_view::{
    MAX_PIXELS_PER_FRAME, MIN_PIXELS_PER_FRAME, TimeWindow, clamp_pixels_per_frame,
    display_span_frames, ranges_overlap, ruler_step, visible_window,
};
