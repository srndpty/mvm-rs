# 改善のロードマップ

「今後やること」の一覧。着手したら該当の文書へ移し、ここからは消す。段階の全体像は [migration-plan.md](migration-plan.md)。

## P1 の残り

- Undo の memory 予算に使う Project の大きさの見積もり（C++ 版の `approximateProjectBytes`）を、app crate で
  snapshot を持つときに決める。
- file の保存・読み込み（atomic write・file identity・`.lock`）は platform crate を作る段階で移植する。
- C++ 版 test_subtitles.cpp の字幕 raster と書き出し計画の検査は、text crate（P4）と export crate（P6）で移植する。

## P2 media 基盤と GPU 方式の判定

- `dev setup`（FFmpeg gpl-shared の取得と SHA256 検証、libclang の確認）、`dev testmedia`、`dev test-full` を足す。
- D3D12VA → wgpu の zero-copy 経路を spike で判定し、`docs/adr/0001-gpu-path.md` に記録する。
