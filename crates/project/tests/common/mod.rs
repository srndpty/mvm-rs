//! 試験で共有する Project の部品。C++ 版 tests/harness/test_timeline_edit.cpp の helper に対応する。
#![allow(dead_code, clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::PathBuf;

use mvm_core::Rational;
use mvm_project::timeline::timeline_clip_duration;
use mvm_project::{Project, TimelineClip, TimelineClipKind, TrackRef};

pub const V1: TrackRef = TrackRef::video(0);
pub const V2: TrackRef = TrackRef::video(1);
pub const V3: TrackRef = TrackRef::video(2);
pub const A1: TrackRef = TrackRef::audio(0);
pub const A2: TrackRef = TrackRef::audio(1);

pub fn fps(num: i64, den: i64) -> Rational {
    Rational::new(num, den)
}

/// 60fps・300 frame の素材を丸ごと使う clip（ID は "id-" + name）。
pub fn clip_on(name: &str, kind: TimelineClipKind, track: TrackRef) -> TimelineClip {
    TimelineClip {
        kind,
        media_path: PathBuf::from(format!("{name}.mp4")),
        name: name.to_owned(),
        id: format!("id-{name}"),
        source_fps: fps(60, 1),
        source_frame_count: 300,
        source_in_frame: 0,
        source_out_frame: 300,
        timeline_start_frame: 0,
        track,
        ..TimelineClip::default()
    }
}

pub fn clip(name: &str) -> TimelineClip {
    clip_on(name, TimelineClipKind::Video, V1)
}

pub fn audio_clip(name: &str) -> TimelineClip {
    clip_on(name, TimelineClipKind::Audio, A1)
}

/// A（0-300）・Manim（300-600）・B（600-900）を V1 に並べた Project。
pub fn three_clips() -> Project {
    let mut project = Project::default();
    project.timeline_clips = vec![
        clip("A"),
        clip_on("Manim", TimelineClipKind::Manim, V1),
        clip("B"),
    ];
    project.timeline_clips[1].timeline_start_frame = 300;
    project.timeline_clips[2].timeline_start_frame = 600;
    project
}

/// "new-1", "new-2", ... を返す ID 発行器。
pub fn sequential_ids() -> impl FnMut() -> String {
    let mut counter = 0;
    move || {
        counter += 1;
        format!("new-{counter}")
    }
}

/// prefix-1, prefix-2, ... を返す ID 発行器。
pub fn prefixed_ids(prefix: &'static str) -> impl FnMut() -> String {
    let mut counter = 0;
    move || {
        counter += 1;
        format!("{prefix}-{counter}")
    }
}

pub fn duration(project: &Project, clip: &TimelineClip) -> i64 {
    timeline_clip_duration(project, clip).expect("clip の尺を求められません")
}

pub fn duration_at(project: &Project, index: usize) -> i64 {
    duration(project, &project.timeline_clips[index])
}

pub fn clip_end(project: &Project, clip: &TimelineClip) -> i64 {
    clip.timeline_start_frame + duration(project, clip)
}

pub fn find<'a>(project: &'a Project, id: &str) -> Option<&'a TimelineClip> {
    project.timeline_clips.iter().find(|clip| clip.id == id)
}

pub fn ids(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_owned()).collect()
}

/// 動画・音声・画像の clip に、同じファイルのプロジェクトパネルの素材を付ける（C++ 版の attachFixtureMedia）。
/// 映像のある clip を先に見る。同じファイルの音声 clip はその素材を共有する。
pub fn attach_fixture_media(project: &mut Project) {
    use mvm_project::path_identity::canonical_path_key;
    use mvm_project::{MediaItem, MediaKind};
    use std::collections::HashMap;
    let mut item_by_path: HashMap<Option<String>, String> = project
        .media_items
        .iter()
        .map(|item| (canonical_path_key(&item.media_path), item.id.clone()))
        .collect();
    for audio_pass in [false, true] {
        for index in 0..project.timeline_clips.len() {
            let clip = &project.timeline_clips[index];
            if !clip.kind.uses_media_item()
                || !clip.media_item_id.is_empty()
                || (clip.kind == TimelineClipKind::Audio) != audio_pass
            {
                continue;
            }
            let key = canonical_path_key(&clip.media_path);
            if let Some(id) = item_by_path.get(&key) {
                project.timeline_clips[index].media_item_id = id.clone();
                continue;
            }
            let id = format!("fixture-media-{}", project.media_items.len());
            let mut item = MediaItem {
                id: id.clone(),
                media_path: clip.media_path.clone(),
                name: if clip.name.is_empty() {
                    id.clone()
                } else {
                    clip.name.clone()
                },
                ..MediaItem::default()
            };
            match clip.kind {
                TimelineClipKind::Video => {
                    item.kind = MediaKind::Video;
                    item.fps = clip.source_fps.reduced().unwrap_or(fps(60, 1));
                    item.frame_count = clip.source_frame_count.max(1);
                    item.width = 1920;
                    item.height = 1080;
                }
                TimelineClipKind::Audio => {
                    item.kind = MediaKind::Audio;
                    item.sample_rate = 48000;
                    item.duration_samples = 48000;
                }
                _ => {
                    item.kind = MediaKind::Image;
                    item.width = 1920;
                    item.height = 1080;
                }
            }
            project.media_items.push(item);
            project.timeline_clips[index].media_item_id = id.clone();
            item_by_path.insert(key, id);
        }
    }
}

/// 速度だけを指定する速度・尺の編集（ripple も上書きもしない）。
pub fn speed_edit(num: i64, den: i64) -> mvm_project::timeline::ClipSpeedDurationEdit {
    use mvm_project::timeline::{ClipSpeedDurationEdit, SpeedDurationInput};
    ClipSpeedDurationEdit {
        input: SpeedDurationInput::Speed,
        speed: fps(num, den),
        duration_frames: 0,
        preserve_pitch: false,
        ripple: false,
        overwrite: false,
    }
}
