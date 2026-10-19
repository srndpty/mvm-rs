# 0002 書き出しは preview と同じ合成を 1 frame ずつ encode する

## 決定

書き出しは MLT を使わず、preview と同じ関数で 1 frame ずつ作る（`crates/export`）。

- 各 frame の layer は `mvm_preview::plan_frame`、合成は `mvm_preview::StillPreview`（D3D12VA の decode と
  wgpu の compositor）、音声は `mvm_preview::AudioMixer` を使う。速度変更・フレーム保持・transition・
  effect・字幕は preview と同じ計算を通る。書き出し用の合成や mix を別に持たない。
- 合成結果は RGBA で CPU へ読み戻し、swscale で BT.709 limited の YUV420P にして libx264（preset medium）で
  encode する。stream には BT.709 の色空間・primaries・transfer と limited range を書く。
- 音声は 48 kHz stereo の AAC（192 kbps）。出力する音声 clip が無ければ音声の stream を作らない。
- 画質は C++ 版と同じ 3 段階（CRF 18 / 23 / 28）。
- `<出力>.mp4.mvmtmp` へ書き、閉じた後に probe で寸法・fps・frame 数・音声の有無が計画と一致することを
  確かめてから正規名へ rename する。失敗・取消では一時 file を消し、正規名には何も書かない。
- 数式・Equation sequence・Graph・Manim の clip が出力に含まれるとき、ピッチ保持の音声 clip があるときは、
  encode を始める前に失敗させる。preview はこれらを描かずに済ませるが、書き出しで黙って抜くと
  見た目の違う動画ができる。

書き出しは worker thread（`ExportJob`）が開始時の Project の snapshot で行う。UI は進捗を読み、取消を頼む。
`ExportJob` を drop すると取り消して join する。書き出し中は進捗の dialog が他の操作を止め、窓も閉じられない。

## 検証

workstation 試験 `crates/export/tests/export.rs`（`#[ignore = "workstation"]`）。

- 200% 速度の clip とフレーム保持を含む timeline を書き出し、書いた MP4 を preview で読み直して、
  全 frame の frame marker が手計算した素材 frame と一致すること、画素の平均差が preview の合成と
  3.0 未満であること。別の frame どうしの差が上限を超えることも確かめる（指標が違いを捉える負の対照）。
- 音量 50%・track の定位を左へ寄せた音声の RMS と符号の変化の回数が、素材の正弦波の式と一致すること。
- 字幕の焼き込みが cue の区間だけに出ること、焼き込まない指定では出ないこと。
- 取消・素材の欠落で、正規名と一時 file のどちらも残らないこと。

encoder 単体（`crates/media/tests/encode.rs`）は GPU を使わないので CI でも回る。

```powershell
cargo test -p mvm-export --test export -- --include-ignored --nocapture
```

平均差の実測値は上の試験が出力する。書き出しの速度は [未検証]（release で測っていない）。

## 採らなかった経路

- MLT の tractor で書き出す C++ 版の経路。preview と別の合成になり、両者の差を試験で追い続ける必要があった。
- 合成を GPU 上で YUV にしてから読み戻す経路。まず読み戻しの帯域が問題になるかを release で測ってから決める。
