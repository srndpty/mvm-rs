//! MP4（H.264 / AAC）の encode と mux の FFI。`unsafe` はこの module と [`crate::ffi`] だけに書く。
//!
//! 映像は straight RGBA を swscale で BT.709 limited の YUV420P にし、libx264 へ渡す。
//! 音声は 48 kHz stereo の float planar を FFmpeg の AAC encoder へ渡す。

use std::ffi::CString;
use std::os::raw::{c_int, c_void};
use std::path::Path;
use std::ptr;

use ffmpeg_sys_next as ff;

use crate::error::MediaError;
use crate::ffi::{ff_error, path_cstring};

unsafe extern "C" {
    /// bindgen の enum を経由せず、画素形式を整数 ABI のまま渡す（[`crate::ffi`] と同じ）。
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
}

const AUDIO_SAMPLE_RATE: c_int = 48_000;
const AUDIO_CHANNELS: c_int = 2;
const AUDIO_BIT_RATE: i64 = 192_000;

/// 検査済みの出力設定。範囲の検査は呼び出し側（[`crate::encode`]）が行う。
pub struct MuxerConfig {
    pub width: c_int,
    pub height: c_int,
    pub fps_num: c_int,
    pub fps_den: c_int,
    pub crf: c_int,
    pub audio: bool,
}

/// 1 本の MP4 の出力。途中で drop すると file は閉じるだけで、中身は不完全なまま残る。
pub struct Muxer {
    format: *mut ff::AVFormatContext,
    video: *mut ff::AVCodecContext,
    video_stream: *mut ff::AVStream,
    audio: *mut ff::AVCodecContext,
    audio_stream: *mut ff::AVStream,
    sws: *mut ff::SwsContext,
    picture: *mut ff::AVFrame,
    sound: *mut ff::AVFrame,
    packet: *mut ff::AVPacket,
    width: c_int,
    height: c_int,
    io_open: bool,
    finished: bool,
}

impl Muxer {
    pub fn create(path: &Path, config: &MuxerConfig) -> Result<Self, MediaError> {
        let text = path_cstring(path)?;
        // libx264 と AAC は frame ごとの統計を info で出す。書き出しの記録には warning 以上だけを残す。
        // SAFETY: 整数を渡すだけ。FFmpeg の log level は process 全体の設定。
        unsafe { ff::av_log_set_level(ff::AV_LOG_WARNING) };
        let mut muxer = Self {
            format: ptr::null_mut(),
            video: ptr::null_mut(),
            video_stream: ptr::null_mut(),
            audio: ptr::null_mut(),
            audio_stream: ptr::null_mut(),
            sws: ptr::null_mut(),
            picture: ptr::null_mut(),
            sound: ptr::null_mut(),
            packet: ptr::null_mut(),
            width: config.width,
            height: config.height,
            io_open: false,
            finished: false,
        };
        // SAFETY: 名前は NUL 終端。成功した context は muxer が持ち、Drop で解放する。
        let code = unsafe {
            ff::avformat_alloc_output_context2(
                &mut muxer.format,
                ptr::null(),
                c"mp4".as_ptr(),
                text.as_ptr(),
            )
        };
        if code < 0 || muxer.format.is_null() {
            return Err(MediaError::new(format!(
                "MP4 の出力を用意できません: {}",
                ff_error(code)
            )));
        }
        muxer.open_video(config)?;
        if config.audio {
            muxer.open_audio()?;
        }
        muxer.open_scaler()?;
        // SAFETY: 引数は無い。null は確保失敗として検査し、成功した pointer は Drop で解放する。
        muxer.packet = unsafe { ff::av_packet_alloc() };
        // SAFETY: 同上。
        muxer.sound = unsafe { ff::av_frame_alloc() };
        if muxer.packet.is_null() || muxer.sound.is_null() {
            return Err(MediaError::new("encode の buffer を確保できません"));
        }
        // SAFETY: format は alloc 済み。pb は avio_open が書き、Drop か finish で閉じる。
        let opened =
            unsafe { ff::avio_open(&mut (*muxer.format).pb, text.as_ptr(), ff::AVIO_FLAG_WRITE) };
        if opened < 0 {
            return Err(MediaError::new(format!(
                "書き出し先を開けません ({}): {}",
                path.display(),
                ff_error(opened)
            )));
        }
        muxer.io_open = true;
        let mut options = ptr::null_mut();
        // SAFETY: key と value は NUL 終端。options はこの block の中で解放する。
        let written = unsafe {
            ff::av_dict_set(
                &mut options,
                c"movflags".as_ptr(),
                c"+faststart".as_ptr(),
                0,
            );
            let written = ff::avformat_write_header(muxer.format, &mut options);
            ff::av_dict_free(&mut options);
            written
        };
        if written < 0 {
            return Err(MediaError::new(format!(
                "MP4 の header を書けません: {}",
                ff_error(written)
            )));
        }
        Ok(muxer)
    }

