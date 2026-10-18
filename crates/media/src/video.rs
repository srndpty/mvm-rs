//! 映像の decode。D3D12VA の NV12 / P010 texture を、同じ DXGI device の wgpu texture として返す。

use std::path::Path;

use crate::classify::ensure_importable_video;
use crate::color::VideoColor;
use crate::error::MediaError;
use crate::ffi::{
    self, Decoder, Frame, HwDevice, Input, Packet, ReadStatus, ReceiveStatus, SendStatus,
};

pub use crate::ffi::DecodedTextureFormat;

/// decode 済みの 1 frame。`texture` は decoder の texture pool の一部（配列の `array_layer` 番目）。
///
/// AVFrame を保持している間は、FFmpeg がその slice を次の decode に再利用しない。
/// この値は、`texture` を読む GPU 処理の完了まで drop しないこと。
pub struct ImportedFrame {
    pub index: i64,
    pub width: u32,
    pub height: u32,
    pub array_layer: u32,
    pub format: DecodedTextureFormat,
    pub color: VideoColor,
    pub texture: wgpu::Texture,
    _frame: Frame,
}

pub struct D3d12Decoder {
    input: Input,
    decoder: Decoder,
    device: wgpu::Device,
    stream_index: usize,
    packet: Packet,
    frame: Frame,
    flushing: bool,
    /// [`Self::decode_source_frame`] が次に seek せず続けられる位置。
    position: Position,
}

/// 素材の読み位置。前方の近い frame だけ seek せずに decode を続ける。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Position {
    /// 開いた直後。frame 0 から読める。
    Start,
    /// 直前にこの番号の frame を返した。
    After(i64),
    /// 失敗などで読み位置が分からない。次は必ず seek する。
    Unknown,
}

/// これより先の frame は、逐次 decode より keyframe への seek の方が速いとみなす。
const FORWARD_DECODE_LIMIT: i64 = 120;
/// seek した keyframe から目的の frame までに decode してよい frame 数。
const MAX_DECODE_AFTER_SEEK: usize = 8192;

impl D3d12Decoder {
    /// 次の frame を返す。素材の終わりでは `None`。
    pub fn decode_frame(&mut self) -> Result<Option<ImportedFrame>, MediaError> {
        self.position = Position::Unknown;
        let frame = self.next_frame()?;
        if let Some(frame) = &frame {
            self.position = Position::After(frame.index);
        }
        Ok(frame)
    }

    fn next_frame(&mut self) -> Result<Option<ImportedFrame>, MediaError> {
        loop {
            match self.decoder.receive(&mut self.frame)? {
                ReceiveStatus::Frame => {
                    // frame の所有を ImportedFrame へ移す。unref すると slice が pool へ戻り、
                    // 合成の前に次の decode で上書きされ得る。
                    let frame = std::mem::replace(&mut self.frame, Frame::new()?);
                    let index = self.decoder.frame_index(&frame)?;
                    let color = self.decoder.yuv_color(&frame)?;
                    let imported = ffi::import_d3d12_frame(&frame, &self.device)?;
                    return Ok(Some(ImportedFrame {
                        index,
                        width: imported.width,
                        height: imported.height,
                        array_layer: imported.array_layer,
                        format: imported.format,
                        color,
                        texture: imported.texture,
                        _frame: frame,
                    }));
                }
                ReceiveStatus::Again => {
                    if self.flushing {
                        return Ok(None);
                    }
                    match self.input.read_packet(&mut self.packet)? {
                        ReadStatus::Eof => {
                            self.decoder.flush()?;
                            self.flushing = true;
                        }
                        ReadStatus::Packet => {
                            let status = if self.packet.stream_index() == Some(self.stream_index) {
                                Some(self.decoder.send(&self.packet))
                            } else {
                                None
                            };
                            self.packet.unref();
                            if let Some(SendStatus::InvalidData) = status.transpose()? {
                                return Err(MediaError::new("映像の packet が壊れています"));
                            }
                        }
                    }
                }
                ReceiveStatus::Eof => return Ok(None),
            }
        }
    }

