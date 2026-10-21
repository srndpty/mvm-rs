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

/// 再生中の音量の表示。mix が書き、UI が読んで 0 へ戻す。値は直前の読み出しから後の絶対値の最大。
/// track は clip の音量・定位・track の gain を掛けた寄与の最大、master はそれらを足した mix の最大で、
/// どちらもモニター音量を掛ける前の値（書き出しと同じ音）。値は左右の組 `[左, 右]`。
///
/// 音声 callback はロックもメモリ確保もしない。track 数の入れ物は再生を始める前に [`Self::prepare`] で
/// 1 回だけ作り、callback はその atomic へ最大を書くだけ。UI の読み出しは値を 0 へ戻すだけで、入れ物は
/// 作り直さない。ロックは入れ物を差し替える UI 側と再生の開始の間だけで取る。
#[derive(Debug, Default)]
pub struct AudioMeters {
    slots: Mutex<Arc<MeterSlots>>,
}

/// 1 回の再生の、track ごとと master の最大。f32 の bit を u32 で持つ。0 以上の有限値は bit の大小と値の
/// 大小が一致するので、`fetch_max` で最大を取れる。
#[derive(Debug, Default)]
struct MeterSlots {
    tracks: Box<[[AtomicU32; 2]]>,
    master: [AtomicU32; 2],
}

impl MeterSlots {
    fn new(track_count: usize) -> Self {
        Self {
            tracks: (0..track_count)
                .map(|_| [AtomicU32::new(0), AtomicU32::new(0)])
                .collect(),
            master: [AtomicU32::new(0), AtomicU32::new(0)],
        }
    }

    /// 音声 callback から呼ぶ。入れ物より多い track は数えない（再生の開始時に数を合わせてある）。
    fn record(&self, tracks: &[[f32; 2]], master: [f32; 2]) {
        for (slot, value) in self.tracks.iter().zip(tracks) {
            raise(slot, *value);
        }
        raise(&self.master, master);
    }

    fn take(&self) -> MeterPeaks {
        MeterPeaks {
            tracks: self.tracks.iter().map(drain).collect(),
            master: drain(&self.master),
        }
    }
}

fn raise(slot: &[AtomicU32; 2], value: [f32; 2]) {
    for (cell, sample) in slot.iter().zip(value) {
        // 値は絶対値なので 0 以上。NaN は最大を壊すので 0 として扱う。
        let level = if sample.is_nan() { 0.0 } else { sample.abs() };
        cell.fetch_max(level.to_bits(), Ordering::Relaxed);
    }
}

