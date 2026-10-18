//! FFmpeg の FFI。この module 以外に `unsafe` を書かない。
//!
//! FFmpeg の DLL は import library で暗黙リンクしているので、OS の loader が起動時に読む。
//! 実行中に読み込み先を変える手段は無い。`third_party/ffmpeg/bin` を PATH の先頭に置くのは
//! `scripts/env.ps1`（`dev.ps1` が読む）の役目で、無ければ起動時に DLL が見つからず失敗する。

use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_int, c_void};
use std::path::Path;
use std::ptr;

use ffmpeg_sys_next as ff;

use crate::error::MediaError;

/// bindgen が enum にした引数や、header に含めていない EXIF を整数 ABI のまま呼ぶ。
#[repr(C)]
struct ExifMetadata {
    entries: *mut c_void,
    count: u32,
    size: u32,
}

unsafe extern "C" {
    #[link_name = "av_pix_fmt_desc_get"]
    fn av_pix_fmt_desc_get_int(pix_fmt: c_int) -> *const ff::AVPixFmtDescriptor;
    #[link_name = "sws_getContext"]
    fn sws_get_context_int(
        src_w: c_int,
        src_h: c_int,
        src_format: c_int,
        dst_w: c_int,
        dst_h: c_int,
        dst_format: c_int,
        flags: c_int,
        src_filter: *mut c_void,
        dst_filter: *mut c_void,
        param: *const f64,
    ) -> *mut ff::SwsContext;
    fn av_exif_matrix_to_orientation(matrix: *const i32) -> c_int;
    fn av_exif_parse_buffer(
        logctx: *mut c_void,
        data: *const u8,
        size: usize,
        ifd: *mut ExifMetadata,
        header_mode: c_int,
    ) -> c_int;
    fn av_exif_ifd_to_dict(
        logctx: *mut c_void,
        ifd: *const ExifMetadata,
        metadata: *mut *mut ff::AVDictionary,
    ) -> c_int;
    fn av_exif_free(ifd: *mut ExifMetadata);
}

const EXIF_TIFF_HEADER: c_int = 0;

fn ff_error(code: c_int) -> String {
    let mut buffer = [0u8; 256];
    // SAFETY: buffer は 256 byte で、av_strerror はその範囲だけ書く。
    unsafe {
        ff::av_strerror(code, buffer.as_mut_ptr().cast::<c_char>(), buffer.len());
    }
    match CStr::from_bytes_until_nul(&buffer) {
        Ok(text) => text.to_string_lossy().into_owned(),
        Err(_) => "不明な FFmpeg エラー".to_owned(),
    }
}

fn path_cstring(path: &Path) -> Result<CString, MediaError> {
    let text = path
        .to_str()
        .ok_or_else(|| MediaError::new("パスを UTF-8 に変換できません"))?;
    CString::new(text).map_err(|_| MediaError::new("パスに NUL を含められません"))
}

pub struct Input {
    ptr: *mut ff::AVFormatContext,
}

// FFmpeg の context はスレッド間で共有しない。
pub struct Packet {
    ptr: *mut ff::AVPacket,
}

pub struct Frame {
    ptr: *mut ff::AVFrame,
}

pub struct Decoder {
    codec: *mut ff::AVCodecContext,
    /// get_format の opaque が指す。codec より後に drop される。
    wanted_format: Box<c_int>,
    time_base: ff::AVRational,
    start_time: i64,
}

pub struct StreamInfo {
    pub index: usize,
    pub is_video: bool,
    pub is_audio: bool,
    pub attached_picture: bool,
    pub codec_name: String,
    pub width: i32,
    pub height: i32,
    pub pix_fmt_has_alpha: bool,
    pub alpha_mode_one: bool,
    pub sample_rate: i32,
    pub avg_frame_rate: (i32, i32),
    pub r_frame_rate: (i32, i32),
    pub time_base: (i32, i32),
    pub duration: i64,
    pub nb_frames: i64,
    pub sar: (i32, i32),
}

pub enum ReadStatus {
    Packet,
    Eof,
}

pub enum SendStatus {
    Sent,
    /// FFmpeg が AVERROR_INVALIDDATA を返した。
    InvalidData,
}

pub enum ReceiveStatus {
    Frame,
    Again,
    Eof,
}

impl Input {
    pub fn open(path: &Path) -> Result<Self, MediaError> {
        let text = path_cstring(path)?;
        let mut ptr = ptr::null_mut();
        // SAFETY: text は NUL 終端。失敗時 ptr は null のまま、成功時は close_input で解放する。
        let code = unsafe {
            ff::avformat_open_input(&mut ptr, text.as_ptr(), ptr::null(), ptr::null_mut())
        };
        if code < 0 || ptr.is_null() {
            return Err(MediaError::new(format!(
                "素材を開けません: {}",
                ff_error(code)
            )));
        }
        Ok(Self { ptr })
    }

