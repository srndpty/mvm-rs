//! 端末ごとの設定（Project には保存しない）: 外部 tool の場所と、文字起こしの前回の指定。
//!
//! `%APPDATA%\mvm\preferences.json` に置く。file が無ければ既定値。読めない・壊れた file は失敗させる
//! （黙って既定値に戻さない）。

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::error::AppError;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Preferences {
    /// whisper.cpp の `whisper-cli.exe`。
    pub whisper_cli: String,
    /// ggml 形式の認識 model。
    pub whisper_model: String,
    /// 認識言語（`ja`・`en`・`auto` など）。
    pub whisper_language: String,
    /// GPU（Vulkan）で認識する。使えなければ失敗する（CPU へ黙って切り替えない）。
    pub whisper_gpu: bool,
    /// `manim.exe`。空なら PATH から探す。
    pub manim_executable: String,
}

impl Default for Preferences {
    fn default() -> Self {
        Self {
            whisper_cli: String::new(),
            whisper_model: String::new(),
            whisper_language: "ja".to_owned(),
            whisper_gpu: false,
            manim_executable: String::new(),
        }
    }
}

fn preferences_path() -> Result<PathBuf, AppError> {
    let base = std::env::var_os("APPDATA")
        .ok_or_else(|| AppError::new("APPDATA が無いので設定の場所を決められません"))?;
    Ok(PathBuf::from(base).join("mvm").join("preferences.json"))
}

impl Preferences {
    pub fn load() -> Result<Self, AppError> {
        Self::load_from(&preferences_path()?)
    }

    pub fn save(&self) -> Result<(), AppError> {
        self.save_to(&preferences_path()?)
    }

    pub(crate) fn load_from(path: &std::path::Path) -> Result<Self, AppError> {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::default());
            }
            Err(error) => {
                return Err(AppError::new(format!(
                    "設定を読めません ({}): {error}",
                    path.display()
                )));
            }
        };
        serde_json::from_str(&text).map_err(|error| {
            AppError::new(format!("設定の形が不正です ({}): {error}", path.display()))
        })
    }

    pub(crate) fn save_to(&self, path: &std::path::Path) -> Result<(), AppError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|error| {
                AppError::new(format!(
                    "設定の場所を作れません ({}): {error}",
                    parent.display()
                ))
            })?;
        }
        let text = serde_json::to_string_pretty(self)
            .map_err(|error| AppError::new(format!("設定を書けません: {error}")))?;
        mvm_platform::atomic_write(path, text.as_bytes())
            .map_err(|error| AppError::new(error.to_string()))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn missing_file_reads_defaults_and_saved_values_round_trip() {
        let directory = std::env::temp_dir().join(format!("mvm-pref-{}", std::process::id()));
        let path = directory.join("nested").join("preferences.json");
        assert_eq!(
            Preferences::load_from(&path).unwrap(),
            Preferences::default()
        );
        let preferences = Preferences {
            whisper_cli: "C:\\tools\\whisper-cli.exe".to_owned(),
            whisper_gpu: true,
            ..Preferences::default()
        };
        preferences.save_to(&path).unwrap();
        assert_eq!(Preferences::load_from(&path).unwrap(), preferences);
        std::fs::write(&path, "{ not json").unwrap();
        assert!(
            Preferences::load_from(&path).is_err(),
            "壊れた設定を既定値で読みました"
        );
        std::fs::write(&path, "{\"unknown\": 1}").unwrap();
        assert!(Preferences::load_from(&path).is_err());
        std::fs::remove_dir_all(&directory).unwrap();
    }
}
