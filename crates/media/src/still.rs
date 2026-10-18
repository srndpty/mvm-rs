//! 静止画を RGBA へ decode し、EXIF の向きを反映する。sRGB 以外の ICC profile は拒否する。

use std::path::Path;

use crate::error::MediaError;
use crate::ffi::{self, Decoder, Frame, Input, ReadStatus, ReceiveStatus, SendStatus};
use crate::icc::icc_profile_is_srgb;
use crate::orientation::apply_orientation;

#[derive(Clone, Copy, Debug)]
pub struct StillLimits {
    pub max_dimension: i32,
    pub max_pixels: i64,
}

impl Default for StillLimits {
    fn default() -> Self {
        Self {
            max_dimension: 16384,
            max_pixels: 64 * 1024 * 1024,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DecodedImage {
    pub width: i32,
    pub height: i32,
    pub rgba: Vec<u8>,
    pub orientation: i32,
}

pub fn decode_still_image(path: &Path, limits: &StillLimits) -> Result<DecodedImage, MediaError> {
    let mut input = Input::open(path)?;
    input.find_stream_info(limits.max_pixels)?;
    let streams = input.streams()?;
    let Some(stream) = streams
        .iter()
        .find(|stream| stream.is_video && !stream.attached_picture)
    else {
        return Err(MediaError::new("画像の stream がありません"));
    };
    if stream.width != 0 || stream.height != 0 {
        within_limits(stream.width, stream.height, limits)?;
    }
    input.discard_except(stream.index);
    let mut decoder = Decoder::open(&input, stream.index, limits.max_pixels, None, 0)?;
    let mut packet = ffi::Packet::new()?;
    let mut frame = Frame::new()?;
    let mut seen = false;
    loop {
        match input.read_packet(&mut packet)? {
            ReadStatus::Eof => {
                decoder_flush(&mut decoder, &mut frame, &mut seen, limits)?;
                break;
            }
            ReadStatus::Packet => {
                if packet.stream_index() == Some(stream.index) {
                    if let SendStatus::InvalidData = decoder.send(&packet)? {
                        return Err(MediaError::new("画像のデータが壊れています"));
                    }
                    pull(&mut decoder, &mut frame, &mut seen, limits)?;
                }
                packet.unref();
                if seen {
                    break;
                }
            }
        }
    }
    if !seen {
        return Err(MediaError::new("画像のデータがありません"));
    }
    within_limits(frame.width(), frame.height(), limits)?;
    let orientation = ffi::frame_orientation(&frame)?;
    if let Some(profile) = ffi::frame_icc_profile(&frame)
        && let Err(rejection) = icc_profile_is_srgb(&profile)
    {
        let description = if rejection.description.is_empty() {
            "説明なし".to_owned()
        } else {
            rejection.description
        };
        return Err(MediaError::new(format!(
            "sRGB 以外の ICC profile を持つ画像には対応していません ({description}: {})。色が変わるため読み込みません。sRGB へ変換してから読み込んでください",
            rejection.reason
        )));
    }
    let rgba = ffi::frame_to_rgba(&frame)?;
    let oriented = apply_orientation(frame.width(), frame.height(), &rgba, orientation)?;
    Ok(DecodedImage {
        width: oriented.width,
        height: oriented.height,
        rgba: oriented.rgba,
        orientation,
    })
}

fn decoder_flush(
    decoder: &mut Decoder,
    frame: &mut Frame,
    seen: &mut bool,
    limits: &StillLimits,
) -> Result<(), MediaError> {
    decoder.flush()?;
    pull(decoder, frame, seen, limits)
}

fn pull(
    decoder: &mut Decoder,
    frame: &mut Frame,
    seen: &mut bool,
    limits: &StillLimits,
) -> Result<(), MediaError> {
    if *seen {
        return Ok(());
    }
    match decoder
        .receive(frame)
        .map_err(|error| annotate_decode(error, limits))?
    {
        ReceiveStatus::Frame => {
            *seen = true;
            Ok(())
        }
        ReceiveStatus::Again | ReceiveStatus::Eof => Ok(()),
    }
}

fn annotate_decode(error: MediaError, limits: &StillLimits) -> MediaError {
    MediaError::new(format!(
        "画像を decode できません (画素数の上限 {}): {}",
        limits.max_pixels,
        error.message()
    ))
}

fn within_limits(width: i32, height: i32, limits: &StillLimits) -> Result<(), MediaError> {
    if width <= 0 || height <= 0 {
        return Err(MediaError::new("画像の寸法を取得できません"));
    }
    if width > limits.max_dimension || height > limits.max_dimension {
        return Err(MediaError::new(format!(
            "画像の一辺が上限 {} を超えています",
            limits.max_dimension
        )));
    }
    let pixels = i64::from(width).saturating_mul(i64::from(height));
    if pixels > limits.max_pixels {
        return Err(MediaError::new(format!(
            "画像の画素数が上限 {} を超えています",
            limits.max_pixels
        )));
    }
    Ok(())
}