    pub fn find_stream_info(&mut self, max_pixels: i64) -> Result<(), MediaError> {
        // max_pixels の option 範囲は 0..=i32::MAX。範囲外は「上限を掛けない」。
        if !(1..=i64::from(i32::MAX)).contains(&max_pixels) {
            // SAFETY: self.ptr は open 済み。option 無しで stream 情報を読む。
            let code = unsafe { ff::avformat_find_stream_info(self.ptr, ptr::null_mut()) };
            if code < 0 {
                return Err(MediaError::new(format!(
                    "stream 情報を取得できません: {}",
                    ff_error(code)
                )));
            }
            return Ok(());
        }
        // SAFETY: self.ptr は open が成功した context で、Drop まで null にならない。
        let count = unsafe { (*self.ptr).nb_streams } as usize;
        let mut dicts = vec![ptr::null_mut(); count];
        let key = CString::new("max_pixels").map_err(|_| MediaError::new("option 名が不正です"))?;
        let value = CString::new(max_pixels.to_string())
            .map_err(|_| MediaError::new("画素数の上限を表せません"))?;
        for dict in &mut dicts {
            // SAFETY: dict は av_dict_set が所有する辞書。後で av_dict_free する。
            let code = unsafe { ff::av_dict_set(dict, key.as_ptr(), value.as_ptr(), 0) };
            if code < 0 {
                for dict in &mut dicts {
                    // SAFETY: dict は null か av_dict_set が確保した辞書。av_dict_free は null も受け付ける。
                    unsafe { ff::av_dict_free(dict) };
                }
                return Err(MediaError::new(format!(
                    "stream 情報を取得できません: {}",
                    ff_error(code)
                )));
            }
        }
        let options = if dicts.is_empty() {
            ptr::null_mut()
        } else {
            dicts.as_mut_ptr()
        };
        // SAFETY: self.ptr は open 済み。options は stream 数と同じ長さ。
        let code = unsafe { ff::avformat_find_stream_info(self.ptr, options) };
        for dict in &mut dicts {
            // SAFETY: dict は null か av_dict_set が確保した辞書。av_dict_free は null も受け付ける。
            unsafe { ff::av_dict_free(dict) };
        }
        if code < 0 {
            return Err(MediaError::new(format!(
                "stream 情報を取得できません: {}",
                ff_error(code)
            )));
        }
        Ok(())
    }

    pub fn format_name(&self) -> String {
        // SAFETY: iformat は open 中だけ有効。name は静的な文字列。
        unsafe {
            let format = (*self.ptr).iformat;
            if format.is_null() || (*format).name.is_null() {
                return String::new();
            }
            CStr::from_ptr((*format).name)
                .to_string_lossy()
                .into_owned()
        }
    }

    pub fn duration_seconds(&self) -> Option<f64> {
        // SAFETY: duration は AV_TIME_BASE 単位のスカラー。
        let duration = unsafe { (*self.ptr).duration };
        if duration <= 0 {
            None
        } else {
            Some(duration as f64 / f64::from(ff::AV_TIME_BASE))
        }
    }

    pub fn streams(&self) -> Result<Vec<StreamInfo>, MediaError> {
        // SAFETY: self.ptr は open が成功した context で、Drop まで null にならない。
        let count = unsafe { (*self.ptr).nb_streams } as usize;
        let mut infos = Vec::with_capacity(count);
        for index in 0..count {
            infos.push(stream_info(self.ptr, index)?);
        }
        Ok(infos)
    }

    pub fn discard_except(&mut self, keep: usize) {
        // SAFETY: self.ptr は open が成功した context で、Drop まで null にならない。
        let count = unsafe { (*self.ptr).nb_streams } as usize;
        for index in 0..count {
            if index == keep {
                continue;
            }
            // SAFETY: stream は format が所有し、discard は demux の読み飛ばし指定。
            unsafe {
                let stream = *(*self.ptr).streams.add(index);
                (*stream).discard = ff::AVDiscard::AVDISCARD_ALL;
            }
        }
    }

    pub fn read_packet(&mut self, packet: &mut Packet) -> Result<ReadStatus, MediaError> {
        // SAFETY: packet は alloc 済み。失敗しても packet は unref できる状態。
        let code = unsafe { ff::av_read_frame(self.ptr, packet.ptr) };
        if code == ff::AVERROR_EOF {
            return Ok(ReadStatus::Eof);
        }
        if code < 0 {
            return Err(MediaError::new(format!(
                "packet を読めません: {}",
                ff_error(code)
            )));
        }
        Ok(ReadStatus::Packet)
    }
}

impl Drop for Input {
    fn drop(&mut self) {
        if !self.ptr.is_null() {
            // SAFETY: open_input が返した context を 1 回だけ閉じる。
            unsafe { ff::avformat_close_input(&mut self.ptr) };
        }
    }
}

