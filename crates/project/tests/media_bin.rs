//! プロジェクトパネル（素材とフォルダ）の試験。C++ 版 tests/harness/test_media_bin.cpp の assertion を移植したもの。
//! 実ファイルの識別（hard link・大文字小文字）は platform crate、JSON の往復は json の試験で扱う。

// 試験の失敗は大きく落ちてよい（helper 関数も試験の一部）。Project::default() から field を足す書き方は意図的。
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::field_reassign_with_default
)]

mod common;

use std::collections::BTreeSet;
use std::path::PathBuf;

use common::fps;
use mvm_project::media_bin::*;
use mvm_project::path_identity::{
    FileIdentityKey, FileIdentityStatus, PathSameness, canonical_path_key, compare_path_identity,
};
use mvm_project::timeline::validate_timeline;
use mvm_project::*;

fn video_item(id: &str, path: &str, folder_id: &str) -> MediaItem {
    MediaItem {
        id: id.to_owned(),
        kind: MediaKind::Video,
        media_path: PathBuf::from(path),
        name: path.to_owned(),
        folder_id: folder_id.to_owned(),
        fps: fps(24000, 1001),
        frame_count: 598,
        width: 1920,
        height: 1080,
        ..MediaItem::default()
    }
}

fn audio_item(id: &str, path: &str, folder_id: &str) -> MediaItem {
    MediaItem {
        id: id.to_owned(),
        kind: MediaKind::Audio,
        media_path: PathBuf::from(path),
        name: path.to_owned(),
        folder_id: folder_id.to_owned(),
        sample_rate: 48000,
        duration_samples: 48000 * 10,
        ..MediaItem::default()
    }
}

fn image_item(id: &str, path: &str) -> MediaItem {
    MediaItem {
        id: id.to_owned(),
        kind: MediaKind::Image,
        media_path: PathBuf::from(path),
        name: path.to_owned(),
        width: 800,
        height: 600,
        ..MediaItem::default()
    }
}

fn folder(id: &str, parent_id: &str) -> MediaFolder {
    MediaFolder {
        id: id.to_owned(),
        name: id.to_owned(),
        parent_id: parent_id.to_owned(),
    }
}

/// root/A/B と、B に video、root に audio を持つ Project。
fn nested_project() -> Project {
    let mut project = Project::default();
    project.media_folders = vec![folder("A", ""), folder("B", "A")];
    project.media_items = vec![
        video_item("v", "C:/media/v.mp4", "B"),
        audio_item("a", "C:/media/a.wav", ""),
    ];
    project
}

fn timeline_clip(
    id: &str,
    kind: TimelineClipKind,
    path: &str,
    track: TrackKind,
    start: i64,
) -> TimelineClip {
    TimelineClip {
        kind,
        media_path: PathBuf::from(path),
        name: id.to_owned(),
        id: id.to_owned(),
        source_fps: fps(60, 1),
        source_frame_count: 60,
        source_in_frame: 0,
        source_out_frame: 60,
        timeline_start_frame: start,
        track: TrackRef::new(track, 0),
        ..TimelineClip::default()
    }
}

/// v.mp4 のリンク対（映像と音声）、a.wav の音声、文字を timeline に置いた Project。
fn used_project() -> Project {
    let mut project = nested_project();
    let mut video = timeline_clip(
        "clip-v",
        TimelineClipKind::Video,
        "C:/media/v.mp4",
        TrackKind::Video,
        0,
    );
    let mut video_audio = timeline_clip(
        "clip-v-audio",
        TimelineClipKind::Audio,
        "C:/media/v.mp4",
        TrackKind::Audio,
        0,
    );
    video.link_group_id = "pair".to_owned();
    video_audio.link_group_id = "pair".to_owned();
    video.media_item_id = "v".to_owned();
    video_audio.media_item_id = "v".to_owned();
    let mut audio = timeline_clip(
        "clip-a",
        TimelineClipKind::Audio,
        "C:/media/a.wav",
        TrackKind::Audio,
        100,
    );
    audio.media_item_id = "a".to_owned();
    let mut text = timeline_clip(
        "clip-text",
        TimelineClipKind::Text,
        "",
        TrackKind::Video,
        100,
    );
    text.text.content = "字幕".to_owned();
    project.timeline_clips = vec![video, video_audio, audio, text];
    project
}

