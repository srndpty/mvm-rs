use std::fmt;

/// 書き出しの失敗。利用者が取り消したときは [`ExportError::Cancelled`]。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExportError {
    Cancelled,
    Failed(String),
}

impl ExportError {
    pub(crate) fn failed(message: impl Into<String>) -> Self {
        Self::Failed(message.into())
    }

    pub fn is_cancelled(&self) -> bool {
        matches!(self, Self::Cancelled)
    }
}

impl fmt::Display for ExportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Cancelled => f.write_str("書き出しを取り消しました"),
            Self::Failed(message) => f.write_str(message),
        }
    }
}

impl std::error::Error for ExportError {}