    /// 素材の `index` 番目の frame を返す。直前より少し後ろなら decode を続け、それ以外は keyframe まで戻ってから進む。
    ///
    /// 返した [`ImportedFrame`] を drop するまで、同じ decoder で次の frame を読まないこと。
    /// texture pool の slice を次の decode が上書きする。
    pub fn decode_source_frame(&mut self, index: i64) -> Result<ImportedFrame, MediaError> {
        if index < 0 {
            return Err(MediaError::new("frame 番号が不正です"));
        }
        if self.flushing || !continues_without_seek(self.position, index) {
            self.reposition(index)?;
        }
        // 途中で失敗したら読み位置は分からない。成功した分だけ位置を進める。
        self.position = Position::Unknown;
        for _ in 0..MAX_DECODE_AFTER_SEEK {
            match self.next_frame()? {
                Some(frame) if frame.index == index => {
                    self.position = Position::After(index);
                    return Ok(frame);
                }
                Some(frame) if frame.index > index => {
                    let got = frame.index;
                    self.position = Position::After(got);
                    return Err(MediaError::new(format!(
                        "frame {index} が無く、{got} まで飛びました"
                    )));
                }
                Some(frame) => {
                    self.position = Position::After(frame.index);
                }
                None => {
                    return Err(MediaError::new(format!("frame {index} がありません")));
                }
            }
        }
        Err(MediaError::new(format!(
            "frame {index} まで decode できません"
        )))
    }

    fn reposition(&mut self, index: i64) -> Result<(), MediaError> {
        self.position = Position::Unknown;
        let timestamp = self.decoder.timestamp_for_frame(index)?;
        self.input.seek(self.stream_index, timestamp)?;
        self.decoder.flush_buffers();
        self.packet.unref();
        self.frame.unref();
        self.flushing = false;
        Ok(())
    }
}

/// `position` から seek せずに `index` まで decode を続けてよいか。
fn continues_without_seek(position: Position, index: i64) -> bool {
    match position {
        Position::Start => index < FORWARD_DECODE_LIMIT,
        Position::After(last) => {
            index > last
                && index
                    .checked_sub(last)
                    .is_some_and(|gap| gap <= FORWARD_DECODE_LIMIT)
        }
        Position::Unknown => false,
    }
}

pub fn open_d3d12va(device: &wgpu::Device, path: &Path) -> Result<D3d12Decoder, MediaError> {
    let mut input = Input::open(path)?;
    input.find_stream_info(i64::MAX)?;
    let streams = input.streams()?;
    let video = streams
        .iter()
        .find(|stream| stream.is_video && !stream.attached_picture)
        .ok_or_else(|| MediaError::new("映像の stream がありません"))?;
    ensure_importable_video(video.pixel_format)?;
    let stream_index = video.index;
    input.discard_except(stream_index);
    let hardware = HwDevice::d3d12(device)?;
    let decoder = Decoder::open(
        &input,
        stream_index,
        i64::MAX,
        Some(&hardware),
        ffi::hw_pixel_format_d3d12(),
    )?;
    Ok(D3d12Decoder {
        input,
        decoder,
        device: device.clone(),
        stream_index,
        packet: Packet::new()?,
        frame: Frame::new()?,
        flushing: false,
        position: Position::Start,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn continues_only_a_short_step_forward() {
        assert!(continues_without_seek(Position::Start, 0));
        assert!(continues_without_seek(Position::Start, 119));
        assert!(!continues_without_seek(Position::Start, 120));
        assert!(continues_without_seek(Position::After(10), 11));
        assert!(continues_without_seek(Position::After(10), 130));
        assert!(
            !continues_without_seek(Position::After(10), 131),
            "遠い前方は seek する"
        );
        assert!(
            !continues_without_seek(Position::After(10), 10),
            "同じ frame は読み直す"
        );
        assert!(
            !continues_without_seek(Position::After(10), 9),
            "後ろは seek する"
        );
        assert!(!continues_without_seek(Position::After(i64::MIN), i64::MAX));
        assert!(!continues_without_seek(Position::Unknown, 0));
    }
}