    fn open_video(&mut self, config: &MuxerConfig) -> Result<(), MediaError> {
        // SAFETY: format は alloc 済み。codec context は self が持ち、Drop で解放する。
        // stream は format が持つ。
        unsafe {
            let codec = ff::avcodec_find_encoder_by_name(c"libx264".as_ptr());
            if codec.is_null() {
                return Err(MediaError::new(
                    "H.264 の encoder (libx264) がありません。gpl 版の FFmpeg が要ります",
                ));
            }
            self.video_stream = ff::avformat_new_stream(self.format, ptr::null());
            self.video = ff::avcodec_alloc_context3(codec);
            if self.video_stream.is_null() || self.video.is_null() {
                return Err(MediaError::new("映像の encoder を確保できません"));
            }
            let context = self.video;
            (*context).width = config.width;
            (*context).height = config.height;
            (*context).time_base = ff::AVRational {
                num: config.fps_den,
                den: config.fps_num,
            };
            (*context).framerate = ff::AVRational {
                num: config.fps_num,
                den: config.fps_den,
            };
            (*context).pix_fmt = ff::AVPixelFormat::AV_PIX_FMT_YUV420P;
            (*context).sample_aspect_ratio = ff::AVRational { num: 1, den: 1 };
            (*context).color_range = ff::AVColorRange::AVCOL_RANGE_MPEG;
            (*context).colorspace = ff::AVColorSpace::AVCOL_SPC_BT709;
            (*context).color_primaries = ff::AVColorPrimaries::AVCOL_PRI_BT709;
            (*context).color_trc = ff::AVColorTransferCharacteristic::AVCOL_TRC_BT709;
            if (*(*self.format).oformat).flags & ff::AVFMT_GLOBALHEADER != 0 {
                (*context).flags |= ff::AV_CODEC_FLAG_GLOBAL_HEADER as c_int;
            }
            let crf = CString::new(config.crf.to_string())
                .map_err(|_| MediaError::new("CRF を文字列にできません"))?;
            let mut options = ptr::null_mut();
            ff::av_dict_set(&mut options, c"preset".as_ptr(), c"medium".as_ptr(), 0);
            ff::av_dict_set(&mut options, c"crf".as_ptr(), crf.as_ptr(), 0);
            let opened = ff::avcodec_open2(context, codec, &mut options);
            ff::av_dict_free(&mut options);
            if opened < 0 {
                return Err(MediaError::new(format!(
                    "H.264 の encoder を初期化できません: {}",
                    ff_error(opened)
                )));
            }
            let copied =
                ff::avcodec_parameters_from_context((*self.video_stream).codecpar, context);
            if copied < 0 {
                return Err(MediaError::new(format!(
                    "映像の stream を設定できません: {}",
                    ff_error(copied)
                )));
            }
            (*self.video_stream).time_base = (*context).time_base;
            (*self.video_stream).avg_frame_rate = (*context).framerate;
            (*self.video_stream).sample_aspect_ratio = (*context).sample_aspect_ratio;

            self.picture = ff::av_frame_alloc();
            if self.picture.is_null() {
                return Err(MediaError::new("映像の frame を確保できません"));
            }
            (*self.picture).format = ff::AVPixelFormat::AV_PIX_FMT_YUV420P as c_int;
            (*self.picture).width = config.width;
            (*self.picture).height = config.height;
            (*self.picture).color_range = ff::AVColorRange::AVCOL_RANGE_MPEG;
            (*self.picture).colorspace = ff::AVColorSpace::AVCOL_SPC_BT709;
            (*self.picture).color_primaries = ff::AVColorPrimaries::AVCOL_PRI_BT709;
            (*self.picture).color_trc = ff::AVColorTransferCharacteristic::AVCOL_TRC_BT709;
            let allocated = ff::av_frame_get_buffer(self.picture, 0);
            if allocated < 0 {
                return Err(MediaError::new(format!(
                    "映像の frame を確保できません: {}",
                    ff_error(allocated)
                )));
            }
        }
        Ok(())
    }

