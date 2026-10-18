//! timeline の波形表示用の min/max peak。decode や描画には依存しない。
//!
//! 値は [-1, 1] の振幅を i8（-127..=127）へ量子化して持つ。表示の縦解像度は高々数十 pixel なので
//! f32 で持つ意味が無く、1 時間の stereo でも約 11MB に収まる。

use std::fmt;

/// 粗い level は 1 つ細かい level の peak をこの数ずつまとめる。
pub const WAVEFORM_LEVEL_FACTOR: i64 = 4;

/// 集計中の作業領域（channel ごとの f32 の min/max）の上限。
/// 壊れた container の巨大な PTS 1 つで数十 GB を確保しにいかないよう、確保する byte 数で打ち切る。
/// stereo なら約 16M peak = 約 6 時間分。
pub const MAXIMUM_WAVEFORM_WORKING_BYTES: usize = 256 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WaveformPeakLevel {
    pub samples_per_peak: i64,
    pub peak_count: i64,
    /// channel-major。channel c の i 番目は `[c * peak_count + i]`。
    pub minimum: Vec<i8>,
    pub maximum: Vec<i8>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WaveformPeaks {
    pub sample_rate: i32,
    pub channels: i32,
    /// `levels[0]` が最も細かい。以降は [`WAVEFORM_LEVEL_FACTOR`] 倍ずつ粗くなる。
    /// zoom out 時に 1 pixel あたり数千 peak を走査しないためのもの。
    pub levels: Vec<WaveformPeakLevel>,
}

impl WaveformPeaks {
    /// 完成した peak が占める byte 数。cache の budget 計算に使う。
    pub fn memory_bytes(&self) -> usize {
        size_of::<Self>()
            + self
                .levels
                .iter()
                .map(|level| {
                    size_of::<WaveformPeakLevel>()
                        + level.minimum.capacity()
                        + level.maximum.capacity()
                })
                .sum::<usize>()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WaveformError {
    InvalidFormat,
    BudgetTooSmall,
    InvalidPlanes,
    TimestampOutOfRange,
    Empty,
}

impl fmt::Display for WaveformError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::InvalidFormat => "波形の sample rate / channel 数 / peak 幅が不正です",
            Self::BudgetTooSmall => "波形の作業領域の上限が小さすぎます",
            Self::InvalidPlanes => "波形へ渡す sample 列が不正です",
            Self::TimestampOutOfRange => "音声素材が長すぎるか、timestamp が不正です",
            Self::Empty => "音声 sample が 1 つも得られませんでした",
        };
        f.write_str(message)
    }
}

impl std::error::Error for WaveformError {}

/// 素材の sample rate から最細 level の peak 幅を決める。約 1/750 秒。
/// timeline の最大 zoom（24 px/frame @ 60fps = 1440 px/s）で 1 peak が 2 pixel 程度になる。
pub fn waveform_samples_per_peak(sample_rate: i32) -> i64 {
    i64::from(sample_rate / 750).max(1)
}

pub struct WaveformPeakBuilder {
    sample_rate: i32,
    channels: usize,
    samples_per_peak: i64,
    peak_count: i64,
    maximum_peak_count: i64,
    /// channel ごとの未量子化の min/max。未到達の bucket は +∞ / -∞ で、finish で無音にする。
    minimum: Vec<Vec<f32>>,
    maximum: Vec<Vec<f32>>,
}

impl WaveformPeakBuilder {
    pub fn new(
        sample_rate: i32,
        channels: i32,
        samples_per_peak: i64,
        working_byte_budget: usize,
    ) -> Result<Self, WaveformError> {
        let channel_count = usize::try_from(channels).map_err(|_| WaveformError::InvalidFormat)?;
        if sample_rate <= 0 || channel_count == 0 || samples_per_peak <= 0 {
            return Err(WaveformError::InvalidFormat);
        }
        // 1 peak あたり channel ごとに f32 の min/max を 1 つずつ持つ。
        let bytes_per_peak = channel_count
            .checked_mul(2 * size_of::<f32>())
            .ok_or(WaveformError::InvalidFormat)?;
        let maximum_peaks = working_byte_budget / bytes_per_peak;
        if maximum_peaks == 0 {
            return Err(WaveformError::BudgetTooSmall);
        }
        Ok(Self {
            sample_rate,
            channels: channel_count,
            samples_per_peak,
            peak_count: 0,
            maximum_peak_count: i64::try_from(maximum_peaks).unwrap_or(i64::MAX),
            minimum: vec![Vec::new(); channel_count],
            maximum: vec![Vec::new(); channel_count],
        })
    }

