//! 48 kHz の音声を、指定した sample から float32 で読む。
//!
//! sample rate が 48 kHz でない素材は失敗させる。channel が 3 以上のときは
//! 波形と同じく stereo へ downmix し、mono は左右へ複製する。resample はしない。

use std::path::Path;

use mvm_core::output_timebase::QUALIFIED_AUDIO_SAMPLE_RATE;

use crate::error::MediaError;
use crate::ffi::{
    AudioConverter, Decoder, Frame, Input, Packet, ReadStatus, ReceiveStatus, SendStatus,
};

const MAXIMUM_INVALID_PACKETS: u32 = 32;

pub struct PcmStream {
    input: Input,
    stream_index: usize,
    decoder: Decoder,
    converter: AudioConverter,
    packet: Packet,
    frame: Frame,
    cache_start: i64,
    left: Vec<f32>,
    right: Vec<f32>,
    eof: bool,
    invalid_packets: u32,
}

impl PcmStream {
    pub fn open(path: &Path) -> Result<Self, MediaError> {
        if path.as_os_str().is_empty() {
            return Err(MediaError::new("音声入力の path が空です"));
        }
        let mut input = Input::open(path)?;
        input.find_stream_info(i64::MAX)?;
        let streams = input.streams()?;
        let Some(audio) = streams.iter().find(|stream| stream.is_audio) else {
            return Err(MediaError::new("音声の stream がありません"));
        };
        if i64::from(audio.sample_rate) != QUALIFIED_AUDIO_SAMPLE_RATE {
            return Err(MediaError::new(format!(
                "音声の sample rate が 48000 Hz ではありません: {}",
                audio.sample_rate
            )));
        }
        let stream_index = audio.index;
        input.discard_except(stream_index);
        let decoder = Decoder::open(&input, stream_index, i64::MAX, None, 0)?;
        let converter = AudioConverter::new(&decoder)?;
        if i64::from(converter.sample_rate()) != QUALIFIED_AUDIO_SAMPLE_RATE {
            return Err(MediaError::new(format!(
                "音声の sample rate が 48000 Hz ではありません: {}",
                converter.sample_rate()
            )));
        }
        Ok(Self {
            input,
            stream_index,
            decoder,
            converter,
            packet: Packet::new()?,
            frame: Frame::new()?,
            cache_start: 0,
            left: Vec::new(),
            right: Vec::new(),
            eof: false,
            invalid_packets: 0,
        })
    }

    pub fn sample_rate(&self) -> i32 {
        self.converter.sample_rate()
    }

    /// `start` から `left.len()` sample を書く。ファイルの外は 0。左右の長さが違うときは失敗する。
    pub fn copy_from(
        &mut self,
        start: i64,
        left: &mut [f32],
        right: &mut [f32],
    ) -> Result<(), MediaError> {
        if left.len() != right.len() {
            return Err(MediaError::new("音声 buffer の左右の長さが違います"));
        }
        if start < 0 {
            return Err(MediaError::new("sample 位置が不正です"));
        }
        if left.is_empty() {
            return Ok(());
        }
        self.ensure(start, left.len())?;
        for (index, (left_slot, right_slot)) in left.iter_mut().zip(right.iter_mut()).enumerate() {
            let Some(sample) = start.checked_add(
                i64::try_from(index).map_err(|_| MediaError::new("sample 位置が不正です"))?,
            ) else {
                return Err(MediaError::new("sample 位置が overflow しました"));
            };
            let Some(offset) = sample.checked_sub(self.cache_start) else {
                *left_slot = 0.0;
                *right_slot = 0.0;
                continue;
            };
            let Ok(offset) = usize::try_from(offset) else {
                *left_slot = 0.0;
                *right_slot = 0.0;
                continue;
            };
            *left_slot = self.left.get(offset).copied().unwrap_or(0.0);
            *right_slot = self.right.get(offset).copied().unwrap_or(0.0);
        }
        Ok(())
    }

