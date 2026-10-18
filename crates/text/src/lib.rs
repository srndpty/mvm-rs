//! 文字の raster。使うフォントは Meiryo だけで、ファイルが無ければ失敗する。
//!
//! 系統のフォントへ黙って置き換えない。字幕の折り返しと定位置もここで決める。

mod error;
mod raster;

pub use error::TextError;
pub use raster::{Align, GlyphRun, SubtitlePaint, place_subtitle, raster, raster_subtitle};

/// Windows に入っている Meiryo。別の場所は探さない。
pub const MEIRYO_PATH: &str = r"C:\Windows\Fonts\meiryo.ttc";
