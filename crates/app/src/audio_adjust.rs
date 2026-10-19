//! 自動音量調整（ラウドネス正規化と BGM の ducking）。解析は worker、適用は 1 回の undo。
//!
//! 解析は開始時の Project の snapshot で行う。結果を確かめている間に Project を編集したら、
//! 適用を拒否して再解析を頼む（古い解析で今の Project を上書きしない）。

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};

use mvm_audio::{LoudnessMeasurement, LoudnessMeter};
use mvm_platform::{FileProbe, probe_file};
use mvm_preview::{AudioMixer, AudioSpan, audio_spans, output_timebase};
use mvm_project::audio_adjustment::{
    AudioAdjustmentSettings, AudioDetectedRange, make_ducking_keys, merge_audio_detected_ranges,
};
use mvm_project::timeline::{timeline_clip_duration, validate_timeline};
use mvm_project::{Project, TimelineClipKind};

use crate::background::{BackgroundJob, JobControl};
use crate::error::AppError;
use crate::session::Session;

/// mix から読み出す 1 回の量（200 ms）。
const BLOCK_SAMPLES: usize = 9600;
/// 声の検出の窓（20 ms）。
const WINDOW_SAMPLES: usize = 960;
/// 同時に解析する clip の数の上限。
const MAXIMUM_WORKERS: usize = 4;

#[derive(Clone, Debug, PartialEq)]
pub struct AudioAdjustmentClipReport {
    pub clip_id: String,
    pub name: String,
    pub voice: bool,
    pub measurement: LoudnessMeasurement,
    pub correction_db: f64,
    /// true peak -1 dBTP を超えないよう、目標より小さく補正した。
    pub peak_limited: bool,
    /// 補正量の範囲（-96..=60 dB）で丸めた。
    pub gain_limited: bool,
}

#[derive(Clone, Debug)]
pub struct AudioAdjustmentOutcome {
    candidate: Project,
    pub settings: AudioAdjustmentSettings,
    pub clips: Vec<AudioAdjustmentClipReport>,
    pub ranges: Vec<AudioDetectedRange>,
    base_revision: u64,
}

impl Session {
    /// dialog の初期値。最後に適用した設定が読めればそれ、無ければ A1 を声・最後の track を BGM にする。
    pub fn audio_adjustment_defaults(&self) -> AudioAdjustmentSettings {
        let tracks = self.project.audio_tracks.len() as i32;
        if let Ok(settings) =
            AudioAdjustmentSettings::from_json(&self.project.last_audio_adjustment_settings)
            && settings.validate(tracks).is_ok()
        {
            return settings;
        }
        if tracks >= 2 {
            AudioAdjustmentSettings {
                voice_tracks: vec![0],
                bgm_track: tracks - 1,
                ..AudioAdjustmentSettings::default()
            }
        } else {
            AudioAdjustmentSettings {
                voice_tracks: Vec::new(),
                bgm_track: 0,
                duck: false,
                ..AudioAdjustmentSettings::default()
            }
        }
    }

    pub fn start_audio_adjustment(
        &mut self,
        settings: AudioAdjustmentSettings,
    ) -> Result<(), AppError> {
        if self.background.audio_adjust.is_some() {
            return Err(AppError::new("音声を解析中です"));
        }
        settings
            .validate(self.project.audio_tracks.len() as i32)
            .map_err(|error| AppError::new(error.to_string()))?;
        if self.is_playing() {
            self.pause_playback();
        }
        self.background.audio_adjust_result = None;
        let project = self.project.clone();
        let revision = self.edit_revision;
        let job = BackgroundJob::start(
            "mvm-audio-adjust",
            self.preview_wake.clone(),
            move |control| analyze_audio_adjustment(project, settings, revision, control),
        )?;
        self.background.audio_adjust = Some(job);
        Ok(())
    }

    pub fn audio_adjustment_progress(&self) -> Option<f32> {
        self.background
            .audio_adjust
            .as_ref()
            .map(BackgroundJob::progress)
    }