    fn ensure(&mut self, start: i64, count: usize) -> Result<(), MediaError> {
        let Some(end) = start.checked_add(i64::try_from(count).unwrap_or(i64::MAX)) else {
            return Err(MediaError::new("sample 位置が overflow しました"));
        };
        let cache_end = self
            .cache_start
            .saturating_add(i64::try_from(self.left.len()).unwrap_or(i64::MAX));
        if self.left.is_empty() || start < self.cache_start || start > cache_end {
            self.seek(start)?;
        }
        while self
            .cache_start
            .saturating_add(i64::try_from(self.left.len()).unwrap_or(i64::MAX))
            < end
            && !self.eof
        {
            let before = self.left.len();
            self.decode_some()?;
            if self.left.len() == before && !self.eof {
                return Err(MediaError::new("音声を先へ読めません"));
            }
        }
        self.discard_before(start);
        Ok(())
    }

    fn seek(&mut self, start: i64) -> Result<(), MediaError> {
        let timestamp = self
            .decoder
            .timestamp_for_sample(self.converter.sample_rate(), start)?;
        self.input.seek(self.stream_index, timestamp)?;
        self.decoder.flush_buffers();
        self.left.clear();
        self.right.clear();
        self.cache_start = start;
        self.eof = false;
        self.invalid_packets = 0;
        Ok(())
    }

    fn decode_some(&mut self) -> Result<(), MediaError> {
        if self.eof {
            return Ok(());
        }
        match self.input.read_packet(&mut self.packet)? {
            ReadStatus::Eof => {
                self.decoder.flush()?;
                self.drain()?;
                self.eof = true;
            }
            ReadStatus::Packet => {
                if self.packet.stream_index() != Some(self.stream_index) {
                    self.packet.unref();
                    return Ok(());
                }
                let status = self.decoder.send(&self.packet);
                self.packet.unref();
                match status? {
                    SendStatus::Sent => self.drain()?,
                    SendStatus::InvalidData if self.invalid_packets < MAXIMUM_INVALID_PACKETS => {
                        self.invalid_packets += 1;
                    }
                    SendStatus::InvalidData => {
                        return Err(MediaError::new(format!(
                            "壊れた音声 packet が {MAXIMUM_INVALID_PACKETS} 個を超えました"
                        )));
                    }
                }
            }
        }
        Ok(())
    }

    fn drain(&mut self) -> Result<(), MediaError> {
        loop {
            match self.decoder.receive(&mut self.frame)? {
                ReceiveStatus::Again | ReceiveStatus::Eof => return Ok(()),
                ReceiveStatus::Frame => {
                    let chunk = self.converter.convert(&self.frame)?;
                    self.frame.unref();
                    self.append(chunk.start_sample, &chunk.planes)?;
                }
            }
        }
    }

    fn append(&mut self, mut start: i64, planes: &[Vec<f32>]) -> Result<(), MediaError> {
        let Some(source) = planes.first() else {
            return Err(MediaError::new("音声 frame に channel がありません"));
        };
        if self.left.is_empty() {
            self.cache_start = start;
        }
        let cache_end = self
            .cache_start
            .saturating_add(i64::try_from(self.left.len()).unwrap_or(i64::MAX));
        if start > cache_end {
            let gap = usize::try_from(start - cache_end)
                .map_err(|_| MediaError::new("音声の隙間が大きすぎます"))?;
            self.left.extend(std::iter::repeat_n(0.0, gap));
            self.right.extend(std::iter::repeat_n(0.0, gap));
        }
        let skip = if start < cache_end {
            let skip = usize::try_from(cache_end - start)
                .map_err(|_| MediaError::new("音声 sample の位置が不正です"))?;
            start = cache_end;
            skip
        } else {
            0
        };
        if skip >= source.len() {
            return Ok(());
        }
        self.left.extend_from_slice(&source[skip..]);
        if let Some(right) = planes.get(1) {
            if right.len() != source.len() {
                return Err(MediaError::new("音声の左右の sample 数が違います"));
            }
            self.right.extend_from_slice(&right[skip..]);
        } else {
            self.right.extend_from_slice(&source[skip..]);
        }
        let _ = start;
        Ok(())
    }

    fn discard_before(&mut self, sample: i64) {
        if sample <= self.cache_start {
            return;
        }
        let Ok(drop_count) = usize::try_from(sample - self.cache_start) else {
            return;
        };
        if drop_count >= self.left.len() {
            self.left.clear();
            self.right.clear();
            self.cache_start = sample;
            return;
        }
        self.left.drain(..drop_count);
        self.right.drain(..drop_count);
        self.cache_start = sample;
    }
}
