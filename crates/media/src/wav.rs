//! 16 bit PCM の WAV を書く。文字起こしの入力（16 kHz mono）と試験素材に使う。

use std::path::Path;

use crate::error::MediaError;

/// interleaved の float（-1..1、範囲外は飽和）を 16 bit PCM の WAV にして `path` へ書く。
pub fn write_pcm16_wav(
    path: &Path,
    sample_rate: u32,
    channels: u16,
    interleaved: &[f32],
) -> Result<(), MediaError> {
    if sample_rate == 0 || channels == 0 || !interleaved.len().is_multiple_of(usize::from(channels))
    {
        return Err(MediaError::new("WAV の形式が不正です"));
    }
    let data_bytes = interleaved
        .len()
        .checked_mul(2)
        .and_then(|bytes| u32::try_from(bytes).ok())
        .ok_or_else(|| MediaError::new("WAV が大きすぎます"))?;
    let block_align = channels
        .checked_mul(2)
        .ok_or_else(|| MediaError::new("WAV の channel 数が多すぎます"))?;
    let byte_rate = sample_rate
        .checked_mul(u32::from(block_align))
        .ok_or_else(|| MediaError::new("WAV の sample rate が大きすぎます"))?;
    let riff_bytes = data_bytes
        .checked_add(36)
        .ok_or_else(|| MediaError::new("WAV が大きすぎます"))?;
    let mut bytes = Vec::with_capacity(data_bytes as usize + 44);
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&riff_bytes.to_le_bytes());
    bytes.extend_from_slice(b"WAVEfmt ");
    bytes.extend_from_slice(&16u32.to_le_bytes());
    bytes.extend_from_slice(&1u16.to_le_bytes());
    bytes.extend_from_slice(&channels.to_le_bytes());
    bytes.extend_from_slice(&sample_rate.to_le_bytes());
    bytes.extend_from_slice(&byte_rate.to_le_bytes());
    bytes.extend_from_slice(&block_align.to_le_bytes());
    bytes.extend_from_slice(&16u16.to_le_bytes());
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&data_bytes.to_le_bytes());
    for sample in interleaved {
        let value = (sample.clamp(-1.0, 1.0) * 32767.0).round() as i16;
        bytes.extend_from_slice(&value.to_le_bytes());
    }
    std::fs::write(path, bytes)
        .map_err(|error| MediaError::new(format!("WAV を書けません ({}): {error}", path.display())))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::PcmStream;

    #[test]
    fn written_stereo_wav_reads_back_through_the_decoder() {
        let directory = std::env::temp_dir().join(format!("mvm-wav-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("ramp.wav");
        let samples: Vec<f32> = (0..4800)
            .flat_map(|index| {
                let value = index as f32 / 4800.0;
                [value, -value]
            })
            .collect();
        write_pcm16_wav(&path, 48_000, 2, &samples).unwrap();
        let mut stream = PcmStream::open(&path).unwrap();
        let mut left = vec![0f32; 4];
        let mut right = vec![0f32; 4];
        stream.copy_from(2400, &mut left, &mut right).unwrap();
        assert!((left[0] - 0.5).abs() < 1e-3, "{left:?}");
        assert!((right[0] + 0.5).abs() < 1e-3, "{right:?}");
        drop(stream);
        std::fs::remove_dir_all(&directory).unwrap();
    }

    #[test]
    fn rejects_a_ragged_channel_count() {
        let path = std::env::temp_dir().join("mvm-never-written.wav");
        assert!(write_pcm16_wav(&path, 48_000, 2, &[0.0, 0.0, 0.0]).is_err());
        assert!(write_pcm16_wav(&path, 0, 1, &[0.0]).is_err());
        assert!(!path.exists());
    }
}