    fn open_audio(&mut self) -> Result<(), MediaError> {
        // SAFETY: format は alloc 済み。codec context は self が持ち、Drop で解放する。
        // stream は format が持つ。
        unsafe {
            let codec = ff::avcodec_find_encoder(ff::AVCodecID::AV_CODEC_ID_AAC);
            if codec.is_null() {
                return Err(MediaError::new("AAC の encoder がありません"));
            }
            self.audio_stream = ff::avformat_new_stream(self.format, ptr::null());
            self.audio = ff::avcodec_alloc_context3(codec);
            if self.audio_stream.is_null() || self.audio.is_null() {
                return Err(MediaError::new("音声の encoder を確保できません"));
            }
            let context = self.audio;
            (*context).sample_fmt = ff::AVSampleFormat::AV_SAMPLE_FMT_FLTP;
            (*context).sample_rate = AUDIO_SAMPLE_RATE;
            ff::av_channel_layout_default(&mut (*context).ch_layout, AUDIO_CHANNELS);
            (*context).bit_rate = AUDIO_BIT_RATE;
            (*context).time_base = ff::AVRational {
                num: 1,
                den: AUDIO_SAMPLE_RATE,
            };
            if (*(*self.format).oformat).flags & ff::AVFMT_GLOBALHEADER != 0 {
                (*context).flags |= ff::AV_CODEC_FLAG_GLOBAL_HEADER as c_int;
            }
            let opened = ff::avcodec_open2(context, codec, ptr::null_mut());
            if opened < 0 {
                return Err(MediaError::new(format!(
                    "AAC の encoder を初期化できません: {}",
                    ff_error(opened)
                )));
            }
            if (*context).frame_size <= 0 {
                return Err(MediaError::new("AAC の frame の長さが不正です"));
            }
            let copied =
                ff::avcodec_parameters_from_context((*self.audio_stream).codecpar, context);
            if copied < 0 {
                return Err(MediaError::new(format!(
                    "音声の stream を設定できません: {}",
                    ff_error(copied)
                )));
            }
            (*self.audio_stream).time_base = (*context).time_base;
        }
        Ok(())
    }

    fn open_scaler(&mut self) -> Result<(), MediaError> {
        let flags = ff::SwsFlags::SWS_BICUBIC as c_int
            | ff::SwsFlags::SWS_ACCURATE_RND as c_int
            | ff::SwsFlags::SWS_FULL_CHR_H_INP as c_int;
        // SAFETY: 寸法は検査済みの正の値。成功した context は Drop で解放する。
        // 係数表は FFmpeg の静的な表を指す。
        unsafe {
            self.sws = sws_get_context_int(
                self.width,
                self.height,
                ff::AVPixelFormat::AV_PIX_FMT_RGBA as c_int,
                self.width,
                self.height,
                ff::AVPixelFormat::AV_PIX_FMT_YUV420P as c_int,
                flags,
                ptr::null_mut(),
                ptr::null_mut(),
                ptr::null(),
            );
            if self.sws.is_null() {
                return Err(MediaError::new("映像の色変換を初期化できません"));
            }
            // 入力は full range の RGB、出力は BT.709 の limited range。
            let configured = ff::sws_setColorspaceDetails(
                self.sws,
                ff::sws_getCoefficients(ff::SWS_CS_ITU709),
                1,
                ff::sws_getCoefficients(ff::SWS_CS_ITU709),
                0,
                0,
                1 << 16,
                1 << 16,
            );
            if configured < 0 {
                return Err(MediaError::new("映像の色空間を設定できません"));
            }
        }
        Ok(())
    }

