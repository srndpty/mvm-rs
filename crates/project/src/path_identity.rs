//! path の同一性の規則。実体の識別子（volume serial + file ID）を OS から取るのは platform crate の役目で、
//! ここには取った結果の比べ方だけを置く。

use std::path::{Component, Path, PathBuf};

/// I/O をしない表記上の key。絶対 path にしてから `.` / `..` を畳み、区切りを `/` に揃える。
/// ファイルが無くても決まり、同じ入力には常に同じ値を返す。絶対 path にできなければ `None`。
///
/// 大文字小文字は畳まない。NTFS は directory ごとに case sensitivity を有効にでき、そこでは A.mp4 と a.mp4 が
/// 別のファイルになる。畳むと別物を同じと誤判定する。
pub fn canonical_path_key(path: &Path) -> Option<String> {
    if path.as_os_str().is_empty() {
        return None;
    }
    let absolute = std::path::absolute(path).ok()?;
    let mut normalized = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            other => normalized.push(other),
        }
    }
    Some(normalized.to_string_lossy().replace('\\', "/"))
}

/// 実体の識別子を取れたかどうか。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileIdentityStatus {
    /// 存在する通常ファイル。volume serial + file ID を持つ（大文字小文字・junction・8.3 名・hard link を吸収する）。
    FileId,
    /// path は何も指していない。
    Missing,
    /// 存在するかもしれないが識別子を取れない（access denied・ネットワーク・ディレクトリ・特殊 FS など）。
    Unavailable,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileIdentityKey {
    pub status: FileIdentityStatus,
    /// [`canonical_path_key`]。
    pub path_key: Option<String>,
    /// FileId のときだけ。
    pub file_key: Option<String>,
}

/// 同じ実体か。Unknown は「同じとも違うとも言えない」であり、呼び出し側が安全な側へ倒す。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PathSameness {
    Same,
    Different,
    Unknown,
}

/// - 両方 FileId → file ID で決める（表記より優先する）
/// - 表記が同じ → Same
/// - どちらかが Unavailable → Unknown
/// - それ以外（少なくとも片方は何も指していない）→ Different
pub fn compare_path_identity(left: &FileIdentityKey, right: &FileIdentityKey) -> PathSameness {
    let (Some(left_path), Some(right_path)) = (&left.path_key, &right.path_key) else {
        return PathSameness::Unknown;
    };
    if left.status == FileIdentityStatus::FileId && right.status == FileIdentityStatus::FileId {
        return if left.file_key == right.file_key {
            PathSameness::Same
        } else {
            PathSameness::Different
        };
    }
    if left_path == right_path {
        return PathSameness::Same;
    }
    if left.status == FileIdentityStatus::Unavailable
        || right.status == FileIdentityStatus::Unavailable
    {
        return PathSameness::Unknown;
    }
    PathSameness::Different
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(status: FileIdentityStatus, path: &str, file: Option<&str>) -> FileIdentityKey {
        FileIdentityKey {
            status,
            path_key: Some(path.to_owned()),
            file_key: file.map(str::to_owned),
        }
    }

    #[test]
    fn canonical_key_is_lexical_and_keeps_case() {
        let key = canonical_path_key(Path::new(r"C:\media\.\clips\..\A.mp4")).unwrap();
        assert_eq!(key, "C:/media/A.mp4");
        assert_ne!(
            key,
            canonical_path_key(Path::new(r"C:\media\a.mp4")).unwrap()
        );
        assert_eq!(canonical_path_key(Path::new("")), None);
    }

    #[test]
    fn compares_by_file_id_before_spelling() {
        use FileIdentityStatus::{FileId, Missing, Unavailable};
        assert_eq!(
            compare_path_identity(
                &key(FileId, "a", Some("1:01")),
                &key(FileId, "b", Some("1:01"))
            ),
            PathSameness::Same
        );
        assert_eq!(
            compare_path_identity(
                &key(FileId, "a", Some("1:01")),
                &key(FileId, "a", Some("1:02"))
            ),
            PathSameness::Different,
            "表記が同じでも実体が違えば違う"
        );
        assert_eq!(
            compare_path_identity(&key(Missing, "a", None), &key(Missing, "a", None)),
            PathSameness::Same
        );
        assert_eq!(
            compare_path_identity(&key(Missing, "a", None), &key(FileId, "b", Some("1:01"))),
            PathSameness::Different
        );
        assert_eq!(
            compare_path_identity(
                &key(Unavailable, "a", None),
                &key(FileId, "b", Some("1:01"))
            ),
            PathSameness::Unknown
        );
    }
}
