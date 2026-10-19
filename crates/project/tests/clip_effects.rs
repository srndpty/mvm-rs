//! effect と keyframe の試験。C++ 版 tests/harness/test_clip_effects.cpp の assertion を移植したもの。
//! 期待値は手で計算した値であり、実装の式を呼んで作らない（往復・不変量の比較を除く）。

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
use mvm_project::clip_effects::*;
use mvm_project::json::{parse_project_json, serialize_project_json};
use mvm_project::timeline::*;
use mvm_project::*;
use serde_json::{Value, json};

fn key(frame: i64, value: f64) -> ClipKeyframe {
    ClipKeyframe::new(frame, value)
}

fn shaped(
    frame: i64,
    value: f64,
    interpolation: KeyInterpolation,
    start: f64,
    end: f64,
    c1: f64,
    c2: f64,
) -> ClipKeyframe {
    ClipKeyframe {
        frame,
        value,
        interpolation,
        curve_start: start,
        curve_end: end,
        control1: c1,
        control2: c2,
    }
}

fn with(frame: i64, value: f64, interpolation: KeyInterpolation) -> ClipKeyframe {
    ClipKeyframe {
        interpolation,
        ..key(frame, value)
    }
}

fn same_curve(
    actual: &[ClipKeyframe],
    expected: &[ClipKeyframe],
    frames: std::ops::RangeInclusive<i64>,
) -> bool {
    frames.into_iter().all(|frame| {
        (evaluate_clip_keys(actual, 0.0, frame) - evaluate_clip_keys(expected, 0.0, frame)).abs()
            < 1e-9
    })
}

/// Manim の 1 clip（素材 120 frame の [10, 110)）を V1 に置いた Project。
fn project_with_clip() -> Project {
    let mut project = Project::default();
    project.timeline_clips.push(TimelineClip {
        kind: TimelineClipKind::Manim,
        media_path: PathBuf::from("clip.mp4"),
        name: "clip".to_owned(),
        id: "clip-1".to_owned(),
        source_fps: fps(60, 1),
        source_frame_count: 120,
        source_in_frame: 10,
        source_out_frame: 110,
        ..TimelineClip::default()
    });
    project
}

#[test]
fn interpolation_trim_and_insert_keep_the_curve() {
    for (interpolation, expected) in [
        (KeyInterpolation::Linear, 25.0),
        (KeyInterpolation::EaseIn, 6.25),
        (KeyInterpolation::EaseOut, 43.75),
        (KeyInterpolation::EaseInOut, 15.625),
    ] {
        let original = vec![with(0, 0.0, interpolation), key(100, 100.0)];
        assert!(
            (evaluate_clip_keys(&original, 0.0, 25) - expected).abs() < 1e-9,
            "{interpolation:?}"
        );
        let mut trimmed = original.clone();
        reframe_clip_keys(&mut trimmed, 101, 51, 25);
        for frame in 0..51 {
            assert!(
                (evaluate_clip_keys(&trimmed, 0.0, frame)
                    - evaluate_clip_keys(&original, 0.0, frame + 25))
                .abs()
                    < 1e-9,
                "トリムで動きを保つ"
            );
        }
        let mut inserted = original.clone();
        insert_clip_key(&mut inserted, 25, expected);
        assert!(
            same_curve(&inserted, &original, 0..=100),
            "key の追加で曲線を保つ"
        );
    }
}

