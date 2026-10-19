//! 文字起こしの入力: 素材の音声の区間を 16 kHz mono の float にする。
//!
//! decode は素材の sample rate のまま行い（sample の位置は PTS から決める）、mono への downmix と
//! 16 kHz への変換は窓付き sinc で少しずつ行う。区間の開始が 2 秒より後なら、2 秒手前へ seek してから読む
//! （mp3 は索引が無いと seek 先が推定になるので先頭から読む）。

use std::collections::VecDeque;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::error::MediaError;
use crate::ffi::{
    AudioConverter, Decoder, Frame, Input, Packet, ReadStatus, ReceiveStatus, SendStatus,
};

pub const SPEECH_SAMPLE_RATE: i64 = 16_000;
/// 窓の片側の tap 数（出力の sample 単位）。
const HALF_TAPS: f64 = 16.0;
const SEEK_LEAD_MS: i64 = 2_000;
const MAXIMUM_INVALID_PACKETS: u32 = 32;

/// 任意の比で mono を 16 kHz にする。入力は絶対 sample 位置（素材の rate）で渡す。
struct Resampler {
    rate: f64,
    /// 入力の窓。`input[0]` の絶対位置が `input_start`。
    input: VecDeque<f32>,
    input_start: i64,
    next_output: i64,
    /// 帯域の上限（入力の Nyquist に対する比）。
    cutoff: f64,
    half_width: f64,
}

impl Resampler {
    fn new(rate: i64, first_output: i64) -> Self {
        let rate = rate as f64;
        let cutoff = (SPEECH_SAMPLE_RATE as f64 / rate).min(1.0) * 0.95;
        Self {
            rate,
            input: VecDeque::new(),
            input_start: 0,
            next_output: first_output,
            cutoff,
            half_width: HALF_TAPS / cutoff,
        }
    }

    fn input_position(&self, output: i64) -> f64 {
        output as f64 * self.rate / SPEECH_SAMPLE_RATE as f64
    }

    /// `start` から始まる入力を足す。前の入力との隙間は無音、重なりは捨てる。
    fn push(&mut self, start: i64, samples: &[f32]) {
        if self.input.is_empty() {
            self.input_start = start;
        }
        let end = self.input_start + self.input.len() as i64;
        let skip = usize::try_from(end - start).unwrap_or(0);
        if start > end {
            let gap = usize::try_from(start - end).unwrap_or(0);
            self.input.extend(std::iter::repeat_n(0.0, gap));
        }
        self.input.extend(samples.iter().skip(skip));
    }

    /// 窓が揃った出力を `out` へ足す。`finish` なら入力の後ろを無音として最後まで出す。
    fn drain(&mut self, out: &mut Vec<(i64, f32)>, finish: bool, end_output: Option<i64>) {
        let input_end = self.input_start + self.input.len() as i64;
        loop {
            if end_output.is_some_and(|end| self.next_output >= end) {
                break;
            }
            let center = self.input_position(self.next_output);
            if !finish && center + self.half_width >= input_end as f64 {
                break;
            }
            if finish && center >= input_end as f64 {
                break;
            }
            let first = (center - self.half_width).ceil() as i64;
            let last = (center + self.half_width).floor() as i64;
            let mut sum = 0.0f64;
            let mut weight = 0.0f64;
            for index in first..=last {
                let x = (index as f64 - center) * self.cutoff;
                let sinc = if x.abs() < 1e-12 {
                    1.0
                } else {
                    (std::f64::consts::PI * x).sin() / (std::f64::consts::PI * x)
                };
                let phase = (index as f64 - center) / self.half_width;
                let window = 0.5 * (1.0 + (std::f64::consts::PI * phase).cos());
                let tap = sinc * window;
                weight += tap;
                let value = index
                    .checked_sub(self.input_start)
                    .and_then(|offset| usize::try_from(offset).ok())
                    .and_then(|offset| self.input.get(offset))
                    .copied()
                    .unwrap_or(0.0);
                sum += f64::from(value) * tap;
            }
            let value = if weight.abs() > 1e-12 {
                sum / weight
            } else {
                0.0
            };
            out.push((self.next_output, value as f32));
            self.next_output += 1;
        }
        // 次の出力の窓より前の入力を捨てる。
        let keep_from =
            (self.input_position(self.next_output) - self.half_width).floor() as i64 - 1;
        while self.input_start < keep_from && !self.input.is_empty() {
            self.input.pop_front();
            self.input_start += 1;
        }
    }
}

