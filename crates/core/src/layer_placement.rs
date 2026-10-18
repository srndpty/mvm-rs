//! 素材 1 枚を出力 canvas へ置く幾何。preview と書き出しで同じ置き方にするための唯一の実装。

/// 出力 canvas を 0..1 に正規化した矩形。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LayerRect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

impl LayerRect {
    /// canvas 全面。
    pub const FULL: Self = Self {
        x: 0.0,
        y: 0.0,
        width: 1.0,
        height: 1.0,
    };

    fn is_positive(&self) -> bool {
        [self.x, self.y, self.width, self.height]
            .iter()
            .all(|value| value.is_finite())
            && self.width > 0.0
            && self.height > 0.0
    }
}

impl Default for LayerRect {
    fn default() -> Self {
        Self::FULL
    }
}

/// 素材 1 枚を canvas へ置いた結果（いずれも正規化座標）。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LayerPlacement {
    /// 描く矩形。
    pub destination: LayerRect,
    /// 素材 frame の中で描く範囲（frame を 0..1 とした座標）。
    pub source_uv: LayerRect,
    /// 回転の中心（crop 範囲を置いた矩形の中心）。
    pub pivot_x: f64,
    pub pivot_y: f64,
}

/// 素材を canvas へ置く。次の順で決める。
/// 1. 素材 frame を比率を保って canvas の中央へ収める（letterbox）
/// 2. canvas 上の crop 範囲で切り抜く（letterbox の余白は透明のまま）
/// 3. crop 範囲を destination へ写す。縦横の倍率は別々でよい（引き伸ばす）
///
/// 寸法が正でない、crop / destination の幅・高さが正でない、crop が余白だけを指す場合は `None`（何も描かない）。
pub fn place_layer(
    frame_width: i32,
    frame_height: i32,
    canvas_width: i32,
    canvas_height: i32,
    crop: &LayerRect,
    destination: &LayerRect,
) -> Option<LayerPlacement> {
    if frame_width <= 0
        || frame_height <= 0
        || canvas_width <= 0
        || canvas_height <= 0
        || !crop.is_positive()
        || !destination.is_positive()
    {
        return None;
    }

    // 1. letterbox。どちらの辺を合わせるかは整数の積で決める。
    let frame_by_canvas = i64::from(frame_width) * i64::from(canvas_height);
    let canvas_by_frame = i64::from(canvas_width) * i64::from(frame_height);
    let mut fitted = LayerRect::FULL;
    if frame_by_canvas > canvas_by_frame {
        fitted.height = canvas_by_frame as f64 / frame_by_canvas as f64;
    } else {
        fitted.width = frame_by_canvas as f64 / canvas_by_frame as f64;
    }
    fitted.x = (1.0 - fitted.width) * 0.5;
    fitted.y = (1.0 - fitted.height) * 0.5;

    // 2. crop と素材の重なり。
    let left = crop.x.max(fitted.x);
    let top = crop.y.max(fitted.y);
    let right = (crop.x + crop.width).min(fitted.x + fitted.width);
    let bottom = (crop.y + crop.height).min(fitted.y + fitted.height);
    if right <= left || bottom <= top {
        return None;
    }

    // 3. crop 範囲 → destination の写像を、描く範囲にも同じく適用する。
    let scale_x = destination.width / crop.width;
    let scale_y = destination.height / crop.height;
    Some(LayerPlacement {
        destination: LayerRect {
            x: destination.x + (left - crop.x) * scale_x,
            y: destination.y + (top - crop.y) * scale_y,
            width: (right - left) * scale_x,
            height: (bottom - top) * scale_y,
        },
        source_uv: LayerRect {
            x: (left - fitted.x) / fitted.width,
            y: (top - fitted.y) / fitted.height,
            width: (right - left) / fitted.width,
            height: (bottom - top) / fitted.height,
        },
        pivot_x: destination.x + destination.width * 0.5,
        pivot_y: destination.y + destination.height * 0.5,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // 期待値は手で計算した値であり、実装の式を呼んで作らない。
    fn assert_rect(actual: LayerRect, x: f64, y: f64, width: f64, height: f64) {
        let near = |a: f64, b: f64| (a - b).abs() < 1e-9;
        assert!(
            near(actual.x, x)
                && near(actual.y, y)
                && near(actual.width, width)
                && near(actual.height, height),
            "{actual:?} != ({x}, {y}, {width}, {height})"
        );
    }

    fn rect(x: f64, y: f64, width: f64, height: f64) -> LayerRect {
        LayerRect {
            x,
            y,
            width,
            height,
        }
    }

    #[test]
    fn default_is_letterbox() {
        let full = LayerRect::FULL;
        let same = place_layer(1920, 1080, 1920, 1080, &full, &full).unwrap();
        assert_rect(same.destination, 0.0, 0.0, 1.0, 1.0);
        assert_rect(same.source_uv, 0.0, 0.0, 1.0, 1.0);

        // 4:3 (1440x1080) を 16:9 へ: 左右に 240px ずつの余白。
        let pillar = place_layer(1440, 1080, 1920, 1080, &full, &full).unwrap();
        assert_rect(
            pillar.destination,
            240.0 / 1920.0,
            0.0,
            1440.0 / 1920.0,
            1.0,
        );
        assert_rect(pillar.source_uv, 0.0, 0.0, 1.0, 1.0);
        assert!((pillar.pivot_x - 0.5).abs() < 1e-9 && (pillar.pivot_y - 0.5).abs() < 1e-9);

        // 縦長 (1080x1920) を 16:9 へ: 高さを合わせる。
        let portrait = place_layer(1080, 1920, 1920, 1080, &full, &full).unwrap();
        let width = (1080.0 * 1080.0 / 1920.0) / 1920.0;
        assert_rect(portrait.destination, (1.0 - width) / 2.0, 0.0, width, 1.0);
    }

    #[test]
    fn crops_in_canvas_coordinates() {
        // 4:3 素材の左の余白 (0..0.125) を含む crop は、素材の左端から切る。
        let crop = rect(0.0, 0.0, 0.5, 1.0);
        let placed = place_layer(1440, 1080, 1920, 1080, &crop, &crop).unwrap();
        assert_rect(placed.destination, 0.125, 0.0, 0.375, 1.0);
        assert_rect(placed.source_uv, 0.0, 0.0, 0.5, 1.0);
        // 回転の中心は描く範囲ではなく crop 範囲を置いた矩形の中心。
        assert!((placed.pivot_x - 0.25).abs() < 1e-9 && (placed.pivot_y - 0.5).abs() < 1e-9);

        // crop 範囲を横 2 倍・縦 0.5 倍に置くと、見える範囲も同じ倍率で写る。
        let stretched =
            place_layer(1440, 1080, 1920, 1080, &crop, &rect(0.0, 0.25, 1.0, 0.5)).unwrap();
        assert_rect(stretched.destination, 0.25, 0.25, 0.75, 0.5);
    }

    #[test]
    fn rejects_empty_and_invalid() {
        let full = LayerRect::FULL;
        let zero = rect(0.0, 0.0, 0.0, 1.0);
        assert!(place_layer(1440, 1080, 1920, 1080, &rect(0.0, 0.0, 0.1, 1.0), &full).is_none());
        assert!(place_layer(0, 1080, 1920, 1080, &full, &full).is_none());
        assert!(place_layer(1920, 1080, 1920, -1, &full, &full).is_none());
        assert!(place_layer(1920, 1080, 1920, 1080, &zero, &full).is_none());
        assert!(place_layer(1920, 1080, 1920, 1080, &full, &zero).is_none());
        assert!(
            place_layer(
                1920,
                1080,
                1920,
                1080,
                &rect(f64::NAN, 0.0, 1.0, 1.0),
                &full
            )
            .is_none()
        );
    }
}