fn stream_info(format: *mut ff::AVFormatContext, index: usize) -> Result<StreamInfo, MediaError> {
    // SAFETY: index は nb_streams 未満。codecpar と metadata は stream が所有する。
    unsafe {
        let stream = *(*format).streams.add(index);
        let codecpar = (*stream).codecpar;
        if codecpar.is_null() {
            return Err(MediaError::new("stream の codec 情報がありません"));
        }
        let codec_type = (*codecpar).codec_type;
        let name_ptr = ff::avcodec_get_name((*codecpar).codec_id);
        let codec_name = if name_ptr.is_null() {
            String::new()
        } else {
            CStr::from_ptr(name_ptr).to_string_lossy().into_owned()
        };
        let pix_fmt_has_alpha = if codec_type == ff::AVMediaType::AVMEDIA_TYPE_VIDEO {
            let desc = av_pix_fmt_desc_get_int((*codecpar).format);
            !desc.is_null() && ((*desc).flags & ff::AV_PIX_FMT_FLAG_ALPHA as u64) != 0
        } else {
            false
        };
        let alpha_mode_one =
            dictionary_value((*stream).metadata, "alpha_mode").as_deref() == Some("1");
        Ok(StreamInfo {
            index,
            is_video: codec_type == ff::AVMediaType::AVMEDIA_TYPE_VIDEO,
            is_audio: codec_type == ff::AVMediaType::AVMEDIA_TYPE_AUDIO,
            attached_picture: ((*stream).disposition & ff::AV_DISPOSITION_ATTACHED_PIC) != 0,
            codec_name,
            width: (*codecpar).width,
            height: (*codecpar).height,
            pix_fmt_has_alpha,
            alpha_mode_one,
            sample_rate: (*codecpar).sample_rate,
            avg_frame_rate: ((*stream).avg_frame_rate.num, (*stream).avg_frame_rate.den),
            r_frame_rate: ((*stream).r_frame_rate.num, (*stream).r_frame_rate.den),
            time_base: ((*stream).time_base.num, (*stream).time_base.den),
            duration: (*stream).duration,
            nb_frames: (*stream).nb_frames,
            sar: (
                (*codecpar).sample_aspect_ratio.num,
                (*codecpar).sample_aspect_ratio.den,
            ),
        })
    }
}

fn dictionary_value(dict: *mut ff::AVDictionary, key: &str) -> Option<String> {
    if dict.is_null() {
        return None;
    }
    let key = CString::new(key).ok()?;
    // SAFETY: dict は stream が所有し、key は呼び出し中だけ有効。返る entry も dict が所有する。
    let entry = unsafe { ff::av_dict_get(dict, key.as_ptr(), ptr::null(), 0) };
    if entry.is_null() {
        return None;
    }
    // SAFETY: entry は直前に null を検査した、dict が所有する entry。
    let value = unsafe { (*entry).value };
    if value.is_null() {
        None
    } else {
        // SAFETY: value は null でなく、dict が所有する NUL 終端の文字列。この関数の中でだけ読む。
        Some(
            unsafe { CStr::from_ptr(value) }
                .to_string_lossy()
                .into_owned(),
        )
    }
}

impl Packet {
    pub fn new() -> Result<Self, MediaError> {
        // SAFETY: null なら確保失敗。成功した pointer は Drop で free する。
        let ptr = unsafe { ff::av_packet_alloc() };
        if ptr.is_null() {
            return Err(MediaError::new("packet を確保できません"));
        }
        Ok(Self { ptr })
    }

    /// 負の値（demuxer の不正）は None。
    pub fn stream_index(&self) -> Option<usize> {
        // SAFETY: self.ptr は alloc が成功した pointer で、Drop まで null にならない。
        let index = unsafe { (*self.ptr).stream_index };
        usize::try_from(index).ok()
    }

    pub fn unref(&mut self) {
        // SAFETY: self.ptr は alloc が成功した pointer で、Drop まで null にならない。
        unsafe { ff::av_packet_unref(self.ptr) };
    }
}

impl Drop for Packet {
    fn drop(&mut self) {
        if !self.ptr.is_null() {
            // SAFETY: alloc が返した pointer を 1 回だけ解放する。av_packet_free は self.ptr を null にする。
            unsafe { ff::av_packet_free(&mut self.ptr) };
        }
    }
}

impl Frame {
    pub fn new() -> Result<Self, MediaError> {
        // SAFETY: 引数は無い。null は確保失敗として検査する。
        let ptr = unsafe { ff::av_frame_alloc() };
        if ptr.is_null() {
            return Err(MediaError::new("frame を確保できません"));
        }
        Ok(Self { ptr })
    }

    pub fn unref(&mut self) {
        // SAFETY: self.ptr は alloc が成功した pointer で、Drop まで null にならない。
        unsafe { ff::av_frame_unref(self.ptr) };
    }

    pub fn width(&self) -> i32 {
        // SAFETY: self.ptr は alloc が成功した pointer で、Drop まで null にならない。
        unsafe { (*self.ptr).width }
    }

    pub fn height(&self) -> i32 {
        // SAFETY: self.ptr は alloc が成功した pointer で、Drop まで null にならない。
        unsafe { (*self.ptr).height }
    }

    pub fn format(&self) -> i32 {
        // SAFETY: self.ptr は alloc が成功した pointer で、Drop まで null にならない。
        unsafe { (*self.ptr).format }
    }

    pub fn best_effort_timestamp(&self) -> i64 {
        // SAFETY: self.ptr は alloc が成功した pointer で、Drop まで null にならない。
        unsafe { (*self.ptr).best_effort_timestamp }
    }
}

impl Drop for Frame {
    fn drop(&mut self) {
        if !self.ptr.is_null() {
            // SAFETY: alloc が返した pointer を 1 回だけ解放する。av_frame_free は self.ptr を null にする。
            unsafe { ff::av_frame_free(&mut self.ptr) };
        }
    }
}

