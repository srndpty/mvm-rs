# 0004 音声の仕上げ（ピッチ保持・ミキサー・自動音量）と字幕・文字起こし

## 決定

### ピッチ保持

- `preserve_pitch` の clip は libavfilter の rubberband（BtbN の gpl-shared が `--enable-librubberband`）で
  time stretch する（`mvm_media::TimeStretcher`）。preview と書き出しは同じ `AudioMixer` を通る。
- 続けて要求された区間は同じ stretcher から出し続け、seek（続きでない区間）では要求した位置の素材から
  入れ直す。出力の先頭は入力の先頭に揃う（FFmpeg の filter が開始の遅延を補う）。
- [事実] 2026-10-19 の実測: 0.4 秒の 440 Hz の音の中心は、tempo 0.5 で −10 ms、tempo 2 で +13 ms ずれた
  （立ち上がりの smearing を含む）。長さは入力 / tempo と一致した。

### ミキサー

- track の音量（-96〜+15 dB）・定位（-100〜100）・ミキサーでの名前は Project に保存し、離したとき・確定したときに
  1 回だけ commit する。再生中に変えたら、今の位置から新しい mix で再生を続ける（端点を開き直す）。
- メーターは mix が track ごとの peak（clip の寄与の最大）と master を記録し、UI が読んで空にする。
  モニター音量は再生の出口だけに掛け、書き出しには掛けない。

### 自動音量調整

- ラウドネスは BS.1770 / EBU R128 の integrated loudness と true peak（4 倍補間）を mvm-audio が純 Rust で測る。
- clip ごとに音量・フェード・track の gain と定位を掛けた音を測り、目標 LUFS との差を補正する（true peak が
  -1 dBTP を超えない範囲）。声の track の 20 ms の窓の RMS で声の区間を検出し、BGM に ducking の key を付ける
  （区間と key の形は project の `make_ducking_keys`、C++ 版と同じ）。
- 解析は worker（最大 4 clip 並行）。結果を確かめてから 1 回の undo で適用する。解析の後に Project を編集した、
  解析中に素材の大きさ・更新時刻・実体が変わった場合は拒否する。

### 字幕

- 字幕パネル（一覧・本文と区間の編集・分割・結合・共通書式・SRT の読み書き）と timeline の S1 レーン
  （選択・移動・端のトリム・Alt で複製）。判定は project の字幕 module。字幕の選択は clip の選択と排他で、
  削除・コピー・貼り付け・複製は選択している方に効く。clipboard は最後にコピーした種類を貼る。

### 文字起こし

- whisper.cpp の `whisper-cli`（外部 process、Job Object）。FFmpeg に whisper の filter は入っていない
  （BtbN は `--disable-whisper`）。素材は任意の sample rate を 16 kHz mono へ窓付き sinc で変換して WAV にする。
- 精度優先の指定は C++ 版と同じ（beam 5・best_of 5・前区間を文脈にしない・非発話の注記を抑制・語彙ヒント）。
  GPU を指定して Vulkan を使えなかったら失敗する（`using Vulkan` が log に無い）。
- whisper-cli・model・言語・GPU は端末の設定（`%APPDATA%\mvm\preferences.json`）に保存する。

## 検証

- 通常の試験: time stretch（2 倍速で長さ半分・音程そのまま、負の対照は間引き）、mix の区切りに依らない出力、
  ラウドネス（EBU Tech 3341 の -23 LUFS・gate・true peak）、自動音量の解析（補正量・声の区間・ducking の key・
  取消・編集後の拒否・1 回の undo）、16 kHz への変換（48 k / 44.1 k、区間の切り出し）、SRT の往復、字幕の編集。
- workstation 試験: `recognizes_speech_with_the_real_whisper`（`MVM_WHISPER_CLI`・`MVM_WHISPER_MODEL`・
  `MVM_TRANSCRIBE_AUDIO` を指定したときだけ。無ければ `dev test-full` が skipped と報告する）。
- [事実] 2026-10-19、msys2 の whisper-cli（whisper.cpp 1.9.1 系）と `ggml-small.bin` で C++ 版の英語の
  合成音声（22.05 kHz）を認識し、"This is a short caption test. The video editor displays these words." を得た。

## 採らなかったもの

- whisper.cpp を link すること（MSVC 向けの build が要る）。外部 process で足りる。
- 素材の内容 hash による照合（C++ 版）。大きさ・更新時刻・file の実体で照合する。
