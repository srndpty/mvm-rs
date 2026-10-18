//! 出力 timebase（output frame と音声 sample と時刻の換算）。整数と有理数だけで計算する。
//!
//! **measured と configurable を混ぜない。**
//! - measured: canonical preview workload を実測して qualify した rate。「出る」と書いてよいのはこれだけ。
//! - configurable: 製品が設定として受理する rate。measured を含むが、measured でない rate も含む。
//!   受理すること自体は qualification ではない。
//!
//! 実装が受理するようになったことを根拠に measured へ昇格させてはいけない。
//! 昇格は canonical workload の実測を取ってから行う。

use std::fmt;

use crate::rational::{Rational, div_ceil, div_floor, gcd};

/// qualified な音声 sample rate。timebase と Project の両方がこれを基準にする。
pub const QUALIFIED_AUDIO_SAMPLE_RATE: i64 = 48_000;

/// canonical preview workload を実測済みの rate。実測を取らずにここへ足さない。
pub const MEASURED_OUTPUT_FRAME_RATES: &[Rational] = &[Rational::new(60, 1)];

/// 製品が設定として受理する rate。NTSC 系（1001 分母）は素材の実 fps がこの形で出てくるため含める。
pub const CONFIGURABLE_OUTPUT_FRAME_RATES: &[Rational] = &[
    Rational::new(24, 1),
    Rational::new(24000, 1001),
    Rational::new(25, 1),
    Rational::new(30, 1),
    Rational::new(30000, 1001),
    Rational::new(50, 1),
    Rational::new(60, 1),
    Rational::new(60000, 1001),
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TimebaseError {
    InvalidNumerator,
    InvalidDenominator,
    InvalidSampleRate,
    UnsupportedProductRate,
    IntermediateOverflow,
    ResultOverflow,
}

impl fmt::Display for TimebaseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::InvalidNumerator => "frame rate の分子が正ではありません",
            Self::InvalidDenominator => "frame rate の分母が正ではありません",
            Self::InvalidSampleRate => "音声 sample rate が正ではありません",
            Self::UnsupportedProductRate => "製品が扱わない frame rate または sample rate です",
            Self::IntermediateOverflow => "timebase の中間計算が溢れました",
            Self::ResultOverflow => "timebase の計算結果が 64 bit に収まりません",
        };
        f.write_str(message)
    }
}

impl std::error::Error for TimebaseError {}

pub fn is_measured_output_frame_rate(rate: Rational) -> bool {
    MEASURED_OUTPUT_FRAME_RATES
        .iter()
        .any(|known| known.same_value(rate))
}

pub fn is_configurable_output_frame_rate(rate: Rational) -> bool {
    CONFIGURABLE_OUTPUT_FRAME_RATES
        .iter()
        .any(|known| known.same_value(rate))
}

/// 約分済みの frame rate と音声 sample rate を持つ出力 timebase。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OutputTimebase {
    frame_rate: Rational,
    audio_sample_rate: i64,
}

impl OutputTimebase {
    /// 正の入力を約分して作る。製品の rate 表は参照しない。
    pub fn new(frame_rate: Rational, audio_sample_rate: i64) -> Result<Self, TimebaseError> {
        if frame_rate.num <= 0 {
            return Err(TimebaseError::InvalidNumerator);
        }
        if frame_rate.den <= 0 {
            return Err(TimebaseError::InvalidDenominator);
        }
        if audio_sample_rate <= 0 {
            return Err(TimebaseError::InvalidSampleRate);
        }
        let divisor = gcd(frame_rate.num, frame_rate.den);
        Ok(Self {
            frame_rate: Rational::new(frame_rate.num / divisor, frame_rate.den / divisor),
            audio_sample_rate,
        })
    }

    /// measured な rate だけを受理する。qualification の判定はこちら。
    pub fn qualified(frame_rate: Rational, audio_sample_rate: i64) -> Result<Self, TimebaseError> {
        Self::new_in_table(frame_rate, audio_sample_rate, is_measured_output_frame_rate)
    }

    /// configurable な rate を受理する。measured とは限らないので「qualify されている」と読み替えない。
    pub fn configured(frame_rate: Rational, audio_sample_rate: i64) -> Result<Self, TimebaseError> {
        Self::new_in_table(
            frame_rate,
            audio_sample_rate,
            is_configurable_output_frame_rate,
        )
    }

    fn new_in_table(
        frame_rate: Rational,
        audio_sample_rate: i64,
        in_table: fn(Rational) -> bool,
    ) -> Result<Self, TimebaseError> {
        let timebase = Self::new(frame_rate, audio_sample_rate)?;
        if !in_table(timebase.frame_rate) || audio_sample_rate != QUALIFIED_AUDIO_SAMPLE_RATE {
            return Err(TimebaseError::UnsupportedProductRate);
        }
        Ok(timebase)
    }

    /// 約分済みの frame rate。
    pub fn frame_rate(&self) -> Rational {
        self.frame_rate
    }

    pub fn audio_sample_rate(&self) -> i64 {
        self.audio_sample_rate
    }

