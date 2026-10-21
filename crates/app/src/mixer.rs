//! オーディオミキサー: track の音量・定位・名前、再生中のメーター、モニター音量。
//!
//! 音量・定位・名前は Project に保存し、離したときに 1 回だけ commit する。
//! 再生中に変えたら、今の位置から新しい mix で再生を続ける。

use mvm_preview::MeterPeaks;
use mvm_project::timeline::set_audio_track_mix;

use crate::error::AppError;
use crate::session::Session;

impl Session {
    /// audio track `index` の音量（dB）・定位（-1..=1）・ミキサーでの名前を 1 回の undo で変える。
    pub fn set_track_mix(
        &mut self,
        index: usize,
        gain_db: f64,
        pan: f64,
        mixer_name: &str,
    ) -> Result<(), AppError> {
        let resume = self.is_playing();
        self.transact(|project| set_audio_track_mix(project, index, gain_db, pan, mixer_name))?;
        if resume && !self.is_playing() {
            self.start_playback();
        }
        Ok(())
    }

    /// 直前の呼び出しから後に鳴った peak。再生していなければ全て 0。
    pub fn take_meters(&mut self) -> MeterPeaks {
        self.meter_source()
            .map(mvm_preview::PreviewEngine::take_meters)
            .unwrap_or_default()
    }

    pub fn monitor_volume(&self) -> f32 {
        self.monitor_gain
    }

    /// 再生の出口の音量（倍率、0..=4）。書き出しには掛けない。
    pub fn set_monitor_volume(&mut self, gain: f32) -> Result<(), AppError> {
        if !gain.is_finite() || !(0.0..=4.0).contains(&gain) {
            return Err(AppError::new("モニター音量が範囲外です"));
        }
        self.monitor_gain = gain;
        if let Some(engine) = self.meter_source() {
            engine
                .set_monitor_volume(gain)
                .map_err(|error| AppError::new(error.to_string()))?;
        }
        Ok(())
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use mvm_project::{Project, Track, TrackKind};

    use super::*;

    fn session() -> Session {
        let mut project = Project::default();
        project
            .audio_tracks
            .push(Track::named(TrackKind::Audio.default_track_name(1)));
        Session::from_project(project)
    }

    #[test]
    fn mix_changes_are_one_undo_step_each() {
        let mut session = session();
        session.set_track_mix(1, -6.0, -0.5, "  BGM ").unwrap();
        let track = &session.project().audio_tracks[1];
        assert_eq!(
            (
                track.mixer_gain_db,
                track.mixer_pan,
                track.mixer_name.as_str()
            ),
            (-6.0, -0.5, "BGM")
        );
        session.undo().unwrap();
        let track = &session.project().audio_tracks[1];
        assert_eq!((track.mixer_gain_db, track.mixer_pan), (0.0, 0.0));
        assert!(track.mixer_name.is_empty());
    }

    #[test]
    fn out_of_range_values_change_nothing() {
        let mut session = session();
        assert!(session.set_track_mix(0, 15.5, 0.0, "").is_err());
        assert!(session.set_track_mix(0, 0.0, 1.5, "").is_err());
        assert!(session.set_track_mix(0, f64::NAN, 0.0, "").is_err());
        assert!(session.set_track_mix(2, 0.0, 0.0, "").is_err());
        assert!(session.set_track_mix(0, 0.0, 0.0, "a\nb").is_err());
        assert!(!session.can_undo(), "失敗した編集が履歴に残りました");
        // 境界の値は通る。
        session.set_track_mix(0, -96.0, 1.0, "").unwrap();
        session.set_track_mix(0, 15.0, -1.0, "").unwrap();
    }

    #[test]
    fn monitor_volume_is_kept_without_an_engine() {
        let mut session = session();
        assert_eq!(session.monitor_volume(), 0.35);
        session.set_monitor_volume(0.25).unwrap();
        assert_eq!(session.monitor_volume(), 0.25);
        assert!(session.set_monitor_volume(-1.0).is_err());
        assert!(session.set_monitor_volume(5.0).is_err());
        assert_eq!(session.take_meters(), MeterPeaks::default());
    }
}
