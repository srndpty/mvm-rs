# 0003 数式は Manim で白い mask にして cache し、Graph は mvm が描く

## 決定

数式・数式列（Equation Sequence）・Graph・Manim clip は `crates/math` が描く。preview と書き出しは
同じ `MathLibrary`（描けた artifact の immutable な snapshot）と同じ関数（`math_paint`・`GraphLayout`）で
frame の絵を決める。

### 数式・数式列

- 描画は Manim Community + MiKTeX（latex・dvisvgm）の外部 process。process は Job Object に入れ、取消・
  timeout では latex などの子 process まで止める（`mvm_platform::run_process`）。
- script は 1 本（`crates/math/assets/mvm_math.py`）。静止・Write・変形・数式列を、`Animation.interpolate`
  を直接呼んで自前の Camera で 1 枚ずつ描く。進み具合（alpha）は mvm が決めて渡す
  （変形と Write は i / N、outline は (2i + 1) / (2N)、pulse は三角波 (N − |2i + 1 − N|) / N）。
  Manim の frame rate や再生の時間には依存しない。
- 描画結果は白い glyph の被覆（A8 の mask）で、色・背景・位置・拡大・回転は mvm が後段で付ける。
  mask は出力の中央（余りは左上寄せ）に置く。変形・強調の一時的な canvas は 200 px の余白を足し、
  出力の幅・高さと偶奇を揃え、端点の式を静止の描画と同じ画素位置へ置く（半画素の shift を Manim に渡す）。
- cache key は「script の版・描画要求の JSON・toolchain の fingerprint（manim・latex・dvisvgm の
  `--version` の 1 行目）」の SHA-256。色・背景・ClipEffects・clip ID は key に入れない。
  disk は `<Project の directory>/cache/math/<Project の file 名>/<key>/` に PNG と `manifest.json`
  （最後に書く。完成の印）。壊れた・食い違う結果は消して描き直す。
- 描画は worker 1 本（`MathService`）。UI は要る描画の並び（playhead に近い clip から）を渡し、要らなくなった
  描画は process ごと止める。TeX の誤りは log の `!` 行を理由にして覚え、「再試行」まで描き直さない。
- 変形の部分の分け方と照合は C++ 版の `mvm-tex-segments/1`・`mvm-tex-match/1` を移植した。数式列は部分式
  （SemanticPart）の範囲を segment にし、利用者の対応を先に、残りを自動の照合で組む。部分式の範囲は TeX の
  字句を壊さない範囲だけを受け付ける（C++ 版の `safeRange`）。Manim が部分を見つけられず式全体で代用したら
  失敗にする（部分の数が分けた数と違う・`Could not find SVG group` の log）。
- 保存先がある Project は、Project lock を持つ instance だけが描く。保存先が変われば新しい場所で描き直す。

### Graph

- 式の compile・評価・標本化（plot の幅の 4 倍）は mvm が行う（C++ 版の `graph_numeric` を移植）。
  漸近線・不連続点は結ばない。曲線・軸・格子は tiny-skia で描き、plot の外へ出さない。
- ラベル（x 軸・y 軸・関数）は LaTeX。静止の数式と同じ描画（1 em = 68 px）を帯の内側へ縮めて置く。

### Manim clip

利用者の script の Scene を Project の寸法・fps の mp4 にし（`manim render --format mp4`）、
`<Project の directory>/cache/manim/<hash>.mp4` に置いて動画の clip として扱う。同じ script・Scene を
生成し直すと asset と clip の動画を置き換える。

### 書き出し

出力する数式・数式列・Graph のラベルの描画が全て済んでいなければ、書き出しを計画の時点で拒否する
（`require_ready`）。preview は描画中の clip を描かずに済ませ、理由を出す。書き出しは Manim を起動しない。

## 検証

- 通常の試験: 分け方・照合（C++ 版の期待値）、key、進み具合、合成、mask の読み書き、数式列の compile、
  Graph の標本化と描画、frame ごとの絵の決め方（Write → 静止、変形の区間）、書き出しの拒否。
- workstation 試験（`#[ignore = "workstation"]`、Manim と MiKTeX が要る）:
  - `mvm-math` の `renders_with_the_real_manim_and_reads_the_cache_back`（静止・Write・TeX の誤りの分類・
    cache の読み直し）、`renders_transforms_and_equation_sequences_with_the_real_manim`（変形の 0 枚目が
    静止と同じ位置、数式列の全連番）、`renders_a_scene_to_an_mp4_with_the_real_manim`。
  - `mvm-export` の `math_clips_are_exported_with_the_rendered_masks`（Write の進みと静止の式が MP4 に出る、
    preview との平均差、描画が無ければ計画で拒否）と `graph_clips_are_exported_like_the_preview`。
- [事実] 2026-10-19、Manim Community v0.21.0・MiKTeX-pdfTeX 4.27（MiKTeX 26.5）・dvisvgm 3.6 で上の
  workstation 試験が全て通過した。

## 採らなかったもの

- C++ 版の formal な検証（描画物の木の所有の検査、端点・静止・pulse の base の全画素照合、2 段階の
  structure / render の process、preview の常駐 memory の予算）。部分の数と代用の log だけを検査する。
- 入力中（未確定）の式の preview と last-good の表示。式は「適用」（Ctrl+Enter）で確定してから描く。
- Graph の描画を Manim（cairo）で行う C++ 版の backend。preview と書き出しを mvm だけで描けるため。
