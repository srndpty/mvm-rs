//! 素材を静止画・時間を持つ素材・拒否のどれにするか。拡張子は見ない。
//!
//! FFmpeg が開けないアニメーション WebP もあるので、開けたかどうかより先に animation flag を見る。

use crate::error::MediaError;

const STILL_CODECS: &[&str] = &[
    "png",
    "apng",
    "mjpeg",
    "jpegls",
    "jpeg2000",
    "bmp",
    "gif",
    "webp",
    "tiff",
    "targa",
    "pcx",
    "qoi",
    "psd",
    "sgi",
    "dds",
    "ppm",
    "pgm",
    "pgmyuv",
    "pbm",
    "pam",
    "jpegxl",
    "jpegxl_anim",
    "xbm",
    "xpm",
    "sunrast",
];

const HDR_CODECS: &[&str] = &["exr", "hdr"];
const IMAGE_CONTAINERS: &[&str] = &["image2", "gif", "apng", "jpegxl_anim"];

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MediaStreamFacts {
    pub ok: bool,
    pub error: String,
    pub format_name: String,
    /// attached picture（カバーアート）を除く。
    pub video_stream_count: i32,
    pub attached_picture_count: i32,
    pub audio_stream_count: i32,
    pub video_codec_name: String,
    /// 画素形式に alpha がある、または webm の alpha_mode が 1。
    pub video_alpha_capable: bool,
    pub still_image_codec: bool,
    pub hdr_image_codec: bool,
    /// still image codec の video packet 数。2 で打ち切る。
    pub image_packet_count_up_to_2: i32,
    pub webp_animation_flag: bool,
    pub width: i32,
    pub height: i32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MediaRoute {
    StillImage,
    TimeBased,
    Rejected,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RouteDecision {
    pub route: MediaRoute,
    pub error: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CodecKind {
    Still,
    Hdr,
    Other,
}

/// 単一画像の器に入った静止画 codec か。同じ codec でも avi などの動画の器なら動画。
pub fn image_codec_kind(codec_name: &str, format_name: &str) -> CodecKind {
    if HDR_CODECS.contains(&codec_name) {
        return CodecKind::Hdr;
    }
    if image_container(format_name) && STILL_CODECS.contains(&codec_name) {
        CodecKind::Still
    } else {
        CodecKind::Other
    }
}

fn image_container(format_name: &str) -> bool {
    format_name.ends_with("_pipe") || IMAGE_CONTAINERS.contains(&format_name)
}

/// RIFF/WEBP の先頭 chunk が VP8X で、animation bit (0x02) が立っているか。
pub fn webp_animation_flag(header: &[u8]) -> bool {
    if header.len() < 21 {
        return false;
    }
    if &header[0..4] != b"RIFF" || &header[8..12] != b"WEBP" || &header[12..16] != b"VP8X" {
        return false;
    }
    let payload = u32::from(header[16])
        | u32::from(header[17]) << 8
        | u32::from(header[18]) << 16
        | u32::from(header[19]) << 24;
    payload >= 10 && (header[20] & 0x02) != 0
}

pub fn route_media(facts: &MediaStreamFacts) -> RouteDecision {
    if facts.webp_animation_flag
        || (facts.still_image_codec && facts.image_packet_count_up_to_2 >= 2)
    {
        return RouteDecision {
            route: MediaRoute::Rejected,
            error: "アニメーション画像 (GIF / WebP / APNG) には対応していません。動画 (mp4 など) へ変換してから読み込んでください"
                .to_owned(),
        };
    }
    if !facts.ok {
        return RouteDecision {
            route: MediaRoute::Rejected,
            error: format!("素材を解析できません: {}", facts.error),
        };
    }
    if facts.hdr_image_codec {
        return RouteDecision {
            route: MediaRoute::Rejected,
            error: format!(
                "HDR 画像 (EXR / Radiance HDR) には対応していません: {}",
                facts.video_codec_name
            ),
        };
    }
    if facts.still_image_codec {
        if facts.image_packet_count_up_to_2 == 0 {
            return RouteDecision {
                route: MediaRoute::Rejected,
                error: "画像のデータがありません".to_owned(),
            };
        }
        return RouteDecision {
            route: MediaRoute::StillImage,
            error: String::new(),
        };
    }
    if facts.video_stream_count == 0 && facts.audio_stream_count == 0 {
        return RouteDecision {
            route: MediaRoute::Rejected,
            error: "映像も音声も持たない素材です".to_owned(),
        };
    }
    RouteDecision {
        route: MediaRoute::TimeBased,
        error: String::new(),
    }
}

#[derive(Clone, Copy, Debug)]
pub struct VideoFacts {
    pub width: i32,
    pub height: i32,
    pub fps_num: i32,
    pub fps_den: i32,
    pub frame_count: i64,
    pub duration_sec: f64,
    pub unbounded: bool,
    pub has_audio: bool,
    pub sar_num: i32,
    pub sar_den: i32,
    pub alpha: bool,
    pub pixel_format: PixelFormatClass,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ClassifiedVideo {
    pub width: i32,
    pub height: i32,
    pub fps_num: i32,
    pub fps_den: i32,
    pub frame_count: i64,
    pub has_audio: bool,
    pub sar_num: i32,
    pub sar_den: i32,
    pub duration_sec: f64,
}

pub fn classify_video(video: &VideoFacts) -> Result<ClassifiedVideo, MediaError> {
    if video.width <= 0 || video.height <= 0 {
        return Err(MediaError::new("映像の解像度を取得できません"));
    }
    if video.unbounded || video.frame_count <= 0 || video.fps_num <= 0 || video.fps_den <= 0 {
        return Err(MediaError::new("有限尺と有効なFPSを持つ動画ではありません"));
    }
    if video.frame_count == 1 {
        return Err(MediaError::new("1 frame だけの動画は読み込めません"));
    }
    if video.alpha {
        return Err(MediaError::new(
            "透過 (アルファ) のある動画には対応していません",
        ));
    }
    ensure_importable_video(video.pixel_format)?;
    let fps = reduce(video.fps_num, video.fps_den);
    let (sar_num, sar_den) = if video.sar_num > 0 && video.sar_den > 0 {
        reduce(video.sar_num, video.sar_den)
    } else {
        (1, 1)
    };
    Ok(ClassifiedVideo {
        width: video.width,
        height: video.height,
        fps_num: fps.0,
        fps_den: fps.1,
        frame_count: video.frame_count,
        has_audio: video.has_audio,
        sar_num,
        sar_den,
        duration_sec: video.duration_sec,
    })
}

#[derive(Clone, Copy, Debug)]
pub struct AudioFacts {
    pub sample_rate: i32,
    pub duration_sec: f64,
    pub unbounded: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ClassifiedAudio {
    pub sample_rate: i32,
    pub duration_samples: i64,
}

pub fn classify_audio(audio: &AudioFacts) -> Result<ClassifiedAudio, MediaError> {
    if audio.unbounded || !audio.duration_sec.is_finite() || audio.duration_sec <= 0.0 {
        return Err(MediaError::new("有限の尺を持つ音声素材ではありません"));
    }
    if audio.sample_rate <= 0 {
        return Err(MediaError::new("音声の sample rate を取得できません"));
    }
    let exact = audio.duration_sec * f64::from(audio.sample_rate);
    if !exact.is_finite() || exact >= 2f64.powi(63) {
        return Err(MediaError::new("音声の尺が扱える範囲を超えています"));
    }
    let samples = exact.round() as i64;
    if samples <= 0 {
        return Err(MediaError::new("音声の尺が 1 sample に満たない素材です"));
    }
    Ok(ClassifiedAudio {
        sample_rate: audio.sample_rate,
        duration_samples: samples,
    })
}

/// D3D12VA が NV12 / P010 にできる画素形式か。header から分からないときは [`PixelFormatClass::Unknown`]。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PixelFormatClass {
    Unknown,
    Yuv420Bit8,
    Yuv420Bit10,
    Unsupported,
}

pub fn pixel_format_class(bit_depth: i32, yuv420: bool, known: bool) -> PixelFormatClass {
    if !known {
        return PixelFormatClass::Unknown;
    }
    match (yuv420, bit_depth) {
        (true, 8) => PixelFormatClass::Yuv420Bit8,
        (true, 10) => PixelFormatClass::Yuv420Bit10,
        _ => PixelFormatClass::Unsupported,
    }
}

pub fn ensure_importable_video(format: PixelFormatClass) -> Result<(), MediaError> {
    if format == PixelFormatClass::Unsupported {
        Err(MediaError::new(
            "8bit または 10bit の 4:2:0 以外の映像には対応していません",
        ))
    } else {
        Ok(())
    }
}

fn reduce(num: i32, den: i32) -> (i32, i32) {
    let divisor = gcd(num, den).max(1);
    (num / divisor, den / divisor)
}

fn gcd(mut a: i32, mut b: i32) -> i32 {
    while b != 0 {
        let next = a % b;
        a = b;
        b = next;
    }
    a.abs()
}

#[cfg(test)]
#[allow(clippy::panic, clippy::unwrap_used)]
mod tests {
    use super::*;

    fn facts() -> MediaStreamFacts {
        MediaStreamFacts {
            ok: true,
            still_image_codec: true,
            image_packet_count_up_to_2: 1,
            video_codec_name: "png".to_owned(),
            ..MediaStreamFacts::default()
        }
    }

    #[test]
    fn routes_a_single_still_and_rejects_animation_and_hdr() {
        assert_eq!(route_media(&facts()).route, MediaRoute::StillImage);
        let mut animated = facts();
        animated.image_packet_count_up_to_2 = 2;
        assert!(route_media(&animated).error.contains("アニメーション"));
        let webp = MediaStreamFacts {
            webp_animation_flag: true,
            ..MediaStreamFacts::default()
        };
        assert!(route_media(&webp).error.contains("アニメーション"));
        let mut hdr = facts();
        hdr.still_image_codec = false;
        hdr.hdr_image_codec = true;
        hdr.video_codec_name = "exr".to_owned();
        assert!(route_media(&hdr).error.contains("HDR"));
    }

    #[test]
    fn routes_time_based_media_and_rejects_an_empty_file() {
        let mut video = MediaStreamFacts {
            ok: true,
            video_stream_count: 1,
            ..MediaStreamFacts::default()
        };
        assert_eq!(route_media(&video).route, MediaRoute::TimeBased);
        video.video_stream_count = 0;
        video.audio_stream_count = 1;
        assert_eq!(route_media(&video).route, MediaRoute::TimeBased);
        video.audio_stream_count = 0;
        assert!(route_media(&video).error.contains("映像も音声も"));
        let broken = MediaStreamFacts {
            error: "開けません".to_owned(),
            ..MediaStreamFacts::default()
        };
        assert!(route_media(&broken).error.contains("解析できません"));
    }

    #[test]
    fn mjpeg_in_a_video_container_is_not_a_still() {
        assert_eq!(image_codec_kind("png", "png_pipe"), CodecKind::Still);
        assert_eq!(image_codec_kind("mjpeg", "jpeg_pipe"), CodecKind::Still);
        assert_eq!(image_codec_kind("mjpeg", "avi"), CodecKind::Other);
        assert_eq!(image_codec_kind("exr", "image2"), CodecKind::Hdr);
    }

    #[test]
    fn webp_animation_bit_requires_the_vp8x_chunk() {
        let mut header = [0u8; 21];
        header[0..4].copy_from_slice(b"RIFF");
        header[8..12].copy_from_slice(b"WEBP");
        header[12..16].copy_from_slice(b"VP8X");
        header[16] = 10;
        assert!(!webp_animation_flag(&header));
        header[20] = 0x02;
        assert!(webp_animation_flag(&header));
        header[12] = b'V';
        header[13] = b'P';
        header[14] = b'8';
        header[15] = b' ';
        assert!(!webp_animation_flag(&header));
    }

    #[test]
    fn classifies_video_and_rejects_one_frame_and_alpha() {
        let video = VideoFacts {
            width: 1920,
            height: 1080,
            fps_num: 120,
            fps_den: 2,
            frame_count: 300,
            duration_sec: 5.0,
            unbounded: false,
            has_audio: true,
            sar_num: 2,
            sar_den: 2,
            alpha: false,
            pixel_format: PixelFormatClass::Yuv420Bit8,
        };
        let classified = classify_video(&video).unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(classified.fps_num, 60);
        assert_eq!(classified.fps_den, 1);
        assert_eq!(classified.sar_num, 1);
        assert_eq!(classified.frame_count, 300);
        let mut one = video;
        one.frame_count = 1;
        assert!(
            classify_video(&one)
                .unwrap_err()
                .message()
                .contains("1 frame")
        );
        let mut alpha = video;
        alpha.alpha = true;
        assert!(
            classify_video(&alpha)
                .unwrap_err()
                .message()
                .contains("アルファ")
        );
        let mut twelve = video;
        twelve.pixel_format = PixelFormatClass::Unsupported;
        assert!(
            classify_video(&twelve)
                .unwrap_err()
                .message()
                .contains("4:2:0")
        );
        let mut ten = video;
        ten.pixel_format = PixelFormatClass::Yuv420Bit10;
        assert!(classify_video(&ten).is_ok());
        let mut unknown = video;
        unknown.pixel_format = PixelFormatClass::Unknown;
        assert!(classify_video(&unknown).is_ok());
    }

    #[test]
    fn pixel_format_class_accepts_only_8_and_10_bit_420() {
        assert_eq!(
            pixel_format_class(8, true, true),
            PixelFormatClass::Yuv420Bit8
        );
        assert_eq!(
            pixel_format_class(10, true, true),
            PixelFormatClass::Yuv420Bit10
        );
        assert_eq!(
            pixel_format_class(12, true, true),
            PixelFormatClass::Unsupported
        );
        assert_eq!(
            pixel_format_class(8, false, true),
            PixelFormatClass::Unsupported
        );
        assert_eq!(
            pixel_format_class(12, true, false),
            PixelFormatClass::Unknown
        );
        assert!(ensure_importable_video(PixelFormatClass::Unknown).is_ok());
        assert!(ensure_importable_video(PixelFormatClass::Unsupported).is_err());
    }

    #[test]
    fn classifies_audio_duration_in_samples() {
        let audio = classify_audio(&AudioFacts {
            sample_rate: 48000,
            duration_sec: 1.5,
            unbounded: false,
        })
        .unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(audio.duration_samples, 72000);
        assert!(
            classify_audio(&AudioFacts {
                sample_rate: 48000,
                duration_sec: f64::INFINITY,
                unbounded: false,
            })
            .is_err()
        );
        assert!(
            classify_audio(&AudioFacts {
                sample_rate: 0,
                duration_sec: 1.0,
                unbounded: false,
            })
            .is_err()
        );
    }
}
