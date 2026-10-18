use std::fmt;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PreviewError {
    message: String,
}

impl PreviewError {
    pub(crate) fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for PreviewError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for PreviewError {}
