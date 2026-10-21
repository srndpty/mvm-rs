//! 画面の背後にある session。Project の読み書き、playhead、preview の再生を持つ。
//!
//! 編集の判定は project、画素の合成は preview が行う。ここは command と、その結果の状態だけを持つ。

mod audio_adjust;
mod autosave;
mod background;
mod commands;
mod error;
mod export;
mod export_settings;
mod files;
mod history;
mod manim_clip;
mod math;
mod math_commands;
mod media;
mod mixer;
mod preferences;
mod recovery_pump;
mod session;
mod shuttle;
mod subtitles;
mod timeline_commands;
mod timeline_view;
mod transcribe;
mod waveforms;

pub use audio_adjust::{AudioAdjustmentClipReport, AudioAdjustmentOutcome};
pub use error::AppError;
pub use export_settings::{ExportSettingsSummary, export_settings_summary};
pub use math::MathClipStatus;
pub use media::{ImportReport, RemovalPlan};
pub use mvm_export::{ExportError, ExportProgress, ExportQuality, ExportRequest, ExportSummary};
pub use mvm_platform::ReplaceApproval;
pub use mvm_preview::{MeterPeaks, PreviewImage, font_families, font_sample};
pub use mvm_project::audio_adjustment::AudioAdjustmentSettings;
pub use preferences::Preferences;
pub use session::{ExistingFile, RecoveryPrompt, Session, replace_approval};
pub use timeline_commands::VideoSettings;
pub use timeline_view::{
    MAX_PIXELS_PER_FRAME, MIN_PIXELS_PER_FRAME, TimeWindow, clamp_pixels_per_frame,
    display_span_frames, ranges_overlap, ruler_step, visible_window,
};
pub use transcribe::{
    MAXIMUM_PROMPT_CHARS, TranscriptionOutcome, TranscriptionRequest, TranscriptionSource,
};
