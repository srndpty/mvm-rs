# mvm (Rust)

Windows 向けノンリニア動画編集ソフト。YouTube 向けの技術・数学解説動画を主対象とする。
C++/Qt 版 (`C:\dev\soft\mvm`) から Rust への全面移行中。

## 現在の状態

**P0 基盤。** Cargo workspace、開発コマンド、lint・試験・CI・pre-commit を用意した段階。
アプリは空の窓を開くだけ。移行計画は [docs/migration-plan.md](docs/migration-plan.md)。

開発の規約は [AGENTS.md](AGENTS.md) に一本化している。

## ツールチェーン

| | |
| --- | --- |
| Rust | `rust-toolchain.toml` で固定（rustup が自動で取得する） |
| target | `x86_64-pc-windows-msvc`（Visual Studio の C++ build tools が必要） |
| GUI | egui / eframe（wgpu） |
| PSScriptAnalyzer | `scripts/PSScriptAnalyzer.version` の版（`Install-Module PSScriptAnalyzer -RequiredVersion <版> -Scope CurrentUser`） |
| pre-commit | `python -m pip install pre-commit` の後に `pre-commit install` |

## 開発用コマンド

リポジトリルートの `dev.ps1` が入口（`dev.cmd` は wrapper）。グローバルのディスパッチャーがあれば `dev <command>` で呼べる。
command の後ろの引数はそのまま cargo へ渡る。

```powershell
.\dev.ps1 build      # debug ビルド（--release で release）
.\dev.ps1 run        # debug の mvm をビルドして起動
.\dev.ps1 gui        # release の mvm をビルドして起動
.\dev.ps1 test       # 試験（workstation 試験は除外。0 件なら失敗）
.\dev.ps1 lint       # rustfmt 差分・clippy・アーキテクチャ検査・PSScriptAnalyzer
.\dev.ps1 fmt        # 整形
.\dev.ps1 check      # 型検査
.\dev.ps1 clean
.\dev.ps1 help
```

## ディレクトリ

| | |
| --- | --- |
| `crates/core` | 純粋なデータと計算（timecode など） |
| `apps/mvm` | 製品アプリ（egui） |
| `xtask` | アーキテクチャ検査（crate の依存方向・unsafe を許可する crate） |
| `scripts` | lint などの正式スクリプト |
| `docs` | 移行計画・ロードマップ |
