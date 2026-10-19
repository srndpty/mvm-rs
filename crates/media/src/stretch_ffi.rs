//! libavfilter の rubberband による time stretch の FFI。この module 以外に graph の pointer を出さない。
//!
//! graph は abuffer（48 kHz stereo の interleaved float）→ rubberband → aformat（同じ形）→ abuffersink。
//! 形式の変換（planar への変換）は graph が自動で挟む。

use std::ffi::CString;
use std::os::raw::c_int;
use std::ptr;

use ffmpeg_sys_next as ff;

use crate::error::MediaError;
use crate::ffi::ff_error;

const AVERROR_EAGAIN: c_int = -11;

pub struct StretchGraph {
    graph: *mut ff::AVFilterGraph,
    source: *mut ff::AVFilterContext,
    sink: *mut ff::AVFilterContext,
    frame: *mut ff::AVFrame,
    next_pts: i64,
}

// SAFETY: graph と frame はこの struct だけが所有し、共有しない。所有者の thread を移しても、
// FFmpeg の graph は thread に結び付いた状態を持たない。
unsafe impl Send for StretchGraph {}

fn text(value: &str) -> Result<CString, MediaError> {
    CString::new(value).map_err(|_| MediaError::new("filter の引数に NUL を含められません"))
}

/// `graph` に filter を 1 つ作る。
///
/// # Safety
/// `graph` は `avfilter_graph_alloc` が返した有効な pointer であること。
unsafe fn create_filter(
    graph: *mut ff::AVFilterGraph,
    filter_name: &str,
    instance: &str,
    args: Option<&str>,
) -> Result<*mut ff::AVFilterContext, MediaError> {
    let name = text(filter_name)?;
    let instance_name = text(instance)?;
    let args = args.map(text).transpose()?;
    // SAFETY: name は NUL 終端の文字列。戻り値は静的な filter の定義。
    let filter = unsafe { ff::avfilter_get_by_name(name.as_ptr()) };
    if filter.is_null() {
        return Err(MediaError::new(format!(
            "FFmpeg に {filter_name} filter がありません"
        )));
    }
    let mut context = ptr::null_mut();
    // SAFETY: graph は呼び出し側が保証する。文字列はこの関数の間だけ生きていればよい（FFmpeg が複製する）。
    let code = unsafe {
        ff::avfilter_graph_create_filter(
            &mut context,
            filter,
            instance_name.as_ptr(),
            args.as_ref().map_or(ptr::null(), |args| args.as_ptr()),
            ptr::null_mut(),
            graph,
        )
    };
    if code < 0 || context.is_null() {
        return Err(MediaError::new(format!(
            "{filter_name} filter を作れません: {}",
            ff_error(code)
        )));
    }
    Ok(context)
}

impl StretchGraph {
    /// `tempo` は素材の進む速さ（2.0 なら 2 倍速で、出力は半分の長さ）。
    pub fn new(sample_rate: i32, tempo: f64) -> Result<Self, MediaError> {
        if sample_rate <= 0 || !tempo.is_finite() || tempo <= 0.0 {
            return Err(MediaError::new("time stretch の設定が不正です"));
        }
        // SAFETY: 引数は無い。null は確保失敗として検査する。
        let graph = unsafe { ff::avfilter_graph_alloc() };
        if graph.is_null() {
            return Err(MediaError::new("filter graph を確保できません"));
        }
        // SAFETY: 引数は無い。null は確保失敗として検査する。
        let frame = unsafe { ff::av_frame_alloc() };
        let mut owned = Self {
            graph,
            source: ptr::null_mut(),
            sink: ptr::null_mut(),
            frame,
            next_pts: 0,
        };
        if frame.is_null() {
            return Err(MediaError::new("音声 frame を確保できません"));
        }
        let format = format!(
            "time_base=1/{sample_rate}:sample_rate={sample_rate}:sample_fmt=flt:channel_layout=stereo"
        );
        let shape = format!("sample_fmts=flt:sample_rates={sample_rate}:channel_layouts=stereo");
        let tempo_args = format!("tempo={tempo:.12}");
        // SAFETY: graph は上で確保し、owned が Drop で解放する。filter の context は graph が所有する。
        unsafe {
            owned.source = create_filter(graph, "abuffer", "in", Some(&format))?;
            let stretch = create_filter(graph, "rubberband", "stretch", Some(&tempo_args))?;
            let shaped = create_filter(graph, "aformat", "shape", Some(&shape))?;
            owned.sink = create_filter(graph, "abuffersink", "out", None)?;
            for (from, to) in [
                (owned.source, stretch),
                (stretch, shaped),
                (shaped, owned.sink),
            ] {
                let code = ff::avfilter_link(from, 0, to, 0);
                if code < 0 {
                    return Err(MediaError::new(format!(
                        "filter をつなげません: {}",
                        ff_error(code)
                    )));
                }
            }
            let code = ff::avfilter_graph_config(graph, ptr::null_mut());
            if code < 0 {
                return Err(MediaError::new(format!(
                    "filter graph を構成できません: {}",
                    ff_error(code)
                )));
            }
        }
        Ok(owned)
    }

