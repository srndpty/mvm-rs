//! 外部 process の command line の組み立てと、PATH からの実行 file の検索。OS の API は呼ばない。

use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};

fn needs_quotes(argument: &[u16]) -> bool {
    argument.is_empty()
        || argument
            .iter()
            .any(|unit| matches!(*unit, 0x20 | 0x09 | 0x0a | 0x0b | 0x22))
}

/// `CommandLineToArgvW` が元の引数へ戻せる形に 1 つの引数を引用する。
/// 引用符の直前と末尾の `\` の並びは倍にし、引用符は `\"` にする。
pub fn quote_argument(argument: &OsStr) -> Vec<u16> {
    let units: Vec<u16> = argument.encode_wide().collect();
    if !needs_quotes(&units) {
        return units;
    }
    let backslash = u16::from(b'\\');
    let quote = u16::from(b'"');
    let mut out = vec![quote];
    let mut index = 0;
    loop {
        let mut backslashes = 0;
        while index < units.len() && units[index] == backslash {
            index += 1;
            backslashes += 1;
        }
        if index == units.len() {
            out.extend(std::iter::repeat_n(backslash, backslashes * 2));
            break;
        }
        let emitted = if units[index] == quote {
            backslashes * 2 + 1
        } else {
            backslashes
        };
        out.extend(std::iter::repeat_n(backslash, emitted));
        out.push(units[index]);
        index += 1;
    }
    out.push(quote);
    out
}

/// executable と引数を空白でつないだ、NUL 終端の command line。
pub(crate) fn build_command_line(executable: &Path, arguments: &[&OsStr]) -> Vec<u16> {
    let mut line = quote_argument(executable.as_os_str());
    for argument in arguments {
        line.push(u16::from(b' '));
        line.extend(quote_argument(argument));
    }
    line.push(0);
    line
}

/// 環境変数 PATH の各 directory だけから `name` を探す（子 process へ引き継ぐのと同じ PATH）。
pub fn find_on_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    for entry in std::env::split_paths(&path) {
        if entry.as_os_str().is_empty() {
            continue;
        }
        let candidate = entry.join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use std::ffi::OsString;

    use super::*;

    fn quoted(text: &str) -> String {
        String::from_utf16(&quote_argument(&OsString::from(text))).unwrap()
    }

    #[test]
    fn quotes_like_the_inverse_of_command_line_to_argv() {
        // 期待値は CommandLineToArgvW の規則（MSDN）から手で書いたもの。
        assert_eq!(quoted("plain"), "plain");
        assert_eq!(quoted(""), "\"\"");
        assert_eq!(quoted("a b"), "\"a b\"");
        assert_eq!(quoted("say \"hi\""), "\"say \\\"hi\\\"\"");
        assert_eq!(quoted("C:\\dir with space\\"), "\"C:\\dir with space\\\\\"");
        assert_eq!(quoted("a\\\\\"b"), "\"a\\\\\\\\\\\"b\"");
        assert_eq!(quoted("C:\\no\\space"), "C:\\no\\space");
        assert_eq!(quoted("日本語 の"), "\"日本語 の\"");
    }

    #[test]
    fn command_line_is_nul_terminated_and_space_separated() {
        let line = build_command_line(
            Path::new("C:\\Program Files\\x.exe"),
            &[OsStr::new("-a"), OsStr::new("two words")],
        );
        assert_eq!(line.last(), Some(&0));
        let text = String::from_utf16(&line[..line.len() - 1]).unwrap();
        assert_eq!(text, "\"C:\\Program Files\\x.exe\" -a \"two words\"");
    }

    #[test]
    fn finds_cmd_on_path_and_misses_an_absent_name() {
        assert!(
            find_on_path("cmd.exe").is_some(),
            "cmd.exe が PATH にありません"
        );
        assert!(find_on_path("mvm-surely-absent-tool.exe").is_none());
    }
}
