//! 同じ directory の一時 file へ flush してから、対象を置換する。
//!
//! C++ 版 `mvm_atomic_write_file` と同じ手順。置換先を他の process が開いている間は
//! 短い間待ってやり直す。`ReplaceFileW` は使わない。

use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use windows::Win32::Foundation::{
    CloseHandle, ERROR_ACCESS_DENIED, ERROR_LOCK_VIOLATION, ERROR_SHARING_VIOLATION, GENERIC_WRITE,
};
use windows::Win32::Storage::FileSystem::{
    CREATE_NEW, CreateFileW, DeleteFileW, FILE_ATTRIBUTE_NORMAL, FILE_FLAG_WRITE_THROUGH,
    FILE_SHARE_MODE, FlushFileBuffers, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
    MoveFileExW, WriteFile,
};
use windows::Win32::System::Threading::{GetCurrentProcessId, Sleep};
use windows::core::{HRESULT, PCWSTR};

use crate::error::PlatformError;

const MAX_WIDE: usize = 32768;
const REPLACE_ATTEMPTS: u32 = 40;
const REPLACE_WAIT_MS: u32 = 50;

static TEMP_SERIAL: AtomicU32 = AtomicU32::new(1);

/// `path` を `bytes` で置換する。失敗したときは元の file を残し、一時 file は消す。
pub fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), PlatformError> {
    if path.as_os_str().is_empty() {
        return Err(PlatformError::new("書き込み先のパスが空です"));
    }
    let temporary = temporary_path(path)?;
    let temporary_wide = wide_path(&temporary)?;
    let mut file = TemporaryFile::create(&temporary_wide)?;
    if let Err(error) = file.write_all(bytes) {
        file.delete();
        return Err(error);
    }
    if let Err(error) = file.finish() {
        file.delete();
        return Err(error);
    }
    let target_wide = wide_path(path)?;
    replace(&temporary_wide, &target_wide)
}

fn temporary_path(target: &Path) -> Result<PathBuf, PlatformError> {
    // SAFETY: 現在 process の ID を読むだけで、失敗する引数は無い。
    let process_id = unsafe { GetCurrentProcessId() };
    let serial = TEMP_SERIAL.fetch_add(1, Ordering::Relaxed);
    let mut name = target.as_os_str().to_owned();
    name.push(format!(".mvmtmp.{process_id}.{serial}"));
    Ok(PathBuf::from(name))
}

fn wide_path(path: &Path) -> Result<Vec<u16>, PlatformError> {
    let mut wide: Vec<u16> = path.as_os_str().encode_wide().collect();
    if wide.len() >= MAX_WIDE {
        return Err(PlatformError::new("パスが長すぎます"));
    }
    wide.push(0);
    Ok(wide)
}

struct TemporaryFile {
    handle: windows::Win32::Foundation::HANDLE,
    path: Vec<u16>,
    open: bool,
}

impl TemporaryFile {
    fn create(path: &[u16]) -> Result<Self, PlatformError> {
        // SAFETY: path は NUL 終端。CREATE_NEW は既存 file を上書きしない。失敗時に handle は返らない。
        let handle = unsafe {
            CreateFileW(
                PCWSTR(path.as_ptr()),
                GENERIC_WRITE.0,
                FILE_SHARE_MODE(0),
                None,
                CREATE_NEW,
                FILE_ATTRIBUTE_NORMAL | FILE_FLAG_WRITE_THROUGH,
                None,
            )
        }
        .map_err(|error| PlatformError::new(format!("一時ファイルを作成できません: {error}")))?;
        Ok(Self {
            handle,
            path: path.to_vec(),
            open: true,
        })
    }

    fn write_all(&self, bytes: &[u8]) -> Result<(), PlatformError> {
        let mut offset = 0;
        while offset < bytes.len() {
            let end = offset.saturating_add(1024 * 1024).min(bytes.len());
            let chunk = &bytes[offset..end];
            let mut written = 0u32;
            // SAFETY: handle は create が返したもの。chunk はこの呼び出しの間だけ有効。
            unsafe { WriteFile(self.handle, Some(chunk), Some(&mut written), None) }.map_err(
                |error| PlatformError::new(format!("一時ファイルへ書けません: {error}")),
            )?;
            let written_bytes = usize::try_from(written)
                .map_err(|_| PlatformError::new("一時ファイルへ書いた大きさを扱えません"))?;
            if written_bytes != chunk.len() {
                return Err(PlatformError::new(
                    "一時ファイルへ書いた大きさが要求と一致しません",
                ));
            }
            offset = end;
        }
        Ok(())
    }

