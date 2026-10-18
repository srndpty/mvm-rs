//! Project JSON（schema 22）の試験。C++ 版の test_timeline_edit・test_media_bin のうち JSON に関わる assertion と、
//! C++ 版が書いた fixture の読み込みを移植したもの。

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

fn project_path() -> PathBuf {
    std::env::temp_dir()
        .join("mvm-rs-json-test")
        .join("project.mvm")
}

fn round_trip(project: &Project) -> Project {
    let path = project_path();
    let text = serialize_project_json(project, &path).unwrap();
    parse_project_json(&text, &path).unwrap()
}

fn serialized_value(project: &Project) -> Value {
    serde_json::from_str(&serialize_project_json(project, &project_path()).unwrap()).unwrap()
}

#[track_caller]
fn assert_rejected(value: &Value, fragment: &str) {
    let text = serde_json::to_string_pretty(value).unwrap();
    match parse_project_json(&text, &project_path()) {
        Ok(_) => panic!("期待: {fragment} を含む失敗 / 実際: 成功"),
        Err(error) => assert!(
            error.message().contains(fragment),
            "期待: {fragment} / 実際: {error}"
        ),
    }
}

fn clip_object(value: &mut Value, index: usize) -> &mut serde_json::Map<String, Value> {
    value["timeline_clips"][index].as_object_mut().unwrap()
}

/// 一部の素材と effect を持つ Project（C++ 版 testPersistenceTransaction の candidate）。
fn rich_project() -> Project {
    let mut project = three_clips();
    let manim = &mut project.timeline_clips[1];
    manim.source_fps = fps(30000, 1001);
    manim.source_in_frame = 30;
    manim.source_out_frame = 270;
    manim.timeline_start_frame = 300;
    // 速度も往復する（29.97fps 素材 240 frame の 80%）。
    manim.speed = fps(4, 5);
    let b = &mut project.timeline_clips[2];
    b.track = V2;
    b.timeline_start_frame = 90;
    b.effects.scale_x_percent = 60.0;
    b.effects.scale_y_percent = 60.0;
    b.effects.opacity_percent = 55.0;
    project.output_width = 3840;
    project.output_height = 2160;
    add_track(&mut project, TrackKind::Audio).unwrap();
    project.audio_tracks[0].muted = true;
    let mut audio = audio_clip("voice");
    audio.timeline_start_frame = 42;
    // ピッチ保持は等速では持てないので、保存を確かめるために 50% にする。
    audio.speed = fps(1, 2);
    audio.preserve_pitch = true;
    audio.link_group_id = "round-trip-link".to_owned();
    project.timeline_clips[2].link_group_id = "round-trip-link".to_owned();
    project.timeline_clips.push(audio);
    attach_fixture_media(&mut project);
    validate_timeline(&project).unwrap();
    project
}

#[test]
fn rich_project_round_trips() {
    let project = rich_project();
    let loaded = round_trip(&project);
    assert_eq!((loaded.output_width, loaded.output_height), (3840, 2160));
    assert_eq!(loaded.timeline_fps, fps(60, 1));
    assert_eq!(loaded.video_tracks, project.video_tracks);
    assert_eq!(loaded.audio_tracks, project.audio_tracks);
    assert_eq!(loaded.timeline_clips.len(), project.timeline_clips.len());
    for (actual, expected) in loaded.timeline_clips.iter().zip(&project.timeline_clips) {
        // 素材の path は project directory へ置き直されるので、それ以外を比べる。
        let mut expected = expected.clone();
        expected.media_path = actual.media_path.clone();
        assert_eq!(*actual, expected);
    }
    // 2 回目の往復は同じ Project になる（path も解決済み）。
    assert_eq!(round_trip(&loaded), loaded);
}

#[test]
fn required_fields_are_not_defaulted() {
    let value = serialized_value(&rich_project());
    for field in [
        "preserve_pitch",
        "enabled",
        "frame_hold",
        "media_item_id",
        "speed_num",
        "effects",
    ] {
        let mut missing = value.clone();
        clip_object(&mut missing, 0).remove(field);
        assert_rejected(&missing, "読めません");
    }
    for field in [
        "timeline_transitions",
        "timeline_markers",
        "in_frame",
        "subtitles",
        "media_items",
        "media_folders",
        "last_audio_adjustment_settings",
    ] {
        let mut missing = value.clone();
        missing.as_object_mut().unwrap().remove(field);
        assert_rejected(&missing, "読めません");
    }
    let mut effects = value.clone();
    clip_object(&mut effects, 0)["effects"]
        .as_object_mut()
        .unwrap()
        .remove("crop_left_keys");
    assert_rejected(&effects, "読めません");
}

