//! 字幕・SRT・全 track の時間編集の試験。C++ 版 tests/harness/test_subtitles.cpp の project 部分を移植したもの。
//! 文字 raster と書き出し計画の検査は、それぞれ text crate と export crate を作る段階で移植する。

// 試験の失敗は大きく落ちてよい（helper 関数も試験の一部）。Project::default() から field を足す書き方は意図的。
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::field_reassign_with_default
)]

mod common;

use std::collections::HashSet;
use std::path::PathBuf;

use common::*;
use mvm_project::clip_effects::ClipKeyframe;
use mvm_project::json::{parse_project_json, serialize_project_json};
use mvm_project::subtitles::*;
use mvm_project::timeline::*;
use mvm_project::*;

fn cue(id: &str, start: i64, end: i64, content: &str, link: &str) -> SubtitleCue {
    SubtitleCue {
        id: id.to_owned(),
        start_frame: start,
        end_frame: end,
        content: content.to_owned(),
        link_clip_id: link.to_owned(),
    }
}

fn with_cues(mut project: Project, cues: Vec<SubtitleCue>) -> Project {
    project.subtitles = Some(SubtitleTrack {
        cues,
        ..SubtitleTrack::default()
    });
    project
}

fn cues(project: &Project) -> &[SubtitleCue] {
    &project
        .subtitles
        .as_ref()
        .expect("字幕 track がありません")
        .cues
}

fn find_cue<'a>(project: &'a Project, id: &str) -> Option<&'a SubtitleCue> {
    cues(project).iter().find(|cue| cue.id == id)
}

fn cue_span(project: &Project, id: &str) -> (i64, i64) {
    let cue = find_cue(project, id).unwrap_or_else(|| panic!("字幕 {id} がありません"));
    (cue.start_frame, cue.end_frame)
}

fn index_of(project: &Project, clip_id: &str) -> usize {
    project
        .clip_index(clip_id)
        .unwrap_or_else(|| panic!("clip {clip_id} がありません"))
}

/// 60fps・frames 長の text clip。
fn text_clip(id: &str, name: &str, frames: i64) -> TimelineClip {
    let mut clip = TimelineClip {
        kind: TimelineClipKind::Text,
        id: id.to_owned(),
        name: name.to_owned(),
        source_fps: fps(60, 1),
        source_frame_count: frames,
        source_out_frame: frames,
        ..TimelineClip::default()
    };
    clip.text.content = name.to_owned();
    clip
}

fn json_path(name: &str) -> PathBuf {
    std::env::temp_dir()
        .join("mvm-rs-subtitles-test")
        .join(name)
}

const SRT: &str = "\u{feff}7\r\n00:00:00,000 --> 00:00:01,000\r\n日本語\r\n次の行\r\n\r\n9\r\n00:00:01,000 --> 00:00:02,000\r\n終わり\r\n";

/// SRT の 2 cue（[0, 60) と [60, 120)）を適用した Project。
fn srt_project() -> Project {
    let mut project = Project::default();
    let cues = parse_srt(SRT, fps(60, 1), &mut prefixed_ids("s")).unwrap();
    apply_subtitle_cues(&mut project, cues, true).unwrap();
    project
}

#[test]
fn srt_reads_japanese_and_applies_half_open_intervals() {
    let parsed = parse_srt(SRT, fps(60, 1), &mut prefixed_ids("s")).unwrap();
    assert_eq!(parsed.len(), 2);
    assert_eq!(parsed[0].end_frame, 60);
    assert_eq!(parsed[0].content, "日本語\n次の行");
    let project = srt_project();
    assert!(active_subtitle_at(&project, -1).is_none());
    assert_eq!(active_subtitle_at(&project, 59).unwrap().id, parsed[0].id);
    assert_eq!(active_subtitle_at(&project, 60).unwrap().id, parsed[1].id);
    assert!(active_subtitle_at(&project, 120).is_none());
    assert_eq!(
        timeline_end_frame(&project).unwrap(),
        120,
        "字幕だけの timeline 尺"
    );

    let path = json_path("subtitles.mvm");
    let text = serialize_project_json(&project, &path).unwrap();
    assert_eq!(
        parse_project_json(&text, &path).unwrap(),
        project,
        "字幕 JSON の往復"
    );

    let srt = write_srt(&project).unwrap();
    let again = parse_srt(&srt, fps(60, 1), &mut prefixed_ids("r")).unwrap();
    assert_eq!(again.len(), 2);
    assert_eq!(again[1].end_frame, 120, "SRT の往復");
}

