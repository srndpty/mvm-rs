//! timeline sample の区間を、開いた PCM から足し合わせる。

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use mvm_audio::WasapiSink;
use mvm_core::Rational;
use mvm_core::output_timebase::OutputTimebase;
use mvm_media::{PcmStream, TimeStretcher};
use mvm_project::audio_mix_gains;
use mvm_project::render::render_segment_gain;

use crate::audio_plan::{AudioSpan, source_sample};
use crate::error::PreviewError;

/// time stretch へ一度に入れる素材の sample 数（100 ms）。
const STRETCH_CHUNK: usize = 4800;

/// 再生中の音量の表示。mix が書き、UI が読んで空にする。値は直前の読み出しから後の絶対値の最大。
#[derive(Debug, Default)]
pub struct AudioMeters {
    peaks: Mutex<MeterPeaks>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct MeterPeaks {
    /// audio track の index ごと。
    pub tracks: Vec<f32>,
    pub master: f32,
}

impl AudioMeters {
    fn record(&self, tracks: &[f32], master: f32) {
        let mut peaks = self
            .peaks
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        if peaks.tracks.len() < tracks.len() {
            peaks.tracks.resize(tracks.len(), 0.0);
        }
        for (slot, value) in peaks.tracks.iter_mut().zip(tracks) {
            *slot = slot.max(*value);
        }
        peaks.master = peaks.master.max(master);
    }

    /// 直前の呼び出しから後の最大を返し、0 へ戻す。
    pub fn take(&self) -> MeterPeaks {
        std::mem::take(
            &mut *self
                .peaks
                .lock()
                .unwrap_or_else(|poison| poison.into_inner()),
        )
    }
}

/// 再生の出口の音量（倍率）。書き出しには掛けない。
#[derive(Debug)]
pub struct MonitorVolume(AtomicU32);

impl Default for MonitorVolume {
    fn default() -> Self {
        Self(AtomicU32::new(1.0f32.to_bits()))
    }
}

impl MonitorVolume {
    /// 0 以上の有限値だけを受け付ける。
    pub fn set(&self, gain: f32) -> Result<(), PreviewError> {
        if !gain.is_finite() || gain < 0.0 {
            return Err(PreviewError::new("モニター音量が不正です"));
        }
        self.0.store(gain.to_bits(), Ordering::Relaxed);
        Ok(())
    }

    pub fn get(&self) -> f32 {
        f32::from_bits(self.0.load(Ordering::Relaxed))
    }
}

pub fn start_audio(
    spans: Vec<AudioSpan>,
    start_sample: i64,
    timebase: OutputTimebase,
    fps: Rational,
    meters: Arc<AudioMeters>,
    monitor: Arc<MonitorVolume>,
) -> Result<WasapiSink, PreviewError> {
    WasapiSink::spawn(move |host| {
        let mut mixer = match AudioMixer::open(spans, timebase, fps) {
            Ok(mixer) => mixer.with_meters(meters),
            Err(error) => {
                host.report(error.to_string());
                return;
            }
        };
        let result = host.run(start_sample, |start, _count, samples| {
            mixer
                .mix(start, samples)
                .map_err(|error| error.to_string())?;
            let gain = monitor.get();
            if gain != 1.0 {
                for sample in samples.iter_mut() {
                    *sample *= gain;
                }
            }
            mixer.record_master(samples);
            Ok(())
        });
        if let Err(error) = result {
            host.report(error.to_string());
        }
    })
    .map_err(|error| PreviewError::new(error.to_string()))
}

/// ピッチ保持の span の続き。続けて要求された区間なら、同じ stretcher から出し続ける。
struct Stretch {
    next_timeline: i64,
    source_cursor: i64,
    stretcher: TimeStretcher,
    pending: Vec<f32>,
    finished: bool,
}

/// 音声区間を、素材ごとに 1 つ開いた PCM から足し合わせる。再生と書き出しが同じ mix を使う。
pub struct AudioMixer {
    timebase: OutputTimebase,
    fps: Rational,
    spans: Vec<AudioSpan>,
    streams: HashMap<PathBuf, PcmStream>,
    stretches: Vec<Option<Stretch>>,
    meters: Option<Arc<AudioMeters>>,
    track_peaks: Vec<f32>,
}

impl AudioMixer {
    pub fn open(
        spans: Vec<AudioSpan>,
        timebase: OutputTimebase,
        fps: Rational,
    ) -> Result<Self, PreviewError> {
        let mut streams = HashMap::new();
        for span in &spans {
            if streams.contains_key(&span.path) {
                continue;
            }
            let stream = PcmStream::open(&span.path)
                .map_err(|error| PreviewError::new(format!("{}: {error}", span.path.display())))?;
            streams.insert(span.path.clone(), stream);
        }
        let stretches = spans.iter().map(|_| None).collect();
        Ok(Self {
            timebase,
            fps,
            spans,
            streams,
            stretches,
            meters: None,
            track_peaks: Vec::new(),
        })
    }