#[test]
fn spline_keys() {
    let original = vec![
        shaped(0, 0.0, KeyInterpolation::Spline, 0.0, 1.0, 0.2, 0.8),
        key(100, 100.0),
    ];
    assert!((evaluate_clip_keys(&original, 0.0, 25) - 21.25).abs() < 1e-9);
    let mut inserted = original.clone();
    insert_clip_key(&mut inserted, 25, 21.25);
    assert!(same_curve(&inserted, &original, 0..=100));
    let mut spline = original.clone();
    reframe_clip_keys(&mut spline, 101, 51, 25);
    for frame in 0..51 {
        assert!(
            (evaluate_clip_keys(&spline, 0.0, frame)
                - evaluate_clip_keys(&original, 0.0, frame + 25))
            .abs()
                < 1e-9
        );
    }
    // 切り出した区間のハンドルが表示曲線と一致する。
    let (control1, control2) = clip_key_spline_controls(&spline[0]);
    let mut restricted = spline.clone();
    restricted[0].control1 = control1;
    restricted[0].control2 = control2;
    restricted[0].curve_start = 0.0;
    restricted[0].curve_end = 1.0;
    assert!(same_curve(&restricted, &spline, 0..=50));

    let mut effects = ClipEffects::default();
    effects.position_x_keys = vec![
        shaped(0, -20.0, KeyInterpolation::Spline, 0.0, 1.0, 0.2, 0.8),
        key(99, 20.0),
    ];
    let mut invalid = effects.clone();
    invalid.position_x_keys[0].control1 = -0.1;
    assert!(
        validate_effect_keys(&invalid, 100, false).is_err(),
        "範囲外の制御値"
    );
    invalid.position_x_keys[0].control1 = f64::NAN;
    assert!(
        validate_effect_keys(&invalid, 100, false).is_err(),
        "非数の制御値"
    );
    let mut crop = ClipEffects::default();
    crop.crop_left_keys = vec![
        shaped(0, 0.0, KeyInterpolation::Spline, 0.0, 1.0, 1.0, 1.0),
        key(99, 80.0),
    ];
    crop.crop_right_keys = vec![
        shaped(0, 80.0, KeyInterpolation::Spline, 0.0, 1.0, 0.0, 0.0),
        key(99, 0.0),
    ];
    assert!(
        validate_effect_keys(&crop, 100, false).is_err(),
        "キー間で不正になるクロップ"
    );
}

// 曲線上への key の追加と、同じ key の削除を続けると元の曲線へ戻る（追加の逆）。
#[test]
fn removing_inserted_keys_restores_the_curve() {
    for interpolation in KeyInterpolation::ALL {
        for (start, end) in [(0.0, 1.0), (0.25, 0.7)] {
            let original = vec![
                shaped(0, 0.0, interpolation, start, end, 0.2, 0.8),
                key(100, 100.0),
            ];
            let mut keys = original.clone();
            insert_clip_key(&mut keys, 25, evaluate_clip_keys(&original, 0.0, 25));
            assert!(keys.len() == 3 && same_curve(&keys, &original, 0..=100));
            assert_eq!(remove_clip_keys(&mut keys, &[25]), 1);
            assert_eq!(keys, original, "追加した key の削除で元へ戻る");
            insert_clip_key(&mut keys, 25, evaluate_clip_keys(&original, 0.0, 25));
            insert_clip_key(&mut keys, 60, evaluate_clip_keys(&original, 0.0, 60));
            assert!(keys.len() == 4 && same_curve(&keys, &original, 0..=100));
            let mut one = keys.clone();
            assert_eq!(remove_clip_keys(&mut one, &[25]), 1);
            assert!(one.len() == 3 && same_curve(&one, &original, 0..=100));
            assert_eq!(remove_clip_keys(&mut keys, &[60, 25]), 2);
            assert_eq!(keys, original, "連続 key の一括削除で元へ戻る");
        }
    }
    // EaseInOut 0 → 100 の f50 は 50。f25 の追加・削除後も 50。
    let mut ease = vec![with(0, 0.0, KeyInterpolation::EaseInOut), key(100, 100.0)];
    insert_clip_key(&mut ease, 25, 15.625);
    remove_clip_keys(&mut ease, &[25]);
    assert!((evaluate_clip_keys(&ease, 0.0, 50) - 50.0).abs() < 1e-9);
    // 別々に作った区間（連続部分でない）は結合しない。
    let mut separate = vec![
        with(0, 0.0, KeyInterpolation::EaseIn),
        with(50, 50.0, KeyInterpolation::EaseOut),
        key(100, 100.0),
    ];
    assert_eq!(remove_clip_keys(&mut separate, &[50]), 1);
    assert_eq!(separate.len(), 2);
    assert_eq!((separate[0].curve_start, separate[0].curve_end), (0.0, 1.0));
    assert_eq!(remove_clip_keys(&mut separate, &[42]), 0);
}

