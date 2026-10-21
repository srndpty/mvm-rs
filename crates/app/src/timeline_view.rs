//! timeline の表示範囲。画素を描かず、どの frame と clip が画面に入るだけを決める。

use mvm_project::Project;
use mvm_project::timeline::clip_interval;

/// 1 frame の幅の下限（px）。これより縮めない。
pub const MIN_PIXELS_PER_FRAME: f32 = 0.005;
/// 1 frame の幅の上限（px）。
pub const MAX_PIXELS_PER_FRAME: f32 = 24.0;
/// 100% と表示する幅。C++ 版と同じく 1 frame を 1 px にする（60 fps なら 30 秒が 1800 px）。
pub const ONE_HUNDRED_PERCENT: f32 = 1.0;
/// 倍率の段階（1 frame の幅、px）。拡大・縮小は 1 段ずつ動く。C++ 版と同じ表。
pub const ZOOM_LEVELS: [f32; 19] = [
    0.005, 0.01, 0.02, 0.05, 0.1, 0.2, 0.35, 0.5, 0.75, 1.0, 1.5, 2.0, 3.0, 4.0, 6.0, 8.0, 12.0,
    16.0, 24.0,
];
/// 開いたときの倍率（150%）。
pub const DEFAULT_PIXELS_PER_FRAME: f32 = 1.5;

/// 画面に見えている timeline frame。`first` は含む、`last` は含まない。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TimeWindow {
    pub first: i64,
    pub last: i64,
}

/// 有限な正の幅へ収める。有限でなければ既定の 100% に戻す。
pub fn clamp_pixels_per_frame(value: f32) -> f32 {
    if !value.is_finite() {
        return DEFAULT_PIXELS_PER_FRAME;
    }
    value.clamp(MIN_PIXELS_PER_FRAME, MAX_PIXELS_PER_FRAME)
}

/// `pixels_per_frame` から `steps` 段だけ拡大（負なら縮小）した倍率。段の間の値は、拡大なら上の段、縮小なら
/// 下の段へ寄せる。端より先へは動かない。
pub fn zoom_level_step(pixels_per_frame: f32, steps: i32) -> f32 {
    let current = clamp_pixels_per_frame(pixels_per_frame);
    let position = ZOOM_LEVELS
        .iter()
        .position(|level| (*level - current).abs() <= f32::EPSILON * level.max(1.0));
    let index = match position {
        Some(index) => index as i32 + steps,
        None => {
            let above = ZOOM_LEVELS
                .iter()
                .position(|level| *level > current)
                .unwrap_or(ZOOM_LEVELS.len()) as i32;
            if steps > 0 {
                above + steps - 1
            } else {
                above + steps
            }
        }
    };
    let last = ZOOM_LEVELS.len() as i32 - 1;
    ZOOM_LEVELS[index.clamp(0, last) as usize]
}

/// `scroll_px` から右へ `width_px` だけ見えている frame 範囲。
pub fn visible_window(scroll_px: f32, width_px: f32, pixels_per_frame: f32) -> TimeWindow {
    let pixels_per_frame = f64::from(clamp_pixels_per_frame(pixels_per_frame));
    let scroll = if scroll_px.is_finite() {
        f64::from(scroll_px).max(0.0)
    } else {
        0.0
    };
    let width = if width_px.is_finite() {
        f64::from(width_px).max(0.0)
    } else {
        0.0
    };
    let first = (scroll / pixels_per_frame).floor();
    let last = ((scroll + width) / pixels_per_frame).ceil();
    TimeWindow {
        first: saturating_frame(first),
        last: saturating_frame(last).max(saturating_frame(first)),
    }
}

fn saturating_frame(value: f64) -> i64 {
    if !value.is_finite() || value <= 0.0 {
        0
    } else if value >= i64::MAX as f64 {
        i64::MAX
    } else {
        value as i64
    }
}

/// `[start, end)` が見えている範囲と重なるか。空の区間は重ならない。
pub fn ranges_overlap(start: i64, end: i64, window: TimeWindow) -> bool {
    start < end && start < window.last && end > window.first
}

/// 目盛りの間の最小の幅（px）。C++ 版と同じ。
const RULER_MIN_GAP: f64 = 70.0;

