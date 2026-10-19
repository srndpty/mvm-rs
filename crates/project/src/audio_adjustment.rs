//! 自動音量調整（ラウドネス正規化と BGM の ducking）の設定・解析入力の射影・減衰カーブ。
//! 音声の測定（ラウドネス・true peak）は audio crate が担い、ここは Project の値だけを扱う。

use std::collections::{BTreeSet, HashSet};
use std::fmt::Write as _;

use crate::clip_effects::ClipKeyframe;
use crate::error::{ProjectResult, fail};
use crate::model::{Project, TimelineClipKind};

#[derive(Clone, Debug, PartialEq)]
pub struct AudioAdjustmentSettings {
    pub voice_tracks: Vec<i32>,
    pub bgm_track: i32,
    pub normalize: bool,
    pub duck: bool,
    pub voice_lufs: f64,
    pub bgm_lufs: f64,
    pub reduction_db: f64,
    pub threshold_db: f64,
    pub attack_ms: i32,
    pub hold_ms: i32,
    pub release_ms: i32,
}

impl Default for AudioAdjustmentSettings {
    fn default() -> Self {
        Self {
            voice_tracks: Vec::new(),
            bgm_track: -1,
            normalize: true,
            duck: true,
            voice_lufs: -16.0,
            bgm_lufs: -24.0,
            reduction_db: 12.0,
            threshold_db: -35.0,
            attack_ms: 150,
            hold_ms: 300,
            release_ms: 600,
        }
    }
}

/// 声が検出された区間（48 kHz の sample）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AudioDetectedRange {
    pub start_sample: i64,
    pub end_sample: i64,
}

impl AudioAdjustmentSettings {
    pub fn validate(&self, track_count: i32) -> ProjectResult<()> {
        let bounded = |value: f64, minimum: f64, maximum: f64| {
            value.is_finite() && (minimum..=maximum).contains(&value)
        };
        let valid = (self.normalize || self.duck)
            && (0..track_count).contains(&self.bgm_track)
            && !(self.duck && self.voice_tracks.is_empty())
            && bounded(self.voice_lufs, -40.0, -5.0)
            && bounded(self.bgm_lufs, -40.0, -5.0)
            && bounded(self.reduction_db, 0.0, 60.0)
            && bounded(self.threshold_db, -96.0, 0.0)
            && (1..=10_000).contains(&self.attack_ms)
            && (0..=10_000).contains(&self.hold_ms)
            && (1..=10_000).contains(&self.release_ms);
        if !valid {
            return fail("対象トラックまたは自動音量調整の設定が範囲外です");
        }
        let mut tracks = HashSet::new();
        for track in &self.voice_tracks {
            if !(0..track_count).contains(track)
                || *track == self.bgm_track
                || !tracks.insert(*track)
            {
                return fail("声と BGM は異なるトラックを重複なく指定してください");
            }
        }
        Ok(())
    }

    /// 解析と再生成の対象になる track か（声か BGM）。
    pub fn targets_track(&self, track: i32) -> bool {
        track == self.bgm_track || self.voice_tracks.contains(&track)
    }

    /// Project の `last_audio_adjustment_settings` に書く JSON（C++ 版と同じ key）。
    pub fn to_json(&self) -> String {
        serde_json::json!({
            "voiceTracks": self.voice_tracks,
            "bgmTrack": self.bgm_track,
            "normalize": self.normalize,
            "duck": self.duck,
            "voiceLufs": self.voice_lufs,
            "bgmLufs": self.bgm_lufs,
            "reductionDb": self.reduction_db,
            "thresholdDb": self.threshold_db,
            "attackMs": self.attack_ms,
            "holdMs": self.hold_ms,
            "releaseMs": self.release_ms,
        })
        .to_string()
    }

