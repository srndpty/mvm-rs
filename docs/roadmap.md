# 改善のロードマップ

「今後やること」の一覧。着手したら該当の文書へ移し、ここからは消す。段階の全体像は [migration-plan.md](migration-plan.md)。

## 編集 UI

- transition は timeline 上で選択・コピーできず、inspector から長さの変更と削除だけを行う。UI 指針の
  「全ての要素で選択・削除・コピー・貼り付けを揃える」に合わせる。
- ドラッグ中の枠は掴んだ clip だけを見せる。複数選択の移動では、他の clip の行き先も見せる。

## preview の再生

- ピッチ保持（`preserve_pitch`）の clip を含むと再生と書き出しは失敗する。time stretch を入れるときに鳴らす。
- 音声素材と再生デバイスは 48 kHz stereo だけを受け付け、それ以外は失敗させている。resample を入れるかは
  実素材で必要になった時点で決める。
- 再生中の seek は WASAPI の端点を開き直す。体感で遅ければ、端点を保ったまま buffer を捨てる形にする。
- 数式・グラフ・Manim の clip は preview に描かず、出力に含むと書き出しは失敗する（math crate を入れる段階で描く）。

## 書き出し

- 書き出しの速度を release で測る。遅ければ、合成を GPU 上で YUV にしてから読み戻す形を検討する。
