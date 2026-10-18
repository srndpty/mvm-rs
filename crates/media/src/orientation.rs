//! EXIF orientation 1..8 を画素へ適用する。定義は EXIF 2.3。

use crate::error::MediaError;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OrientedImage {
    pub width: i32,
    pub height: i32,
    pub rgba: Vec<u8>,
}

pub fn apply_orientation(
    width: i32,
    height: i32,
    rgba: &[u8],
    orientation: i32,
) -> Result<OrientedImage, MediaError> {
    if !(1..=8).contains(&orientation) {
        return Err(MediaError::new(format!(
            "EXIF orientation は 1..8 のいずれかでなければなりません: {orientation}"
        )));
    }
    let pixels = i64::from(width).checked_mul(i64::from(height));
    let bytes = pixels.and_then(|count| count.checked_mul(4));
    if width <= 0 || height <= 0 || bytes != Some(i64::try_from(rgba.len()).unwrap_or(i64::MAX)) {
        return Err(MediaError::new("画像の寸法と画素数が一致しません"));
    }
    if orientation == 1 {
        return Ok(OrientedImage {
            width,
            height,
            rgba: rgba.to_vec(),
        });
    }
    let swaps = orientation >= 5;
    let dst_w = if swaps { height } else { width };
    let dst_h = if swaps { width } else { height };
    let mut output = vec![0u8; rgba.len()];
    for y in 0..dst_h {
        for x in 0..dst_w {
            let (sx, sy) = source_pixel(orientation, width, height, x, y);
            let src = pixel_offset(sx, sy, width)?;
            let dst = pixel_offset(x, y, dst_w)?;
            output[dst..dst + 4].copy_from_slice(&rgba[src..src + 4]);
        }
    }
    Ok(OrientedImage {
        width: dst_w,
        height: dst_h,
        rgba: output,
    })
}

fn source_pixel(orientation: i32, width: i32, height: i32, x: i32, y: i32) -> (i32, i32) {
    match orientation {
        2 => (width - 1 - x, y),
        3 => (width - 1 - x, height - 1 - y),
        4 => (x, height - 1 - y),
        5 => (y, x),
        6 => (y, height - 1 - x),
        7 => (width - 1 - y, height - 1 - x),
        8 => (width - 1 - y, x),
        _ => (x, y),
    }
}

fn pixel_offset(x: i32, y: i32, width: i32) -> Result<usize, MediaError> {
    let row = i64::from(y)
        .checked_mul(i64::from(width))
        .and_then(|pixels| pixels.checked_add(i64::from(x)))
        .and_then(|pixels| pixels.checked_mul(4))
        .ok_or_else(|| MediaError::new("画像の寸法と画素数が一致しません"))?;
    usize::try_from(row).map_err(|_| MediaError::new("画像の寸法と画素数が一致しません"))
}

#[cfg(test)]
#[allow(clippy::panic)]
mod tests {
    use super::*;

    fn pixel(rgba: &[u8], width: i32, x: i32, y: i32) -> [u8; 4] {
        let offset = ((y * width + x) * 4) as usize;
        rgba[offset..offset + 4].try_into().unwrap_or([0; 4])
    }

    #[test]
    fn orientation_6_turns_a_horizontal_pair_clockwise() {
        // 左が赤、右が青。時計回り 90 度で、上が赤、下が青、幅と高さが入れ替わる。
        let rgba = [255, 0, 0, 255, 0, 0, 255, 255];
        let turned = apply_orientation(2, 1, &rgba, 6).unwrap_or_else(|error| panic!("{error}"));
        assert_eq!((turned.width, turned.height), (1, 2));
        assert_eq!(pixel(&turned.rgba, 1, 0, 0), [255, 0, 0, 255]);
        assert_eq!(pixel(&turned.rgba, 1, 0, 1), [0, 0, 255, 255]);
    }

    #[test]
    fn orientation_2_mirrors_horizontally_and_1_keeps_pixels() {
        let rgba = [255, 0, 0, 255, 0, 0, 255, 255];
        let mirrored = apply_orientation(2, 1, &rgba, 2).unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(pixel(&mirrored.rgba, 2, 0, 0), [0, 0, 255, 255]);
        assert_eq!(pixel(&mirrored.rgba, 2, 1, 0), [255, 0, 0, 255]);
        let same = apply_orientation(2, 1, &rgba, 1).unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(same.rgba, rgba);
    }

    #[test]
    fn rejects_an_orientation_outside_1_to_8_and_a_short_buffer() {
        assert!(apply_orientation(1, 1, &[0, 0, 0, 255], 0).is_err());
        assert!(apply_orientation(1, 1, &[0, 0, 0, 255], 9).is_err());
        assert!(apply_orientation(2, 1, &[0, 0, 0, 255], 1).is_err());
    }
}
