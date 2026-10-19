//! 描画結果（artifact）。Manim の PNG の alpha だけを、1 画素 1 byte の被覆（mask）として持つ。
//!
//! disk の形: `<cache>/<key>/` に PNG と `manifest.json`。manifest は全ての PNG を書いた後に最後に書き、
//! manifest があることを完成の印にする。読むときは manifest の大きさ・枚数・key と食い違えば捨てる。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use mvm_media::{StillLimits, decode_still_image};
use serde::{Deserialize, Serialize};

use crate::error::MathError;

/// 白い glyph の被覆。値は 0..=255（255 が glyph の内側）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Mask {
    pub width: u32,
    pub height: u32,
    pub coverage: Vec<u8>,
}

impl Mask {
    pub fn new(width: u32, height: u32, coverage: Vec<u8>) -> Result<Self, MathError> {
        let expected = (width as usize)
            .checked_mul(height as usize)
            .ok_or_else(|| MathError::new("mask の大きさが overflow しました"))?;
        if width == 0 || height == 0 || coverage.len() != expected {
            return Err(MathError::new("mask の大きさと画素数が一致しません"));
        }
        Ok(Self {
            width,
            height,
            coverage,
        })
    }

    /// PNG を読み、alpha を被覆にする。
    pub fn read_png(path: &Path) -> Result<Self, MathError> {
        let image = decode_still_image(path, &StillLimits::default()).map_err(|error| {
            MathError::new(format!(
                "数式の画像を読めません ({}): {error}",
                path.display()
            ))
        })?;
        let width =
            u32::try_from(image.width).map_err(|_| MathError::new("数式の画像の幅が不正です"))?;
        let height = u32::try_from(image.height)
            .map_err(|_| MathError::new("数式の画像の高さが不正です"))?;
        let coverage = image
            .rgba
            .as_chunks::<4>()
            .0
            .iter()
            .map(|pixel| pixel[3])
            .collect();
        Self::new(width, height, coverage)
    }

    /// alpha が 0 でない画素の外接矩形（left, top, right, bottom、right と bottom は含まない）。
    pub fn bounds(&self) -> Option<(u32, u32, u32, u32)> {
        let mut bounds: Option<(u32, u32, u32, u32)> = None;
        for (index, value) in self.coverage.iter().enumerate() {
            if *value == 0 {
                continue;
            }
            let x = (index % self.width as usize) as u32;
            let y = (index / self.width as usize) as u32;
            bounds = Some(match bounds {
                None => (x, y, x + 1, y + 1),
                Some((left, top, right, bottom)) => {
                    (left.min(x), top.min(y), right.max(x + 1), bottom.max(y + 1))
                }
            });
        }
        bounds
    }
}

/// 名前の付いた mask の並び。静止は 1 枚、Write・変形は frame の数、Equation Sequence は区間ごと。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MathArtifact {
    pub sequences: BTreeMap<String, Vec<Arc<Mask>>>,
}

impl MathArtifact {
    pub fn sequence(&self, name: &str) -> Option<&[Arc<Mask>]> {
        self.sequences.get(name).map(Vec::as_slice)
    }

    pub fn frame(&self, name: &str, index: usize) -> Option<&Arc<Mask>> {
        self.sequences.get(name)?.get(index)
    }

    /// 読み込んだ全 mask の byte 数。
    pub fn bytes(&self) -> usize {
        self.sequences
            .values()
            .flatten()
            .map(|mask| mask.coverage.len())
            .sum()
    }
}

pub const MANIFEST_VERSION: &str = "mvm-math-artifact/1";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub version: String,
    pub key: String,
    pub sequences: BTreeMap<String, SequenceEntry>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SequenceEntry {
    pub width: u32,
    pub height: u32,
    pub frames: usize,
}

fn frame_name(sequence: &str, index: usize) -> String {
    format!("{}-{index:05}.png", sequence.replace('/', "_"))
}

