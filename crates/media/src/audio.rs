//! 音声を頭から decode し、表示用の waveform peak を作る。
//! sample rate は素材のまま。3 channel 以上は stereo へ downmix する。

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use mvm_core::waveform_peaks::{
    MAXIMUM_WAVEFORM_WORKING_BYTES, WaveformPeakBuilder, waveform_samples_per_peak,
};

use crate::error::MediaError;
use crate::ffi::{
    AudioConverter, Decoder, Frame, Input, Packet, ReadStatus, ReceiveStatus, SendStatus,
};

/// 波形は表示用なので、少数の壊れた packet は読み飛ばす。これを超えたら素材の破損として失敗させる。
const MAXIMUM_INVALID_PACKETS: u32 = 32;

pub fn decode_waveform(
    path: &Path,
    cancel: Option<&AtomicBool>,
) -> Result<mvm_core::waveform_peaks::WaveformPeaks, MediaError> {
    if path.as_os_str().is_empty() {
        return Err(MediaError::new("音声入力の path が空です"));
    }
    let mut input = Input::open(path)?;
    input.find_stream_info(i64::MAX)?;
    let streams = input.streams()?;
    let Some(audio) = streams.iter().find(|stream| stream.is_audio) else {
        return Err(MediaError::new("音声の stream がありません"));
    };
    input.discard_except(audio.index);
    let mut decoder = Decoder::open(&input, audio.index, i64::MAX, None, 0)?;
    let mut converter = AudioConverter::new(&decoder)?;
    let mut builder = WaveformPeakBuilder::new(
        converter.sample_rate(),
        i32::try_from(converter.channels()).map_err(|_| MediaError::new("channel 数が不正です"))?,
        waveform_samples_per_peak(converter.sample_rate()),
        MAXIMUM_WAVEFORM_WORKING_BYTES,
    )
    .map_err(|error| MediaError::new(error.to_string()))?;
    let mut packet = Packet::new()?;
    let mut frame = Frame::new()?;
    let mut invalid_packets = 0;
    loop {
        if cancel.is_some_and(|flag| flag.load(Ordering::Relaxed)) {
            return Err(MediaError::new("波形の生成を中断しました"));
        }
        match input.read_packet(&mut packet)? {
            ReadStatus::Eof => break,
            ReadStatus::Packet => {
                if packet.stream_index() != Some(audio.index) {
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
                drain(&mut decoder, &mut converter, &mut frame, &mut builder)?;
            }
        }
    }
    decoder.flush()?;
    drain(&mut decoder, &mut converter, &mut frame, &mut builder)?;
    builder
        .finish()
        .map_err(|error| MediaError::new(error.to_string()))
}

fn drain(
    decoder: &mut Decoder,
    converter: &mut AudioConverter,
    frame: &mut Frame,
    builder: &mut WaveformPeakBuilder,
) -> Result<(), MediaError> {
    loop {
        match decoder.receive(frame)? {
            ReceiveStatus::Again | ReceiveStatus::Eof => return Ok(()),
            ReceiveStatus::Frame => {
                let chunk = converter.convert(frame)?;
                frame.unref();
                let borrowed: Vec<&[f32]> = chunk.planes.iter().map(Vec::as_slice).collect();
                builder
                    .add_planar(chunk.start_sample, &borrowed)
                    .map_err(|error| MediaError::new(error.to_string()))?;
            }
        }
    }
}
