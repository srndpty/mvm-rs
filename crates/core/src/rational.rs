//! 有理数と、128 bit で計算する整数の丸め。fps・速度・時刻を浮動小数点で持たないための共通部品。

/// `num / den` の有理数。fps・速度・正確な時刻に使う。約分済みとは限らない。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Rational {
    pub num: i64,
    pub den: i64,
}

impl Rational {
    pub const fn new(num: i64, den: i64) -> Self {
        Self { num, den }
    }

    /// 分子・分母がともに正か（fps・速度として使えるか）。
    pub const fn is_positive(self) -> bool {
        self.num > 0 && self.den > 0
    }

    /// 正の有理数を約分する。正でなければ `None`。
    pub fn reduced(self) -> Option<Self> {
        if !self.is_positive() {
            return None;
        }
        let divisor = gcd(self.num, self.den);
        Some(Self::new(self.num / divisor, self.den / divisor))
    }

    /// 約分した値どうしで等しいか。正でない値はどれとも等しくない。
    pub fn same_value(self, other: Self) -> bool {
        matches!((self.reduced(), other.reduced()), (Some(left), Some(right)) if left == right)
    }

    /// `self × factor` を約分して返す。速度 s の clip の実効 fps（素材 fps × s）に使う。
    /// どちらかが正でない、または積を i64 で表せなければ `None`。
    pub fn multiply(self, factor: Self) -> Option<Self> {
        if !self.is_positive() || !factor.is_positive() {
            return None;
        }
        // 掛ける前に交差約分して、表せる範囲を広げる。
        let a = gcd(self.num, factor.den);
        let b = gcd(factor.num, self.den);
        let num = (self.num / a).checked_mul(factor.num / b)?;
        let den = (self.den / b).checked_mul(factor.den / a)?;
        Self::new(num, den).reduced()
    }
}

/// 非負の i64 の最大公約数。両方 0 なら 0。
pub fn gcd(left: i64, right: i64) -> i64 {
    let (mut left, mut right) = (left.unsigned_abs(), right.unsigned_abs());
    while right != 0 {
        (left, right) = (right, left % right);
    }
    // 結果が 2^63 になるのは引数が i64::MIN と (0 または i64::MIN) のときだけ。
    // 呼び出し側は正の値だけを渡すので起きないが、型の上では i64::MAX に飽和させる。
    i64::try_from(left).unwrap_or(i64::MAX)
}

/// 数学的な floor 除算（負の値も -∞ 方向へ丸める）。`denominator > 0`。
pub fn div_floor(numerator: i128, denominator: i128) -> i128 {
    numerator.div_euclid(denominator)
}

/// 数学的な ceil 除算（負の値も +∞ 方向へ丸める）。`denominator > 0`。
pub fn div_ceil(numerator: i128, denominator: i128) -> i128 {
    -(-numerator).div_euclid(denominator)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reduces_positive_rationals_only() {
        assert_eq!(Rational::new(120, 2).reduced(), Some(Rational::new(60, 1)));
        assert_eq!(Rational::new(0, 1).reduced(), None);
        assert_eq!(Rational::new(60, -1).reduced(), None);
        assert!(Rational::new(120, 2).same_value(Rational::new(60, 1)));
        assert!(!Rational::new(0, 1).same_value(Rational::new(0, 1)));
    }

    #[test]
    fn multiplies_with_cross_reduction() {
        assert_eq!(
            Rational::new(30000, 1001).multiply(Rational::new(1, 2)),
            Some(Rational::new(15000, 1001))
        );
        // 交差約分しなければ i64 を超える積。
        assert_eq!(
            Rational::new(i64::MAX, 3).multiply(Rational::new(3, i64::MAX)),
            Some(Rational::new(1, 1))
        );
        assert_eq!(
            Rational::new(i64::MAX, 1).multiply(Rational::new(2, 1)),
            None
        );
        assert_eq!(Rational::new(60, 1).multiply(Rational::new(0, 1)), None);
    }

    #[test]
    fn rounds_toward_infinities() {
        assert_eq!(div_floor(7, 2), 3);
        assert_eq!(div_floor(-7, 2), -4);
        assert_eq!(div_floor(-8, 2), -4);
        assert_eq!(div_ceil(7, 2), 4);
        assert_eq!(div_ceil(-7, 2), -3);
        assert_eq!(div_ceil(8, 2), 4);
    }
}
