//! 複数 track の Project の試験。C++ 版 tests/harness/test_two_track_project.cpp を移植したもの。
//! file への保存と読み込みは platform crate を作る段階で扱うので、ここでは JSON の文字列で検査する。

// 試験の失敗は大きく落ちてよい（helper 関数も試験の一部）。Project::default() から field を足す書き方は意図的。
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::field_reassign_with_default
)]

mod common;

use std::path::{Path, PathBuf};

use common::*;
use mvm_project::json::{parse_project_json, serialize_project_json};
use mvm_project::timeline::*;
use mvm_project::*;
use serde_json::{Value, json};

/// 60fps・100 frame の素材を丸ごと使う clip。kind は track の種類に合わせる。
fn clip(id: &str, track: TrackRef, start: i64) -> TimelineClip {
    TimelineClip {
        kind: if track.kind == TrackKind::Audio {
            TimelineClipKind::Audio
        } else {
            TimelineClipKind::Video
        },
        media_path: PathBuf::from(format!("{id}.mp4")),
        name: id.to_owned(),
        id: id.to_owned(),
        source_fps: fps(60, 1),
        source_frame_count: 100,
        source_out_frame: 100,
        timeline_start_frame: start,
        track,
        ..TimelineClip::default()
    }
}

fn with_clips(clips: Vec<TimelineClip>) -> Project {
    let mut project = Project::default();
    project.timeline_clips = clips;
    project
}

fn ids_at(project: &Project, kind: TrackKind, frame: i64) -> Vec<Option<&str>> {
    active_clips_at(project, kind, frame)
        .into_iter()
        .map(|index| index.map(|index| project.timeline_clips[index].id.as_str()))
        .collect()
}

fn arbitrary() -> Project {
    with_clips(vec![
        clip("late", V1, 300),
        clip("overlay", V2, 50),
        clip("early", V1, 0),
    ])
}

#[test]
fn validation_ignores_vector_order() {
    let project = arbitrary();
    assert_eq!(
        validate_timeline(&project).unwrap().total_frames,
        400,
        "任意の start と gap"
    );
    let mut negative = project.clone();
    negative.timeline_clips[0].timeline_start_frame = -1;
    assert!(validate_timeline(&negative).is_err(), "負の start");
    let mut invalid_track = project;
    invalid_track.timeline_clips[0].track = V3;
    assert!(
        validate_timeline(&invalid_track).is_err(),
        "存在しない track"
    );

    assert!(
        validate_timeline(&with_clips(vec![clip("a", V1, 0), clip("b", V1, 99)])).is_err(),
        "同じ track の重なり"
    );
    validate_timeline(&with_clips(vec![clip("a", V1, 0), clip("b", V2, 50)]))
        .expect("track 間の重なりは可");
    validate_timeline(&with_clips(vec![clip("a", V1, 0), clip("voice", A1, 50)]))
        .expect("video と audio の重なりは可");
    validate_timeline(&with_clips(vec![clip("a", V1, 0), clip("b", V1, 100)]))
        .expect("終端と始端が接するのは可");
}

#[test]
fn active_clip_lookup() {
    let project = arbitrary();
    assert_eq!(
        ids_at(&project, TrackKind::Video, 75),
        vec![Some("early"), Some("overlay")],
        "V1 と V2 を同時に引く"
    );
    assert_eq!(
        active_clip_at(&project, V2, 150),
        None,
        "半開区間の終端は active でない"
    );
    assert_eq!(
        ids_at(&project, TrackKind::Video, 200),
        vec![None, None],
        "gap"
    );
    assert_eq!(
        timeline_end_frame(&project).unwrap(),
        400,
        "全 track の最大終端"
    );
    let top_ends_last = with_clips(vec![clip("bottom", V1, 0), clip("top", V2, 500)]);
    assert_eq!(
        timeline_end_frame(&top_ends_last).unwrap(),
        600,
        "上の track が最後に終わる"
    );

    // track を 3 本以上へ増やしても同じ規則で扱う。
    let mut many = Project::default();
    add_track(&mut many, TrackKind::Video).unwrap();
    many.timeline_clips = vec![clip("v1", V1, 0), clip("v2", V2, 0), clip("v3", V3, 0)];
    validate_timeline(&many).unwrap();
    assert_eq!(ids_at(&many, TrackKind::Video, 10)[2], Some("v3"));
}

#[test]
fn move_is_transactional() {
    let mut first = clip("first", V1, 0);
    first.source_in_frame = 10;
    first.source_out_frame = 90;
    first.effects.position_x_percent = 12.5;
    first.effects.scale_x_percent = 60.0;
    first.effects.scale_y_percent = 60.0;
    first.effects.opacity_percent = 55.0;
    first.effects.fade_in_frames = 7;
    let mut project = with_clips(vec![first.clone(), clip("second", V1, 200)]);

    move_clip(&mut project, "first", V1, 100).unwrap();
    let expected = TimelineClip {
        timeline_start_frame: 100,
        ..first
    };
    assert_eq!(
        project.timeline_clips[0], expected,
        "水平移動で ID・trim・effects・素材情報を変えない"
    );
    let before = project.clone();
    assert!(
        move_clip(&mut project, "first", V1, 150).is_err(),
        "同じ track へ重なる移動"
    );
    assert_eq!(project, before, "拒否した移動は変えない");
    move_clip(&mut project, "first", V2, 200).unwrap();
    assert_eq!(project.timeline_clips[0].track, V2);
    move_clip(&mut project, "first", V1, 100).unwrap();
    assert_eq!(project.timeline_clips[0].track, V1);
}

