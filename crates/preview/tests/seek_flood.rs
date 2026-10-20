//! 実素材で、逆方向のシャトルのように seek を描画より速く送ったとき、最後の seek への応答が待ち行列で遅れないか。
//! 1 枚の seek にかかる時間を同じ素材で測り、それとの比で判定する（絶対の秒数は debug ビルドでは意味が無い）。

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use mvm_core::Rational;
use mvm_preview::{MathLibrary, PreviewEngine};
use mvm_project::{Project, TimelineClip, TimelineClipKind, TrackRef};
use mvm_testkit::{read_frame_marker, smoke_dir};

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

/// `frame` を描いた画像が届くまでの時間。帯の frame 番号も確かめる。
fn wait_for(engine: &PreviewEngine, frame: i64, started: Instant) -> Duration {
    let image = engine
        .wait_image_until(Duration::from_secs(60), |image| image.frame == frame)
        .unwrap_or_else(|| panic!("frame {frame} が届きません"));
    let elapsed = started.elapsed();
    let stride = usize::try_from(image.width).unwrap() * 4;
    let marker = read_frame_marker(&image.rgba, image.width, image.height, stride).expect("帯");
    assert_eq!(i64::from(marker), frame, "別の frame を描きました");
    elapsed
}

fn reverse_shuttle_answers_the_last_seek_promptly(name: &str) {
    let media = smoke_dir().join(name);
    require_file(&media);
    let engine = PreviewEngine::start().expect("起動");
    engine
        .set_project(
            Arc::new(project(media)),
            Arc::new(MathLibrary::default()),
            1,
        )
        .unwrap();
    // device と decoder の初期化を済ませてから、1 枚の seek（GOP の途中へ戻る）にかかる時間を測る。
    engine.seek(250, 2).unwrap();
    wait_for(&engine, 250, Instant::now());
    let started = Instant::now();
    engine.seek(150, 3).unwrap();
    let single = wait_for(&engine, 150, started);
    // UI の 1 frame ごとに 1 つ前の frame へ seek する（逆方向シャトル）。描画より速ければ seek が溜まる。
    let mut generation = 4;
    for frame in (60..150).rev() {
        engine.seek(frame, generation).unwrap();
        generation += 1;
        std::thread::sleep(Duration::from_millis(16));
    }
    let started = Instant::now();
    engine.seek(37, generation).unwrap();
    let last = wait_for(&engine, 37, started);
    eprintln!("{name}: 1 枚 {single:?}、溜めた後の最後の seek {last:?}");
    // 間引いていれば、描いている途中の 1 枚と最後の 1 枚の分だけ待つ。溜まった seek を全部描けば数十枚分になる。
    assert!(
        last < single * 4 + Duration::from_millis(500),
        "{name}: 最後の seek まで {last:?}（1 枚 {single:?}）"
    );
}

#[test]
#[ignore = "workstation"]
fn reverse_shuttle_on_h264_answers_the_last_seek_promptly() {
    reverse_shuttle_answers_the_last_seek_promptly("v1080p60_h264.mp4");
}

#[test]
#[ignore = "workstation"]
fn reverse_shuttle_on_hevc_answers_the_last_seek_promptly() {
    reverse_shuttle_answers_the_last_seek_promptly("v1080p60_hevc.mp4");
}

fn require_file(path: &Path) {
    assert!(
        path.is_file(),
        "検証素材がありません ({}). 先に dev.ps1 testmedia を実行してください",
        path.display()
    );
}