    /// planar f32 の sample 列を、素材内の sample 位置 `start_sample` から置く。
    /// 位置で置くので、decode 結果に隙間や重なりがあっても時刻がずれない。
    /// 素材開始より前（負の位置）の sample は捨てる。
    /// 作業領域が budget を超える位置、または位置の計算が溢れる位置は失敗にする。
    pub fn add_planar(
        &mut self,
        start_sample: i64,
        planes: &[&[f32]],
    ) -> Result<(), WaveformError> {
        let frame_count = planes.first().map_or(0, |plane| plane.len());
        if planes.len() != self.channels || planes.iter().any(|plane| plane.len() != frame_count) {
            return Err(WaveformError::InvalidPlanes);
        }
        let frame_count = i64::try_from(frame_count).map_err(|_| WaveformError::InvalidPlanes)?;
        if frame_count == 0 {
            return Ok(());
        }
        // 全体が素材開始より前なら捨てるだけ（i64::MIN でも溢れない形で判定する）。
        if start_sample <= -frame_count {
            return Ok(());
        }
        let last_sample = start_sample
            .checked_add(frame_count - 1)
            .ok_or(WaveformError::TimestampOutOfRange)?;
        let required_peaks = last_sample / self.samples_per_peak + 1;
        if required_peaks > self.maximum_peak_count {
            return Err(WaveformError::TimestampOutOfRange);
        }
        if required_peaks > self.peak_count {
            let length =
                usize::try_from(required_peaks).map_err(|_| WaveformError::TimestampOutOfRange)?;
            for low in &mut self.minimum {
                low.resize(length, f32::INFINITY);
            }
            for high in &mut self.maximum {
                high.resize(length, f32::NEG_INFINITY);
            }
            self.peak_count = required_peaks;
        }
        let first_frame = if start_sample < 0 { -start_sample } else { 0 };
        for (channel, plane) in planes.iter().enumerate() {
            let (low, high) = (&mut self.minimum[channel], &mut self.maximum[channel]);
            for frame in first_frame..frame_count {
                let value = plane[frame as usize];
                if !value.is_finite() {
                    continue;
                }
                let bucket = ((start_sample + frame) / self.samples_per_peak) as usize;
                low[bucket] = low[bucket].min(value);
                high[bucket] = high[bucket].max(value);
            }
        }
        Ok(())
    }

    /// 最後に sample を置いた位置までを peak 化し、粗い level も作る。
    /// 1 sample も置かれていなければ失敗にする（空の波形を成功にしない）。
    pub fn finish(self) -> Result<WaveformPeaks, WaveformError> {
        if self.peak_count == 0 {
            return Err(WaveformError::Empty);
        }
        let mut finest = WaveformPeakLevel {
            samples_per_peak: self.samples_per_peak,
            peak_count: self.peak_count,
            minimum: Vec::new(),
            maximum: Vec::new(),
        };
        for (low, high) in self.minimum.iter().zip(&self.maximum) {
            for (&low, &high) in low.iter().zip(high) {
                let touched = low <= high;
                finest.minimum.push(if touched { quantize(low) } else { 0 });
                finest
                    .maximum
                    .push(if touched { quantize(high) } else { 0 });
            }
        }
        let mut levels = vec![finest];
        while let Some(last) = levels.last()
            && last.peak_count > WAVEFORM_LEVEL_FACTOR
        {
            let coarser = coarsen(last, self.channels);
            levels.push(coarser);
        }
        Ok(WaveformPeaks {
            sample_rate: self.sample_rate,
            channels: self.channels as i32,
            levels,
        })
    }
}

