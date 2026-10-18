//! 数式 clip の Project model と JSON の試験。C++ 版 tests/harness/test_project_math.cpp を移植したもの。
//! 旧 schema（16〜21）の読み込みと Undo の memory 見積もりの検査は、移植しない方針なので除く。

// 試験の失敗は大きく落ちてよい（helper 関数も試験の一部）。Project::default() から field を足す書き方は意図的。
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::field_reassign_with_default
)]

mod common;

use std::path::PathBuf;

use common::*;
use mvm_project::json::{parse_project_json, serialize_project_json};
use mvm_project::math_clip::*;
use mvm_project::timeline::*;
use mvm_project::*;
use serde_json::{Value, json};

fn math_clip(id: &str, source: &str) -> TimelineClip {
    let mut clip = TimelineClip {
        kind: TimelineClipKind::Math,
        name: source.to_owned(),
        id: id.to_owned(),
        source_fps: fps(60, 1),
        source_frame_count: 300,
        source_out_frame: 300,
        track: V2,
        ..TimelineClip::default()
    };
    clip.math.source = source.to_owned();
    clip
}

fn write(frames: i64) -> MathClipAnimation {
    MathClipAnimation {
        intro: MathIntroKind::Write,
        intro_frames: frames,
    }
}

fn with_clips(clips: Vec<TimelineClip>) -> Project {
    let mut project = Project::default();
    let tracks = clips
        .iter()
        .map(|clip| clip.track.index + 1)
        .max()
        .unwrap_or(1);
    while project.video_tracks.len() < tracks {
        add_track(&mut project, TrackKind::Video).unwrap();
    }
    project.timeline_clips = clips;
    project
}

fn project_with_math() -> Project {
    let first = math_clip("math-1", "ax^2 + bx + c = 0");
    // 描けない式も Project の値としては正しい（検証は形だけを見る）。
    let mut second = math_clip("math-2", "\\fracc{\"引用\"}\n{改行}");
    second.timeline_start_frame = 300;
    second.math.font_size = 120;
    second.math.color = "#FF00FFCC".to_owned();
    second.math.background_color = "#80000000".to_owned();
    second.effects.position_y_percent = -20.0;
    second.math_animation = write(90);
    with_clips(vec![first, second])
}

/// 数式のデータを外した文字 clip。
fn text_project() -> Project {
    let mut clip = math_clip("text-1", "x");
    clip.kind = TimelineClipKind::Text;
    clip.math = MathClipData::default();
    clip.text.content = "文字".to_owned();
    with_clips(vec![clip])
}

fn json_path() -> PathBuf {
    std::env::temp_dir()
        .join("mvm-rs-math-test")
        .join("math-test.mvm")
}

fn serialize(project: &Project) -> String {
    serialize_project_json(project, &json_path()).unwrap()
}

fn loads(text: &str) -> bool {
    parse_project_json(text, &json_path()).is_ok()
}

#[test]
fn math_clips_round_trip() {
    let project = project_with_math();
    validate_timeline(&project).unwrap();
    let text = serialize(&project);
    assert!(
        !text.contains("renderer"),
        "描画方針（renderer）を保存しない"
    );
    let value: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(value["schema_version"], json!(PROJECT_SCHEMA_VERSION));
    let clips = &value["timeline_clips"];
    assert_eq!(clips[0]["kind"], json!("math"));
    assert!(clips[0]["math"].is_object());
    assert!(
        clips[0].get("math_animation").is_none(),
        "intro の無い数式 clip は math_animation を書かない"
    );
    assert_eq!(
        clips[1]["math_animation"],
        json!({ "intro": "write", "intro_frames": 90 })
    );
    let parsed = parse_project_json(&text, &json_path()).unwrap();
    assert_eq!(
        parsed, project,
        "式（引用符・改行・日本語）・文字サイズ・色・背景・effects・Write の往復"
    );
    assert_eq!(
        parsed.timeline_clips[0].math_animation,
        MathClipAnimation::default()
    );
}