#[test]
fn invalid_srt_is_rejected_with_line_number() {
    for text in [
        "1\n00:00:01,000 --> 00:00:00,000\n逆\n",
        "1\n00:00:00,000 --> 00:00:00,001\n短い\n",
        "1\n00:00:00,000 --> 00:00:01,000\n\n",
        "1\n00:00:00,000 --> 00:00:01,000\n<b>装飾</b>\n",
        "1\n00:00:00,000 --> 00:00:01,000\n前\n\n2\n00:00:00,500 --> 00:00:02,000\n重複\n",
    ] {
        let error = parse_srt(text, fps(60, 1), &mut prefixed_ids("s")).unwrap_err();
        assert!(error.message().contains("行目"), "{text:?}: {error}");
    }
}

// 本文中の < > { } は装飾ではない。実際の装飾記法だけを拒否する。
#[test]
fn srt_markup_detection() {
    for plain in ["a < b", "std::vector<int>", "f(x) = { x > 0 のとき }", "<>"] {
        let text = format!("1\n00:00:00,000 --> 00:00:01,000\n{plain}\n");
        let read = parse_srt(&text, fps(60, 1), &mut prefixed_ids("s")).unwrap();
        assert_eq!(read.len(), 1);
        assert_eq!(read[0].content, plain);
    }
    for markup in [
        "<i>斜体</i>",
        "<B>太字</B>",
        "<font color=\"red\">赤</font>",
        "{\\an8}上に表示",
    ] {
        let text = format!("1\n00:00:00,000 --> 00:00:01,000\n{markup}\n");
        let error = parse_srt(&text, fps(60, 1), &mut prefixed_ids("s")).unwrap_err();
        assert!(error.message().contains("装飾記法"), "{markup}: {error}");
    }
    let mut unsupported = srt_project();
    unsupported.subtitles.as_mut().unwrap().cues[0].content = "<b>本文</b>".to_owned();
    assert!(
        write_srt(&unsupported).is_err(),
        "未対応記法は SRT へ出さない"
    );
}

// 文字起こしで作った（clip にリンクした）字幕も SRT へ出せる。リンクは SRT に含めない。
#[test]
fn linked_subtitles_write_to_srt_without_links() {
    let mut project = Project::default();
    project.timeline_clips = vec![text_clip("speech", "発話", 120)];
    let project = with_cues(
        project,
        vec![
            cue("a", 0, 60, "std::vector<int> の説明", "speech"),
            cue("b", 60, 120, "後半", "speech"),
        ],
    );
    let text = write_srt(&project).unwrap();
    let reread = parse_srt(&text, fps(60, 1), &mut prefixed_ids("s")).unwrap();
    assert_eq!(reread.len(), 2);
    assert_eq!(reread[0].content, "std::vector<int> の説明");
    assert_eq!((reread[0].end_frame, reread[1].start_frame), (60, 60));
    assert!(reread[1].link_clip_id.is_empty());
}

#[test]
fn time_conversion_and_timed_text() {
    assert_eq!(
        subtitle_time_to_frame(25, fps(60, 1)).unwrap(),
        2,
        "ちょうど半分は後ろへ丸める"
    );
    assert_eq!(
        subtitle_time_to_frame(1001, fps(30000, 1001)).unwrap(),
        30,
        "非整数 fps の換算"
    );
    let segment = SubtitleTimedText {
        start_ms: 1000,
        end_ms: 2000,
        content: "速い字幕".to_owned(),
    };
    let timed = subtitle_cues_from_timed_text(
        &[segment],
        fps(60, 1),
        fps(2, 1),
        120,
        &mut prefixed_ids("t"),
    )
    .unwrap();
    assert_eq!(
        (timed[0].start_frame, timed[0].end_frame),
        (150, 180),
        "速度の逆数と配置を反映"
    );
    assert!(
        subtitle_cues_from_timed_text(&[], fps(60, 1), fps(1, 1), 0, &mut prefixed_ids("t"))
            .is_err(),
        "認識結果 0 件"
    );
}

