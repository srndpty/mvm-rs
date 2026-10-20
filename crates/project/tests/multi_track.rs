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

fn linked_pair(id: &str) -> (TimelineClip, TimelineClip) {
    let mut video = clip(id, V1, 0);
    let mut audio = clip(&format!("{id}-a"), A1, 0);
    video.link_group_id = format!("g-{id}");
    audio.link_group_id = format!("g-{id}");
    (video, audio)
}

#[test]
fn the_first_video_goes_to_v1_with_its_sound_on_a1() {
    let mut project = Project::default();
    let (video, audio) = linked_pair("v");
    place_video_media_at(&mut project, video, Some(audio), 30).unwrap();
    let placed = project.clip("v").unwrap();
    assert_eq!(placed.track, V1);
    assert_eq!(placed.timeline_start_frame, 30);
    let sound = project.clip("v-a").unwrap();
    assert_eq!(sound.track, A1);
    assert_eq!(sound.timeline_start_frame, 30);
    assert_eq!(project.video_tracks.len(), 2);
}

#[test]
fn an_overlapping_video_uses_the_free_v2_and_adds_an_audio_track_when_a1_is_busy() {
    let mut project = Project::default();
    let (first, first_audio) = linked_pair("v");
    place_video_media_at(&mut project, first, Some(first_audio), 0).unwrap();
    let (second, second_audio) = linked_pair("w");
    place_video_media_at(&mut project, second, Some(second_audio), 10).unwrap();
    // V1 は [0, 100) が使用中なので、空いている既定の V2 へ置く（track は足さない）。
    assert_eq!(project.clip("w").unwrap().track, TrackRef::video(1));
    assert_eq!(project.video_tracks.len(), 2);
    // A1 も使用中なので、足した A2 へ置く。
    assert_eq!(project.clip("w-a").unwrap().track, A2);
    assert_eq!(project.clip("v-a").unwrap().track, A1);
    assert_eq!(project.audio_tracks.len(), 2);

    // V1・V2 とも使用中の区間なら、そのときだけ V3 を足す。
    let (third, third_audio) = linked_pair("x");
    place_video_media_at(&mut project, third, Some(third_audio), 50).unwrap();
    assert_eq!(project.clip("x").unwrap().track, TrackRef::video(2));
    assert_eq!(project.video_tracks.len(), 3);
    assert_eq!(project.clip("x-a").unwrap().track, TrackRef::audio(2));
}

#[test]
fn a_video_after_the_existing_ones_reuses_v1_and_a1() {
    let mut project = Project::default();
    let (first, first_audio) = linked_pair("v");
    place_video_media_at(&mut project, first, Some(first_audio), 0).unwrap();
    let (second, second_audio) = linked_pair("w");
    // 最初の clip は [0, 100) なので、100 からなら V1・A1 が空いている。
    place_video_media_at(&mut project, second, Some(second_audio), 100).unwrap();
    assert_eq!(project.clip("w").unwrap().track, V1);
    assert_eq!(project.clip("w-a").unwrap().track, A1);
    assert_eq!(project.video_tracks.len(), 2);
    assert_eq!(project.audio_tracks.len(), 1);
}

#[test]
fn a_muted_track_is_skipped_when_placing_a_video() {
    let mut project = Project::default();
    project.video_tracks[0].muted = true;
    project.audio_tracks[0].muted = true;
    let (video, audio) = linked_pair("v");
    place_video_media_at(&mut project, video, Some(audio), 0).unwrap();
    assert_eq!(project.clip("v").unwrap().track, TrackRef::video(1));
    assert_eq!(project.clip("v-a").unwrap().track, A2);
}

#[test]
fn a_silent_video_does_not_add_an_audio_track() {
    let mut project = Project::default();
    project.audio_tracks.clear();
    place_video_media_at(&mut project, clip("v", V1, 0), None, 0).unwrap();
    assert!(project.audio_tracks.is_empty());
}

#[test]
fn only_video_clips_are_placed_as_video_media() {
    let mut project = Project::default();
    let before = project.clone();
    assert!(place_video_media_at(&mut project, clip("a", A1, 0), None, 0).is_err());
    assert!(place_video_media_at(&mut project, clip("v", V1, 0), None, -1).is_err());
    assert_eq!(project, before);
}

#[test]
fn ripple_delete_closes_the_selected_span_on_every_track() {
    let mut project = Project::default();
    project.timeline_clips = vec![
        clip("a", V1, 0),
        clip("b", V1, 100),
        clip("c", V1, 200),
        clip("s", A1, 150),
    ];
    ripple_delete_clips(&mut project, &["b".to_owned()]).unwrap();
    assert!(project.clip("b").is_none());
    assert_eq!(project.clip("c").unwrap().timeline_start_frame, 100);
    // 別の track の clip も同じ時間だけ詰める（[150, 200) は消えた区間に入るので削られる）。
    let sound = project.clip("s").unwrap();
    assert_eq!(sound.timeline_start_frame, 100);
    assert_eq!(sound.source_duration(), 50);
}

