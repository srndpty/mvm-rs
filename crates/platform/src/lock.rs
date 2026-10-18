//! Project file の排他 lock。`.mvm.lock` を共有なしで開き、閉じると消える。
//!
//! 他の process が持っている間は共有違反になり、こちらは開かない。

use std::path::{Path, PathBuf};

use windows::Win32::Foundation::{
    CloseHandle, ERROR_SHARING_VIOLATION, GENERIC_READ, GENERIC_WRITE, HANDLE,
};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FILE_ATTRIBUTE_TEMPORARY, FILE_FLAG_DELETE_ON_CLOSE, FILE_SHARE_MODE, OPEN_ALWAYS,
};
use windows::core::{HRESULT, PCWSTR};

use crate::error::PlatformError;
use crate::wide::wide_path;

pub struct ProjectLock {
    handle: HANDLE,
    open: bool,
    path: PathBuf,
}

impl ProjectLock {
    pub fn path(&self) -> &Path {
        &self.path
    }
}

pub fn acquire_project_lock(project_path: &Path) -> Result<ProjectLock, PlatformError> {
    if project_path.as_os_str().is_empty() {
        return Err(PlatformError::new("Project のパスが空です"));
    }
    if let Some(parent) = project_path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent).map_err(|error| {
            PlatformError::new(format!(
                "Project の directory を作れません ({}): {error}",
                parent.display()
            ))
        })?;
    }
    let mut lock_path = project_path.as_os_str().to_owned();
    lock_path.push(".lock");
    let lock_path = PathBuf::from(lock_path);
    let wide = wide_path(&lock_path)?;
    // SAFETY: path は NUL 終端。共有 0 なので、既に開いている lock は共有違反で失敗する。
    // DELETE_ON_CLOSE は、この handle を閉じたとき（crash を含む）に lock file を消す。
    let handle = unsafe {
        CreateFileW(
            PCWSTR(wide.as_ptr()),
            GENERIC_READ.0 | GENERIC_WRITE.0,
            FILE_SHARE_MODE(0),
            None,
            OPEN_ALWAYS,
            FILE_ATTRIBUTE_TEMPORARY | FILE_FLAG_DELETE_ON_CLOSE,
            None,
        )
    }
    .map_err(|error| {
        if error.code() == HRESULT::from_win32(ERROR_SHARING_VIOLATION.0) {
            PlatformError::new("他の mvm がこの Project を開いています")
        } else {
            PlatformError::new(format!("Project lock を取得できません: {error}"))
        }
    })?;
    Ok(ProjectLock {
        handle,
        open: true,
        path: lock_path,
    })
}

impl Drop for ProjectLock {
    fn drop(&mut self) {
        if !self.open {
            return;
        }
        self.open = false;
        // SAFETY: acquire が返した handle をここで 1 回だけ閉じる。
        unsafe {
            let _ = CloseHandle(self.handle);
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use std::fs;
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;

    static SCRATCH: AtomicU64 = AtomicU64::new(1);

    fn scratch() -> PathBuf {
        let n = SCRATCH.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("mvm-lock-{}-{n}", std::process::id()));
        fs::create_dir_all(&dir).expect("作業 directory");
        dir
    }

    #[test]
    fn a_second_lock_fails_until_the_first_is_dropped() {
        let dir = scratch();
        let project = dir.join("edit.mvm");
        fs::write(&project, b"{}").expect("書く");
        let first = acquire_project_lock(&project).expect("1 つ目");
        assert!(first.path().is_file(), "lock file が見えません");
        match acquire_project_lock(&project) {
            Err(error) => assert!(error.message().contains("他の mvm"), "{error}"),
            Ok(_lock) => panic!("2 つ目の lock が取得できました"),
        }
        drop(first);
        let third = acquire_project_lock(&project).expect("解放後");
        drop(third);
        assert!(
            !dir.join("edit.mvm.lock").exists(),
            "閉じたあとも lock file が残っています"
        );
        fs::remove_dir_all(&dir).expect("片付け");
    }
}
