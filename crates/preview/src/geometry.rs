//! effect と letterbox から、compositor の affine と crop を作る。GPU は使わない。

use mvm_core::layer_placement::place_layer;
use mvm_gpu::{LayerParams, SourceKind, YuvMatrix, YuvRange};
use mvm_project::clip_effects::{ClipEffects, map_clip_effects};

use crate::error::PreviewError;

/// 素材を canvas へ置くパラメータ。crop で何も見えなければ `None`。
#[allow(clippy::too_many_arguments)]
pub fn layer_params(
    frame_width: u32,
    frame_height: u32,
    canvas_width: u32,
    canvas_height: u32,
    effects: &ClipEffects,
    opacity: f32,
    kind: SourceKind,
    matrix: YuvMatrix,
    range: YuvRange,
) -> Result<Option<LayerParams>, PreviewError> {
    let frame_width_i =
        i32::try_from(frame_width).map_err(|_| PreviewError::new("素材の幅が大きすぎます"))?;
    let frame_height_i =
        i32::try_from(frame_height).map_err(|_| PreviewError::new("素材の高さが大きすぎます"))?;
    let canvas_width_i =
        i32::try_from(canvas_width).map_err(|_| PreviewError::new("出力の幅が大きすぎます"))?;
    let canvas_height_i =
        i32::try_from(canvas_height).map_err(|_| PreviewError::new("出力の高さが大きすぎます"))?;
    let mapped = map_clip_effects(effects);
    let Some(placed) = place_layer(
        frame_width_i,
        frame_height_i,
        canvas_width_i,
        canvas_height_i,
        &mapped.source_rect,
        &mapped.destination_rect,
    ) else {
        return Ok(None);
    };

    let canvas_w = f64::from(canvas_width);
    let canvas_h = f64::from(canvas_height);
    let frame_w = f64::from(frame_width);
    let frame_h = f64::from(frame_height);
    let dx = placed.destination.x * canvas_w;
    let dy = placed.destination.y * canvas_h;
    let dw = placed.destination.width * canvas_w;
    let dh = placed.destination.height * canvas_h;
    let (sx0, sw) = pixel_span(placed.source_uv.x, placed.source_uv.width, frame_w)?;
    let (sy0, sh) = pixel_span(placed.source_uv.y, placed.source_uv.height, frame_h)?;
    if dw <= 0.0 || dh <= 0.0 || sw <= 0.0 || sh <= 0.0 {
        return Ok(None);
    }

    let (sin, cos) = mapped.rotation_degrees.to_radians().sin_cos();
    let px = placed.pivot_x * canvas_w;
    let py = placed.pivot_y * canvas_h;
    // 画面上の回転の逆。出力 pixel の中心を素材 pixel へ戻す。
    let x_bias = px - cos * px - sin * py;
    let y_bias = py + sin * px - cos * py;
    let sx_scale = sw / dw;
    let sy_scale = sh / dh;
    let affine = [
        sx_scale * cos,
        sx_scale * sin,
        sx_scale * x_bias + sx0 - sx_scale * dx,
        sy_scale * -sin,
        sy_scale * cos,
        sy_scale * y_bias + sy0 - sy_scale * dy,
    ];
    if affine.iter().any(|value| !value.is_finite()) {
        return Err(PreviewError::new("layer の変換係数が不正です"));
    }

    let crop_x = floor_u32(sx0)?;
    let crop_y = floor_u32(sy0)?;
    let crop_w = span_u32(sw, frame_width, crop_x)?;
    let crop_h = span_u32(sh, frame_height, crop_y)?;
    let mut params = LayerParams::full(kind, frame_width, frame_height);
    params.matrix = matrix;
    params.range = range;
    params.opacity = opacity;
    params.affine = affine.map(|value| value as f32);
    params.crop = [crop_x, crop_y, crop_w, crop_h];
    Ok(Some(params))
}