pub struct HwDevice {
    ptr: *mut ff::AVBufferRef,
}

impl HwDevice {
    pub fn d3d12(device: &wgpu::Device) -> Result<Self, MediaError> {
        // SAFETY: av_hwdevice_ctx_alloc は AVHWDeviceContext を data に持つ buffer を返す。
        // null は確保失敗として検査し、以降は Self の Drop が参照を外す。
        let ptr =
            unsafe { ff::av_hwdevice_ctx_alloc(ff::AVHWDeviceType::AV_HWDEVICE_TYPE_D3D12VA) };
        if ptr.is_null() {
            return Err(MediaError::new("D3D12VA context を確保できません"));
        }
        let context = Self { ptr };
        // AddRef した device を渡す。hwcontext_d3d12va.h の契約で、context の解放時に
        // FFmpeg が必ず Release する（init に失敗した場合も含む）。
        let raw = raw_d3d12_device(device)?;
        // SAFETY: ptr は null を検査済みの D3D12VA 用 buffer で、data は AVHWDeviceContext、
        // hwctx は AVD3D12VADeviceContext を指す（layout は下の const assert で固定）。
        let code = unsafe {
            let hw = (*context.ptr).data.cast::<ff::AVHWDeviceContext>();
            let d3d = (*hw).hwctx.cast::<D3d12DeviceContext>();
            (*d3d).device = raw;
            ff::av_hwdevice_ctx_init(context.ptr)
        };
        if code < 0 {
            return Err(MediaError::new(format!(
                "D3D12VA device を初期化できません: {}",
                ff_error(code)
            )));
        }
        Ok(context)
    }
}

impl Drop for HwDevice {
    fn drop(&mut self) {
        if !self.ptr.is_null() {
            // SAFETY: ctx_alloc が返した buffer の参照を 1 回だけ外す。
            unsafe { ff::av_buffer_unref(&mut self.ptr) };
        }
    }
}

#[repr(C)]
struct D3d12DeviceContext {
    device: *mut c_void,
    video_device: *mut c_void,
    lock: Option<unsafe extern "C" fn(*mut c_void)>,
    unlock: Option<unsafe extern "C" fn(*mut c_void)>,
    lock_ctx: *mut c_void,
    resource_flags: i32,
    heap_flags: i32,
}

#[repr(C)]
struct D3d12Sync {
    fence: *mut c_void,
    event: *mut c_void,
    fence_value: u64,
}

#[repr(C)]
struct D3d12Frame {
    texture: *mut c_void,
    subresource_index: i32,
    _pad: i32,
    sync: D3d12Sync,
    flags: i32,
    _pad2: i32,
}

const _: () = assert!(size_of::<D3d12DeviceContext>() == 48);
const _: () = assert!(std::mem::offset_of!(D3d12DeviceContext, resource_flags) == 40);
const _: () = assert!(size_of::<D3d12Frame>() == 48);
const _: () = assert!(std::mem::offset_of!(D3d12Frame, sync) == 16);
const _: () = assert!(std::mem::offset_of!(D3d12Frame, flags) == 40);

fn raw_d3d12_device(device: &wgpu::Device) -> Result<*mut c_void, MediaError> {
    // SAFETY: as_hal は DX12 device の参照を返す。clone で AddRef し、forget して
    // その参照を FFmpeg に渡す。FFmpeg が context 解放時に Release する。
    let guard = unsafe { device.as_hal::<wgpu_hal::api::Dx12>() };
    let Some(hal) = guard else {
        return Err(MediaError::new("wgpu device は DX12 ではありません"));
    };
    let cloned = hal.raw_device().clone();
    let raw = windows::core::Interface::as_raw(&cloned);
    std::mem::forget(cloned);
    Ok(raw.cast())
}

impl Decoder {
    pub fn open(
        input: &Input,
        index: usize,
        max_pixels: i64,
        hardware: Option<&HwDevice>,
        hw_format: i32,
    ) -> Result<Self, MediaError> {
        // SAFETY: index は stream 数未満。codec context は Drop で free する。
        unsafe {
            let stream = *(*input.ptr).streams.add(index);
            let codecpar = (*stream).codecpar;
            let decoder = ff::avcodec_find_decoder((*codecpar).codec_id);
            if decoder.is_null() {
                return Err(MediaError::new(format!(
                    "decoder がありません: {}",
                    stream_info(input.ptr, index)?.codec_name
                )));
            }
            let codec = ff::avcodec_alloc_context3(decoder);
            if codec.is_null() {
                return Err(MediaError::new("decoder context を確保できません"));
            }
            let copied = ff::avcodec_parameters_to_context(codec, codecpar);
            if copied < 0 {
                ff::avcodec_free_context(&mut (codec as *mut _));
                return Err(MediaError::new(format!(
                    "decoder を設定できません: {}",
                    ff_error(copied)
                )));
            }
            if (1..=i64::from(i32::MAX)).contains(&max_pixels) {
                (*codec).max_pixels = max_pixels;
            }
            let mut wanted_format = Box::new(hw_format);
            if let Some(hw) = hardware {
                (*codec).hw_device_ctx = ff::av_buffer_ref(hw.ptr);
                (*codec).opaque = (wanted_format.as_mut() as *mut c_int).cast::<c_void>();
                (*codec).get_format = Some(select_hardware_format);
            }
            let opened = ff::avcodec_open2(codec, decoder, ptr::null_mut());
            if opened < 0 {
                ff::avcodec_free_context(&mut (codec as *mut _));
                return Err(MediaError::new(format!(
                    "decoder を初期化できません (画素数の上限 {max_pixels}): {}",
                    ff_error(opened)
                )));
            }
            Ok(Self {
                codec,
                wanted_format,
                time_base: (*stream).time_base,
                start_time: (*stream).start_time,
            })
        }
    }

