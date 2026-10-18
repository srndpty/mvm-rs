# 改善のロードマップ

「今後やること」の一覧。着手したら該当の文書へ移し、ここからは消す。段階の全体像は [migration-plan.md](migration-plan.md)。

## P1 core + project

- C++ 版の `src/core/*`（timecode 以外）と `src/project/*` を移植する。
- `tests/core/*` と `tests/harness/test_timeline_edit.cpp` などの assertion を `#[test]` へ移す。
- schema 22 の `.mvm` の読み書きと、C++ 版で保存した実 project の往復を確かめる。

## P2 media 基盤と GPU 方式の判定

- `dev setup`（FFmpeg gpl-shared の取得と SHA256 検証、libclang の確認）、`dev testmedia`、`dev test-full` を足す。
- D3D12VA → wgpu の zero-copy 経路を spike で判定し、`docs/adr/0001-gpu-path.md` に記録する。
