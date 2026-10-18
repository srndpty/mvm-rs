//! 合成パラメータの検査。GPU を使わない。

use mvm_core::{YuvMatrix, YuvRange};

use crate::GpuError;
use crate::yuv::{self, YuvDepth};

/// uniform は 24 個の f32（96 byte）。WGSL の `Params` と同じ並び。
pub(crate) const UNIFORM_BYTES: u64 = 96;

pub const MAX_DIMENSION: u32 = 16384;
pub const MAX_PIXELS: u64 = 64 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceKind {
    Nv12,
    P010,
    Rgba,
    /// D3D12VA から import した NV12。
    Nv12Texture,
    /// D3D12VA から import した P010。
    P010Texture,
}

/// affine は出力 pixel の中心 `(x, y)` を素材 pixel へ写す。
/// `sx = affine[0] * x + affine[1] * y + affine[2]`
/// `sy = affine[3] * x + affine[4] * y + affine[5]`
/// crop は素材 pixel の `[x, y, width, height]`。
#[derive(Clone, Copy, Debug)]
pub struct LayerParams {
    pub width: u32,
    pub height: u32,
    pub stride_y: u32,
    pub stride_uv: u32,
    pub kind: SourceKind,
    pub matrix: YuvMatrix,
    pub range: YuvRange,
    pub affine: [f32; 6],
    pub crop: [u32; 4],
    pub opacity: f32,
}

impl LayerParams {
    pub fn full(kind: SourceKind, width: u32, height: u32) -> Self {
        let (stride_y, stride_uv) = match kind {
            SourceKind::Nv12 => (width, width),
            SourceKind::P010 => (width.saturating_mul(2), width.saturating_mul(2)),
            SourceKind::Rgba => (width.saturating_mul(4), 0),
            SourceKind::Nv12Texture | SourceKind::P010Texture => (0, 0),
        };
        Self {
            width,
            height,
            stride_y,
            stride_uv,
            kind,
            matrix: YuvMatrix::Bt709,
            range: YuvRange::Limited,
            affine: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
            crop: [0, 0, width, height],
            opacity: 1.0,
        }
    }
}

pub(crate) fn uniform_words(params: &LayerParams) -> [f32; 24] {
    let mut data = [0f32; 24];
    data[0] = params.affine[0];
    data[1] = params.affine[1];
    data[2] = params.affine[2];
    data[3] = params.affine[3];
    data[4] = params.affine[4];
    data[5] = params.affine[5];
    data[6] = params.opacity;
    data[7] = kind_code(params.kind);
    data[8] = params.crop[0] as f32;
    data[9] = params.crop[1] as f32;
    data[10] = params.crop[2] as f32;
    data[11] = params.crop[3] as f32;
    data[12] = params.width as f32;
    data[13] = params.height as f32;
    if let Some(depth) = depth_of(params.kind) {
        let (mul, range) = yuv::coeffs(params.matrix, params.range, depth);
        data[16..20].copy_from_slice(&mul);
        data[20..24].copy_from_slice(&range);
    }
    data
}

fn kind_code(kind: SourceKind) -> f32 {
    match kind {
        SourceKind::Nv12 | SourceKind::Nv12Texture => 0.0,
        SourceKind::P010 | SourceKind::P010Texture => 1.0,
        SourceKind::Rgba => 2.0,
    }
}

fn depth_of(kind: SourceKind) -> Option<YuvDepth> {
    match kind {
        SourceKind::Nv12 | SourceKind::Nv12Texture => Some(YuvDepth::Bit8),
        SourceKind::P010 | SourceKind::P010Texture => Some(YuvDepth::Bit10),
        SourceKind::Rgba => None,
    }
}

pub fn validate_output(width: u32, height: u32) -> Result<(), GpuError> {
    check_size(width, height, false)
}

