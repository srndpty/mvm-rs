//! J / K / L のシャトル。1 倍の正方向は通常の再生にし、それ以外の倍率は経過時間から求めた frame へ seek を
//! 繰り返す（映像だけ。音声は鳴らさない）。

use mvm_core::shuttle::{next_shuttle_rate, shuttle_frame_from_elapsed};
use mvm_project::timeline::timeline_end_frame;

use crate::error::AppError;
use crate::session::Session;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ShuttleState {
    rate: i32,
    base_frame: i64,
    started_ms: u64,
    /// 最後に seek した frame。playhead がこれと違えば、利用者が動かしたので止める。
    shown_frame: i64,
    /// 始めたときの Project。編集・undo で変われば、古い基準の frame で送り続けずに止める。
    revision: u64,
}

impl Session {
    /// L（`direction` = 1）・J（-1）。同じ向きなら倍速にし、逆向きなら遅くして止める。
    pub fn shuttle(&mut self, direction: i32, now_ms: u64) -> Result<(), AppError> {
        let current = match self.shuttle {
            Some(state) => state.rate,
            None if self.is_playing() => 1,
            None => 0,
        };
        let Some(next) = next_shuttle_rate(current, direction) else {
            return Err(AppError::new("シャトルの速度が不正です"));
        };
        self.shuttle = None;
        if self.is_playing() {
            self.pause_playback();
        }
        if next == 0 {
            return Ok(());
        }
        if next == 1 {
            self.start_playback();
            return Ok(());
        }
        let last = self.last_frame()?;
        let base_frame = self.playhead_frame.clamp(0, last);
        self.shuttle = Some(ShuttleState {
            rate: next,
            base_frame,
            started_ms: now_ms,
            shown_frame: self.playhead_frame,
            revision: self.edit_revision,
        });
        Ok(())
    }

    /// K。シャトルと再生を止める。
    pub fn stop_shuttle(&mut self) {
        self.shuttle = None;
        if self.is_playing() {
            self.pause_playback();
        }
    }

    /// 今の倍率。止まっていれば 0、通常の再生は 1。
    pub fn shuttle_rate(&self) -> i32 {
        match self.shuttle {
            Some(state) => state.rate,
            None if self.is_playing() => 1,
            None => 0,
        }
    }

    /// シャトル中なら経過時間の frame へ進める。続いていれば true（画面を起こし続ける）。
    pub fn poll_shuttle(&mut self, now_ms: u64) -> bool {
        let Some(mut state) = self.shuttle else {
            return false;
        };
        // 再生を始めた・playhead を動かした・Project を変えたら、シャトルをやめる。
        if self.is_playing()
            || self.playhead_frame != state.shown_frame
            || self.edit_revision != state.revision
        {
            self.shuttle = None;
            return false;
        }
        let Ok(last) = self.last_frame() else {
            self.shuttle = None;
            return false;
        };
        let elapsed_ns = i64::try_from(now_ms.saturating_sub(state.started_ms))
            .unwrap_or(i64::MAX / 1_000_000)
            .saturating_mul(1_000_000);
        let fps = self.project.timeline_fps;
        let Some(frame) = shuttle_frame_from_elapsed(
            state.base_frame.min(last),
            elapsed_ns,
            fps.num,
            fps.den,
            state.rate,
            last,
        ) else {
            self.shuttle = None;
            return false;
        };
        self.seek(frame);
        state.shown_frame = frame;
        let at_end = (state.rate < 0 && frame == 0) || (state.rate > 0 && frame == last);
        self.shuttle = (!at_end).then_some(state);
        !at_end
    }

    fn last_frame(&self) -> Result<i64, AppError> {
        let end =
            timeline_end_frame(&self.project).map_err(|error| AppError::new(error.to_string()))?;
        if end <= 0 {
            return Err(AppError::new("timeline に clip がありません"));
        }
        Ok(end - 1)
    }
}

#[cfg(test)]
mod tests {
    use mvm_core::Rational;
    use mvm_project::{Project, TextClipData, TimelineClip, TimelineClipKind, TrackRef};