fn ids(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_owned()).collect()
}

fn has_clip(project: &Project, id: &str) -> bool {
    project.timeline_clips.iter().any(|clip| clip.id == id)
}

#[test]
fn edits() {
    let mut project = nested_project();
    validate_media_bin(&project).unwrap();
    add_media_folder(&mut project, folder("C", "B")).unwrap();
    assert_eq!(project.media_folders.len(), 3);

    // 自分の子孫・自分自身へ移すと循環する。失敗時は Project を変えない。
    let before_cycle = project.clone();
    assert!(move_media_bin_entries(&mut project, &ids(&["A"]), "C").is_err());
    assert!(move_media_bin_entries(&mut project, &ids(&["A"]), "A").is_err());
    assert_eq!(project, before_cycle);
    assert!(move_media_bin_entries(&mut project, &ids(&["v"]), "missing").is_err());
    assert!(move_media_bin_entries(&mut project, &ids(&["missing"]), "").is_err());
    move_media_bin_entries(&mut project, &ids(&["v", "C"]), "").unwrap();
    assert!(find_media_item(&project, "v").unwrap().folder_id.is_empty());
    assert!(
        find_media_folder(&project, "C")
            .unwrap()
            .parent_id
            .is_empty()
    );

    assert!(rename_media_bin_entry(&mut project, "A", String::new()).is_err());
    rename_media_bin_entry(&mut project, "a", "ナレーション".to_owned()).unwrap();
    assert_eq!(find_media_item(&project, "a").unwrap().name, "ナレーション");

    // 同じファイルの素材は 2 つ持たない。表記ゆれ（..）も同じファイルとして扱う。
    assert!(add_media_item(&mut project, audio_item("a2", "C:/media/x/../a.wav", "")).is_err());
    assert_eq!(project.media_items.len(), 2);
    add_media_item(&mut project, image_item("i", "C:/media/still.png")).unwrap();

    // 調べ直した値で技術的な値だけを置き換える。id・名前・フォルダ・ファイルは変えない。
    move_media_bin_entries(&mut project, &ids(&["i"]), "A").unwrap();
    rename_media_bin_entry(&mut project, "i", "表紙".to_owned()).unwrap();
    let mut portrait = image_item("other-id", "C:/elsewhere/other.png");
    portrait.width = 600;
    portrait.height = 800;
    refresh_media_item(&mut project, "i", &portrait).unwrap();
    let refreshed = find_media_item(&project, "i").unwrap();
    assert_eq!((refreshed.width, refreshed.height), (600, 800));
    assert_eq!(
        (refreshed.name.as_str(), refreshed.folder_id.as_str()),
        ("表紙", "A")
    );
    assert_eq!(refreshed.media_path, PathBuf::from("C:/media/still.png"));
    let before_refresh = project.clone();
    let broken = MediaItem {
        width: 0,
        ..portrait.clone()
    };
    assert!(refresh_media_item(&mut project, "i", &broken).is_err());
    assert!(refresh_media_item(&mut project, "missing", &portrait).is_err());
    assert_eq!(project, before_refresh);
}

