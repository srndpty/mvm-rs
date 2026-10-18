//! mvm の Project model と、その編集・検証・読み書き。OS・GPU・UI に依存しない。
//!
//! 編集は全て「候補を作って全体を検証し、通ったときだけ置き換える」。失敗した編集は Project を変えない。

pub mod audio_adjustment;
pub mod bytes;
pub mod clip_effects;
pub mod equation_sequence;
pub mod error;
pub mod graph_clip;
pub mod json;
pub mod math_clip;
pub mod media_bin;
pub mod model;
pub mod path_identity;
pub mod recovery;
pub mod render;
pub mod subtitles;
pub mod timeline;

pub use error::{ProjectError, ProjectResult};
pub use model::*;