#[test]
fn edit_split_and_merge_subtitles() {
    let mut project = srt_project();
    let first = cues(&project)[0].clone();
    let before = project.clone();
    let blank = SubtitleCue {
        content: "　\n\t".to_owned(),
        ..first.clone()
    };
    assert!(
        edit_subtitle(&mut project, blank).is_err(),
        "全角空白だけの本文"
    );
    let overlapping = SubtitleCue {
        end_frame: 100,
        ..first.clone()
    };
    assert!(
        edit_subtitle(&mut project, overlapping).is_err(),
        "重なる編集"
    );
    assert_eq!(project, before);

    split_subtitle(&mut project, &first.id, 30, "split", None).unwrap();
    assert_eq!(cues(&project).len(), 3);
    // 分割は本文も分ける。区間の中の比率に最も近い句読点・改行の直後で分ける。
    assert_eq!(cues(&project)[0].content, "日本語");
    assert_eq!(cues(&project)[1].content, "次の行");
    merge_subtitle(&mut project, &first.id).unwrap();
    assert_eq!(cues(&project).len(), 2);
    assert_eq!(
        cues(&project)[0].content,
        "日本語\n次の行",
        "分割した本文の結合で元へ戻る"
    );
}

#[test]
fn split_subtitle_text_cases() {
    let pair = |a: &str, b: &str| (a.to_owned(), b.to_owned());
    let text = "はい、さっさと人間関係の今日ですからね";
    assert_eq!(
        split_subtitle_text(text, 0.3, None),
        pair("はい、", "さっさと人間関係の今日ですからね")
    );
    assert_eq!(
        split_subtitle_text(text, 0.3, Some(7)),
        pair("はい、さっさと", "人間関係の今日ですからね"),
        "カーソル位置（文字数）で分ける"
    );
    let plain = "あいうえおかきくけこ";
    for at in [None, Some(0), Some(10)] {
        assert_eq!(
            split_subtitle_text(plain, 0.5, at),
            pair("あいうえお", "かきくけこ"),
            "{at:?}"
        );
    }
    assert_eq!(
        split_subtitle_text("あ", 0.5, None),
        pair("あ", "あ"),
        "1 文字は両方に残す"
    );
}

#[test]
fn all_track_time_edit_moves_clips_and_subtitles() {
    let mut project = srt_project();
    project
        .timeline_clips
        .push(text_clip("text", "別の文字", 180));
    let mut second = text_clip("text2", "別の文字", 180);
    second.track = V2;
    second.timeline_start_frame = 120;
    project.timeline_clips.push(second);
    add_track(&mut project, TrackKind::Video).unwrap();
    validate_timeline(&project).unwrap();

    edit_timeline_time(&mut project, 30, 30, 0).unwrap();
    assert_eq!(project.timeline_clips.len(), 3);
    let right = project
        .timeline_clips
        .iter()
        .find(|clip| clip.track == V1 && clip.timeline_start_frame == 30)
        .unwrap();
    assert_eq!(right.source_out_frame, 120);
    assert_eq!(project.clip("text2").unwrap().timeline_start_frame, 90);
    assert_eq!(cues(&project).last().unwrap().start_frame, 30);

    let before = project.clone();
    assert!(edit_timeline_time(&mut project, -1, 0, 20).is_err());
    assert_eq!(project, before, "不正な時間編集は変えない");
    edit_timeline_time(&mut project, 45, 0, 15).unwrap();
    assert_eq!(project.timeline_clips.len(), 4, "挿入境界で分割");
    let before = project.clone();
    edit_timeline_time(&mut project, 50, 0, 0).unwrap();
    assert_eq!(project, before, "0 frame の時間編集は分割しない");
}

fn linked_pair() -> (TimelineClip, TimelineClip) {
    let mut video = TimelineClip {
        id: "video".to_owned(),
        name: "映像".to_owned(),
        media_path: PathBuf::from("素材.mp4"),
        source_fps: fps(60, 1),
        source_frame_count: 180,
        source_out_frame: 180,
        link_group_id: "pair".to_owned(),
        ..TimelineClip::default()
    };
    video.effects.opacity_keys = vec![ClipKeyframe::new(0, 0.0), ClipKeyframe::new(179, 100.0)];
    let mut audio = video.clone();
    audio.id = "audio".to_owned();
    audio.kind = TimelineClipKind::Audio;
    audio.track = A1;
    audio.effects.opacity_keys.clear();
    (video, audio)
}

