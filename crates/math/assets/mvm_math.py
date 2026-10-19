"""mvm の数式の描画。request.json の指定どおりに PNG を書き、out/manifest.json に大きさと枚数を書く。

- 式は request.json から読み、この source へ埋め込まない。
- 失敗は error-kind.txt（latex / structure / other）と error.txt に書いて終了コード 3。
- 時間の進み具合は mvm が決めて alpha（分子・分母）で渡す。Animation.interpolate を直接呼び、
  自前の Camera で 1 枚ずつ描く（再生の時間や frame rate に依存しない）。
- 静止の大きさは式の外接矩形 + 8 px の余白。変形などの一時的な canvas は 200 px の余白を足し、
  出力の幅・高さと偶奇を揃える。端点は静止の描画と同じ位置（中央、余りは左上寄せ）に置く。
"""
import json
import logging
import math
import pathlib
import sys

HERE = pathlib.Path(__file__).resolve().parent
OUT = HERE / "out"


def fail(kind, message):
    (HERE / "error-kind.txt").write_text(kind, encoding="utf-8")
    (HERE / "error.txt").write_text(message, encoding="utf-8")
    sys.exit(3)


try:
    from manim import (ORIGIN, RIGHT, SMALL_BUFF, UP, WHITE, AnimationGroup, FadeIn, FadeOut,
                       Indicate, MathTex, ReplacementTransform, Scene, ShowPassingFlash,
                       SurroundingRectangle, TexTemplate, Write, config, linear, logger, smooth)
    from manim.camera.camera import Camera
    from PIL import Image
except Exception as error:  # noqa: BLE001
    fail("other", "manim を import できません: " + repr(error))

REQUEST = json.loads((HERE / "request.json").read_text(encoding="utf-8"))
PX_PER_UNIT = 1080.0 / 8.0
PAD_PX = 8
CANVAS_PAD_PX = 200

# 全ての描画が同じ TeX 環境を使う。約分の斜線は文字の大きさに合った線で描く。
TEMPLATE = TexTemplate()
TEMPLATE.add_to_preamble(r"\usepackage[thicklines]{cancel}")


class FallbackLog(logging.Handler):
    """Manim は部分の SVG group が見つからないと log を出し、式全体で代用する。その log を集める。"""

    def __init__(self):
        super().__init__(logging.ERROR)
        self.messages = []

    def emit(self, record):
        text = record.getMessage()
        if "Could not find SVG group" in text:
            self.messages.append(text)


FALLBACK = FallbackLog()
logger.addHandler(FALLBACK)


def build(segments, font_size):
    try:
        tex = MathTex(*segments, font_size=font_size, tex_template=TEMPLATE)
    except ValueError as error:
        fail("latex" if "latex error" in str(error).lower() else "other", str(error))
    except Exception as error:  # noqa: BLE001
        fail("other", repr(error))
    if FALLBACK.messages:
        fail("structure", "Manim が式の部分を見つけられず、式全体で代用しました: " + FALLBACK.messages[0])
    if len(tex.submobjects) != len(segments):
        fail("structure", "式の部分の数が分けた数と違います (Manim %d、mvm %d)"
             % (len(tex.submobjects), len(segments)))
    tex.set_color(WHITE)
    return tex


def static_size(tex):
    return (math.ceil(tex.width * PX_PER_UNIT) + 2 * PAD_PX,
            math.ceil(tex.height * PX_PER_UNIT) + 2 * PAD_PX)


def with_parity(size, odd):
    return size if (size % 2 == 1) == odd else size + 1


def canvas_for(sizes):
    width = max(size[0] for size in sizes) + 2 * CANVAS_PAD_PX
    height = max(size[1] for size in sizes) + 2 * CANVAS_PAD_PX
    return with_parity(width, REQUEST["width_odd"]), with_parity(height, REQUEST["height_odd"])


def camera(size):
    width, height = size
    config.pixel_width = width
    config.pixel_height = height
    config.frame_width = width / PX_PER_UNIT
    config.frame_height = height / PX_PER_UNIT
    return Camera(background_opacity=0)


def placed(tex, canvas, static):
    """canvas の中で、静止の描画を中央（余りは左上寄せ）に置いたのと同じ位置へ置く。"""
    item = tex.copy()
    item.move_to(ORIGIN)
    shift_x = (canvas[0] - static[0]) // 2 - (canvas[0] - static[0]) / 2
    shift_down = (canvas[1] - static[1]) // 2 - (canvas[1] - static[1]) / 2
    item.shift(RIGHT * (shift_x / PX_PER_UNIT) + UP * (-shift_down / PX_PER_UNIT))
    return item


