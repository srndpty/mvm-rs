//! YCbCr から RGB への係数。shader はここの値を uniform で受け取る。
//!
//! 行列は ITU-R BT.601 / BT.709 / BT.2020 の非恒定輝度。レンジは BT.2100 Table 9。
//! P010 は 10bit を 16bit の上位に置くので、unorm の sample から 10bit 符号へ戻してから展開する。

use mvm_core::{YuvMatrix, YuvRange};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum YuvDepth {
    Bit8,
    Bit10,
}

/// `yuv_mul` は `(R の Cr, G の Cb, G の Cr, B の Cb)`。
/// `yuv_range` は `(Y scale, Y bias, C scale, C bias)`。`E = sample * scale + bias`。
pub(crate) fn coeffs(matrix: YuvMatrix, range: YuvRange, depth: YuvDepth) -> ([f32; 4], [f32; 4]) {
    (mul_coeffs(matrix), range_coeffs(range, depth))
}

#[cfg(test)]
fn apply_yuv(
    y: f32,
    u: f32,
    v: f32,
    matrix: YuvMatrix,
    range: YuvRange,
    depth: YuvDepth,
) -> [f32; 3] {
    let (mul, range) = coeffs(matrix, range, depth);
    let yy = y * range[0] + range[1];
    let cb = u * range[2] + range[3];
    let cr = v * range[2] + range[3];
    let red = yy + mul[0] * cr;
    let green = yy - mul[1] * cb - mul[2] * cr;
    let blue = yy + mul[3] * cb;
    [
        red.clamp(0.0, 1.0),
        green.clamp(0.0, 1.0),
        blue.clamp(0.0, 1.0),
    ]
}

fn mul_coeffs(matrix: YuvMatrix) -> [f32; 4] {
    let (kr, kb) = match matrix {
        YuvMatrix::Bt709 => (0.2126, 0.0722),
        YuvMatrix::Bt601 => (0.299, 0.114),
        YuvMatrix::Bt2020 => (0.2627, 0.0593),
    };
    let kg = 1.0 - kr - kb;
    [
        2.0 * (1.0 - kr),
        2.0 * kb * (1.0 - kb) / kg,
        2.0 * kr * (1.0 - kr) / kg,
        2.0 * (1.0 - kb),
    ]
}

