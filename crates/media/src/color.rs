//! FFmpeg の色指定を、compositor が使える行列とレンジにする。
//!
//! 未指定の色空間は、高さが 576 以下（NTSC 480 / PAL 576）なら BT.601、それ以外は BT.709。
//! 未指定のレンジは limited。信号があるときはそれを使う。対応しない行列は失敗させる。

use ffmpeg_sys_next as ff;
use mvm_core::{YuvMatrix, YuvRange};

use crate::error::MediaError;

const SD_MAX_HEIGHT: i32 = 576;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VideoColor {
    pub matrix: YuvMatrix,
    pub range: YuvRange,
}

pub fn resolve_yuv(
    space: ff::AVColorSpace,
    range: ff::AVColorRange,
    height: i32,
) -> Result<VideoColor, MediaError> {
    let matrix = match space {
        ff::AVColorSpace::AVCOL_SPC_BT709 => YuvMatrix::Bt709,
        ff::AVColorSpace::AVCOL_SPC_BT470BG | ff::AVColorSpace::AVCOL_SPC_SMPTE170M => {
            YuvMatrix::Bt601
        }
        ff::AVColorSpace::AVCOL_SPC_BT2020_NCL => YuvMatrix::Bt2020,
        ff::AVColorSpace::AVCOL_SPC_UNSPECIFIED => {
            if height <= 0 {
                return Err(MediaError::new("映像の高さが不正です"));
            }
            if height <= SD_MAX_HEIGHT {
                YuvMatrix::Bt601
            } else {
                YuvMatrix::Bt709
            }
        }
        other => {
            return Err(MediaError::new(format!(
                "対応していない YUV の色空間です: {other:?}"
            )));
        }
    };
    let range = match range {
        ff::AVColorRange::AVCOL_RANGE_UNSPECIFIED | ff::AVColorRange::AVCOL_RANGE_MPEG => {
            YuvRange::Limited
        }
        ff::AVColorRange::AVCOL_RANGE_JPEG => YuvRange::Full,
        other => {
            return Err(MediaError::new(format!(
                "対応していない YUV のレンジです: {other:?}"
            )));
        }
    };
    Ok(VideoColor { matrix, range })
}

#[cfg(test)]
#[allow(clippy::panic, clippy::unwrap_used)]
mod tests {
    use super::*;

    fn color(
        space: ff::AVColorSpace,
        range: ff::AVColorRange,
        height: i32,
    ) -> Result<VideoColor, MediaError> {
        resolve_yuv(space, range, height)
    }

    #[test]
    fn signaled_matrix_wins_and_sd_defaults_to_bt601() {
        let hd = color(
            ff::AVColorSpace::AVCOL_SPC_UNSPECIFIED,
            ff::AVColorRange::AVCOL_RANGE_UNSPECIFIED,
            1080,
        )
        .unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(hd.matrix, YuvMatrix::Bt709);
        assert_eq!(hd.range, YuvRange::Limited);
        let pal = color(
            ff::AVColorSpace::AVCOL_SPC_UNSPECIFIED,
            ff::AVColorRange::AVCOL_RANGE_UNSPECIFIED,
            576,
        )
        .unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(pal.matrix, YuvMatrix::Bt601);
        let above_sd = color(
            ff::AVColorSpace::AVCOL_SPC_UNSPECIFIED,
            ff::AVColorRange::AVCOL_RANGE_MPEG,
            577,
        )
        .unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(above_sd.matrix, YuvMatrix::Bt709);
        let signaled = color(
            ff::AVColorSpace::AVCOL_SPC_SMPTE170M,
            ff::AVColorRange::AVCOL_RANGE_JPEG,
            1080,
        )
        .unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(signaled.matrix, YuvMatrix::Bt601);
        assert_eq!(signaled.range, YuvRange::Full);
        let bt2020 = color(
            ff::AVColorSpace::AVCOL_SPC_BT2020_NCL,
            ff::AVColorRange::AVCOL_RANGE_MPEG,
            2160,
        )
        .unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(bt2020.matrix, YuvMatrix::Bt2020);
        let bg = color(
            ff::AVColorSpace::AVCOL_SPC_BT470BG,
            ff::AVColorRange::AVCOL_RANGE_MPEG,
            480,
        )
        .unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(bg.matrix, YuvMatrix::Bt601);
    }

    #[test]
    fn unsupported_matrix_and_missing_height_fail() {
        let constant = color(
            ff::AVColorSpace::AVCOL_SPC_BT2020_CL,
            ff::AVColorRange::AVCOL_RANGE_MPEG,
            2160,
        );
        assert!(constant.unwrap_err().message().contains("色空間"));
        let fcc = color(
            ff::AVColorSpace::AVCOL_SPC_FCC,
            ff::AVColorRange::AVCOL_RANGE_MPEG,
            480,
        );
        assert!(fcc.is_err());
        let missing = color(
            ff::AVColorSpace::AVCOL_SPC_UNSPECIFIED,
            ff::AVColorRange::AVCOL_RANGE_MPEG,
            0,
        );
        assert!(missing.unwrap_err().message().contains("高さ"));
        let signaled_without_height = color(
            ff::AVColorSpace::AVCOL_SPC_BT709,
            ff::AVColorRange::AVCOL_RANGE_MPEG,
            0,
        )
        .unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(signaled_without_height.matrix, YuvMatrix::Bt709);
    }
}
