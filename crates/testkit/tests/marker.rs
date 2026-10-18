#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use mvm_testkit::{
    CELL_COUNT, CELL_SIZE, MARKER_HEIGHT, MARKER_WIDTH, MarkerError, read_frame_marker,
    read_frame_marker_at,
};

fn paint(frame: u16, width: usize, height: usize) -> Vec<u8> {
    let mut rgba = vec![0u8; width * height * 4];
    for cell in 0..CELL_COUNT as usize {
        let on = match cell {
            0 | 18 => true,
            1 => false,
            index => ((frame >> (index - 2)) & 1) == 1,
        };
        let luma = if on { 235 } else { 16 };
        let x0 = cell * CELL_SIZE as usize;
        for y in 0..MARKER_HEIGHT as usize {
            for x in x0..x0 + CELL_SIZE as usize {
                let pixel = (y * width + x) * 4;
                rgba[pixel] = luma;
                rgba[pixel + 1] = luma;
                rgba[pixel + 2] = luma;
                rgba[pixel + 3] = 255;
            }
        }
    }
    rgba
}

#[test]
fn reads_frame_numbers_from_the_band() {
    let width = MARKER_WIDTH as usize;
    let height = MARKER_HEIGHT as usize;
    for frame in [0u16, 1, 0x00FF, 0x8000, u16::MAX] {
        let rgba = paint(frame, width, height);
        let read = read_frame_marker(&rgba, MARKER_WIDTH, MARKER_HEIGHT, width * 4).unwrap();
        assert_eq!(read, frame, "frame {frame}");
    }
}

#[test]
fn reads_a_band_that_is_not_at_the_origin() {
    let width = (MARKER_WIDTH as usize) * 2;
    let height = MARKER_HEIGHT as usize;
    let mut rgba = vec![0u8; width * height * 4];
    let band = paint(42, MARKER_WIDTH as usize, MARKER_HEIGHT as usize);
    let origin_x = 1216usize;
    for y in 0..MARKER_HEIGHT as usize {
        let src = y * MARKER_WIDTH as usize * 4;
        let dst = (y * width + origin_x) * 4;
        rgba[dst..dst + MARKER_WIDTH as usize * 4]
            .copy_from_slice(&band[src..src + MARKER_WIDTH as usize * 4]);
    }
    let read = read_frame_marker_at(
        &rgba,
        width as u32,
        height as u32,
        width * 4,
        origin_x as u32,
        0,
    )
    .unwrap();
    assert_eq!(read, 42);
}

#[test]
fn rejects_a_band_with_broken_sync() {
    let width = MARKER_WIDTH as usize;
    let mut rgba = paint(7, width, MARKER_HEIGHT as usize);
    // cell 0 の中心を黒にする。番号の bit は正しいまま。
    for y in 28..36 {
        for x in 28..36 {
            let pixel = (y * width + x) * 4;
            rgba[pixel] = 0;
            rgba[pixel + 1] = 0;
            rgba[pixel + 2] = 0;
        }
    }
    assert_eq!(
        read_frame_marker(&rgba, MARKER_WIDTH, MARKER_HEIGHT, width * 4),
        Err(MarkerError::Sync)
    );
}

#[test]
fn rejects_luma_on_the_threshold_as_white() {
    let width = MARKER_WIDTH as usize;
    let mut rgba = paint(0, width, MARKER_HEIGHT as usize);
    for y in 0..MARKER_HEIGHT as usize {
        for x in 0..CELL_SIZE as usize {
            let pixel = (y * width + x) * 4;
            rgba[pixel] = 128;
            rgba[pixel + 1] = 128;
            rgba[pixel + 2] = 128;
        }
    }
    assert_eq!(
        read_frame_marker(&rgba, MARKER_WIDTH, MARKER_HEIGHT, width * 4),
        Err(MarkerError::Sync)
    );
}

#[test]
fn rejects_an_image_narrower_than_the_band() {
    let rgba = vec![235u8; 100 * 64 * 4];
    assert_eq!(
        read_frame_marker(&rgba, 100, 64, 100 * 4),
        Err(MarkerError::TooSmall)
    );
}