    /// `to_json` の逆。形が違う・値が欠けたものは失敗する（既定値で埋めない）。
    pub fn from_json(text: &str) -> ProjectResult<Self> {
        #[derive(serde::Deserialize)]
        #[serde(rename_all = "camelCase", deny_unknown_fields)]
        struct Dto {
            voice_tracks: Vec<i32>,
            bgm_track: i32,
            normalize: bool,
            duck: bool,
            voice_lufs: f64,
            bgm_lufs: f64,
            reduction_db: f64,
            threshold_db: f64,
            attack_ms: i32,
            hold_ms: i32,
            release_ms: i32,
        }
        let dto: Dto = serde_json::from_str(text).map_err(|error| {
            crate::error::ProjectError::new(format!("自動音量調整の設定を読めません: {error}"))
        })?;
        Ok(Self {
            voice_tracks: dto.voice_tracks,
            bgm_track: dto.bgm_track,
            normalize: dto.normalize,
            duck: dto.duck,
            voice_lufs: dto.voice_lufs,
            bgm_lufs: dto.bgm_lufs,
            reduction_db: dto.reduction_db,
            threshold_db: dto.threshold_db,
            attack_ms: dto.attack_ms,
            hold_ms: dto.hold_ms,
            release_ms: dto.release_ms,
        })
    }
}

/// 重なり・接する区間をまとめる。空の区間は捨てる。
pub fn merge_audio_detected_ranges(mut ranges: Vec<AudioDetectedRange>) -> Vec<AudioDetectedRange> {
    ranges.sort_by_key(|range| range.start_sample);
    let mut merged: Vec<AudioDetectedRange> = Vec::with_capacity(ranges.len());
    for range in ranges {
        if range.end_sample <= range.start_sample {
            continue;
        }
        match merged.last_mut() {
            Some(last) if range.start_sample <= last.end_sample => {
                last.end_sample = last.end_sample.max(range.end_sample);
            }
            _ => merged.push(range),
        }
    }
    merged
}

/// BGM clip の ducking カーブ（dB の keyframe）。声の区間の手前で attack をかけて下げ、hold の後 release で戻す。
/// 短い間で音量を戻さないよう、attack + hold + release で届く区間はまとめる。完全な無減衰なら空にする。
pub fn make_ducking_keys(
    ranges: &[AudioDetectedRange],
    settings: &AudioAdjustmentSettings,
    clip_start_frame: i64,
    duration_frames: i64,
    fps: mvm_core::Rational,
) -> Vec<ClipKeyframe> {
    if duration_frames <= 0 || !fps.is_positive() {
        return Vec::new();
    }
    let merged = merge_audio_detected_ranges(ranges.to_vec());
    let (attack, hold, release) = (
        i64::from(settings.attack_ms) * 48,
        i64::from(settings.hold_ms) * 48,
        i64::from(settings.release_ms) * 48,
    );
    let mut envelopes: Vec<AudioDetectedRange> = Vec::with_capacity(merged.len());
    for range in merged {
        match envelopes.last_mut() {
            Some(last) if range.start_sample - attack <= last.end_sample + hold + release => {
                last.end_sample = last.end_sample.max(range.end_sample);
            }
            _ => envelopes.push(range),
        }
    }
    let samples_per_frame = 48000.0 * fps.den as f64 / fps.num as f64;
    // 折点は昇順で評価するため、過ぎた区間を毎回先頭から探し直さない。
    let mut envelope_index = 0;
    let mut value_at = |local: i64| -> f64 {
        let sample = (clip_start_frame + local) as f64 * samples_per_frame;
        while envelopes
            .get(envelope_index)
            .is_some_and(|range| sample >= (range.end_sample + hold + release) as f64)
        {
            envelope_index += 1;
        }
        let Some(range) = envelopes.get(envelope_index) else {
            return 0.0;
        };
        let start = range.start_sample as f64;
        if sample < (range.start_sample - attack) as f64 {
            0.0
        } else if sample < start {
            -settings.reduction_db * (sample - (range.start_sample - attack) as f64) / attack as f64
        } else if sample <= (range.end_sample + hold) as f64 {
            -settings.reduction_db
        } else if sample < (range.end_sample + hold + release) as f64 {
            -settings.reduction_db
                * (1.0 - (sample - (range.end_sample + hold) as f64) / release as f64)
        } else {
            0.0
        }
    };
    // 折点の前後を残し、frame 丸めによる線形補間のずれを防ぐ。
    let mut positions = BTreeSet::from([0, duration_frames - 1]);
    for range in &envelopes {
        for sample in [
            range.start_sample - attack,
            range.start_sample,
            range.end_sample + hold,
            range.end_sample + hold + release,
        ] {
            let local = sample as f64 / samples_per_frame - clip_start_frame as f64;
            if local < -1.0 || local > duration_frames as f64 {
                continue;
            }
            for frame in [local.floor() as i64, local.ceil() as i64] {
                positions.insert(frame.clamp(0, duration_frames - 1));
            }
        }
    }
    let keys: Vec<ClipKeyframe> = positions
        .into_iter()
        .map(|position| ClipKeyframe::new(position, value_at(position)))
        .collect();
    if keys.iter().all(|key| key.value == 0.0) {
        return Vec::new();
    }
    keys
}

