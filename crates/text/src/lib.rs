//! 文字の raster。書体は OS に入っているものを名前で引き、無ければ失敗する（別の書体へは置き換えない）。
//!
//! 系統のフォントへ黙って置き換えない。字幕の折り返しと定位置もここで決める。

mod error;
mod raster;

pub use error::TextError;
pub use raster::{
    Align, GlyphRun, SubtitlePaint, block_size, font_families, font_sample, place_subtitle, raster,
    raster_subtitle,
};
