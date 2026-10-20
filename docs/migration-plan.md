# mvm → Rust 全面移行計画

## Context

`C:\dev\soft\mvm` は C++/Qt Quick（MSYS2 UCRT64）製の Windows 向け動画編集ソフト。追跡対象のコード（C++/QML）は約 19 万行あり、そのうち製品として使っている部分は project 層 1.3 万行、controller 1.3 万行、QML 1.3 万行、preview/media 層およそ 2.5 万行。残りは spike、検証用 harness、試験。
use-after-free は構造的な問題で、次の箇所で起きている。
- WASAPI sink が clock を参照で保持（実際に UAF が起きた）
- `preview_engine.cpp` で lock を外した後に worker の生ポインタを使う
- `DecodedGpuFrame` の AddRef していない texture
- controller の `this` を capture した worker
- `currentClip()` が返す、commit で無効になる `project_` 内部へのポインタ

これを言語レベルで断つため、新リポジトリ `C:\dev\soft\mvm-rs` に Rust で丸ごと書き直す。C++ 版は**仕様と試験の参照元**として読むだけにし、FFI での段階的共存はしない。

## 決定事項

| 項目 | 決定 |
| --- | --- |
| GUI | egui / eframe（wgpu・DX12 backend） |
| .mvm | C++ 版の最新 **schema 22** を読み書きする。旧 schema 16〜21 の受理と schema 間の差分検査は移植しない |
| 書き出し | MLT は廃止する。preview と同じ wgpu compositor で描き、FFmpeg（libavcodec の libx264 / AAC）で encode・mux する |
| toolchain | `x86_64-pc-windows-msvc`。FFmpeg は固定版の prebuilt **gpl-shared**（BtbN の n8.1 系）を script で取得し、SHA256 で検証する |
| 参照 | filmcraft（`C:\dev\tools\filmcraft`, MIT/Apache）の crate 構成・lint 設定・unsafe の隔離・egui の構成を参考にする |
| 移植しない | spike / probe app、`presentmon-patches`、`qt-patches`、P2/P4 の formal 検証機構（vblank authority、ledger など）、300 本以上ある phase 固有の check script、MLT adapter 一式、Qt の text raster |

## ワークスペース構成（責務の分離 = crate の境界）

C++ の layering 規則は crate の依存関係で強制する。lint で grep する必要はなくなる。

```
crates/
  core/      ← src/core      有理数 timebase・timecode・source↔timeline frame mapping・fade・layer placement・waveform peaks・ETA（純粋、依存なし）
  project/   ← src/project   Project model・schema 22 の serde・timeline 編集の全操作・effects/keyframes・subtitles・media bin・math/graph/eq-seq の data・render plan・undo の予算計算（純粋。依存は core と serde だけ）
  platform/  ← src/util      atomic write・file identity・long path・Job Object 付き process・Explorer で表示・project lock（unsafe を許可する crate その1）
  media/     FFmpeg の probe / decode（D3D12VA）/ 静止画 / 音声 decode / 波形 decode（unsafe を許可する crate その2。FFI module に限る）
  gpu/       wgpu compositor（WGSL）。NV12・P010・RGBA の layer を affine・crop・opacity で合成する。preview と export で共有
  audio/     WASAPI sink・IAudioClock を master にする clock・A/V scheduler・mixer bus（windows crate を使う。unsafe は FFI module に限る）
  text/      cosmic-text による文字 raster（Meiryo を明示指定し、無ければ失敗）
  preview/   preview engine（状態機械・worker・event mailbox）。docs/preview-engine-contract.md の契約のうち、残すものを移植
  export/    書き出し計画と encode・mux
  math/      Manim / LaTeX / graph backend の外部 process 実行と cache key（Python script は assets として持ち込む）
  app/       旧 MvmController から UI を除いた部分: AppState・command・undo・autosave/recovery・job 管理
  testkit/   frame marker の読み取り・試験素材の解決・fixture
apps/mvm/    egui の UI（timeline・project panel・inspector・dialog・menu・shortcut）
xtask/       arch 検査（cargo metadata で crate 間の依存方向と unsafe を許可する crate を検証）
```