#[test]
fn all_track_time_edit_splits_linked_media() {
    let (video, audio) = linked_pair();
    let mut linked = Project::default();
    linked.timeline_clips = vec![video.clone(), audio];
    linked.video_tracks[0].muted = true;
    linked.audio_tracks[0].muted = true;
    attach_fixture_media(&mut linked);
    validate_timeline(&linked).unwrap();
    edit_timeline_time(&mut linked, 60, 30, 0).unwrap();
    let clips = &linked.timeline_clips;
    assert_eq!(clips.len(), 4);
    let link = |id: &str| linked.clip(id).unwrap().link_group_id.clone();
    let right = |kind: TimelineClipKind| {
        clips
            .iter()
            .find(|clip| clip.kind == kind && clip.timeline_start_frame == 60)
            .unwrap()
    };
    let (video_right, audio_right) = (
        right(TimelineClipKind::Video),
        right(TimelineClipKind::Audio),
    );
    assert_eq!(link("video"), link("audio"));
    assert_eq!(video_right.link_group_id, audio_right.link_group_id);
    assert_ne!(
        link("video"),
        video_right.link_group_id,
        "分割した右側は別のリンク"
    );
    assert_eq!(video_right.source_in_frame, 90);
    assert!(
        (video_right.effects.evaluated_at(0).opacity_percent - 90.0 * 100.0 / 179.0).abs() < 1e-8,
        "後半の keyframe 曲線を保つ"
    );

    // 素材 frame で表せない境界は操作全体を拒否する。
    let mut different = video;
    different.link_group_id.clear();
    different.effects.opacity_keys.clear();
    different.source_fps = fps(30, 1);
    let mut unrepresentable = Project::default();
    unrepresentable.timeline_clips = vec![different];
    attach_fixture_media(&mut unrepresentable);
    validate_timeline(&unrepresentable).unwrap();
    let before = unrepresentable.clone();
    assert!(edit_timeline_time(&mut unrepresentable, 1, 0, 1).is_err());
    assert_eq!(unrepresentable, before);
}

/// 文字起こし元の clip（[60, 180)）と、リンクした字幕 2 つ・手入力の字幕 1 つ。
fn speech_source() -> Project {
    let mut project = Project::default();
    let mut speech = text_clip("speech", "発話", 120);
    speech.timeline_start_frame = 60;
    project.timeline_clips = vec![speech];
    let project = with_cues(
        project,
        vec![
            cue("a", 50, 90, "前半", "speech"),
            cue("b", 100, 170, "後半", "speech"),
            cue("free", 300, 330, "手入力", ""),
        ],
    );
    validate_timeline(&project).unwrap();
    project
}

#[test]
fn linked_subtitles_follow_moves_and_deletes() {
    let source = speech_source();
    let start = |project: &Project, id: &str| cue_span(project, id).0;
    let mut moved = source.clone();
    move_clip(&mut moved, "speech", V1, 90).unwrap();
    assert_eq!(
        (
            start(&moved, "a"),
            start(&moved, "b"),
            start(&moved, "free")
        ),
        (80, 130, 300)
    );
    let mut moved = source.clone();
    move_clips(
        &mut moved,
        &ids(&["speech"]),
        "speech",
        V1,
        90,
        LinkMode::Single,
        None,
    )
    .unwrap();
    assert_eq!(moved.timeline_clips[0].timeline_start_frame, 90);
    assert_eq!(start(&moved, "a"), 50, "Alt の移動では字幕を追従させない");
    let mut moved = source.clone();
    move_clip(&mut moved, "speech", V1, 0).unwrap();
    assert_eq!(
        moved.timeline_clips[0].timeline_start_frame, 10,
        "リンク字幕が 0 より前へ出ない位置で止める"
    );
    assert_eq!(start(&moved, "a"), 0);
    let mut moved = source.clone();
    assert!(move_clip(&mut moved, "speech", V1, 210).is_err());
    assert_eq!(moved, source, "他の字幕と重なる移動は全体を拒否");
    let mut moved = source.clone();
    delete_timeline_clip(&mut moved, 0).unwrap();
    assert_eq!(cues(&moved).len(), 1);
    assert_eq!(cues(&moved)[0].id, "free", "clip の削除でリンク字幕も消す");
}