#[test]
fn motion_and_crop_validation() {
    let mut motion = ClipEffects::default();
    motion.position_x_keys = vec![key(0, -100.0), key(100, 100.0)];
    motion.rotation_keys = vec![key(0, -180.0), key(100, 180.0)];
    validate_effect_keys(&motion, 101, false).unwrap();
    let evaluated = motion.evaluated_at(25);
    assert_eq!(
        (evaluated.position_x_percent, evaluated.rotation_degrees),
        (-50.0, -90.0)
    );
    assert!(
        validate_effect_keys(&motion, 101, true).is_err(),
        "音声へモーションを適用しない"
    );
    motion.position_x_keys.push(key(100, 1.0));
    assert!(
        validate_effect_keys(&motion, 101, false).is_err(),
        "重複 key"
    );
    let mut crop = ClipEffects::default();
    crop.crop_left_keys = vec![with(0, 0.0, KeyInterpolation::EaseOut), key(100, 80.0)];
    crop.crop_right_keys = vec![with(0, 80.0, KeyInterpolation::EaseIn), key(100, 0.0)];
    assert!(
        validate_effect_keys(&crop, 101, false).is_err(),
        "端は 80% でも途中が 120% になる"
    );
    crop.crop_left_keys[1].value = 40.0;
    crop.crop_right_keys[0].value = 40.0;
    validate_effect_keys(&crop, 101, false).unwrap();
}

// 分割・trim の後も、全 frame で元のモーションを保つ。レート調整は端から端へ伸縮する。
#[test]
fn motion_survives_split_trim_and_rate_stretch() {
    let mut original = project_with_clip();
    for channel in EFFECT_CHANNELS
        .iter()
        .filter(|channel| !channel.kind.is_audio())
    {
        let crop = channel.kind >= ClipKeyKind::CropLeft;
        let (from, to) = if crop { (2.0, 12.0) } else { (10.0, 80.0) };
        *original.timeline_clips[0].effects.keys_mut(channel.kind) =
            vec![with(0, from, KeyInterpolation::EaseInOut), key(99, to)];
    }
    let check_remaining = |edited: &Project| {
        for clip in &edited.timeline_clips {
            for frame in 0..duration(edited, clip) {
                let before = original.timeline_clips[0]
                    .effects
                    .evaluated_at(clip.timeline_start_frame + frame);
                let after = clip.effects.evaluated_at(frame);
                for channel in &EFFECT_CHANNELS {
                    assert!((before.base(channel.kind) - after.base(channel.kind)).abs() < 1e-8);
                }
            }
        }
    };
    let mut edited = original.clone();
    split_timeline_clips(
        &mut edited,
        &ids(&["clip-1"]),
        37,
        &mut || "split-motion".to_owned(),
        LinkMode::Single,
    )
    .unwrap();
    check_remaining(&edited);
    let mut edited = original.clone();
    trim_timeline_clip(&mut edited, "clip-1", TrimEdge::Left, 23, LinkMode::Single).unwrap();
    check_remaining(&edited);
    let mut edited = original.clone();
    trim_timeline_clip(
        &mut edited,
        "clip-1",
        TrimEdge::Right,
        -29,
        LinkMode::Single,
    )
    .unwrap();
    check_remaining(&edited);
    let mut edited = original.clone();
    rate_stretch_timeline_clip(
        &mut edited,
        "clip-1",
        TrimEdge::Right,
        100,
        LinkMode::Single,
    )
    .unwrap();
    for channel in EFFECT_CHANNELS
        .iter()
        .filter(|channel| !channel.kind.is_audio())
    {
        let keys = edited.timeline_clips[0].effects.keys(channel.kind);
        assert_eq!(keys.len(), 2);
        assert_eq!(keys[1].frame, 199);
        assert_eq!(keys[0].interpolation, KeyInterpolation::EaseInOut);
    }
}

#[test]
fn default_detection_counts_keys() {
    assert!(ClipEffects::default().is_default());
    // preview / 書き出しは default の clip で effect の処理ごと省く。key だけの clip を落とさない。
    let opacity = ClipEffects {
        opacity_keys: vec![key(0, 100.0), key(10, 0.0)],
        ..ClipEffects::default()
    };
    assert!(!opacity.is_default());
    let volume = ClipEffects {
        volume_keys: vec![key(5, 100.0)],
        ..ClipEffects::default()
    };
    assert!(!volume.is_default());
    let level = ClipEffects {
        volume_percent: 50.0,
        ..ClipEffects::default()
    };
    assert!(!level.is_default());
}

