//! 指定 frame へ戻る decode が、帯の番号と一致するか。

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::Path;

use mvm_gpu::{Compositor, Layer, LayerParams, LayerPixels, SourceKind};
use mvm_media::{DecodedTextureFormat, ImportedFrame, open_d3d12va};
use mvm_testkit::{read_frame_marker, smoke_dir};

#[test]
#[ignore = "workstation"]
fn decode_source_frame_matches_markers_when_seeking_back_and_forward() {
    let path = smoke_dir().join("v1080p60_h264.mp4");
    require_file(&path);
    let compositor = Compositor::new().expect("DX12 device がありません");
    let mut decoder = open_d3d12va(compositor.device(), &path).expect("h264 の D3D12VA");
    // 40 → 41 は続けて読み、41 → 250 は前方へ seek する。
    for index in [0_i64, 17, 0, 40, 41, 250, 2] {
        let frame = decoder
            .decode_source_frame(index)
            .expect("指定 frame を decode");
        assert_eq!(frame.index, index);
        let rgba = compositor
            .composite(
                frame.width,
                frame.height,
                &[Layer {
                    params: params_for(&frame),
                    pixels: pixels_for(&frame),
                }],
            )
            .expect("合成");
        let stride = usize::try_from(frame.width).expect("幅") * 4;
        let marker = read_frame_marker(&rgba, frame.width, frame.height, stride).expect("帯");
        let expected = u16::try_from(index).expect("frame 番号");
        assert_eq!(marker, expected, "frame {index}");
    }
}

fn params_for(frame: &ImportedFrame) -> LayerParams {
    let kind = match frame.format {
        DecodedTextureFormat::Nv12 => SourceKind::Nv12Texture,
        DecodedTextureFormat::P010 => SourceKind::P010Texture,
    };
    let mut params = LayerParams::full(kind, frame.width, frame.height);
    params.matrix = frame.color.matrix;
    params.range = frame.color.range;
    params
}

fn pixels_for(frame: &ImportedFrame) -> LayerPixels<'_> {
    match frame.format {
        DecodedTextureFormat::Nv12 => LayerPixels::Nv12Texture {
            texture: &frame.texture,
            array_layer: frame.array_layer,
        },
        DecodedTextureFormat::P010 => LayerPixels::P010Texture {
            texture: &frame.texture,
            array_layer: frame.array_layer,
        },
    }
}

fn require_file(path: &Path) {
    assert!(
        path.is_file(),
        "検証素材がありません ({}). 先に .\\dev.ps1 testmedia を実行してください",
        path.display()
    );
}
