//! 利用者が置き換えを承認した既存 file だけを置き換える。
//!
//! 承認した時点の実体（volume・file ID・大きさ・更新時刻）を [`ReplaceApproval`] に覚える。置き換える直前に
//! その path を、他の process の書き込みを拒む handle で開いて照合し、書き終えた一時 file を POSIX semantics の
//! rename 1 回で対象の名前へ置く（既存を置き換える）。
//!
//! - 途中で process が落ちても、対象の名前は常に前の file か新しい file を指す。退避名は使わない。
//! - 照合から置き換えまで、他の process は承認した file の中身を書き換えられない。
//! - 単一操作の置き換えは、置き換えられる file を開いている全ての handle が削除の共有を許すときだけ成功する
//!   [事実: 2026-10-20 に NTFS で確かめた。削除の共有を拒む handle があると共有違反になる]。そのため照合中の
//!   handle も削除（名前の変更）を共有する。照合から置き換えまでの数 µs の間に、他の process が承認した file を
//!   別名へ移し、同じ名前で新しい file を作ると、その file を置き換えてしまう。置き換える前と後で照合した file の
//!   名前（hard link）の数を比べ、1 つ減っていなければ失敗として報告する。対象が他にも hard link を持つ file でも、
//!   置き換えで失うのは対象の名前 1 つなので成功と判定する。
//! - POSIX semantics の rename に対応しない file system（FAT 系など）では、置き換えずに失敗する。

use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use windows::Win32::Foundation::{
    CloseHandle, ERROR_ACCESS_DENIED, ERROR_FILE_NOT_FOUND, ERROR_INVALID_PARAMETER,
    ERROR_LOCK_VIOLATION, ERROR_NOT_SUPPORTED, ERROR_PATH_NOT_FOUND, ERROR_SHARING_VIOLATION,
    HANDLE,
};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, DELETE, FILE_ATTRIBUTE_NORMAL, FILE_READ_ATTRIBUTES, FILE_RENAME_INFO,
    FILE_SHARE_DELETE, FILE_SHARE_MODE, FILE_SHARE_READ, FILE_SHARE_WRITE, FILE_STANDARD_INFO,
    FileRenameInfoEx, FileStandardInfo, GetFileInformationByHandleEx, OPEN_EXISTING,
    SetFileInformationByHandle,
};
use windows::Win32::System::Threading::Sleep;
use windows::core::{HRESULT, PCWSTR};

use crate::error::PlatformError;
use crate::file_id::{FileProbe, FileProbeStatus, probe_file, read_identity};
use crate::wide::wide_path;

const OPEN_ATTEMPTS: u32 = 40;
const OPEN_WAIT_MS: u32 = 50;

/// `FILE_RENAME_INFO.Flags`（winbase.h）。windows crate に定数が無いので書く。
const FILE_RENAME_FLAG_REPLACE_IF_EXISTS: u32 = 0x1;
const FILE_RENAME_FLAG_POSIX_SEMANTICS: u32 = 0x2;

/// 置き換えを承認した既存 file。承認した時点の実体を覚え、置き換える直前に照合する。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReplaceApproval {
    path: PathBuf,
    probe: FileProbe,
}

impl ReplaceApproval {
    /// `path` の今の実体を覚える。file が無ければ `None`（置き換えるものが無い）。
    /// 実体を確かめられない path（directory・開けない file）は失敗にする。
    pub fn capture(path: &Path) -> Result<Option<Self>, PlatformError> {
        let path = std::path::absolute(path).map_err(|error| {
            PlatformError::new(format!(
                "パスを解決できません ({}): {error}",
                path.display()
            ))
        })?;
        let probe = probe_file(&path);
        match probe.status {
            FileProbeStatus::Missing => Ok(None),
            FileProbeStatus::File => Ok(Some(Self { path, probe })),
            FileProbeStatus::Unavailable => Err(PlatformError::new(format!(
                "既にある file を確かめられません: {}",
                path.display()
            ))),
        }
    }

    /// この process が `path`（絶対パス）に置いた file の実体。
    pub(crate) fn placed(path: PathBuf, probe: FileProbe) -> Self {
        Self { path, probe }
    }

    /// 承認した file の絶対パス。
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// `path` の今の実体が承認したものと同じか。
    pub fn is_current(&self) -> bool {
        self.matches(&probe_file(&self.path))
    }