#[test]
fn remove() {
    // A を消すと、子孫の B とその中の素材も消える。root の素材は残る。
    let mut project = nested_project();
    remove_media_bin_entries(&mut project, &ids(&["A"])).unwrap();
    assert!(project.media_folders.is_empty());
    assert_eq!(project.media_items.len(), 1);
    assert_eq!(project.media_items[0].id, "a");

    // 使用中の素材は、それを参照する clip（とリンク相手）ごと消える。
    let mut used = used_project();
    validate_timeline(&used).unwrap();
    validate_media_bin(&used).unwrap();
    assert_eq!(
        media_items_in_use(&used),
        BTreeSet::from(["v".to_owned(), "a".to_owned()])
    );
    let plan = plan_media_bin_removal(&used, &ids(&["v"])).unwrap();
    assert_eq!(plan.clip_ids, ids(&["clip-v", "clip-v-audio"]));
    assert_eq!(
        remove_media_bin_entries(&mut used, &ids(&["v"])).unwrap(),
        plan.clip_ids
    );
    assert!(find_media_item(&used, "v").is_none());
    assert!(!has_clip(&used, "clip-v") && !has_clip(&used, "clip-v-audio"));
    // 対照: 別素材の clip と、ファイルを参照しない文字 clip は残る。
    assert!(has_clip(&used, "clip-a") && has_clip(&used, "clip-text"));
    assert!(find_media_item(&used, "a").is_some());

    // folder の削除は、その中の素材を参照する clip も消す。
    let mut via_folder = used_project();
    assert_eq!(
        remove_media_bin_entries(&mut via_folder, &ids(&["A"]))
            .unwrap()
            .len(),
        2
    );
    assert!(!has_clip(&via_folder, "clip-v") && has_clip(&via_folder, "clip-a"));

    // 未使用の素材なら clip は消えない。
    let mut unused = used_project();
    unused.timeline_clips.drain(0..2);
    assert!(
        plan_media_bin_removal(&unused, &ids(&["v"]))
            .unwrap()
            .clip_ids
            .is_empty()
    );

    // 失敗時は Project を変えない。
    let mut invalid = used_project();
    let before = invalid.clone();
    assert!(remove_media_bin_entries(&mut invalid, &ids(&["v", "missing"])).is_err());
    assert!(remove_media_bin_entries(&mut invalid, &[]).is_err());
    assert_eq!(invalid, before);
    assert!(plan_media_bin_removal(&invalid, &ids(&["missing"])).is_err());
}

// 使っている素材の時間軸が外部で変わったときは、素材だけを更新しない。
#[test]
fn refresh_timing() {
    let mut used = used_project(); // v.mp4（24000/1001, 598 frame）をリンク対が使う
    let before = used.clone();
    let mut retimed = find_media_item(&used, "v").unwrap().clone();
    retimed.fps = fps(30, 1);
    retimed.frame_count = 120;
    assert!(refresh_media_item(&mut used, "v", &retimed).is_err());
    let mut recast = find_media_item(&used, "a").unwrap().clone();
    recast.duration_samples = 48000;
    assert!(refresh_media_item(&mut used, "a", &recast).is_err());
    assert_eq!(used, before);
    // 解像度だけなら clip の時間軸に関わらないので更新する。
    let mut resized = find_media_item(&used, "v").unwrap().clone();
    resized.width = 1280;
    resized.height = 720;
    refresh_media_item(&mut used, "v", &resized).unwrap();
    assert_eq!(find_media_item(&used, "v").unwrap().width, 1280);
    // 使われていない素材なら時間軸が変わっても更新する。
    let mut unused = used_project();
    unused.timeline_clips.drain(0..2);
    refresh_media_item(&mut unused, "v", &retimed).unwrap();
    assert_eq!(find_media_item(&unused, "v").unwrap().fps, fps(30, 1));
}

