//! compositor が marker を崩さずに読む戻せるか。実 GPU が要る。

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use mvm_gpu::{Compositor, Layer, LayerParams, LayerPixels, SourceKind};
use mvm_testkit::{
    CELL_COUNT, CELL_SIZE, MARKER_HEIGHT, MARKER_WIDTH, read_frame_marker, read_frame_marker_at,
};

#[test]
#[ignore = "workstation"]
fn upload_paths_keep_the_frame_marker() {
    let compositor = Compositor::new().expect("DX12 device がありません");
    for frame in [0u16, 1, 0x00FF, 0x8000] {
        let nv12 = paint_nv12(frame);
        let read = composite_one(
            &compositor,
            LayerPixels::Nv12 {
                y: &nv12.y,
                uv: &nv12.uv,
            },
            LayerParams::full(SourceKind::Nv12, MARKER_WIDTH, MARKER_HEIGHT),
        );
        assert_eq!(read, frame, "NV12");

        let p010 = paint_p010(frame);
        let read = composite_one(
            &compositor,
            LayerPixels::P010 {
                y: &p010.y,
                uv: &p010.uv,
            },
            LayerParams::full(SourceKind::P010, MARKER_WIDTH, MARKER_HEIGHT),
        );
        assert_eq!(read, frame, "P010");

        let rgba = paint_rgba(frame);
        let read = composite_one(
            &compositor,
            LayerPixels::Rgba { data: &rgba },
            LayerParams::full(SourceKind::Rgba, MARKER_WIDTH, MARKER_HEIGHT),
        );
        assert_eq!(read, frame, "RGBA");
    }
}

#[test]
#[ignore = "workstation"]
fn two_layers_keep_both_marker_bands() {
    let compositor = Compositor::new().expect("DX12 device がありません");
    let left = paint_nv12(3);
    let right = paint_nv12(9);
    let mut shifted = LayerParams::full(SourceKind::Nv12, MARKER_WIDTH, MARKER_HEIGHT);
    shifted.affine[2] = -(MARKER_WIDTH as f32);
    let output_width = MARKER_WIDTH * 2;
    let rgba = compositor
        .composite(
            output_width,
            MARKER_HEIGHT,
            &[
                Layer {
                    params: LayerParams::full(SourceKind::Nv12, MARKER_WIDTH, MARKER_HEIGHT),
                    pixels: LayerPixels::Nv12 {
                        y: &left.y,
                        uv: &left.uv,
                    },
                },
                Layer {
                    params: shifted,
                    pixels: LayerPixels::Nv12 {
                        y: &right.y,
                        uv: &right.uv,
                    },
                },
            ],
        )
        .expect("合成");
    let stride = output_width as usize * 4;
    assert_eq!(
        read_frame_marker(&rgba, output_width, MARKER_HEIGHT, stride).expect("左の帯"),
        3
    );
    assert_eq!(
        read_frame_marker_at(&rgba, output_width, MARKER_HEIGHT, stride, MARKER_WIDTH, 0)
            .expect("右の帯"),
        9
    );
}

fn composite_one(compositor: &Compositor, pixels: LayerPixels<'_>, params: LayerParams) -> u16 {
    let rgba = compositor
        .composite(MARKER_WIDTH, MARKER_HEIGHT, &[Layer { params, pixels }])
        .expect("合成");
    read_frame_marker(
        &rgba,
        MARKER_WIDTH,
        MARKER_HEIGHT,
        MARKER_WIDTH as usize * 4,
    )
    .expect("marker")
}

struct Planes {
    y: Vec<u8>,
    uv: Vec<u8>,
}

fn paint_nv12(frame: u16) -> Planes {
    let width = MARKER_WIDTH as usize;
    let height = MARKER_HEIGHT as usize;
    let mut y = vec![16u8; width * height];
    let uv = vec![128u8; width * (height / 2)];
    for cell in 0..CELL_COUNT {
        if !cell_is_white(frame, cell) {
            continue;
        }
        fill_cell(&mut y, width, cell, 235);
    }
    Planes { y, uv }
}

fn paint_p010(frame: u16) -> Planes {
    let width = MARKER_WIDTH as usize;
    let height = MARKER_HEIGHT as usize;
    let mut y = vec![0u8; width * height * 2];
    let mut uv = vec![0u8; width * (height / 2) * 2];
    write_u16(&mut y, 64 << 6);
    write_u16(&mut uv, 512 << 6);
    for cell in 0..CELL_COUNT {
        if !cell_is_white(frame, cell) {
            continue;
        }
        let x0 = cell as usize * CELL_SIZE as usize;
        for row in 0..height {
            for x in x0..x0 + CELL_SIZE as usize {
                let offset = (row * width + x) * 2;
                y[offset..offset + 2].copy_from_slice(&(940u16 << 6).to_le_bytes());
            }
        }
    }
    Planes { y, uv }
}

fn paint_rgba(frame: u16) -> Vec<u8> {
    let width = MARKER_WIDTH as usize;
    let height = MARKER_HEIGHT as usize;
    let mut rgba = vec![0u8; width * height * 4];
    for pixel in rgba.as_chunks_mut::<4>().0 {
        pixel[3] = 255;
    }
    for cell in 0..CELL_COUNT {
        if !cell_is_white(frame, cell) {
            continue;
        }
        let x0 = cell as usize * CELL_SIZE as usize;
        for row in 0..height {
            for x in x0..x0 + CELL_SIZE as usize {
                let offset = (row * width + x) * 4;
                rgba[offset..offset + 3].fill(255);
            }
        }
    }
    rgba
}

fn cell_is_white(frame: u16, cell: u32) -> bool {
    match cell {
        0 | 18 => true,
        1 => false,
        index => ((frame >> (index - 2)) & 1) == 1,
    }
}

fn fill_cell(plane: &mut [u8], width: usize, cell: u32, value: u8) {
    let x0 = cell as usize * CELL_SIZE as usize;
    let height = MARKER_HEIGHT as usize;
    for row in 0..height {
        let start = row * width + x0;
        plane[start..start + CELL_SIZE as usize].fill(value);
    }
}

fn write_u16(bytes: &mut [u8], value: u16) {
    for chunk in bytes.as_chunks_mut::<2>().0 {
        chunk.copy_from_slice(&value.to_le_bytes());
    }
}