/// 描いた PNG を cache へ写し、最後に manifest を書く。`frames` は sequence ごとの PNG の並び。
pub fn store_artifact(
    directory: &Path,
    key: &str,
    frames: &BTreeMap<String, Vec<PathBuf>>,
    sizes: &BTreeMap<String, (u32, u32)>,
) -> Result<(), MathError> {
    if directory.exists() {
        std::fs::remove_dir_all(directory)
            .map_err(|error| MathError::new(format!("古い数式の cache を消せません: {error}")))?;
    }
    std::fs::create_dir_all(directory)
        .map_err(|error| MathError::new(format!("数式の cache を作れません: {error}")))?;
    let mut sequences = BTreeMap::new();
    for (name, paths) in frames {
        let (width, height) = sizes
            .get(name)
            .copied()
            .ok_or_else(|| MathError::new(format!("{name} の大きさがありません")))?;
        for (index, path) in paths.iter().enumerate() {
            std::fs::copy(path, directory.join(frame_name(name, index)))
                .map_err(|error| MathError::new(format!("数式の画像を写せません: {error}")))?;
        }
        sequences.insert(
            name.clone(),
            SequenceEntry {
                width,
                height,
                frames: paths.len(),
            },
        );
    }
    let manifest = Manifest {
        version: MANIFEST_VERSION.to_owned(),
        key: key.to_owned(),
        sequences,
    };
    let text = serde_json::to_string_pretty(&manifest)
        .map_err(|error| MathError::new(format!("manifest を作れません: {error}")))?;
    mvm_platform::atomic_write(&directory.join("manifest.json"), text.as_bytes())
        .map_err(|error| MathError::new(error.to_string()))
}

/// cache の artifact を読む。無ければ `Ok(None)`。壊れていれば失敗（呼び出し側が消して描き直す）。
pub fn load_artifact(directory: &Path, key: &str) -> Result<Option<MathArtifact>, MathError> {
    let manifest_path = directory.join("manifest.json");
    let text = match std::fs::read_to_string(&manifest_path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(MathError::new(format!("manifest を読めません: {error}"))),
    };
    let manifest: Manifest = serde_json::from_str(&text)
        .map_err(|error| MathError::new(format!("manifest が壊れています: {error}")))?;
    if manifest.version != MANIFEST_VERSION || manifest.key != key {
        return Err(MathError::new("manifest の版または key が違います"));
    }
    let mut artifact = MathArtifact::default();
    for (name, entry) in &manifest.sequences {
        let mut masks = Vec::with_capacity(entry.frames);
        for index in 0..entry.frames {
            let mask = Mask::read_png(&directory.join(frame_name(name, index)))?;
            if mask.width != entry.width || mask.height != entry.height {
                return Err(MathError::new(format!(
                    "{name} の {index} 枚目の大きさが manifest と違います"
                )));
            }
            masks.push(Arc::new(mask));
        }
        artifact.sequences.insert(name.clone(), masks);
    }
    Ok(Some(artifact))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn mask_requires_matching_pixels_and_reports_bounds() {
        assert!(Mask::new(2, 2, vec![0; 3]).is_err());
        assert!(Mask::new(0, 2, Vec::new()).is_err());
        let mask = Mask::new(4, 3, vec![0, 0, 0, 0, 0, 9, 9, 0, 0, 0, 9, 0]).unwrap();
        assert_eq!(mask.bounds(), Some((1, 1, 3, 3)));
        assert_eq!(Mask::new(1, 1, vec![0]).unwrap().bounds(), None);
    }

    #[test]
    fn missing_manifest_is_none_and_a_foreign_key_is_rejected() {
        let directory = std::env::temp_dir().join(format!("mvm-artifact-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        assert_eq!(load_artifact(&directory, "k").unwrap(), None);
        store_artifact(&directory, "k", &BTreeMap::new(), &BTreeMap::new()).unwrap();
        assert_eq!(
            load_artifact(&directory, "k").unwrap(),
            Some(MathArtifact::default())
        );
        assert!(load_artifact(&directory, "other").is_err());
        std::fs::remove_dir_all(&directory).unwrap();
    }
}