pub(crate) fn validate_layer(
    output_width: u32,
    output_height: u32,
    params: &LayerParams,
) -> Result<(), GpuError> {
    validate_output(output_width, output_height)?;
    let chroma = matches!(
        params.kind,
        SourceKind::Nv12 | SourceKind::P010 | SourceKind::Nv12Texture | SourceKind::P010Texture
    );
    check_size(params.width, params.height, chroma)?;
    if !params.opacity.is_finite() || !(0.0..=1.0).contains(&params.opacity) {
        return Err(GpuError::message("opacity は 0 から 1 の範囲で指定します"));
    }
    if params.affine.iter().any(|value| !value.is_finite()) {
        return Err(GpuError::message("affine の係数が不正です"));
    }
    let [x, y, crop_w, crop_h] = params.crop;
    if crop_w == 0 || crop_h == 0 {
        return Err(GpuError::message("crop の幅と高さは 1 以上です"));
    }
    let crop_right = x
        .checked_add(crop_w)
        .ok_or_else(|| GpuError::message("crop が素材の外に出ています"))?;
    let crop_bottom = y
        .checked_add(crop_h)
        .ok_or_else(|| GpuError::message("crop が素材の外に出ています"))?;
    if crop_right > params.width || crop_bottom > params.height {
        return Err(GpuError::message("crop が素材の外に出ています"));
    }
    let y_bytes = match params.kind {
        SourceKind::Nv12Texture | SourceKind::P010Texture => return Ok(()),
        SourceKind::Nv12 => 1,
        SourceKind::P010 => 2,
        SourceKind::Rgba => 4,
    };
    let min_y = params
        .width
        .checked_mul(y_bytes)
        .ok_or_else(|| GpuError::message("stride が溢れました"))?;
    if params.stride_y < min_y {
        return Err(GpuError::message("Y / RGBA の stride が幅より短いです"));
    }
    if params.kind != SourceKind::Rgba {
        let min_uv = if params.kind == SourceKind::P010 {
            params
                .width
                .checked_mul(2)
                .ok_or_else(|| GpuError::message("stride が溢れました"))?
        } else {
            params.width
        };
        if params.stride_uv < min_uv {
            return Err(GpuError::message("UV の stride が幅より短いです"));
        }
    }
    Ok(())
}

fn check_size(width: u32, height: u32, even: bool) -> Result<(), GpuError> {
    if width == 0 || height == 0 || width > MAX_DIMENSION || height > MAX_DIMENSION {
        return Err(GpuError::message(
            "幅と高さは 1 から 16384 の範囲で指定します",
        ));
    }
    if even && (!width.is_multiple_of(2) || !height.is_multiple_of(2)) {
        return Err(GpuError::message(
            "NV12 / P010 の幅と高さは偶数で指定します",
        ));
    }
    let pixels = u64::from(width) * u64::from(height);
    if pixels > MAX_PIXELS {
        return Err(GpuError::message("画素数が上限を超えています"));
    }
    Ok(())
}

impl GpuError {
    pub(crate) fn message(text: &'static str) -> Self {
        Self {
            message: text.to_owned(),
        }
    }
}

#[cfg(test)]
mod tests {
    use mvm_core::{YuvMatrix, YuvRange};

    use super::*;
    use crate::yuv::{self, YuvDepth};

    #[test]
    fn accepts_a_full_nv12_layer() {
        let params = LayerParams::full(SourceKind::Nv12, 1920, 1080);
        assert!(validate_layer(1920, 1080, &params).is_ok());
    }

    #[test]
    fn rejects_odd_nv12_and_opacity_outside_the_unit_interval() {
        let mut odd = LayerParams::full(SourceKind::Nv12, 1919, 1080);
        odd.width = 1919;
        assert!(validate_layer(1920, 1080, &odd).is_err());
        let texture = LayerParams::full(SourceKind::P010Texture, 1919, 1080);
        assert!(validate_layer(1920, 1080, &texture).is_err());
        let mut opacity = LayerParams::full(SourceKind::Rgba, 64, 64);
        opacity.opacity = 1.1;
        assert!(validate_layer(64, 64, &opacity).is_err());
        opacity.opacity = -0.01;
        assert!(validate_layer(64, 64, &opacity).is_err());
    }

    #[test]
    fn rejects_a_crop_past_the_source() {
        let mut params = LayerParams::full(SourceKind::Rgba, 64, 64);
        params.crop = [60, 0, 8, 8];
        assert!(validate_layer(64, 64, &params).is_err());
    }

    #[test]
    fn rejects_a_zero_output() {
        assert!(validate_output(0, 1080).is_err());
    }

    #[test]
    fn uniform_carries_the_selected_matrix_and_range() {
        let mut params = LayerParams::full(SourceKind::Nv12, 64, 64);
        params.matrix = YuvMatrix::Bt601;
        params.range = YuvRange::Full;
        let words = uniform_words(&params);
        let (mul, range) = yuv::coeffs(YuvMatrix::Bt601, YuvRange::Full, YuvDepth::Bit8);
        assert_eq!(&words[16..20], &mul);
        assert_eq!(&words[20..24], &range);
        assert_eq!(words[7], 0.0);
        let defaults = uniform_words(&LayerParams::full(SourceKind::Nv12, 64, 64));
        assert_ne!(&words[16..20], &defaults[16..20]);
        assert_ne!(&words[20..24], &defaults[20..24]);
        let p010 = uniform_words(&LayerParams::full(SourceKind::P010Texture, 64, 64));
        assert_eq!(p010[7], 1.0);
        let rgba = uniform_words(&LayerParams::full(SourceKind::Rgba, 64, 64));
        assert_eq!(rgba[7], 2.0);
        assert_eq!(&rgba[16..24], &[0.0; 8]);
    }
}