**lifetime の設計規則**（AGENTS.md にも書く）
- 他の object を指す back-reference は持たない。clip などは ID で参照する。
- worker は自分の状態を所有し、channel で通信する。immutable な snapshot は `Arc` で共有する。
- COM object は windows-rs の RAII に任せる。AVFrame の wrapper は `Drop` で解放し、device への `Arc` を保持する。
- thread の handle は所有者の struct に置き、`Drop` で停止・join を保証する。

## dev コマンド（`dev.ps1` が本体、`dev.cmd` は薄い wrapper）

旧版の `dev.ps1` と同じ形（`switch` で各処理へ委譲し、日本語で help を出す）にする。グローバルの `C:\Users\lambe\bin\dev.ps1` から `dev <cmd>` で呼べる。

| cmd | 内容 | 追加する段階 |
| --- | --- | --- |
| help | 一覧を表示 | P0 |
| build | `cargo build --workspace`（`-Release` で release） | P0 |
| run | debug の `mvm` を build して起動 | P0（空の窓） |
| gui | release の `mvm` を起動 | P0 |
| test | `cargo test --workspace`（workstation 試験は除外） | P0 |
| test-full | `--include-ignored` を付けて workstation 試験も実行 | P2 |
| lint | `cargo fmt --check` + `cargo clippy -D warnings` + `cargo xtask arch` | P0 |
| fmt | `cargo fmt` | P0 |
| check | `cargo check --workspace --all-targets`（型検査） | P0 |
| clean | `cargo clean` | P0 |
| setup | FFmpeg の取得と検証、LLVM（libclang）の確認 | P2 |
| testmedia | 試験素材の生成（旧 `make-testmedia.ps1` を移植） | P2 |

`dev.ps1` の中では `FFMPEG_DIR`、`LIBCLANG_PATH`、FFmpeg の DLL を探す PATH を設定する。判定や build の処理を `dev.ps1` に重複して書かない。

## 6 点セット

- **formatter**: rustfmt（`rustfmt.toml`、edition 2024、max_width 100）
- **linter**: clippy。`[workspace.lints]` で `unsafe_code = "forbid"`、`dbg_macro` と `todo` を warn にする。FFI crate だけ `deny` + `undocumented_unsafe_blocks = "deny"`。`.ps1` は PSScriptAnalyzer
- **type check**: `cargo check --all-targets`
- **test**: `cargo test`。実 GPU・音声 endpoint・窓が必要な試験は `#[ignore = "workstation"]` にする
- **CI**: GitHub Actions の `windows-latest`。fmt、clippy、check、test（portable）、xtask arch を回す。FFmpeg と試験素材は cache する。旧 CI と同じく `fail-fast: false`
- **pre-commit**: pre-commit-hooks（空白・EOF・改行の混在・yaml/json/toml・512 KB の大容量 file）と local hook の `cargo fmt --check`。clippy は `pre-push` に置く（コミットを重くしない旧方針を継承）
- その他: `git init`、`.gitignore`（target, third_party/ffmpeg, tests/assets）、`.gitattributes`、`.editorconfig`、`rust-toolchain.toml`（`rustup update` 後の最新 stable に固定。現在 1.89）

## AGENTS.md（CLAUDE.md は参照 1 行だけ）

ユーザー指定の 3 項目に加え、旧 AGENTS.md と filmcraft から次を短く移す。
- 人間が読む想定の文書は日本語（識別子と CLI 名は英語）
- DRY と責務の分離。判定ロジックは 1 か所に置く。ただし試験の期待値は実装から独立に書く
- 個人向けかつ開発中なので、フォールバックや過剰な互換性残しは不要（禁物）。依存が無ければ明確に失敗させる。旧 schema 用の分岐は作らない
- fail-closed: 試験が 0 件なら失敗、比較を省いたら skipped と報告する。検査を足すときは、それが無いと失敗する負の試験も足す
- crate の依存方向、unsafe を許可する crate、lifetime の設計規則（上記）
- 製品コードでは `unwrap` / `expect` / `panic!` を使わず `Result` で返す（試験では可）。worker は失敗を error として通知する
- 文書の印 `[事実]` `[推測]` `[未検証]` `[回避策]`、今後の作業は `docs/roadmap.md` だけに書く、コミットは日本語で依頼されたときだけ
- UI の指針: dark theme、編集中は preview して確定時に 1 回だけ commit（undo 1 段）、frame と秒を併記