fn quantize(value: f32) -> i8 {
    (value.clamp(-1.0, 1.0) * 127.0).round() as i8
}

fn coarsen(finer: &WaveformPeakLevel, channels: usize) -> WaveformPeakLevel {
    let factor = WAVEFORM_LEVEL_FACTOR as usize;
    let finer_count = finer.peak_count as usize;
    let mut coarser = WaveformPeakLevel {
        samples_per_peak: finer.samples_per_peak * WAVEFORM_LEVEL_FACTOR,
        peak_count: (finer.peak_count + WAVEFORM_LEVEL_FACTOR - 1) / WAVEFORM_LEVEL_FACTOR,
        minimum: Vec::new(),
        maximum: Vec::new(),
    };
    for channel in 0..channels {
        let range = channel * finer_count..(channel + 1) * finer_count;
        for chunk in finer.minimum[range.clone()].chunks(factor) {
            coarser
                .minimum
                .push(chunk.iter().copied().min().unwrap_or(0));
        }
        for chunk in finer.maximum[range].chunks(factor) {
            coarser
                .maximum
                .push(chunk.iter().copied().max().unwrap_or(0));
        }
    }
    coarser
}

/// 1 列分の振幅の範囲（-1..=1）。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WaveformColumn {
    pub minimum: f32,
    pub maximum: f32,
}