// 音量の 1dB 刻み。期待値は 10^(±1/20) を独立に計算した値。
#[test]
fn volume_steps_in_decibels() {
    let near = |value: Option<f64>, expected: f64| {
        value.is_some_and(|value| (value - expected).abs() < 1e-9)
    };
    assert!(near(
        step_volume_percent_by_db(100.0, 1.0),
        112.20184543019634
    ));
    assert!(near(
        step_volume_percent_by_db(100.0, -1.0),
        89.12509381337456
    ));
    assert!(
        near(step_volume_percent_by_db(0.0, 1.0), 0.11220184543019634),
        "0% から上げると下限から +1dB"
    );
    assert!(near(step_volume_percent_by_db(0.0, -1.0), 0.0));
    assert!(near(step_volume_percent_by_db(0.1, -1.0), 0.1));
    assert!(near(step_volume_percent_by_db(0.105, -1.0), 0.1));
    assert!(near(step_volume_percent_by_db(190.0, 1.0), 200.0));
    assert!(near(step_volume_percent_by_db(200.0, 1.0), 200.0));
    for (percent, step) in [
        (-1.0, 1.0),
        (201.0, -1.0),
        (f64::NAN, 1.0),
        (100.0, 0.0),
        (100.0, f64::NAN),
    ] {
        assert_eq!(step_volume_percent_by_db(percent, step), None);
    }
}

fn combined_effects() -> ClipEffects {
    ClipEffects {
        crop_left_percent: 10.0,
        crop_top_percent: 20.0,
        crop_right_percent: 30.0,
        crop_bottom_percent: 10.0,
        scale_x_percent: 60.0,
        scale_y_percent: 60.0,
        position_x_percent: 15.0,
        position_y_percent: -5.0,
        rotation_degrees: 25.0,
        opacity_percent: 55.0,
        fade_in_frames: 12,
        fade_out_frames: 18,
        ..ClipEffects::default()
    }
}

#[test]
fn effects_map_to_normalized_rects() {
    ClipEffects::default().validate(100).unwrap();
    let effects = combined_effects();
    effects.validate(100).unwrap();
    let mapping = map_clip_effects(&effects);
    assert!(
        (mapping.source_rect.x - 0.1).abs() < 1e-9
            && (mapping.source_rect.width - 0.6).abs() < 1e-9
    );
    assert!(
        (mapping.destination_rect.width - 0.36).abs() < 1e-9
            && (mapping.destination_rect.x - 0.37).abs() < 1e-9
    );
    assert!((mapping.base_opacity - 0.55).abs() < 1e-9 && mapping.fade_in_frames == 12);

    let invalid = |change: &dyn Fn(&mut ClipEffects)| {
        let mut changed = combined_effects();
        change(&mut changed);
        changed.validate(100)
    };
    assert!(
        invalid(&|e| e.crop_right_percent = 90.0).is_err(),
        "左右 crop 合計 100% 以上"
    );
    assert!(
        invalid(&|e| {
            e.fade_in_frames = 90;
            e.fade_out_frames = 20;
        })
        .is_err(),
        "fade の重複"
    );
    assert!(invalid(&|e| e.scale_x_percent = f64::NAN).is_err());
    for scale in [0.0, 0.5, 1000.5, f64::INFINITY] {
        assert!(
            invalid(&|e| e.scale_y_percent = scale).is_err(),
            "拡大率 Y {scale}"
        );
        assert!(
            invalid(&|e| e.scale_x_percent = scale).is_err(),
            "拡大率 X {scale}"
        );
    }
    // 縦横の拡大率は別々に効く。crop 範囲の中心は保ったまま、幅と高さを別の倍率にする。
    let stretched = ClipEffects {
        scale_x_percent: 150.0,
        scale_y_percent: 50.0,
        ..ClipEffects::default()
    };
    stretched.validate(100).unwrap();
    let rect = map_clip_effects(&stretched).destination_rect;
    assert!((rect.width - 1.5).abs() < 1e-9 && (rect.height - 0.5).abs() < 1e-9);
    assert!((rect.x + 0.25).abs() < 1e-9 && (rect.y - 0.25).abs() < 1e-9);
}