    pub fn cancel_audio_adjustment(&mut self) {
        if let Some(job) = &self.background.audio_adjust {
            job.cancel();
        }
    }

    /// 解析が終わっていれば結果を受け取る。成功なら [`Self::audio_adjustment_result`] で読める。
    pub fn poll_audio_adjustment(&mut self) -> Option<Result<(), AppError>> {
        let finished = self.background.audio_adjust.as_mut()?.take_finished()?;
        self.background.audio_adjust = None;
        Some(match finished.and_then(|result| result) {
            Ok(outcome) => {
                self.background.audio_adjust_result = Some(outcome);
                Ok(())
            }
            Err(error) => Err(error),
        })
    }

    pub fn audio_adjustment_result(&self) -> Option<&AudioAdjustmentOutcome> {
        self.background.audio_adjust_result.as_ref()
    }

    pub fn discard_audio_adjustment(&mut self) {
        self.background.audio_adjust_result = None;
    }

    /// 解析の結果を 1 回の undo で適用する。解析の後に Project を変えていたら拒否する。
    /// 結果は適用に成功したときだけ消費する。拒否・失敗では残し（UI が結果を見せ続けられる）、古い結果は
    /// 何度でも拒否する。
    pub fn apply_audio_adjustment(&mut self) -> Result<(), AppError> {
        let Some(outcome) = self.background.audio_adjust_result.as_ref() else {
            return Err(AppError::new("適用する解析の結果がありません"));
        };
        if outcome.base_revision != self.edit_revision {
            return Err(AppError::new(
                "解析の後に Project が変わりました。もう一度解析してください",
            ));
        }
        let candidate = outcome.candidate.clone();
        self.transact(move |project| {
            *project = candidate;
            Ok(None)
        })?;
        self.background.audio_adjust_result = None;
        Ok(())
    }
}