/// 素材内の時刻区間 [start_seconds, end_seconds) の min/max。区間幅に見合う level を選ぶ。
/// 素材の範囲外・不正な channel・空の区間は `None`。
pub fn waveform_column(
    peaks: &WaveformPeaks,
    channel: i32,
    start_seconds: f64,
    end_seconds: f64,
) -> Option<WaveformColumn> {
    if peaks.sample_rate <= 0
        || !(0..peaks.channels).contains(&channel)
        || !start_seconds.is_finite()
        || !end_seconds.is_finite()
        || end_seconds <= start_seconds
    {
        return None;
    }
    let sample_rate = f64::from(peaks.sample_rate);
    // 端の peak は区間の外まで覆うので、peak 幅がそのまま区間外への滲みになる。
    // 区間幅の 1/WAVEFORM_LEVEL_FACTOR 以下の peak 幅を持つ最も粗い level を選び、
    // 滲みを 1 列の 1/4 未満に抑える。区間が最細 peak より狭ければ最細を使う。
    let maximum_seconds_per_peak = (end_seconds - start_seconds) / WAVEFORM_LEVEL_FACTOR as f64;
    let level = peaks
        .levels
        .iter()
        .skip(1)
        .take_while(|level| level.samples_per_peak as f64 / sample_rate <= maximum_seconds_per_peak)
        .last()
        .or_else(|| peaks.levels.first())?;
    let seconds_per_peak = level.samples_per_peak as f64 / sample_rate;
    let first = (start_seconds / seconds_per_peak).floor();
    let last = (end_seconds / seconds_per_peak).ceil();
    if last <= 0.0 || first >= level.peak_count as f64 {
        return None;
    }
    // f64 → i64 の `as` は飽和する。
    let begin = (first as i64).max(0);
    let end = level.peak_count.min((begin + 1).max(last as i64));
    let base = i64::from(channel) * level.peak_count;
    let range = (base + begin) as usize..(base + end) as usize;
    let minimum = level.minimum.get(range.clone())?.iter().copied().min()?;
    let maximum = level.maximum.get(range)?.iter().copied().max()?;
    Some(WaveformColumn {
        minimum: f32::from(minimum) / 127.0,
        maximum: f32::from(maximum) / 127.0,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // 期待値は実装の式を使わず、手で数えた値を直接書く。

    #[test]
    fn rejects_invalid_format_and_empty_waveform() {
        let budget = MAXIMUM_WAVEFORM_WORKING_BYTES;
        assert!(WaveformPeakBuilder::new(0, 1, 4, budget).is_err());
        assert!(WaveformPeakBuilder::new(48000, 0, 4, budget).is_err());
        assert!(WaveformPeakBuilder::new(48000, 1, 0, budget).is_err());
        let builder = WaveformPeakBuilder::new(8, 1, 4, budget).unwrap();
        assert_eq!(builder.finish().unwrap_err(), WaveformError::Empty);
    }

    #[test]
    fn rejects_planes_that_do_not_match_the_format() {
        let mut builder =
            WaveformPeakBuilder::new(8, 2, 4, MAXIMUM_WAVEFORM_WORKING_BYTES).unwrap();
        let samples = [0.5_f32; 4];
        assert_eq!(
            builder.add_planar(0, &[&samples]),
            Err(WaveformError::InvalidPlanes)
        );
        assert_eq!(
            builder.add_planar(0, &[&samples, &samples[..3]]),
            Err(WaveformError::InvalidPlanes)
        );
    }

    #[test]
    fn broken_timestamps_do_not_allocate_or_overflow() {
        let values = [0.5_f32; 100];
        let planes: [&[f32]; 2] = [&values, &values];
        let mut builder =
            WaveformPeakBuilder::new(48000, 2, 64, MAXIMUM_WAVEFORM_WORKING_BYTES).unwrap();
        assert_eq!(
            builder.add_planar(i64::MAX - 1, &planes),
            Err(WaveformError::TimestampOutOfRange)
        );
        // 受理すると stereo で約 2^40/64 peak × 16 byte = 256GiB を確保しにいく。
        assert_eq!(
            builder.add_planar(1 << 40, &planes),
            Err(WaveformError::TimestampOutOfRange)
        );
        assert_eq!(builder.add_planar(i64::MIN, &planes), Ok(()));
        assert_eq!(builder.add_planar(0, &planes), Ok(()));
        let peaks = builder.finish().unwrap();
        assert_eq!(peaks.levels[0].peak_count, 2);
    }

    #[test]
    fn budget_boundary_is_sixteen_bytes_per_stereo_peak() {
        let values = [0.5_f32; 4];
        let planes: [&[f32]; 2] = [&values, &values];
        assert_eq!(
            WaveformPeakBuilder::new(8, 2, 4, 15).err(),
            Some(WaveformError::BudgetTooSmall)
        );
        let mut builder = WaveformPeakBuilder::new(8, 2, 4, 32).unwrap();
        assert_eq!(builder.add_planar(4, &planes), Ok(()));
        assert_eq!(
            builder.add_planar(5, &planes),
            Err(WaveformError::TimestampOutOfRange)
        );
    }

    /// 2 channel、peak 幅 4、sample rate 8（1 peak = 0.5 秒）。
    fn two_channel_peaks() -> WaveformPeaks {
        let mut builder =
            WaveformPeakBuilder::new(8, 2, 4, MAXIMUM_WAVEFORM_WORKING_BYTES).unwrap();
        // peak 0（sample 0..3）: left は正だけ、right は負だけ。
        builder
            .add_planar(0, &[&[0.5, 0.25, 1.0, 0.75], &[-0.5, -1.0, -0.25, -0.75]])
            .unwrap();
        // sample -2, -1 は捨てられ、sample 0, 1 に -0.5, 0.0 / 0.0, 0.0 が加わる。
        builder
            .add_planar(-2, &[&[1.0, 1.0, -0.5, 0.0], &[1.0, 1.0, 0.0, 0.0]])
            .unwrap();
        // peak 1 は飛ばして peak 2（sample 8..11）だけに置く。
        builder
            .add_planar(8, &[&[0.0, 2.0, 0.0, 0.0], &[0.0, 0.0, 0.0, -2.0]])
            .unwrap();
        builder.finish().unwrap()
    }

    #[test]
    fn aggregates_min_max_per_channel() {
        let peaks = two_channel_peaks();
        assert_eq!((peaks.sample_rate, peaks.channels), (8, 2));
        let level = &peaks.levels[0];
        assert_eq!((level.samples_per_peak, level.peak_count), (4, 3));
        // left: peak0 = [-0.5, 1.0]、peak1 は未到達で無音、peak2 は 2.0 を 1.0 へ clamp。
        assert_eq!(&level.minimum[0..3], &[-64, 0, 0]);
        assert_eq!(&level.maximum[0..3], &[127, 0, 127]);
        // right: peak0 = [-1.0, 0.0]、peak2 = [-1.0, 0.0]。
        assert_eq!((level.minimum[3], level.maximum[3]), (-127, 0));
        assert_eq!((level.minimum[5], level.maximum[5]), (-127, 0));
    }

    #[test]
    fn positive_only_bucket_keeps_its_minimum() {
        let mut builder =
            WaveformPeakBuilder::new(8, 1, 4, MAXIMUM_WAVEFORM_WORKING_BYTES).unwrap();
        builder.add_planar(0, &[&[0.5, 0.75, 0.5, 0.5]]).unwrap();
        let peaks = builder.finish().unwrap();
        assert_eq!(
            (peaks.levels[0].minimum[0], peaks.levels[0].maximum[0]),
            (64, 95)
        );
    }

    #[test]
    fn columns_cover_touched_peaks_only() {
        let peaks = two_channel_peaks();
        let first = waveform_column(&peaks, 0, 0.0, 0.5).unwrap();
        assert_eq!((first.minimum, first.maximum), (-64.0 / 127.0, 1.0));
        let silent = waveform_column(&peaks, 0, 0.5, 1.0).unwrap();
        assert_eq!((silent.minimum, silent.maximum), (0.0, 0.0));
        assert_eq!(
            waveform_column(&peaks, 1, 0.25, 1.25).unwrap().minimum,
            -1.0
        );
        assert_eq!(waveform_column(&peaks, 0, 1.5, 2.0), None);
        assert_eq!(waveform_column(&peaks, 0, -1.0, -0.5), None);
        assert_eq!(waveform_column(&peaks, 2, 0.0, 0.5), None);
        assert_eq!(waveform_column(&peaks, 0, 0.5, 0.5), None);
    }

    #[test]
    fn coarse_levels_keep_peaks_without_bleeding() {
        // 境界が浮動小数で揺れないよう、1 peak = 1/128 秒にする。
        let mut builder =
            WaveformPeakBuilder::new(128, 1, 1, MAXIMUM_WAVEFORM_WORKING_BYTES).unwrap();
        let mut values = vec![0.0_f32; 1000];
        values[777] = 0.5;
        values[123] = -0.25;
        builder.add_planar(0, &[&values]).unwrap();
        let peaks = builder.finish().unwrap();
        // 1000 peak → 250 → 63 → 16 → 4 で止まる。
        assert_eq!(peaks.levels.len(), 5);
        assert_eq!(
            (peaks.levels[4].samples_per_peak, peaks.levels[4].peak_count),
            (256, 4)
        );
        // [6.2, 7.0) 秒は sample 777（6.07 秒）を含まない。粗すぎる level を選ぶと滲み込む。
        let outside = waveform_column(&peaks, 0, 6.2, 7.0).unwrap();
        assert_eq!((outside.minimum, outside.maximum), (0.0, 0.0));
        let whole = waveform_column(&peaks, 0, 0.0, 10.0).unwrap();
        assert_eq!(
            (whole.minimum, whole.maximum),
            (-32.0 / 127.0, 64.0 / 127.0)
        );
        // sample 777 = 6.0703125 秒。
        assert_eq!(
            waveform_column(&peaks, 0, 6.0703125, 6.078125)
                .unwrap()
                .maximum,
            64.0 / 127.0
        );
        assert_eq!(
            waveform_column(&peaks, 0, 6.078125, 6.0859375)
                .unwrap()
                .maximum,
            0.0
        );
    }

    #[test]
    fn memory_bytes_counts_stored_peaks() {
        let peaks = two_channel_peaks();
        let stored: usize = peaks
            .levels
            .iter()
            .map(|level| level.minimum.len() + level.maximum.len())
            .sum();
        assert_eq!(stored, 12);
        assert!(peaks.memory_bytes() >= stored);
    }

    #[test]
    fn samples_per_peak_follows_sample_rate() {
        assert_eq!(waveform_samples_per_peak(48000), 64);
        assert_eq!(waveform_samples_per_peak(100), 1);
    }
}
