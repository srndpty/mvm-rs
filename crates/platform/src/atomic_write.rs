//! 同じ directory の一時 file へ書いて flush してから、対象の名前へ置く。
//!
//! 置き方は [`CommitPolicy`] で決める。どの置き方も、対象の名前は常に「前の file」か「書き終えた新しい
//! file」のどちらかを指す（rename 1 回で切り替える）。途中で process が落ちても、対象の名前が消えたり
//! 書きかけの中身になったりしない。残るのは `<対象>.mvmtmp.<process ID>.<連番>` の一時 file だけ。

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
use crate::file_id::{FileProbeStatus, probe_file};
use crate::replace::{ReplaceApproval, replace_approved};
use crate::wide::wide_path;

const REPLACE_ATTEMPTS: u32 = 40;
const REPLACE_WAIT_MS: u32 = 50;

static TEMP_SERIAL: AtomicU32 = AtomicU32::new(1);

/// 書き終えた file を対象の名前へ置くときの、既存の file の扱い。保存の経路はどれもこのどれかを選ぶ。
#[derive(Clone, Copy, Debug)]
pub enum CommitPolicy<'a> {
    /// 既存の file があれば何も変えずに失敗する（新規作成・新しい書き出し先）。
    CreateNew,
    /// 承認した実体のときだけ置き換える（上書き保存・確認した上書き）。照合の後は置き換えまで
    /// 他の process に書き換えさせない。
    ReplaceApproved(&'a ReplaceApproval),
    /// 無条件に置き換える。この process だけが書く file（自動復旧データ・設定・cache）に限る。
    ReplaceAny,
}

/// `path` を `bytes` で無条件に置き換える（[`CommitPolicy::ReplaceAny`]）。
pub fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), PlatformError> {
    commit_bytes(path, bytes, CommitPolicy::ReplaceAny).map(drop)
}

/// `bytes` を一時 file に書いて flush し、`policy` に従って `path` へ置く。失敗したときは対象を変えず、
/// 一時 file を消す。成功したら、置いた file の実体（次に置き換えるときの承認）を返す。
pub fn commit_bytes(
    path: &Path,
    bytes: &[u8],
    policy: CommitPolicy<'_>,
) -> Result<ReplaceApproval, PlatformError> {
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
    let result = commit_file(&temporary, path, policy);
    if result.is_err() {
        file.delete();
    }
    result
}

/// 書き終えた `temporary`（`target` と同じ directory にある、この process だけが知る名前）を、`policy` に
/// 従って `target` へ置く。成功したら置いた file の実体を返す。失敗しても `temporary` は消さない。
pub fn commit_file(
    temporary: &Path,
    target: &Path,
    policy: CommitPolicy<'_>,
) -> Result<ReplaceApproval, PlatformError> {
    let target = std::path::absolute(target).map_err(|error| {
        PlatformError::new(format!(
            "パスを解決できません ({}): {error}",
            target.display()
        ))
    })?;
    // 名前を変えても実体（file ID・大きさ・更新時刻）は変わらない。一時 file の名前は他の process が
    // 知らないので、ここで読んだ実体がそのまま置いた file の実体になる。
    let placed = probe_file(temporary);
    if placed.status != FileProbeStatus::File {
        return Err(PlatformError::new(format!(
            "書き終えた一時 file を確かめられません: {}",
            temporary.display()
        )));
    }
    let temporary_wide = wide_path(temporary)?;
    let target_wide = wide_path(&target)?;
    match policy {
        CommitPolicy::CreateNew => move_to_new(&temporary_wide, &target_wide, &target)?,
        CommitPolicy::ReplaceAny => replace(&temporary_wide, &target_wide)?,
        CommitPolicy::ReplaceApproved(approval) => {
            if target != approval.path() {
                return Err(PlatformError::new(format!(
                    "置き換えを承認した file と置き先が違います: {}",
                    target.display()
                )));
            }
            replace_approved(temporary, approval)?;
        }
    }
    Ok(ReplaceApproval::placed(target, placed))
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
    let mut last = PlatformError::new("file を置換できません");
    for attempt in 0..REPLACE_ATTEMPTS {
        // SAFETY: 両方とも NUL 終端の同じ directory 上のパス。成功すると temporary は target になる。
        match unsafe { MoveFileExW(PCWSTR(temporary.as_ptr()), PCWSTR(target.as_ptr()), flags) } {
            Ok(()) => return Ok(()),
            Err(error) if retryable(&error) && attempt + 1 < REPLACE_ATTEMPTS => {
                last = PlatformError::new(format!("file を置換できません: {error}"));
                // SAFETY: 待つ時間は定数。他の状態は変えない。
                unsafe { Sleep(REPLACE_WAIT_MS) };
            }
            Err(error) => {
                last = PlatformError::new(format!("file を置換できません: {error}"));
                break;
            }
        }
    }
    Err(last)
}

