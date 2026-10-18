//! 書き出しの残り時間の推定と表記。

/// 起動直後の数 frame は encoder の立ち上がりで速度が大きく揺れる。1 秒未満の観測では推定しない。
const MINIMUM_OBSERVATION_SECONDS: f64 = 1.0;

/// i64 へ変換できる上限（排他的）。`i64::MAX as f64` は 2^63 へ丸められる。
const SECONDS_CONVERSION_LIMIT: f64 = i64::MAX as f64;

/// 書き出しの残り時間（秒）を推定する。
///
/// `baseline_completed` は最初に届いた進捗で、準備時間（素材の open など）を速度から除外するために使う。
/// 推定に足る観測が無い間は `None` を返し、0 秒や不定値へ縮退させない。
pub fn estimate_export_remaining_seconds(
    baseline_completed: i64,
    completed: i64,
    total: i64,
    elapsed_since_baseline_seconds: f64,
) -> Option<f64> {
    if total <= 0
        || baseline_completed < 0
        || completed < baseline_completed
        || completed > total
        || !elapsed_since_baseline_seconds.is_finite()
        || elapsed_since_baseline_seconds < 0.0
    {
        return None;
    }
    if completed == total {
        return Some(0.0);
    }
    let advanced = completed - baseline_completed;
    if advanced <= 0 || elapsed_since_baseline_seconds < MINIMUM_OBSERVATION_SECONDS {
        return None;
    }
    let seconds_per_frame = elapsed_since_baseline_seconds / advanced as f64;
    let estimate = seconds_per_frame * (total - completed) as f64;
    // 有限な入力でも積は溢れ得る。表示できない推定値は出さない。
    (estimate.is_finite() && estimate < SECONDS_CONVERSION_LIMIT).then_some(estimate)
}

/// 残り秒数を利用者向けの表記にする。例: "残り約 5秒"、"残り約 1分05秒"、"残り約 1時間02分"。
/// 端数は切り上げ、0 以下と非有限値は 0 秒、i64 を超える値は上限で頭打ちにする。
pub fn format_export_remaining(remaining_seconds: f64) -> String {
    let seconds = if remaining_seconds.is_finite() && remaining_seconds > 0.0 {
        // f64 → i64 の `as` は範囲外を飽和させる。
        remaining_seconds.ceil() as i64
    } else {
        0
    };
    if seconds < 60 {
        format!("残り約 {seconds}秒")
    } else if seconds < 3600 {
        format!("残り約 {}分{:02}秒", seconds / 60, seconds % 60)
    } else {
        format!(
            "残り約 {}時間{:02}分",
            seconds / 3600,
            (seconds % 3600) / 60
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // 期待値は実装の式を使わず、手計算した値を直接書く。
    fn near(value: Option<f64>, expected: f64) -> bool {
        value.is_some_and(|value| (value - expected).abs() < 1e-9)
    }

    #[test]
    fn estimates_from_baseline_speed() {
        // 2 秒で 60 frame → 残り 60 frame は 2 秒。
        assert!(near(
            estimate_export_remaining_seconds(0, 60, 120, 2.0),
            2.0
        ));
        // 基準点 30 から 3 秒で 30 frame → 残り 90 frame は 9 秒。
        assert!(near(
            estimate_export_remaining_seconds(30, 60, 150, 3.0),
            9.0
        ));
        assert!(near(
            estimate_export_remaining_seconds(0, 120, 120, 5.0),
            0.0
        ));
    }

    #[test]
    fn withholds_estimate_without_enough_observation() {
        assert_eq!(estimate_export_remaining_seconds(10, 10, 120, 5.0), None);
        assert_eq!(estimate_export_remaining_seconds(0, 60, 120, 0.5), None);
        assert_eq!(estimate_export_remaining_seconds(0, 60, 0, 2.0), None);
        assert_eq!(estimate_export_remaining_seconds(-1, 60, 120, 2.0), None);
        assert_eq!(estimate_export_remaining_seconds(70, 60, 120, 2.0), None);
        assert_eq!(estimate_export_remaining_seconds(0, 130, 120, 2.0), None);
        assert_eq!(
            estimate_export_remaining_seconds(0, 60, 120, f64::NAN),
            None
        );
        assert_eq!(estimate_export_remaining_seconds(0, 1, 100, 1e308), None);
        // 1e19 秒 > i64::MAX（約 9.22e18）。
        assert_eq!(estimate_export_remaining_seconds(0, 1, 2, 1e19), None);
        // 2^63 ちょうども拒否する。
        assert_eq!(
            estimate_export_remaining_seconds(0, 1, 2, 9_223_372_036_854_775_808.0),
            None
        );
    }

    #[test]
    fn formats_remaining_time() {
        assert_eq!(format_export_remaining(0.0), "残り約 0秒");
        assert_eq!(format_export_remaining(4.2), "残り約 5秒");
        assert_eq!(format_export_remaining(59.0), "残り約 59秒");
        assert_eq!(format_export_remaining(60.0), "残り約 1分00秒");
        assert_eq!(format_export_remaining(125.0), "残り約 2分05秒");
        assert_eq!(format_export_remaining(3599.0), "残り約 59分59秒");
        assert_eq!(format_export_remaining(3600.0), "残り約 1時間00分");
        assert_eq!(format_export_remaining(3725.0), "残り約 1時間02分");
        assert_eq!(format_export_remaining(-3.0), "残り約 0秒");
        assert_eq!(format_export_remaining(f64::INFINITY), "残り約 0秒");
        // 9223372036854775807 秒 = 2562047788015215 時間 + 1807 秒（30 分 7 秒）。
        assert_eq!(
            format_export_remaining(1e300),
            "残り約 2562047788015215時間30分"
        );
        assert_eq!(
            format_export_remaining(9_223_372_036_854_775_808.0),
            "残り約 2562047788015215時間30分"
        );
    }
}