/// C++ 版の `std::setprecision(17)` と同じ表記（printf の `%.17g`）。解析入力の射影は fingerprint の元なので、
/// C++ 版が記録した fingerprint と一致させる。
fn format_g17(value: f64) -> String {
    const PRECISION: i32 = 17;
    if value == 0.0 {
        return if value.is_sign_negative() { "-0" } else { "0" }.to_owned();
    }
    if !value.is_finite() {
        return if value.is_nan() {
            "nan".to_owned()
        } else if value > 0.0 {
            "inf".to_owned()
        } else {
            "-inf".to_owned()
        };
    }
    let scientific = format!("{:.*e}", (PRECISION - 1) as usize, value);
    let (mantissa, exponent) = scientific.split_once('e').unwrap_or((&scientific, "0"));
    let exponent: i32 = exponent.parse().unwrap_or(0);
    let trim = |text: &str| -> String {
        if text.contains('.') {
            text.trim_end_matches('0').trim_end_matches('.').to_owned()
        } else {
            text.to_owned()
        }
    };
    if (-4..PRECISION).contains(&exponent) {
        let decimals = (PRECISION - 1 - exponent).max(0) as usize;
        trim(&format!("{value:.decimals$}"))
    } else {
        let sign = if exponent < 0 { '-' } else { '+' };
        format!("{}e{sign}{:02}", trim(mantissa), exponent.abs())
    }
}

fn append_keys(out: &mut String, keys: &[ClipKeyframe]) {
    let _ = write!(out, "{}", keys.len());
    for key in keys {
        let _ = write!(
            out,
            " {} {} {} {} {} {} {}",
            key.frame,
            format_g17(key.value),
            key.interpolation as i32,
            format_g17(key.curve_start),
            format_g17(key.curve_end),
            format_g17(key.control1),
            format_g17(key.control2)
        );
    }
}

/// 全ての audio track を声として扱う射影。
pub fn audio_adjustment_input_projection_all(project: &Project) -> String {
    let settings = AudioAdjustmentSettings {
        voice_tracks: (0..project.audio_tracks.len() as i32).collect(),
        ..AudioAdjustmentSettings::default()
    };
    audio_adjustment_input_projection(project, &settings)
}