    /// AAC の 1 frame の sample 数。最後の 1 回だけ、これより短くてよい。
    pub fn audio_frame_size(&self) -> Option<usize> {
        if self.audio.is_null() {
            return None;
        }
        // SAFETY: self.audio は open が成功した context で、Drop まで null にならない。
        let size = unsafe { (*self.audio).frame_size };
        usize::try_from(size).ok()
    }

    /// 行を詰めた straight RGBA の 1 frame を、`pts`（frame 番号）として encode する。
    pub fn write_video(&mut self, rgba: &[u8], pts: i64) -> Result<(), MediaError> {
        self.ensure_open()?;
        let stride = self
            .width
            .checked_mul(4)
            .ok_or_else(|| MediaError::new("映像の stride が溢れました"))?;
        let expected = usize::try_from(stride)
            .ok()
            .zip(usize::try_from(self.height).ok())
            .and_then(|(stride, height)| stride.checked_mul(height))
            .ok_or_else(|| MediaError::new("映像の大きさが溢れました"))?;
        if rgba.len() != expected {
            return Err(MediaError::new(format!(
                "映像の画素数が出力と違います ({} byte, 期待 {expected} byte)",
                rgba.len()
            )));
        }
        // SAFETY: picture は get_buffer 済みの YUV420P。make_writable の後に sws が全 plane を書く。
        // 入力は長さを検査した RGBA で、sws は読むだけ。
        unsafe {
            let writable = ff::av_frame_make_writable(self.picture);
            if writable < 0 {
                return Err(MediaError::new(format!(
                    "映像の frame に書けません: {}",
                    ff_error(writable)
                )));
            }
            let source = [rgba.as_ptr(), ptr::null(), ptr::null(), ptr::null()];
            let source_stride = [stride, 0, 0, 0];
            let converted = ff::sws_scale(
                self.sws,
                source.as_ptr(),
                source_stride.as_ptr(),
                0,
                self.height,
                (*self.picture).data.as_ptr(),
                (*self.picture).linesize.as_ptr(),
            );
            if converted != self.height {
                return Err(MediaError::new("映像の色変換に失敗しました"));
            }
            (*self.picture).pts = pts;
        }
        self.send(self.video, self.video_stream, self.picture)
    }

    /// 左右の float を `pts`（48 kHz の sample 番号）から encode する。
    pub fn write_audio(&mut self, left: &[f32], right: &[f32], pts: i64) -> Result<(), MediaError> {
        self.ensure_open()?;
        let Some(frame_size) = self.audio_frame_size() else {
            return Err(MediaError::new("音声の stream がありません"));
        };
        if left.len() != right.len() || left.is_empty() || left.len() > frame_size {
            return Err(MediaError::new("音声の frame の長さが不正です"));
        }
        let samples = c_int::try_from(left.len())
            .map_err(|_| MediaError::new("音声の frame が長すぎます"))?;
        // SAFETY: sound は alloc 済み。unref で前の buffer を手放し、get_buffer が planar の 2 plane を
        // samples 個ずつ確保する。copy はその範囲だけに書く。
        unsafe {
            ff::av_frame_unref(self.sound);
            (*self.sound).nb_samples = samples;
            (*self.sound).format = ff::AVSampleFormat::AV_SAMPLE_FMT_FLTP as c_int;
            (*self.sound).sample_rate = AUDIO_SAMPLE_RATE;
            let copied =
                ff::av_channel_layout_copy(&mut (*self.sound).ch_layout, &(*self.audio).ch_layout);
            if copied < 0 {
                return Err(MediaError::new(format!(
                    "音声の channel 配置を複製できません: {}",
                    ff_error(copied)
                )));
            }
            let allocated = ff::av_frame_get_buffer(self.sound, 0);
            if allocated < 0 {
                return Err(MediaError::new(format!(
                    "音声の frame を確保できません: {}",
                    ff_error(allocated)
                )));
            }
            let planes = (*self.sound).data;
            if planes[0].is_null() || planes[1].is_null() {
                return Err(MediaError::new("音声の frame の plane がありません"));
            }
            ptr::copy_nonoverlapping(left.as_ptr(), planes[0].cast::<f32>(), left.len());
            ptr::copy_nonoverlapping(right.as_ptr(), planes[1].cast::<f32>(), right.len());
            (*self.sound).pts = pts;
        }
        self.send(self.audio, self.audio_stream, self.sound)
    }

