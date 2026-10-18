//! project crate の失敗。利用者へそのまま見せる日本語の理由を持つ。

use std::fmt;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectError(String);

impl ProjectError {
    pub fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }

    pub fn message(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ProjectError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ProjectError {}

pub type ProjectResult<T> = Result<T, ProjectError>;

/// `ProjectError` を作って `Err` で返す短縮形。
pub(crate) fn fail<T>(message: impl Into<String>) -> ProjectResult<T> {
    Err(ProjectError::new(message))
}
