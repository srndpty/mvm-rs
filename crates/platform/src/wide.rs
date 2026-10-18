//! NUL 終端の UTF-16 path。Win32 に渡す直前だけに使う。

use std::os::windows::ffi::OsStrExt;
use std::path::Path;

use crate::error::PlatformError;

const MAX_WIDE: usize = 32768;

pub(crate) fn wide_path(path: &Path) -> Result<Vec<u16>, PlatformError> {
    let mut wide: Vec<u16> = path.as_os_str().encode_wide().collect();
    if wide.len() >= MAX_WIDE {
        return Err(PlatformError::new("パスが長すぎます"));
    }
    wide.push(0);
    Ok(wide)
}