/// 対象 track の各 clip を測り、正規化の補正と ducking の key を入れた Project の候補を作る。
pub(crate) fn analyze_audio_adjustment(
    project: Project,
    settings: AudioAdjustmentSettings,
    base_revision: u64,
    control: &JobControl,
) -> Result<AudioAdjustmentOutcome, AppError> {
    settings
        .validate(project.audio_tracks.len() as i32)
        .map_err(|error| AppError::new(error.to_string()))?;
    let cancelled = || AppError::new("音声の解析を取り消しました");
    let mut candidate = project.clone();
    // 解析の入力は明示した track。mute・solo と前回の自動調整を測定から外す。
    let mut source = project;
    for track in &mut source.audio_tracks {
        track.muted = false;
        track.solo = false;
    }
    for clip in &mut source.timeline_clips {
        clip.effects.normalization_gain_db = 0.0;
        clip.effects.ducking_db = 0.0;
        clip.effects.ducking_keys.clear();
    }
    let spans = audio_spans(&source).map_err(|error| AppError::new(error.to_string()))?;
    let mut groups: Vec<(String, Vec<AudioSpan>)> = Vec::new();
    for span in spans {
        if !settings.targets_track(span.segment.clip.track.index as i32) {
            continue;
        }
        let id = span.segment.original.id.clone();
        match groups.iter_mut().find(|(group, _)| *group == id) {
            Some((_, group)) => group.push(span),
            None => groups.push((id, vec![span])),
        }
    }
    let is_bgm = |spans: &[AudioSpan]| {
        spans
            .first()
            .is_some_and(|span| span.segment.clip.track.index as i32 == settings.bgm_track)
    };
    if !groups.iter().any(|(_, spans)| is_bgm(spans)) {
        return Err(AppError::new(
            "BGM トラックに解析できる音声クリップがありません",
        ));
    }
    if settings.duck && groups.iter().all(|(_, spans)| is_bgm(spans)) {
        return Err(AppError::new(
            "声トラックに解析できる音声クリップがありません",
        ));
    }
    let paths = unique_paths(&groups);
    let files_at_start: Vec<FileProbe> = paths.iter().map(|path| probe_file(path)).collect();

    let timebase = output_timebase(&source).map_err(|error| AppError::new(error.to_string()))?;
    let total: i64 = groups
        .iter()
        .flat_map(|(_, spans)| spans)
        .map(|span| span.timeline_end_sample - span.timeline_start_sample)
        .sum();
    let done = AtomicI64::new(0);
    let next = AtomicUsize::new(0);
    let results: Vec<Mutex<Option<Result<Measured, AppError>>>> =
        groups.iter().map(|_| Mutex::new(None)).collect();
    let fps = source.timeline_fps;
    let workers = groups.len().clamp(1, MAXIMUM_WORKERS);
    std::thread::scope(|scope| {
        for _ in 0..workers {
            scope.spawn(|| {
                loop {
                    let index = next.fetch_add(1, Ordering::Relaxed);
                    let Some((_, spans)) = groups.get(index) else {
                        break;
                    };
                    let measured = measure_clip(spans, timebase, fps, control, &|count| {
                        let sum = done.fetch_add(count, Ordering::Relaxed) + count;
                        if total > 0 {
                            control.set_progress(sum as f64 / total as f64);
                        }
                    });
                    let failed = measured.is_err();
                    if let Some(slot) = results.get(index) {
                        *slot.lock().unwrap_or_else(|poison| poison.into_inner()) = Some(measured);
                    }
                    if failed {
                        control.cancel();
                        break;
                    }
                }
            });
        }
    });
    let mut measured_clips = Vec::with_capacity(groups.len());
    for slot in results {
        match slot
            .into_inner()
            .unwrap_or_else(|poison| poison.into_inner())
        {
            Some(Ok(measured)) => measured_clips.push(measured),
            Some(Err(error)) => return Err(error),
            None => return Err(cancelled()),
        }
    }
    if control.is_cancelled() {
        return Err(cancelled());
    }

    let mut reports = Vec::with_capacity(groups.len());
    let mut ranges = Vec::new();
    for ((clip_id, spans), measured) in groups.iter().zip(measured_clips) {
        let voice = !is_bgm(spans);
        let Some(target) = candidate
            .timeline_clips
            .iter_mut()
            .find(|clip| clip.id == *clip_id)
        else {
            return Err(AppError::new("解析した clip が Project にありません"));
        };
        let mut report = AudioAdjustmentClipReport {
            clip_id: clip_id.clone(),
            name: target.name.clone(),
            voice,
            measurement: measured.measurement,
            correction_db: target.effects.normalization_gain_db,
            peak_limited: false,
            gain_limited: false,
        };
        if settings.normalize && measured.measurement.measurable {
            let wanted = if voice {
                settings.voice_lufs
            } else {
                settings.bgm_lufs
            } - measured.measurement.integrated_lufs;
            let ceiling = -1.0 - measured.measurement.true_peak_db;
            report.peak_limited = wanted > ceiling;
            let correction = wanted.min(ceiling);
            report.correction_db = correction.clamp(-96.0, 60.0);
            report.gain_limited = correction != report.correction_db;
            target.effects.normalization_gain_db = report.correction_db;
        }
        if settings.duck && voice {
            let normalized = 10f64.powf(target.effects.normalization_gain_db / 20.0);
            let threshold = 10f64.powf(settings.threshold_db / 20.0);
            ranges.extend(
                measured
                    .windows
                    .iter()
                    .filter(|window| window.rms * normalized >= threshold)
                    .map(|window| AudioDetectedRange {
                        start_sample: window.start,
                        end_sample: window.end,
                    }),
            );
        }
        reports.push(report);
    }
    let ranges = merge_audio_detected_ranges(ranges);
    if settings.duck {
        let fps = candidate.timeline_fps;
        let durations: HashMap<String, i64> = candidate
            .timeline_clips
            .iter()
            .filter(|clip| {
                clip.enabled
                    && clip.kind == TimelineClipKind::Audio
                    && clip.track.index as i32 == settings.bgm_track
            })
            .map(|clip| {
                timeline_clip_duration(&candidate, clip).map(|duration| (clip.id.clone(), duration))
            })
            .collect::<Result<_, _>>()
            .map_err(|error| AppError::new(error.to_string()))?;
        for clip in &mut candidate.timeline_clips {
            if let Some(duration) = durations.get(&clip.id) {
                clip.effects.ducking_db = 0.0;
                clip.effects.ducking_keys = make_ducking_keys(
                    &ranges,
                    &settings,
                    clip.timeline_start_frame,
                    *duration,
                    fps,
                );
            }
        }
    }
    candidate.last_audio_adjustment_settings = settings.to_json();
    validate_timeline(&candidate).map_err(|error| AppError::new(error.to_string()))?;
    let files_at_end: Vec<FileProbe> = paths.iter().map(|path| probe_file(path)).collect();
    if files_at_start != files_at_end {
        return Err(AppError::new(
            "解析中に素材の内容が変わりました。もう一度解析してください",
        ));
    }
    control.set_progress(1.0);
    Ok(AudioAdjustmentOutcome {
        candidate,
        settings,
        clips: reports,
        ranges,
        base_revision,
    })
}