#[test]
fn linked_subtitles_follow_split_and_unlink() {
    let source = speech_source();
    let mut moved = source.clone();
    split_timeline_clips(
        &mut moved,
        &ids(&["speech"]),
        100,
        &mut || "right".to_owned(),
        LinkMode::Linked,
    )
    .unwrap();
    assert_eq!(cues(&moved)[0].link_clip_id, "speech");
    assert_eq!(
        cues(&moved)[1].link_clip_id,
        "right",
        "分割位置以降の字幕は右半分へ付け替える"
    );

    // 分割位置を跨ぐ字幕は 2 つに分け、後半を右半分へリンクする。
    let mut moved = source.clone();
    split_timeline_clips(
        &mut moved,
        &ids(&["speech"]),
        80,
        &mut prefixed_ids("split"),
        LinkMode::Linked,
    )
    .unwrap();
    let all = cues(&moved);
    assert_eq!(all.len(), 4);
    assert_eq!(
        (
            all[0].id.as_str(),
            all[0].end_frame,
            all[0].link_clip_id.as_str(),
            all[0].content.as_str()
        ),
        ("a", 80, "speech", "前")
    );
    assert_eq!(
        (
            all[1].start_frame,
            all[1].end_frame,
            all[1].content.as_str()
        ),
        (80, 90, "半")
    );
    assert_ne!(all[1].link_clip_id, "speech");
    assert_eq!(all[1].link_clip_id, all[2].link_clip_id);
    let right = all[1].link_clip_id.clone();
    let index = index_of(&moved, &right);
    delete_timeline_clip(&mut moved, index).unwrap();
    assert_eq!(
        cues(&moved).len(),
        2,
        "右半分の clip を消すと、その上の字幕だけが消える"
    );
    assert_eq!(
        (cues(&moved)[0].id.as_str(), cues(&moved)[0].end_frame),
        ("a", 80)
    );

    let mut moved = source.clone();
    unlink_timeline_clip(&mut moved, "speech").unwrap();
    assert!(
        cues(&moved).iter().all(|cue| cue.link_clip_id.is_empty()),
        "リンク解除で字幕のリンクも外す"
    );
    assert!(
        unlink_timeline_clip(&mut moved, "speech").is_err(),
        "リンクの無い clip の解除は拒否"
    );
}

/// 音声 clip（素材 [20, 140) → timeline [0, 120)）と、リンクした字幕 2 つ・手入力の字幕 1 つ。
fn voice_media() -> Project {
    let mut project = Project::default();
    project.timeline_clips = vec![TimelineClip {
        id: "voice".to_owned(),
        name: "音声".to_owned(),
        media_path: PathBuf::from("素材.mp4"),
        source_fps: fps(60, 1),
        source_frame_count: 300,
        source_in_frame: 20,
        source_out_frame: 140,
        ..TimelineClip::default()
    }];
    attach_fixture_media(&mut project);
    let project = with_cues(
        project,
        vec![
            cue("a", 20, 30, "前", "voice"),
            cue("b", 70, 90, "後", "voice"),
            cue("free", 300, 330, "手入力", ""),
        ],
    );
    validate_timeline(&project).unwrap();
    project
}

