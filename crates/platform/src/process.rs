//! 外部 process を 1 つ起動し、終了・timeout・取消のいずれかまで待つ。
//!
//! 起動した process は Job Object に入れる。process が起動した子（Manim が起動する latex など）も同じ
//! job に入るので、timeout・取消のときも、正常終了の後に残った子も、job を閉じた時点でまとめて終わらせる
//! （JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE）。子へ継承させる handle は、この呼び出しの標準入出力だけに限る。

use std::ffi::{OsStr, c_void};
use std::path::Path;
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0, WAIT_TIMEOUT};
use windows::Win32::Security::SECURITY_ATTRIBUTES;
use windows::Win32::Storage::FileSystem::{
    CREATE_ALWAYS, CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_GENERIC_READ, FILE_GENERIC_WRITE,
    FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
};
use windows::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
    SetInformationJobObject, TerminateJobObject,
};
use windows::Win32::System::Threading::{
    CREATE_NO_WINDOW, CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT, CreateProcessW,
    DeleteProcThreadAttributeList, EXTENDED_STARTUPINFO_PRESENT, GetExitCodeProcess,
    InitializeProcThreadAttributeList, LPPROC_THREAD_ATTRIBUTE_LIST,
    PROC_THREAD_ATTRIBUTE_HANDLE_LIST, PROCESS_INFORMATION, ResumeThread, STARTF_USESHOWWINDOW,
    STARTF_USESTDHANDLES, STARTUPINFOEXW, TerminateProcess, UpdateProcThreadAttribute,
    WaitForSingleObject,
};
use windows::core::{PCWSTR, PWSTR};

use crate::command_line::build_command_line;
use crate::error::PlatformError;
use crate::wide::wide_path;

/// 待機の 1 回の長さ。取消と timeout はこの間隔で見る。
const POLL_MS: u32 = 50;

pub struct ProcessRequest<'a> {
    /// 実行する file の path。PATH から探さない。
    pub executable: &'a Path,
    pub arguments: &'a [&'a OsStr],
    pub working_directory: Option<&'a Path>,
    /// 標準出力・標準エラーを書く file（作り直す）。`None` なら捨てる。標準入力は常に NUL。
    pub stdout_path: Option<&'a Path>,
    pub stderr_path: Option<&'a Path>,
    pub timeout: Option<Duration>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProcessOutcome {
    /// 終わった。0 以外の終了コードもここ。
    Exited(i32),
    TimedOut,
    Cancelled,
}

/// 閉じ忘れない HANDLE。
struct Owned(HANDLE);

impl Drop for Owned {
    fn drop(&mut self) {
        if !self.0.is_invalid() && !self.0.0.is_null() {
            // SAFETY: この struct が作った handle を 1 回だけ閉じる。
            unsafe {
                let _ = CloseHandle(self.0);
            }
        }
    }
}

struct AttributeList {
    buffer: Vec<u8>,
}

impl AttributeList {
    fn list(&mut self) -> LPPROC_THREAD_ATTRIBUTE_LIST {
        LPPROC_THREAD_ATTRIBUTE_LIST(self.buffer.as_mut_ptr().cast())
    }
}

impl Drop for AttributeList {
    fn drop(&mut self) {
        // SAFETY: buffer は InitializeProcThreadAttributeList が初期化した list で、ここで 1 回だけ片付ける。
        unsafe { DeleteProcThreadAttributeList(self.list()) };
    }
}

fn win32(context: &str, error: windows::core::Error) -> PlatformError {
    PlatformError::new(format!("{context}: {error}"))
}

fn open_output(
    path: Option<&Path>,
    security: &SECURITY_ATTRIBUTES,
) -> Result<Owned, PlatformError> {
    let wide = match path {
        Some(path) => wide_path(path)?,
        None => "NUL".encode_utf16().chain(Some(0)).collect(),
    };
    // SAFETY: wide は NUL 終端。security はこの呼び出しの間だけ生きていればよい。
    let handle = unsafe {
        CreateFileW(
            PCWSTR(wide.as_ptr()),
            FILE_GENERIC_WRITE.0,
            FILE_SHARE_READ | FILE_SHARE_DELETE,
            Some(security),
            CREATE_ALWAYS,
            FILE_ATTRIBUTE_NORMAL,
            None,
        )
    }
    .map_err(|error| win32("出力 file を作れません", error))?;
    Ok(Owned(handle))
}

