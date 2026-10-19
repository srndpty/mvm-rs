//! 合成済みの frame と mix 済みの音声を、1 本の MP4（H.264 / AAC）にする。
//!
//! 映像は frame 番号、音声は 48 kHz の sample 番号をそのまま時刻にする。どちらも 0 から隙間なく渡す。

use std::path::Path;

use mvm_core::Rational;

use crate::encode_ffi::{Muxer, MuxerConfig};
use crate::error::MediaError;

/// H.264 の CRF の範囲。
pub const MAX_CRF: u8 = 51;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Mp4Settings {
    pub width: u32,
    pub height: u32,
    pub fps: Rational,
    /// libx264 の constant rate factor（0..=51、小さいほど高画質）。
    pub crf: u8,
    /// 音声の stream を持つか。持たないときは [`Mp4Writer::write_audio`] を呼ばない。
    pub audio: bool,
}

/// 書き終えた MP4 の中身。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Mp4Summary {
    pub video_frames: i64,
    pub audio_samples: i64,
}

pub struct Mp4Writer {
    muxer: Muxer,
    frames: i64,
    audio_samples: i64,
    audio_frame: Option<usize>,
    pending_left: Vec<f32>,
    pending_right: Vec<f32>,
}

impl Mp4Writer {
    /// `path` へ書き始める。拡張子に依らず MP4 にする。
    pub fn create(path: &Path, settings: &Mp4Settings) -> Result<Self, MediaError> {
        let config = muxer_config(settings)?;
        let muxer = Muxer::create(path, &config)?;
        let audio_frame = muxer.audio_frame_size();
        if settings.audio && audio_frame.is_none() {
            return Err(MediaError::new("音声の stream を用意できません"));
        }
        Ok(Self {
            muxer,
            frames: 0,
            audio_samples: 0,
            audio_frame,
            pending_left: Vec::new(),
            pending_right: Vec::new(),
        })
    }

    /// 次の frame。行を詰めた straight RGBA（出力の幅 × 高さ × 4 byte）。
    pub fn write_frame(&mut self, rgba: &[u8]) -> Result<(), MediaError> {
        self.muxer.write_video(rgba, self.frames)?;
        self.frames = self
            .frames
            .checked_add(1)
            .ok_or_else(|| MediaError::new("frame 数が溢れました"))?;
        Ok(())
    }

    /// 次の sample 列。長さは自由で、AAC の frame 長に詰めてから encode する。
    pub fn write_audio(&mut self, left: &[f32], right: &[f32]) -> Result<(), MediaError> {
        let Some(frame) = self.audio_frame else {
            return Err(MediaError::new("音声の stream がありません"));
        };
        if left.len() != right.len() {
            return Err(MediaError::new("音声の左右の長さが違います"));
        }
        self.pending_left.extend_from_slice(left);
        self.pending_right.extend_from_slice(right);
        while self.pending_left.len() >= frame {
            self.encode_pending(frame)?;
        }
        Ok(())
    }

    /// 残りを encode して file を閉じる。
    pub fn finish(mut self) -> Result<Mp4Summary, MediaError> {
        if !self.pending_left.is_empty() {
            self.encode_pending(self.pending_left.len())?;
        }
        self.muxer.finish()?;
        Ok(Mp4Summary {
            video_frames: self.frames,
            audio_samples: self.audio_samples,
        })
    }

    fn encode_pending(&mut self, count: usize) -> Result<(), MediaError> {
        let (Some(left), Some(right)) = (
            self.pending_left.get(..count),
            self.pending_right.get(..count),
        ) else {
            return Err(MediaError::new("音声の残りが足りません"));
        };
        self.muxer.write_audio(left, right, self.audio_samples)?;
        self.pending_left.drain(..count);
        self.pending_right.drain(..count);
        self.audio_samples = i64::try_from(count)
            .ok()
            .and_then(|count| self.audio_samples.checked_add(count))
            .ok_or_else(|| MediaError::new("音声の sample 数が溢れました"))?;
        Ok(())
    }
}

fn muxer_config(settings: &Mp4Settings) -> Result<MuxerConfig, MediaError> {
    if settings.crf > MAX_CRF {
        return Err(MediaError::new(format!(
            "CRF は 0〜{MAX_CRF} で指定してください: {}",
            settings.crf
        )));
    }
    // YUV420P は幅・高さが偶数でなければならない。
    if settings.width == 0
        || settings.height == 0
        || !settings.width.is_multiple_of(2)
        || !settings.height.is_multiple_of(2)
    {
        return Err(MediaError::new(format!(
            "出力の寸法が不正です: {}x{}",
            settings.width, settings.height
        )));
    }
    let fps = settings
        .fps
        .reduced()
        .ok_or_else(|| MediaError::new("出力の fps が不正です"))?;
    let too_large = || MediaError::new("出力の寸法または fps が大きすぎます");
    Ok(MuxerConfig {
        width: i32::try_from(settings.width).map_err(|_| too_large())?,
        height: i32::try_from(settings.height).map_err(|_| too_large())?,
        fps_num: i32::try_from(fps.num).map_err(|_| too_large())?,
        fps_den: i32::try_from(fps.den).map_err(|_| too_large())?,
        crf: i32::from(settings.crf),
        audio: settings.audio,
    })
}