// preview の枠: 見えている矩形（出力画素）と、その逆（矩形 → 位置と拡大率）。
#[test]
fn visual_geometry_round_trips() {
    let near = |a: f64, b: f64| (a - b).abs() < 1e-6;
    let full = clip_visual_geometry(&ClipEffects::default(), 1920, 1080, 1920, 1080).unwrap();
    assert!(
        near(full.x, 0.0)
            && near(full.y, 0.0)
            && near(full.width, 1920.0)
            && near(full.height, 1080.0)
    );
    assert!(near(full.pivot_x, 960.0));
    // 4:3 素材は左右に 240px の余白があり、枠は素材の見えている範囲だけを囲む。
    let pillar = clip_visual_geometry(&ClipEffects::default(), 1440, 1080, 1920, 1080).unwrap();
    assert!(near(pillar.x, 240.0) && near(pillar.width, 1440.0));

    let mut moved = ClipEffects::default();
    set_effects_for_visual_rect(
        &mut moved, 1920, 1080, 1920, 1080, 100.0, 50.0, 960.0, 270.0,
    )
    .unwrap();
    assert!(near(moved.scale_x_percent, 50.0) && near(moved.scale_y_percent, 25.0));
    let geometry = clip_visual_geometry(&moved, 1920, 1080, 1920, 1080).unwrap();
    assert!(
        near(geometry.x, 100.0)
            && near(geometry.y, 50.0)
            && near(geometry.width, 960.0)
            && near(geometry.height, 270.0)
    );

    // crop・回転・letterbox があっても往復する。回転と crop は変えない。
    let complex = ClipEffects {
        crop_left_percent: 5.0,
        crop_top_percent: 10.0,
        crop_right_percent: 20.0,
        rotation_degrees: 30.0,
        scale_x_percent: 80.0,
        scale_y_percent: 120.0,
        ..ClipEffects::default()
    };
    assert!(clip_visual_geometry(&complex, 1440, 1080, 1920, 1080).is_some());
    let mut rebuilt = complex.clone();
    set_effects_for_visual_rect(
        &mut rebuilt,
        1440,
        1080,
        1920,
        1080,
        300.0,
        200.0,
        700.0,
        500.0,
    )
    .unwrap();
    let after = clip_visual_geometry(&rebuilt, 1440, 1080, 1920, 1080).unwrap();
    assert!(
        near(after.x, 300.0)
            && near(after.y, 200.0)
            && near(after.width, 700.0)
            && near(after.height, 500.0)
    );
    assert!(near(rebuilt.rotation_degrees, 30.0) && near(rebuilt.crop_left_percent, 5.0));
    rebuilt.validate(100).unwrap();

    // 大きさ 0 や NaN の枠、寸法の不正は受け付けず effects を変えない。
    let mut unchanged = complex.clone();
    assert!(
        set_effects_for_visual_rect(&mut unchanged, 1440, 1080, 1920, 1080, 0.0, 0.0, 0.0, 100.0)
            .is_err()
    );
    assert!(
        set_effects_for_visual_rect(
            &mut unchanged,
            1440,
            1080,
            1920,
            1080,
            f64::NAN,
            0.0,
            10.0,
            10.0
        )
        .is_err()
    );
    assert!(
        set_effects_for_visual_rect(&mut unchanged, 0, 1080, 1920, 1080, 0.0, 0.0, 10.0, 10.0)
            .is_err()
    );
    assert_eq!(unchanged, complex);
    // 余白だけの crop は何も見えない。16:9 素材なら見えている（対照）。
    let margin_only = ClipEffects {
        crop_right_percent: 95.0,
        ..ClipEffects::default()
    };
    assert!(clip_visual_geometry(&margin_only, 1440, 1080, 1920, 1080).is_none());
    assert!(clip_visual_geometry(&margin_only, 1920, 1080, 1920, 1080).is_some());
}

fn json_path() -> PathBuf {
    std::env::temp_dir()
        .join("mvm-rs-effects-test")
        .join("effects.mvm")
}

fn effects_project() -> Project {
    let mut project = project_with_clip();
    let mut effects = combined_effects();
    effects.scale_y_percent = 45.0; // X と違う値でも往復すること
    effects.opacity_keys = vec![
        shaped(
            20,
            80.0,
            KeyInterpolation::EaseInOut,
            0.1,
            0.9,
            1.0 / 3.0,
            2.0 / 3.0,
        ),
        key(80, 20.123456789123),
    ];
    effects.position_x_keys = vec![
        shaped(0, -20.0, KeyInterpolation::Spline, 0.0, 1.0, 0.2, 0.8),
        key(90, 30.0),
    ];
    effects.rotation_keys = vec![key(0, -45.0), key(90, 90.0)];
    project.timeline_clips[0].effects = effects;
    project
}