/// 目盛りの間隔（frame）。隣の目盛りが [`RULER_MIN_GAP`] 以上離れる、fps に沿ったきりのよい値。
pub fn ruler_step(pixels_per_frame: f32, nominal_fps: i64) -> i64 {
    let pixels_per_frame = f64::from(clamp_pixels_per_frame(pixels_per_frame));
    let raw = (RULER_MIN_GAP / pixels_per_frame).ceil().max(1.0);
    let raw = saturating_frame(raw).max(1);
    let fps = nominal_fps.max(1);
    const MULTIPLIERS: [i64; 9] = [1, 2, 5, 10, 30, 60, 300, 900, 3600];
    let mut steps = vec![1, 5, 10, 15, 30];
    for multiplier in MULTIPLIERS {
        if let Some(step) = fps.checked_mul(multiplier) {
            steps.push(step);
        }
    }
    steps.sort_unstable();
    steps.dedup();
    steps.into_iter().find(|step| *step >= raw).unwrap_or(raw)
}

/// 横スクロールの中身の長さ（frame）。clip が無くても 10 秒は見せる。区間を計算できない clip は数えない。
pub fn display_span_frames(project: &Project) -> i64 {
    let mut end = 0i64;
    for clip in &project.timeline_clips {
        if let Ok((_, clip_end)) = clip_interval(project, clip) {
            end = end.max(clip_end);
        }
    }
    let fps = project.timeline_fps;
    let ten_seconds = if fps.den > 0 {
        fps.num.saturating_mul(10) / fps.den
    } else {
        0
    };
    end.max(ten_seconds).max(1)
}

#[cfg(test)]
mod tests {
    use mvm_core::Rational;
    use mvm_project::{Project, TimelineClip, TimelineClipKind, TrackRef};

    use super::*;

    fn text_clip(name: &str, start: i64, duration: i64) -> TimelineClip {
        TimelineClip {
            kind: TimelineClipKind::Text,
            name: name.to_owned(),
            id: format!("id-{name}"),
            source_fps: Rational::new(60, 1),
            source_frame_count: duration,
            source_in_frame: 0,
            source_out_frame: duration,
            timeline_start_frame: start,
            track: TrackRef::video(0),
            ..TimelineClip::default()
        }
    }

    #[test]
    fn window_is_half_open_and_rejects_non_overlapping_clips() {
        let window = visible_window(80.0, 80.0, 8.0);
        assert_eq!(
            window,
            TimeWindow {
                first: 10,
                last: 20
            }
        );
        assert!(ranges_overlap(10, 20, window));
        assert!(ranges_overlap(19, 25, window));
        assert!(!ranges_overlap(0, 10, window), "終端ちょうどは見えない");
        assert!(!ranges_overlap(20, 30, window), "先頭ちょうどは見えない");
        assert!(!ranges_overlap(12, 12, window), "空の区間は見えない");
    }

    #[test]
    fn non_finite_zoom_returns_to_the_default() {
        assert_eq!(clamp_pixels_per_frame(f32::NAN), 1.5);
        assert_eq!(clamp_pixels_per_frame(0.0), 0.005);
        assert_eq!(clamp_pixels_per_frame(-3.0), 0.005);
        assert_eq!(clamp_pixels_per_frame(24.0), 24.0);
        assert_eq!(clamp_pixels_per_frame(1000.0), 24.0);
    }

    #[test]
    fn zoom_moves_one_level_at_a_time_and_stops_at_the_ends() {
        assert_eq!(zoom_level_step(1.5, 1), 2.0);
        assert_eq!(zoom_level_step(1.5, -1), 1.0);
        assert_eq!(zoom_level_step(1.5, 2), 3.0);
        assert_eq!(zoom_level_step(24.0, 1), 24.0);
        assert_eq!(zoom_level_step(0.005, -1), 0.005);
        // 段の間（1.2）からは、拡大なら 1.5、縮小なら 1.0 へ。
        assert_eq!(zoom_level_step(1.2, 1), 1.5);
        assert_eq!(zoom_level_step(1.2, -1), 1.0);
    }

    #[test]
    fn ruler_step_picks_the_smallest_nice_gap() {
        // 70 / 8 = 8.75。9 以上の候補 1,5,10,15,30,60,… の最小は 10。
        assert_eq!(ruler_step(8.0, 60), 10);
        // 150% は 1 秒 90 px なので 1 秒ごと。
        assert_eq!(ruler_step(1.5, 60), 60);
        // 70 / 0.25 = 280。60*2 = 120 は足りず、60*5 = 300。
        assert_eq!(ruler_step(0.25, 60), 300);
    }

    #[test]
    fn span_ignores_a_clip_whose_interval_cannot_be_computed() {
        let broken = {
            let mut clip = text_clip("bad", 0, 10);
            clip.source_fps = Rational::new(0, 1);
            clip
        };
        let mut project = Project {
            timeline_clips: vec![broken],
            ..Project::default()
        };
        assert_eq!(display_span_frames(&project), 600);

        project.timeline_clips.push(text_clip("long", 500, 200));
        assert_eq!(display_span_frames(&project), 700);
    }
}