    /// output frame の正確な時刻（秒）を約分した有理数。
    pub fn exact_media_time(&self, output_frame: i64) -> Result<Rational, TimebaseError> {
        let numerator = i128::from(output_frame)
            .checked_mul(i128::from(self.frame_rate.den))
            .ok_or(TimebaseError::IntermediateOverflow)?;
        let denominator = i128::from(self.frame_rate.num);
        let divisor = gcd_i128(numerator, denominator);
        Ok(Rational::new(
            narrow(numerator / divisor)?,
            narrow(denominator / divisor)?,
        ))
    }

    /// output frame が始まる最初の音声 sample（ceil）。seek の目標 sample もこれで決める。
    pub fn first_audio_sample(&self, output_frame: i64) -> Result<i64, TimebaseError> {
        let product = i128::from(output_frame)
            .checked_mul(i128::from(self.frame_rate.den))
            .and_then(|value| value.checked_mul(i128::from(self.audio_sample_rate)))
            .ok_or(TimebaseError::IntermediateOverflow)?;
        narrow(div_ceil(product, i128::from(self.frame_rate.num)))
    }

    /// 音声 sample を含む output frame（floor）。scheduler と状態表示もこれで決める。
    pub fn output_frame(&self, audio_sample: i64) -> Result<i64, TimebaseError> {
        let numerator = i128::from(audio_sample)
            .checked_mul(i128::from(self.frame_rate.num))
            .ok_or(TimebaseError::IntermediateOverflow)?;
        let denominator = i128::from(self.audio_sample_rate)
            .checked_mul(i128::from(self.frame_rate.den))
            .ok_or(TimebaseError::IntermediateOverflow)?;
        narrow(div_floor(numerator, denominator))
    }

    /// wall-clock の経過 nanosecond から output frame を導く（floor）。音声 master が無い経路も独自の式を持たない。
    pub fn output_frame_for_nanoseconds(&self, nanoseconds: i64) -> Result<i64, TimebaseError> {
        const NANOSECONDS_PER_SECOND: i128 = 1_000_000_000;
        let numerator = i128::from(nanoseconds)
            .checked_mul(i128::from(self.frame_rate.num))
            .ok_or(TimebaseError::IntermediateOverflow)?;
        let denominator = i128::from(self.frame_rate.den)
            .checked_mul(NANOSECONDS_PER_SECOND)
            .ok_or(TimebaseError::IntermediateOverflow)?;
        narrow(div_floor(numerator, denominator))
    }
}

fn narrow(value: i128) -> Result<i64, TimebaseError> {
    i64::try_from(value).map_err(|_| TimebaseError::ResultOverflow)
}

