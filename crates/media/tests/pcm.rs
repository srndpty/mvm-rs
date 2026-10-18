//! 48 kHz PCM の読み位置。期待する sample は WAV に書いた整数から独立に決める。

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::Path;

use mvm_media::PcmStream;

#[test]
fn rejects_a_missing_file_and_a_rate_other_than_48khz() {
    let error = open_error(Path::new(""));
    assert!(error.contains("空です"), "{error}");

    let missing = open_error(Path::new("no-such-audio.wav"));
    assert!(missing.contains("開けません"), "{missing}");

    let path = std::env::temp_dir().join(format!("mvm-pcm-44k-{}.wav", std::process::id()));
    write_wav(&path, 44_100, &[[0, 0]]);
    let error = open_error(&path);
    assert!(error.contains("48000"), "{error}");
    std::fs::remove_file(&path).expect("片付け");
}

fn open_error(path: &Path) -> String {
    match PcmStream::open(path) {
        Ok(_) => panic!("{} を開けてしまいました", path.display()),
        Err(error) => error.to_string(),
    }
}

#[test]
fn reads_the_requested_sample_and_silence_past_the_end() {
    let path = std::env::temp_dir().join(format!("mvm-pcm-48k-{}.wav", std::process::id()));
    // sample 0 は無音、sample 3 の左だけ 0.5（s16 の 16384）。
    let mut frames = vec![[0i16, 0], [0, 0], [0, 0], [16384, -16384]];
    frames.extend(std::iter::repeat_n([0, 0], 16));
    write_wav(&path, 48_000, &frames);
    let mut stream = PcmStream::open(&path).expect("wav");
    assert_eq!(stream.sample_rate(), 48_000);

    let mut left = [1f32; 8];
    let mut right = [1f32; 8];
    let mut mismatch = [0f32; 2];
    assert!(stream.copy_from(0, &mut left, &mut mismatch).is_err());
    stream.copy_from(0, &mut left, &mut right).expect("読む");
    assert!(left[0].abs() < 0.001, "先頭 {left:?}");
    assert!((left[3] - 0.5).abs() < 0.001, "左 {}", left[3]);
    assert!((right[3] + 0.5).abs() < 0.001, "右 {}", right[3]);
    assert!(left[7].abs() < 0.001, "終端の外 {}", left[7]);

    stream
        .copy_from(3, &mut left[..1], &mut right[..1])
        .expect("seek");
    assert!((left[0] - 0.5).abs() < 0.001, "seek 後 {}", left[0]);
    stream
        .copy_from(10_000, &mut left[..1], &mut right[..1])
        .expect("終端の外");
    assert!(left[0].abs() < 0.001, "終端の外 {}", left[0]);
    assert!(right[0].abs() < 0.001, "終端の外 {}", right[0]);
    drop(stream);
    std::fs::remove_file(&path).expect("片付け");
}

fn write_wav(path: &Path, sample_rate: u32, frames: &[[i16; 2]]) {
    let data_bytes = frames.len() * 4;
    let mut bytes = Vec::with_capacity(44 + data_bytes);
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&(36 + data_bytes as u32).to_le_bytes());
    bytes.extend_from_slice(b"WAVE");
    bytes.extend_from_slice(b"fmt ");
    bytes.extend_from_slice(&16u32.to_le_bytes());
    bytes.extend_from_slice(&1u16.to_le_bytes());
    bytes.extend_from_slice(&2u16.to_le_bytes());
    bytes.extend_from_slice(&sample_rate.to_le_bytes());
    bytes.extend_from_slice(&(sample_rate * 4).to_le_bytes());
    bytes.extend_from_slice(&4u16.to_le_bytes());
    bytes.extend_from_slice(&16u16.to_le_bytes());
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&(data_bytes as u32).to_le_bytes());
    for frame in frames {
        bytes.extend_from_slice(&frame[0].to_le_bytes());
        bytes.extend_from_slice(&frame[1].to_le_bytes());
    }
    std::fs::write(path, bytes).expect("wav");
}
