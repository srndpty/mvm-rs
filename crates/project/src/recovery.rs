//! crash recovery の envelope。canonical そのものではなく、autosave 時点の照合情報を持つ。
//!
//! 旧形式（envelope の無い Project JSON）は読まない。path が同じ Project かは、OS から取った
//! [`FileIdentityKey`] を呼び出し側が渡す。

use std::path::Path;

use serde_json::Value;

use crate::error::{ProjectResult, fail};
use crate::json::{parse_project_json, reject_duplicate_keys, serialize_project_json};
use crate::model::Project;
use crate::path_identity::{FileIdentityKey, PathSameness, compare_path_identity};

const VERSION: i64 = 1;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecoveryHeader {
    pub canonical_sha256: String,
    pub saved_at: String,
    pub session_id: String,
    pub canonical_path: String,
    pub project_json: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecoveryDisposition {
    /// recovery の中身が、今の canonical と同じ。
    Stale,
    /// 記録した hash と、今の canonical の hash が一致する。中身は canonical と違う。
    Restorable,
    /// canonical が、recovery を書いたときから変わっている。
    CanonicalChanged,
}

pub fn write_recovery_envelope(
    project: &Project,
    canonical_path: &Path,
    canonical_sha256: &str,
    saved_at: &str,
    session_id: &str,
) -> ProjectResult<String> {
    if saved_at.is_empty() || session_id.is_empty() || !sha256_ok(canonical_sha256) {
        return fail("自動復旧データの識別情報が不正です");
    }
    let project_json = serialize_project_json(project, canonical_path)?;
    let hash = json_string(canonical_sha256)?;
    let saved = json_string(saved_at)?;
    let session = json_string(session_id)?;
    let path = json_string(&canonical_path.to_string_lossy())?;
    Ok(format!(
        "{{\n  \"recovery_format_version\": {VERSION},\n  \"canonical_sha256\": {hash},\n  \"saved_at\": {saved},\n  \"session_id\": {session},\n  \"canonical_path\": {path},\n  \"project\": {project_json}}}\n"
    ))
}

pub fn parse_recovery_header(text: &str) -> ProjectResult<RecoveryHeader> {
    reject_duplicate_keys(text).map_err(|error| {
        crate::ProjectError::new(format!("自動復旧データを解釈できません: {error}"))
    })?;
    let value: Value = serde_json::from_str(text).map_err(|error| {
        crate::ProjectError::new(format!("自動復旧データを解釈できません: {error}"))
    })?;
    let Some(object) = value.as_object() else {
        return fail("自動復旧データが object ではありません");
    };
    const KEYS: [&str; 6] = [
        "recovery_format_version",
        "canonical_sha256",
        "saved_at",
        "session_id",
        "canonical_path",
        "project",
    ];
    if object.len() != KEYS.len() || KEYS.iter().any(|key| !object.contains_key(*key)) {
        return fail("自動復旧データの field が過不足しています");
    }
    let version = object
        .get("recovery_format_version")
        .and_then(Value::as_i64)
        .filter(|version| *version == VERSION)
        .ok_or_else(|| crate::ProjectError::new("recovery_format_version が不正です"))?;
    let _ = version;
    let canonical_sha256 = required_string(object, "canonical_sha256")?;
    if !sha256_ok(&canonical_sha256) {
        return fail("canonical_sha256 が SHA-256 ではありません");
    }
    let saved_at = required_string(object, "saved_at")?;
    let session_id = required_string(object, "session_id")?;
    let canonical_path = required_string(object, "canonical_path")?;
    if saved_at.is_empty() || session_id.is_empty() || canonical_path.is_empty() {
        return fail("自動復旧データの識別情報が空です");
    }
    let project = object
        .get("project")
        .filter(|value| value.is_object())
        .ok_or_else(|| {
            crate::ProjectError::new("自動復旧データの Project 本体が object ではありません")
        })?;
    Ok(RecoveryHeader {
        canonical_sha256,
        saved_at,
        session_id,
        canonical_path,
        project_json: project.to_string(),
    })
}

pub fn recovery_project(header: &RecoveryHeader, project_path: &Path) -> ProjectResult<Project> {
    parse_project_json(&header.project_json, project_path)
}

/// 記録された path と、今開いている path が同じ実体か。Unknown は別 Project として扱う。
pub fn recovery_belongs_to(recorded: &FileIdentityKey, opened: &FileIdentityKey) -> bool {
    compare_path_identity(recorded, opened) == PathSameness::Same
}

pub fn classify_recovery(
    recovery: &Project,
    canonical: &Project,
    recorded_hash: &str,
    current_hash: &str,
) -> RecoveryDisposition {
    if recovery == canonical {
        RecoveryDisposition::Stale
    } else if recorded_hash == current_hash {
        RecoveryDisposition::Restorable
    } else {
        RecoveryDisposition::CanonicalChanged
    }
}

fn sha256_ok(value: &str) -> bool {
    value.is_empty()
        || (value.len() == 64
            && value
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase()))
}