/// 置換しない rename。既存の target があれば失敗する（確認と rename の間に競合しない）。
fn move_to_new(temporary: &[u16], target: &[u16], path: &Path) -> Result<(), PlatformError> {
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
        commit_bytes(&path, b"first", CommitPolicy::CreateNew).expect("新規作成");
        assert_eq!(fs::read(&path).expect("読み戻し"), b"first");
        let error = commit_bytes(&path, b"second", CommitPolicy::CreateNew)
            .expect_err("既存を置換しました");
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
        replace_with(&approval, b"replaced").expect("承認した file の置き換え");
        assert_eq!(fs::read(&path).expect("読み戻し"), b"replaced");

        // 置き換えた後は、前の承認では置き換えない。
        let error = replace_with(&approval, b"again").expect_err("古い承認");
        assert!(error.message().contains("変わりました"), "{error}");
        assert_eq!(fs::read(&path).expect("読み戻し"), b"replaced");

        // 承認の後に別の process が消して作り直した file も置き換えない。
        let approval = ReplaceApproval::capture(&path)
            .expect("確かめる")
            .expect("ある");
        fs::remove_file(&path).expect("消す");
        fs::write(&path, b"other process").expect("作り直す");
        replace_with(&approval, b"mine").expect_err("差し替わった file");
        assert_eq!(fs::read(&path).expect("読み戻し"), b"other process");

        // 承認の後に消えた file は、置き換えずに失敗する。
        let approval = ReplaceApproval::capture(&path)
            .expect("確かめる")
            .expect("ある");
        fs::remove_file(&path).expect("消す");
        replace_with(&approval, b"mine").expect_err("消えた file");
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
        replace_with(&approval, b"mine").expect_err("書き込み中の file");
        drop(writer);
        assert_eq!(fs::read(&path).expect("読み戻し"), b"busy");

        let names: Vec<_> = fs::read_dir(&dir)
            .expect("directory")
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, ["lecture.mvm"], "一時 file が残っています");
        fs::remove_dir_all(&dir).expect("片付け");
    }

    #[test]
    fn an_approved_file_with_another_hard_link_is_replaced() {
        let dir = scratch_dir();
        let path = dir.join("lecture.mvm");
        let other = dir.join("backup.mvm");
        fs::write(&path, b"approved").expect("既存");
        fs::hard_link(&path, &other).expect("hard link");
        let approval = ReplaceApproval::capture(&path)
            .expect("確かめる")
            .expect("ある");
        let placed =
            replace_with(&approval, b"replaced").expect("hard link のある file の置き換え");
        assert_eq!(fs::read(&path).expect("読み戻し"), b"replaced");
        // 置き換えるのは対象の名前だけで、他の名前は前の中身を指したまま残る。
        assert_eq!(fs::read(&other).expect("他の名前"), b"approved");
        assert!(placed.is_current());
        fs::remove_dir_all(&dir).expect("片付け");
    }

    fn replace_with(
        approval: &ReplaceApproval,
        bytes: &[u8],
    ) -> Result<ReplaceApproval, PlatformError> {
        commit_bytes(
            approval.path(),
            bytes,
            CommitPolicy::ReplaceApproved(approval),
        )
    }

    #[test]
    fn the_returned_identity_approves_the_next_save_until_someone_else_writes() {
        let dir = scratch_dir();
        let path = dir.join("lecture.mvm");
        let mut last = commit_bytes(&path, b"v0", CommitPolicy::CreateNew).expect("作成");
        for version in 1..20u8 {
            last = replace_with(&last, &[version; 64]).expect("前に書いた file の置き換え");
            assert_eq!(fs::read(&path).expect("読み戻し"), [version; 64]);
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
        fs::write(&path, b"another tool").expect("外部の書き込み");
        replace_with(&last, b"mine").expect_err("外部で書き換えた file を置き換えました");
        assert_eq!(fs::read(&path).expect("読み戻し"), b"another tool");
        fs::remove_dir_all(&dir).expect("片付け");
    }

    /// 置き換えを `rounds` 回繰り返す間、別の thread から対象の名前を見続け、名前が無かった回数を返す。
    fn missing_while(path: &Path, rounds: u32, mut replace_once: impl FnMut(u32)) -> u64 {
        use std::sync::Arc;
        use std::sync::atomic::AtomicBool;
        let done = Arc::new(AtomicBool::new(false));
        let watcher = {
            let (path, done) = (path.to_path_buf(), Arc::clone(&done));
            std::thread::spawn(move || {
                let mut missing = 0u64;
                while !done.load(Ordering::Acquire) {
                    if probe_file(&path).status == FileProbeStatus::Missing {
                        missing += 1;
                    }
                }
                missing
            })
        };
        for version in 1..=rounds {
            replace_once(version);
        }
        done.store(true, Ordering::Release);
        watcher.join().expect("watcher")
    }

    #[test]
    fn the_target_name_never_disappears_while_replacing() {
        let dir = scratch_dir();
        let path = dir.join("watched.mvm");
        let mut last = commit_bytes(&path, &[0; 4096], CommitPolicy::CreateNew).expect("作成");
        let missing = missing_while(&path, 200, |version| {
            last = replace_with(&last, &[(version % 251) as u8; 4096]).expect("置き換え");
        });
        assert_eq!(missing, 0, "置き換えの間に名前が無い瞬間がありました");

        // 負の対照: 退避名へ移してから置く 2 段の置き換えなら、この見張りは名前の無い瞬間を見つける。
        let backup = dir.join("watched.mvm.old");
        let temporary = dir.join("watched.mvm.new");
        let missing = missing_while(&path, 200, |version| {
            fs::write(&temporary, [(version % 251) as u8; 4096]).expect("一時 file");
            fs::rename(&path, &backup).expect("退避");
            fs::rename(&temporary, &path).expect("置く");
            fs::remove_file(&backup).expect("退避の削除");
        });
        assert!(missing > 0, "見張りが 2 段の置き換えの隙間を見つけません");
        fs::remove_dir_all(&dir).expect("片付け");
    }

    /// 子 process が置き換えを繰り返す途中で強制終了させ、対象が常に完全な版のどれかであることを確かめる。
    /// 子は [`crash_injection_child`] で、`MVM_CRASH_CHILD_DIR` を受け取ったときだけ動く。
    #[test]
    fn a_process_killed_while_replacing_leaves_a_complete_version() {
        const SIZE: usize = 256 * 1024;
        let dir = scratch_dir();
        let path = dir.join("crash.mvm");
        commit_bytes(&path, &[0; SIZE], CommitPolicy::CreateNew).expect("作成");
        let exe = std::env::current_exe().expect("試験の実行 file");
        let mut seen = std::collections::BTreeSet::new();
        for round in 0..12u64 {
            let mut child = std::process::Command::new(&exe)
                .args([
                    "--exact",
                    "atomic_write::tests::crash_injection_child",
                    "--include-ignored",
                    "--test-threads=1",
                ])
                .env("MVM_CRASH_CHILD_DIR", &dir)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
                .expect("子 process");
            // 置き換えの途中に当たるよう、終了させる時刻をずらす。
            std::thread::sleep(std::time::Duration::from_millis(150 + round * 37));
            child.kill().expect("強制終了");
            let _ = child.wait();
            let bytes = fs::read(&path).expect("強制終了の後も対象の名前がある");
            assert_eq!(bytes.len(), SIZE, "round {round}: 書きかけの中身です");
            let first = bytes[0];
            assert!(
                bytes.iter().all(|byte| *byte == first),
                "round {round}: 版が混ざっています"
            );
            seen.insert(first);
        }
        assert!(
            seen.len() > 1,
            "子 process が置き換える前に終了させています: {seen:?}"
        );
        fs::remove_dir_all(&dir).expect("片付け");
    }

    #[test]
    #[ignore = "crash injection の子 process（親の試験が起動する）"]
    fn crash_injection_child() {
        let Some(dir) = std::env::var_os("MVM_CRASH_CHILD_DIR") else {
            return;
        };
        let path = PathBuf::from(dir).join("crash.mvm");
        let mut last = ReplaceApproval::capture(&path)
            .expect("確かめる")
            .expect("ある");
        // 親が強制終了させるまで置き換え続ける。
        let mut version = 0u32;
        loop {
            version = version.wrapping_add(1);
            let byte = (version % 250 + 1) as u8;
            last = replace_with(&last, &vec![byte; 256 * 1024]).expect("置き換え");
        }
    }

    #[test]
    fn placing_a_file_without_approval_never_replaces() {
        let dir = scratch_dir();
        let target = dir.join("out.mp4");
        let first = dir.join("first.tmp");
        fs::write(&first, b"first").expect("一時 file");
        commit_file(&first, &target, CommitPolicy::CreateNew).expect("新しい file");
        assert_eq!(fs::read(&target).expect("読み戻し"), b"first");
        let second = dir.join("second.tmp");
        fs::write(&second, b"second").expect("一時 file");
        commit_file(&second, &target, CommitPolicy::CreateNew).expect_err("承認の無い置換");
        assert_eq!(fs::read(&target).expect("読み戻し"), b"first");
        assert!(second.is_file(), "失敗した一時 file は呼び出し側が片付ける");

        let approval = ReplaceApproval::capture(&target)
            .expect("確かめる")
            .expect("ある");
        commit_file(
            &second,
            &dir.join("other.mp4"),
            CommitPolicy::ReplaceApproved(&approval),
        )
        .expect_err("別の置き先");
        let placed = commit_file(&second, &target, CommitPolicy::ReplaceApproved(&approval))
            .expect("承認した置換");
        assert!(placed.is_current(), "置いた file の実体を返していません");
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