    /// 壊れた packet は error にせず [`SendStatus::InvalidData`] で返す。扱いは呼び出し側が決める。
    pub fn send(&mut self, packet: &Packet) -> Result<SendStatus, MediaError> {
        // SAFETY: self.codec は open が成功した context で、Drop まで null にならない。packet は alloc 済み。
        let code = unsafe { ff::avcodec_send_packet(self.codec, packet.ptr) };
        if code == ff::AVERROR_INVALIDDATA {
            return Ok(SendStatus::InvalidData);
        }
        if code < 0 {
            return Err(MediaError::new(format!(
                "packet を decode できません: {}",
                ff_error(code)
            )));
        }
        Ok(SendStatus::Sent)
    }

    pub fn flush(&mut self) -> Result<(), MediaError> {
        // SAFETY: self.codec は open が成功した context で、Drop まで null にならない。null の packet は flush の合図。
        let code = unsafe { ff::avcodec_send_packet(self.codec, ptr::null()) };
        if code < 0 && code != ff::AVERROR_EOF {
            return Err(MediaError::new(format!(
                "decoder を flush できません: {}",
                ff_error(code)
            )));
        }
        Ok(())
    }

    pub fn receive(&mut self, frame: &mut Frame) -> Result<ReceiveStatus, MediaError> {
        // SAFETY: self.codec は open が成功した context で、Drop まで null にならない。frame は alloc 済み。
        let code = unsafe { ff::avcodec_receive_frame(self.codec, frame.ptr) };
        if code == ff::AVERROR(ff::EAGAIN as c_int) {
            return Ok(ReceiveStatus::Again);
        }
        if code == ff::AVERROR_EOF {
            return Ok(ReceiveStatus::Eof);
        }
        if code < 0 {
            return Err(MediaError::new(format!(
                "frame を decode できません: {}",
                ff_error(code)
            )));
        }
        Ok(ReceiveStatus::Frame)
    }

    pub fn frame_index(&self, frame: &Frame) -> Result<i64, MediaError> {
        let pts = frame.best_effort_timestamp();
        if pts == ff::AV_NOPTS_VALUE {
            return Err(MediaError::new("frame に PTS がありません"));
        }
        let start = if self.start_time == ff::AV_NOPTS_VALUE {
            0
        } else {
            self.start_time
        };
        let relative = pts
            .checked_sub(start)
            .ok_or_else(|| MediaError::new("PTS から stream 開始を減算できません"))?;
        // SAFETY: self.codec は open が成功した context で、Drop まで null にならない。
        let fps = unsafe { (*self.codec).framerate };
        // SAFETY: av_rescale_q は整数だけを受け取る純粋な計算。
        let index = unsafe {
            ff::av_rescale_q(
                relative,
                self.time_base,
                ff::AVRational {
                    num: fps.den,
                    den: fps.num,
                },
            )
        };
        if index == i64::MIN {
            return Err(MediaError::new("PTS を frame 番号へ換算できません"));
        }
        Ok(index)
    }
}

impl Drop for Decoder {
    fn drop(&mut self) {
        let _opaque = self.wanted_format.as_ref();
        if !self.codec.is_null() {
            // SAFETY: open が確保した context。get_format が指す wanted_format より先に解放する。
            unsafe { ff::avcodec_free_context(&mut self.codec) };
        }
    }
}

unsafe extern "C" fn select_hardware_format(
    ctx: *mut ff::AVCodecContext,
    formats: *const ff::AVPixelFormat,
) -> ff::AVPixelFormat {
    // SAFETY: opaque は Decoder が open から drop まで所有する Box<c_int>。
    // formats は FFmpeg が AV_PIX_FMT_NONE で終端する。64 個で打ち切る。
    unsafe {
        if ctx.is_null() || formats.is_null() || (*ctx).opaque.is_null() {
            return ff::AVPixelFormat::AV_PIX_FMT_NONE;
        }
        let wanted = *(*ctx).opaque.cast::<c_int>();
        let mut index = 0;
        loop {
            if index > 64 {
                return ff::AVPixelFormat::AV_PIX_FMT_NONE;
            }
            let format = *formats.add(index);
            if format == ff::AVPixelFormat::AV_PIX_FMT_NONE {
                return ff::AVPixelFormat::AV_PIX_FMT_NONE;
            }
            if format as c_int == wanted {
                return format;
            }
            index += 1;
        }
    }
}

pub fn hw_pixel_format_d3d12() -> i32 {
    ff::AVPixelFormat::AV_PIX_FMT_D3D12 as i32
}

fn is_d3d12(format: i32) -> bool {
    format == ff::AVPixelFormat::AV_PIX_FMT_D3D12 as i32
}

