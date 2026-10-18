//! 検証素材の frame marker を読む。
//!
//! 帯は映像の左上。セルは 64px 四方で 19 個（幅 1216px）。
//! cell 0 は白、cell 1 は黒、cell 2..17 は frame 番号の 16bit（LSB first）、cell 18 は白。
//! 機械判定はセル中心 8x8 の平均輝度だけを使う。

/// セルの一辺（px）。
pub const CELL_SIZE: u32 = 64;
/// 帯のセル数。
pub const CELL_COUNT: u32 = 19;
/// 帯の幅。
pub const MARKER_WIDTH: u32 = CELL_SIZE * CELL_COUNT;
/// 帯の高さ。
pub const MARKER_HEIGHT: u32 = CELL_SIZE;

const SAMPLE: u32 = 8;
const THRESHOLD: f64 = 128.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MarkerError {
    /// 帯が画像に収まらない。
    TooSmall,
    /// 同期セル（白・黒・白）が崩れている。番号は信用しない。
    Sync,
}

impl std::fmt::Display for MarkerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let message = match self {
            Self::TooSmall => "frame marker の帯が画像に収まりません",
            Self::Sync => "frame marker の同期セルを確認できません",
        };
        f.write_str(message)
    }
}

impl std::error::Error for MarkerError {}

/// 原点 (0, 0) の帯を読む。`rgba` は 1 画素 4 byte、`stride` は 1 行の byte 数。
pub fn read_frame_marker(
    rgba: &[u8],
    width: u32,
    height: u32,
    stride: usize,
) -> Result<u16, MarkerError> {
    read_frame_marker_at(rgba, width, height, stride, 0, 0)
}

/// 帯の左上を (`origin_x`, `origin_y`) として読む。合成結果の途中に帯があるときに使う。
pub fn read_frame_marker_at(
    rgba: &[u8],
    width: u32,
    height: u32,
    stride: usize,
    origin_x: u32,
    origin_y: u32,
) -> Result<u16, MarkerError> {
    let end_x = origin_x
        .checked_add(MARKER_WIDTH)
        .ok_or(MarkerError::TooSmall)?;
    let end_y = origin_y
        .checked_add(MARKER_HEIGHT)
        .ok_or(MarkerError::TooSmall)?;
    if end_x > width || end_y > height {
        return Err(MarkerError::TooSmall);
    }
    let row_bytes = usize::try_from(width)
        .ok()
        .and_then(|pixels| pixels.checked_mul(4))
        .ok_or(MarkerError::TooSmall)?;
    if stride < row_bytes {
        return Err(MarkerError::TooSmall);
    }
    let rows = usize::try_from(height).map_err(|_| MarkerError::TooSmall)?;
    let needed = stride.checked_mul(rows).ok_or(MarkerError::TooSmall)?;
    if rgba.len() < needed {
        return Err(MarkerError::TooSmall);
    }

    let mut white = [false; CELL_COUNT as usize];
    for cell in 0..CELL_COUNT {
        let center_x = origin_x + cell * CELL_SIZE + CELL_SIZE / 2;
        let center_y = origin_y + CELL_SIZE / 2;
        white[cell as usize] = average_luma(rgba, stride, center_x, center_y) > THRESHOLD;
    }
    if !white[0] || white[1] || !white[18] {
        return Err(MarkerError::Sync);
    }
    let mut value = 0u16;
    for bit in 0..16 {
        if white[2 + bit] {
            value |= 1 << bit;
        }
    }
    Ok(value)
}

fn average_luma(rgba: &[u8], stride: usize, center_x: u32, center_y: u32) -> f64 {
    let x0 = center_x - SAMPLE / 2;
    let y0 = center_y - SAMPLE / 2;
    let mut sum = 0.0;
    for y in y0..y0 + SAMPLE {
        let row = y as usize * stride;
        for x in x0..x0 + SAMPLE {
            let pixel = row + x as usize * 4;
            let red = f64::from(rgba[pixel]);
            let green = f64::from(rgba[pixel + 1]);
            let blue = f64::from(rgba[pixel + 2]);
            sum += 0.299 * red + 0.587 * green + 0.114 * blue;
        }
    }
    sum / f64::from(SAMPLE * SAMPLE)
}
