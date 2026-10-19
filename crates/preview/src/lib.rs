//! playhead の frame を合成し、音声があるときは IAudioClock で再生する。
//!
//! 数式・グラフ・Manim は描かない。停止中も再生中も、decode と合成は worker が行う。
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
pub use mix::AudioMixer;
pub use plan::{
    FramePlan, PlannedLayer, PlannedSource, PreviewImage, black_image, output_size, plan_frame,
};
pub use render::StillPreview;
