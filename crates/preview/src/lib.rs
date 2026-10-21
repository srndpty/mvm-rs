//! playhead の frame を合成し、音声があるときは IAudioClock で再生する。
//!
//! 数式・Graph は math crate の描画（[`MathLibrary`]）を重ね、まだ描けていない clip は描かない。
//! 停止中も再生中も、decode と合成は worker が行う。
//! 1 frame の計画・合成（[`plan_frame`]・[`StillPreview`]）と音声の mix（[`AudioMixer`]）は
//! 書き出しも使う。preview と書き出しで同じ画素・同じ音にするため、別に実装しない。

mod audio_plan;
mod engine;
mod error;
mod geometry;
mod mix;
mod plan;
mod render;

pub use audio_plan::{AudioSpan, audio_spans, output_timebase, playback_end};
pub use engine::{PreviewDelivery, PreviewEngine};
pub use error::PreviewError;
pub use mix::{AudioMeters, AudioMixer, MeterPeaks, MonitorVolume};
pub use mvm_math::MathLibrary;
pub use plan::{
    FramePlan, GraphSource, PlannedLayer, PlannedSource, PreviewImage, black_image, font_families,
    font_sample, output_size, plan_frame, text_block_size,
};
pub use render::StillPreview;
