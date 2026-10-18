//! timeline sample の区間を、開いた PCM から足し合わせる。

use std::collections::HashMap;
use std::path::PathBuf;

use mvm_audio::WasapiSink;
use mvm_core::Rational;
use mvm_core::output_timebase::OutputTimebase;
use mvm_media::PcmStream;
use mvm_project::audio_mix_gains;
use mvm_project::render::render_segment_gain;

use crate::audio_plan::{AudioSpan, source_sample};
use crate::error::PreviewError;

pub fn start_audio(
    spans: Vec<AudioSpan>,
    start_sample: i64,
    timebase: OutputTimebase,
    fps: Rational,
) -> Result<WasapiSink, PreviewError> {
    WasapiSink::spawn(move |host| {
        let mut streams = HashMap::new();
        for span in &spans {
            if streams.contains_key(&span.path) {
                continue;
            }
            match PcmStream::open(&span.path) {
                Ok(stream) => {
                    streams.insert(span.path.clone(), stream);
                }
                Err(error) => {
                    host.report(error.to_string());
                    return;
                }
            }
        }
        let result = host.run(start_sample, |start, _count, samples| {
            mix_block(timebase, fps, &spans, &mut streams, start, samples)
                .map_err(|error| error.to_string())
        });
        if let Err(error) = result {
            host.report(error.to_string());
        }
    })
    .map_err(|error| PreviewError::new(error.to_string()))
}

fn mix_block(
    timebase: OutputTimebase,
    fps: Rational,
    spans: &[AudioSpan],
    streams: &mut HashMap<PathBuf, PcmStream>,
    start: i64,
    samples: &mut [f32],
) -> Result<(), PreviewError> {
    if !samples.len().is_multiple_of(2) {
        return Err(PreviewError::new("音声 buffer の長さが不正です"));
    }
    let count = samples.len() / 2;
    samples.fill(0.0);
    let Some(end) = start.checked_add(i64::try_from(count).unwrap_or(i64::MAX)) else {
        return Err(PreviewError::new("sample 位置が overflow しました"));
    };
    for span in spans {
        let from = start.max(span.timeline_start_sample);
        let to = end.min(span.timeline_end_sample);
        if from >= to {
            continue;
        }
        let Some(src_from) = source_sample(span, from)? else {
            continue;
        };
        let Some(last) = to.checked_sub(1) else {
            continue;
        };
        let Some(src_last) = source_sample(span, last)? else {
            continue;
        };
        let src_begin = src_from.min(src_last);
        let src_end = src_from.max(src_last).saturating_add(1);
        let src_len = usize::try_from(src_end.saturating_sub(src_begin))
            .map_err(|_| PreviewError::new("素材 sample が多すぎます"))?;
        let mut left = vec![0f32; src_len];
        let mut right = vec![0f32; src_len];
        let stream = streams
            .get_mut(&span.path)
            .ok_or_else(|| PreviewError::new("音声の decoder がありません"))?;
        stream
            .copy_from(src_begin, &mut left, &mut right)
            .map_err(|error| PreviewError::new(error.to_string()))?;
        let mut cached_frame = i64::MIN;
        let mut gain_left = 0f32;
        let mut gain_right = 0f32;
        let mut timeline = from;
        while timeline < to {
            let frame = timebase
                .output_frame(timeline)
                .map_err(|error| PreviewError::new(error.to_string()))?;
            if frame != cached_frame {
                cached_frame = frame;
                (gain_left, gain_right) = gains(span, fps, frame)?;
            }
            let Some(src) = source_sample(span, timeline)? else {
                timeline += 1;
                continue;
            };
            let offset = usize::try_from(src.saturating_sub(src_begin)).unwrap_or(usize::MAX);
            let (sample_left, sample_right) = match (left.get(offset), right.get(offset)) {
                (Some(left), Some(right)) => (*left, *right),
                _ => (0.0, 0.0),
            };
            let index = usize::try_from(timeline - start)
                .map_err(|_| PreviewError::new("音声 buffer の位置が不正です"))?;
            let Some(dest) = index.checked_mul(2) else {
                return Err(PreviewError::new("音声 buffer の位置が不正です"));
            };
            if dest + 1 < samples.len() {
                samples[dest] += sample_left * gain_left;
                samples[dest + 1] += sample_right * gain_right;
            }
            timeline += 1;
        }
    }
    Ok(())
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
