//! playhead の frame を合成し、音声があるときは IAudioClock で再生する。
//!
//! 数式・グラフ・Manim は描かない。停止中も再生中も、decode と合成は worker が行う。

mod audio_plan;
mod engine;
mod error;
mod geometry;
mod mix;
mod plan;
mod render;

pub use engine::{PreviewDelivery, PreviewEngine};
pub use error::PreviewError;
pub use plan::{
    FramePlan, PlannedLayer, PlannedSource, PreviewImage, black_image, output_size, plan_frame,
};
pub use render::StillPreview;
