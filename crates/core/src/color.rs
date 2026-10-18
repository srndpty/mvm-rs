//! YCbCr の行列とレンジ。係数と未指定時の解釈は、使う crate が持つ。

/// ITU-R の非恒定輝度の YCbCr 行列。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum YuvMatrix {
    /// ITU-R BT.709。
    #[default]
    Bt709,
    /// ITU-R BT.601（SMPTE 170M / BT.470 BG）。
    Bt601,
    /// ITU-R BT.2020 非恒定輝度。
    Bt2020,
}

/// YCbCr の量子化レンジ。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum YuvRange {
    /// スタジオレンジ。8bit なら輝度 16–235、色差 16–240。
    #[default]
    Limited,
    /// フルレンジ。8bit なら 0–255。
    Full,
}