#[test]
fn schema_and_format_marker_are_required() {
    let value = serialized_value(&rich_project());
    for version in [13, 19, 21, 23] {
        let mut old = value.clone();
        old["schema_version"] = json!(version);
        assert_rejected(&old, "対応していない schema_version");
    }
    let mut missing_schema = value.clone();
    missing_schema
        .as_object_mut()
        .unwrap()
        .remove("schema_version");
    assert_rejected(&missing_schema, "schema_version がありません");
    let mut stranger = value.clone();
    stranger.as_object_mut().unwrap().remove("format");
    assert_rejected(&stranger, "format marker");
    let mut wrong = value.clone();
    wrong["format"] = json!("other-project");
    assert_rejected(&wrong, "format marker");
}

#[test]
fn duplicate_and_unknown_fields() {
    let text = serialize_project_json(&rich_project(), &project_path()).unwrap();
    // 重複した key（文字列の上で 1 か所だけ増やす）。
    let duplicated = text.replacen(
        "\"enabled\": true,",
        "\"enabled\": true,\n      \"enabled\": true,",
        1,
    );
    assert_ne!(duplicated, text);
    assert!(
        parse_project_json(&duplicated, &project_path())
            .unwrap_err()
            .message()
            .contains("重複")
    );
    // 未知の field: top-level と通常の clip では読み飛ばし、effects では拒否する。
    let mut value: Value = serde_json::from_str(&text).unwrap();
    value["future_field"] = json!({"anything": [1, 2]});
    clip_object(&mut value, 0).insert("future_clip_field".to_owned(), json!(1));
    let accepted = serde_json::to_string(&value).unwrap();
    parse_project_json(&accepted, &project_path()).unwrap();
    clip_object(&mut value, 0)["effects"]["glow"] = json!(1);
    assert_rejected(&value, "読めません");
}

#[test]
fn frame_hold_round_trips() {
    let mut held = Project::default();
    held.timeline_clips = vec![clip("hold-source")];
    insert_frame_hold(
        &mut held,
        "id-hold-source",
        120,
        120,
        &mut prefixed_ids("new"),
    )
    .unwrap();
    attach_fixture_media(&mut held);
    let restored = round_trip(&held);
    assert_eq!(restored.timeline_clips.len(), 3);
    assert_eq!(
        restored.timeline_clips[2].frame_hold,
        held.timeline_clips[2].frame_hold
    );
    assert_eq!(
        (
            restored.timeline_clips[2].source_in_frame,
            restored.timeline_clips[2].source_out_frame
        ),
        (0, 120)
    );
}

fn transition_project() -> Project {
    let mut project = Project::default();
    let mut a = clip("A");
    a.source_frame_count = 600;
    a.source_out_frame = 300;
    let mut b = clip("B");
    b.source_frame_count = 600;
    b.source_in_frame = 300;
    b.source_out_frame = 600;
    b.timeline_start_frame = 300;
    project.timeline_clips = vec![a, b];
    project.timeline_transitions = vec![TimelineTransition {
        id: "t1".to_owned(),
        outgoing_clip_id: "id-A".to_owned(),
        incoming_clip_id: "id-B".to_owned(),
        frames_before_cut: 30,
        frames_after_cut: 30,
        kind: TransitionKind::Blend,
    }];
    attach_fixture_media(&mut project);
    project
}

#[test]
fn transitions_and_disabled_clips_round_trip() {
    let mut saved = transition_project();
    saved.timeline_clips[1].enabled = false;
    let loaded = round_trip(&saved);
    assert_eq!(loaded.timeline_transitions, saved.timeline_transitions);
    assert!(loaded.timeline_clips[0].enabled && !loaded.timeline_clips[1].enabled);

    let value = serialized_value(&saved);
    let mut missing = value.clone();
    missing["timeline_transitions"][0]
        .as_object_mut()
        .unwrap()
        .remove("frames_after_cut");
    assert_rejected(&missing, "読めません");
    let mut no_kind = value.clone();
    no_kind["timeline_transitions"][0]
        .as_object_mut()
        .unwrap()
        .remove("kind");
    assert_rejected(&no_kind, "読めません");
    let mut unknown_kind = value.clone();
    unknown_kind["timeline_transitions"][0]["kind"] = json!("dissolve");
    assert_rejected(&unknown_kind, "kind");
}