#[test]
fn ripple_delete_merges_touching_spans() {
    let mut project = Project::default();
    project.timeline_clips = vec![clip("a", V1, 0), clip("b", V1, 100), clip("c", V1, 200)];
    ripple_delete_clips(&mut project, &["a".to_owned(), "b".to_owned()]).unwrap();
    assert_eq!(project.timeline_clips.len(), 1);
    assert_eq!(project.clip("c").unwrap().timeline_start_frame, 0);
}

#[test]
fn ripple_delete_without_a_selection_changes_nothing() {
    let mut project = Project::default();
    project.timeline_clips = vec![clip("a", V1, 0)];
    let before = project.clone();
    assert!(ripple_delete_clips(&mut project, &["無い".to_owned()]).is_err());
    assert_eq!(project, before);
}

#[test]
fn project_settings_follow_the_media_with_square_pixels() {
    assert_eq!(
        video_settings_for_media(1920, 1080, fps(1, 1), fps(60, 1)).unwrap(),
        (1920, 1080, fps(60, 1))
    );
    // 4:3 の SAR を持つ 1440×1080 は 1920 幅。
    assert_eq!(
        video_settings_for_media(1440, 1080, fps(4, 3), fps(30000, 1001)).unwrap(),
        (1920, 1080, fps(30000, 1001))
    );
    // 720 × 10/11 = 654.5… は最も近い偶数の 654、奇数の高さは切り上げる。
    assert_eq!(
        video_settings_for_media(720, 481, fps(10, 11), fps(48, 2)).unwrap(),
        (654, 482, fps(24, 1))
    );
}

#[test]
fn project_settings_reject_unsupported_media() {
    assert!(video_settings_for_media(1920, 1080, fps(1, 1), fps(15, 1)).is_err());
    assert!(video_settings_for_media(0, 1080, fps(1, 1), fps(30, 1)).is_err());
    assert!(video_settings_for_media(40_000, 1080, fps(1, 1), fps(30, 1)).is_err());
    assert!(video_settings_for_media(1920, 1080, fps(0, 1), fps(30, 1)).is_err());
}

fn text_project() -> Project {
    let mut project = Project::default();
    place_text_clip(&mut project, V1, 0, "最初の文字", &mut || {
        "t".to_owned()
    })
    .unwrap();
    project
}

#[test]
fn editing_a_text_clip_renames_it_from_the_first_line() {
    let mut project = text_project();
    let mut text = project.clip("t").unwrap().text.clone();
    text.content = "一行目がとても長い見出しの文字列で二十四文字を超えています\n二行目".to_owned();
    text.font_size = 96;
    text.color = "#FFFF0000".to_owned();
    edit_text_clip(&mut project, "t", text).unwrap();
    let clip = project.clip("t").unwrap();
    assert_eq!(clip.text.font_size, 96);
    assert_eq!(
        clip.name,
        "一行目がとても長い見出しの文字列で二十四文字を超…"
    );
}

#[test]
fn a_text_clip_keeps_a_body_and_valid_style() {
    let mut project = text_project();
    let before = project.clone();
    let text = project.clip("t").unwrap().text.clone();
    let empty = TextClipData {
        content: "  ".to_owned(),
        ..text.clone()
    };
    assert!(edit_text_clip(&mut project, "t", empty).is_err());
    let bad_color = TextClipData {
        color: "red".to_owned(),
        ..text.clone()
    };
    assert!(edit_text_clip(&mut project, "t", bad_color).is_err());
    let off_screen = TextClipData {
        x: project.output_width,
        ..text.clone()
    };
    assert!(edit_text_clip(&mut project, "t", off_screen).is_err());
    assert!(edit_text_clip(&mut project, "無い", text).is_err());
    assert_eq!(project, before);
}

#[test]
fn a_broken_linked_pair_reports_its_own_error_without_a_new_track() {
    let mut project = Project::default();
    let (video, mut audio) = linked_pair("v");
    audio.link_group_id = "別のリンク".to_owned();
    let error = place_video_media_at(&mut project, video, Some(audio), 0).unwrap_err();
    // A1 は空いているので、track を足して試し直さずに元の原因を返す。
    assert!(
        error
            .to_string()
            .starts_with("リンクclipは同じlink IDを持つ"),
        "{error}"
    );
    assert_eq!(project.audio_tracks.len(), 1);
}

#[test]
fn a_linked_pair_uses_a1_when_it_is_free_after_the_busy_span() {
    let mut project = Project::default();
    project.timeline_clips = vec![clip("busy", A1, 0)];
    let (video, audio) = linked_pair("v");
    // A1 の clip は [0, 100) なので、100 からなら A1 に置ける。
    place_video_media_at(&mut project, video, Some(audio), 100).unwrap();
    assert_eq!(project.clip("v-a").unwrap().track, A1);
    assert_eq!(project.audio_tracks.len(), 1);
}