#[test]
fn validation() {
    let invalid = |change: &dyn Fn(&mut Project)| {
        let mut project = nested_project();
        change(&mut project);
        validate_media_bin(&project)
    };
    assert!(
        invalid(&|p| p.media_items[1].width = 640).is_err(),
        "解像度を持つ音声素材"
    );
    assert!(
        invalid(&|p| p.media_items[0].fps = fps(0, 1001)).is_err(),
        "fps の無い動画素材"
    );
    assert!(
        invalid(&|p| p.media_items[0].fps = fps(120, 2)).is_err(),
        "約分されていない fps"
    );
    assert!(
        invalid(&|p| {
            let mut image = image_item("i", "C:/media/still.png");
            image.frame_count = 10;
            p.media_items.push(image);
        })
        .is_err(),
        "尺を持つ静止画素材"
    );
    assert!(
        invalid(&|p| p.media_folders[0].parent_id = "B".to_owned()).is_err(),
        "循環した folder"
    );
    assert!(
        invalid(&|p| p.media_items[0].folder_id = "missing".to_owned()).is_err(),
        "存在しない folder"
    );
    // folder と item は同じ id 名前空間を共有する。UI は id だけで entry を指すため。
    assert!(
        invalid(&|p| p.media_items[1].id = "A".to_owned()).is_err(),
        "folder と同じ id の素材"
    );

    // 存在しないファイルは表記で比べる。区切り文字と .. は揃える。
    let mut missing = Project::default();
    missing.media_items = vec![
        audio_item("m1", "C:/mvm-missing/Tone.wav", ""),
        audio_item("m2", r"C:\mvm-missing\x\..\Tone.wav", ""),
    ];
    assert!(
        validate_media_bin(&missing).is_err(),
        "区切り文字と .. だけが違う素材の重複"
    );
    // 大文字小文字は畳まない。case-sensitive directory では別のファイルでありうる。
    let mut case_only = Project::default();
    case_only.media_items = vec![
        audio_item("m1", "C:/mvm-missing/Tone.wav", ""),
        audio_item("m2", "C:/mvm-missing/tone.wav", ""),
    ];
    validate_media_bin(&case_only).unwrap();
}

// 実体（file ID）が取れているときは表記より実体で決める。
#[test]
fn case_sensitive_identity() {
    let key = |path: &str, file: &str| FileIdentityKey {
        status: FileIdentityStatus::FileId,
        path_key: Some(path.to_owned()),
        file_key: Some(file.to_owned()),
    };
    let upper = key("C:/case-sensitive/A.mp4", "1:aa");
    let lower = key("C:/case-sensitive/a.mp4", "1:bb");
    assert_eq!(
        compare_path_identity(&upper, &lower),
        PathSameness::Different
    );
    // 表記が同じでも file ID が違えば別物（表記で先に Same を返さない）。
    let replaced = key("C:/case-sensitive/A.mp4", "1:cc");
    assert_eq!(
        compare_path_identity(&upper, &replaced),
        PathSameness::Different
    );
    let alias = key("C:/case-sensitive/x.mp4", "1:aa");
    assert_eq!(compare_path_identity(&upper, &alias), PathSameness::Same);
    assert_ne!(
        canonical_path_key("C:/case-sensitive/A.mp4".as_ref()),
        canonical_path_key("C:/case-sensitive/a.mp4".as_ref())
    );
}

// 動画・音声・画像の clip はプロジェクトパネルの素材を media_item_id で指す。
#[test]
fn media_references() {
    let control = used_project();
    validate_media_references(&control).unwrap();
    let variant = |change: &dyn Fn(&mut Project)| {
        let mut project = control.clone();
        change(&mut project);
        project
    };
    assert!(
        validate_media_references(&variant(&|p| p.timeline_clips[2].media_item_id.clear()))
            .is_err()
    );
    assert!(
        validate_media_bin(&variant(
            &|p| p.timeline_clips[2].media_item_id = "missing".to_owned()
        ))
        .is_err()
    );
    // 動画 clip が音声素材を指す。
    assert!(
        validate_media_references(&variant(&|p| {
            p.timeline_clips[0].media_item_id = "a".to_owned();
            p.timeline_clips[0].media_path = PathBuf::from("C:/media/a.wav");
        }))
        .is_err()
    );
    assert!(
        validate_media_references(&variant(
            &|p| p.timeline_clips[2].media_path = PathBuf::from("C:/media/other.wav")
        ))
        .is_err()
    );
    // 表記ゆれ（区切り文字・..）は同じファイル。
    validate_media_references(&variant(&|p| {
        p.timeline_clips[2].media_path = PathBuf::from(r"C:\media\x\..\a.wav");
    }))
    .unwrap();
    // 大文字小文字の違いは I/O なしでは同じと言えない。
    assert!(
        validate_media_references(&variant(
            &|p| p.timeline_clips[2].media_path = PathBuf::from("C:/MEDIA/A.WAV")
        ))
        .is_err()
    );
    assert!(
        validate_media_references(&variant(
            &|p| p.timeline_clips[3].media_item_id = "a".to_owned()
        ))
        .is_err()
    );
}