#[test]
fn math_clip_validation() {
    let base = project_with_math();
    validate_timeline(&base).unwrap();
    let height = base.output_height;
    let invalid = |name: &str, change: &dyn Fn(&mut TimelineClip)| {
        let mut project = base.clone();
        change(&mut project.timeline_clips[0]);
        assert!(validate_timeline(&project).is_err(), "{name}");
    };
    invalid("空の式", &|c| c.math.source.clear());
    invalid("空白だけの式", &|c| {
        c.math.source = " \t\n".to_owned()
    });
    invalid("未対応の記法", &|c| {
        c.math.syntax = "typst".to_owned()
    });
    invalid("文字サイズ 0", &|c| c.math.font_size = 0);
    invalid("出力の高さを超える文字サイズ", &|c| {
        c.math.font_size = height + 1
    });
    invalid("#AARRGGBB でない色", &|c| {
        c.math.color = "#FFF".to_owned()
    });
    invalid("16 進でない背景色", &|c| {
        c.math.background_color = "#GG000000".to_owned()
    });
    invalid("media path を持つ数式 clip", &|c| {
        c.media_path = PathBuf::from("x.png")
    });
    invalid("速度を持つ数式 clip", &|c| c.speed = fps(2, 1));
    invalid("数式のデータを持つ数式以外の clip", &|c| {
        c.kind = TimelineClipKind::Text
    });
    // Write の尺。clip は 300 frame。
    invalid("intro が無いのに尺を持つ", &|c| {
        c.math_animation = MathClipAnimation {
            intro: MathIntroKind::None,
            intro_frames: 30,
        }
    });
    invalid("尺 0 の Write", &|c| c.math_animation = write(0));
    invalid("clip の尺を超える Write", &|c| {
        c.math_animation = write(301)
    });

    let valid = |change: &dyn Fn(&mut Project)| {
        let mut project = base.clone();
        change(&mut project);
        validate_timeline(&project).unwrap();
        project
    };
    valid(&|p| p.timeline_clips[0].math.font_size = height);
    valid(&|p| p.timeline_clips[0].math_animation = write(300));
    // 描画の方式の上限は Project の値に持ち込まない。60 fps で 5 分の Write も Project として正しい。
    let long = valid(&|p| {
        p.timeline_clips[0].source_frame_count = 18000;
        p.timeline_clips[0].source_out_frame = 18000;
        p.timeline_clips[1].timeline_start_frame = 18000;
        p.timeline_clips[0].math_animation = write(18000);
    });
    assert_eq!(
        parse_project_json(&serialize(&long), &json_path()).unwrap(),
        long
    );

    let mut text = text_project();
    validate_timeline(&text).expect("対照: 数式のデータを外した文字 clip");
    text.timeline_clips[0].math_animation = write(30);
    assert!(
        validate_timeline(&text).is_err(),
        "数式の animation を持つ数式以外の clip"
    );
}

