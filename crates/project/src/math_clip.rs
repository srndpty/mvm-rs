//! 数式 clip の正（Project に保存する値）。数式の意味と見た目だけを持つ。
//!
//! 何で描くか（backend）は描画方針でありここに持たない。どこに置くか（位置・拡大・回転）は画像 clip と同じく
//! ClipEffects が持つ。描いた画像は派生物で、Project には保存しない。

use crate::error::{ProjectResult, fail};
use crate::model::parse_argb_color;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MathClipData {
    /// 記法。現在は latex だけを受理する。
    pub syntax: String,
    /// 利用者が書いた式。
    pub source: String,
    /// 1 em の px（拡大 100% のとき）。描く解像度を決める。
    pub font_size: i32,
    pub color: String,
    /// glyph の bbox（と余白）を塗る色。既定は透明。
    pub background_color: String,
}

impl Default for MathClipData {
    fn default() -> Self {
        Self {
            syntax: "latex".to_owned(),
            source: String::new(),
            font_size: 96,
            color: "#FFFFFFFF".to_owned(),
            background_color: "#00000000".to_owned(),
        }
    }
}

impl MathClipData {
    /// 値の形だけを見る（空でない式、既知の記法、文字サイズの範囲、色の形式）。
    /// 式を描けるかどうかは見ない。描けない式も Project の正として保存できる。
    pub fn validate(&self, output_height: i32) -> ProjectResult<()> {
        if self.syntax != "latex" {
            return fail(format!("数式の記法が未対応です: {}", self.syntax));
        }
        if self
            .source
            .chars()
            .all(|character| matches!(character, ' ' | '\t' | '\r' | '\n'))
        {
            return fail("数式が空です");
        }
        if !(1..=output_height).contains(&self.font_size) {
            return fail(format!(
                "数式の文字サイズは 1 から出力の高さ ({output_height}) までです"
            ));
        }
        if parse_argb_color(&self.color).is_none()
            || parse_argb_color(&self.background_color).is_none()
        {
            return fail("数式の色は #AARRGGBB で指定してください");
        }
        Ok(())
    }
}

/// clip の先頭で式を出す animation の種類。Write は Manim の Write で式を書いていく。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum MathIntroKind {
    #[default]
    None,
    Write,
}

impl MathIntroKind {
    pub fn name(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Write => "write",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        [Self::None, Self::Write]
            .into_iter()
            .find(|kind| kind.name() == name)
    }
}

/// 数式 clip の時間の振る舞い。式の意味と見た目（MathClipData）とは分けて持ち、静止の描画と cache key を
/// 変えない。位置・拡大・不透明度などの動きは ClipEffects が持つ。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MathClipAnimation {
    pub intro: MathIntroKind,
    /// intro の尺。clip の素材 frame（fade と同じ domain）で数え、clip の見えている先頭から始まる。
    /// None のときは 0。
    pub intro_frames: i64,
}

impl MathClipAnimation {
    /// 形と時間の意味だけを見る。None なら尺は 0、Write なら 1 から clip の尺（素材 frame）まで。
    /// 描画の方式による上限は持たない（描けない長さは描画側が「未対応」として失敗する）。
    pub fn validate(&self, clip_source_frames: i64) -> ProjectResult<()> {
        match self.intro {
            MathIntroKind::None if self.intro_frames != 0 => {
                fail("数式の intro が無いのに尺があります")
            }
            MathIntroKind::Write if !(1..=clip_source_frames).contains(&self.intro_frames) => {
                fail("数式の Write の尺は 1 frame から clip の尺までです")
            }
            _ => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn data(source: &str) -> MathClipData {
        MathClipData {
            source: source.to_owned(),
            ..MathClipData::default()
        }
    }

    #[test]
    fn validates_shape_only() {
        assert!(data("x^2").validate(1080).is_ok());
        assert!(data(" \t\r\n").validate(1080).is_err());
        assert!(
            MathClipData {
                syntax: "mathml".to_owned(),
                ..data("x")
            }
            .validate(1080)
            .is_err()
        );
        assert!(
            MathClipData {
                font_size: 1081,
                ..data("x")
            }
            .validate(1080)
            .is_err()
        );
        assert!(
            MathClipData {
                font_size: 0,
                ..data("x")
            }
            .validate(1080)
            .is_err()
        );
        assert!(
            MathClipData {
                color: "#FFF".to_owned(),
                ..data("x")
            }
            .validate(1080)
            .is_err()
        );
    }

    #[test]
    fn validates_intro_length_against_clip() {
        let none = MathClipAnimation::default();
        assert!(none.validate(10).is_ok());
        let none_with_length = MathClipAnimation {
            intro_frames: 1,
            ..none
        };
        assert!(none_with_length.validate(10).is_err());
        let write = |frames| MathClipAnimation {
            intro: MathIntroKind::Write,
            intro_frames: frames,
        };
        assert!(write(1).validate(10).is_ok());
        assert!(write(10).validate(10).is_ok());
        assert!(write(0).validate(10).is_err());
        assert!(write(11).validate(10).is_err());
    }
}