/// `request` を実行する。`poll` は約 50 ms ごとに呼び、true を返したら止める（取消）。
/// 戻った時点で job の process は全て終わっている。
pub fn run_process(
    request: &ProcessRequest<'_>,
    poll: &mut dyn FnMut() -> bool,
) -> Result<ProcessOutcome, PlatformError> {
    if request.executable.as_os_str().is_empty() {
        return Err(PlatformError::new("実行する file が指定されていません"));
    }
    let executable = wide_path(request.executable)?;
    let directory = request.working_directory.map(wide_path).transpose()?;
    let mut command_line = build_command_line(request.executable, request.arguments);

    // SAFETY: 名前の無い job を作る。戻り値は Owned が閉じる。
    let job = Owned(
        unsafe { CreateJobObjectW(None, PCWSTR::null()) }
            .map_err(|error| win32("Job Object を作れません", error))?,
    );
    let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
    limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
    // SAFETY: limits はこの呼び出しの間生きており、大きさは構造体の大きさと一致する。
    unsafe {
        SetInformationJobObject(
            job.0,
            JobObjectExtendedLimitInformation,
            std::ptr::from_ref(&limits).cast::<c_void>(),
            std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        )
    }
    .map_err(|error| win32("Job Object を設定できません", error))?;

    let security = SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: std::ptr::null_mut(),
        bInheritHandle: true.into(),
    };
    let nul: Vec<u16> = "NUL".encode_utf16().chain(Some(0)).collect();
    // SAFETY: nul は NUL 終端。security はこの呼び出しの間生きている。
    let stdin = Owned(
        unsafe {
            CreateFileW(
                PCWSTR(nul.as_ptr()),
                FILE_GENERIC_READ.0,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                Some(&security),
                OPEN_EXISTING,
                FILE_ATTRIBUTE_NORMAL,
                None,
            )
        }
        .map_err(|error| win32("標準入力を開けません", error))?,
    );
    let stdout = open_output(request.stdout_path, &security)?;
    let stderr = open_output(request.stderr_path, &security)?;

    // 継承させる handle をこの 3 つに限る。
    let mut size = 0usize;
    // SAFETY: 大きさの問い合わせ。list は渡さない（失敗は想定どおりなので無視する）。
    let _ = unsafe { InitializeProcThreadAttributeList(None, 1, None, &mut size) };
    if size == 0 {
        return Err(PlatformError::new("属性 list の大きさを得られません"));
    }
    let mut buffer = vec![0u8; size];
    let raw = LPPROC_THREAD_ATTRIBUTE_LIST(buffer.as_mut_ptr().cast());
    // SAFETY: buffer は問い合わせた大きさを持つ。成功したら AttributeList が片付ける。
    unsafe { InitializeProcThreadAttributeList(Some(raw), 1, None, &mut size) }
        .map_err(|error| win32("属性 list を作れません", error))?;
    let mut attributes = AttributeList { buffer };
    let inherited = [stdin.0, stdout.0, stderr.0];
    // SAFETY: inherited は CreateProcessW が戻るまで生きている。大きさは配列の byte 数。
    unsafe {
        UpdateProcThreadAttribute(
            attributes.list(),
            0,
            PROC_THREAD_ATTRIBUTE_HANDLE_LIST as usize,
            Some(inherited.as_ptr().cast()),
            std::mem::size_of_val(&inherited),
            None,
            None,
        )
    }
    .map_err(|error| win32("継承する handle を指定できません", error))?;

    let mut startup = STARTUPINFOEXW::default();
    startup.StartupInfo.cb = std::mem::size_of::<STARTUPINFOEXW>() as u32;
    startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES | STARTF_USESHOWWINDOW;
    // SW_HIDE。
    startup.StartupInfo.wShowWindow = 0;
    startup.StartupInfo.hStdInput = stdin.0;
    startup.StartupInfo.hStdOutput = stdout.0;
    startup.StartupInfo.hStdError = stderr.0;
    startup.lpAttributeList = attributes.list();
    let mut information = PROCESS_INFORMATION::default();
    // job へ入れる前に子を起動させないよう、停止状態で作ってから job へ入れて再開する。
    // SAFETY: 文字列は全て NUL 終端で、command_line は書き換え可能な buffer。startup と information は
    // この呼び出しの間生きている。
    unsafe {
        CreateProcessW(
            PCWSTR(executable.as_ptr()),
            Some(PWSTR(command_line.as_mut_ptr())),
            None,
            None,
            true,
            CREATE_NO_WINDOW
                | CREATE_SUSPENDED
                | CREATE_UNICODE_ENVIRONMENT
                | EXTENDED_STARTUPINFO_PRESENT,
            None,
            directory
                .as_ref()
                .map_or(PCWSTR::null(), |directory| PCWSTR(directory.as_ptr())),
            &startup.StartupInfo,
            &mut information,
        )
    }
    .map_err(|error| {
        win32(
            &format!("{} を起動できません", request.executable.display()),
            error,
        )
    })?;
    let process = Owned(information.hProcess);
    let thread = Owned(information.hThread);
    // SAFETY: process は CreateProcessW が返した有効な handle。
    if let Err(error) = unsafe { AssignProcessToJobObject(job.0, process.0) } {
        // SAFETY: 停止状態の子を止めて待つ。
        unsafe {
            let _ = TerminateProcess(process.0, 1);
            WaitForSingleObject(process.0, u32::MAX);
        }
        return Err(win32("process を Job Object へ入れられません", error));
    }
    // SAFETY: thread は CreateProcessW が返した停止中の主 thread。
    if unsafe { ResumeThread(thread.0) } == u32::MAX {
        // SAFETY: job の process を止めて待つ。
        unsafe {
            let _ = TerminateJobObject(job.0, 1);
            WaitForSingleObject(process.0, u32::MAX);
        }
        return Err(PlatformError::new("process を再開できません"));
    }
    // 子は自分の handle を持ったので、こちらの複製は閉じる。
    drop(stdin);
    drop(stdout);
    drop(stderr);
    drop(attributes);

    let started = Instant::now();
    let outcome = loop {
        // SAFETY: process は有効な handle。
        let waited = unsafe { WaitForSingleObject(process.0, POLL_MS) };
        if waited == WAIT_OBJECT_0 {
            let mut code = 0u32;
            // SAFETY: process は終わった有効な handle。code は書き込める。
            unsafe { GetExitCodeProcess(process.0, &mut code) }
                .map_err(|error| win32("終了コードを得られません", error))?;
            break ProcessOutcome::Exited(code as i32);
        }
        if waited != WAIT_TIMEOUT {
            // SAFETY: job の process を止めて待つ。
            unsafe {
                let _ = TerminateJobObject(job.0, 1);
                WaitForSingleObject(process.0, u32::MAX);
            }
            return Err(PlatformError::new("process の終了を待てません"));
        }
        if poll() {
            break ProcessOutcome::Cancelled;
        }
        if request
            .timeout
            .is_some_and(|timeout| started.elapsed() >= timeout)
        {
            break ProcessOutcome::TimedOut;
        }
    };
    if !matches!(outcome, ProcessOutcome::Exited(_)) {
        // SAFETY: job の process を止めて待つ。
        unsafe {
            let _ = TerminateJobObject(job.0, 1);
            WaitForSingleObject(process.0, u32::MAX);
        }
    }
    // job を閉じると、残った孫 process も終わる。
    drop(job);
    Ok(outcome)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use std::time::Instant;

    use super::*;
    use crate::command_line::find_on_path;

    fn cmd() -> std::path::PathBuf {
        find_on_path("cmd.exe").expect("cmd.exe")
    }

    fn scratch(name: &str) -> std::path::PathBuf {
        let directory =
            std::env::temp_dir().join(format!("mvm-process-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        directory
    }

    #[test]
    fn captures_output_and_exit_code() {
        let directory = scratch("exit");
        let out = directory.join("out.txt");
        let arguments = [OsStr::new("/c"), OsStr::new("echo hello& exit 3")];
        let outcome = run_process(
            &ProcessRequest {
                executable: &cmd(),
                arguments: &arguments,
                working_directory: Some(&directory),
                stdout_path: Some(&out),
                stderr_path: None,
                timeout: Some(Duration::from_secs(20)),
            },
            &mut || false,
        )
        .unwrap();
        assert_eq!(outcome, ProcessOutcome::Exited(3));
        assert_eq!(std::fs::read_to_string(&out).unwrap().trim(), "hello");
        std::fs::remove_dir_all(&directory).unwrap();
    }

    #[test]
    fn timeout_and_cancel_stop_the_process_tree() {
        let directory = scratch("stop");
        // ping で数十秒待つ。子 process（ping）ごと止まることを、所要時間で確かめる。
        let arguments = [OsStr::new("/c"), OsStr::new("ping -n 30 127.0.0.1 > NUL")];
        let request = ProcessRequest {
            executable: &cmd(),
            arguments: &arguments,
            working_directory: Some(&directory),
            stdout_path: None,
            stderr_path: None,
            timeout: Some(Duration::from_millis(300)),
        };
        let started = Instant::now();
        assert_eq!(
            run_process(&request, &mut || false).unwrap(),
            ProcessOutcome::TimedOut
        );
        assert!(started.elapsed() < Duration::from_secs(10));

        let request = ProcessRequest {
            timeout: None,
            ..request
        };
        let mut calls = 0;
        let started = Instant::now();
        let outcome = run_process(&request, &mut || {
            calls += 1;
            calls > 4
        })
        .unwrap();
        assert_eq!(outcome, ProcessOutcome::Cancelled);
        assert!(started.elapsed() < Duration::from_secs(10));
        std::fs::remove_dir_all(&directory).unwrap();
    }

    #[test]
    fn a_missing_executable_fails_to_start() {
        let error = run_process(
            &ProcessRequest {
                executable: Path::new("C:\\mvm-surely-missing\\tool.exe"),
                arguments: &[],
                working_directory: None,
                stdout_path: None,
                stderr_path: None,
                timeout: None,
            },
            &mut || false,
        )
        .unwrap_err();
        assert!(error.message().contains("起動できません"), "{error}");
    }
}
