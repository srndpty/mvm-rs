# 改善のロードマップ

「今後やること」の一覧。着手したら該当の文書へ移し、ここからは消す。段階の全体像は [migration-plan.md](migration-plan.md)。

## P1 の残り

- Undo の memory 予算に使う Project の大きさの見積もり（C++ 版の `approximateProjectBytes`）を、app crate で
  snapshot を持つときに決める。

## preview の再生

- ピッチ保持（`preserve_pitch`）の clip を含むと再生は失敗する。time stretch を入れるときに鳴らす。
- 音声素材と再生デバイスは 48 kHz stereo だけを受け付け、それ以外は失敗させている。resample を入れるかは
  実素材で必要になった時点で決める。
- 再生中の seek は WASAPI の端点を開き直す。体感で遅ければ、端点を保ったまま buffer を捨てる形にする。
- 数式・グラフ・Manim の clip は preview に描かない（math crate を入れる段階で描く）。

## 自動保存と復旧

- file identity と `.lock` は、autosave と recovery を入れるときに platform へ足す。

## 字幕

- 書き出し計画の検査は export crate（P6）で移植する。raster 自体は text crate にある。
