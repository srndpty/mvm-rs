//! clip の fade 係数。

/// clip-local frame だけを根拠に fade 係数（0..=1）を求める。
/// timeline frame や preview の出力序数を渡してはならない。範囲外・不整合な入力は 0。
pub fn clip_fade_factor(
    local_frame: i64,
    clip_duration: i64,
    fade_in_frames: i64,
    fade_out_frames: i64,
) -> f64 {
    let valid = clip_duration > 0
        && (0..clip_duration).contains(&local_frame)
        && (0..=clip_duration).contains(&fade_in_frames)
        && (0..=clip_duration).contains(&fade_out_frames)
        && fade_in_frames <= clip_duration - fade_out_frames;
    if !valid {
        return 0.0;
    }
    let fade_in = edge_factor(local_frame, fade_in_frames);
    let fade_out = edge_factor(clip_duration - 1 - local_frame, fade_out_frames);
    fade_in.min(fade_out)
}

/// 端からの距離に対する係数。fade の最初の frame は 0、fade の最後の frame で 1 に達する。
fn edge_factor(distance_from_edge: i64, fade_frames: i64) -> f64 {
    if fade_frames <= 0 || distance_from_edge >= fade_frames {
        return 1.0;
    }
    if distance_from_edge < 0 || fade_frames == 1 {
        return 0.0;
    }
    distance_from_edge as f64 / (fade_frames - 1) as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ramps_from_zero_to_one_inside_the_fade() {
        // 10 frame の clip に 3 frame の fade in: 0, 0.5, 1, 1, ...
        assert_eq!(clip_fade_factor(0, 10, 3, 0), 0.0);
        assert_eq!(clip_fade_factor(1, 10, 3, 0), 0.5);
        assert_eq!(clip_fade_factor(2, 10, 3, 0), 1.0);
        assert_eq!(clip_fade_factor(9, 10, 3, 0), 1.0);
        // 3 frame の fade out: 最後の frame が 0。
        assert_eq!(clip_fade_factor(9, 10, 0, 3), 0.0);
        assert_eq!(clip_fade_factor(8, 10, 0, 3), 0.5);
        assert_eq!(clip_fade_factor(7, 10, 0, 3), 1.0);
        // 1 frame の fade はその frame が 0。
        assert_eq!(clip_fade_factor(0, 10, 1, 0), 0.0);
        assert_eq!(clip_fade_factor(1, 10, 1, 0), 1.0);
    }

    #[test]
    fn rejects_inconsistent_input() {
        assert_eq!(clip_fade_factor(0, 0, 0, 0), 0.0);
        assert_eq!(clip_fade_factor(-1, 10, 0, 0), 0.0);
        assert_eq!(clip_fade_factor(10, 10, 0, 0), 0.0);
        assert_eq!(clip_fade_factor(5, 10, -1, 0), 0.0);
        assert_eq!(clip_fade_factor(5, 10, 11, 0), 0.0);
        assert_eq!(clip_fade_factor(5, 10, 6, 5), 0.0);
        assert_eq!(clip_fade_factor(5, 10, 5, 5), 1.0);
    }
}
