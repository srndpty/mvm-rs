//! compositor が marker を崩さずに読む戻せるか。実 GPU が要る。

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use mvm_gpu::{Compositor, Layer, LayerParams, LayerPixels, SourceKind, YuvMatrix, YuvRange};
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

#[test]
#[ignore = "workstation"]
fn yuv_matrix_and_range_change_rgb() {
    let compositor = Compositor::new().expect("DX12 device がありません");
    let y = 81.0;
    let cb = 90.0;
    let cr = 240.0;
    for matrix in [YuvMatrix::Bt601, YuvMatrix::Bt709, YuvMatrix::Bt2020] {
        let pixel = composite_solid(
            &compositor,
            SourceKind::Nv12,
            [y as u16, cb as u16, cr as u16],
            matrix,
            YuvRange::Limited,
        );
        assert_rgb(
            &pixel,
            reference_rgb(matrix, false, 8, y, cb, cr),
            &format!("{matrix:?}"),
        );
    }
    let limited = composite_solid(
        &compositor,
        SourceKind::Nv12,
        [16, 128, 128],
        YuvMatrix::Bt709,
        YuvRange::Limited,
    );
    assert_eq!(&limited[0..3], &[0, 0, 0]);
    let full = composite_solid(
        &compositor,
        SourceKind::Nv12,
        [16, 128, 128],
        YuvMatrix::Bt709,
        YuvRange::Full,
    );
    assert!(full[0] > 8, "full range の Y=16 は黒ではない: {full:?}");
    let p010 = composite_solid(
        &compositor,
        SourceKind::P010,
        [324, 360, 960],
        YuvMatrix::Bt601,
        YuvRange::Limited,
    );
    assert_rgb(
        &p010,
        reference_rgb(YuvMatrix::Bt601, false, 10, 324.0, 360.0, 960.0),
        "P010 BT.601",
    );
}

fn composite_solid(
    compositor: &Compositor,
    kind: SourceKind,
    codes: [u16; 3],
    matrix: YuvMatrix,
    range: YuvRange,
) -> [u8; 4] {
    let mut params = LayerParams::full(kind, 2, 2);
    params.matrix = matrix;
    params.range = range;
    let planes = match kind {
        SourceKind::Nv12 => solid_nv12(codes[0] as u8, codes[1] as u8, codes[2] as u8),
        SourceKind::P010 => solid_p010(codes[0], codes[1], codes[2]),
        SourceKind::Rgba | SourceKind::Nv12Texture | SourceKind::P010Texture => {
            panic!("solid 試験は upload の NV12 / P010 だけ")
        }
    };
    let pixels = match kind {
        SourceKind::Nv12 => LayerPixels::Nv12 {
            y: &planes.y,
            uv: &planes.uv,
        },
        SourceKind::P010 => LayerPixels::P010 {
            y: &planes.y,
            uv: &planes.uv,
        },
        SourceKind::Rgba | SourceKind::Nv12Texture | SourceKind::P010Texture => {
            panic!("solid 試験は upload の NV12 / P010 だけ")
        }
    };
    let rgba = compositor
        .composite(2, 2, &[Layer { params, pixels }])
        .expect("合成");
    rgba[0..4].try_into().expect("pixel")
}

fn solid_nv12(y: u8, u: u8, v: u8) -> Planes {
    Planes {
        y: vec![y; 4],
        uv: vec![u, v],
    }
}

fn solid_p010(y: u16, u: u16, v: u16) -> Planes {
    let mut plane_y = Vec::new();
    let stored_y = (y << 6).to_le_bytes();
    for _ in 0..4 {
        plane_y.extend_from_slice(&stored_y);
    }
    let mut uv = Vec::new();
    uv.extend_from_slice(&(u << 6).to_le_bytes());
    uv.extend_from_slice(&(v << 6).to_le_bytes());
    Planes { y: plane_y, uv }
}

fn assert_rgb(pixel: &[u8], expected: [f64; 3], what: &str) {
    for (index, expected) in expected.iter().enumerate() {
        let quantized = (expected * 255.0).round() as i32;
        let actual = i32::from(pixel[index]);
        let delta = (actual - quantized).abs();
        assert!(
            delta <= 2,
            "{what} ch{index}: got {actual}, expected {quantized}"
        );
    }
}

fn reference_rgb(matrix: YuvMatrix, full: bool, bits: u32, y: f64, cb: f64, cr: f64) -> [f64; 3] {
    let (kr, kb) = match matrix {
        YuvMatrix::Bt601 => (0.299, 0.114),
        YuvMatrix::Bt709 => (0.2126, 0.0722),
        YuvMatrix::Bt2020 => (0.2627, 0.0593),
    };
    let kg = 1.0 - kr - kb;
    let (yy, cb, cr) = if bits == 8 && !full {
        (
            (y - 16.0) / 219.0,
            (cb - 128.0) / 224.0,
            (cr - 128.0) / 224.0,
        )
    } else if bits == 8 {
        (y / 255.0, (cb - 128.0) / 255.0, (cr - 128.0) / 255.0)
    } else if !full {
        (
            (y - 64.0) / 876.0,
            (cb - 512.0) / 896.0,
            (cr - 512.0) / 896.0,
        )
    } else {
        (y / 1023.0, (cb - 512.0) / 1023.0, (cr - 512.0) / 1023.0)
    };
    let red = yy + 2.0 * (1.0 - kr) * cr;
    let green = yy - 2.0 * kb * (1.0 - kb) / kg * cb - 2.0 * kr * (1.0 - kr) / kg * cr;
    let blue = yy + 2.0 * (1.0 - kb) * cb;
    [
        red.clamp(0.0, 1.0),
        green.clamp(0.0, 1.0),
        blue.clamp(0.0, 1.0),
    ]
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
