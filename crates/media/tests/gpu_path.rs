//! D3D12VA の texture を wgpu へ渡して、2 本の 1080p60 を 60fps で合成できるか。
//! 採否は release ビルドの所要時間と marker の一致で決める。

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::Path;
use std::time::Instant;

use mvm_gpu::{Compositor, Layer, LayerParams, LayerPixels, SourceKind};
use mvm_media::{ProbedMedia, open_d3d12va, probe_media_file};
use mvm_testkit::{MARKER_WIDTH, read_frame_marker, read_frame_marker_at, smoke_dir};

const FRAMES: u16 = 60;

#[test]
#[ignore = "workstation"]
fn d3d12va_two_1080p_layers_match_markers() {
    let h264 = smoke_dir().join("v1080p60_h264.mp4");
    let hevc = smoke_dir().join("v1080p60_hevc.mp4");
    require_file(&h264);
    require_file(&hevc);
    match probe_media_file(&h264).expect("h264 を probe") {
        ProbedMedia::Video(video) => {
            assert_eq!((video.width, video.height), (1920, 1080));
            assert_eq!((video.fps_num, video.fps_den), (60, 1));
            assert_eq!(video.frame_count, 300);
            assert!(video.has_audio);
        }
        other => panic!("h264 は動画です: {other:?}"),
    }

    let compositor = Compositor::new().expect("DX12 device がありません");
    let mut h264_decoder = open_d3d12va(compositor.device(), &h264).expect("h264 の D3D12VA");
    let mut hevc_decoder = open_d3d12va(compositor.device(), &hevc).expect("hevc の D3D12VA");
    let output_width = MARKER_WIDTH * 2;
    let output_height = 1080;
    let started = Instant::now();
    for expected in 0..FRAMES {
        let left = h264_decoder
            .decode_frame()
            .expect("h264 decode")
            .expect("h264 の frame が尽きました");
        let right = hevc_decoder
            .decode_frame()
            .expect("hevc decode")
            .expect("hevc の frame が尽きました");
        assert_eq!(left.index, i64::from(expected));
        assert_eq!(right.index, i64::from(expected));
        let mut shifted = LayerParams::full(SourceKind::Nv12Texture, right.width, right.height);
        shifted.affine[2] = -(MARKER_WIDTH as f32);
        let rgba = compositor
            .composite(
                output_width,
                output_height,
                &[
                    Layer {
                        params: LayerParams::full(SourceKind::Nv12Texture, left.width, left.height),
                        pixels: LayerPixels::Nv12Texture {
                            texture: &left.texture,
                            array_layer: left.array_layer,
                        },
                    },
                    Layer {
                        params: shifted,
                        pixels: LayerPixels::Nv12Texture {
                            texture: &right.texture,
                            array_layer: right.array_layer,
                        },
                    },
                ],
            )
            .expect("合成");
        let stride = output_width as usize * 4;
        let left_marker =
            read_frame_marker(&rgba, output_width, output_height, stride).expect("左の帯");
        let right_marker =
            read_frame_marker_at(&rgba, output_width, output_height, stride, MARKER_WIDTH, 0)
                .expect("右の帯");
        assert_eq!(left_marker, expected, "h264 marker");
        assert_eq!(right_marker, expected, "hevc marker");
    }
    let elapsed = started.elapsed();
    eprintln!("D3D12VA 2 layer x {FRAMES} frames: {elapsed:?}");
    if !cfg!(debug_assertions) {
        assert!(
            elapsed.as_secs_f64() <= 1.0,
            "release で 60 frame の合成が 1 秒を超えました: {elapsed:?}"
        );
    }
}

fn require_file(path: &Path) {
    assert!(
        path.is_file(),
        "検証素材がありません ({}). 先に .\\dev.ps1 testmedia を実行してください",
        path.display()
    );
}