    use super::*;

    fn session() -> Session {
        Session::from_project(Project {
            timeline_clips: vec![TimelineClip {
                kind: TimelineClipKind::Text,
                name: "文字".to_owned(),
                id: "t".to_owned(),
                source_fps: Rational::new(60, 1),
                source_frame_count: 180,
                source_out_frame: 180,
                track: TrackRef::video(0),
                text: TextClipData {
                    content: "文字".to_owned(),
                    ..TextClipData::default()
                },
                ..TimelineClip::default()
            }],
            ..Project::default()
        })
    }

    #[test]
    fn a_reverse_shuttle_follows_the_elapsed_time_and_stops_at_the_start() {
        let mut session = session();
        session.seek(40);
        // 止まっている所からの J は逆 1 倍（通常の再生にはしない）。
        session.shuttle(-1, 1_000).expect("逆 1 倍");
        assert_eq!(session.shuttle_rate(), -1);
        assert!(!session.is_playing());
        assert!(session.poll_shuttle(1_250));
        // 60 fps の 0.25 秒は 15 frame。
        assert_eq!(session.playhead_frame(), 25);
        assert!(!session.poll_shuttle(3_000));
        assert_eq!(session.playhead_frame(), 0);
        assert_eq!(session.shuttle_rate(), 0);
    }

    #[test]
    fn the_opposite_key_slows_the_shuttle_down() {
        let mut session = session();
        session.seek(90);
        session.shuttle(-1, 0).expect("逆");
        session.shuttle(-1, 0).expect("逆 2 倍");
        session.shuttle(-1, 0).expect("逆 4 倍");
        assert_eq!(session.shuttle_rate(), -4);
        session.shuttle(1, 0).expect("逆 2 倍へ");
        assert_eq!(session.shuttle_rate(), -2);
        session.stop_shuttle();
        assert_eq!(session.shuttle_rate(), 0);
    }

    #[test]
    fn moving_the_playhead_ends_the_shuttle() {
        let mut session = session();
        session.shuttle(-1, 0).expect("逆");
        session.shuttle(-1, 0).expect("逆 2 倍");
        assert_eq!(session.shuttle_rate(), -2);
        session.seek(100);
        assert!(!session.poll_shuttle(500));
        assert_eq!(session.shuttle_rate(), 0);
        assert_eq!(session.playhead_frame(), 100);
    }

    #[test]
    fn rapid_shuttle_keys_leave_the_playhead_on_the_last_shown_frame() {
        let mut session = session();
        session.seek(170);
        let mut clock = 0;
        for direction in [-1, -1, -1, 1, -1, 1, 1] {
            session.shuttle(direction, clock).expect("シャトル");
            clock += 20;
            session.poll_shuttle(clock);
            clock += 20;
        }
        // J ×3（-4 倍）→ L（-2）→ J（-4）→ L（-2）→ L（-1）。
        assert_eq!(session.shuttle_rate(), -1);
        session.poll_shuttle(clock);
        let shown = session.playhead_frame();
        session.stop_shuttle();
        // 止めた後は時間が経っても playhead が動かない。
        assert!(!session.poll_shuttle(clock + 5_000));
        assert_eq!(session.playhead_frame(), shown);
        assert!(shown < 170);
    }

    #[test]
    fn an_edit_during_the_shuttle_ends_it() {
        let mut session = session();
        session.seek(170);
        session.shuttle(-1, 0).expect("逆");
        assert!(session.poll_shuttle(100));
        let shown = session.playhead_frame();
        // playhead を動かさない編集（マーカーの追加）でも、シャトルは止める。
        session.add_marker().expect("マーカー");
        assert!(!session.poll_shuttle(1_000));
        assert_eq!(session.shuttle_rate(), 0);
        assert_eq!(session.playhead_frame(), shown);
    }

    #[test]
    fn an_empty_timeline_has_nothing_to_shuttle() {
        let mut session = Session::new();
        assert!(session.shuttle(-1, 0).is_err());
        assert_eq!(session.shuttle_rate(), 0);
    }
}
