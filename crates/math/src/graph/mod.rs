//! Graph clip の描画。式の compile・標本化・描画は mvm が行う（Manim を使わない）。

pub mod expression;
pub mod render;
pub mod sample;

pub use render::{GraphLayout, LABEL_FONT_EM, graph_label_texts};
