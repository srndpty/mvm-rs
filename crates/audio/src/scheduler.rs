//! 音声 sample から、次に出す映像 frame を決める。
//!
//! qualified な 60 fps / 48 kHz では 1 frame = 800 sample。式は output timebase の
//! 有理数換算だけで、800 を別の定数として持たない。

use mvm_core::output_timebase::OutputTimebase;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScheduleAction {
    Hold,
    Request,
    CatchUp,
    End,
    ClockRegression,
    Invalid,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScheduleDecision {
    pub action: ScheduleAction,
    pub target_frame: i64,
    pub skipped_frames: i64,
}

/// `pending_seek` が 0 以上のあいだは、その frame を通常の追いつきより先に 1 回だけ要求する。
/// 表示した側が pending を消す。`video_frame_count` は出してよい frame の本数（終端は含まない）。
pub fn schedule_video(
    timebase: OutputTimebase,
    audio_sample: i64,
    last_displayed: i64,
    last_requested: i64,
    video_frame_count: i64,
    pending_seek: i64,
) -> ScheduleDecision {
    if audio_sample < 0
        || last_displayed < -1
        || last_requested < -1
        || video_frame_count <= 0
        || pending_seek < -1
    {
        return invalid();
    }
    let Ok(target) = timebase.output_frame(audio_sample) else {
        return invalid();
    };
    if pending_seek >= 0 {
        if pending_seek >= video_frame_count || pending_seek <= last_displayed {
            return invalid();
        }
        return ScheduleDecision {
            action: ScheduleAction::Request,
            target_frame: pending_seek,
            skipped_frames: 0,
        };
    }
    if target >= video_frame_count {
        return ScheduleDecision {
            action: ScheduleAction::End,
            target_frame: target,
            skipped_frames: 0,
        };
    }
    if target < last_displayed {
        return ScheduleDecision {
            action: ScheduleAction::ClockRegression,
            target_frame: target,
            skipped_frames: 0,
        };
    }
    if target == last_displayed || target == last_requested {
        return ScheduleDecision {
            action: ScheduleAction::Hold,
            target_frame: target,
            skipped_frames: 0,
        };
    }
    let previous = if last_requested >= 0 {
        last_requested
    } else {
        last_displayed
    };
    let skipped = if previous >= 0 && target > previous {
        target - previous - 1
    } else {
        0
    };
    ScheduleDecision {
        action: if skipped > 0 {
            ScheduleAction::CatchUp
        } else {
            ScheduleAction::Request
        },
        target_frame: target,
        skipped_frames: skipped,
    }
}

/// 表示した frame が、その瞬間の音声より 2 frame 以上先なら true。
pub fn video_is_ahead(timebase: OutputTimebase, video_frame: i64, audio_sample: i64) -> bool {
    if video_frame < 0 || audio_sample < 0 {
        return true;
    }
    match timebase.output_frame(audio_sample) {
        Ok(target) => video_frame > target.saturating_add(1),
        Err(_) => true,
    }
}

fn invalid() -> ScheduleDecision {
    ScheduleDecision {
        action: ScheduleAction::Invalid,
        target_frame: -1,
        skipped_frames: 0,
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use mvm_core::Rational;
    use mvm_core::output_timebase::OutputTimebase;

    use super::*;

    fn qualified() -> OutputTimebase {
        OutputTimebase::qualified(Rational::new(60, 1), 48_000).unwrap()
    }

    #[test]
    fn one_qualified_frame_is_800_samples() {
        let timebase = qualified();
        assert_eq!(timebase.first_audio_sample(1).unwrap(), 800);
        assert_eq!(timebase.output_frame(799).unwrap(), 0);
        assert_eq!(timebase.output_frame(800).unwrap(), 1);
        let decision = schedule_video(timebase, 800, 0, 0, 3_600, -1);
        assert_eq!(
            decision,
            ScheduleDecision {
                action: ScheduleAction::Request,
                target_frame: 1,
                skipped_frames: 0,
            }
        );
    }

    #[test]
    fn holds_requests_and_skips_from_the_sample_clock() {
        let timebase = qualified();
        assert_eq!(
            schedule_video(timebase, 0, -1, -1, 100, -1).action,
            ScheduleAction::Request
        );
        assert_eq!(
            schedule_video(timebase, 0, 0, 0, 100, -1).action,
            ScheduleAction::Hold
        );
        let catch_up = schedule_video(timebase, 2_400, 0, 0, 100, -1);
        assert_eq!(
            catch_up,
            ScheduleDecision {
                action: ScheduleAction::CatchUp,
                target_frame: 3,
                skipped_frames: 2,
            }
        );
        assert_eq!(
            schedule_video(timebase, 800 * 100, 0, 0, 100, -1).action,
            ScheduleAction::End
        );
        assert_eq!(
            schedule_video(timebase, 0, 5, 5, 100, -1).action,
            ScheduleAction::ClockRegression
        );
    }

    #[test]
    fn pending_seek_is_requested_once_and_invalid_input_is_rejected() {
        let timebase = qualified();
        assert_eq!(
            schedule_video(timebase, 0, 1, 1, 100, 4),
            ScheduleDecision {
                action: ScheduleAction::Request,
                target_frame: 4,
                skipped_frames: 0,
            }
        );
        assert_eq!(
            schedule_video(timebase, 0, 4, 4, 100, 4).action,
            ScheduleAction::Invalid
        );
        assert_eq!(
            schedule_video(timebase, 0, 0, 0, 100, 100).action,
            ScheduleAction::Invalid
        );
        assert_eq!(
            schedule_video(timebase, -1, 0, 0, 100, -1).action,
            ScheduleAction::Invalid
        );
        assert_eq!(
            schedule_video(timebase, 0, -2, 0, 100, -1).action,
            ScheduleAction::Invalid
        );
        assert_eq!(
            schedule_video(timebase, 0, 0, 0, 0, -1).action,
            ScheduleAction::Invalid
        );
    }

    #[test]
    fn thirty_fps_uses_1600_samples_and_ahead_frames_are_caught() {
        let thirty = OutputTimebase::configured(Rational::new(30, 1), 48_000).unwrap();
        assert_eq!(thirty.first_audio_sample(1).unwrap(), 1_600);
        assert_eq!(schedule_video(thirty, 1_600, 0, 0, 100, -1).target_frame, 1);
        let timebase = qualified();
        assert!(video_is_ahead(timebase, 2, 0));
        assert!(!video_is_ahead(timebase, 1, 0));
        assert!(!video_is_ahead(timebase, 0, 0));
        assert!(video_is_ahead(timebase, -1, 0));
    }
}
