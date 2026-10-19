//! J / K / L のシャトル。速度の段階と、経過時間から求める timeline の frame。

/// シャトルの最大の倍率。
pub const MAXIMUM_SHUTTLE_RATE: i32 = 16;

fn is_shuttle_rate(rate: i32) -> bool {
    rate != 0 && rate.unsigned_abs().is_power_of_two() && rate.abs() <= MAXIMUM_SHUTTLE_RATE
}

/// J（`direction` = -1）・L（+1）を押したときの次の倍率。同じ向きなら倍にし（16 倍まで）、逆向きなら半分にして
/// 1 倍から更に逆へ押すと止める（0）。止まっているときは ±1 倍から始める。不正な入力は `None`。
pub fn next_shuttle_rate(current: i32, direction: i32) -> Option<i32> {
    if !matches!(direction, -1 | 1) || (current != 0 && !is_shuttle_rate(current)) {
        return None;
    }
    if current == 0 {
        return Some(direction);
    }
    if (current > 0) == (direction > 0) {
        return Some(direction * (current.abs() * 2).min(MAXIMUM_SHUTTLE_RATE));
    }
    if current.abs() == 1 {
        return Some(0);
    }
    Some(current.signum() * (current.abs() / 2))
}

/// 経過時間 `elapsed_ns` の後の frame。`base_frame` から `rate` 倍で進み（負なら戻り）、0 と `last_frame` で
/// 止まる。frame は経過時間から切り捨てで数える。
pub fn shuttle_frame_from_elapsed(
    base_frame: i64,
    elapsed_ns: i64,
    fps_num: i64,
    fps_den: i64,
    rate: i32,
    last_frame: i64,
) -> Option<i64> {
    if !is_shuttle_rate(rate)
        || base_frame < 0
        || last_frame < 0
        || base_frame > last_frame
        || elapsed_ns < 0
        || fps_num <= 0
        || fps_den <= 0
    {
        return None;
    }
    let advanced = i128::from(elapsed_ns) * i128::from(fps_num) * i128::from(rate.abs())
        / (1_000_000_000_i128 * i128::from(fps_den));
    let advanced = i64::try_from(advanced).unwrap_or(i64::MAX);
    Some(if rate > 0 {
        base_frame + advanced.min(last_frame - base_frame)
    } else {
        base_frame - advanced.min(base_frame)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // C++ 版 tests/harness/test_timeline_playback.cpp の testShuttleClockAndEditPoints を移植した値。

    #[test]
    fn rates_double_toward_the_pressed_side_and_halve_back_to_a_stop() {
        assert_eq!(next_shuttle_rate(0, 1), Some(1));
        assert_eq!(next_shuttle_rate(1, 1), Some(2));
        assert_eq!(next_shuttle_rate(2, 1), Some(4));
        assert_eq!(next_shuttle_rate(4, 1), Some(8));
        assert_eq!(next_shuttle_rate(8, 1), Some(16));
        assert_eq!(next_shuttle_rate(16, 1), Some(16));
        assert_eq!(next_shuttle_rate(16, -1), Some(8));
        assert_eq!(next_shuttle_rate(4, -1), Some(2));
        assert_eq!(next_shuttle_rate(1, -1), Some(0));
        assert_eq!(next_shuttle_rate(0, -1), Some(-1));
        assert_eq!(next_shuttle_rate(-1, -1), Some(-2));
        assert_eq!(next_shuttle_rate(-2, -1), Some(-4));
        assert_eq!(next_shuttle_rate(-2, 1), Some(-1));
    }

    #[test]
    fn invalid_rates_and_directions_are_rejected() {
        assert_eq!(next_shuttle_rate(3, 1), None);
        assert_eq!(next_shuttle_rate(17, 1), None);
        assert_eq!(next_shuttle_rate(0, 0), None);
        assert_eq!(next_shuttle_rate(32, -1), None);
    }

    #[test]
    fn the_frame_advances_with_the_rate_and_stops_at_the_ends() {
        assert_eq!(
            shuttle_frame_from_elapsed(40, 250_000_000, 60, 1, 2, 179),
            Some(70)
        );
        assert_eq!(
            shuttle_frame_from_elapsed(40, 250_000_000, 60, 1, -4, 179),
            Some(0)
        );
        assert_eq!(
            shuttle_frame_from_elapsed(3, 1_000_000_000, 60, 1, -4, 179),
            Some(0)
        );
        assert_eq!(
            shuttle_frame_from_elapsed(175, 1_000_000_000, 60, 1, 4, 179),
            Some(179)
        );
        assert_eq!(
            shuttle_frame_from_elapsed(0, 250_000_000, 60, 1, 16, 179),
            Some(179)
        );
        // 29.97 fps の 1 秒・2 倍は 59.94 frame なので 59。
        assert_eq!(
            shuttle_frame_from_elapsed(0, 1_000_000_000, 30_000, 1001, 2, 1000),
            Some(59)
        );
    }

    #[test]
    fn invalid_shuttle_inputs_are_rejected() {
        assert_eq!(shuttle_frame_from_elapsed(0, 0, 60, 1, 0, 179), None);
        assert_eq!(shuttle_frame_from_elapsed(0, 0, 60, 1, 17, 179), None);
        assert_eq!(shuttle_frame_from_elapsed(0, 0, 60, 1, 3, 179), None);
        assert_eq!(shuttle_frame_from_elapsed(180, 0, 60, 1, 2, 179), None);
        assert_eq!(shuttle_frame_from_elapsed(0, -1, 60, 1, 2, 179), None);
        assert_eq!(shuttle_frame_from_elapsed(0, 0, 60, 0, 2, 179), None);
    }
}
