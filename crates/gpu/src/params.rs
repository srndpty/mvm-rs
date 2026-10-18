//! 合成パラメータの検査。GPU を使わない。

use crate::GpuError;

pub const MAX_DIMENSION: u32 = 16384;
pub const MAX_PIXELS: u64 = 64 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceKind {
    Nv12,
    P010,
    Rgba,
    Nv12Texture,
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
            SourceKind::Nv12Texture => (0, 0),
        };
        Self {
            width,
            height,
            stride_y,
            stride_uv,
            kind,
            affine: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
            crop: [0, 0, width, height],
            opacity: 1.0,
        }
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
    let chroma = matches!(params.kind, SourceKind::Nv12 | SourceKind::P010);
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
    if params.kind != SourceKind::Nv12Texture {
        let y_bytes = match params.kind {
            SourceKind::Nv12 => 1,
            SourceKind::P010 => 2,
            SourceKind::Rgba => 4,
            SourceKind::Nv12Texture => 0,
        };
        let min_y = params
            .width
            .checked_mul(y_bytes)
            .ok_or_else(|| GpuError::message("stride が溢れました"))?;
        if params.stride_y < min_y {
            return Err(GpuError::message("Y / RGBA の stride が幅より短いです"));
        }
        if chroma {
            let min_uv = match params.kind {
                SourceKind::Nv12 => params.width,
                SourceKind::P010 => params
                    .width
                    .checked_mul(2)
                    .ok_or_else(|| GpuError::message("stride が溢れました"))?,
                _ => 0,
            };
            if params.stride_uv < min_uv {
                return Err(GpuError::message("UV の stride が幅より短いです"));
            }
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
    use super::*;

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
}
