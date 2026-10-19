# mvm (Rust)

Windows 向けノンリニア動画編集ソフト。YouTube 向けの技術・数学解説動画を主対象とする。
C++/Qt 版 (`C:\dev\soft\mvm`) から Rust への全面移行中。

## 現在の状態

**P6 書き出し。** 純粋ロジック（P1）と、FFmpeg で probe・静止画・音声・映像を読む経路（P2）、
メニュー・プロジェクトパネル・timeline の GUI（P3）、preview の再生（P4）に加え、timeline で編集できる。
全 tool（V A C B N R Y U P H Z T）・inspector・keyframe（Pen）・transition の長さ・track の表示 / 消音 / ソロ・
copy / paste・undo / redo を持つ。編集中は自動復旧データ（`.mvm.recovery`）を書き、`.mvm.lock` で
同じ Project の二重起動を止め、外部で書き換えられた file には上書きの前に確認を出す。
preview の decode と合成は worker thread が行い、音声があれば WASAPI の IAudioClock を正にして映像を進める
（[docs/adr/0001-gpu-path.md](docs/adr/0001-gpu-path.md)）。
「ファイル > 書き出し」（Ctrl+M）で、preview と同じ合成を H.264 / AAC の MP4 にする
（[docs/adr/0002-export-path.md](docs/adr/0002-export-path.md)）。数式系の描画はまだ無い。移行計画は [docs/migration-plan.md](docs/migration-plan.md)。

開発の規約は [AGENTS.md](AGENTS.md) に一本化している。

## ツールチェーン

| | |
| --- | --- |
| Rust | `rust-toolchain.toml` で固定（rustup が自動で取得する） |
| FFmpeg | `.\dev.ps1 setup` が pin した BtbN gpl-shared を `third_party/ffmpeg` へ置く。別の FFmpeg にはフォールバックしない |
| libclang | `setup` が無ければ LLVM を入れる。bindgen が使う |
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
.\dev.ps1 setup      # FFmpeg gpl-shared の取得と libclang の確認
.\dev.ps1 test       # 試験（workstation 試験は除外。0 件なら失敗）
.\dev.ps1 test-full  # workstation 試験も含める（0 件なら失敗。C++ 版 corpus の試験は MVM_CPP_PROJECT_CORPUS があるときだけ）
.\dev.ps1 testmedia  # 検証素材を生成する（既定は Smoke）
.\dev.ps1 lint       # rustfmt 差分・clippy・アーキテクチャ検査・PSScriptAnalyzer
.\dev.ps1 fmt        # 整形
.\dev.ps1 check      # 型検査
.\dev.ps1 clean
.\dev.ps1 help
```

## C++ 版との互換の確認

C++ 版が書いた `.mvm` を読み、Rust で書き直した file を C++ 版の `mvm_project_validate.exe` で読み直す
workstation 試験がある（C++ 版の DLL を探せるよう MSYS2 UCRT64 の `bin` を PATH に入れる）。

```powershell
$env:PATH = "C:\msys64\ucrt64\bin;$env:PATH"
$env:MVM_CPP_PROJECT_CORPUS = 'C:\dev\soft\mvm'
$env:MVM_CPP_PROJECT_VALIDATE = 'C:\dev\soft\mvm\build\ucrt64-release\bin\mvm_project_validate.exe'
cargo test -p mvm-project --test json reads_cpp_project_corpus -- --include-ignored --nocapture
```

## ディレクトリ

| | |
| --- | --- |
| `crates/core` | 純粋な計算（有理数・timecode・素材 frame と timeline frame の対応・fade・波形 peak など） |
| `crates/media` | FFmpeg による probe・静止画・音声・映像 decode と MP4 の encode |
| `crates/gpu` | 合成（DX12 / wgpu） |
| `crates/testkit` | 試験素材の frame marker |
| `crates/project` | Project model・timeline 編集・tool の判定・effects / keyframe・字幕と SRT・media bin・数式 / Graph のデータ・schema 22 の JSON・自動復旧の envelope |
| `crates/platform` | OS の file 操作（atomic write・file の実体・Project lock） |
| `crates/preview` | preview の 1 frame の計画・合成・音声の mix と再生 |
| `crates/export` | 書き出し（preview と同じ合成を MP4 へ encode する） |
| `crates/app` | session（command・undo / redo・保存・自動保存・preview の再生・書き出しの開始と取消） |
| `apps/mvm` | 製品アプリ（egui） |
| `xtask` | アーキテクチャ検査（crate の依存方向・unsafe を許可する crate） |
| `scripts` | lint などの正式スクリプト |
| `docs` | 移行計画・ロードマップ |
