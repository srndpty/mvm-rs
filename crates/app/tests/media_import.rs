//! 実素材の取り込みと配置。FFmpeg で検証素材を調べるので workstation 試験にする（`.\dev.ps1 testmedia` が要る）。

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::PathBuf;

use mvm_app::Session;
use mvm_project::{MediaKind, TimelineClipKind, TrackRef};
use mvm_testkit::smoke_dir;

fn smoke(name: &str) -> PathBuf {
    let path = smoke_dir().join(name);
    assert!(
        path.is_file(),
        "検証素材がありません ({}). 先に dev.ps1 testmedia を実行してください",
        path.display()
    );
    path
}

#[test]
#[ignore = "workstation"]
fn importing_registers_each_file_once_with_its_probed_values() {
    let mut session = Session::new();
    let video = smoke("v1080p60_h264.mp4");
    let audio = smoke("wav_48k.wav");
    let report = session
        .import_media_files(&[video.clone(), audio.clone()], "")
        .expect("読み込み");
    assert_eq!((report.imported, report.skipped), (2, 0));
    assert!(report.failures.is_empty());
    let items = &session.project().media_items;
    assert_eq!(items.len(), 2);
    assert_eq!(items[0].kind, MediaKind::Video);
    assert_eq!((items[0].width, items[0].height), (1920, 1080));
    assert_eq!(items[0].frame_count, 300);
    assert_eq!(items[1].kind, MediaKind::Audio);
    assert_eq!(items[1].sample_rate, 48_000);
    // 同じファイルをもう一度読んでも増やさない。
    let again = session
        .import_media_files(&[video, PathBuf::from("Z:/無い.mp4")], "")
        .expect("再読み込み");
    assert_eq!((again.imported, again.skipped), (0, 1));
    assert_eq!(again.failures.len(), 1);
    assert!(again.message().contains("無い.mp4"), "{}", again.message());
    assert_eq!(session.project().media_items.len(), 2);
    // 読み込みは timeline に置かない。
    assert!(session.project().timeline_clips.is_empty());
}

#[test]
#[ignore = "workstation"]
fn a_video_with_sound_is_placed_as_a_linked_pair_at_the_playhead() {
    let mut session = Session::new();
    session.seek(30);
    session
        .add_media_file_at_playhead(&smoke("v1080p60_h264.mp4"))
        .expect("配置");
    let clips = &session.project().timeline_clips;
    assert_eq!(clips.len(), 2);
    let video = clips
        .iter()
        .find(|clip| clip.kind == TimelineClipKind::Video)
        .expect("映像");
    let audio = clips
        .iter()
        .find(|clip| clip.kind == TimelineClipKind::Audio)
        .expect("音声");
    assert_eq!(video.track, TrackRef::video(0));
    assert_eq!(audio.track, TrackRef::audio(0));
    assert_eq!(video.timeline_start_frame, 30);
    assert_eq!(audio.timeline_start_frame, 30);
    assert_eq!(video.link_group_id, audio.link_group_id);
    assert_eq!(video.source_out_frame, 300);
    assert_eq!(session.selection(), std::slice::from_ref(&video.id));
    // 1 回の操作は 1 段の undo。
    session.undo().expect("戻す");
    assert!(session.project().timeline_clips.is_empty());
    assert!(session.project().media_items.is_empty());
}

#[test]
#[ignore = "workstation"]
fn panel_items_dropped_on_the_timeline_are_lined_up_from_the_drop_point() {
    let mut session = Session::new();
    session
        .import_media_files(&[smoke("wav_48k.wav")], "")
        .expect("読み込み");
    let id = session.project().media_items[0].id.clone();
    session
        .place_media_items_at(&[id.clone(), id], TrackRef::audio(0), 12)
        .expect("ドロップ");
    let clips = &session.project().timeline_clips;
    assert_eq!(clips.len(), 2);
    // 同じファイルは 1 つの素材を共有する。
    assert_eq!(session.project().media_items.len(), 1);
    assert_eq!(clips[0].timeline_start_frame, 12);
    // 60 fps の 5 秒の音声は 300 frame。2 つ目はその直後へ並ぶ。
    assert_eq!(clips[0].source_out_frame, 300);
    assert_eq!(clips[1].timeline_start_frame, 312);
    assert!(clips.iter().all(|clip| clip.track == TrackRef::audio(0)));
}

#[test]
#[ignore = "workstation"]
fn a_panel_item_is_placed_and_removing_it_takes_its_clips() {
    let mut session = Session::new();
    session
        .import_media_files(&[smoke("png_alpha.png")], "")
        .expect("読み込み");
    let id = session.project().media_items[0].id.clone();
    session.add_media_item_at_playhead(&id).expect("配置");
    let clip = &session.project().timeline_clips[0];
    assert_eq!(clip.kind, TimelineClipKind::Image);
    let plan = session
        .plan_media_removal(std::slice::from_ref(&id))
        .unwrap();
    assert_eq!(plan.clip_count, 1);
    session.remove_media_entries(&plan).expect("削除");
    assert!(session.project().timeline_clips.is_empty());
    assert!(session.project().media_items.is_empty());
}

#[test]
#[ignore = "workstation"]
fn the_waveform_of_a_wav_is_decoded_in_the_background() {
    let mut session = Session::new();
    let wav = smoke("wav_48k.wav");
    // 波形は timeline にある素材の分だけを持つ。
    session.add_media_file_at_playhead(&wav).expect("配置");
    let placed = session.project().timeline_clips[0].media_path.clone();
    assert!(session.waveform(&placed).is_none());
    let mut peaks = None;
    for step in 0..2_000_u64 {
        session.poll_waveforms(step * 10);
        peaks = session.waveform(&placed);
        if peaks.is_some() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    let peaks = peaks.expect("波形");
    assert_eq!(peaks.sample_rate, 48_000);
    assert_eq!(peaks.channels, 2);
    // 1 kHz / 500 Hz の正弦波なので、どの区間でも振幅を持つ。
    let column = mvm_core::waveform_peaks::waveform_column(&peaks, 0, 1.0, 1.1).expect("列");
    assert!(column.maximum > 0.1 && column.minimum < -0.1, "{column:?}");
}