struct Window {
    start: i64,
    end: i64,
    rms: f64,
}

struct Measured {
    measurement: LoudnessMeasurement,
    windows: Vec<Window>,
}

fn unique_paths(groups: &[(String, Vec<AudioSpan>)]) -> Vec<PathBuf> {
    let mut paths: Vec<PathBuf> = groups
        .iter()
        .flat_map(|(_, spans)| spans.iter().map(|span| span.path.clone()))
        .collect();
    paths.sort();
    paths.dedup();
    paths
}

/// 1 clip の音（音量・フェード・track の gain と定位を掛けた後）を測る。
fn measure_clip(
    spans: &[AudioSpan],
    timebase: mvm_core::output_timebase::OutputTimebase,
    fps: mvm_core::Rational,
    control: &JobControl,
    advance: &dyn Fn(i64),
) -> Result<Measured, AppError> {
    let start = spans
        .iter()
        .map(|span| span.timeline_start_sample)
        .min()
        .unwrap_or(0);
    let end = spans
        .iter()
        .map(|span| span.timeline_end_sample)
        .max()
        .unwrap_or(0);
    let mut mixer = AudioMixer::open(spans.to_vec(), timebase, fps)
        .map_err(|error| AppError::new(error.to_string()))?;
    let mut meter = LoudnessMeter::new();
    let mut windows = Vec::new();
    let mut buffer = vec![0f32; BLOCK_SAMPLES * 2];
    let mut at = start;
    while at < end {
        if control.is_cancelled() {
            return Err(AppError::new("音声の解析を取り消しました"));
        }
        let count = usize::try_from((end - at).min(BLOCK_SAMPLES as i64))
            .map_err(|_| AppError::new("解析する区間が不正です"))?;
        let block = &mut buffer[..count * 2];
        mixer
            .mix(at, block)
            .map_err(|error| AppError::new(error.to_string()))?;
        if !meter.push(block) {
            return Err(AppError::new("解析する音声の形が不正です"));
        }
        for (index, window) in block.chunks(WINDOW_SAMPLES * 2).enumerate() {
            let frames = window.len() / 2;
            if frames == 0 {
                continue;
            }
            let (left, right) =
                window
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .fold((0.0, 0.0), |(l, r), pair| {
                        (
                            l + f64::from(pair[0]) * f64::from(pair[0]),
                            r + f64::from(pair[1]) * f64::from(pair[1]),
                        )
                    });
            let first = at + (index * WINDOW_SAMPLES) as i64;
            windows.push(Window {
                start: first,
                end: first + frames as i64,
                rms: (f64::max(left, right) / frames as f64).sqrt(),
            });
        }
        at += count as i64;
        advance(count as i64);
    }
    Ok(Measured {
        measurement: meter.finish(),
        windows,
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use std::path::Path;

    use mvm_core::Rational;
    use mvm_project::{TimelineClip, Track, TrackKind, TrackRef};

    use super::*;

    /// `seconds` 秒の 1 kHz（振幅 `amplitude`）。`silent` の秒区間は無音にする。
    fn tone(path: &Path, amplitude: f64, seconds: usize, silent: Option<(usize, usize)>) {
        let samples: Vec<f32> = (0..48_000 * seconds)
            .flat_map(|index| {
                let second = index / 48_000;
                let quiet = silent.is_some_and(|(from, to)| (from..to).contains(&second));
                let value = if quiet {
                    0.0
                } else {
                    (amplitude * (index as f64 * 1000.0 * std::f64::consts::TAU / 48_000.0).sin())
                        as f32
                };
                [value, value]
            })
            .collect();
        mvm_media::write_pcm16_wav(path, 48_000, 2, &samples).unwrap();
    }

    fn clip(id: &str, path: &Path, track: usize, seconds: i64) -> TimelineClip {
        TimelineClip {
            kind: TimelineClipKind::Audio,
            media_path: path.to_path_buf(),
            name: id.to_owned(),
            id: id.to_owned(),
            media_item_id: format!("item-{id}"),
            source_fps: Rational::new(60, 1),
            source_frame_count: seconds * 60,
            source_in_frame: 0,
            source_out_frame: seconds * 60,
            track: TrackRef::audio(track),
            ..TimelineClip::default()
        }
    }

    fn fixture(directory: &Path) -> Project {
        let voice = directory.join("voice.wav");
        let bgm = directory.join("bgm.wav");
        // 声は 2〜4 秒だけ無音。BGM は -20 dBFS の連続音。
        tone(&voice, 10f64.powf(-12.0 / 20.0), 6, Some((2, 4)));
        tone(&bgm, 10f64.powf(-20.0 / 20.0), 6, None);
        let mut project = Project::default();
        project
            .audio_tracks
            .push(Track::named(TrackKind::Audio.default_track_name(1)));
        project.timeline_clips = vec![clip("voice", &voice, 0, 6), clip("bgm", &bgm, 1, 6)];
        project
    }

    fn settings() -> AudioAdjustmentSettings {
        AudioAdjustmentSettings {
            voice_tracks: vec![0],
            bgm_track: 1,
            ..AudioAdjustmentSettings::default()
        }
    }

    #[test]
    fn normalizes_each_clip_and_ducks_the_bgm_only_under_the_voice() {
        let directory = std::env::temp_dir().join(format!("mvm-adjust-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let project = fixture(&directory);
        let outcome =
            analyze_audio_adjustment(project, settings(), 1, &JobControl::default()).unwrap();

        // 1 kHz の stereo 正弦波は振幅の dBFS が LUFS。BGM は -20 → -24 で -4 dB。
        // 声は無音の境に掛かる block が相対 gate を通るので -12 より少し低く出る（規格どおり）。
        let voice = outcome
            .clips
            .iter()
            .find(|clip| clip.clip_id == "voice")
            .unwrap();
        let bgm = outcome
            .clips
            .iter()
            .find(|clip| clip.clip_id == "bgm")
            .unwrap();
        let voice_lufs = voice.measurement.integrated_lufs;
        assert!((-12.6..=-12.0).contains(&voice_lufs), "{voice:?}");
        assert!(
            (voice.correction_db - (-16.0 - voice_lufs)).abs() < 1e-9,
            "{voice:?}"
        );
        assert!(
            (bgm.measurement.integrated_lufs + 20.0).abs() < 0.1,
            "{bgm:?}"
        );
        assert!((bgm.correction_db + 4.0).abs() < 0.1, "{bgm:?}");
        assert!(!voice.peak_limited && !bgm.peak_limited);

        // 声のある 0〜2 秒・4〜6 秒だけが検出され、間の無音は外れる。
        assert_eq!(outcome.ranges.len(), 2, "{:?}", outcome.ranges);
        assert_eq!(outcome.ranges[0].start_sample, 0);
        assert_eq!(outcome.ranges[0].end_sample, 96_000);
        assert_eq!(outcome.ranges[1].start_sample, 192_000);

        let candidate = &outcome.candidate;
        let bgm_clip = candidate.clip("bgm").unwrap();
        let voice_clip = candidate.clip("voice").unwrap();
        assert!(
            !bgm_clip.effects.ducking_keys.is_empty(),
            "BGM に ducking が無い"
        );
        assert!(
            voice_clip.effects.ducking_keys.is_empty(),
            "声に ducking が付いた"
        );
        // 3 秒目（声の無い区間の中央、frame 180）では戻り、1 秒目（frame 60）では下がっている。
        let at = |frame| {
            mvm_project::clip_effects::evaluate_clip_keys(
                &bgm_clip.effects.ducking_keys,
                0.0,
                frame,
            )
        };
        assert!((at(60) + 12.0).abs() < 1e-9, "{}", at(60));
        assert!(at(180).abs() < 1e-9, "{}", at(180));
        assert_eq!(
            AudioAdjustmentSettings::from_json(&candidate.last_audio_adjustment_settings).unwrap(),
            settings()
        );
        std::fs::remove_dir_all(&directory).unwrap();
    }

    #[test]
    fn a_cancelled_analysis_changes_nothing() {
        let directory = std::env::temp_dir().join(format!("mvm-adjust-c-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let project = fixture(&directory);
        let control = JobControl::default();
        control.cancel();
        let error = analyze_audio_adjustment(project, settings(), 1, &control).unwrap_err();
        assert!(error.message().contains("取り消し"), "{error}");
        std::fs::remove_dir_all(&directory).unwrap();
    }

    #[test]
    fn a_bgm_track_without_clips_is_rejected() {
        let directory = std::env::temp_dir().join(format!("mvm-adjust-e-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let mut project = fixture(&directory);
        project.timeline_clips.retain(|clip| clip.id != "bgm");
        let error =
            analyze_audio_adjustment(project, settings(), 1, &JobControl::default()).unwrap_err();
        assert!(error.message().contains("BGM"), "{error}");
        std::fs::remove_dir_all(&directory).unwrap();
    }

    #[test]
    fn applying_after_an_edit_is_refused() {
        let directory = std::env::temp_dir().join(format!("mvm-adjust-a-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let mut session = Session::from_project(fixture(&directory));
        let outcome = analyze_audio_adjustment(
            session.project.clone(),
            settings(),
            session.edit_revision,
            &JobControl::default(),
        )
        .unwrap();
        session.background.audio_adjust_result = Some(outcome.clone());
        session.add_marker().unwrap();
        assert!(
            session.apply_audio_adjustment().is_err(),
            "編集後の適用を通しました"
        );
        // 拒否しても結果は残り（UI が見せ続けられる）、何度でも拒否する。
        assert!(session.audio_adjustment_result().is_some());
        assert!(session.apply_audio_adjustment().is_err());

        session.background.audio_adjust_result = Some(AudioAdjustmentOutcome {
            base_revision: session.edit_revision,
            ..outcome
        });
        session.apply_audio_adjustment().unwrap();
        assert!(
            session.audio_adjustment_result().is_none(),
            "適用した結果が残りました"
        );
        assert!(
            session
                .project
                .clip("bgm")
                .unwrap()
                .effects
                .normalization_gain_db
                < -3.0
        );
        session.undo().unwrap();
        assert_eq!(
            session
                .project
                .clip("bgm")
                .unwrap()
                .effects
                .normalization_gain_db,
            0.0,
            "1 回の undo で戻りません"
        );
        std::fs::remove_dir_all(&directory).unwrap();
    }
}
