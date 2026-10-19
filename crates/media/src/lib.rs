//! 素材の種別判定と decode、書き出しの encode。判定はここだけに置く。
//!
//! unsafe は [`ffi`] と [`encode_ffi`] だけ。公開 API は path と画素と [`Result`] だけを扱う。

mod audio;
mod classify;
mod color;
mod encode;
#[allow(unsafe_code)]
mod encode_ffi;
mod error;
#[allow(unsafe_code)]
mod ffi;
mod icc;
mod orientation;
mod pcm;
mod probe;
mod still;
mod video;

pub use audio::decode_waveform;
pub use classify::{
    AudioFacts, CodecKind, MediaRoute, MediaStreamFacts, RouteDecision, VideoFacts, classify_audio,
    classify_video, image_codec_kind, route_media, webp_animation_flag,
};
pub use color::VideoColor;
pub use encode::{MAX_CRF, Mp4Settings, Mp4Summary, Mp4Writer};
pub use error::MediaError;
pub use icc::icc_profile_is_srgb;
pub use orientation::apply_orientation;
pub use pcm::PcmStream;
pub use probe::{ProbedAudio, ProbedMedia, ProbedVideo, probe_media_file};
pub use still::{DecodedImage, StillLimits, decode_still_image};
pub use video::{D3d12Decoder, DecodedTextureFormat, ImportedFrame, open_d3d12va};