#[test]
fn effects_round_trip_through_json() {
    let project = effects_project();
    let text = serialize_project_json(&project, &json_path()).unwrap();
    let loaded = parse_project_json(&text, &json_path()).unwrap();
    assert_eq!(
        loaded.timeline_clips[0].effects,
        project.timeline_clips[0].effects
    );
}

#[test]
fn effects_json_rejects_broken_keys() {
    let text = serialize_project_json(&effects_project(), &json_path()).unwrap();
    let base: Value = serde_json::from_str(&text).unwrap();
    let rejected = |value: &Value| {
        parse_project_json(&serde_json::to_string(value).unwrap(), &json_path()).is_err()
    };
    // 対照: 保存したままの JSON は読める。
    assert!(!rejected(&base));
    fn effects(value: &mut Value) -> &mut Value {
        &mut value["timeline_clips"][0]["effects"]
    }
    // spline の制御値の欠落。
    let mut missing_control = base.clone();
    effects(&mut missing_control)["position_x_keys"][0]
        .as_object_mut()
        .unwrap()
        .remove("control1");
    assert!(rejected(&missing_control));
    // 縦横共通の拡大率（scale_percent）は読み替えずに拒否する。
    let mut uniform = base.clone();
    let object = effects(&mut uniform).as_object_mut().unwrap();
    let scale = object.remove("scale_x_percent").unwrap();
    object.insert("scale_percent".to_owned(), scale);
    assert!(rejected(&uniform));
    // 同じ frame の key。
    let mut duplicate = base.clone();
    effects(&mut duplicate)["opacity_keys"][1]["frame"] = json!(20);
    assert!(rejected(&duplicate));
    // effects object の一部だけ。
    let mut partial = base.clone();
    effects(&mut partial)
        .as_object_mut()
        .unwrap()
        .retain(|key, _| key == "scale_x_percent");
    assert!(rejected(&partial));
}

// 速度（speed_num / speed_den）は必須。約分済みの 10%〜1000% だけを受理する。
#[test]
fn clip_speed_in_json() {
    let mut project = Project::default();
    let mut half = clip("a");
    half.source_frame_count = 10;
    half.source_out_frame = 10;
    half.speed = fps(1, 2);
    project.timeline_clips = vec![half];
    attach_fixture_media(&mut project);
    let base: Value =
        serde_json::from_str(&serialize_project_json(&project, &json_path()).unwrap()).unwrap();
    let load = |num: Option<i64>, den: Option<i64>| {
        let mut value = base.clone();
        let clip = value["timeline_clips"][0].as_object_mut().unwrap();
        match (num, den) {
            (Some(num), Some(den)) => {
                clip.insert("speed_num".to_owned(), json!(num));
                clip.insert("speed_den".to_owned(), json!(den));
            }
            _ => {
                clip.remove("speed_num");
                clip.remove("speed_den");
            }
        }
        parse_project_json(&serde_json::to_string(&value).unwrap(), &json_path())
    };
    assert_eq!(
        load(Some(1), Some(2)).unwrap().timeline_clips[0].speed,
        fps(1, 2),
        "対照群: 50%"
    );
    assert!(
        load(None, None).is_err(),
        "速度の無い clip を既定値で受理しない"
    );
    assert!(load(Some(1), Some(11)).is_err(), "10% 未満");
    assert!(load(Some(11), Some(1)).is_err(), "1000% 超");
    assert!(
        load(Some(10), Some(1)).is_ok() && load(Some(1), Some(10)).is_ok(),
        "10% と 1000% ちょうど"
    );
    assert!(load(Some(2), Some(4)).is_err(), "約分されていない速度");
}