## 段階計画

各段階の終わりで `dev lint` / `dev test` を通し、C++ 版の該当試験を移植したものが通ることを gate にする。

**P0 基盤（このプランの承認後、最初に実行）**
- git init、Cargo workspace、6 点セット、AGENTS.md、CLAUDE.md、README（日本語）、`dev.ps1` / `dev.cmd`、CI
- `apps/mvm` は eframe の空の窓、`crates/core` はひな形を置く

**P1 core + project（純粋ロジック。価値が最も高く、リスクは最も低い）**
- `src/core/*` と `src/project/*` を移植する
- timeline の編集操作は pure function として移す: move / trim / split / ripple / roll / slip / slide / rate stretch / speed / frame hold / ripple delete / transitions / `validateTimeline`
- 試験は `tests/core/*` と `tests/harness/test_timeline_edit.cpp`（41 関数）・`test_clip_effects`・`test_subtitles`・`test_media_bin`・`test_equation_sequence`・`test_graph_domain`・`test_audio_adjustment` の assertion を `#[test]` に移す
- schema 22 の fixture（`tests/fixtures/math-score/good.mvm` など）の読み込みと round-trip を検証する。C++ 版で保存した実 project の読み込みでも確認する
- serde の derive は重複 key を拒否し、未知の key は無視する（旧 parser と同じ）
- undo は `Vec<Arc<Project>>` の snapshot にし、100 件 / 256 MiB の予算で古いものを捨てる（`editHistoryEntriesToDrop` を移植）

**P2 media 基盤と GPU 方式の判定 spike**
- `dev setup`: BtbN の FFmpeg n8.1 gpl-shared を取得し、LLVM（winget）の libclang を確認する
- FFmpeg の binding は `rsmpeg` か `ffmpeg-sys-next` を使う（hwcontext を直接触れる必要がある）
- `make-testmedia.ps1` を移植し、frame marker の reader を testkit に置く
- **判定**: wgpu の DX12 device を FFmpeg の D3D12VA に渡し、decode 済みの `ID3D12Resource`（NV12）を `create_texture_from_hal` で wgpu の texture にする zero-copy 経路を試す
  - 基準: 1080p60 の素材 2 本を 2 layer で合成して 60fps、frame marker が一致すること
  - 不合格なら、D3D11VA で decode して staging に読み戻し、wgpu に upload する方式（filmcraft と同じ）を採る
  - 結果は `docs/adr/0001-gpu-path.md` に記録する。採らなかった方は実行時の fallback として残さない
- probe（旧 `probeMediaFile` の media 種別判定を 1 か所に置く）、静止画（EXIF の向き、ICC は sRGB 以外を拒否）、音声 decode、波形 peaks

**P3 最小 GUI**
- egui の shell: menu、dock 状の layout
- project の新規作成 / 開く / 保存、project panel、timeline の表示（track・clip・ruler・playhead、仮想化、zoom）
- 停止中の preview（seek した frame を表示）
- GUI の試験は `egui_kittest` を使う（focus を奪わない）

**P4 preview の再生**
- WASAPI の sink と IAudioClock の master clock、A/V scheduler（1 frame = 800 sample の論理）
- 複数 layer、transition、文字・静止画 layer
- 状態機械と shutdown の順序は契約を簡略化して移植する（P2/P4 の formal 検証機構は捨てる）

**P5 編集 UI の完成**
- 全 tool（V A C B N R Y U P H Z T）、`TimelineGestures.js` の判定を純関数に移植して試験を付ける
- inspector と keyframe、transition の編集、shortcut 一式、track の eye / mute / solo
- copy / paste、undo / redo、autosave（2 s の debounce と最大 30 s の間隔）、recovery の envelope、`.lock`、外部変更の検出