/// UV 区間を素材 pixel の始点と幅にする。
fn pixel_span(origin: f64, size: f64, limit: f64) -> Result<(f64, f64), PreviewError> {
    if !origin.is_finite() || !size.is_finite() || !limit.is_finite() {
        return Err(PreviewError::new("layer の切り抜きが不正です"));
    }
    let start = (origin * limit).clamp(0.0, limit);
    let end = ((origin + size) * limit).clamp(0.0, limit);
    Ok((start, (end - start).max(0.0)))
}

fn floor_u32(value: f64) -> Result<u32, PreviewError> {
    if !value.is_finite() || value < 0.0 || value > u32::MAX as f64 {
        return Err(PreviewError::new("layer の切り抜きが素材の外です"));
    }
    Ok(value.floor() as u32)
}

fn span_u32(size: f64, limit: u32, origin: u32) -> Result<u32, PreviewError> {
    if !size.is_finite() || size <= 0.0 {
        return Err(PreviewError::new("layer の切り抜きが空です"));
    }
    let width = size.ceil().min(f64::from(limit.saturating_sub(origin)));
    if width < 1.0 || width > u32::MAX as f64 {
        return Err(PreviewError::new("layer の切り抜きが空です"));
    }
    Ok(width as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn near(actual: f32, expected: f32) {
        assert!(
            (actual - expected).abs() < 1e-3,
            "{actual} と {expected} が離れています"
        );
    }

    #[test]
    fn identity_frame_maps_pixel_centers_onto_themselves() {
        let params = layer_params(
            1920,
            1080,
            1920,
            1080,
            &ClipEffects::default(),
            1.0,
            SourceKind::Rgba,
            YuvMatrix::Bt709,
            YuvRange::Limited,
        )
        .expect("幾何")
        .expect("全面");
        for (actual, expected) in params.affine.iter().zip([1.0, 0.0, 0.0, 0.0, 1.0, 0.0]) {
            near(*actual, expected);
        }
        assert_eq!(params.crop, [0, 0, 1920, 1080]);
    }

    #[test]
    fn letterbox_shifts_a_square_frame_to_the_center() {
        // 100x100 を 200x100 の中央へ。左端は出力 x = 50、素材は全面。
        let params = layer_params(
            100,
            100,
            200,
            100,
            &ClipEffects::default(),
            0.5,
            SourceKind::Rgba,
            YuvMatrix::Bt709,
            YuvRange::Limited,
        )
        .expect("幾何")
        .expect("中央");
        for (actual, expected) in params.affine.iter().zip([1.0, 0.0, -50.0, 0.0, 1.0, 0.0]) {
            near(*actual, expected);
        }
        assert_eq!(params.crop, [0, 0, 100, 100]);
        assert!((params.opacity - 0.5).abs() < 1e-6);
    }

    #[test]
    fn half_turn_maps_the_left_edge_to_the_right_edge() {
        let params = layer_params(
            100,
            100,
            100,
            100,
            &ClipEffects {
                rotation_degrees: 180.0,
                ..ClipEffects::default()
            },
            1.0,
            SourceKind::Rgba,
            YuvMatrix::Bt709,
            YuvRange::Limited,
        )
        .expect("幾何")
        .expect("回転");
        for (actual, expected) in params
            .affine
            .iter()
            .zip([-1.0, 0.0, 100.0, 0.0, -1.0, 100.0])
        {
            near(*actual, expected);
        }
    }

    #[test]
    fn a_crop_that_hides_the_frame_draws_nothing() {
        let hidden = layer_params(
            100,
            100,
            100,
            100,
            &ClipEffects {
                crop_left_percent: 100.0,
                ..ClipEffects::default()
            },
            1.0,
            SourceKind::Rgba,
            YuvMatrix::Bt709,
            YuvRange::Limited,
        )
        .expect("幾何");
        assert!(hidden.is_none());
    }
}