#[test]
fn timeline_marks_round_trip() {
    let mut project = Project::default();
    project.timeline_markers = vec![5, 15, 40];
    project.in_frame = Some(5);
    project.out_frame = Some(40);
    let loaded = round_trip(&project);
    assert_eq!(loaded.timeline_markers, project.timeline_markers);
    assert_eq!(
        (loaded.in_frame, loaded.out_frame),
        (project.in_frame, project.out_frame)
    );
}

#[test]
fn subtitles_round_trip() {
    let mut project = rich_project();
    project.subtitles = Some(SubtitleTrack {
        cues: vec![
            SubtitleCue {
                id: "cue-1".to_owned(),
                start_frame: 10,
                end_frame: 40,
                content: "こんにちは\n\"世界\"".to_owned(),
                link_clip_id: "id-voice".to_owned(),
            },
            SubtitleCue {
                id: "cue-2".to_owned(),
                start_frame: 50,
                end_frame: 60,
                content: "未リンク".to_owned(),
                link_clip_id: String::new(),
            },
        ],
        ..SubtitleTrack::default()
    });
    let loaded = round_trip(&project);
    assert_eq!(loaded.subtitles, project.subtitles);
    // 未リンクの字幕は項目自体を書かない。空文字列は受け付けない。
    let mut value = serialized_value(&project);
    assert!(value["subtitles"]["cues"][1].get("link_clip_id").is_none());
    value["subtitles"]["cues"][1]["link_clip_id"] = json!("");
    assert_rejected(&value, "リンク先");
}

fn media_project(root: &Path) -> Project {
    let mut project = Project::default();
    project.media_folders = vec![
        MediaFolder {
            id: "A".to_owned(),
            name: "A".to_owned(),
            parent_id: String::new(),
        },
        MediaFolder {
            id: "B".to_owned(),
            name: "B".to_owned(),
            parent_id: "A".to_owned(),
        },
    ];
    project.media_items = vec![
        MediaItem {
            id: "v".to_owned(),
            kind: MediaKind::Video,
            media_path: root.join("media").join("v.mp4"),
            name: "v.mp4".to_owned(),
            folder_id: "B".to_owned(),
            fps: fps(24000, 1001),
            frame_count: 598,
            width: 1920,
            height: 1080,
            ..MediaItem::default()
        },
        MediaItem {
            id: "a".to_owned(),
            kind: MediaKind::Audio,
            media_path: PathBuf::from("D:/elsewhere/a.wav"),
            name: "a.wav".to_owned(),
            sample_rate: 48000,
            duration_samples: 480_000,
            ..MediaItem::default()
        },
    ];
    project
}

#[test]
fn media_bin_round_trips_with_relative_paths() {
    let path = project_path();
    let root = path.parent().unwrap();
    let project = media_project(root);
    let text = serialize_project_json(&project, &path).unwrap();
    assert!(
        text.contains("\"media_path\": \"media/v.mp4\""),
        "project 配下の素材は相対で書く"
    );
    assert!(
        text.contains("\"media_path\": \"D:/elsewhere/a.wav\""),
        "別 drive の素材は絶対で書く"
    );
    let loaded = parse_project_json(&text, &path).unwrap();
    assert_eq!(loaded.media_folders, project.media_folders);
    assert_eq!(loaded.media_items, project.media_items);
}

#[test]
fn media_items_reject_missing_or_invalid_values() {
    let value = serialized_value(&media_project(project_path().parent().unwrap()));
    let mut missing_items = value.clone();
    missing_items.as_object_mut().unwrap().remove("media_items");
    assert_rejected(&missing_items, "読めません");
    let mut missing_field = value.clone();
    missing_field["media_items"][1]
        .as_object_mut()
        .unwrap()
        .remove("duration_samples");
    assert_rejected(&missing_field, "読めません");
    let mut unknown = value.clone();
    unknown["media_items"][1]["kind"] = json!("midi");
    assert_rejected(&unknown, "未知の media item kind");
    let mut zero = value.clone();
    zero["media_items"][1]["duration_samples"] = json!(0);
    assert_rejected(&zero, "音声情報");
}

#[test]
fn media_references_round_trip_and_are_required() {
    let project = rich_project();
    let loaded = round_trip(&project);
    for (actual, expected) in loaded.timeline_clips.iter().zip(&project.timeline_clips) {
        assert_eq!(actual.media_item_id, expected.media_item_id);
    }
    // 素材の無い clip を持つ Project は保存しない。
    let mut orphan = project.clone();
    orphan.timeline_clips[0].media_item_id = "missing".to_owned();
    assert!(serialize_project_json(&orphan, &project_path()).is_err());
}

