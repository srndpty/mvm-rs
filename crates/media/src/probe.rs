//! ファイルを開いて素材の種別と値を決める。静止画かどうかは [`route_media`] が決める。

use std::fs::File;
use std::io::Read;
use std::path::Path;

use crate::classify::{
    AudioFacts, CodecKind, MediaRoute, MediaStreamFacts, VideoFacts, classify_audio,
    classify_video, image_codec_kind, route_media, webp_animation_flag,
};
use crate::error::MediaError;
use crate::ffi::{self, Input};
use crate::still::{DecodedImage, StillLimits, decode_still_image};

#[derive(Clone, Debug)]
pub enum ProbedMedia {
    Video(ProbedVideo),
    Audio(ProbedAudio),
    Image(DecodedImage),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ProbedVideo {
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProbedAudio {
    pub sample_rate: i32,
    pub duration_samples: i64,
}

pub fn probe_media_file(path: &Path) -> Result<ProbedMedia, MediaError> {
    let facts = stream_facts(path, &StillLimits::default())?;
    let decision = route_media(&facts);
    match decision.route {
        MediaRoute::Rejected => Err(MediaError::new(decision.error)),
        MediaRoute::StillImage => Ok(ProbedMedia::Image(decode_still_image(
            path,
            &StillLimits::default(),
        )?)),
        MediaRoute::TimeBased => classify_time_based(path, &facts),
    }
}

pub(crate) fn stream_facts(
    path: &Path,
    limits: &StillLimits,
) -> Result<MediaStreamFacts, MediaError> {
    let mut header = [0u8; 21];
    if let Ok(mut file) = File::open(path) {
        let _ = file.read(&mut header);
    }
    let mut facts = MediaStreamFacts {
        webp_animation_flag: webp_animation_flag(&header),
        ..MediaStreamFacts::default()
    };
    let mut input = match Input::open(path) {
        Ok(input) => input,
        Err(error) => {
            facts.error = error.message().to_owned();
            return Ok(facts);
        }
    };
    if let Err(error) = input.find_stream_info(limits.max_pixels) {
        facts.error = error.message().to_owned();
        return Ok(facts);
    }
    facts.format_name = input.format_name();
    let streams = input.streams()?;
    let mut image_index = None;
    for stream in &streams {
        if stream.is_audio {
            facts.audio_stream_count += 1;
            continue;
        }
        if !stream.is_video {
            continue;
        }
        if stream.attached_picture {
            facts.attached_picture_count += 1;
            continue;
        }
        if facts.video_stream_count == 0 {
            facts.video_codec_name = stream.codec_name.clone();
            facts.width = stream.width;
            facts.height = stream.height;
            facts.video_alpha_capable = stream.pix_fmt_has_alpha || stream.alpha_mode_one;
            image_index = Some(stream.index);
        }
        facts.video_stream_count += 1;
    }
    match image_codec_kind(&facts.video_codec_name, &facts.format_name) {
        CodecKind::Still => facts.still_image_codec = true,
        CodecKind::Hdr => facts.hdr_image_codec = true,
        CodecKind::Other => {}
    }
    if facts.still_image_codec {
        let Some(index) = image_index else {
            facts.error = "画像の stream がありません".to_owned();
            return Ok(facts);
        };
        input.discard_except(index);
        facts.image_packet_count_up_to_2 = ffi::count_image_packets(&mut input, index)?;
    }
    facts.ok = true;
    Ok(facts)
}

fn classify_time_based(path: &Path, facts: &MediaStreamFacts) -> Result<ProbedMedia, MediaError> {
    let mut input = Input::open(path)?;
    // codecpar の sample rate や frame 数は、header だけでは 0 のことがある。
    input.find_stream_info(i64::MAX)?;
    input_time_based(&input, facts)
}

fn input_time_based(input: &Input, facts: &MediaStreamFacts) -> Result<ProbedMedia, MediaError> {
    let streams = input.streams()?;
    if facts.video_stream_count > 0 {
        let video = streams
            .iter()
            .find(|stream| stream.is_video && !stream.attached_picture)
            .ok_or_else(|| MediaError::new("映像の stream がありません"))?;
        let (fps_num, fps_den) = if video.avg_frame_rate.0 > 0 && video.avg_frame_rate.1 > 0 {
            video.avg_frame_rate
        } else {
            video.r_frame_rate
        };
        let duration = stream_duration_seconds(video).or_else(|| input.duration_seconds());
        let frame_count = if video.nb_frames > 0 {
            video.nb_frames
        } else if let Some(duration) = duration {
            if fps_den > 0 {
                (duration * f64::from(fps_num) / f64::from(fps_den)).round() as i64
            } else {
                0
            }
        } else {
            0
        };
        let has_audio = streams.iter().any(|stream| stream.is_audio);
        let classified = classify_video(&VideoFacts {
            width: video.width,
            height: video.height,
            fps_num,
            fps_den,
            frame_count,
            duration_sec: duration.unwrap_or(0.0),
            unbounded: duration.is_none() && video.nb_frames <= 0,
            has_audio,
            sar_num: video.sar.0,
            sar_den: video.sar.1,
            alpha: facts.video_alpha_capable,
            pixel_format: video.pixel_format,
        })?;
        return Ok(ProbedMedia::Video(ProbedVideo {
            width: classified.width,
            height: classified.height,
            fps_num: classified.fps_num,
            fps_den: classified.fps_den,
            frame_count: classified.frame_count,
            has_audio: classified.has_audio,
            sar_num: classified.sar_num,
            sar_den: classified.sar_den,
            duration_sec: classified.duration_sec,
        }));
    }
    let audio = streams
        .iter()
        .find(|stream| stream.is_audio)
        .ok_or_else(|| MediaError::new("音声を開けません"))?;
    let duration = stream_duration_seconds(audio).or_else(|| input.duration_seconds());
    let classified = classify_audio(&AudioFacts {
        sample_rate: audio.sample_rate,
        duration_sec: duration.unwrap_or(0.0),
        unbounded: duration.is_none(),
    })?;
    Ok(ProbedMedia::Audio(ProbedAudio {
        sample_rate: classified.sample_rate,
        duration_samples: classified.duration_samples,
    }))
}

fn stream_duration_seconds(stream: &ffi::StreamInfo) -> Option<f64> {
    if stream.duration <= 0 || stream.time_base.1 == 0 {
        return None;
    }
    Some(stream.duration as f64 * f64::from(stream.time_base.0) / f64::from(stream.time_base.1))
}
