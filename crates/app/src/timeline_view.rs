//! timeline の表示範囲。画素を描かず、どの frame と clip が画面に入るだけを決める。

use mvm_project::Project;
use mvm_project::timeline::clip_interval;

/// 1 frame の幅の下限（px）。これより縮めない。
pub const MIN_PIXELS_PER_FRAME: f32 = 0.25;
/// 1 frame の幅の上限（px）。
pub const MAX_PIXELS_PER_FRAME: f32 = 64.0;
/// 100% と表示する幅。
pub const DEFAULT_PIXELS_PER_FRAME: f32 = 8.0;

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

/// 目盛りの間隔（frame）。隣の目盛りがおよそ 96px 以上離れる、fps に沿ったきりのよい値。
pub fn ruler_step(pixels_per_frame: f32, nominal_fps: i64) -> i64 {
    let pixels_per_frame = f64::from(clamp_pixels_per_frame(pixels_per_frame));
    let raw = (96.0 / pixels_per_frame).ceil().max(1.0);
    let raw = saturating_frame(raw).max(1);
    let fps = nominal_fps.max(1);
    const MULTIPLIERS: [i64; 8] = [1, 2, 5, 10, 30, 60, 300, 3600];
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
        assert_eq!(clamp_pixels_per_frame(f32::NAN), 8.0);
        assert_eq!(clamp_pixels_per_frame(0.0), 0.25);
        assert_eq!(clamp_pixels_per_frame(-3.0), 0.25);
        assert_eq!(clamp_pixels_per_frame(64.0), 64.0);
        assert_eq!(clamp_pixels_per_frame(1000.0), 64.0);
    }

    #[test]
    fn ruler_step_picks_the_smallest_nice_gap() {
        // 96 / 8 = 12。12 以上の候補 1,5,10,15,30,60,… の最小は 15。
        assert_eq!(ruler_step(8.0, 60), 15);
        // 96 / 0.25 = 384。60*5 = 300 は足りず、60*10 = 600。
        assert_eq!(ruler_step(0.25, 60), 600);
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
