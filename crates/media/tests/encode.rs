//! MP4 の書き出し。期待値は書いた frame 数・sample 数と、正弦波の式から独立に決める。

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::f32::consts::TAU;
use std::path::{Path, PathBuf};

use mvm_core::Rational;
use mvm_media::{MAX_CRF, Mp4Settings, Mp4Writer, PcmStream, ProbedMedia, probe_media_file};

const WIDTH: u32 = 64;
const HEIGHT: u32 = 48;
const FRAMES: usize = 30;
/// 60 fps の 1 frame は 48 kHz の 800 sample。
const SAMPLES_PER_FRAME: usize = 800;
const TONE_HZ: f32 = 1000.0;
const AMPLITUDE: f32 = 0.5;

fn temp_path(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!("mvm-encode-{}-{name}", std::process::id()))
}

fn settings(audio: bool) -> Mp4Settings {
    Mp4Settings {
        width: WIDTH,
        height: HEIGHT,
        fps: Rational::new(60, 1),
        crf: 18,
        audio,
    }
}

fn solid(value: u8) -> Vec<u8> {
    let mut rgba = vec![value; (WIDTH * HEIGHT * 4) as usize];
    for pixel in rgba.as_chunks_mut::<4>().0 {
        pixel[3] = 255;
    }
    rgba
}

#[test]
fn writes_every_frame_and_sample_and_reads_back_as_the_same_video() {
    let path = temp_path("av.mp4");
    let mut writer = Mp4Writer::create(&path, &settings(true)).expect("作成");
    let mut written_samples = 0usize;
    for frame in 0..FRAMES {
        writer.write_frame(&solid((frame * 8) as u8)).expect("映像");
        // 左だけに 1 kHz、右は無音。左右が入れ替わると右の RMS が大きくなる。
        let left: Vec<f32> = (0..SAMPLES_PER_FRAME)
            .map(|index| {
                let t = (written_samples + index) as f32 / 48_000.0;
                AMPLITUDE * (TAU * TONE_HZ * t).sin()
            })
            .collect();
        let right = vec![0f32; SAMPLES_PER_FRAME];
        writer.write_audio(&left, &right).expect("音声");
        written_samples += SAMPLES_PER_FRAME;
    }
    let summary = writer.finish().expect("閉じる");
    assert_eq!(summary.video_frames, FRAMES as i64);
    assert_eq!(summary.audio_samples, (FRAMES * SAMPLES_PER_FRAME) as i64);

    let ProbedMedia::Video(video) = probe_media_file(&path).expect("probe") else {
        panic!("映像として読めません");
    };
    assert_eq!((video.width, video.height), (WIDTH as i32, HEIGHT as i32));
    assert_eq!((video.fps_num, video.fps_den), (60, 1));
    assert_eq!(video.frame_count, FRAMES as i64);
    assert!(video.has_audio, "音声の stream がありません");

    // AAC の先頭と末尾は encoder の遅延で崩れるので、中央の 0.25 秒だけを比べる。
    let mut stream = PcmStream::open(&path).expect("音声を開く");
    let mut left = vec![0f32; 12_000];
    let mut right = vec![0f32; 12_000];
    stream
        .copy_from(6_000, &mut left, &mut right)
        .expect("読む");
    let expected_rms = AMPLITUDE / 2f32.sqrt();
    let left_rms = rms(&left);
    assert!(
        (left_rms - expected_rms).abs() < expected_rms * 0.05,
        "左の RMS {left_rms}（期待 {expected_rms}）"
    );
    assert!(rms(&right) < 0.01, "右に音が漏れています: {}", rms(&right));
    // 0.25 秒の 1 kHz は 500 回符号が変わる。
    let crossings = left
        .windows(2)
        .filter(|pair| (pair[0] < 0.0) != (pair[1] < 0.0))
        .count();
    assert!(crossings.abs_diff(500) <= 4, "符号の変化 {crossings} 回");
    drop(stream);
    std::fs::remove_file(&path).expect("片付け");
}

#[test]
fn a_video_only_file_has_no_audio_stream() {
    let path = temp_path("video-only.mp4");
    let mut writer = Mp4Writer::create(&path, &settings(false)).expect("作成");
    for _ in 0..3 {
        writer.write_frame(&solid(40)).expect("映像");
    }
    assert!(
        writer.write_audio(&[0.0], &[0.0]).is_err(),
        "音声の無い出力に音声を書けてしまいました"
    );
    writer.finish().expect("閉じる");
    let ProbedMedia::Video(video) = probe_media_file(&path).expect("probe") else {
        panic!("映像として読めません");
    };
    assert_eq!(video.frame_count, 3);
    assert!(!video.has_audio, "音声の stream があります");
    std::fs::remove_file(&path).expect("片付け");
}

#[test]
fn rejects_bad_settings_and_frames_of_the_wrong_size() {
    let path = temp_path("bad.mp4");
    let mut bad = settings(false);
    bad.crf = MAX_CRF + 1;
    assert!(create_error(&path, &bad).contains("CRF"));
    let mut odd = settings(false);
    odd.width = WIDTH + 1;
    assert!(create_error(&path, &odd).contains("寸法"));
    let mut fps = settings(false);
    fps.fps = Rational::new(0, 1);
    assert!(create_error(&path, &fps).contains("fps"));

    let mut writer = Mp4Writer::create(&path, &settings(true)).expect("作成");
    let error = writer.write_frame(&[0u8; 16]).unwrap_err().to_string();
    assert!(error.contains("画素数"), "{error}");
    assert!(writer.write_audio(&[0.0, 0.0], &[0.0]).is_err());
    drop(writer);
    let _ = std::fs::remove_file(&path);
}

fn create_error(path: &Path, settings: &Mp4Settings) -> String {
    match Mp4Writer::create(path, settings) {
        Ok(_) => panic!("不正な設定で書き始めてしまいました: {settings:?}"),
        Err(error) => error.to_string(),
    }
}

fn rms(samples: &[f32]) -> f32 {
    (samples.iter().map(|sample| sample * sample).sum::<f32>() / samples.len() as f32).sqrt()
}