pub fn frame_to_rgba(frame: &Frame) -> Result<Vec<u8>, MediaError> {
    let width = frame.width();
    let height = frame.height();
    if width <= 0 || height <= 0 {
        return Err(MediaError::new("画像の寸法を取得できません"));
    }
    let flags = ff::SwsFlags::SWS_BICUBIC as c_int
        | ff::SwsFlags::SWS_ACCURATE_RND as c_int
        | ff::SwsFlags::SWS_FULL_CHR_H_INT as c_int;
    // SAFETY: sws context はこの関数の終わりで free する。出力 buffer は tightly packed RGBA。
    // 画素形式は FFmpeg が frame に書いた整数のまま渡す。
    unsafe {
        let context = sws_get_context_int(
            width,
            height,
            (*frame.ptr).format,
            width,
            height,
            ff::AVPixelFormat::AV_PIX_FMT_RGBA as c_int,
            flags,
            ptr::null_mut(),
            ptr::null_mut(),
            ptr::null_mut(),
        );
        if context.is_null() {
            return Err(MediaError::new("画像の色変換を初期化できません"));
        }
        let stride = width as usize * 4;
        let mut rgba = vec![0u8; stride * height as usize];
        let mut dst = [
            rgba.as_mut_ptr(),
            ptr::null_mut(),
            ptr::null_mut(),
            ptr::null_mut(),
        ];
        let dst_stride = [stride as c_int, 0, 0, 0];
        let written = ff::sws_scale(
            context,
            (*frame.ptr).data.as_ptr() as *const *const u8,
            (*frame.ptr).linesize.as_ptr(),
            0,
            height,
            dst.as_mut_ptr(),
            dst_stride.as_ptr(),
        );
        ff::sws_freeContext(context);
        if written != height {
            return Err(MediaError::new("画像の色変換に失敗しました"));
        }
        Ok(rgba)
    }
}

pub fn frame_orientation(frame: &Frame) -> Result<i32, MediaError> {
    let mut from_matrix = 0;
    // SAFETY: side data は frame が所有する。display matrix は 9 個の i32。
    unsafe {
        let matrix = ff::av_frame_get_side_data(
            frame.ptr,
            ff::AVFrameSideDataType::AV_FRAME_DATA_DISPLAYMATRIX,
        );
        if !matrix.is_null() {
            if (*matrix).size < size_of::<i32>() * 9 {
                return Err(MediaError::new("display matrix の大きさが不正です"));
            }
            from_matrix = av_exif_matrix_to_orientation((*matrix).data.cast::<i32>());
            if !(1..=8).contains(&from_matrix) {
                return Err(MediaError::new("display matrix を向きへ換算できません"));
            }
        }
    }
    let from_exif = exif_orientation(frame)?;
    if from_matrix != 0 && from_exif != 0 && from_matrix != from_exif {
        return Err(MediaError::new(format!(
            "display matrix と EXIF の向きが一致しません ({from_matrix} / {from_exif})"
        )));
    }
    Ok(if from_matrix != 0 {
        from_matrix
    } else if from_exif != 0 {
        from_exif
    } else {
        1
    })
}

fn exif_orientation(frame: &Frame) -> Result<i32, MediaError> {
    // SAFETY: EXIF side data が無ければ 0。ある場合は parse して辞書から Orientation を読む。
    unsafe {
        let side =
            ff::av_frame_get_side_data(frame.ptr, ff::AVFrameSideDataType::AV_FRAME_DATA_EXIF);
        if side.is_null() {
            return Ok(0);
        }
        let mut metadata = ExifMetadata {
            entries: ptr::null_mut(),
            count: 0,
            size: 0,
        };
        let parsed = av_exif_parse_buffer(
            ptr::null_mut(),
            (*side).data,
            (*side).size,
            &mut metadata,
            EXIF_TIFF_HEADER,
        );
        if parsed < 0 {
            return Err(MediaError::new(format!(
                "EXIF を解析できません: {}",
                ff_error(parsed)
            )));
        }
        let mut dict = ptr::null_mut();
        let converted = av_exif_ifd_to_dict(ptr::null_mut(), &metadata, &mut dict);
        av_exif_free(&mut metadata);
        if converted < 0 {
            return Err(MediaError::new(format!(
                "EXIF を解析できません: {}",
                ff_error(converted)
            )));
        }
        let value = dictionary_value(dict, "Orientation");
        ff::av_dict_free(&mut dict);
        let Some(value) = value else {
            return Ok(0);
        };
        let parsed = value.parse::<i32>().ok();
        match parsed {
            Some(orientation) if (1..=8).contains(&orientation) => Ok(orientation),
            _ => Err(MediaError::new(format!(
                "EXIF の orientation が不正です: {value}"
            ))),
        }
    }
}

pub fn frame_icc_profile(frame: &Frame) -> Option<Vec<u8>> {
    // SAFETY: ICC side data は frame が所有する。コピーしてから返す。
    unsafe {
        let side = ff::av_frame_get_side_data(
            frame.ptr,
            ff::AVFrameSideDataType::AV_FRAME_DATA_ICC_PROFILE,
        );
        if side.is_null() || (*side).data.is_null() || (*side).size == 0 {
            return None;
        }
        Some(std::slice::from_raw_parts((*side).data, (*side).size).to_vec())
    }
}