    fn matches(&self, probe: &FileProbe) -> bool {
        probe.status == FileProbeStatus::File
            && probe.volume_serial == self.probe.volume_serial
            && probe.file_id == self.probe.file_id
            && probe.size == self.probe.size
            && probe.last_write_100ns == self.probe.last_write_100ns
    }
}

/// `temporary`（同じ directory の、書き終えた file）を、承認した file と置き換える。
/// 失敗したときは承認した file を元の名前に残す。`temporary` は呼び出し側が片付ける。
pub(crate) fn replace_approved(
    temporary: &Path,
    approval: &ReplaceApproval,
) -> Result<(), PlatformError> {
    let target = approval.path();
    let guard = Handle::open_with_retry(
        target,
        DELETE.0 | FILE_READ_ATTRIBUTES.0,
        FILE_SHARE_READ | FILE_SHARE_DELETE,
    )
    .map_err(|error| match error {
        OpenError::Missing => PlatformError::new(format!(
            "置き換えを承認した file がありません: {}",
            target.display()
        )),
        OpenError::Other(message) => PlatformError::new(format!(
            "置き換える file を開けません ({}): {message}",
            target.display()
        )),
    })?;
    if !approval.matches(&read_identity(guard.0)) {
        return Err(PlatformError::new(format!(
            "置き換えを承認した後に file が変わりました: {}",
            target.display()
        )));
    }
    let links_before = guard.link_count().map_err(|error| {
        PlatformError::new(format!(
            "置き換える file を確かめられません ({}): {error}",
            target.display()
        ))
    })?;
    let incoming = Handle::open_with_retry(
        temporary,
        DELETE.0 | FILE_READ_ATTRIBUTES.0,
        FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
    )
    .map_err(|error| {
        PlatformError::new(format!(
            "書き終えた一時 file を開けません ({}): {}",
            temporary.display(),
            match error {
                OpenError::Missing => "ありません".to_owned(),
                OpenError::Other(message) => message,
            }
        ))
    })?;
    incoming.rename_replacing(target).map_err(|error| {
        let code = error.code();
        let message = if code == HRESULT::from_win32(ERROR_INVALID_PARAMETER.0)
            || code == HRESULT::from_win32(ERROR_NOT_SUPPORTED.0)
        {
            "この drive の file system は、単一操作での置き換え（POSIX semantics の rename）に対応していません。別の名前で保存してください".to_owned()
        } else {
            error.to_string()
        };
        PlatformError::new(format!(
            "file を置き換えられません ({}): {message}",
            target.display()
        ))
    })?;
    match guard.link_count() {
        Ok(links_after) if lost_one_name(links_before, links_after) => Ok(()),
        Ok(_) => Err(PlatformError::new(format!(
            "置き換える直前に、承認した file が別の名前へ移されました。{} にあった別の file を置き換えた可能性があります",
            target.display()
        ))),
        Err(error) => Err(PlatformError::new(format!(
            "置き換えた file を確かめられません ({}): {error}",
            target.display()
        ))),
    }
}

/// 照合した file が置き換えで対象の名前を失ったか。他の hard link は残るので、0 ではなく 1 つ減ったことを見る。
/// 別名へ移された file は名前の数が変わらない。
fn lost_one_name(before: u32, after: u32) -> bool {
    before.checked_sub(1) == Some(after)
}

enum OpenError {
    Missing,
    Other(String),
}

/// この module が開いた file の handle。drop で閉じる。
struct Handle(HANDLE);

impl Handle {
    /// 他の process が使用中なら短い間待ってやり直す。
    fn open_with_retry(
        path: &Path,
        access: u32,
        share: FILE_SHARE_MODE,
    ) -> Result<Self, OpenError> {
        let wide = wide_path(path).map_err(|error| OpenError::Other(error.to_string()))?;
        let mut last = String::new();
        for attempt in 0..OPEN_ATTEMPTS {
            // SAFETY: wide は NUL 終端。共有の指定で、開いている間に他の process ができる操作を決める。
            // 失敗時に handle は返らない。
            let opened = unsafe {
                CreateFileW(
                    PCWSTR(wide.as_ptr()),
                    access,
                    share,
                    None,
                    OPEN_EXISTING,
                    FILE_ATTRIBUTE_NORMAL,
                    None,
                )
            };
            match opened {
                Ok(handle) => return Ok(Self(handle)),
                Err(error) if missing(&error) => return Err(OpenError::Missing),
                Err(error) if busy(&error) && attempt + 1 < OPEN_ATTEMPTS => {
                    last = error.to_string();
                    // SAFETY: 待つ時間は定数。他の状態は変えない。
                    unsafe { Sleep(OPEN_WAIT_MS) };
                }
                Err(error) => return Err(OpenError::Other(error.to_string())),
            }
        }
        Err(OpenError::Other(last))
    }

