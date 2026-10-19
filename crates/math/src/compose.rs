//! mask を出力の raster の中央（余りは左上寄せ）に置き、色を付ける。preview と書き出しが同じ関数を通す。
//! 位置・拡大・回転は ClipEffects が後段で掛けるので、ここでは常に中央へ置く。

use std::sync::Arc;

use crate::artifact::Mask;
use crate::error::MathError;

#[derive(Clone, Debug, PartialEq)]
pub struct PaintLayer {
    pub mask: Arc<Mask>,
    /// 0xAARRGGBB。
    pub color: u32,
}

/// 1 frame の数式。下の layer から順に重ねる。背景は最初の layer の矩形を塗る。
#[derive(Clone, Debug, PartialEq)]
pub struct MathPaint {
    pub layers: Vec<PaintLayer>,
    /// 0xAARRGGBB。alpha が 0 なら塗らない。
    pub background: u32,
}

/// mask を出力の中央へ置いたときの左上。mask が出力より大きければ失敗する（はみ出した式を黙って切らない）。
pub fn placement(mask: &Mask, width: u32, height: u32) -> Result<(u32, u32), MathError> {
    if mask.width > width || mask.height > height {
        return Err(MathError::new(format!(
            "数式（{}x{}）が出力（{width}x{height}）より大きいので置けません。文字サイズを下げてください",
            mask.width, mask.height
        )));
    }
    Ok(((width - mask.width) / 2, (height - mask.height) / 2))
}

fn channels(argb: u32) -> [f32; 4] {
    let [a, r, g, b] = argb.to_be_bytes();
    [
        f32::from(r) / 255.0,
        f32::from(g) / 255.0,
        f32::from(b) / 255.0,
        f32::from(a) / 255.0,
    ]
}

/// straight alpha の `pixel` へ、色 `color`・不透明度 `alpha` を source-over で重ねる。
pub(crate) fn over_pixel(pixel: &mut [u8], color: [f32; 4], alpha: f32) {
    if alpha <= 0.0 {
        return;
    }
    let under_alpha = f32::from(pixel[3]) / 255.0;
    let out_alpha = alpha + under_alpha * (1.0 - alpha);
    if out_alpha <= 0.0 {
        return;
    }
    for channel in 0..3 {
        let under = f32::from(pixel[channel]) / 255.0;
        let value = (color[channel] * alpha + under * under_alpha * (1.0 - alpha)) / out_alpha;
        pixel[channel] = (value * 255.0).round().clamp(0.0, 255.0) as u8;
    }
    pixel[3] = (out_alpha * 255.0).round().clamp(0.0, 255.0) as u8;
}

/// 出力の大きさの RGBA（straight alpha、行の隙間なし）。
pub fn compose(paint: &MathPaint, width: u32, height: u32) -> Result<Vec<u8>, MathError> {
    let bytes = (width as usize)
        .checked_mul(height as usize)
        .and_then(|pixels| pixels.checked_mul(4))
        .ok_or_else(|| MathError::new("出力の画素数が overflow しました"))?;
    let mut rgba = vec![0u8; bytes];
    let stride = width as usize * 4;
    if let Some(first) = paint.layers.first() {
        let background = channels(paint.background);
        if background[3] > 0.0 {
            let (left, top) = placement(&first.mask, width, height)?;
            for y in 0..first.mask.height as usize {
                let row = (top as usize + y) * stride;
                for x in 0..first.mask.width as usize {
                    let at = row + (left as usize + x) * 4;
                    over_pixel(&mut rgba[at..at + 4], background, background[3]);
                }
            }
        }
    }
    for layer in &paint.layers {
        let (left, top) = placement(&layer.mask, width, height)?;
        let color = channels(layer.color);
        let mask_width = layer.mask.width as usize;
        for (index, coverage) in layer.mask.coverage.iter().enumerate() {
            if *coverage == 0 {
                continue;
            }
            let x = left as usize + index % mask_width;
            let y = top as usize + index / mask_width;
            let at = y * stride + x * 4;
            over_pixel(
                &mut rgba[at..at + 4],
                color,
                f32::from(*coverage) / 255.0 * color[3],
            );
        }
    }
    Ok(rgba)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    fn mask(width: u32, height: u32, value: u8) -> Arc<Mask> {
        Arc::new(Mask::new(width, height, vec![value; (width * height) as usize]).unwrap())
    }

    fn pixel(rgba: &[u8], width: u32, x: u32, y: u32) -> [u8; 4] {
        let at = ((y * width + x) * 4) as usize;
        [rgba[at], rgba[at + 1], rgba[at + 2], rgba[at + 3]]
    }

    #[test]
    fn centers_with_the_remainder_to_the_top_left_and_colors_the_glyph() {
        // 出力 7x5 の中央へ 2x2 を置くと左上は (2, 1)。
        let paint = MathPaint {
            layers: vec![PaintLayer {
                mask: mask(2, 2, 255),
                color: 0xFF_FF_00_00,
            }],
            background: 0,
        };
        let rgba = compose(&paint, 7, 5).unwrap();
        assert_eq!(pixel(&rgba, 7, 2, 1), [255, 0, 0, 255]);
        assert_eq!(pixel(&rgba, 7, 3, 2), [255, 0, 0, 255]);
        assert_eq!(pixel(&rgba, 7, 1, 1), [0, 0, 0, 0], "矩形の外は透明");
        assert_eq!(pixel(&rgba, 7, 4, 1), [0, 0, 0, 0]);
    }

    #[test]
    fn half_coverage_over_an_opaque_background_blends_in_straight_alpha() {
        let paint = MathPaint {
            layers: vec![PaintLayer {
                mask: mask(1, 1, 128),
                color: 0xFF_FF_FF_FF,
            }],
            background: 0xFF_00_00_00,
        };
        let rgba = compose(&paint, 1, 1).unwrap();
        // 白を約 50% で黒へ重ねると灰色の不透明。
        assert_eq!(pixel(&rgba, 1, 0, 0), [128, 128, 128, 255]);
    }

    #[test]
    fn a_mask_larger_than_the_output_is_refused() {
        let paint = MathPaint {
            layers: vec![PaintLayer {
                mask: mask(10, 2, 255),
                color: 0xFF_FF_FF_FF,
            }],
            background: 0,
        };
        assert!(compose(&paint, 8, 8).is_err());
    }

    #[test]
    fn the_accent_layer_is_drawn_over_the_base() {
        let base = mask(2, 1, 255);
        let accent = Arc::new(Mask::new(2, 1, vec![0, 255]).unwrap());
        let paint = MathPaint {
            layers: vec![
                PaintLayer {
                    mask: base,
                    color: 0xFF_FF_FF_FF,
                },
                PaintLayer {
                    mask: accent,
                    color: 0xFF_FF_FF_00,
                },
            ],
            background: 0,
        };
        let rgba = compose(&paint, 2, 1).unwrap();
        assert_eq!(pixel(&rgba, 2, 0, 0), [255, 255, 255, 255]);
        assert_eq!(pixel(&rgba, 2, 1, 0), [255, 255, 0, 255]);
    }
}