    /// stereo の interleaved sample を入れる。空なら何もしない。
    pub fn push(&mut self, interleaved: &[f32], sample_rate: i32) -> Result<(), MediaError> {
        if !interleaved.len().is_multiple_of(2) {
            return Err(MediaError::new("stereo の sample 数が奇数です"));
        }
        let count = interleaved.len() / 2;
        if count == 0 {
            return Ok(());
        }
        let samples = c_int::try_from(count)
            .map_err(|_| MediaError::new("一度に入れる sample が多すぎます"))?;
        // SAFETY: self.frame は alloc が成功した pointer。buffer は av_frame_get_buffer が確保し、
        // data[0] は samples * 2 個の float を持つ（packed の flt）。add_frame が参照を引き取り、frame を空にする。
        unsafe {
            ff::av_frame_unref(self.frame);
            (*self.frame).format = ff::AVSampleFormat::AV_SAMPLE_FMT_FLT as c_int;
            (*self.frame).sample_rate = sample_rate;
            (*self.frame).nb_samples = samples;
            (*self.frame).pts = self.next_pts;
            ff::av_channel_layout_default(&mut (*self.frame).ch_layout, 2);
            let code = ff::av_frame_get_buffer(self.frame, 0);
            if code < 0 {
                return Err(MediaError::new(format!(
                    "音声 frame の buffer を確保できません: {}",
                    ff_error(code)
                )));
            }
            let data = (*self.frame).data[0].cast::<f32>();
            ptr::copy_nonoverlapping(interleaved.as_ptr(), data, interleaved.len());
            let code = ff::av_buffersrc_add_frame_flags(self.source, self.frame, 0);
            if code < 0 {
                return Err(MediaError::new(format!(
                    "time stretch へ音声を渡せません: {}",
                    ff_error(code)
                )));
            }
        }
        self.next_pts = self
            .next_pts
            .checked_add(count as i64)
            .ok_or_else(|| MediaError::new("time stretch の時刻が overflow しました"))?;
        Ok(())
    }

    /// 入力の終わりを知らせる。残りは続く `pull` で出る。
    pub fn finish(&mut self) -> Result<(), MediaError> {
        // SAFETY: null の frame は EOF の通知。source は graph が所有する有効な context。
        let code = unsafe { ff::av_buffersrc_add_frame_flags(self.source, ptr::null_mut(), 0) };
        if code < 0 {
            return Err(MediaError::new(format!(
                "time stretch を閉じられません: {}",
                ff_error(code)
            )));
        }
        Ok(())
    }

    /// 出せる分を `out` の末尾へ足す。終わりに達したら true。
    pub fn pull(&mut self, out: &mut Vec<f32>) -> Result<bool, MediaError> {
        loop {
            // SAFETY: sink は graph が所有する有効な context。frame は alloc が成功した pointer。
            let code = unsafe { ff::av_buffersink_get_frame(self.sink, self.frame) };
            if code == AVERROR_EAGAIN {
                return Ok(false);
            }
            if code == ff::AVERROR_EOF {
                return Ok(true);
            }
            if code < 0 {
                return Err(MediaError::new(format!(
                    "time stretch の出力を読めません: {}",
                    ff_error(code)
                )));
            }
            // SAFETY: aformat が packed の flt stereo に揃えたので、data[0] は nb_samples * 2 個の float を持つ。
            // 読み終えたら unref して buffer を返す。
            unsafe {
                let frame = &*self.frame;
                let valid = frame.format == ff::AVSampleFormat::AV_SAMPLE_FMT_FLT as c_int
                    && frame.ch_layout.nb_channels == 2
                    && frame.nb_samples >= 0;
                if !valid {
                    ff::av_frame_unref(self.frame);
                    return Err(MediaError::new("time stretch の出力の形式が想定と違います"));
                }
                let len = frame.nb_samples as usize * 2;
                let data = std::slice::from_raw_parts(frame.data[0].cast::<f32>(), len);
                out.extend_from_slice(data);
                ff::av_frame_unref(self.frame);
            }
        }
    }
}

impl Drop for StretchGraph {
    fn drop(&mut self) {
        // SAFETY: どちらも alloc が返した pointer を 1 回だけ解放する。free は pointer を null にする。
        // filter の context は graph と一緒に解放される。
        unsafe {
            if !self.frame.is_null() {
                ff::av_frame_free(&mut self.frame);
            }
            if !self.graph.is_null() {
                ff::avfilter_graph_free(&mut self.graph);
            }
        }
    }
}
