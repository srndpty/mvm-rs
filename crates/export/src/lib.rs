//! 書き出し。preview と同じ計画（[`mvm_preview::plan_frame`]）・compositor
//! （[`mvm_preview::StillPreview`]）・mix（[`mvm_preview::AudioMixer`]）で 1 frame ずつ作り、
//! H.264 / AAC の MP4 にする。見て聞いたものと書き出しを食い違わせないため、合成を別に持たない。

mod error;
mod job;
mod plan;
mod run;

pub use error::ExportError;
pub use job::{ExportJob, ExportProgress, ExportStatus};
pub use plan::{ExportPlan, ExportQuality, ExportRequest, plan_export, temporary_path};
pub use run::{ExportControl, ExportSummary, run_export};