MANIFEST = {}


def frame_path(name, index):
    return OUT / ("%s-%05d.png" % (name.replace("/", "_"), index))


def capture(cam, mobjects, name, index):
    cam.reset()
    cam.capture_mobjects(mobjects)
    path = frame_path(name, index)
    path.parent.mkdir(parents=True, exist_ok=True)
    Image.fromarray(cam.pixel_array).save(path)


def record(name, size, frames):
    MANIFEST[name] = {"width": size[0], "height": size[1], "frames": frames}


def ratio(pair):
    return pair[0] / pair[1]


def draw_static(name, tex):
    size = static_size(tex)
    capture(camera(size), [placed(tex, size, size)], name, 0)
    record(name, size, 1)
    return size


def draw_transform(name, source, target, item):
    source_size = static_size(source)
    target_size = static_size(target)
    canvas = canvas_for([source_size, target_size])
    cam = camera(canvas)
    a = placed(source, canvas, source_size)
    b = placed(target, canvas, target_size)
    animations = [ReplacementTransform(a[i], b[j], rate_func=smooth) for i, j in item["pairs"]]
    animations += [FadeOut(a[i], rate_func=smooth) for i in item["unmatched_source"]]
    animations += [FadeIn(b[j], rate_func=smooth) for j in item["unmatched_target"]]
    group = AnimationGroup(*animations)
    group.begin()
    shown = [animation.mobject for animation in animations]
    for index, alpha in enumerate(item["alpha"]):
        group.interpolate(ratio(alpha))
        capture(cam, shown, name, index)
    record(name, canvas, len(item["alpha"]))


def draw_action(name, tex, item):
    static = static_size(tex)
    canvas = canvas_for([static])
    cam = camera(canvas)
    whole = placed(tex, canvas, static)
    target = whole[item["segment"]]
    if item["operation"] == "outline":
        base = [whole]
        shape = SurroundingRectangle(target, color=WHITE, buff=SMALL_BUFF)
        animation = ShowPassingFlash(shape, time_width=0.3, rate_func=smooth)
    elif item["operation"] == "pulse":
        base = [part for index, part in enumerate(whole.submobjects) if index != item["segment"]]
        animation = Indicate(target.copy(), scale_factor=1.2, color=WHITE, rate_func=linear)
    else:
        fail("other", "未知の operation です: " + str(item["operation"]))
    capture(cam, base, name + "/base", 0)
    record(name + "/base", canvas, 1)
    animation.begin()
    for index, alpha in enumerate(item["alpha"]):
        animation.interpolate(ratio(alpha))
        capture(cam, [animation.mobject], name, index)
    record(name, canvas, len(item["alpha"]))


def run():
    kind = REQUEST["kind"]
    if kind == "static":
        draw_static("static", build([REQUEST["source"]], REQUEST["font_size"]))
    elif kind == "write":
        tex = build([REQUEST["source"]], REQUEST["font_size"])
        size = static_size(tex)
        cam = camera(size)
        animation = Write(placed(tex, size, size))
        animation.begin()
        for index, alpha in enumerate(REQUEST["alpha"]):
            animation.interpolate(ratio(alpha))
            capture(cam, [animation.mobject], "write", index)
        record("write", size, len(REQUEST["alpha"]))
    elif kind == "transform":
        source = build(REQUEST["source"]["segments"], REQUEST["source"]["font_size"])
        target = build(REQUEST["target"]["segments"], REQUEST["target"]["font_size"])
        draw_transform("transform", source, target, REQUEST)
    elif kind == "equation":
        states = [build(state["segments"], state["font_size"]) for state in REQUEST["states"]]
        for index, tex in enumerate(states):
            draw_static("s%d" % index, tex)
        for index, item in enumerate(REQUEST["transitions"]):
            draw_transform("t%d" % index, states[item["from"]], states[item["to"]], item)
        for index, item in enumerate(REQUEST["actions"]):
            draw_action("a%d" % index, states[item["state"]], item)
    else:
        fail("other", "未知の描画の種類です: " + str(kind))
    OUT.mkdir(parents=True, exist_ok=True)
    (OUT / "manifest.json").write_text(json.dumps({"sequences": MANIFEST}), encoding="utf-8")


# scene 自体の最後の 1 枚（-s）は使わない。小さい canvas にしておく。
config.pixel_width = 16
config.pixel_height = 16
config.frame_width = 16 / PX_PER_UNIT
config.frame_height = 16 / PX_PER_UNIT


class MvmMath(Scene):
    def construct(self):
        run()