/// 解析・再生成の入力の射影。設定で選択した track に限り、timeline fps と音声 clip の素材対応、速度、トリム、
/// 音量、フェード、音声トランジション、track のゲインとパンだけを書く。mute・solo、自動補正の出力、字幕、
/// 映像の変形は含めない。
pub fn audio_adjustment_input_projection(
    project: &Project,
    settings: &AudioAdjustmentSettings,
) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "fps {} {}",
        project.timeline_fps.num, project.timeline_fps.den
    );
    for (index, track) in project.audio_tracks.iter().enumerate() {
        if settings.targets_track(index as i32) {
            let _ = writeln!(
                out,
                "{index} {} {}",
                format_g17(track.mixer_gain_db),
                format_g17(track.mixer_pan)
            );
        }
    }
    for clip in &project.timeline_clips {
        if clip.kind != TimelineClipKind::Audio || !settings.targets_track(clip.track.index as i32)
        {
            continue;
        }
        let _ = write!(
            out,
            "clip {} {} {} {} {} {} {} {} {} {} {} {} {} {} {} ",
            clip.id,
            i32::from(clip.enabled),
            clip.track.index,
            clip.source_fps.num,
            clip.source_fps.den,
            clip.source_frame_count,
            clip.source_in_frame,
            clip.source_out_frame,
            clip.timeline_start_frame,
            clip.speed.num,
            clip.speed.den,
            i32::from(clip.preserve_pitch),
            format_g17(clip.effects.volume_percent),
            clip.effects.fade_in_frames,
            clip.effects.fade_out_frames
        );
        match &clip.frame_hold {
            Some(hold) => {
                let _ = write!(
                    out,
                    "hold {} {} {} {} {} {}",
                    hold.source_frame,
                    hold.source_fps.num,
                    hold.source_fps.den,
                    hold.source_frame_count,
                    hold.speed.num,
                    hold.speed.den
                );
            }
            None => out.push_str("hold -"),
        }
        let path = clip.media_path.to_string_lossy().replace('\\', "/");
        let _ = writeln!(out, " path {path}");
        append_keys(&mut out, &clip.effects.volume_keys);
        out.push('\n');
    }
    for transition in &project.timeline_transitions {
        let audio_target = |id: &str| {
            project.clip(id).is_some_and(|clip| {
                clip.kind == TimelineClipKind::Audio
                    && settings.targets_track(clip.track.index as i32)
            })
        };
        if audio_target(&transition.outgoing_clip_id) && audio_target(&transition.incoming_clip_id)
        {
            let _ = writeln!(
                out,
                "xfade {} {} {} {} {}",
                transition.id,
                transition.outgoing_clip_id,
                transition.incoming_clip_id,
                transition.frames_before_cut,
                transition.frames_after_cut
            );
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clip_effects::{ClipEffects, evaluate_clip_keys, validate_effect_keys};

    fn fps60() -> mvm_core::Rational {
        mvm_core::Rational::new(60, 1)
    }

    fn range(start: i64, end: i64) -> AudioDetectedRange {
        AudioDetectedRange {
            start_sample: start,
            end_sample: end,
        }
    }

    #[test]
    fn settings_round_trip_through_the_cpp_keys() {
        let settings = AudioAdjustmentSettings {
            voice_tracks: vec![0, 2],
            bgm_track: 1,
            reduction_db: 9.5,
            ..AudioAdjustmentSettings::default()
        };
        let text = settings.to_json();
        assert!(text.contains("\"voiceTracks\":[0,2]"), "{text}");
        assert!(text.contains("\"bgmTrack\":1"), "{text}");
        assert_eq!(AudioAdjustmentSettings::from_json(&text).unwrap(), settings);
        // 欠けた key・未知の key は既定値で埋めずに失敗する。
        assert!(AudioAdjustmentSettings::from_json("{\"bgmTrack\":1}").is_err());
        let extra = text.replacen('{', "{\"other\":1,", 1);
        assert!(AudioAdjustmentSettings::from_json(&extra).is_err());
    }

    #[test]
    fn formats_like_printf_g17() {
        // 期待値は C の printf("%.17g") の出力を写したもの。
        assert_eq!(format_g17(0.0), "0");
        assert_eq!(format_g17(100.0), "100");
        assert_eq!(format_g17(-6.0), "-6");
        assert_eq!(format_g17(0.1), "0.10000000000000001");
        assert_eq!(format_g17(1.0 / 3.0), "0.33333333333333331");
        assert_eq!(format_g17(2.0 / 3.0), "0.66666666666666663");
        assert_eq!(format_g17(1e20), "1e+20");
        assert_eq!(format_g17(0.0001), "0.0001");
        assert_eq!(format_g17(0.00001), "1.0000000000000001e-05");
        assert_eq!(format_g17(112.20184543019634), "112.20184543019634");
    }

    // 1 時間に分散した 1200 発話の全折点を生成し、下降・保持・復帰が最後の発話まで正しい。
    #[test]
    fn long_ducking_curve() {
        let settings = AudioAdjustmentSettings::default();
        let ranges: Vec<_> = (0..1200)
            .map(|interval| range(interval * 144_000 + 48_000, interval * 144_000 + 96_000))
            .collect();
        let keys = make_ducking_keys(&ranges, &settings, 0, 216_000, fps60());
        assert_eq!(keys.len(), 4802);
        for interval in 0..1200 {
            assert_eq!(evaluate_clip_keys(&keys, 0.0, interval * 180 + 51), 0.0);
            assert_eq!(evaluate_clip_keys(&keys, 0.0, interval * 180 + 60), -12.0);
            assert_eq!(evaluate_clip_keys(&keys, 0.0, interval * 180 + 138), -12.0);
            assert_eq!(evaluate_clip_keys(&keys, 0.0, interval * 180 + 174), 0.0);
        }
    }

    #[test]
    fn validates_settings() {
        let settings = AudioAdjustmentSettings {
            voice_tracks: vec![0],
            bgm_track: 1,
            ..AudioAdjustmentSettings::default()
        };
        settings.validate(2).unwrap();
        let invalid: [&dyn Fn(&mut AudioAdjustmentSettings); 6] = [
            &|s| s.bgm_track = 0,
            &|s| s.voice_tracks = vec![0, 0],
            &|s| s.threshold_db = f64::NAN,
            &|s| s.attack_ms = 0,
            &|s| s.release_ms = -1,
            &|s| {
                s.normalize = false;
                s.duck = false;
            },
        ];
        for change in invalid {
            let mut bad = settings.clone();
            change(&mut bad);
            assert!(bad.validate(2).is_err());
        }
    }

    // 独立した時刻と減衰量で attack・hold・release を比較する。
    #[test]
    fn ducking_shape() {
        let settings = AudioAdjustmentSettings {
            voice_tracks: vec![0],
            bgm_track: 1,
            ..AudioAdjustmentSettings::default()
        };
        let keys = make_ducking_keys(&[range(48_000, 96_000)], &settings, 0, 240, fps60());
        for (frame, expected) in [
            (51, 0.0),
            (60, -12.0),
            (138, -12.0),
            (156, -6.0),
            (174, 0.0),
        ] {
            assert!(
                (evaluate_clip_keys(&keys, 0.0, frame) - expected).abs() < 1e-8,
                "{frame}"
            );
        }
        let merged = make_ducking_keys(
            &[range(48_000, 96_000), range(100_800, 144_000)],
            &settings,
            0,
            300,
            fps60(),
        );
        assert_eq!(
            evaluate_clip_keys(&merged, 0.0, 124),
            -12.0,
            "短い間で音量を戻さず二重減衰しない"
        );
        assert!(
            make_ducking_keys(&[], &settings, 0, 240, fps60()).is_empty(),
            "検出 0 件は無減衰"
        );
        let cropped = make_ducking_keys(&[range(48_000, 96_000)], &settings, 90, 30, fps60());
        assert_eq!(cropped.first().unwrap().value, -12.0, "減衰中に始まる BGM");
        assert_eq!(cropped.last().unwrap().frame, 29, "clip 境界");

        let mut effects = ClipEffects {
            normalization_gain_db: 6.0,
            ducking_keys: keys.clone(),
            ..ClipEffects::default()
        };
        effects.validate(240).unwrap();
        validate_effect_keys(&effects, 240, true).unwrap();
        effects.ducking_keys[0].value = 1.0;
        assert!(
            validate_effect_keys(&effects, 240, true).is_err(),
            "増幅する ducking を拒否"
        );
        effects.ducking_keys = keys;
        assert!(
            validate_effect_keys(&effects, 240, false).is_err(),
            "映像の ducking を拒否"
        );
        effects.normalization_gain_db = f64::INFINITY;
        assert!(effects.validate(240).is_err(), "非有限の正規化を拒否");
    }
}