    /// encoder に残った packet を書き、trailer を書いて file を閉じる。
    pub fn finish(&mut self) -> Result<(), MediaError> {
        self.ensure_open()?;
        self.send(self.video, self.video_stream, ptr::null())?;
        if !self.audio.is_null() {
            self.send(self.audio, self.audio_stream, ptr::null())?;
        }
        // SAFETY: header を書いた format。trailer の後に pb を閉じる。
        let trailer = unsafe { ff::av_write_trailer(self.format) };
        if trailer < 0 {
            return Err(MediaError::new(format!(
                "MP4 の trailer を書けません: {}",
                ff_error(trailer)
            )));
        }
        self.finished = true;
        // SAFETY: avio_open が開いた pb を 1 回だけ閉じる。avio_closep は pb を null にする。
        let closed = unsafe { ff::avio_closep(&mut (*self.format).pb) };
        self.io_open = false;
        if closed < 0 {
            return Err(MediaError::new(format!(
                "書き出した file を閉じられません: {}",
                ff_error(closed)
            )));
        }
        Ok(())
    }

    fn ensure_open(&self) -> Result<(), MediaError> {
        if self.finished || !self.io_open {
            return Err(MediaError::new("書き出しはもう閉じています"));
        }
        Ok(())
    }

    /// `frame` が null なら flush。出てきた packet を全部書く。
    fn send(
        &mut self,
        context: *mut ff::AVCodecContext,
        stream: *mut ff::AVStream,
        frame: *const ff::AVFrame,
    ) -> Result<(), MediaError> {
        // SAFETY: context と stream は open が成功したもので、Drop まで null にならない。
        // frame は self が持つ frame か null（flush）。
        let sent = unsafe { ff::avcodec_send_frame(context, frame) };
        if sent < 0 {
            return Err(MediaError::new(format!(
                "encode できません: {}",
                ff_error(sent)
            )));
        }
        loop {
            // SAFETY: 同上。packet は alloc 済みで、受け取った中身は write が unref する。
            let received = unsafe { ff::avcodec_receive_packet(context, self.packet) };
            if received == ff::AVERROR(ff::EAGAIN as c_int) || received == ff::AVERROR_EOF {
                return Ok(());
            }
            if received < 0 {
                return Err(MediaError::new(format!(
                    "encode した packet を受け取れません: {}",
                    ff_error(received)
                )));
            }
            // SAFETY: packet は receive が書いた有効な packet。interleaved_write が所有を引き取る。
            let written = unsafe {
                ff::av_packet_rescale_ts(self.packet, (*context).time_base, (*stream).time_base);
                (*self.packet).stream_index = (*stream).index;
                ff::av_interleaved_write_frame(self.format, self.packet)
            };
            if written < 0 {
                return Err(MediaError::new(format!(
                    "MP4 へ書けません: {}",
                    ff_error(written)
                )));
            }
        }
    }
}

impl Drop for Muxer {
    fn drop(&mut self) {
        // SAFETY: どの pointer も、確保した物を 1 回だけ解放する。free 系の関数は null を受け付け、
        // pointer を null にする。stream は format_free が解放する。
        unsafe {
            if self.io_open && !self.format.is_null() {
                ff::avio_closep(&mut (*self.format).pb);
            }
            if !self.sws.is_null() {
                ff::sws_freeContext(self.sws);
                self.sws = ptr::null_mut();
            }
            ff::av_frame_free(&mut self.picture);
            ff::av_frame_free(&mut self.sound);
            ff::av_packet_free(&mut self.packet);
            ff::avcodec_free_context(&mut self.video);
            ff::avcodec_free_context(&mut self.audio);
            if !self.format.is_null() {
                ff::avformat_free_context(self.format);
                self.format = ptr::null_mut();
            }
        }
    }
}
