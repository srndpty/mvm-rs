# 改善のロードマップ

「今後やること」の一覧。着手したら該当の文書へ移し、ここからは消す。段階の全体像は [migration-plan.md](migration-plan.md)。

## P1 の残り

- Undo の memory 予算に使う Project の大きさの見積もり（C++ 版の `approximateProjectBytes`）を、app crate で
  snapshot を持つときに決める。

## preview

- 停止中の preview は UI thread で同期して decode・合成している。遠い seek の間は画面が止まる。P4 の worker と
  event mailbox に移す。

## 自動保存と復旧

- file identity と `.lock` は、autosave と recovery を入れるときに platform へ足す。

## 字幕

- C++ 版 test_subtitles.cpp の字幕 raster と書き出し計画の検査は、text crate（P4）と export crate（P6）で移植する。