#[test]
fn math_clip_json_is_strict() {
    let good = serialize(&project_with_math());
    assert!(loads(&good), "対照: 保存したままの JSON は読める");
    let base: Value = serde_json::from_str(&good).unwrap();
    let mutated = |change: &dyn Fn(&mut Value)| {
        let mut value = base.clone();
        change(&mut value);
        loads(&serde_json::to_string(&value).unwrap())
    };
    fn object<'a>(
        value: &'a mut Value,
        clip: usize,
        key: &str,
    ) -> &'a mut serde_json::Map<String, Value> {
        let clip = &mut value["timeline_clips"][clip];
        if key.is_empty() { clip } else { &mut clip[key] }
            .as_object_mut()
            .unwrap()
    }
    assert!(
        !mutated(&|v| drop(object(v, 0, "").remove("math"))),
        "math object が無い数式 clip"
    );
    assert!(
        !mutated(&|v| drop(
            object(v, 0, "math").insert("renderer".to_owned(), json!("manim-mathtex"))
        )),
        "math の未知の field"
    );
    assert!(
        !mutated(&|v| drop(object(v, 0, "math").remove("font_size"))),
        "math の欠けた field"
    );
    assert!(
        !mutated(&|v| drop(object(v, 0, "").insert("media_path".to_owned(), json!("x.png")))),
        "media_path を持つ数式 clip"
    );
    assert!(
        !mutated(&|v| drop(object(v, 0, "math").insert("syntax".to_owned(), json!("typst")))),
        "未対応の記法"
    );

    let animation = |change: &dyn Fn(&mut serde_json::Map<String, Value>)| {
        mutated(&|v| change(object(v, 1, "math_animation")))
    };
    assert!(
        !animation(&|a| drop(a.insert("rate_func".to_owned(), json!("linear")))),
        "math_animation の未知の field"
    );
    assert!(
        !animation(&|a| drop(a.remove("intro_frames"))),
        "math_animation の欠けた field"
    );
    assert!(
        !animation(&|a| drop(a.insert("intro".to_owned(), json!("unwrite")))),
        "未知の intro"
    );
    assert!(
        !animation(&|a| drop(a.insert("intro_frames".to_owned(), json!(301)))),
        "clip の尺を超える Write"
    );
    assert!(
        animation(&|a| {
            a.insert("intro".to_owned(), json!("none"));
            a.insert("intro_frames".to_owned(), json!(0));
        }),
        "対照: 明示した intro 無しは読める"
    );

    // 重複 key は Value を経由すると消えるので、文字列で作る。
    for (marker, duplicate) in [
        ("\"math\": {", "\"math\": {\"font_size\": 50,"),
        (
            "\"math_animation\": {",
            "\"math_animation\": {\"intro_frames\": 30,",
        ),
    ] {
        assert!(good.contains(marker), "{marker}");
        assert!(
            !loads(&good.replacen(marker, duplicate, 1)),
            "{marker} の重複した field"
        );
    }

    let text = serialize(&text_project());
    assert!(loads(&text), "対照: 文字 clip の file は読める");
    let mut value: Value = serde_json::from_str(&text).unwrap();
    object(&mut value, 0, "").insert(
        "math_animation".to_owned(),
        json!({ "intro": "none", "intro_frames": 0 }),
    );
    assert!(
        !loads(&serde_json::to_string(&value).unwrap()),
        "数式以外の clip の math_animation"
    );
}

// Write の尺は clip の見えている先頭から数え、分割・trim で clip の尺に収める。
#[test]
fn write_intro_follows_split_and_trim() {
    let mut clip = math_clip("math-1", "x");
    clip.math_animation = write(90);
    clip.effects.fade_in_frames = 30;
    let base = with_clips(vec![clip]);
    validate_timeline(&base).unwrap();
    let mut new_id = || "math-1-right".to_owned();

    let mut project = base.clone();
    split_timeline_clips(
        &mut project,
        &ids(&["math-1"]),
        60,
        &mut new_id,
        LinkMode::Single,
    )
    .unwrap();
    assert_eq!(
        project.clip("math-1").unwrap().math_animation,
        write(60),
        "分割の左は縮めた尺に収める"
    );
    assert_eq!(
        project.clip("math-1-right").unwrap().math_animation,
        MathClipAnimation::default(),
        "右は Write を持たない"
    );
    let mut project = base.clone();
    split_timeline_clips(
        &mut project,
        &ids(&["math-1"]),
        200,
        &mut new_id,
        LinkMode::Single,
    )
    .unwrap();
    assert_eq!(
        project.clip("math-1").unwrap().math_animation,
        write(90),
        "Write の後の分割は尺を変えない"
    );

    for (edge, delta, expected) in [
        (TrimEdge::Left, 250, 50),
        (TrimEdge::Right, -260, 40),
        (TrimEdge::Right, -100, 90),
    ] {
        let mut project = base.clone();
        trim_timeline_clip(&mut project, "math-1", edge, delta, LinkMode::Single).unwrap();
        assert_eq!(
            project.timeline_clips[0].math_animation,
            write(expected),
            "{edge:?} {delta}"
        );
    }

    // 置いたときと違う fps の timeline で trim すると、同じ秒数へ換算する。
    let mut project = base;
    let clip = &mut project.timeline_clips[0];
    clip.source_fps = fps(30, 1);
    clip.source_frame_count = 150;
    clip.source_out_frame = 150;
    clip.math_animation = write(45);
    clip.effects.fade_in_frames = 0;
    validate_timeline(&project).unwrap();
    trim_timeline_clip(
        &mut project,
        "math-1",
        TrimEdge::Right,
        -60,
        LinkMode::Single,
    )
    .unwrap();
    assert_eq!(project.timeline_clips[0].source_fps, fps(60, 1));
    assert_eq!(
        project.timeline_clips[0].math_animation,
        write(90),
        "30 fps の 45 frame は 60 fps で 90 frame（1.5 秒）"
    );
}
