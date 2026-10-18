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
}

impl D3d12Decoder {
    pub fn decode_frame(&mut self) -> Result<Option<ImportedFrame>, MediaError> {
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
    })
}