    fn with_meters(mut self, meters: Arc<AudioMeters>) -> Self {
        self.meters = Some(meters);
        self
    }

    /// timeline の `start` sample から、左右を交互に並べた `samples` を埋める。区間の外は無音。
    pub fn mix(&mut self, start: i64, samples: &mut [f32]) -> Result<(), PreviewError> {
        if !samples.len().is_multiple_of(2) {
            return Err(PreviewError::new("音声 buffer の長さが不正です"));
        }
        samples.fill(0.0);
        self.track_peaks.iter_mut().for_each(|peak| *peak = 0.0);
        let count = i64::try_from(samples.len() / 2)
            .map_err(|_| PreviewError::new("音声 buffer が長すぎます"))?;
        let Some(end) = start.checked_add(count) else {
            return Err(PreviewError::new("sample 位置が overflow しました"));
        };
        for index in 0..self.spans.len() {
            let span = &self.spans[index];
            let from = start.max(span.timeline_start_sample);
            let to = end.min(span.timeline_end_sample);
            if from >= to {
                continue;
            }
            let contribution = if span.segment.original.preserve_pitch {
                self.stretched(index, from, to)?
            } else {
                self.resampled(index, from, to)?
            };
            let span = &self.spans[index];
            let peak = add_with_gains(
                self.timebase,
                self.fps,
                span,
                from,
                start,
                &contribution,
                samples,
            )?;
            let track = span.segment.clip.track.index;
            if self.track_peaks.len() <= track {
                self.track_peaks.resize(track + 1, 0.0);
            }
            self.track_peaks[track] = self.track_peaks[track].max(peak);
        }
        Ok(())
    }

    fn record_master(&self, samples: &[f32]) {
        if let Some(meters) = &self.meters {
            let master = samples
                .iter()
                .fold(0.0f32, |peak, sample| peak.max(sample.abs()));
            meters.record(&self.track_peaks, master);
        }
    }

    /// 速度どおりに素材の sample を拾う（音程は速度とともに変わる）。
    fn resampled(&mut self, index: usize, from: i64, to: i64) -> Result<Vec<f32>, PreviewError> {
        let span = &self.spans[index];
        let (Some(src_from), Some(src_last)) =
            (source_sample(span, from)?, source_sample(span, to - 1)?)
        else {
            return Ok(Vec::new());
        };
        let src_begin = src_from.min(src_last);
        let src_end = src_from.max(src_last).saturating_add(1);
        let src_len = usize::try_from(src_end.saturating_sub(src_begin))
            .map_err(|_| PreviewError::new("素材 sample が多すぎます"))?;
        let mut left = vec![0f32; src_len];
        let mut right = vec![0f32; src_len];
        let stream = self
            .streams
            .get_mut(&span.path)
            .ok_or_else(|| PreviewError::new("音声の decoder がありません"))?;
        stream
            .copy_from(src_begin, &mut left, &mut right)
            .map_err(|error| PreviewError::new(error.to_string()))?;
        let length = usize::try_from(to - from)
            .map_err(|_| PreviewError::new("音声 buffer の位置が不正です"))?;
        let mut out = vec![0f32; length * 2];
        for (offset, timeline) in (from..to).enumerate() {
            let Some(src) = source_sample(span, timeline)? else {
                continue;
            };
            let at = usize::try_from(src.saturating_sub(src_begin)).unwrap_or(usize::MAX);
            if let (Some(left), Some(right)) = (left.get(at), right.get(at)) {
                out[offset * 2] = *left;
                out[offset * 2 + 1] = *right;
            }
        }
        Ok(out)
    }