/// `path` の音声の `[begin_ms, end_ms)`（`end_ms` が無ければ最後まで）を 16 kHz mono にする。
/// 素材の終わりより後ろは含めない。
pub fn decode_speech(
    path: &Path,
    begin_ms: i64,
    end_ms: Option<i64>,
    cancel: &AtomicBool,
) -> Result<Vec<f32>, MediaError> {
    if begin_ms < 0 || end_ms.is_some_and(|end| end <= begin_ms) {
        return Err(MediaError::new("文字起こしの区間が不正です"));
    }
    let mut input = Input::open(path)?;
    input.find_stream_info(i64::MAX)?;
    let streams = input.streams()?;
    let Some(audio) = streams.iter().find(|stream| stream.is_audio) else {
        return Err(MediaError::new("音声の stream がありません"));
    };
    let stream_index = audio.index;
    input.discard_except(stream_index);
    let mut decoder = Decoder::open(&input, stream_index, i64::MAX, None, 0)?;
    let mut converter = AudioConverter::new(&decoder)?;
    let rate = i64::from(converter.sample_rate());
    if rate <= 0 {
        return Err(MediaError::new("音声の sample rate が不正です"));
    }
    let to_output = |ms: i64| ms * SPEECH_SAMPLE_RATE / 1000;
    let begin_output = to_output(begin_ms);
    let end_output = end_ms.map(to_output);
    let mp3 = input.format_name().split(',').any(|name| name == "mp3");
    if begin_ms > SEEK_LEAD_MS && !mp3 {
        let target = (begin_ms - SEEK_LEAD_MS) * rate / 1000;
        let timestamp = decoder.timestamp_for_sample(converter.sample_rate(), target)?;
        input.seek(stream_index, timestamp)?;
        decoder.flush_buffers();
    }
    let mut resampler = Resampler::new(rate, begin_output);
    let mut produced = Vec::new();
    let mut samples = Vec::new();
    let mut packet = Packet::new()?;
    let mut frame = Frame::new()?;
    let mut invalid_packets = 0;
    let mut started = false;
    let mut finished_early = false;
    let mut collect = |resampler: &mut Resampler, produced: &mut Vec<(i64, f32)>, finish: bool| {
        resampler.drain(produced, finish, end_output);
        for (index, value) in produced.drain(..) {
            if index >= begin_output {
                samples.push(value);
            }
        }
    };
    'read: loop {
        if cancel.load(Ordering::Relaxed) {
            return Err(MediaError::new("文字起こしの準備を取り消しました"));
        }
        let eof = match input.read_packet(&mut packet)? {
            ReadStatus::Eof => {
                decoder.flush()?;
                true
            }
            ReadStatus::Packet => {
                if packet.stream_index() != Some(stream_index) {
                    packet.unref();
                    continue;
                }
                let status = decoder.send(&packet);
                packet.unref();
                match status? {
                    SendStatus::Sent => {}
                    SendStatus::InvalidData if invalid_packets < MAXIMUM_INVALID_PACKETS => {
                        invalid_packets += 1;
                        continue;
                    }
                    SendStatus::InvalidData => {
                        return Err(MediaError::new(format!(
                            "壊れた音声 packet が {MAXIMUM_INVALID_PACKETS} 個を超えました"
                        )));
                    }
                }
                false
            }
        };
        loop {
            match decoder.receive(&mut frame)? {
                ReceiveStatus::Again | ReceiveStatus::Eof => break,
                ReceiveStatus::Frame => {
                    let chunk = converter.convert(&frame)?;
                    frame.unref();
                    let channels = chunk.planes.len().max(1) as f32;
                    let length = chunk.planes.first().map_or(0, Vec::len);
                    let mono: Vec<f32> = (0..length)
                        .map(|index| {
                            chunk
                                .planes
                                .iter()
                                .map(|plane| plane.get(index).copied().unwrap_or(0.0))
                                .sum::<f32>()
                                / channels
                        })
                        .collect();
                    if !started {
                        // seek が区間の開始を越えたら、先頭から読み直す（推定の位置で始めない）。
                        let first_needed =
                            (resampler.input_position(begin_output) - resampler.half_width) as i64;
                        if chunk.start_sample > first_needed.max(0) && begin_ms > 0 {
                            input.seek(stream_index, 0)?;
                            decoder.flush_buffers();
                            converter = AudioConverter::new(&decoder)?;
                            started = true;
                            continue 'read;
                        }
                        started = true;
                    }
                    resampler.push(chunk.start_sample, &mono);
                    collect(&mut resampler, &mut produced, false);
                    if end_output.is_some_and(|end| resampler.next_output >= end) {
                        finished_early = true;
                        break 'read;
                    }
                }
            }
        }
        if eof {
            break;
        }
    }
    if !finished_early {
        collect(&mut resampler, &mut produced, true);
    }
    if samples.is_empty() {
        return Err(MediaError::new("文字起こしの区間に音声がありません"));
    }
    Ok(samples)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::write_pcm16_wav;

    fn tone_file(directory: &Path, rate: u32, seconds: f64, frequency: f64) -> std::path::PathBuf {
        let count = (f64::from(rate) * seconds) as usize;
        let samples: Vec<f32> = (0..count)
            .flat_map(|index| {
                let value = (index as f64 * frequency * std::f64::consts::TAU / f64::from(rate))
                    .sin() as f32
                    * 0.5;
                [value, value]
            })
            .collect();
        let path = directory.join(format!("tone-{rate}.wav"));
        write_pcm16_wav(&path, rate, 2, &samples).unwrap();
        path
    }

    fn rising_crossings(samples: &[f32]) -> usize {
        samples
            .windows(2)
            .filter(|pair| pair[0] < 0.0 && pair[1] >= 0.0)
            .count()
    }

    #[test]
    fn converts_48k_and_44k1_to_16k_with_the_same_pitch_and_length() {
        let directory = std::env::temp_dir().join(format!("mvm-speech-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let cancel = AtomicBool::new(false);
        for rate in [48_000, 44_100] {
            let path = tone_file(&directory, rate, 3.0, 440.0);
            let speech = decode_speech(&path, 0, None, &cancel).unwrap();
            assert_eq!(speech.len(), 48_000, "{rate}: 3 秒は 48000 sample");
            let crossings = rising_crossings(&speech[..16_000]);
            assert!((438..=442).contains(&crossings), "{rate}: {crossings}");
            let peak = speech[1000..15_000]
                .iter()
                .fold(0.0f32, |peak, value| peak.max(value.abs()));
            assert!((peak - 0.5).abs() < 0.02, "{rate}: 振幅 {peak}");
        }
        std::fs::remove_dir_all(&directory).unwrap();
    }

    #[test]
    fn a_later_range_is_cut_at_the_requested_milliseconds() {
        let directory = std::env::temp_dir().join(format!("mvm-speech-r-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        // 前半 3 秒は 440 Hz、後半 3 秒は 880 Hz。3.5〜5.0 秒だけを取り出す。
        let mut samples = Vec::new();
        for index in 0..48_000 * 6 {
            let frequency = if index < 48_000 * 3 { 440.0 } else { 880.0 };
            let value =
                (index as f64 * frequency * std::f64::consts::TAU / 48_000.0).sin() as f32 * 0.5;
            samples.extend([value, value]);
        }
        let path = directory.join("two.wav");
        write_pcm16_wav(&path, 48_000, 2, &samples).unwrap();
        let speech = decode_speech(&path, 3_500, Some(5_000), &AtomicBool::new(false)).unwrap();
        assert_eq!(speech.len(), 24_000);
        let crossings = rising_crossings(&speech[..16_000]);
        assert!((878..=882).contains(&crossings), "{crossings}");
        std::fs::remove_dir_all(&directory).unwrap();
    }

    #[test]
    fn cancel_and_bad_ranges_fail() {
        let directory = std::env::temp_dir().join(format!("mvm-speech-c-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let path = tone_file(&directory, 48_000, 1.0, 440.0);
        assert!(decode_speech(&path, 0, None, &AtomicBool::new(true)).is_err());
        assert!(decode_speech(&path, 500, Some(500), &AtomicBool::new(false)).is_err());
        assert!(decode_speech(&path, 5_000, None, &AtomicBool::new(false)).is_err());
        std::fs::remove_dir_all(&directory).unwrap();
    }
}
