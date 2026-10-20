//! 同じ directory の一時 file へ flush してから、対象を置換する。
//!
//! C++ 版 `mvm_atomic_write_file` と同じ手順。置換先を他の process が開いている間は
//! 短い間待ってやり直す。`ReplaceFileW` は使わない。
//!
//! [`atomic_create`] は既存の file を置換しない。有無の確認と書き込みの間に別の process が
//! 作っても、最後の rename が失敗するので上書きしない。

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use windows::Win32::Foundation::{
    CloseHandle, ERROR_ACCESS_DENIED, ERROR_ALREADY_EXISTS, ERROR_FILE_EXISTS,
    ERROR_LOCK_VIOLATION, ERROR_SHARING_VIOLATION, GENERIC_WRITE,
};
use windows::Win32::Storage::FileSystem::{
    CREATE_NEW, CreateFileW, DeleteFileW, FILE_ATTRIBUTE_NORMAL, FILE_FLAG_WRITE_THROUGH,
    FILE_SHARE_MODE, FlushFileBuffers, MOVE_FILE_FLAGS, MOVEFILE_REPLACE_EXISTING,
    MOVEFILE_WRITE_THROUGH, MoveFileExW, WriteFile,
};
use windows::Win32::System::Threading::{GetCurrentProcessId, Sleep};
use windows::core::{HRESULT, PCWSTR};

use crate::error::PlatformError;
use crate::replace::{ReplaceApproval, replace_approved};
use crate::wide::wide_path;

const REPLACE_ATTEMPTS: u32 = 40;
const REPLACE_WAIT_MS: u32 = 50;

static TEMP_SERIAL: AtomicU32 = AtomicU32::new(1);

/// 一時 file を書き終えた後、対象の名前へどう置くか。
#[derive(Clone, Copy)]
enum Placement<'a> {
    /// 既存の file を無条件に置換する（session が持ち続けている保存先・自動復旧）。
    Replace,
    /// 既存の file があれば失敗する。
    CreateNew,
    /// 承認した file だけを置換する。
    Approved(&'a ReplaceApproval),
}

/// `path` を `bytes` で置換する。失敗したときは元の file を残し、一時 file は消す。
pub fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), PlatformError> {
    write_and_move(path, bytes, Placement::Replace)
}

/// `path` に file が無いときだけ `bytes` で作る。既にあれば何も書かずに失敗する。
pub fn atomic_create(path: &Path, bytes: &[u8]) -> Result<(), PlatformError> {
    write_and_move(path, bytes, Placement::CreateNew)
}

/// 置き換えを承認した file を `bytes` で置き換える。承認の後に別の file へ変わっていれば何も書かずに失敗する。
pub fn atomic_replace_approved(
    approval: &ReplaceApproval,
    bytes: &[u8],
) -> Result<(), PlatformError> {
    write_and_move(approval.path(), bytes, Placement::Approved(approval))
}

/// 書き終えた `temporary`（`target` と同じ directory）を `target` に置く。`approval` が無ければ既存の
/// file を置き換えず、あれば承認した file だけを置き換える。失敗しても `temporary` は消さない。
pub fn place_file(
    temporary: &Path,
    target: &Path,
    approval: Option<&ReplaceApproval>,
) -> Result<(), PlatformError> {
    match approval {
        None => move_to_new(&wide_path(temporary)?, &wide_path(target)?, target),
        Some(approval) => {
            let absolute = std::path::absolute(target).map_err(|error| {
                PlatformError::new(format!(
                    "パスを解決できません ({}): {error}",
                    target.display()
                ))
            })?;
            if absolute != approval.path() {
                return Err(PlatformError::new(format!(
                    "置き換えを承認した file と置き先が違います: {}",
                    target.display()
                )));
            }
            replace_approved(temporary, approval)
        }
    }
}

fn write_and_move(
    path: &Path,
    bytes: &[u8],
    placement: Placement<'_>,
) -> Result<(), PlatformError> {
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
    let result = match placement {
        Placement::Replace => replace(&temporary_wide, &target_wide),
        Placement::CreateNew => move_to_new(&temporary_wide, &target_wide, path),
        Placement::Approved(approval) => replace_approved(&temporary, approval),
    };
    if result.is_err() {
        file.delete();
    }
    result
}

fn temporary_path(target: &Path) -> Result<PathBuf, PlatformError> {
    // SAFETY: 現在 process の ID を読むだけで、失敗する引数は無い。
    let process_id = unsafe { GetCurrentProcessId() };
    let serial = TEMP_SERIAL.fetch_add(1, Ordering::Relaxed);
    let mut name = target.as_os_str().to_owned();
    name.push(format!(".mvmtmp.{process_id}.{serial}"));
    Ok(PathBuf::from(name))
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
    Err(last)
}