    /// 音程を保って速度を変える。前の区間の続きでなければ、`from` の素材位置から入れ直す。
    fn stretched(&mut self, index: usize, from: i64, to: i64) -> Result<Vec<f32>, PreviewError> {
        let span = &self.spans[index];
        let length = usize::try_from(to - from)
            .map_err(|_| PreviewError::new("音声 buffer の位置が不正です"))?;
        let continues = self.stretches[index]
            .as_ref()
            .is_some_and(|state| state.next_timeline == from);
        if !continues {
            let Some(cursor) = source_sample(span, from)? else {
                return Ok(Vec::new());
            };
            if span.speed_den <= 0 || span.speed_num <= 0 {
                return Err(PreviewError::new("clip の速度が不正です"));
            }
            let tempo = span.speed_num as f64 / span.speed_den as f64;
            self.stretches[index] = Some(Stretch {
                next_timeline: from,
                source_cursor: cursor,
                stretcher: TimeStretcher::new(tempo)
                    .map_err(|error| PreviewError::new(error.to_string()))?,
                pending: Vec::new(),
                finished: false,
            });
        }
        let stream = self
            .streams
            .get_mut(&span.path)
            .ok_or_else(|| PreviewError::new("音声の decoder がありません"))?;
        let state = self.stretches[index]
            .as_mut()
            .ok_or_else(|| PreviewError::new("time stretch の状態がありません"))?;
        // 素材の終端より先は無音を入れる。出力が要る長さに届かない stretcher は壊れているので、
        // 入れた量に上限を設けて止める（無限に待たない）。
        let tempo = span.speed_num as f64 / span.speed_den as f64;
        let budget = (length as f64 * tempo) as usize + 48_000 * 4;
        let mut fed = 0usize;
        let mut left = vec![0f32; STRETCH_CHUNK];
        let mut right = vec![0f32; STRETCH_CHUNK];
        let mut interleaved = vec![0f32; STRETCH_CHUNK * 2];
        while state.pending.len() < length * 2 && !state.finished {
            if fed > budget {
                return Err(PreviewError::new("ピッチ保持の音声が出力されません"));
            }
            stream
                .copy_from(state.source_cursor, &mut left, &mut right)
                .map_err(|error| PreviewError::new(error.to_string()))?;
            for (at, (l, r)) in left.iter().zip(&right).enumerate() {
                interleaved[at * 2] = *l;
                interleaved[at * 2 + 1] = *r;
            }
            state
                .stretcher
                .push(&interleaved)
                .map_err(|error| PreviewError::new(error.to_string()))?;
            state.finished = state
                .stretcher
                .pull(&mut state.pending)
                .map_err(|error| PreviewError::new(error.to_string()))?;
            state.source_cursor = state
                .source_cursor
                .checked_add(STRETCH_CHUNK as i64)
                .ok_or_else(|| PreviewError::new("素材 sample が overflow しました"))?;
            fed += STRETCH_CHUNK;
        }
        let take = (length * 2).min(state.pending.len());
        let mut out: Vec<f32> = state.pending.drain(..take).collect();
        out.resize(length * 2, 0.0);
        state.next_timeline = to;
        Ok(out)
    }
}

/// `contribution`（timeline の `from` からの stereo）へ frame ごとの音量と定位を掛けて足す。足した値の絶対値の最大を返す。
fn add_with_gains(
    timebase: OutputTimebase,
    fps: Rational,
    span: &AudioSpan,
    from: i64,
    block_start: i64,
    contribution: &[f32],
    samples: &mut [f32],
) -> Result<f32, PreviewError> {
    let mut cached_frame = i64::MIN;
    let mut gain_left = 0f32;
    let mut gain_right = 0f32;
    let mut peak = 0f32;
    for (offset, pair) in contribution.as_chunks::<2>().0.iter().enumerate() {
        let timeline = from + offset as i64;
        let frame = timebase
            .output_frame(timeline)
            .map_err(|error| PreviewError::new(error.to_string()))?;
        if frame != cached_frame {
            cached_frame = frame;
            (gain_left, gain_right) = gains(span, fps, frame)?;
        }
        let index = usize::try_from(timeline - block_start)
            .map_err(|_| PreviewError::new("音声 buffer の位置が不正です"))?;
        let Some(dest) = index.checked_mul(2) else {
            return Err(PreviewError::new("音声 buffer の位置が不正です"));
        };
        if dest + 1 < samples.len() {
            let left = pair[0] * gain_left;
            let right = pair[1] * gain_right;
            samples[dest] += left;
            samples[dest + 1] += right;
            peak = peak.max(left.abs()).max(right.abs());
        }
    }
    Ok(peak)
}

fn gains(span: &AudioSpan, fps: Rational, frame: i64) -> Result<(f32, f32), PreviewError> {
    let gain = render_segment_gain(&span.segment, fps, frame).ok_or_else(|| {
        PreviewError::new(format!("{} の音量を計算できません", span.segment.clip.name))
    })?;
    if !gain.is_finite() {
        return Err(PreviewError::new(format!(
            "{} の音量が不正です",
            span.segment.clip.name
        )));
    }
    let (left, right) = audio_mix_gains(0.0, span.segment.mixer_pan);
    let left = gain * left;
    let right = gain * right;
    if !left.is_finite() || !right.is_finite() {
        return Err(PreviewError::new(format!(
            "{} の定位が不正です",
            span.segment.clip.name
        )));
    }
    Ok((left as f32, right as f32))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use std::path::Path;

    use mvm_project::{Project, TimelineClip, TimelineClipKind, TrackRef};

    use super::*;
    use crate::audio_plan::{audio_spans, output_timebase};

    fn tone_file(directory: &Path) -> PathBuf {
        let samples: Vec<f32> = (0..96_000)
            .flat_map(|index| {
                let value =
                    ((index as f64 * 440.0 * std::f64::consts::TAU / 48_000.0).sin() * 0.5) as f32;
                [value, value]
            })
            .collect();
        let path = directory.join("tone.wav");
        mvm_media::write_pcm16_wav(&path, 48_000, 2, &samples).unwrap();
        path
    }

    fn project(path: &Path, preserve_pitch: bool) -> Project {
        let clip = TimelineClip {
            kind: TimelineClipKind::Audio,
            media_path: path.to_path_buf(),
            name: "tone".to_owned(),
            id: "tone".to_owned(),
            media_item_id: "item".to_owned(),
            source_fps: Rational::new(60, 1),
            source_frame_count: 120,
            source_in_frame: 0,
            source_out_frame: 120,
            track: TrackRef::audio(0),
            speed: Rational::new(2, 1),
            preserve_pitch,
            ..TimelineClip::default()
        };
        Project {
            timeline_clips: vec![clip],
            ..Project::default()
        }
    }

    fn mixed(project: &Project, block: usize) -> Vec<f32> {
        let spans = audio_spans(project).unwrap();
        let timebase = output_timebase(project).unwrap();
        let mut mixer = AudioMixer::open(spans, timebase, project.timeline_fps).unwrap();
        let mut out = vec![0f32; 48_000 * 2];
        for (index, chunk) in out.chunks_mut(block * 2).enumerate() {
            mixer.mix((index * block) as i64, chunk).unwrap();
        }
        out
    }

    fn rising_crossings(samples: &[f32]) -> usize {
        let left: Vec<f32> = samples.iter().step_by(2).copied().collect();
        left.windows(2)
            .filter(|pair| pair[0] < 0.0 && pair[1] >= 0.0)
            .count()
    }

    #[test]
    fn pitch_preserved_speed_up_keeps_the_frequency_and_plain_speed_up_raises_it() {
        let directory = std::env::temp_dir().join(format!("mvm-mix-pitch-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let path = tone_file(&directory);

        // 2 秒の素材を 200% で 1 秒に詰める。
        let kept = rising_crossings(&mixed(&project(&path, true), 4800));
        assert!((430..=450).contains(&kept), "ピッチ保持の交差 {kept}");
        // 負の対照: ピッチ保持しない 200% は音程が倍になる。
        let raised = rising_crossings(&mixed(&project(&path, false), 4800));
        assert!((870..=890).contains(&raised), "通常の交差 {raised}");
        std::fs::remove_dir_all(&directory).unwrap();
    }

    #[test]
    fn continuing_blocks_match_one_long_block() {
        let directory = std::env::temp_dir().join(format!("mvm-mix-blocks-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let path = tone_file(&directory);
        let project = project(&path, true);
        let small = mixed(&project, 480);
        let large = mixed(&project, 48_000);
        assert_eq!(small.len(), large.len());
        let differing = small
            .iter()
            .zip(&large)
            .filter(|(left, right)| (*left - *right).abs() > 1e-6)
            .count();
        assert_eq!(differing, 0, "区切りの違いで出力が変わりました");
        std::fs::remove_dir_all(&directory).unwrap();
    }

    #[test]
    fn meters_keep_the_largest_value_until_taken() {
        let meters = AudioMeters::default();
        meters.record(&[0.25, 0.5], 0.5);
        meters.record(&[0.75], 0.25);
        assert_eq!(
            meters.take(),
            MeterPeaks {
                tracks: vec![0.75, 0.5],
                master: 0.5
            }
        );
        assert_eq!(meters.take(), MeterPeaks::default());
    }

    #[test]
    fn monitor_volume_rejects_negative_and_non_finite_values() {
        let monitor = MonitorVolume::default();
        assert_eq!(monitor.get(), 1.0);
        assert!(monitor.set(-0.1).is_err());
        assert!(monitor.set(f32::NAN).is_err());
        monitor.set(0.5).unwrap();
        assert_eq!(monitor.get(), 0.5);
    }
}
