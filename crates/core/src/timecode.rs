//! `HH:MM:SS:FF` 形式の timecode。frame 数と有理数 fps の間で変換する。
//!
//! FF の桁は fps を最も近い整数に丸めた公称値で数える（drop frame は扱わない）。

/// 有理数 fps を最も近い整数へ丸めた公称 fps。不正な fps では `None`。
fn nominal_fps(fps_num: i64, fps_den: i64) -> Option<i64> {
    if fps_num <= 0 || fps_den <= 0 {
        return None;
    }
    let (num, den) = (i128::from(fps_num), i128::from(fps_den));
    let rounded = (2 * num + den) / (2 * den);
    i64::try_from(rounded).ok().filter(|fps| *fps > 0)
}

/// frame 数を timecode にする。負の frame は 0 として扱う。不正な fps では `None`。
pub fn format_timecode(frames: i64, fps_num: i64, fps_den: i64) -> Option<String> {
    let fps = nominal_fps(fps_num, fps_den)?;
    let frames = frames.max(0);
    let seconds = frames / fps;
    Some(format!(
        "{:02}:{:02}:{:02}:{:02}",
        seconds / 3600,
        (seconds / 60) % 60,
        seconds % 60,
        frames % fps
    ))
}

/// timecode を frame 数にする。形式・範囲・fps のいずれかが不正なら `None`。
pub fn parse_timecode(text: &str, fps_num: i64, fps_den: i64) -> Option<i64> {
    let fps = nominal_fps(fps_num, fps_den)?;
    let mut parts = [0_i64; 4];
    let mut fields = text.split(':');
    for part in &mut parts {
        let field = fields.next()?;
        if field.is_empty() || !field.bytes().all(|byte| byte.is_ascii_digit()) {
            return None;
        }
        *part = field.parse().ok()?;
    }
    if fields.next().is_some() {
        return None;
    }
    let [hours, minutes, seconds, frame] = parts;
    if minutes >= 60 || seconds >= 60 || frame >= fps {
        return None;
    }
    let total = ((i128::from(hours) * 60 + i128::from(minutes)) * 60 + i128::from(seconds))
        * i128::from(fps)
        + i128::from(frame);
    i64::try_from(total).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_with_nominal_fps() {
        assert_eq!(
            format_timecode(1800, 30000, 1001).as_deref(),
            Some("00:01:00:00")
        );
        assert_eq!(
            format_timecode(3600 * 60 + 61, 60, 1).as_deref(),
            Some("01:00:01:01")
        );
        assert_eq!(format_timecode(-5, 60, 1).as_deref(), Some("00:00:00:00"));
    }

    #[test]
    fn rejects_invalid_fps() {
        assert_eq!(format_timecode(10, 0, 1), None);
        assert_eq!(format_timecode(10, 60, 0), None);
        assert_eq!(parse_timecode("00:00:00:00", -60, 1), None);
    }

    #[test]
    fn parses_round_trip() {
        assert_eq!(parse_timecode("00:01:00:00", 30000, 1001), Some(1800));
        assert_eq!(parse_timecode("01:00:01:01", 60, 1), Some(3600 * 60 + 61));
    }

    #[test]
    fn rejects_malformed_text() {
        for text in [
            "00:00:00:30",
            "00:60:00:00",
            "00:00:60:00",
            "00:00:00:-1",
            "00:00:00:+1",
            "broken",
            "00:00:00",
            "00:00:00:00:00",
            "00::00:00",
            "",
        ] {
            assert_eq!(parse_timecode(text, 30000, 1001), None, "{text:?}");
        }
    }

    #[test]
    fn rejects_overflow() {
        assert_eq!(parse_timecode("9223372036854775807:00:00:00", 60, 1), None);
    }
}
