//! 素材 frame と output（timeline）frame の対応。Project・preview・書き出しの全てがこれを使う。
//!
//! 位置は「素材の 0 frame を原点にした output frame 位置」p で数える。R = output fps / 素材 fps とすると:
//!
//! - 境界: 素材境界 s の output 位置 = ceil(s R)（clip の尺と cut の範囲）
//! - 表示: output 位置 p が表示する素材 frame = floor(p / R + 1/2)
//!
//! 表示の四捨五入は C++ 版が書き出しに使っていた MLT の producer と同じ（C++ 版
//! docs/premiere-like-editing.md §16.7 で実測）。ちょうど 1/2 の境界は有理数で厳密に計算する。

use crate::rational::{Rational, div_ceil, div_floor, gcd};

/// R = output / source を約分した比（num / den）。
struct Ratio {
    num: i128,
    den: i128,
}

fn output_per_source(source: Rational, output: Rational) -> Option<Ratio> {
    if !source.is_positive() || !output.is_positive() {
        return None;
    }
    // 64 bit の因子ごとに約分してから掛ける。64 bit どうしの積なので 128 bit に収まる。
    let a = gcd(output.num, source.num);
    let b = gcd(source.den, output.den);
    Some(Ratio {
        num: i128::from(output.num / a) * i128::from(source.den / b),
        den: i128::from(output.den / b) * i128::from(source.num / a),
    })
}

/// 素材境界の output 位置。`round_up` なら ceil(s R)、そうでなければ floor(s R)。
/// `from` / `to` を入れ替えれば output 境界 → 素材境界にも使える。
pub fn convert_frame_boundary(
    frame: i64,
    from: Rational,
    to: Rational,
    round_up: bool,
) -> Option<i64> {
    if frame < 0 {
        return None;
    }
    let ratio = output_per_source(from, to)?;
    let numerator = i128::from(frame).checked_mul(ratio.num)?;
    let value = if round_up {
        div_ceil(numerator, ratio.den)
    } else {
        div_floor(numerator, ratio.den)
    };
    i64::try_from(value).ok()
}

/// output 位置 p（0 以上）が表示する素材 frame floor(p / R + 1/2)。
pub fn source_frame_at_output_position(
    output_position: i64,
    source: Rational,
    output: Rational,
) -> Option<i64> {
    if output_position < 0 {
        return None;
    }
    // floor(p / R + 1/2) = floor((2 p R.den + R.num) / (2 R.num))
    let ratio = output_per_source(source, output)?;
    let numerator = (2 * i128::from(output_position))
        .checked_mul(ratio.den)?
        .checked_add(ratio.num)?;
    let denominator = ratio.num.checked_mul(2)?;
    i64::try_from(div_floor(numerator, denominator)).ok()
}

/// 素材 frame s を表示する最初の output 位置 ceil((s - 1/2) R)。0 未満は 0 にする。
/// [`source_frame_at_output_position`] の逆で、s の表示区間は [first(s), first(s + 1)) になる。
pub fn first_output_position_of_source_frame(
    source_frame: i64,
    source: Rational,
    output: Rational,
) -> Option<i64> {
    if source_frame < 0 {
        return None;
    }
    // ceil((s - 1/2) R) = ceil((2 s - 1) R.num / (2 R.den))
    let ratio = output_per_source(source, output)?;
    let numerator = (2 * i128::from(source_frame) - 1).checked_mul(ratio.num)?;
    let denominator = ratio.den.checked_mul(2)?;
    i64::try_from(div_ceil(numerator, denominator).max(0)).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rate(num: i64, den: i64) -> Rational {
        Rational::new(num, den)
    }

    // 期待値は C++ 版で MLT を実行して得た対応を手で書き写したもの。実装の式から作らない。
    #[test]
    fn displayed_frame_matches_mlt() {
        // 25fps 素材を 60fps へ置く。p = 6 は 2.5 で、上へ丸める。
        let pal = [0, 0, 1, 1, 2, 2, 3, 3, 3, 4, 4, 5, 5];
        for (position, expected) in (0..).zip(pal) {
            assert_eq!(
                source_frame_at_output_position(position, rate(25, 1), rate(60, 1)),
                Some(expected),
                "25fps 素材の 60fps 位置 {position}"
            );
        }
        // 50fps 素材を 60fps へ置き、位置 4..9 を出すと MLT は 3..8 を出した。
        for (position, expected) in (4..).zip([3, 4, 5, 6, 7, 8]) {
            assert_eq!(
                source_frame_at_output_position(position, rate(50, 1), rate(60, 1)),
                Some(expected),
                "50fps 素材の 60fps 位置 {position}"
            );
        }
        for position in 0..10 {
            assert_eq!(
                source_frame_at_output_position(position, rate(30000, 1001), rate(30000, 1001)),
                Some(position)
            );
        }
    }

    #[test]
    fn rounds_boundaries() {
        // 30 * 60 * 1001 / 30000 = 60.06
        assert_eq!(
            convert_frame_boundary(30, rate(30000, 1001), rate(60, 1), true),
            Some(61)
        );
        assert_eq!(
            convert_frame_boundary(30, rate(30000, 1001), rate(60, 1), false),
            Some(60)
        );
        assert_eq!(
            convert_frame_boundary(61, rate(60, 1), rate(30000, 1001), false),
            Some(30)
        );
        assert_eq!(
            convert_frame_boundary(-1, rate(60, 1), rate(60, 1), true),
            None
        );
        assert_eq!(
            convert_frame_boundary(1, rate(0, 1), rate(60, 1), true),
            None
        );
    }

    // 2 つの関数を互いに照合するだけで、どちらの式も期待値の計算には使わない。
    #[test]
    fn inverse_is_consistent() {
        let pairs = [
            (rate(25, 1), rate(60, 1)),
            (rate(50, 1), rate(60, 1)),
            (rate(24000, 1001), rate(60, 1)),
            (rate(120, 1), rate(60, 1)),
            (rate(30, 1), rate(60, 1)),
            (rate(60, 1), rate(30000, 1001)),
            (rate(3000, 73), rate(60, 1)),
        ];
        for (source, output) in pairs {
            for position in 0..500 {
                let frame = source_frame_at_output_position(position, source, output).unwrap();
                let begin = first_output_position_of_source_frame(frame, source, output).unwrap();
                let end = first_output_position_of_source_frame(frame + 1, source, output).unwrap();
                assert!(
                    begin <= position && position < end,
                    "{source:?} p={position}"
                );
            }
        }
        assert_eq!(
            first_output_position_of_source_frame(0, rate(25, 1), rate(60, 1)),
            Some(0)
        );
    }

    #[test]
    fn rejects_overflow() {
        let max = i64::MAX;
        assert_eq!(
            source_frame_at_output_position(max, rate(max, 1), rate(1, max)),
            None
        );
        assert_eq!(
            first_output_position_of_source_frame(max, rate(1, max), rate(max, 1)),
            None
        );
        assert_eq!(
            convert_frame_boundary(max, rate(1, max), rate(max, 1), true),
            None
        );
    }
}