    fn finish(&mut self) -> Result<(), PlatformError> {
        // SAFETY: handle は create が返した開いている file。flush は中身を媒体へ出す。
        unsafe { FlushFileBuffers(self.handle) }.map_err(|error| {
            PlatformError::new(format!("一時ファイルを flush できません: {error}"))
        })?;
        self.close()?;
        Ok(())
    }

    fn close(&mut self) -> Result<(), PlatformError> {
        if !self.open {
            return Ok(());
        }
        self.open = false;
        // SAFETY: handle をここで 1 回だけ閉じる。以降は使わない。
        unsafe { CloseHandle(self.handle) }
            .map_err(|error| PlatformError::new(format!("一時ファイルを閉じられません: {error}")))
    }

    fn delete(&mut self) {
        let _ = self.close();
        // SAFETY: path は NUL 終端。file が無くても失敗するだけで、他の file は消さない。
        unsafe {
            let _ = DeleteFileW(PCWSTR(self.path.as_ptr()));
        }
    }
}

impl Drop for TemporaryFile {
    fn drop(&mut self) {
        let _ = self.close();
    }
}

fn replace(temporary: &[u16], target: &[u16]) -> Result<(), PlatformError> {
    let flags = MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH;
    let mut last = PlatformError::new("Project file を置換できません");
    for attempt in 0..REPLACE_ATTEMPTS {
        // SAFETY: 両方とも NUL 終端の同じ directory 上のパス。成功すると temporary は target になる。
        match unsafe { MoveFileExW(PCWSTR(temporary.as_ptr()), PCWSTR(target.as_ptr()), flags) } {
            Ok(()) => return Ok(()),
            Err(error) if retryable(&error) && attempt + 1 < REPLACE_ATTEMPTS => {
                last = PlatformError::new(format!("Project file を置換できません: {error}"));
                // SAFETY: 待つ時間は定数。他の状態は変えない。
                unsafe { Sleep(REPLACE_WAIT_MS) };
            }
            Err(error) => {
                last = PlatformError::new(format!("Project file を置換できません: {error}"));
                break;
            }
        }
    }
    // SAFETY: 置換できなかった一時 file を消す。target には触れていない。
    unsafe {
        let _ = DeleteFileW(PCWSTR(temporary.as_ptr()));
    }
    Err(last)
}

fn retryable(error: &windows::core::Error) -> bool {
    let code = error.code();
    code == HRESULT::from_win32(ERROR_ACCESS_DENIED.0)
        || code == HRESULT::from_win32(ERROR_SHARING_VIOLATION.0)
        || code == HRESULT::from_win32(ERROR_LOCK_VIOLATION.0)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use std::fs;
    use std::sync::atomic::AtomicU64;

    use super::*;

    static SCRATCH: AtomicU64 = AtomicU64::new(1);

    fn scratch_dir() -> PathBuf {
        let n = SCRATCH.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("mvm-atomic-{}-{n}", std::process::id()));
        fs::create_dir_all(&dir).expect("作業 directory");
        dir
    }

    #[test]
    fn replaces_existing_bytes_and_removes_the_temporary_file() {
        let dir = scratch_dir();
        let path = dir.join("project.mvm");
        atomic_write(&path, b"one").expect("初回");
        atomic_write(&path, b"two-two").expect("置換");
        assert_eq!(fs::read(&path).expect("読み戻し"), b"two-two");
        let leftovers = fs::read_dir(&dir)
            .expect("directory")
            .filter_map(|entry| entry.ok())
            .filter(|entry| entry.file_name().to_string_lossy().contains(".mvmtmp."))
            .count();
        assert_eq!(leftovers, 0);
        fs::remove_dir_all(&dir).expect("片付け");
    }

    #[test]
    fn rejects_an_empty_path_and_a_missing_parent() {
        let error = atomic_write(Path::new(""), b"x").expect_err("空パス");
        assert!(error.message().contains("空"), "{error}");

        let dir = scratch_dir();
        let path = dir.join("missing").join("project.mvm");
        let error = atomic_write(&path, b"x").expect_err("親が無い");
        assert!(!path.exists(), "{error}");
        assert!(
            !dir.join("missing").exists(),
            "失敗した書き込みが directory を作っています"
        );
        fs::remove_dir_all(&dir).expect("片付け");
    }
}