// 時間の対応を変える clip の編集では、リンクした字幕が素材の時刻（音声）に合わせて動く。
#[test]
fn linked_subtitles_follow_source_time() {
    let media = voice_media();
    let mut edited = media.clone();
    set_clip_speed_duration(&mut edited, "voice", &speed_edit(1, 2), LinkMode::Linked).unwrap();
    assert_eq!(cue_span(&edited, "a"), (40, 60), "50% で 2 倍の位置・長さ");
    assert_eq!(cue_span(&edited, "b"), (140, 180));
    assert_eq!(cue_span(&edited, "free").0, 300);

    let mut edited = media.clone();
    trim_timeline_clip(&mut edited, "voice", TrimEdge::Right, -55, LinkMode::Linked).unwrap();
    assert!(
        find_cue(&edited, "b").is_none(),
        "切り落とした音声の字幕は消す"
    );
    assert_eq!(cue_span(&edited, "a"), (20, 30));
    let mut edited = media.clone();
    trim_timeline_clip(&mut edited, "voice", TrimEdge::Right, -45, LinkMode::Linked).unwrap();
    assert_eq!(
        cue_span(&edited, "b"),
        (70, 75),
        "trim 位置を跨ぐ字幕は縮める"
    );
    let mut edited = media.clone();
    trim_timeline_clip(&mut edited, "voice", TrimEdge::Left, 25, LinkMode::Linked).unwrap();
    assert_eq!(edited.timeline_clips[0].timeline_start_frame, 25);
    assert_eq!(cue_span(&edited, "a"), (25, 30));
    assert_eq!(cue_span(&edited, "b"), (70, 90));
    let mut edited = media.clone();
    trim_timeline_clip(&mut edited, "voice", TrimEdge::Left, 35, LinkMode::Linked).unwrap();
    assert!(find_cue(&edited, "a").is_none());

    let mut edited = media.clone();
    slip_timeline_clip(&mut edited, "voice", 10, LinkMode::Linked).unwrap();
    let slipped = edited.timeline_clips[0].source_in_frame - 20;
    assert_ne!(slipped, 0);
    assert_eq!(
        cue_span(&edited, "a").0,
        20 - slipped,
        "スリップでは同じ音声の位置へ動かす"
    );
    assert_eq!(cue_span(&edited, "b").0, 70 - slipped);

    // 丸めると 0 frame になるリンク字幕は黙って消さずに編集を拒否する。
    let mut tiny = media.clone();
    tiny.subtitles
        .as_mut()
        .unwrap()
        .cues
        .insert(1, cue("tiny", 40, 44, "短い", "voice"));
    let before = tiny.clone();
    let error = set_clip_speed_duration(&mut tiny, "voice", &speed_edit(10, 1), LinkMode::Linked)
        .unwrap_err();
    assert!(error.message().contains("1 frame"), "{error}");
    assert_eq!(tiny, before);

    // 置き直した字幕が他の字幕と重なる編集は全体を拒否する。
    let mut crowded = media;
    let track = crowded.subtitles.as_mut().unwrap();
    track.cues.push(cue("next", 45, 50, "次", ""));
    track.cues.sort_by_key(|cue| cue.start_frame);
    let before = crowded.clone();
    let error = set_clip_speed_duration(&mut crowded, "voice", &speed_edit(1, 2), LinkMode::Linked)
        .unwrap_err();
    assert!(error.message().contains("重なる"), "{error}");
    assert_eq!(crowded, before);
}

// フレーム保持は保持位置への時間の挿入でもある。字幕も clip と同じだけ後ろへ送る。
#[test]
fn frame_hold_shifts_subtitles() {
    let mut held = voice_media();
    held.subtitles.as_mut().unwrap().cues = vec![
        cue("cross", 45, 55, "前後", "voice"),
        cue("after", 60, 70, "後", "voice"),
        cue("free", 100, 110, "手入力", ""),
    ];
    insert_frame_hold(&mut held, "voice", 50, 20, &mut prefixed_ids("hold")).unwrap();
    let right = held
        .timeline_clips
        .iter()
        .find(|clip| {
            clip.id != "voice" && clip.frame_hold.is_none() && clip.timeline_start_frame == 70
        })
        .expect("保持の右側の clip")
        .id
        .clone();
    assert_eq!(cue_span(&held, "after"), (80, 90));
    assert_eq!(find_cue(&held, "after").unwrap().link_clip_id, right);
    assert_eq!(cue_span(&held, "free").0, 120, "未リンクの字幕も送る");
    assert_eq!(cue_span(&held, "cross"), (45, 50));
    let cross_right = cues(&held)
        .iter()
        .find(|cue| cue.start_frame == 70)
        .expect("跨ぐ字幕の後半");
    assert_eq!(cross_right.end_frame, 75);
    assert_eq!(cross_right.link_clip_id, right);
}

