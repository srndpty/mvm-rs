//! 描画・発音の区間分け。書き出し・シャトル/スクラブ・preview が同じ区間分けを使う。
//!
//! トランジションの無い clip は clip そのものが 1 区間になる。トランジションがあると clip は余白の分だけ延び
//! （outgoing は cut の後ろへ、incoming は cut の前へ）、同じ track の 2 clip が重なる。映像はクロスディゾルブに
//! するため incoming の頭の区間 [cut - before, cut + after) を lane 1（outgoing の上）に切り出し、outgoing は下
//! （lane 0）で不透明のまま残す。incoming は素材の余白を不透明な黒で埋めた出力全体の 1 枚として不透明度 p で
//! 重ねる。これで余白の所も含めて A(1 - p) + B p になる。両方を 1 - p / p にすると中央で暗くなる。
//! 音声は clip ごとに 1 区間で、等パワー（sin / cos）の gain で重ねて加算する。

use std::collections::HashMap;
use std::f64::consts::FRAC_PI_2;

use mvm_core::Rational;

use crate::clip_effects::{evaluate_clip_keys, evaluate_clip_opacity, evaluate_clip_volume};
use crate::error::{ProjectResult, fail};
use crate::model::{
    Project, TimelineClip, TimelineTransition, TrackKind, TransitionKind, audio_mix_gains,
};
use crate::timeline::{
    ClipIdIndex, TrimEdge, clip_duration_at, clip_fade_source_frame_at, clip_interval,
    clip_with_edge_at,
};

/// トランジションで値が 0 → 1（または 1 → 0）へ変わる区間（timeline frame）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TransitionEnvelope {
    pub start_frame: i64,
    pub frames: i64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TimelineRenderSegment {
    /// project.timeline_clips の index。
    pub clip_index: usize,
    /// effect の評価に使う元の clip。
    pub original: TimelineClip,
    /// 区間に合わせた clip。timeline 上の位置と素材範囲が区間と一致する（素材 frame の対応は original と同じ）。
    /// decode・producer の cut はこちらを使う。
    pub clip: TimelineClip,
    pub lane: u32,
    /// この区間で 0 → 1。
    pub fade_in: Option<TransitionEnvelope>,
    /// 音声だけ。1 → 0。
    pub fade_out: Option<TransitionEnvelope>,
    pub mixer_gain_db: f64,
    pub mixer_pan: f64,
}

/// 描画するトランジションなら (outgoing, incoming) の index。数式の変形は clip を延ばして重ねる混合ではないので
/// 区間分けでは cut として扱う（変形の画素は両 clip の静止の区間の上に別に重ねる）。
fn rendered_transition(
    project: &Project,
    index: &ClipIdIndex,
    transition: &TimelineTransition,
    kind: TrackKind,
) -> Option<(usize, usize)> {
    let outgoing = index.find(&transition.outgoing_clip_id)?;
    let incoming = index.find(&transition.incoming_clip_id)?;
    if transition.kind != TransitionKind::Blend {
        return None;
    }
    let (a, b) = (
        &project.timeline_clips[outgoing],
        &project.timeline_clips[incoming],
    );
    (a.enabled && b.enabled && a.track.kind == kind && b.track.kind == kind)
        .then_some((outgoing, incoming))
}

/// 描画に使うトランジションがあるか（無効な clip を含むものは数えない）。
pub fn has_rendered_transitions(project: &Project, kind: TrackKind) -> bool {
    if project.timeline_transitions.is_empty() {
        return false;
    }
    let index = ClipIdIndex::new(project);
    project
        .timeline_transitions
        .iter()
        .any(|transition| rendered_transition(project, &index, transition, kind).is_some())
}

