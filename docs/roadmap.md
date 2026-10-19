# 改善のロードマップ

「今後やること」の一覧。着手したら該当の文書へ移し、ここからは消す。段階の全体像は [migration-plan.md](migration-plan.md)。

## 編集 UI

- transition は timeline 上で選択・コピーできず、inspector から長さの変更と削除だけを行う。UI 指針の
  「全ての要素で選択・削除・コピー・貼り付けを揃える」に合わせる。
- ドラッグ中の枠は掴んだ clip だけを見せる。複数選択の移動では、他の clip の行き先も見せる。
- S1 レーンに矩形選択が無い（clip の行と同じ空白からのドラッグ選択を足す）。

## preview の再生

- 音声素材と再生デバイスは 48 kHz stereo だけを受け付け、それ以外は失敗させている。resample を入れるかは
  実素材で必要になった時点で決める。
- 再生中の seek は WASAPI の端点を開き直す。体感で遅ければ、端点を保ったまま buffer を捨てる形にする。
- ミキサーで音量・定位を変えると、再生中は WASAPI の端点を開き直して続ける。途切れが気になれば、mix の
  設定だけを再生中の mixer へ渡す形にする。

## 書き出し

- 書き出しの速度を release で測る。遅ければ、合成を GPU 上で YUV にしてから読み戻す形を検討する。

## 数式

- 入力中（未確定）の式の preview と、描き直し中に前の描画を見せる last-good が無い。描画中の clip は preview に
  出ない。
- 描けた連番は全て memory に置く（A8 で 1 画素 1 byte）。長い Write や多数の数式で memory が問題になれば、
  C++ 版のような常駐の予算と LRU を入れる。[未検証] 実際の使用量。
- 数式列の editor は、状態の式・表示時間・変形の尺・部分式（式の欄の選択から）・対応・強調の追加と削除だけを
  持つ。状態の並べ替え・部分式の名前の変更・外れた部分式の付け直しは project の関数はあるが UI が無い。
- Manim clip の script が変わったか（fingerprint）を開いたときに確かめていない。「再生成」で描き直す。
- Manim clip の fingerprint は script の 1 file だけで、import する module や画像などの依存 file を追跡しない。
- 公開した Manim の動画（`cache/manim/`）を消す手段が無い。どの Project・undo の履歴からも参照されない file を
  見つけて消す掃除を足す。

## 字幕・文字起こし

- 文字起こしの model は whisper.cpp の large-v3 のまま。C++ 版の比較（Parakeet-ja の方が CER が低い）を受けた
  model の切り替えと区切り直しは、実素材で比べてから決める。
