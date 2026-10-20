//! 通常ファイルの実体（volume serial + 128 bit file ID）と、大きさ・更新時刻。
//!
//! ディレクトリと、識別子を取れない path は Unavailable。存在しない path は Missing。

use std::path::Path;

use windows::Win32::Foundation::{CloseHandle, ERROR_FILE_NOT_FOUND, ERROR_PATH_NOT_FOUND, HANDLE};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FILE_BASIC_INFO, FILE_FLAG_BACKUP_SEMANTICS, FILE_ID_INFO, FILE_READ_ATTRIBUTES,
    FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, FILE_STANDARD_INFO, FileBasicInfo,
    FileIdInfo, FileStandardInfo, GetFileInformationByHandleEx, OPEN_EXISTING,
};
use windows::core::{HRESULT, PCWSTR};

use crate::wide::wide_path;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileProbeStatus {
    File,
    Missing,
    Unavailable,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FileProbe {
    pub status: FileProbeStatus,
    pub volume_serial: u64,
    pub file_id: [u8; 16],
    pub size: u64,
    pub last_write_100ns: i64,
}

impl FileProbe {
    pub fn missing() -> Self {
        Self {
            status: FileProbeStatus::Missing,
            volume_serial: 0,
            file_id: [0; 16],
            size: 0,
            last_write_100ns: 0,
        }
    }

    pub fn unavailable() -> Self {
        Self {
            status: FileProbeStatus::Unavailable,
            ..Self::missing()
        }
    }
}

pub fn probe_file(path: &Path) -> FileProbe {
    if path.as_os_str().is_empty() {
        return FileProbe::unavailable();
    }
    let Ok(wide) = wide_path(path) else {
        return FileProbe::unavailable();
    };
    // SAFETY: path は NUL 終端。共有をすべて許すので、他の process が開いていても読める。
    let handle = match unsafe {
        CreateFileW(
            PCWSTR(wide.as_ptr()),
            FILE_READ_ATTRIBUTES.0,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            None,
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS,
            None,
        )
    } {
        Ok(handle) => handle,
        Err(error) => {
            return if missing_error(&error) {
                FileProbe::missing()
            } else {
                FileProbe::unavailable()
            };
        }
    };
    let probe = read_identity(handle);
    // SAFETY: この関数が開いた handle を、読んだあと 1 回だけ閉じる。
    unsafe {
        let _ = CloseHandle(handle);
    }
    probe
}

fn missing_error(error: &windows::core::Error) -> bool {
    let code = error.code();
    code == HRESULT::from_win32(ERROR_FILE_NOT_FOUND.0)
        || code == HRESULT::from_win32(ERROR_PATH_NOT_FOUND.0)
}

pub(crate) fn read_identity(handle: HANDLE) -> FileProbe {
    // SAFETY: 3 つの struct は POD。API が成功したときだけ中身を読む。
    let (id, basic, standard) = unsafe {
        let mut id = std::mem::zeroed::<FILE_ID_INFO>();
        let mut basic = std::mem::zeroed::<FILE_BASIC_INFO>();
        let mut standard = std::mem::zeroed::<FILE_STANDARD_INFO>();
        let id_ok = GetFileInformationByHandleEx(
            handle,
            FileIdInfo,
            &mut id as *mut FILE_ID_INFO as *mut _,
            std::mem::size_of::<FILE_ID_INFO>() as u32,
        );
        let basic_ok = GetFileInformationByHandleEx(
            handle,
            FileBasicInfo,
            &mut basic as *mut FILE_BASIC_INFO as *mut _,
            std::mem::size_of::<FILE_BASIC_INFO>() as u32,
        );
        let standard_ok = GetFileInformationByHandleEx(
            handle,
            FileStandardInfo,
            &mut standard as *mut FILE_STANDARD_INFO as *mut _,
            std::mem::size_of::<FILE_STANDARD_INFO>() as u32,
        );
        if id_ok.is_err() || basic_ok.is_err() || standard_ok.is_err() {
            return FileProbe::unavailable();
        }
        (id, basic, standard)
    };
    if standard.Directory || standard.EndOfFile < 0 {
        return FileProbe::unavailable();
    }
    FileProbe {
        status: FileProbeStatus::File,
        volume_serial: id.VolumeSerialNumber,
        file_id: id.FileId.Identifier,
        size: standard.EndOfFile as u64,
        last_write_100ns: basic.LastWriteTime,
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use std::fs;
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;

    static SCRATCH: AtomicU64 = AtomicU64::new(1);

    fn scratch() -> std::path::PathBuf {
        let n = SCRATCH.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("mvm-identity-{}-{n}", std::process::id()));
        fs::create_dir_all(&dir).expect("作業 directory");
        dir
    }

    #[test]
    fn a_file_has_a_stable_id_and_a_missing_path_is_missing() {
        let dir = scratch();
        let path = dir.join("clip.mvm");
        fs::write(&path, b"abc").expect("書く");
        let first = probe_file(&path);
        let second = probe_file(&path);
        assert_eq!(first.status, FileProbeStatus::File);
        assert_eq!(first, second);
        assert_eq!(first.size, 3);
        assert_ne!(first.file_id, [0; 16]);
        assert_eq!(
            probe_file(&dir.join("nope.mvm")).status,
            FileProbeStatus::Missing
        );
        assert_eq!(
            probe_file(Path::new("")).status,
            FileProbeStatus::Unavailable
        );
        assert_eq!(probe_file(&dir).status, FileProbeStatus::Unavailable);
        fs::remove_dir_all(&dir).expect("片付け");
    }
}