/// 置換しない rename。既存の target があれば失敗する（確認と rename の間に競合しない）。
pub(crate) fn move_to_new(
    temporary: &[u16],
    target: &[u16],
    path: &Path,
) -> Result<(), PlatformError> {
    // SAFETY: 両方とも NUL 終端の同じ directory 上のパス。REPLACE_EXISTING が無いので target は変えない。
    unsafe {
        MoveFileExW(
            PCWSTR(temporary.as_ptr()),
            PCWSTR(target.as_ptr()),
            MOVE_FILE_FLAGS(MOVEFILE_WRITE_THROUGH.0),
        )
    }
    .map_err(|error| {
        let code = error.code();
        if code == HRESULT::from_win32(ERROR_ALREADY_EXISTS.0)
            || code == HRESULT::from_win32(ERROR_FILE_EXISTS.0)
        {
            PlatformError::new(format!("既に file があります: {}", path.display()))
        } else {
            PlatformError::new(format!("file を作れません ({}): {error}", path.display()))
        }
    })
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
    fn create_refuses_an_existing_file_and_leaves_it_intact() {
        let dir = scratch_dir();
        let path = dir.join("project.mvm");
        atomic_create(&path, b"first").expect("新規作成");
        assert_eq!(fs::read(&path).expect("読み戻し"), b"first");
        let error = atomic_create(&path, b"second").expect_err("既存を置換しました");
        assert!(error.message().contains("既に"), "{error}");
        assert_eq!(fs::read(&path).expect("読み戻し"), b"first");
        // 置換する側は既存を置換できる（負の対照）。
        atomic_write(&path, b"third").expect("置換");
        assert_eq!(fs::read(&path).expect("読み戻し"), b"third");
        let names: Vec<_> = fs::read_dir(&dir)
            .expect("directory")
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, ["project.mvm"], "一時 file が残っています");
        fs::remove_dir_all(&dir).expect("片付け");
    }

    #[test]
    fn an_approved_file_is_replaced_only_while_it_is_the_same_file() {
        let dir = scratch_dir();
        let path = dir.join("lecture.mvm");
        assert!(
            ReplaceApproval::capture(&path)
                .expect("無い file")
                .is_none()
        );
        fs::write(&path, b"approved").expect("既存");
        let approval = ReplaceApproval::capture(&path)
            .expect("確かめる")
            .expect("ある");
        atomic_replace_approved(&approval, b"replaced").expect("承認した file の置き換え");
        assert_eq!(fs::read(&path).expect("読み戻し"), b"replaced");

        // 置き換えた後は、前の承認では置き換えない。
        let error = atomic_replace_approved(&approval, b"again").expect_err("古い承認");
        assert!(error.message().contains("変わりました"), "{error}");
        assert_eq!(fs::read(&path).expect("読み戻し"), b"replaced");

        // 承認の後に別の process が消して作り直した file も置き換えない。
        let approval = ReplaceApproval::capture(&path)
            .expect("確かめる")
            .expect("ある");
        fs::remove_file(&path).expect("消す");
        fs::write(&path, b"other process").expect("作り直す");
        atomic_replace_approved(&approval, b"mine").expect_err("差し替わった file");
        assert_eq!(fs::read(&path).expect("読み戻し"), b"other process");

        // 承認の後に消えた file は、置き換えずに失敗する。
        let approval = ReplaceApproval::capture(&path)
            .expect("確かめる")
            .expect("ある");
        fs::remove_file(&path).expect("消す");
        atomic_replace_approved(&approval, b"mine").expect_err("消えた file");
        assert!(!path.exists());

        // 他の process が書き込み中の file は置き換えない。
        fs::write(&path, b"busy").expect("作る");
        let approval = ReplaceApproval::capture(&path)
            .expect("確かめる")
            .expect("ある");
        let writer = fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .expect("書き込み中");
        atomic_replace_approved(&approval, b"mine").expect_err("書き込み中の file");
        drop(writer);
        assert_eq!(fs::read(&path).expect("読み戻し"), b"busy");

        let names: Vec<_> = fs::read_dir(&dir)
            .expect("directory")
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            names,
            ["lecture.mvm"],
            "一時 file か退避した file が残っています"
        );
        fs::remove_dir_all(&dir).expect("片付け");
    }

    #[test]
    fn placing_a_file_without_approval_never_replaces() {
        let dir = scratch_dir();
        let target = dir.join("out.mp4");
        let first = dir.join("first.tmp");
        fs::write(&first, b"first").expect("一時 file");
        place_file(&first, &target, None).expect("新しい file");
        assert_eq!(fs::read(&target).expect("読み戻し"), b"first");
        let second = dir.join("second.tmp");
        fs::write(&second, b"second").expect("一時 file");
        place_file(&second, &target, None).expect_err("承認の無い置換");
        assert_eq!(fs::read(&target).expect("読み戻し"), b"first");
        assert!(second.is_file(), "失敗した一時 file は呼び出し側が片付ける");

        let approval = ReplaceApproval::capture(&target)
            .expect("確かめる")
            .expect("ある");
        place_file(&second, &dir.join("other.mp4"), Some(&approval)).expect_err("別の置き先");
        place_file(&second, &target, Some(&approval)).expect("承認した置換");
        assert_eq!(fs::read(&target).expect("読み戻し"), b"second");
        assert!(!second.exists());
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
