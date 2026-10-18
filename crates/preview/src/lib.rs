//! 停止中の preview。playhead の 1 frame を、映像と静止画の layer として合成する。
//!
//! 文字・数式・グラフ・Manim・字幕は描かない。再生・音声・worker は持たない。

mod error;
mod geometry;
mod plan;
mod render;

pub use error::PreviewError;
pub use plan::{
    FramePlan, PlannedLayer, PlannedSource, PreviewImage, black_image, output_size, plan_frame,
};
pub use render::StillPreview;
