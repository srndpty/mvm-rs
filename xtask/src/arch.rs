//! crate 間の依存方向と、unsafe を許可する crate の検査。
//!
//! 層の規則はこのファイルの `RULES` だけに書く。workspace に crate を足したら、
//! ここへ規則を足さない限り検査が失敗する（規則の無い crate は通さない）。

use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

/// workspace 内の crate ごとの規則。
struct Rule {
    name: &'static str,
    /// 依存してよい workspace 内の crate。
    allowed_deps: &'static [&'static str],
    /// `unsafe` を許可する（workspace の lint を使わず、自前の `[lints]` を持つ）。
    allows_unsafe: bool,
}

const RULES: &[Rule] = &[
    Rule {
        name: "mvm-core",
        allowed_deps: &[],
        allows_unsafe: false,
    },
    Rule {
        name: "mvm-project",
        allowed_deps: &["mvm-core"],
        allows_unsafe: false,
    },
    Rule {
        name: "mvm",
        allowed_deps: &["mvm-core"],
        allows_unsafe: false,
    },
    Rule {
        name: "xtask",
        allowed_deps: &[],
        allows_unsafe: false,
    },
];

/// 検査に使う crate の情報。
struct Package {
    name: String,
    deps: Vec<String>,
    manifest_path: PathBuf,
}

pub fn run() -> Result<(), Vec<String>> {
    let packages = load_packages().map_err(|error| vec![error])?;
    let mut errors = Vec::new();
    for package in &packages {
        let manifest = std::fs::read_to_string(&package.manifest_path).map_err(|error| {
            vec![format!(
                "{} を読めません: {error}",
                package.manifest_path.display()
            )]
        })?;
        errors.extend(check_package(
            package,
            &workspace_names(&packages),
            &manifest,
        ));
    }
    if packages.is_empty() {
        errors.push("workspace に crate がありません".to_owned());
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

fn workspace_names(packages: &[Package]) -> Vec<&str> {
    packages
        .iter()
        .map(|package| package.name.as_str())
        .collect()
}

fn check_package(package: &Package, workspace: &[&str], manifest: &str) -> Vec<String> {
    let Some(rule) = RULES.iter().find(|rule| rule.name == package.name) else {
        return vec![format!(
            "{} の規則が xtask/src/arch.rs の RULES にありません",
            package.name
        )];
    };
    let mut errors = Vec::new();
    for dep in &package.deps {
        if workspace.contains(&dep.as_str()) && !rule.allowed_deps.contains(&dep.as_str()) {
            errors.push(format!(
                "{} は {} に依存してはいけません",
                package.name, dep
            ));
        }
    }
    let uses_workspace_lints = uses_workspace_lints(manifest);
    if !rule.allows_unsafe && !uses_workspace_lints {
        errors.push(format!(
            "{} は [lints] workspace = true を使う必要があります（unsafe を許可しない crate）",
            package.name
        ));
    }
    if rule.allows_unsafe && uses_workspace_lints {
        errors.push(format!(
            "{} は unsafe を許可する crate なので、自前の [lints] で unsafe_code = \"deny\" を設定します",
            package.name
        ));
    }
    errors
}

/// manifest の `[lints]` 節に `workspace = true` があるか。
fn uses_workspace_lints(manifest: &str) -> bool {
    let mut in_lints = false;
    for line in manifest.lines().map(str::trim) {
        if line.starts_with('[') {
            in_lints = line == "[lints]";
        } else if in_lints && line.replace(' ', "") == "workspace=true" {
            return true;
        }
    }
    false
}

fn load_packages() -> Result<Vec<Package>, String> {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_owned());
    let output = Command::new(cargo)
        .args(["metadata", "--format-version", "1", "--no-deps"])
        .output()
        .map_err(|error| format!("cargo metadata を起動できません: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "cargo metadata が失敗しました: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    let metadata: Value = serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("cargo metadata の出力を解析できません: {error}"))?;
    parse_packages(&metadata)
}

fn parse_packages(metadata: &Value) -> Result<Vec<Package>, String> {
    let packages = metadata
        .get("packages")
        .and_then(Value::as_array)
        .ok_or("cargo metadata に packages がありません")?;
    packages
        .iter()
        .map(|package| {
            let name = package
                .get("name")
                .and_then(Value::as_str)
                .ok_or("package に name がありません")?;
            let manifest_path = package
                .get("manifest_path")
                .and_then(Value::as_str)
                .ok_or_else(|| format!("{name} に manifest_path がありません"))?;
            let deps = package
                .get("dependencies")
                .and_then(Value::as_array)
                .ok_or_else(|| format!("{name} に dependencies がありません"))?
                .iter()
                .filter_map(|dep| dep.get("name").and_then(Value::as_str))
                .map(str::to_owned)
                .collect();
            Ok(Package {
                name: name.to_owned(),
                deps,
                manifest_path: Path::new(manifest_path).to_path_buf(),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const WORKSPACE_LINTS: &str = "[package]\nname = \"x\"\n\n[lints]\nworkspace = true\n";

    fn package(name: &str, deps: &[&str]) -> Package {
        Package {
            name: name.to_owned(),
            deps: deps.iter().map(|dep| (*dep).to_owned()).collect(),
            manifest_path: PathBuf::new(),
        }
    }

    #[test]
    fn accepts_allowed_dependency() {
        let errors = check_package(
            &package("mvm", &["mvm-core", "eframe"]),
            &["mvm", "mvm-core"],
            WORKSPACE_LINTS,
        );
        assert!(errors.is_empty(), "{errors:?}");
    }

    #[test]
    fn rejects_reverse_dependency() {
        let errors = check_package(
            &package("mvm-core", &["mvm"]),
            &["mvm", "mvm-core"],
            WORKSPACE_LINTS,
        );
        assert_eq!(errors.len(), 1, "{errors:?}");
    }

    #[test]
    fn rejects_crate_without_rule() {
        let errors = check_package(&package("mvm-unknown", &[]), &[], WORKSPACE_LINTS);
        assert_eq!(errors.len(), 1, "{errors:?}");
    }

    #[test]
    fn rejects_missing_workspace_lints() {
        let manifest = "[package]\nname = \"x\"\n\n[lints.rust]\nunsafe_code = \"deny\"\n";
        let errors = check_package(&package("mvm-core", &[]), &["mvm-core"], manifest);
        assert_eq!(errors.len(), 1, "{errors:?}");
    }

    #[test]
    fn workspace_lints_must_be_in_lints_section() {
        assert!(!uses_workspace_lints(
            "[package]\nworkspace = true\n[lints.rust]\n"
        ));
        assert!(uses_workspace_lints("[lints]\nworkspace=true\n"));
    }

    #[test]
    fn every_rule_names_a_distinct_crate() {
        for (index, rule) in RULES.iter().enumerate() {
            assert!(
                RULES[index + 1..]
                    .iter()
                    .all(|other| other.name != rule.name),
                "{} が重複しています",
                rule.name
            );
        }
    }
}
