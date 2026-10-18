//! path の実体を、project が比べられる key にする。

use std::path::Path;

use mvm_platform::{FileProbeStatus, probe_file};
use mvm_project::path_identity::{FileIdentityKey, FileIdentityStatus, canonical_path_key};

pub fn identity_key(path: &Path) -> FileIdentityKey {
    let probe = probe_file(path);
    let status = match probe.status {
        FileProbeStatus::File => FileIdentityStatus::FileId,
        FileProbeStatus::Missing => FileIdentityStatus::Missing,
        FileProbeStatus::Unavailable => FileIdentityStatus::Unavailable,
    };
    let file_key = (status == FileIdentityStatus::FileId).then(|| {
        let mut key = format!("{}:", probe.volume_serial);
        for byte in probe.file_id {
            key.push(char::from(b"0123456789abcdef"[(byte >> 4) as usize]));
            key.push(char::from(b"0123456789abcdef"[(byte & 0x0f) as usize]));
        }
        key
    });
    FileIdentityKey {
        status,
        path_key: canonical_path_key(path),
        file_key,
    }
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(bytes);
    let mut text = String::with_capacity(64);
    for byte in digest {
        text.push(char::from(b"0123456789abcdef"[(byte >> 4) as usize]));
        text.push(char::from(b"0123456789abcdef"[(byte & 0x0f) as usize]));
    }
    text
}

pub fn utc_timestamp(now: std::time::SystemTime) -> String {
    let millis = now
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0);
    let seconds = i64::try_from(millis / 1000).unwrap_or(0);
    let ms = u32::try_from(millis % 1000).unwrap_or(0);
    let days = seconds.div_euclid(86_400);
    let tod = u32::try_from(seconds.rem_euclid(86_400)).unwrap_or(0);
    let hour = tod / 3_600;
    let minute = (tod % 3_600) / 60;
    let second = tod % 60;
    let (year, month, day) = civil_from_days(days);
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.{ms:03}Z")
}

/// Howard Hinnant の civil_from_days。Unix epoch からの日数を日付にする。
fn civil_from_days(days: i64) -> (i32, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if month <= 2 { y + 1 } else { y };
    (
        i32::try_from(year).unwrap_or(1970),
        u32::try_from(month).unwrap_or(1),
        u32::try_from(day).unwrap_or(1),
    )
}

pub fn recovery_path(project_path: &Path) -> std::path::PathBuf {
    let mut name = project_path.as_os_str().to_owned();
    name.push(".recovery");
    std::path::PathBuf::from(name)
}

pub fn lock_path(project_path: &Path) -> std::path::PathBuf {
    let mut name = project_path.as_os_str().to_owned();
    name.push(".lock");
    std::path::PathBuf::from(name)
}
