//! 実素材を再生中の preview を手放したとき、手放す操作が decode の終わりを待たないか。
//! Project の切り替え（session の install）は、この操作だけで前の preview を片付ける。

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use mvm_core::Rational;
use mvm_preview::{MathLibrary, PreviewEngine};
use mvm_project::{Project, TimelineClip, TimelineClipKind, TrackRef};
use mvm_testkit::smoke_dir;

/// 手放す操作にかけてよい時間。command を 1 つ送るだけなので、decode 1 枚分よりずっと短い。
const RETIRE_LIMIT: Duration = Duration::from_millis(20);

fn project(media: PathBuf) -> Project {
    Project {
        timeline_clips: vec![TimelineClip {
            kind: TimelineClipKind::Video,
            media_path: media,
            name: "clip".to_owned(),
            id: "clip".to_owned(),
            source_fps: Rational::new(60, 1),
            source_frame_count: 300,
            source_in_frame: 0,
            source_out_frame: 300,
            timeline_start_frame: 0,
            track: TrackRef::video(0),
            ..TimelineClip::default()
        }],
        ..Project::default()
    }
}

#[test]
#[ignore = "workstation"]
fn retiring_a_decoding_engine_does_not_wait_for_the_worker() {
    let media = smoke_dir().join("v4k60_hevc10.mp4");
    assert!(
        media.is_file(),
        "検証素材がありません ({}). 先に .\\dev.ps1 testmedia を実行してください",
        media.display()
    );
    let engine = PreviewEngine::start().expect("起動");
    engine
        .set_project(
            Arc::new(project(media)),
            Arc::new(MathLibrary::default()),
            1,
        )
        .unwrap();
    engine.play(0, 2).unwrap();
    engine
        .wait_image_until(Duration::from_secs(60), |image| image.frame >= 1)
        .expect("再生が始まりません");

    let started = Instant::now();
    let thread = engine.retire().expect("worker の thread");
    let retire = started.elapsed();
    while !thread.is_finished() {
        assert!(
            started.elapsed() < Duration::from_secs(30),
            "停止を頼んだ worker が止まりません"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
    let stopped = started.elapsed();
    thread.join().unwrap();
    eprintln!("手放す操作: {retire:?}、worker が止まるまで: {stopped:?}");
    assert!(
        retire < RETIRE_LIMIT,
        "手放す操作が {retire:?} かかりました（worker の停止は {stopped:?}）"
    );
}