// fps の違う Project へ移した key は同じ秒位置へ来る。
#[test]
fn retimed_keys_keep_seconds() {
    let frames = |keys: &[ClipKeyframe]| {
        keys.iter()
            .map(|key| (key.frame, key.value))
            .collect::<Vec<_>>()
    };
    let mut keys = vec![key(0, 100.0), key(30, 50.0), key(60, 20.0), key(119, 0.0)];
    retime_clip_keys(&mut keys, fps(60, 1), fps(30, 1), 60).unwrap();
    assert_eq!(
        frames(&keys),
        vec![(0, 100.0), (15, 50.0), (30, 20.0), (59, 0.0)]
    );
    retime_clip_keys(&mut keys, fps(30, 1), fps(60, 1), 120).unwrap();
    assert_eq!(
        frames(&keys),
        vec![(0, 100.0), (30, 50.0), (60, 20.0), (118, 0.0)]
    );
    // 同じ frame に重なった key は先の 1 つを残し、ちょうど 1/2 は後ろへ丸める。
    let mut keys = vec![key(9, 10.0), key(10, 20.0), key(11, 30.0)];
    retime_clip_keys(&mut keys, fps(60, 1), fps(30, 1), 60).unwrap();
    assert_eq!(frames(&keys), vec![(5, 10.0), (6, 30.0)]);
    let mut keys = vec![key(30, 40.0)];
    retime_clip_keys(&mut keys, fps(30000, 1001), fps(30, 1), 60).unwrap();
    assert_eq!(frames(&keys), vec![(30, 40.0)]);
    let original = vec![key(3, 50.0)];
    let mut keys = original.clone();
    assert!(retime_clip_keys(&mut keys, fps(0, 1), fps(30, 1), 60).is_err());
    assert_eq!(keys, original, "不正な fps では失敗し、key を変えない");
}

#[test]
fn effect_channels_follow_key_kind_order() {
    for (index, channel) in EFFECT_CHANNELS.iter().enumerate() {
        assert_eq!(
            channel.kind as usize, index,
            "EFFECT_CHANNELS は ClipKeyKind の宣言順"
        );
        assert_eq!(ClipKeyKind::from_name(channel.name), Some(channel.kind));
    }
}

/// 60fps・300 frame の映像 clip を 100 frame から置いた Project。
fn motion_project() -> Project {
    let mut project = Project::default();
    let mut placed = clip("v");
    placed.timeline_start_frame = 100;
    project.timeline_clips = vec![placed];
    project
}

#[test]
fn the_edit_frame_is_the_playhead_inside_the_clip_and_its_start_outside() {
    let project = motion_project();
    assert_eq!(effect_edit_frame(&project, "id-v", 130).unwrap(), 30);
    assert_eq!(effect_edit_frame(&project, "id-v", 100).unwrap(), 0);
    assert_eq!(effect_edit_frame(&project, "id-v", 399).unwrap(), 299);
    assert_eq!(effect_edit_frame(&project, "id-v", 400).unwrap(), 0);
    assert_eq!(effect_edit_frame(&project, "id-v", 50).unwrap(), 0);
    assert!(effect_edit_frame(&project, "無い", 0).is_err());
}

#[test]
fn an_unanimated_value_changes_the_fixed_value() {
    let mut project = motion_project();
    set_clip_effect_at(&mut project, "id-v", ClipKeyKind::PositionX, 30, 25.0).unwrap();
    let effects = &project.clip("id-v").unwrap().effects;
    assert_eq!(effects.position_x_percent, 25.0);
    assert!(effects.position_x_keys.is_empty());
}

#[test]
fn an_animated_value_sets_the_key_at_the_edit_frame() {
    let mut project = motion_project();
    project.timeline_clips[0].effects.scale_x_keys = vec![key(0, 100.0), key(60, 200.0)];
    // 既存のキーの位置なら値だけを変える。
    set_clip_effect_at(&mut project, "id-v", ClipKeyKind::ScaleX, 60, 150.0).unwrap();
    // キーの無い位置ならキーを足す。固定値は変えない。
    set_clip_effect_at(&mut project, "id-v", ClipKeyKind::ScaleX, 30, 120.0).unwrap();
    let effects = &project.clip("id-v").unwrap().effects;
    let keys: Vec<_> = effects
        .scale_x_keys
        .iter()
        .map(|key| (key.frame, key.value))
        .collect();
    assert_eq!(keys, [(0, 100.0), (30, 120.0), (60, 150.0)]);
    assert_eq!(effects.scale_x_percent, 100.0);
}

#[test]
fn effect_values_outside_the_channel_or_the_clip_kind_are_rejected() {
    let mut project = motion_project();
    let before = project.clone();
    assert!(set_clip_effect_at(&mut project, "id-v", ClipKeyKind::ScaleX, 0, 0.5).is_err());
    assert!(set_clip_effect_at(&mut project, "id-v", ClipKeyKind::Rotation, 0, f64::NAN).is_err());
    assert!(set_clip_effect_at(&mut project, "id-v", ClipKeyKind::Volume, 0, 50.0).is_err());
    // 左右のクロップの合計が 100% 以上の映像は残らないので、検証で断る。
    set_clip_effect_at(&mut project, "id-v", ClipKeyKind::CropLeft, 0, 60.0).unwrap();
    assert!(set_clip_effect_at(&mut project, "id-v", ClipKeyKind::CropRight, 0, 50.0).is_err());
    assert_eq!(
        project.clip("id-v").unwrap().effects.crop_right_percent,
        0.0
    );
    assert_ne!(project, before);
}