fn fixture(name: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name);
    std::fs::read_to_string(path).unwrap()
}

// C++ 版が書いた schema 22 の Project を読み、書き直したものを読み直しても同じ Project になる。
#[test]
fn reads_projects_written_by_cpp() {
    let path = PathBuf::from(r"C:\fixtures\math-score\good.mvm");
    let project = parse_project_json(&fixture("math-score-good.mvm"), &path).unwrap();
    assert!(
        project
            .timeline_clips
            .iter()
            .any(|clip| clip.kind == TimelineClipKind::Math)
    );
    let rewritten = serialize_project_json(&project, &path).unwrap();
    assert_eq!(parse_project_json(&rewritten, &path).unwrap(), project);
    // bad-font は C++ 版の負例（数式の文字サイズ 1081 が出力の高さ 1080 を超える）。
    let error = parse_project_json(&fixture("math-score-bad-font.mvm"), &path).unwrap_err();
    assert!(error.message().contains("数式の文字サイズ"), "{error}");
}

// 旧 schema は読まない（移行しない）。
#[test]
fn rejects_old_schema_written_by_cpp() {
    let error = parse_project_json(&fixture("equation-sequence-schema21.mvm"), &project_path())
        .unwrap_err();
    assert!(
        error.message().contains("対応していない schema_version"),
        "{error}"
    );
}

/// C++ 版が手元で書いた .mvm を集めて読む（`MVM_CPP_PROJECT_CORPUS` に directory を指定する）。
/// 試験の副産物には意図的に壊したファイルもあるので、件数と失敗の理由を出す診断として使う。
/// `MVM_CPP_PROJECT_VALIDATE` に C++ 版の mvm_project_validate.exe を指定すると、Rust で書き直した
/// file を C++ 版が読めて検証を通ることも確かめる（こちらは 1 件でも拒否されたら失敗）。
#[test]
#[ignore = "workstation"]
fn reads_cpp_project_corpus() {
    let Ok(root) = std::env::var("MVM_CPP_PROJECT_CORPUS") else {
        panic!("MVM_CPP_PROJECT_CORPUS が指定されていません");
    };
    let validator = std::env::var_os("MVM_CPP_PROJECT_VALIDATE").map(PathBuf::from);
    let output = std::env::temp_dir().join("mvm-rs-corpus-roundtrip");
    if validator.is_some() {
        let _ = std::fs::remove_dir_all(&output);
        std::fs::create_dir_all(&output).unwrap();
    }
    let mut stack = vec![PathBuf::from(root)];
    let (mut parsed, mut other_schema, mut failed) = (0, 0, Vec::new());
    let mut rejected_by_cpp = Vec::new();
    while let Some(directory) = stack.pop() {
        for entry in std::fs::read_dir(&directory).unwrap().flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            if path.extension().and_then(|extension| extension.to_str()) != Some("mvm") {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            if !text.contains("\"schema_version\": 22") {
                other_schema += 1;
                continue;
            }
            match parse_project_json(&text, &path) {
                Ok(project) => {
                    let rewritten = serialize_project_json(&project, &path).unwrap();
                    assert_eq!(
                        parse_project_json(&rewritten, &path).unwrap(),
                        project,
                        "{}",
                        path.display()
                    );
                    parsed += 1;
                    if let Some(validator) = &validator {
                        let destination = output.join(format!("{parsed}.mvm"));
                        let text = serialize_project_json(&project, &destination).unwrap();
                        std::fs::write(&destination, text).unwrap();
                        let result = std::process::Command::new(validator)
                            .arg(&destination)
                            .output()
                            .unwrap();
                        if !result.status.success() {
                            rejected_by_cpp.push(format!(
                                "{} → {}: {}",
                                path.display(),
                                destination.display(),
                                String::from_utf8_lossy(&result.stderr).trim()
                            ));
                        }
                    }
                }
                Err(error) => failed.push(format!("{}: {error}", path.display())),
            }
        }
    }
    eprintln!(
        "読めた: {parsed} / 他の schema: {other_schema} / 失敗: {}",
        failed.len()
    );
    for failure in &failed {
        eprintln!("  {failure}");
    }
    assert!(parsed > 0, "1 件も読めていません");
    if validator.is_some() {
        eprintln!("C++ 版が拒否した書き直し: {}", rejected_by_cpp.len());
        for rejection in &rejected_by_cpp {
            eprintln!("  {rejection}");
        }
        assert!(
            rejected_by_cpp.is_empty(),
            "Rust で書き直した file を C++ 版が読めません"
        );
    }
}
