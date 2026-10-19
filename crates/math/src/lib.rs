//! 数式・Equation Sequence・Graph の描画。
//!
//! - 数式（LaTeX）は Manim と MiKTeX の外部 process で白い mask にし、cache に置く（[`MathService`]）。
//!   Project には描画結果を保存しない。色・背景・配置は mvm が合成で付ける（[`compose`]）。
//! - Graph は mvm が式を標本化して描く。ラベルだけ LaTeX の mask を使う（[`graph`]）。
//! - preview と書き出しは同じ [`MathLibrary`] と [`math_paint`] で frame の絵を決める。

mod artifact;
mod compose;
mod equation;
mod error;
pub mod graph;
mod job;
mod library;
mod manim;
mod manim_clip;
pub mod segments;
mod service;

pub use artifact::{Mask, MathArtifact};
pub use compose::{MathPaint, PaintLayer, compose, placement};
pub use error::MathError;
pub use job::{MathJob, Parity, TexSpec};
pub use library::{
    ACTION_ACCENT, MathLibrary, clip_jobs, math_paint, require_ready, transition_job, wanted_jobs,
};
pub use manim_clip::{ManimClipOutput, ManimClipRequest, render_manim_clip, script_fingerprint};
pub use service::{MathService, MathStatus, Wake};