pub struct ImportedTexture {
    pub texture: wgpu::Texture,
    pub array_layer: u32,
    pub width: u32,
    pub height: u32,
}

pub fn import_d3d12_nv12(
    frame: &Frame,
    device: &wgpu::Device,
) -> Result<ImportedTexture, MediaError> {
    if !is_d3d12(frame.format()) {
        return Err(MediaError::new("D3D12VA の frame ではありません"));
    }
    // SAFETY: D3D12VA の frame->data[0] は AVD3D12VAFrame。texture は frame の buffer が
    // 参照を持つので、import 前に AddRef する。fence を待ってから wgpu へ渡す。
    unsafe {
        let desc = (*frame.ptr).data[0].cast::<D3d12Frame>();
        if desc.is_null() || (*desc).texture.is_null() {
            return Err(MediaError::new("D3D12VA frame に texture がありません"));
        }
        wait_fence((*desc).sync.fence, (*desc).sync.fence_value)?;
        let resource: windows::Win32::Graphics::Direct3D12::ID3D12Resource =
            windows::core::Interface::from_raw((*desc).texture);
        let raw_desc = resource.GetDesc();
        let flags = raw_desc.Flags;
        let reference_only =
            windows::Win32::Graphics::Direct3D12::D3D12_RESOURCE_FLAG_VIDEO_DECODE_REFERENCE_ONLY;
        let deny_shader =
            windows::Win32::Graphics::Direct3D12::D3D12_RESOURCE_FLAG_DENY_SHADER_RESOURCE;
        if flags.contains(reference_only) || flags.contains(deny_shader) {
            std::mem::forget(resource);
            return Err(MediaError::new(
                "D3D12VA の texture を shader から読めません",
            ));
        }
        if raw_desc.Format != windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_NV12 {
            std::mem::forget(resource);
            return Err(MediaError::new("D3D12VA の texture が NV12 ではありません"));
        }
        let width =
            u32::try_from(raw_desc.Width).map_err(|_| MediaError::new("texture の幅が不正です"))?;
        let height = raw_desc.Height;
        let layers = u32::from(raw_desc.DepthOrArraySize);
        let layer = u32::try_from((*desc).subresource_index).unwrap_or(u32::MAX);
        if layer >= layers {
            std::mem::forget(resource);
            return Err(MediaError::new("D3D12VA の array index が範囲外です"));
        }
        let extra = resource.clone();
        std::mem::forget(resource);
        let hal = wgpu_hal::dx12::Device::texture_from_raw(
            extra,
            wgpu::TextureFormat::NV12,
            wgpu::TextureDimension::D2,
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: layers,
            },
            1,
            1,
        );
        let texture = device.create_texture_from_hal::<wgpu_hal::api::Dx12>(
            hal,
            &wgpu::TextureDescriptor {
                label: Some("d3d12va-nv12"),
                size: wgpu::Extent3d {
                    width,
                    height,
                    depth_or_array_layers: layers,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::NV12,
                usage: wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            },
            wgpu::TextureUses::RESOURCE,
        );
        Ok(ImportedTexture {
            texture,
            array_layer: layer,
            width,
            height,
        })
    }
}

fn wait_fence(fence: *mut c_void, value: u64) -> Result<(), MediaError> {
    if fence.is_null() || value == 0 {
        return Ok(());
    }
    // SAFETY: fence は frame が所有する。from_raw の Drop で Release しないよう forget する。
    unsafe {
        let fence: windows::Win32::Graphics::Direct3D12::ID3D12Fence =
            windows::core::Interface::from_raw(fence);
        let completed = fence.GetCompletedValue();
        if completed < value {
            let event = windows::Win32::System::Threading::CreateEventW(None, false, false, None)
                .map_err(|error| {
                MediaError::new(format!("fence の event を作れません: {error}"))
            })?;
            fence
                .SetEventOnCompletion(value, event)
                .map_err(|error| MediaError::new(format!("fence を待てません: {error}")))?;
            windows::Win32::System::Threading::WaitForSingleObject(
                event,
                windows::Win32::System::Threading::INFINITE,
            );
        }
        std::mem::forget(fence);
    }
    Ok(())
}

pub struct AudioChunk {
    pub start_sample: i64,
    pub planes: Vec<Vec<f32>>,
}

pub struct AudioConverter {
    swr: *mut ff::SwrContext,
    channels: usize,
    sample_rate: i32,
    time_base: ff::AVRational,
    start_time: i64,
}