fn range_coeffs(range: YuvRange, depth: YuvDepth) -> [f32; 4] {
    match (depth, range) {
        (YuvDepth::Bit8, YuvRange::Limited) => {
            [255.0 / 219.0, -16.0 / 219.0, 255.0 / 224.0, -128.0 / 224.0]
        }
        (YuvDepth::Bit8, YuvRange::Full) => [1.0, 0.0, 1.0, -128.0 / 255.0],
        (YuvDepth::Bit10, YuvRange::Limited) => [
            65535.0 / (64.0 * 876.0),
            -64.0 / 876.0,
            65535.0 / (64.0 * 896.0),
            -512.0 / 896.0,
        ],
        (YuvDepth::Bit10, YuvRange::Full) => [
            65535.0 / (64.0 * 1023.0),
            0.0,
            65535.0 / (64.0 * 1023.0),
            -512.0 / 1023.0,
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reference(
        matrix: YuvMatrix,
        range: YuvRange,
        bits: u32,
        y: f64,
        cb: f64,
        cr: f64,
    ) -> [f64; 3] {
        let (kr, kb) = match matrix {
            YuvMatrix::Bt709 => (0.2126, 0.0722),
            YuvMatrix::Bt601 => (0.299, 0.114),
            YuvMatrix::Bt2020 => (0.2627, 0.0593),
        };
        let kg = 1.0 - kr - kb;
        let (yy, cb, cr) = match (bits, range) {
            (8, YuvRange::Limited) => (
                (y - 16.0) / 219.0,
                (cb - 128.0) / 224.0,
                (cr - 128.0) / 224.0,
            ),
            (8, YuvRange::Full) => (y / 255.0, (cb - 128.0) / 255.0, (cr - 128.0) / 255.0),
            (10, YuvRange::Limited) => (
                (y - 64.0) / 876.0,
                (cb - 512.0) / 896.0,
                (cr - 512.0) / 896.0,
            ),
            (10, YuvRange::Full) => (y / 1023.0, (cb - 512.0) / 1023.0, (cr - 512.0) / 1023.0),
            _ => return [f64::NAN, f64::NAN, f64::NAN],
        };
        let red = yy + 2.0 * (1.0 - kr) * cr;
        let green = yy - 2.0 * kb * (1.0 - kb) / kg * cb - 2.0 * kr * (1.0 - kr) / kg * cr;
        let blue = yy + 2.0 * (1.0 - kb) * cb;
        [
            red.clamp(0.0, 1.0),
            green.clamp(0.0, 1.0),
            blue.clamp(0.0, 1.0),
        ]
    }

    fn assert_rgb(actual: [f32; 3], expected: [f64; 3]) {
        for (actual, expected) in actual.iter().zip(expected) {
            let delta = (f64::from(*actual) - expected).abs();
            assert!(
                delta < 1.0e-4,
                "{actual} vs {expected} (delta {delta}), rgb {actual:?}"
            );
        }
    }

    fn sample8(code: f64) -> f32 {
        (code / 255.0) as f32
    }

    fn sample10(code: f64) -> f32 {
        let stored = (code as u16) << 6;
        f32::from(stored) / 65535.0
    }

    #[test]
    fn matrices_match_the_itu_formula_and_differ_on_a_saturated_color() {
        let y = 81.0;
        let cb = 90.0;
        let cr = 240.0;
        let mut greens = Vec::new();
        for matrix in [YuvMatrix::Bt601, YuvMatrix::Bt709, YuvMatrix::Bt2020] {
            let actual = apply_yuv(
                sample8(y),
                sample8(cb),
                sample8(cr),
                matrix,
                YuvRange::Limited,
                YuvDepth::Bit8,
            );
            let expected = reference(matrix, YuvRange::Limited, 8, y, cb, cr);
            assert_rgb(actual, expected);
            greens.push(actual[1]);
        }
        assert!((greens[0] - greens[1]).abs() > 0.05);
        assert!((greens[1] - greens[2]).abs() > 0.01);
    }

    #[test]
    fn limited_ends_are_black_and_white_and_full_range_is_not() {
        let limited_black = apply_yuv(
            sample8(16.0),
            sample8(128.0),
            sample8(128.0),
            YuvMatrix::Bt709,
            YuvRange::Limited,
            YuvDepth::Bit8,
        );
        let limited_white = apply_yuv(
            sample8(235.0),
            sample8(128.0),
            sample8(128.0),
            YuvMatrix::Bt709,
            YuvRange::Limited,
            YuvDepth::Bit8,
        );
        assert_rgb(limited_black, [0.0, 0.0, 0.0]);
        assert_rgb(limited_white, [1.0, 1.0, 1.0]);
        let full = apply_yuv(
            sample8(16.0),
            sample8(128.0),
            sample8(128.0),
            YuvMatrix::Bt709,
            YuvRange::Full,
            YuvDepth::Bit8,
        );
        assert_rgb(
            full,
            reference(YuvMatrix::Bt709, YuvRange::Full, 8, 16.0, 128.0, 128.0),
        );
        assert!(full[0] > 0.05);
    }

    #[test]
    fn p010_uses_10bit_codes_stored_in_the_high_bits() {
        let black = apply_yuv(
            sample10(64.0),
            sample10(512.0),
            sample10(512.0),
            YuvMatrix::Bt709,
            YuvRange::Limited,
            YuvDepth::Bit10,
        );
        let white = apply_yuv(
            sample10(940.0),
            sample10(512.0),
            sample10(512.0),
            YuvMatrix::Bt709,
            YuvRange::Limited,
            YuvDepth::Bit10,
        );
        assert_rgb(black, [0.0, 0.0, 0.0]);
        assert_rgb(white, [1.0, 1.0, 1.0]);
        let actual = apply_yuv(
            sample10(324.0),
            sample10(360.0),
            sample10(960.0),
            YuvMatrix::Bt601,
            YuvRange::Limited,
            YuvDepth::Bit10,
        );
        assert_rgb(
            actual,
            reference(YuvMatrix::Bt601, YuvRange::Limited, 10, 324.0, 360.0, 960.0),
        );
        let full = apply_yuv(
            sample10(0.0),
            sample10(512.0),
            sample10(512.0),
            YuvMatrix::Bt2020,
            YuvRange::Full,
            YuvDepth::Bit10,
        );
        assert_rgb(
            full,
            reference(YuvMatrix::Bt2020, YuvRange::Full, 10, 0.0, 512.0, 512.0),
        );
    }
}
