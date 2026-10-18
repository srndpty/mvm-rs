//! `dev testmedia` が作る素材で、種別判定と静止画・音声 decode を検査する。

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::PathBuf;

use mvm_media::{
    DecodedImage, ProbedMedia, StillLimits, decode_still_image, decode_waveform, probe_media_file,
};
use mvm_testkit::smoke_dir;

fn import(name: &str) -> PathBuf {
    let path = smoke_dir().join("_import").join(name);
    assert!(
        path.is_file(),
        "検証素材がありません ({}). 先に .\\dev.ps1 testmedia を実行してください",
        path.display()
    );
    path
}

fn image(name: &str) -> DecodedImage {
    decode_still_image(&import(name), &StillLimits::default()).expect(name)
}

#[test]
#[ignore = "workstation"]
fn stills_decode_to_the_expected_size() {
    for name in [
        "png_rgb24.png",
        "bmp_24.bmp",
        "tga_24.tga",
        "qoi_rgb.qoi",
        "tiff_rgb48.tif",
        "webp_static.webp",
        "gif_static.gif",
    ] {
        let decoded = image(name);
        assert_eq!((decoded.width, decoded.height), (64, 48), "{name}");
    }
    let quadrant = image("jpg_quadrant.jpg");
    assert_eq!((quadrant.width, quadrant.height), (64, 32));
    let pal = image("png_pal8_alpha.png");
    assert_eq!((pal.width, pal.height), (64, 32));
    assert!(
        pal.rgba
            .as_chunks::<4>()
            .0
            .iter()
            .any(|pixel| pixel[3] < 255)
    );
    let alpha = decode_still_image(&smoke_dir().join("png_alpha.png"), &StillLimits::default())
        .expect("png_alpha");
    assert_eq!((alpha.width, alpha.height), (512, 512));
    assert!(
        alpha
            .rgba
            .as_chunks::<4>()
            .0
            .iter()
            .any(|pixel| pixel[3] < 10)
    );
    assert!(
        alpha
            .rgba
            .as_chunks::<4>()
            .0
            .iter()
            .any(|pixel| pixel[3] > 250)
    );
}

#[test]
#[ignore = "workstation"]
fn exif_orientation_6_swaps_the_axes() {
    let upright = image("jpg_exif_orient1.jpg");
    assert_eq!(
        (upright.width, upright.height, upright.orientation),
        (64, 32, 1)
    );
    let turned = image("jpg_exif_orient6.jpg");
    assert_eq!(
        (turned.width, turned.height, turned.orientation),
        (32, 64, 6)
    );
    let named = decode_still_image(
        &smoke_dir().join("_import").join("写真　縦向き＆EXIF.jpg"),
        &StillLimits::default(),
    )
    .expect("日本語パス");
    assert_eq!((named.width, named.height), (32, 64));
    // orientation 6 は 90 度時計回り。元画像の左上の赤は、回転後の (16, 0) 付近に来る。
    let red = pixel(&turned.rgba, turned.width, 16, 0);
    let blue = pixel(&turned.rgba, turned.width, 0, 0);
    assert!(red[0] > 180 && red[2] < 80, "赤であるべき画素: {red:?}");
    assert!(blue[2] > 180 && blue[0] < 80, "青であるべき画素: {blue:?}");
}

#[test]
#[ignore = "workstation"]
fn icc_accepts_srgb_and_rejects_other_primaries() {
    let srgb = image("png_icc_srgb.png");
    assert_eq!((srgb.width, srgb.height), (64, 48));
    let p3 = decode_still_image(&import("png_icc_display_p3.png"), &StillLimits::default());
    assert!(p3.expect_err("Display P3").message().contains("ICC"));
    let fake = decode_still_image(&import("png_icc_fake_srgb.png"), &StillLimits::default());
    assert!(fake.expect_err("偽の sRGB").message().contains("原色"));
}

#[test]
#[ignore = "workstation"]
fn animated_hdr_alpha_and_corrupt_files_are_rejected() {
    for name in [
        "gif_animated.gif",
        "apng_animated.png",
        "webp_animated.webp",
    ] {
        let error = probe_media_file(&import(name)).expect_err(name);
        assert!(
            error.message().contains("アニメーション"),
            "{name}: {error}"
        );
    }
    let hdr = probe_media_file(&import("exr_float.exr")).expect_err("exr");
    assert!(hdr.message().contains("HDR"), "{hdr}");
    for name in ["vp9_alpha.webm", "prores4444_alpha.mov"] {
        let error = probe_media_file(&import(name)).expect_err(name);
        assert!(error.message().contains("アルファ"), "{name}: {error}");
    }
    let corrupt =
        probe_media_file(&smoke_dir().join("_corrupt").join("text.mp4")).expect_err("text");
    assert!(corrupt.message().contains("解析できません"), "{corrupt}");
}

#[test]
#[ignore = "workstation"]
fn audio_and_short_video_probe_to_fixed_facts() {
    for name in [
        "opus_48k.opus",
        "vorbis_48k.ogg",
        "mp3_cover.mp3",
        "m4a_cover.m4a",
        "flac_cover.flac",
    ] {
        match probe_media_file(&import(name)).expect(name) {
            ProbedMedia::Audio(audio) => {
                assert_eq!(audio.sample_rate, 48000, "{name}");
                assert!(
                    audio.duration_samples > 48_000 * 9 / 10
                        && audio.duration_samples < 48_000 * 12 / 10,
                    "{name}: {}",
                    audio.duration_samples
                );
            }
            other => panic!("{name} は音声です: {other:?}"),
        }
    }
    match probe_media_file(&import("mjpeg_av.avi")).expect("mjpeg") {
        ProbedMedia::Video(video) => {
            assert_eq!(video.frame_count, 10);
            assert!(video.has_audio);
            assert_eq!(video.width, 64);
        }
        other => panic!("mjpeg は動画です: {other:?}"),
    }
    match probe_media_file(&import("mp4_h264_with_cover.mp4")).expect("cover video") {
        ProbedMedia::Video(video) => {
            assert_eq!(video.frame_count, 10);
            assert!(!video.has_audio);
            assert_eq!(video.width, 64);
        }
        other => panic!("カバー付き mp4 は動画です: {other:?}"),
    }
    let peaks = decode_waveform(&smoke_dir().join("wav_48k.wav"), None).expect("波形");
    assert_eq!(peaks.sample_rate, 48000);
    assert_eq!(peaks.channels, 2);
    assert!(!peaks.levels.is_empty());
    assert!(!peaks.levels[0].minimum.is_empty());
}

fn pixel(rgba: &[u8], width: i32, x: i32, y: i32) -> [u8; 4] {
    let index = ((y * width + x) * 4) as usize;
    rgba[index..index + 4].try_into().unwrap()
}