    /// 開いている file を `to` へ置く。`to` に file があれば、POSIX semantics で置き換える（1 回の操作）。
    fn rename_replacing(&self, to: &Path) -> windows::core::Result<()> {
        let name: Vec<u16> = to.as_os_str().encode_wide().collect();
        let name_bytes = name
            .len()
            .checked_mul(2)
            .and_then(|bytes| u32::try_from(bytes).ok())
            .ok_or_else(|| {
                windows::core::Error::from(HRESULT::from_win32(ERROR_INVALID_PARAMETER.0))
            })?;
        let header = std::mem::offset_of!(FILE_RENAME_INFO, FileName);
        let total = header + name.len() * 2 + 2;
        let size = u32::try_from(total).map_err(|_| {
            windows::core::Error::from(HRESULT::from_win32(ERROR_INVALID_PARAMETER.0))
        })?;
        // FILE_RENAME_INFO は HANDLE を含むので 8 byte 境界に置く。
        let mut buffer = vec![0u64; total.div_ceil(8)];
        let info = buffer.as_mut_ptr().cast::<FILE_RENAME_INFO>();
        // SAFETY: buffer は FILE_RENAME_INFO の頭と名前（NUL 終端）が収まる大きさで、8 byte 境界にある。
        // 名前は FileName の位置から書き、buffer の外へは出ない。
        unsafe {
            (*info).Anonymous.Flags =
                FILE_RENAME_FLAG_REPLACE_IF_EXISTS | FILE_RENAME_FLAG_POSIX_SEMANTICS;
            (*info).RootDirectory = HANDLE::default();
            (*info).FileNameLength = name_bytes;
            let target = buffer.as_mut_ptr().cast::<u8>().add(header).cast::<u16>();
            std::ptr::copy_nonoverlapping(name.as_ptr(), target, name.len());
        }
        // SAFETY: handle は DELETE 権限で開いた file。buffer は上で組んだ FILE_RENAME_INFO で、呼び出しの間生きている。
        unsafe {
            SetFileInformationByHandle(self.0, FileRenameInfoEx, buffer.as_ptr().cast(), size)
        }
    }

    /// file の名前（hard link）の数。名前を全て失った file は 0。
    fn link_count(&self) -> windows::core::Result<u32> {
        let mut info = FILE_STANDARD_INFO::default();
        // SAFETY: handle は FILE_READ_ATTRIBUTES で開いた file。info は呼び出しの間生きている POD。
        unsafe {
            GetFileInformationByHandleEx(
                self.0,
                FileStandardInfo,
                (&mut info as *mut FILE_STANDARD_INFO).cast(),
                std::mem::size_of::<FILE_STANDARD_INFO>() as u32,
            )
        }?;
        Ok(info.NumberOfLinks)
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        // SAFETY: open_with_retry が返した handle をここで 1 回だけ閉じる。
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

fn missing(error: &windows::core::Error) -> bool {
    let code = error.code();
    code == HRESULT::from_win32(ERROR_FILE_NOT_FOUND.0)
        || code == HRESULT::from_win32(ERROR_PATH_NOT_FOUND.0)
}

fn busy(error: &windows::core::Error) -> bool {
    let code = error.code();
    code == HRESULT::from_win32(ERROR_ACCESS_DENIED.0)
        || code == HRESULT::from_win32(ERROR_SHARING_VIOLATION.0)
        || code == HRESULT::from_win32(ERROR_LOCK_VIOLATION.0)
}

#[cfg(test)]
mod tests {
    use super::lost_one_name;

    #[test]
    fn only_losing_exactly_one_name_counts_as_replaced() {
        assert!(lost_one_name(1, 0));
        assert!(lost_one_name(3, 2));
        // 別名へ移された file（数が変わらない）や、名前が増えた file は置き換わっていない。
        assert!(!lost_one_name(1, 1));
        assert!(!lost_one_name(2, 2));
        assert!(!lost_one_name(2, 3));
        assert!(!lost_one_name(3, 1));
        assert!(!lost_one_name(0, 0));
    }
}
