//! 停止中の preview が、要求した frame の帯を出すか。

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::Path;

use mvm_core::Rational;
use mvm_preview::{StillPreview, plan_frame};
use mvm_project::{Project, TimelineClip, TimelineClipKind, TrackRef};
use mvm_testkit::{read_frame_marker, smoke_dir};

#[test]
#[ignore = "workstation"]
fn stopped_preview_shows_the_requested_frame() {
    let media = smoke_dir().join("v1080p60_h264.mp4");
    require_file(&media);
    let project = Project {
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
    };
    let mut preview = StillPreview::new().expect("DX12 device がありません");
    for frame in [0_i64, 17, 2, 17] {
        let plan = plan_frame(&project, frame, &mvm_preview::MathLibrary::default()).expect("計画");
        let image = preview.render(&project, frame, &plan).expect("合成");
        assert_eq!((image.width, image.height), (1920, 1080));
        let stride = usize::try_from(image.width).expect("幅") * 4;
        let marker = read_frame_marker(&image.rgba, image.width, image.height, stride).expect("帯");
        let expected = u16::try_from(frame).expect("frame 番号");
        assert_eq!(marker, expected, "frame {frame}");
    }
}

fn require_file(path: &Path) {
    assert!(
        path.is_file(),
        "検証素材がありません ({}). 先に .\\dev.ps1 testmedia を実行してください",
        path.display()
    );
}