impl AudioConverter {
    pub fn new(decoder: &Decoder) -> Result<Self, MediaError> {
        // SAFETY: channel layout は codec context から複製し、関数内で uninit する。
        unsafe {
            let source_channels = (*decoder.codec).ch_layout.nb_channels;
            let sample_rate = (*decoder.codec).sample_rate;
            if source_channels <= 0 || sample_rate <= 0 {
                return Err(MediaError::new(
                    "音声の channel 数または sample rate が不正です",
                ));
            }
            let mut input = std::mem::zeroed::<ff::AVChannelLayout>();
            let mut output = std::mem::zeroed::<ff::AVChannelLayout>();
            if source_channels > 2
                && (*decoder.codec).ch_layout.order == ff::AVChannelOrder::AV_CHANNEL_ORDER_UNSPEC
            {
                ff::av_channel_layout_default(&mut input, source_channels);
            } else {
                let copied = ff::av_channel_layout_copy(&mut input, &(*decoder.codec).ch_layout);
                if copied < 0 {
                    return Err(MediaError::new(format!(
                        "音声の channel 配置を複製できません: {}",
                        ff_error(copied)
                    )));
                }
            }
            let output_code = if source_channels > 2 {
                ff::av_channel_layout_default(&mut output, 2);
                0
            } else {
                ff::av_channel_layout_copy(&mut output, &input)
            };
            if output_code < 0 {
                ff::av_channel_layout_uninit(&mut input);
                return Err(MediaError::new(format!(
                    "音声の channel 配置を複製できません: {}",
                    ff_error(output_code)
                )));
            }
            let channels = output.nb_channels as usize;
            let mut swr = ptr::null_mut();
            let configured = ff::swr_alloc_set_opts2(
                &mut swr,
                &output,
                ff::AVSampleFormat::AV_SAMPLE_FMT_FLTP,
                sample_rate,
                &input,
                (*decoder.codec).sample_fmt,
                sample_rate,
                0,
                ptr::null_mut(),
            );
            ff::av_channel_layout_uninit(&mut input);
            ff::av_channel_layout_uninit(&mut output);
            if configured < 0 || swr.is_null() {
                return Err(MediaError::new(format!(
                    "音声 format converter を初期化できません: {}",
                    ff_error(configured)
                )));
            }
            let inited = ff::swr_init(swr);
            if inited < 0 {
                ff::swr_free(&mut swr);
                return Err(MediaError::new(format!(
                    "音声 format converter を初期化できません: {}",
                    ff_error(inited)
                )));
            }
            Ok(Self {
                swr,
                channels,
                sample_rate,
                time_base: decoder.time_base,
                start_time: decoder.start_time,
            })
        }
    }

    pub fn channels(&self) -> usize {
        self.channels
    }

    pub fn sample_rate(&self) -> i32 {
        self.sample_rate
    }

    pub fn convert(&mut self, frame: &Frame) -> Result<AudioChunk, MediaError> {
        let pts = frame.best_effort_timestamp();
        if pts == ff::AV_NOPTS_VALUE {
            return Err(MediaError::new("音声 frame に PTS がありません"));
        }
        let start = if self.start_time == ff::AV_NOPTS_VALUE {
            0
        } else {
            self.start_time
        };
        let relative = pts
            .checked_sub(start)
            .ok_or_else(|| MediaError::new("音声PTSからstream開始PTSを減算できません"))?;
        // SAFETY: av_rescale_q は整数だけを受け取る純粋な計算。
        let start_sample = unsafe {
            ff::av_rescale_q(
                relative,
                self.time_base,
                ff::AVRational {
                    num: 1,
                    den: self.sample_rate,
                },
            )
        };
        if start_sample == i64::MIN {
            return Err(MediaError::new("音声PTSをsample位置へ換算できません"));
        }
        // SAFETY: frame.ptr は alloc 済みで、receive が書いた音声 frame。
        let samples = unsafe { (*frame.ptr).nb_samples };
        if samples < 0 {
            return Err(MediaError::new("音声 frame の sample 数が不正です"));
        }
        let mut planes = vec![vec![0f32; samples as usize]; self.channels];
        let mut outputs: Vec<*mut u8> = planes
            .iter_mut()
            .map(|plane| plane.as_mut_ptr().cast())
            .collect();
        // SAFETY: outputs は planes の各 channel を指す。入力は frame の extended_data。
        let converted = unsafe {
            ff::swr_convert(
                self.swr,
                outputs.as_mut_ptr(),
                samples,
                (*frame.ptr).extended_data as *const *const u8,
                samples,
            )
        };
        if converted < 0 {
            return Err(MediaError::new(format!(
                "音声 sample 変換に失敗しました: {}",
                ff_error(converted)
            )));
        }
        // SAFETY: self.swr は init が成功した context で、Drop まで null にならない。
        let delay = unsafe { ff::swr_get_delay(self.swr, i64::from(self.sample_rate)) };
        if converted != samples || delay != 0 {
            return Err(MediaError::new(
                "音声 format converter が sample を持ち越しました (frame の位置を決められません)",
            ));
        }
        Ok(AudioChunk {
            start_sample,
            planes,
        })
    }
}

impl Drop for AudioConverter {
    fn drop(&mut self) {
        if !self.swr.is_null() {
            // SAFETY: swr_alloc_set_opts2 が返した context を 1 回だけ解放する。swr_free は self.swr を null にする。
            unsafe { ff::swr_free(&mut self.swr) };
        }
    }
}

pub fn count_image_packets(input: &mut Input, stream_index: usize) -> Result<i32, MediaError> {
    let mut packet = Packet::new()?;
    let mut count = 0;
    while count < 2 {
        match input.read_packet(&mut packet)? {
            ReadStatus::Eof => break,
            ReadStatus::Packet => {
                if packet.stream_index() == Some(stream_index) {
                    count += 1;
                }
                packet.unref();
            }
        }
    }
    Ok(count)
}