/// kind の track に載る有効な clip の描画区間。無効な clip は含めず、無効な clip を含むトランジションは描かない
/// （両側とも cut で切り替わる）。track の mute は呼び出し側が見る。Project は検証を通っていること。
/// 区間を素材 frame へ一意に換算できなければ失敗する。
pub fn timeline_render_segments(
    project: &Project,
    kind: TrackKind,
) -> ProjectResult<Vec<TimelineRenderSegment>> {
    // clip index → (トランジション, cut)
    let mut heads: HashMap<usize, (&TimelineTransition, i64)> = HashMap::new();
    let mut tails: HashMap<usize, (&TimelineTransition, i64)> = HashMap::new();
    let clip_index = ClipIdIndex::new(project);
    for transition in &project.timeline_transitions {
        let Some((outgoing, incoming)) =
            rendered_transition(project, &clip_index, transition, kind)
        else {
            continue;
        };
        let (_, cut) = clip_interval(project, &project.timeline_clips[outgoing])?;
        heads.insert(incoming, (transition, cut));
        tails.insert(outgoing, (transition, cut));
    }
    let mut segments = Vec::new();
    for (index, original) in project.timeline_clips.iter().enumerate() {
        if !original.enabled || original.track.kind != kind {
            continue;
        }
        let mut extended = original.clone();
        let mut fade_in = None;
        let mut fade_out = None;
        let mut head_region_end = 0;
        if let Some((transition, cut)) = heads.get(&index) {
            let region_start = cut - transition.frames_before_cut;
            head_region_end = cut + transition.frames_after_cut;
            fade_in = Some(TransitionEnvelope {
                start_frame: region_start,
                frames: transition.frames_before_cut + transition.frames_after_cut,
            });
            extended = clip_with_edge_at(project, &extended, TrimEdge::Left, region_start)?;
        }
        if let Some((transition, cut)) = tails.get(&index) {
            fade_out = Some(TransitionEnvelope {
                start_frame: cut - transition.frames_before_cut,
                frames: transition.frames_before_cut + transition.frames_after_cut,
            });
            extended = clip_with_edge_at(
                project,
                &extended,
                TrimEdge::Right,
                cut + transition.frames_after_cut,
            )?;
        }
        let segment =
            |clip: TimelineClip, lane, fade_in, fade_out, gain, pan| TimelineRenderSegment {
                clip_index: index,
                original: original.clone(),
                clip,
                lane,
                fade_in,
                fade_out,
                mixer_gain_db: gain,
                mixer_pan: pan,
            };
        if kind == TrackKind::Audio {
            let track = &project.audio_tracks[original.track.index];
            segments.push(segment(
                extended,
                0,
                fade_in,
                fade_out,
                track.mixer_gain_db,
                track.mixer_pan,
            ));
            continue;
        }
        if fade_in.is_none() {
            segments.push(segment(extended, 0, None, None, 0.0, 0.0));
            continue;
        }
        // 映像: 頭のトランジション区間を lane 1 へ切り出し、残りを lane 0 に置く。
        let (_, end) = clip_interval(project, &extended)?;
        let over = if head_region_end < end {
            clip_with_edge_at(project, &extended, TrimEdge::Right, head_region_end)?
        } else {
            extended.clone()
        };
        let over_out = over.source_out_frame;
        let over_synthetic = over.has_synthetic_source_domain();
        segments.push(segment(over, 1, fade_in, None, 0.0, 0.0));
        if head_region_end >= end {
            continue;
        }
        let rest = clip_with_edge_at(project, &extended, TrimEdge::Left, head_region_end)?;
        // 分割と同じく、2 つの区間の素材境界が一致しなければ 1 frame ずれて見える。
        if !over_synthetic && over_out != rest.source_in_frame {
            return fail(format!(
                "トランジションの区間を素材 frame へ一意に換算できません: {}",
                original.name
            ));
        }
        segments.push(segment(rest, 0, None, None, 0.0, 0.0));
    }
    Ok(segments)
}

/// envelope の中の進み具合（frame の中央で測る）。区間の前は 0、後は 1。
pub fn transition_progress(envelope: &TransitionEnvelope, timeline_frame: i64) -> f64 {
    if envelope.frames <= 0 {
        return 1.0;
    }
    (((timeline_frame - envelope.start_frame) as f64 + 0.5) / envelope.frames as f64)
        .clamp(0.0, 1.0)
}

/// 元の clip の local frame を clip の中に収めて、effect と素材 frame を評価する。
struct OriginalFrame {
    local: i64,
    source_local: i64,
    source_duration: i64,
}

fn original_frame_at(
    original: &TimelineClip,
    timeline_fps: Rational,
    timeline_frame: i64,
) -> Option<OriginalFrame> {
    let duration = clip_duration_at(original, timeline_fps).ok()?;
    let local = (timeline_frame - original.timeline_start_frame).clamp(0, duration - 1);
    let source_local = clip_fade_source_frame_at(original, timeline_fps, local).ok()?;
    Some(OriginalFrame {
        local,
        source_local,
        source_duration: original.source_duration(),
    })
}

/// timeline_frame での不透明度。元の clip の effect（key・フェード）を、延ばした区間では clip の端の値のまま評価し、
/// トランジションの進み具合を掛ける。
pub fn render_segment_opacity(
    segment: &TimelineRenderSegment,
    timeline_fps: Rational,
    timeline_frame: i64,
) -> Option<f64> {
    let at = original_frame_at(&segment.original, timeline_fps, timeline_frame)?;
    let opacity = evaluate_clip_opacity(
        &segment.original.effects,
        at.local,
        at.source_local,
        at.source_duration,
    );
    Some(match &segment.fade_in {
        Some(envelope) => opacity * transition_progress(envelope, timeline_frame),
        None => opacity,
    })
}

/// timeline_frame での音量。clip の音量にラウドネス補正・ducking・トランジション（等パワー）・track の音量を掛ける。
pub fn render_segment_gain(
    segment: &TimelineRenderSegment,
    timeline_fps: Rational,
    timeline_frame: i64,
) -> Option<f64> {
    let at = original_frame_at(&segment.original, timeline_fps, timeline_frame)?;
    let effects = &segment.original.effects;
    let mut gain = evaluate_clip_volume(effects, at.local, at.source_local, at.source_duration);
    gain *= 10.0_f64.powf(
        (effects.normalization_gain_db
            + evaluate_clip_keys(&effects.ducking_keys, effects.ducking_db, at.local))
            / 20.0,
    );
    // 等パワー: 重なった 2 clip の gain の二乗和が 1 になる。
    if let Some(envelope) = &segment.fade_in {
        gain *= (transition_progress(envelope, timeline_frame) * FRAC_PI_2).sin();
    }
    if let Some(envelope) = &segment.fade_out {
        gain *= (transition_progress(envelope, timeline_frame) * FRAC_PI_2).cos();
    }
    Some(gain * audio_mix_gains(segment.mixer_gain_db, 0.0).0)
}
