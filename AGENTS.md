# AGENTS.md

人間と AI の全ての貢献者が守る規約。`CLAUDE.md` はこのファイルを参照するだけにし、規約はここに一本化する。

## 基本方針

- 人間が読む想定の文書は日本語で書く（README・docs・コメント・コミットメッセージ・ユーザー向けのエラー文）。識別子・CLI 名・crate 名は英語。
- DRY や責務の分離などを意識する。同じ判定ロジックを 2 か所に書かない。責務は crate と module の境界で分ける。
- ただし、個人向けプロジェクトかつ開発中のため、フォールバックや過剰な互換性残しは不要（禁物）。
  - 依存（FFmpeg・フォント・GPU・音声デバイスなど）が無ければ、代替を探さず明確なメッセージで失敗させる。暗黙のフォールバックは失敗を成功に見せかける。
  - 旧 schema・旧 API のための分岐を残さない。形式を変えたら古いものは読まない。
  - 環境差を先回りして吸収しない。必要になったら、その時に明示的に扱う。

## 移行の位置づけ

C++ 版 `C:\dev\soft\mvm` から Rust へ全面移行している。C++ 版は**仕様と試験の参照元**として読むだけで、FFI で共存させない。
移行計画と各段階の gate は [docs/migration-plan.md](docs/migration-plan.md)。

## crate の構成と依存方向

層の規則の正本は `xtask/src/arch.rs` の `RULES` で、`cargo xtask arch`（`dev lint` に含む）が検査する。
crate を足すときは、まず `RULES` に依存してよい crate を書く。規則の無い crate は検査を通らない。

- `crates/core`、`crates/project` は純粋（OS・GPU・UI・FFmpeg に依存しない）。
- `unsafe` を許可するのは OS / FFmpeg の FFI を担う crate（`platform`・`media`・`audio`）だけ。
  - workspace の lint（`unsafe_code = "forbid"`）を使わず、自前の `[lints]` で `unsafe_code = "deny"` と `clippy::undocumented_unsafe_blocks = "deny"` を設定する。
  - `#[allow(unsafe_code)]` は FFI の module だけに付け、全ての `unsafe` block に `// SAFETY:` を書く。
  - 公開 API は安全にする。生ポインタや FFI の型を公開シグネチャに出さず、失敗は `Result` で返す。
- UI（`apps/mvm`）は状態の表示と操作の受け付けだけを担う。編集の判定は `project`、command・undo・保存は `app` に置く。

## lifetime の設計規則（use-after-free を構造的に作らない）

C++ 版の UAF の大半は「他の object を参照で持つ」「lock の外で生ポインタを使う」「`this` を capture した thread」から生じた。

- 他の object への back-reference を持たない。clip・track・素材は ID で参照し、使う時点で引き直す。
- 長く生きる参照（`&'a T` を持つ struct）を作らない。共有は immutable な snapshot を `Arc` で渡す。
- worker thread は自分の状態を所有し、外とは channel で通信する。`Arc<Mutex<_>>` は最小限にする。
- thread の handle は所有者の struct に置き、`Drop` で停止と join を保証する。detach しない。
- COM object は `windows` crate の RAII、FFmpeg の object は `Drop` を持つ wrapper で解放する。GPU の resource を参照する frame は device への `Arc` を保持する。

## エラー処理

- 製品コードで `unwrap` / `expect` / `panic!` / `unreachable!` / `todo!` / `unimplemented!` を使わない（workspace の clippy lint で禁止。試験では可）。`Result` と `?` で返す。
- 入力（media・project file・ユーザー操作）から来る数は信用しない。添字は `get()`、長さ・offset は `checked_*`、0 になり得る値（fps・sample rate・大きさ）で割らない。
- worker の失敗は error として通知する。黙って止まった worker を残さない。

## 試験

- **fail-closed**: 試験が 0 件なら失敗（`dev test` が検査する）。比較を省いたなら skipped と報告し、通過と書かない。null でない・`Ok` であることは成功の証拠ではない。値を検査する。
- 検査を足すときは、それが無いと失敗する負の試験も足す。
- 試験の期待値は実装から独立に書く（ここだけは DRY の例外）。実装の関数で期待値を計算すると、実装のバグを追認してしまう。
- 実 GPU・音声 endpoint・実窓が必要な試験は `#[ignore = "workstation"]` にする。CI では実行しない。
- GUI の試験は focus を奪わず、ユーザーの画面状態に依存させない。
- 性能の数値は release ビルドで測ったものだけを使う。数値を文書へ手で写さない。

## 開発コマンド

入口は `dev.ps1`（`dev <command>`）。判定やビルド処理は `dev.ps1` に重複して書かず、cargo や `scripts/` の正式スクリプトへ委譲する。
機能を足したら、対応する dev コマンドもその時に足す。

## 文書

- 記述の印: `[事実]`（実測・確認済み）、`[推測]`、`[未検証]`、`[回避策]`。「動くはず」を「動く」と書かない。
- 今後の作業は `docs/roadmap.md` にだけ書く。着手したら該当文書へ移し、roadmap からは消す。

## コミット

- 依頼されたときだけコミットする。メッセージは日本語で、何をしたかより「なぜ」を書く。
- pre-commit を無効化（`--no-verify`）しない。フックが落ちたら原因を直す。

## UI の指針

- dark theme。テキストの記号をアイコン代わりにしない。
- 編集中は preview し、確定時に 1 回だけ commit する（undo 1 段）。
- 時間は frame と秒を併記する。
- timeline の全ての要素で、選択・削除・コピー・貼り付けの操作を揃える。