#[test]
fn toggling_a_key_adds_the_current_value_and_removing_the_last_keeps_it() {
    let mut project = motion_project();
    project.timeline_clips[0].effects.rotation_keys = vec![key(0, 0.0), key(100, 90.0)];
    toggle_clip_key(&mut project, "id-v", ClipKeyKind::Rotation, 50).unwrap();
    let keys = &project.clip("id-v").unwrap().effects.rotation_keys;
    assert_eq!(keys.len(), 3);
    assert_eq!(keys[1].frame, 50);
    assert_eq!(keys[1].value, 45.0);
    toggle_clip_key(&mut project, "id-v", ClipKeyKind::Rotation, 50).unwrap();
    toggle_clip_key(&mut project, "id-v", ClipKeyKind::Rotation, 0).unwrap();
    toggle_clip_key(&mut project, "id-v", ClipKeyKind::Rotation, 100).unwrap();
    let effects = &project.clip("id-v").unwrap().effects;
    assert!(effects.rotation_keys.is_empty());
    assert_eq!(effects.rotation_degrees, 90.0);
}

#[test]
fn animation_on_starts_from_the_current_value_and_off_freezes_it() {
    let mut project = motion_project();
    project.timeline_clips[0].effects.position_y_percent = -10.0;
    set_clip_key_animation(&mut project, "id-v", ClipKeyKind::PositionY, 20, true).unwrap();
    let keys = &project.clip("id-v").unwrap().effects.position_y_keys;
    assert_eq!(keys.len(), 1);
    assert_eq!((keys[0].frame, keys[0].value), (20, -10.0));
    project.timeline_clips[0].effects.position_y_keys = vec![key(0, 0.0), key(100, 50.0)];
    set_clip_key_animation(&mut project, "id-v", ClipKeyKind::PositionY, 40, false).unwrap();
    let effects = &project.clip("id-v").unwrap().effects;
    assert!(effects.position_y_keys.is_empty());
    assert_eq!(effects.position_y_percent, 20.0);
}

#[test]
fn fades_are_limited_by_the_source_range() {
    let mut project = motion_project();
    set_clip_fades(&mut project, "id-v", 30, 60).unwrap();
    let effects = &project.clip("id-v").unwrap().effects;
    assert_eq!((effects.fade_in_frames, effects.fade_out_frames), (30, 60));
    assert!(set_clip_fades(&mut project, "id-v", 200, 200).is_err());
    assert!(set_clip_fades(&mut project, "id-v", -1, 0).is_err());
    assert_eq!(project.clip("id-v").unwrap().effects.fade_in_frames, 30);
}

#[test]
fn changing_the_interpolation_resets_the_partial_curve_once() {
    let mut project = motion_project();
    let first = shaped(0, 0.0, KeyInterpolation::Linear, 0.25, 0.75, 0.0, 0.0);
    project.timeline_clips[0].effects.opacity_keys = vec![first, key(100, 100.0)];
    // 同じ補間なら部分曲線を残す。
    set_clip_key_interpolation(
        &mut project,
        "id-v",
        ClipKeyKind::Opacity,
        0,
        KeyInterpolation::Linear,
    )
    .unwrap();
    let key0 = project.clip("id-v").unwrap().effects.opacity_keys[0];
    assert_eq!((key0.curve_start, key0.curve_end), (0.25, 0.75));
    set_clip_key_interpolation(
        &mut project,
        "id-v",
        ClipKeyKind::Opacity,
        0,
        KeyInterpolation::EaseIn,
    )
    .unwrap();
    let key0 = project.clip("id-v").unwrap().effects.opacity_keys[0];
    assert_eq!(key0.interpolation, KeyInterpolation::EaseIn);
    assert_eq!((key0.curve_start, key0.curve_end), (0.0, 1.0));
    assert!(
        set_clip_key_interpolation(
            &mut project,
            "id-v",
            ClipKeyKind::Opacity,
            50,
            KeyInterpolation::Spline
        )
        .is_err()
    );
}