// 全 track の時間編集やリップルトリムが他の clip を二分したら、右側の音声の字幕は右側の clip へ付け替える。
#[test]
fn bisected_clip_relinks_right_subtitles() {
    let mut rippled = voice_media();
    rippled.subtitles.as_mut().unwrap().cues = vec![
        cue("left", 20, 30, "左", "voice"),
        cue("right", 100, 110, "右", "voice"),
    ];
    edit_timeline_time(&mut rippled, 80, 20, 0).unwrap();
    let right_piece = rippled
        .timeline_clips
        .iter()
        .find(|clip| clip.id != "voice" && clip.timeline_start_frame == 80)
        .unwrap()
        .id
        .clone();
    assert_eq!(cue_span(&rippled, "right").0, 80);
    assert_eq!(
        find_cue(&rippled, "right").unwrap().link_clip_id,
        right_piece
    );
    assert_eq!(find_cue(&rippled, "left").unwrap().link_clip_id, "voice");
    let mut without_left = rippled.clone();
    delete_timeline_clip(&mut without_left, 0).unwrap();
    assert!(
        find_cue(&without_left, "right").is_some() && find_cue(&without_left, "left").is_none()
    );
    let mut without_right = rippled.clone();
    let index = index_of(&without_right, &right_piece);
    delete_timeline_clip(&mut without_right, index).unwrap();
    assert!(
        find_cue(&without_right, "right").is_none() && find_cue(&without_right, "left").is_some()
    );

    // 別 track の clip のリップルトリムでも、横切られた長い clip の右側の字幕を落とさない。
    let mut trimmed = voice_media();
    trimmed.subtitles.as_mut().unwrap().cues = vec![cue("right", 100, 110, "右", "voice")];
    let mut other = text_clip("other", "別トラック", 100);
    other.track = V2;
    trimmed.timeline_clips.push(other);
    add_track(&mut trimmed, TrackKind::Video).unwrap();
    validate_timeline(&trimmed).unwrap();
    ripple_trim_timeline_clip(
        &mut trimmed,
        "other",
        TrimEdge::Right,
        -20,
        LinkMode::Linked,
    )
    .unwrap();
    let trimmed_right = trimmed
        .timeline_clips
        .iter()
        .find(|clip| clip.id != "voice" && clip.id != "other" && clip.timeline_start_frame == 80)
        .unwrap()
        .id
        .clone();
    assert_eq!(cue_span(&trimmed, "right").0, 80);
    assert_eq!(
        find_cue(&trimmed, "right").unwrap().link_clip_id,
        trimmed_right
    );
    let mut without_left = trimmed.clone();
    let index = index_of(&without_left, "voice");
    delete_timeline_clip(&mut without_left, index).unwrap();
    assert!(find_cue(&without_left, "right").is_some());
    let mut without_right = trimmed;
    let index = index_of(&without_right, &trimmed_right);
    delete_timeline_clip(&mut without_right, index).unwrap();
    assert!(find_cue(&without_right, "right").is_none());
}

// 時間削除で clip が丸ごと消えても、区間の外へはみ出したリンク字幕は残し、リンクだけ外す。
#[test]
fn swallowed_clip_keeps_overhanging_subtitle() {
    let mut swallowed = voice_media();
    swallowed.timeline_clips[0].timeline_start_frame = 50; // timeline [50, 170)
    swallowed.subtitles.as_mut().unwrap().cues = vec![cue("tail", 160, 190, "はみ出し", "voice")];
    validate_timeline(&swallowed).unwrap();
    edit_timeline_time(&mut swallowed, 50, 120, 0).unwrap();
    assert!(swallowed.timeline_clips.is_empty());
    assert_eq!(cue_span(&swallowed, "tail"), (50, 70));
    assert!(
        find_cue(&swallowed, "tail")
            .unwrap()
            .link_clip_id
            .is_empty()
    );
}

#[test]
fn subtitle_links_in_json_and_validation() {
    let source = speech_source();
    let path = json_path("linked.mvm");
    let text = serialize_project_json(&source, &path).unwrap();
    assert!(
        text.contains("\"link_clip_id\": \"speech\""),
        "字幕リンクを保存"
    );
    assert_eq!(
        parse_project_json(&text, &path).unwrap(),
        source,
        "字幕リンクの JSON 往復"
    );

    let mut dangling = source.clone();
    dangling.subtitles.as_mut().unwrap().cues[0].link_clip_id = "missing".to_owned();
    assert!(
        validate_timeline(&dangling).is_err(),
        "存在しない clip へのリンク"
    );
    let dangling_text = text.replacen(
        "\"link_clip_id\": \"speech\"",
        "\"link_clip_id\": \"missing\"",
        1,
    );
    assert_ne!(dangling_text, text);
    assert!(
        parse_project_json(&dangling_text, &path).is_err(),
        "存在しないリンク先の JSON"
    );

    // 素材の削除などで消えた clip へのリンクだけを外す。
    let mut orphaned = source;
    orphaned.timeline_clips.clear();
    finalize_timeline_candidate(&mut orphaned).unwrap();
    assert_eq!(cues(&orphaned).len(), 3);
    assert!(cues(&orphaned)[0].link_clip_id.is_empty());
}

#[test]
fn unlinking_by_clip_set() {
    let mut project = speech_source();
    let clips: HashSet<String> = ["speech".to_owned()].into();
    assert_eq!(linked_subtitle_start(&project, &clips), Some(50));
    assert_eq!(unlink_subtitles(&mut project, &clips), 2);
    assert_eq!(linked_subtitle_start(&project, &clips), None);
}