fn root() -> PathBuf {
    std::env::temp_dir().join("mvm-rs-multi-track-test")
}

fn multi_track(root: &Path) -> Project {
    let mut project = Project::default();
    add_track(&mut project, TrackKind::Video).unwrap();
    project.video_tracks[1].muted = true;
    project.audio_tracks[0].solo = true;
    let mut bottom = clip("bottom", V1, 120);
    let mut top = clip("top", V3, 40);
    let mut voice = clip("voice", A1, 5);
    bottom.media_path = root.join("bottom.mp4");
    top.media_path = root.join("top.mp4");
    voice.media_path = root.join("voice.wav");
    top.kind = TimelineClipKind::Manim;
    top.effects.position_y_percent = -18.0;
    top.effects.scale_x_percent = 60.0;
    top.effects.scale_y_percent = 60.0;
    top.effects.rotation_degrees = 22.0;
    top.effects.opacity_percent = 70.0;
    top.effects.crop_left_percent = 5.0;
    top.effects.fade_in_frames = 8;
    top.effects.fade_out_frames = 9;
    project.timeline_clips = vec![bottom, top, voice];
    attach_fixture_media(&mut project);
    project
}

#[test]
fn tracks_round_trip_through_json() {
    let root = root();
    let path = root.join("multi-track.mvm");
    let project = multi_track(&root);
    let text = serialize_project_json(&project, &path).unwrap();
    let value: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(value["format"], json!("mvm-project"));
    let clips = value["timeline_clips"].as_array().unwrap();
    assert!(
        clips
            .iter()
            .all(|clip| clip.get("track_kind").is_some() && clip.get("track_index").is_some()),
        "全 clip に track を書く"
    );
    assert_eq!(clips[1]["track_index"], json!(2));
    assert_eq!(clips[2]["track_kind"], json!("audio"));
    assert_eq!(value["video_tracks"][1]["muted"], json!(true));
    assert_eq!(value["audio_tracks"][0]["solo"], json!(true));
    let loaded = parse_project_json(&text, &path).unwrap();
    assert_eq!(
        loaded, project,
        "track 構成・mute・solo・任意の start・effects の往復"
    );
}

// track の配列と clip の track_kind / track_index は必須。欠けた file を既定値で読まない。
#[test]
fn track_fields_are_required() {
    let root = root();
    let path = root.join("multi-track.mvm");
    let text = serialize_project_json(&multi_track(&root), &path).unwrap();
    let base: Value = serde_json::from_str(&text).unwrap();
    let loads =
        |value: &Value| parse_project_json(&serde_json::to_string(value).unwrap(), &path).is_ok();
    assert!(loads(&base), "対照: 保存したままの JSON は読める");
    let mutated = |change: &dyn Fn(&mut serde_json::Map<String, Value>)| {
        let mut value = base.clone();
        change(value.as_object_mut().unwrap());
        value
    };
    let clip0 = |root: &mut serde_json::Map<String, Value>| -> serde_json::Map<String, Value> {
        root["timeline_clips"][0].as_object().unwrap().clone()
    };
    for key in ["track_kind", "track_index"] {
        let value = mutated(&|root| {
            let mut clip = clip0(root);
            clip.remove(key);
            root["timeline_clips"][0] = Value::Object(clip);
        });
        assert!(!loads(&value), "clip の {key} の欠落");
    }
    assert!(
        !loads(&mutated(
            &|root| root["timeline_clips"][0]["track_index"] = json!(3)
        )),
        "存在しない track index"
    );
    for key in ["video_tracks", "audio_tracks"] {
        assert!(
            !loads(&mutated(&|root| drop(root.remove(key)))),
            "{key} の欠落"
        );
    }
    // 先頭の track は V1（video）。
    assert!(
        !loads(&mutated(&|root| drop(
            root["video_tracks"][0]
                .as_object_mut()
                .unwrap()
                .remove("solo")
        ))),
        "solo の無い track"
    );
    assert!(
        !loads(&mutated(
            &|root| root["video_tracks"][0]["solo"] = json!(true)
        )),
        "video track の solo"
    );
    assert!(
        !loads(&mutated(&|root| root["schema_version"] = json!(2))),
        "古い schema を黙って読まない"
    );
}

// メモリ上で video track の solo を立てても、検証と保存で止める（読めない file を書かない）。
#[test]
fn video_solo_is_rejected_before_saving() {
    let root = root();
    let mut project = multi_track(&root);
    project.video_tracks[0].solo = true;
    assert!(validate_timeline(&project).is_err());
    assert!(serialize_project_json(&project, &root.join("multi-track.mvm")).is_err());
}