fn drain(slot: &[AtomicU32; 2]) -> [f32; 2] {
    [
        f32::from_bits(slot[0].swap(0, Ordering::Relaxed)),
        f32::from_bits(slot[1].swap(0, Ordering::Relaxed)),
    ]
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct MeterPeaks {
    /// audio track の index ごとの `[左, 右]`。
    pub tracks: Vec<[f32; 2]>,
    pub master: [f32; 2],
}

impl AudioMeters {
    /// 再生を始める前（音声 callback の外）に、`track_count` 本分の入れ物を作って差し替える。
    fn prepare(&self, track_count: usize) -> Arc<MeterSlots> {
        let slots = Arc::new(MeterSlots::new(track_count));
        *self
            .slots
            .lock()
            .unwrap_or_else(|poison| poison.into_inner()) = Arc::clone(&slots);
        slots
    }

    /// 直前の呼び出しから後の最大を返し、0 へ戻す（UI から呼ぶ）。
    pub fn take(&self) -> MeterPeaks {
        self.slots
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .take()
    }
}

fn stereo_max(a: [f32; 2], b: [f32; 2]) -> [f32; 2] {
    [a[0].max(b[0]), a[1].max(b[1])]
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
            // メーターは track も master も mix の値（モニター音量を掛ける前）で測る。
            mixer.record_master(samples);
            let gain = monitor.get();
            if gain != 1.0 {
                for sample in samples.iter_mut() {
                    *sample *= gain;
                }
            }
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
    /// clip の素材範囲の終わり（含まない）。ここから先の素材は入れない。
    source_end: i64,
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
    meters: Option<Arc<MeterSlots>>,
    /// track ごとの、この buffer での最大。数は開いたときに決め、mix の中では作り直さない。
    track_peaks: Vec<[f32; 2]>,
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
        let track_count = spans
            .iter()
            .map(|span| span.segment.clip.track.index + 1)
            .max()
            .unwrap_or(0);
        Ok(Self {
            timebase,
            fps,
            spans,
            streams,
            stretches,
            meters: None,
            track_peaks: vec![[0.0; 2]; track_count],
        })
    }

    fn with_meters(mut self, meters: Arc<AudioMeters>) -> Self {
        self.meters = Some(meters.prepare(self.track_peaks.len()));
        self
    }

    /// timeline の `start` sample から、左右を交互に並べた `samples` を埋める。区間の外は無音。
    pub fn mix(&mut self, start: i64, samples: &mut [f32]) -> Result<(), PreviewError> {
        if !samples.len().is_multiple_of(2) {
            return Err(PreviewError::new("音声 buffer の長さが不正です"));
        }
        samples.fill(0.0);
        self.track_peaks
            .iter_mut()
            .for_each(|peak| *peak = [0.0; 2]);
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
            // 数は開いたときに全 span の track から決めてある。
            if let Some(slot) = self.track_peaks.get_mut(span.segment.clip.track.index) {
                *slot = stereo_max(*slot, peak);
            }
        }
        Ok(())
    }

    fn record_master(&self, samples: &[f32]) {
        if let Some(meters) = &self.meters {
            let master = samples
                .as_chunks::<2>()
                .0
                .iter()
                .fold([0.0f32; 2], |peak, pair| {
                    stereo_max(peak, [pair[0].abs(), pair[1].abs()])
                });
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
            // clip の最後の timeline sample が使う素材 sample の次まで（素材範囲の外を混ぜない）。
            let source_end = source_sample(span, span.timeline_end_sample - 1)?
                .map_or(cursor, |last| last + 1)
                .max(cursor);
            self.stretches[index] = Some(Stretch {
                next_timeline: from,
                source_cursor: cursor,
                source_end,
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
        // 素材は clip の素材範囲の終わりまでだけ入れ、そこで stretcher を閉じて残りを出し切る（範囲の外の
        // 音を混ぜない）。出し切っても足りない分は無音。出力が要る長さに届かない stretcher は壊れているので、
        // 繰り返しに上限を設けて止める（無限に待たない）。
        let tempo = span.speed_num as f64 / span.speed_den as f64;
        let budget = (length as f64 * tempo) as usize / STRETCH_CHUNK + 64;
        let mut rounds = 0usize;
        let mut left = vec![0f32; STRETCH_CHUNK];
        let mut right = vec![0f32; STRETCH_CHUNK];
        let mut interleaved = Vec::with_capacity(STRETCH_CHUNK * 2);
        while state.pending.len() < length * 2 && !state.finished {
            rounds += 1;
            if rounds > budget {
                return Err(PreviewError::new("ピッチ保持の音声が出力されません"));
            }
            let remaining = state.source_end.saturating_sub(state.source_cursor).max(0);
            let count = usize::try_from(remaining)
                .unwrap_or(usize::MAX)
                .min(STRETCH_CHUNK);
            if count == 0 {
                state
                    .stretcher
                    .finish()
                    .map_err(|error| PreviewError::new(error.to_string()))?;
            } else {
                stream
                    .copy_from(state.source_cursor, &mut left[..count], &mut right[..count])
                    .map_err(|error| PreviewError::new(error.to_string()))?;
                interleaved.clear();
                for (l, r) in left[..count].iter().zip(&right[..count]) {
                    interleaved.push(*l);
                    interleaved.push(*r);
                }
                state
                    .stretcher
                    .push(&interleaved)
                    .map_err(|error| PreviewError::new(error.to_string()))?;
                state.source_cursor = state
                    .source_cursor
                    .checked_add(count as i64)
                    .ok_or_else(|| PreviewError::new("素材 sample が overflow しました"))?;
            }
            state.finished = state
                .stretcher
                .pull(&mut state.pending)
                .map_err(|error| PreviewError::new(error.to_string()))?;
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
) -> Result<[f32; 2], PreviewError> {
    let mut cached_frame = i64::MIN;
    let mut gain_left = 0f32;
    let mut gain_right = 0f32;
    let mut peak = [0f32; 2];
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
            peak = stereo_max(peak, [left.abs(), right.abs()]);
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

    /// 素材の 0.5〜1.5 秒を切り出した clip の直後（1.5〜1.6 秒）に大きな音を置き、ピッチ保持の 0.5 倍速・
    /// 2 倍速で、出力に範囲の外の音が混ざらないことを確かめる。区切りの大きさにも依らない。
    #[test]
    fn pitch_preserved_trim_never_reads_past_the_source_range() {
        let directory = std::env::temp_dir().join(format!("mvm-mix-trim-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let samples: Vec<f32> = (0..96_000)
            .flat_map(|index| {
                let value = if (72_000..76_800).contains(&index) {
                    0.9
                } else {
                    ((index as f64 * 440.0 * std::f64::consts::TAU / 48_000.0).sin() * 0.1) as f32
                };
                [value, value]
            })
            .collect();
        let path = directory.join("loud-after.wav");
        mvm_media::write_pcm16_wav(&path, 48_000, 2, &samples).unwrap();
        for (num, den) in [(1, 2), (2, 1)] {
            // 素材 frame 30..90（60 fps で 0.5〜1.5 秒）。
            let clip = TimelineClip {
                kind: TimelineClipKind::Audio,
                media_path: path.clone(),
                name: "trim".to_owned(),
                id: "trim".to_owned(),
                media_item_id: "item".to_owned(),
                source_fps: Rational::new(60, 1),
                source_frame_count: 120,
                source_in_frame: 30,
                source_out_frame: 90,
                track: TrackRef::audio(0),
                speed: Rational::new(num, den),
                preserve_pitch: true,
                ..TimelineClip::default()
            };
            let project = Project {
                timeline_clips: vec![clip],
                ..Project::default()
            };
            let spans = audio_spans(&project).unwrap();
            let end = spans[0].timeline_end_sample;
            let timebase = output_timebase(&project).unwrap();
            for block in [480usize, 4096, 96_000] {
                let mut mixer =
                    AudioMixer::open(spans.clone(), timebase, project.timeline_fps).unwrap();
                let mut out = vec![0f32; usize::try_from(end).unwrap() * 2];
                for (index, chunk) in out.chunks_mut(block * 2).enumerate() {
                    mixer.mix((index * block) as i64, chunk).unwrap();
                }
                let peak = out.iter().fold(0f32, |peak, value| peak.max(value.abs()));
                assert!(
                    peak < 0.3,
                    "速度 {num}/{den}・区切り {block}: 範囲の外の音が混ざりました（peak {peak}）"
                );
                let energy: f32 = out.iter().map(|value| value * value).sum();
                assert!(energy > 1.0, "速度 {num}/{den}: 音が出ていません");
            }
        }
        std::fs::remove_dir_all(&directory).unwrap();
    }

    #[test]
    fn meters_keep_the_largest_value_until_taken() {
        let meters = AudioMeters::default();
        let slots = meters.prepare(2);
        slots.record(&[[0.25, 0.125], [0.5, 0.0]], [0.5, 0.25]);
        slots.record(&[[0.0, 0.75]], [0.25, 0.375]);
        // 左右は別々に最大を取る（左の大きい音が右の表示を持ち上げない）。
        assert_eq!(
            meters.take(),
            MeterPeaks {
                tracks: vec![[0.25, 0.75], [0.5, 0.0]],
                master: [0.5, 0.375]
            }
        );
        // 読み出しは値だけを 0 へ戻し、track の数（入れ物）は残す。
        assert_eq!(
            meters.take(),
            MeterPeaks {
                tracks: vec![[0.0; 2]; 2],
                master: [0.0; 2]
            }
        );
        // 読み出した後も、callback は同じ入れ物へ書き続けられる。
        slots.record(&[[0.0, 0.0], [0.125, 0.5]], [0.0, 0.5]);
        assert_eq!(
            meters.take(),
            MeterPeaks {
                tracks: vec![[0.0, 0.0], [0.125, 0.5]],
                master: [0.0, 0.5]
            }
        );
    }

    #[test]
    fn meters_ignore_nan_and_extra_tracks() {
        let meters = AudioMeters::default();
        assert_eq!(meters.take(), MeterPeaks::default(), "再生前は空");
        let slots = meters.prepare(1);
        slots.record(&[[f32::NAN, 0.25], [1.0, 1.0]], [f32::NAN, 0.5]);
        assert_eq!(
            meters.take(),
            MeterPeaks {
                tracks: vec![[0.0, 0.25]],
                master: [0.0, 0.5]
            }
        );
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
