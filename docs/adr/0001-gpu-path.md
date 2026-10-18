# 0001 映像は D3D12VA から wgpu へ zero-copy する

## 決定

映像の decode は D3D12VA だけにする。decode 済みの NV12 texture は、同じ DXGI device 上の wgpu texture として合成する。画素を CPU へ戻してから upload する decode 経路は持たない。

texture が shader から読めない（`VIDEO_DECODE_REFERENCE_ONLY` または `DENY_SHADER_RESOURCE`）ときは、別の経路へ切り替えず失敗させる。

静止画と試験のために、CPU 上の NV12 / P010 / RGBA を compositor へ upload する機能は残す。これは映像 decode の代替ではない。

## 測定

[事実] 2026-10-18、NVIDIA GeForce RTX 4090、`cargo test --release`（workspace の release profile）。

入力は `dev testmedia` の Smoke 素材 `v1080p60_h264.mp4` と `v1080p60_hevc.mp4`（各 1920×1080、60fps）。各 60 frame を D3D12VA で decode し、wgpu へ import して 2 layer を合成した（出力 2432×1080。2 本目の frame marker が 1 本目と重ならない位置）。合成結果を CPU へ読み戻し、両方の marker が PTS から求めた frame 番号と一致した。

この 60 frame（decode、import、合成、読み戻し）の所要時間は 300.66 ms で、1 秒以内だった。D3D11VA の読み戻し経路を除いた後の同じ release 試験でも、marker は一致した。

判定の試験は `crates/media/tests/gpu_path.rs`（`#[ignore = "workstation"]`）。再測定は release で次を実行する。

```powershell
cargo test -p mvm-media --release --test gpu_path -- --ignored --nocapture
```

## 採らなかった経路

D3D11VA で decode し、NV12 を CPU へ転送してから wgpu へ upload する経路は、上記が marker と所要時間の両方を満たしたため実装から除いた。実行時の代替にはしない。