**P6 書き出し**
- 書き出し計画（旧 `mapTimelineExportPlan`）を移植する
- wgpu の offscreen 合成、libx264 / AAC での encode、速度変更・frame hold・音声の mix / gain / pan
- 書き出し中の進捗表示と取消
- preview と書き出しの pixel 一致試験（parity）

**P7 残りの機能**
- Math / EquationSequence / Graph / Manim clip（外部 process と cache）
- 字幕、Whisper による文字起こし、自動音量調整、audio mixer、pitch 保持（Rubber Band）

**P7 後の照合（C++ 版との差分の洗い出し）**
- C++ 版の `MvmController` の Q_INVOKABLE・`Main.qml` のメニューとショートカット・右クリックメニュー・各 inspector を
  Rust 版の command と UI に一つずつ当て、抜けていたものを足した。純粋な判定は project / core、command は app、
  表示と操作の受け付けは apps/mvm に置いた
- [事実] 足したもの: 素材の読み込み（「読み込み…」・「ファイル > メディアを追加」・OS からのドロップ）と
  プロジェクトパネルの編集（フォルダ・名前の変更・移動・削除の確認）、パネルから timeline へのドラッグと
  ダブルクリックでの配置、track の追加と削除、プロジェクト設定（幅・高さ・fps と「素材に合わせる」）、
  clip の右クリック（速度・デュレーション〈Ctrl+R〉・フレーム保持・リンク解除・リップル削除〈全トラック〉・
  コピー / カット / 複製）、空白とルーラーの右クリック（空白を詰める・ペースト・マーカーとイン / アウトの消去）、
  inspector の effect（位置・拡大率・回転・クロップ・不透明度・音量・ダッキングのアニメーションとキー・補間、
  フェード）と文字 clip の編集、preview の枠（移動と角の拡縮）、音声 clip の波形、J / K / L のシャトル、
  「再生」メニュー
- [事実] C++ 版から意図して変えたもの: 「メディアを追加」の動画は、V1 から順に置く区間が空いている非 mute の
  video track へ置き、どこにも置けないときだけ track を足す（C++ 版 `addVideoClip` は 2 本目から毎回新しい
  track を足した）。リンク音声も A1 から順に空いている track へ置く
- 照合で見つけ、まだ移していないものは [roadmap.md](roadmap.md) に書いた

**P8 切り替え**
- 日常の動画制作を Rust 版で一通り行う
- C++ 版の README に移行済みと記載し、開発を止める

## 主な参照元（C++ 側）

- 編集の中核: `src/project/timeline_edit.cpp`（4,973 行）、`project.h`、`project_json.cpp`（schema 22）
- 編集の試験: `tests/harness/test_timeline_edit.cpp`
- controller: `apps/mvm/mvm_controller*.cpp`（185 invokable → app crate の command 群へ）
- preview: `docs/preview-engine-contract.md`、`src/preview_engine/preview_engine.cpp`、`src/media/gpu_preview/nv12_converter.cpp`（HLSL → WGSL）
- 仕様と経緯: `docs/premiere-like-editing.md`、`docs/math-clips.md`、`docs/research/test-media-format.md`
- 規約: 旧 `AGENTS.md`

## 検証

- **P0**: `dev help` / `dev check` / `dev lint` / `dev test` / `dev run`（空の窓）が成功すること、`pre-commit run --all-files` が通ること、push 後に CI が通ること、グローバルの `dev run` から起動できること
- **P1 以降**: 各段階で移植した試験の件数と通過数を報告する（0 件は失敗扱い）。C++ 版が保存した `.mvm` を Rust 版で開き、再保存した結果を C++ 版で開けることを確認する
- **P2 以降**: `dev test-full` で実 GPU と音声の試験を実行し、frame marker で decode が要求 frame と一致することを機械的に判定する
- **GUI**: `dev run` で実際に操作し、各段階の機能を目視で確認する