fn gcd_i128(left: i128, right: i128) -> i128 {
    let (mut left, mut right) = (left.unsigned_abs(), right.unsigned_abs());
    while right != 0 {
        (left, right) = (right, left % right);
    }
    // 分母（frame rate の分子）は正なので、結果は 1 以上で i128 に収まる。
    i128::try_from(left).unwrap_or(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rate(num: i64, den: i64) -> Rational {
        Rational::new(num, den)
    }

    #[test]
    fn canonicalizes_and_rejects_invalid_input() {
        let reduced = OutputTimebase::new(rate(120, 2), 48000).unwrap();
        assert_eq!(reduced.frame_rate(), rate(60, 1));
        let qualified = OutputTimebase::qualified(rate(120, 2), 48000).unwrap();
        assert_eq!(qualified.frame_rate(), rate(60, 1));
        assert_eq!(
            OutputTimebase::new(rate(0, 1), 48000),
            Err(TimebaseError::InvalidNumerator)
        );
        assert_eq!(
            OutputTimebase::new(rate(-1, 1), 48000),
            Err(TimebaseError::InvalidNumerator)
        );
        assert_eq!(
            OutputTimebase::new(rate(60, 0), 48000),
            Err(TimebaseError::InvalidDenominator)
        );
        assert_eq!(
            OutputTimebase::new(rate(60, -1), 48000),
            Err(TimebaseError::InvalidDenominator)
        );
        assert_eq!(
            OutputTimebase::new(rate(60, 1), 0),
            Err(TimebaseError::InvalidSampleRate)
        );
    }

    // measured と configurable は別の表である。期待値は両方 literal で書く。
    // 実装の判定関数を呼ぶと、表を壊した変更を試験が追認してしまう。
    #[test]
    fn measured_and_configurable_tables_stay_separate() {
        assert!(OutputTimebase::qualified(rate(60, 1), 48000).is_ok());
        for (num, den) in [
            (24, 1),
            (24000, 1001),
            (25, 1),
            (30, 1),
            (30000, 1001),
            (50, 1),
            (60000, 1001),
        ] {
            assert_eq!(
                OutputTimebase::qualified(rate(num, den), 48000),
                Err(TimebaseError::UnsupportedProductRate),
                "未計測の {num}/{den} を qualified として受理しました"
            );
            let configured = OutputTimebase::configured(rate(num, den), 48000).unwrap();
            assert_eq!(configured.frame_rate(), rate(num, den));
        }
        for (num, den) in [(48, 1), (15, 1), (23, 1), (59, 1), (120, 1), (24000, 1002)] {
            assert_eq!(
                OutputTimebase::configured(rate(num, den), 48000),
                Err(TimebaseError::UnsupportedProductRate)
            );
            assert_eq!(
                OutputTimebase::qualified(rate(num, den), 48000),
                Err(TimebaseError::UnsupportedProductRate)
            );
        }
        assert_eq!(
            OutputTimebase::configured(rate(60, 1), 44100),
            Err(TimebaseError::UnsupportedProductRate)
        );
        assert_eq!(
            OutputTimebase::qualified(rate(60, 1), 44100),
            Err(TimebaseError::UnsupportedProductRate)
        );
    }

    #[test]
    fn maps_frames_and_samples_with_mathematical_rounding() {
        let timebase = OutputTimebase::qualified(rate(60, 1), 48000).unwrap();
        assert_eq!(timebase.exact_media_time(0), Ok(rate(0, 1)));
        assert_eq!(timebase.exact_media_time(1), Ok(rate(1, 60)));
        assert_eq!(timebase.exact_media_time(-1), Ok(rate(-1, 60)));
        assert_eq!(timebase.first_audio_sample(0), Ok(0));
        assert_eq!(timebase.first_audio_sample(1), Ok(800));
        assert_eq!(timebase.first_audio_sample(-1), Ok(-800));
        assert_eq!(timebase.first_audio_sample(7), Ok(5600));
        assert_eq!(timebase.output_frame(0), Ok(0));
        assert_eq!(timebase.output_frame(799), Ok(0));
        assert_eq!(timebase.output_frame(800), Ok(1));
        assert_eq!(timebase.output_frame(5599), Ok(6));
        assert_eq!(timebase.output_frame(-1), Ok(-1));
        assert_eq!(timebase.output_frame(-800), Ok(-1));
        assert_eq!(timebase.output_frame(-801), Ok(-2));

        let fractional = OutputTimebase::new(rate(3, 2), 10).unwrap();
        assert_eq!(fractional.first_audio_sample(1), Ok(7));
        assert_eq!(fractional.first_audio_sample(-1), Ok(-6));
        assert_eq!(fractional.output_frame(1), Ok(0));
        assert_eq!(fractional.output_frame(-1), Ok(-1));
    }

    #[test]
    fn rejects_overflow() {
        let timebase = OutputTimebase::qualified(rate(60, 1), 48000).unwrap();
        assert!(timebase.exact_media_time(i64::MIN).is_ok());
        assert!(timebase.exact_media_time(i64::MAX).is_ok());
        assert!(timebase.output_frame(i64::MIN).is_ok());
        assert!(timebase.output_frame(i64::MAX).is_ok());
        assert_eq!(
            timebase.first_audio_sample(i64::MIN),
            Err(TimebaseError::ResultOverflow)
        );
        assert_eq!(
            timebase.first_audio_sample(i64::MAX),
            Err(TimebaseError::ResultOverflow)
        );

        let wide = OutputTimebase::new(rate(i64::MAX, i64::MAX - 1), i64::MAX).unwrap();
        assert_eq!(
            wide.first_audio_sample(i64::MAX),
            Err(TimebaseError::IntermediateOverflow)
        );
        let half = OutputTimebase::new(rate(1, 2), 1).unwrap();
        assert_eq!(
            half.exact_media_time(i64::MAX),
            Err(TimebaseError::ResultOverflow)
        );
    }

    // 期待値は floor(ns * num / (den * 1e9)) を独立に計算した literal。
    #[test]
    fn maps_wall_clock_nanoseconds() {
        let timebase = OutputTimebase::qualified(rate(60, 1), 48000).unwrap();
        assert_eq!(timebase.output_frame_for_nanoseconds(0), Ok(0));
        assert_eq!(timebase.output_frame_for_nanoseconds(16_666_666), Ok(0));
        assert_eq!(timebase.output_frame_for_nanoseconds(16_666_667), Ok(1));
        assert_eq!(timebase.output_frame_for_nanoseconds(1_000_000_000), Ok(60));
        assert_eq!(timebase.output_frame_for_nanoseconds(999_999_999), Ok(59));
        assert_eq!(
            timebase.output_frame_for_nanoseconds(10_000_000_000),
            Ok(600)
        );
        assert_eq!(
            timebase.output_frame_for_nanoseconds(-1_000_000_000),
            Ok(-60)
        );
        assert_eq!(timebase.output_frame_for_nanoseconds(-1), Ok(-1));

        let ntsc = OutputTimebase::new(rate(24000, 1001), 48000).unwrap();
        assert_eq!(ntsc.output_frame_for_nanoseconds(1_000_000_000), Ok(23));

        let wide = OutputTimebase::new(rate(i64::MAX, 1), 48000).unwrap();
        assert_eq!(
            wide.output_frame_for_nanoseconds(i64::MAX),
            Err(TimebaseError::ResultOverflow)
        );
    }
}