fn required_string(object: &serde_json::Map<String, Value>, key: &str) -> ProjectResult<String> {
    object
        .get(key)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| crate::ProjectError::new(format!("{key} が文字列ではありません")))
}

fn json_string(value: &str) -> ProjectResult<String> {
    serde_json::to_string(value).map_err(|error| {
        crate::ProjectError::new(format!("自動復旧データの文字列を書けません: {error}"))
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use std::path::Path;

    use super::*;
    use crate::path_identity::{FileIdentityKey, FileIdentityStatus};

    fn key(path: &str, file: Option<&str>) -> FileIdentityKey {
        FileIdentityKey {
            status: if file.is_some() {
                FileIdentityStatus::FileId
            } else {
                FileIdentityStatus::Missing
            },
            path_key: Some(path.to_owned()),
            file_key: file.map(str::to_owned),
        }
    }

    #[test]
    fn envelope_round_trips_the_project_and_the_hash() {
        let project = Project::default();
        let path = Path::new(r"C:\edit\score.mvm");
        let text =
            write_recovery_envelope(&project, path, "", "2026-10-19T00:00:00.000Z", "session-1")
                .expect("書く");
        let header = parse_recovery_header(&text).expect("読む");
        assert_eq!(header.canonical_sha256, "");
        assert_eq!(header.session_id, "session-1");
        assert!(header.canonical_path.contains("score.mvm"));
        let loaded = recovery_project(&header, path).expect("Project");
        assert_eq!(loaded.video_tracks.len(), project.video_tracks.len());
        assert_eq!(loaded.output_width, 1920);
        assert_eq!(
            classify_recovery(&loaded, &project, "", ""),
            RecoveryDisposition::Stale
        );
    }

    #[test]
    fn unknown_field_duplicate_key_and_bad_hash_are_rejected() {
        let project = Project::default();
        let path = Path::new("score.mvm");
        let text = write_recovery_envelope(&project, path, "ab", "t", "s").expect_err("短い hash");
        assert!(text.to_string().contains("識別"), "{text}");

        let ok = write_recovery_envelope(&project, path, "", "t", "s").expect("書く");
        let extra = ok.replacen("{", r#"{"extra": 1, "#, 1);
        let error = parse_recovery_header(&extra).expect_err("未知の field");
        assert!(
            error.to_string().contains("field")
                || error.to_string().contains("重複")
                || error.to_string().contains("解釈"),
            "{error}"
        );

        let duplicated = ok.replacen("\"session_id\"", "\"session_id\": \"s\", \"session_id\"", 1);
        assert!(parse_recovery_header(&duplicated).is_err());
        assert!(parse_recovery_header("[]").is_err());
        assert!(parse_recovery_header(&ok.replacen("1", "2", 1)).is_err());
    }

    #[test]
    fn classify_prefers_equal_projects_over_a_different_hash() {
        let project = Project::default();
        let mut edited = project.clone();
        edited.output_width = 1280;
        assert_eq!(
            classify_recovery(&project, &project, "aaa", "bbb"),
            RecoveryDisposition::Stale
        );
        assert_eq!(
            classify_recovery(&edited, &project, "same", "same"),
            RecoveryDisposition::Restorable
        );
        assert_eq!(
            classify_recovery(&edited, &project, "old", "new"),
            RecoveryDisposition::CanonicalChanged
        );
        assert_eq!(
            classify_recovery(&edited, &project, "", ""),
            RecoveryDisposition::Restorable
        );
    }

    #[test]
    fn a_different_file_id_is_not_the_same_project() {
        assert!(recovery_belongs_to(
            &key("C:/edit/score.mvm", Some("1")),
            &key("C:/edit/score.mvm", Some("1"))
        ));
        assert!(!recovery_belongs_to(
            &key("C:/edit/score.mvm", Some("1")),
            &key("D:/other/score.mvm", Some("2"))
        ));
        assert!(!recovery_belongs_to(
            &key("C:/edit/score.mvm", None),
            &FileIdentityKey {
                status: FileIdentityStatus::Unavailable,
                path_key: Some("C:/edit/other.mvm".to_owned()),
                file_key: None,
            }
        ));
    }
}
