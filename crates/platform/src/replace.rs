//! 利用者が置き換えを承認した既存 file だけを置き換える。
//!
//! 承認した時点の実体（volume・file ID・大きさ・更新時刻）を [`ReplaceApproval`] に覚える。置き換える直前に
//! その path を、他の process の書き込みと削除を拒む handle で開いて照合し、照合した handle のまま退避名へ
//! rename する。新しい file は既存を置換しない rename で置き、最後に退避した file を消す。照合から置き換えまでの
//! 間に、承認していない file へ差し替わることはない（差し替わっていれば照合か rename が失敗する）。

use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use windows::Win32::Foundation::{
    CloseHandle, ERROR_ACCESS_DENIED, ERROR_FILE_NOT_FOUND, ERROR_LOCK_VIOLATION,
    ERROR_PATH_NOT_FOUND, ERROR_SHARING_VIOLATION, HANDLE,
};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, DELETE, FILE_ATTRIBUTE_NORMAL, FILE_DISPOSITION_INFO, FILE_READ_ATTRIBUTES,
    FILE_RENAME_INFO, FILE_SHARE_READ, FileDispositionInfo, FileRenameInfo, OPEN_EXISTING,
    SetFileInformationByHandle,
};
use windows::Win32::System::Threading::Sleep;
use windows::core::{HRESULT, PCWSTR};

use crate::error::PlatformError;
use crate::file_id::{FileProbe, FileProbeStatus, probe_file, read_identity};
use crate::wide::wide_path;

const OPEN_ATTEMPTS: u32 = 40;
const OPEN_WAIT_MS: u32 = 50;

static BACKUP_SERIAL: AtomicU32 = AtomicU32::new(1);

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

    /// 承認した file の絶対パス。
    pub fn path(&self) -> &Path {
        &self.path
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
    let target_wide = wide_path(target)?;
    let guard = Guarded::open(&target_wide, target)?;
    if !approval.matches(&read_identity(guard.handle)) {
        return Err(PlatformError::new(format!(
            "置き換えを承認した後に file が変わりました: {}",
            target.display()
        )));
    }
    let backup = backup_path(target);
    guard.rename(&backup).map_err(|error| {
        PlatformError::new(format!(
            "置き換える file を退避できません ({}): {error}",
            target.display()
        ))
    })?;
    let temporary_wide = wide_path(temporary)?;
    if let Err(error) = crate::atomic_write::move_to_new(&temporary_wide, &target_wide, target) {
        // 退避した file を元の名前へ戻す。戻せなければ、退避先を知らせる（file は失っていない）。
        return Err(match guard.rename(target) {
            Ok(()) => error,
            Err(restore) => PlatformError::new(format!(
                "{error}。元の file を戻せません（{} に残っています）: {restore}",
                backup.display()
            )),
        });
    }
    guard.delete();
    Ok(())
}

fn backup_path(target: &Path) -> PathBuf {
    let serial = BACKUP_SERIAL.fetch_add(1, Ordering::Relaxed);
    let mut name = target.as_os_str().to_owned();
    name.push(format!(".mvmold.{}.{serial}", std::process::id()));
    PathBuf::from(name)
}

/// 他の process の書き込みと削除を拒んで開いた、置き換える file。
struct Guarded {
    handle: HANDLE,
}

impl Guarded {
    fn open(wide: &[u16], path: &Path) -> Result<Self, PlatformError> {
        let mut last = None;
        for attempt in 0..OPEN_ATTEMPTS {
            // SAFETY: wide は NUL 終端。共有は読み取りだけなので、開いている間は他の process が書き換えも
            // 削除もできない。失敗時に handle は返らない。
            let opened = unsafe {
                CreateFileW(
                    PCWSTR(wide.as_ptr()),
                    DELETE.0 | FILE_READ_ATTRIBUTES.0,
                    FILE_SHARE_READ,
                    None,
                    OPEN_EXISTING,
                    FILE_ATTRIBUTE_NORMAL,
                    None,
                )
            };
            match opened {
                Ok(handle) => return Ok(Self { handle }),
                Err(error) if missing(&error) => {
                    return Err(PlatformError::new(format!(
                        "置き換えを承認した file がありません: {}",
                        path.display()
                    )));
                }
                Err(error) if busy(&error) && attempt + 1 < OPEN_ATTEMPTS => {
                    last = Some(error);
                    // SAFETY: 待つ時間は定数。他の状態は変えない。
                    unsafe { Sleep(OPEN_WAIT_MS) };
                }
                Err(error) => {
                    last = Some(error);
                    break;
                }
            }
        }
        Err(PlatformError::new(format!(
            "置き換える file を開けません ({}): {}",
            path.display(),
            last.map_or_else(String::new, |error| error.to_string())
        )))
    }

    /// 開いている file の名前を `to` へ変える。`to` に file があれば失敗する。
    fn rename(&self, to: &Path) -> Result<(), PlatformError> {
        let name: Vec<u16> = to.as_os_str().encode_wide().collect();
        let name_bytes = name
            .len()
            .checked_mul(2)
            .and_then(|bytes| u32::try_from(bytes).ok())
            .ok_or_else(|| PlatformError::new("パスが長すぎます"))?;
        let header = std::mem::offset_of!(FILE_RENAME_INFO, FileName);
        let total = header + name.len() * 2 + 2;
        // FILE_RENAME_INFO は HANDLE を含むので 8 byte 境界に置く。
        let mut buffer = vec![0u64; total.div_ceil(8)];
        let info = buffer.as_mut_ptr().cast::<FILE_RENAME_INFO>();
        // SAFETY: buffer は FILE_RENAME_INFO の頭と名前（NUL 終端）が収まる大きさで、8 byte 境界にある。
        // 名前は FileName の位置から書き、buffer の外へは出ない。
        unsafe {
            (*info).Anonymous.ReplaceIfExists = false;
            (*info).RootDirectory = HANDLE::default();
            (*info).FileNameLength = name_bytes;
            let target = buffer.as_mut_ptr().cast::<u8>().add(header).cast::<u16>();
            std::ptr::copy_nonoverlapping(name.as_ptr(), target, name.len());
        }
        let size = u32::try_from(total).map_err(|_| PlatformError::new("パスが長すぎます"))?;
        // SAFETY: handle は DELETE 権限で開いた file。buffer は上で組んだ FILE_RENAME_INFO で、呼び出しの間生きている。
        unsafe {
            SetFileInformationByHandle(self.handle, FileRenameInfo, buffer.as_ptr().cast(), size)
        }
        .map_err(|error| PlatformError::new(error.to_string()))
    }

    /// 閉じたときに消す。置き換えが済んだ後の、退避した file に使う。
    fn delete(self) {
        let info = FILE_DISPOSITION_INFO { DeleteFile: true };
        // SAFETY: handle は DELETE 権限で開いた file。info は呼び出しの間生きている。消せなくても退避名の
        // file が残るだけで、置いた file には触れない。
        unsafe {
            let _ = SetFileInformationByHandle(
                self.handle,
                FileDispositionInfo,
                (&info as *const FILE_DISPOSITION_INFO).cast(),
                std::mem::size_of::<FILE_DISPOSITION_INFO>() as u32,
            );
        }
    }
}

impl Drop for Guarded {
    fn drop(&mut self) {
        // SAFETY: open が返した handle をここで 1 回だけ閉じる。
        unsafe {
            let _ = CloseHandle(self.handle);
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
