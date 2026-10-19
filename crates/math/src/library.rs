//! 描けた artifact の snapshot と、Project の clip がどの描画を要り、ある frame で何を見せるか。
//!
//! preview と書き出しは同じ [`MathLibrary`] と同じ関数で frame の絵を決める。snapshot は immutable で、
//! 描画の service が新しい artifact を足すたびに作り直して `Arc` で渡す。

use std::collections::HashMap;
use std::sync::Arc;

use mvm_project::equation_sequence::EquationSequenceClipData;
use mvm_project::math_clip::MathIntroKind;
use mvm_project::timeline::{clip_interval, evaluate_equation_clip, math_intro_source_frame_at};
use mvm_project::{
    Project, TimelineClip, TimelineClipKind, TimelineTransition, TransitionKind, parse_argb_color,
};

use crate::artifact::{Mask, MathArtifact};
use crate::compose::{MathPaint, PaintLayer};
use crate::equation::{action_order, compile};
use crate::error::MathError;
use crate::graph::render::{LABEL_FONT_EM, graph_label_texts};
use crate::job::{MathJob, Parity, TexSpec, pulse_alpha};
use crate::segments::{match_segments, segment_tex};

/// 強調（outline・pulse）の色。
pub const ACTION_ACCENT: u32 = 0xFFFF_FF00;

#[derive(Clone, Debug, Default)]
pub struct MathLibrary {
    artifacts: HashMap<MathJob, Arc<MathArtifact>>,
}

impl MathLibrary {
    pub fn new(artifacts: HashMap<MathJob, Arc<MathArtifact>>) -> Self {
        Self { artifacts }
    }

    pub fn get(&self, job: &MathJob) -> Option<&Arc<MathArtifact>> {
        self.artifacts.get(job)
    }

    pub fn contains(&self, job: &MathJob) -> bool {
        self.artifacts.contains_key(job)
    }

    pub fn len(&self) -> usize {
        self.artifacts.len()
    }

    pub fn is_empty(&self) -> bool {
        self.artifacts.is_empty()
    }

    /// Graph のラベルの mask。
    pub fn graph_label(&self, text: &str) -> Option<Arc<Mask>> {
        self.get(&label_job(text))?.frame("static", 0).cloned()
    }
}

fn tex(clip: &TimelineClip) -> TexSpec {
    TexSpec {
        source: clip.math.source.clone(),
        font_size: clip.math.font_size,
    }
}

fn label_job(text: &str) -> MathJob {
    MathJob::Static(TexSpec {
        source: text.to_owned(),
        font_size: LABEL_FONT_EM,
    })
}

fn argb(text: &str) -> Result<u32, MathError> {
    parse_argb_color(text).ok_or_else(|| MathError::new(format!("色が不正です: {text}")))
}

/// 数式の変形の描画要求。両端が数式 clip でなければ None。
pub fn transition_job(
    project: &Project,
    transition: &TimelineTransition,
) -> Result<Option<MathJob>, MathError> {
    if transition.kind != TransitionKind::MathTransform {
        return Ok(None);
    }
    let (Some(outgoing), Some(incoming)) = (
        project.clip(&transition.outgoing_clip_id),
        project.clip(&transition.incoming_clip_id),
    ) else {
        return Ok(None);
    };
    if outgoing.kind != TimelineClipKind::Math || incoming.kind != TimelineClipKind::Math {
        return Ok(None);
    }
    let source_segments = segment_tex(&outgoing.math.source);
    let target_segments = segment_tex(&incoming.math.source);
    let matching = match_segments(&source_segments, &target_segments);
    Ok(Some(MathJob::Transform {
        source: tex(outgoing),
        target: tex(incoming),
        frames: transition.frames_before_cut + transition.frames_after_cut,
        pairs: matching
            .pairs
            .iter()
            .map(|pair| (pair.source, pair.target))
            .collect(),
        unmatched_source: matching.unmatched_source,
        unmatched_target: matching.unmatched_target,
        source_segments: source_segments
            .into_iter()
            .map(|segment| segment.text)
            .collect(),
        target_segments: target_segments
            .into_iter()
            .map(|segment| segment.text)
            .collect(),
        parity: Parity::of(project.output_width, project.output_height),
    }))
}

fn equation_job(project: &Project, data: &EquationSequenceClipData) -> Result<MathJob, MathError> {
    Ok(MathJob::Equation(compile(
        data,
        Parity::of(project.output_width, project.output_height),
    )?))
}

/// clip が要る描画（変形を除く）。描けない Project の値は失敗にする。
pub fn clip_jobs(project: &Project, clip: &TimelineClip) -> Result<Vec<MathJob>, MathError> {
    Ok(match clip.kind {
        TimelineClipKind::Math => {
            let mut jobs = vec![MathJob::Static(tex(clip))];
            if clip.math_animation.intro == MathIntroKind::Write {
                jobs.push(MathJob::Write {
                    tex: tex(clip),
                    frames: clip.math_animation.intro_frames,
                });
            }
            jobs
        }
        TimelineClipKind::EquationSequence => vec![equation_job(project, &clip.equation_sequence)?],
        TimelineClipKind::Graph => graph_label_texts(&clip.graph)
            .iter()
            .map(|text| label_job(text))
            .collect(),
        _ => Vec::new(),
    })
}

/// Project の有効な clip と数式の変形が要る描画。playhead に近い clip から並べる（先に描く）。
/// 描けない値の clip は飛ばす（その理由は [`clip_jobs`] が clip ごとに返す）。
pub fn wanted_jobs(project: &Project, playhead: i64) -> Vec<MathJob> {
    let mut ranked: Vec<(i64, MathJob)> = Vec::new();
    let distance = |clip: &TimelineClip| -> i64 {
        match clip_interval(project, clip) {
            Ok((start, _)) if playhead < start => start - playhead,
            Ok((_, end)) if playhead >= end => playhead - end + 1,
            Ok(_) => 0,
            Err(_) => i64::MAX,
        }
    };
    for clip in &project.timeline_clips {
        if !clip.enabled {
            continue;
        }
        if let Ok(jobs) = clip_jobs(project, clip) {
            let rank = distance(clip);
            ranked.extend(jobs.into_iter().map(|job| (rank, job)));
        }
    }
    for transition in &project.timeline_transitions {
        let enabled = |id: &str| project.clip(id).is_some_and(|clip| clip.enabled);
        if !enabled(&transition.outgoing_clip_id) || !enabled(&transition.incoming_clip_id) {
            continue;
        }
        if let Ok(Some(job)) = transition_job(project, transition) {
            let rank = project
                .clip(&transition.incoming_clip_id)
                .map_or(i64::MAX, distance);
            ranked.push((rank, job));
        }
    }
    ranked.sort_by_key(|(rank, _)| *rank);
    let mut jobs: Vec<MathJob> = Vec::with_capacity(ranked.len());
    for (_, job) in ranked {
        if !jobs.contains(&job) {
            jobs.push(job);
        }
    }
    jobs
}

fn missing(job: &MathJob) -> MathError {
    let what = match job {
        MathJob::Static(_) => "数式",
        MathJob::Write { .. } => "数式の Write",
        MathJob::Transform { .. } => "数式の変形",
        MathJob::Equation(_) => "数式列",
    };
    MathError::new(format!("{what}の描画が済んでいません"))
}

fn frame<'a>(
    library: &'a MathLibrary,
    job: &MathJob,
    name: &str,
    index: i64,
) -> Result<Option<&'a Arc<Mask>>, MathError> {
    let Some(artifact) = library.get(job) else {
        return Ok(None);
    };
    let index = usize::try_from(index).map_err(|_| MathError::new("数式の frame が不正です"))?;
    artifact
        .frame(name, index)
        .map(Some)
        .ok_or_else(|| MathError::new(format!("数式の {name} の {index} 枚目がありません")))
}

fn lerp_color(from: u32, to: u32, numerator: i64, denominator: i64) -> u32 {
    let t = if denominator > 0 {
        (numerator as f64 / denominator as f64).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let a = from.to_be_bytes();
    let b = to.to_be_bytes();
    let mixed: Vec<u8> = a
        .iter()
        .zip(b.iter())
        .map(|(x, y)| (f64::from(*x) + (f64::from(*y) - f64::from(*x)) * t).round() as u8)
        .collect();
    u32::from_be_bytes([mixed[0], mixed[1], mixed[2], mixed[3]])
}

/// 数式・数式列の clip を timeline の `frame` で描く絵。描画が済んでいなければ `Ok(None)`。
/// `clip` は Project の元の clip（区間に切った clip ではない）。
pub fn math_paint(
    project: &Project,
    library: &MathLibrary,
    clip: &TimelineClip,
    frame: i64,
) -> Result<Option<MathPaint>, MathError> {
    let local = frame - clip.timeline_start_frame;
    let fps = project.timeline_fps;
    let to_error = |error: mvm_project::ProjectError| MathError::new(error.to_string());
    match clip.kind {
        TimelineClipKind::Math => {
            let color = argb(&clip.math.color)?;
            for transition in &project.timeline_transitions {
                if transition.kind != TransitionKind::MathTransform
                    || (transition.outgoing_clip_id != clip.id
                        && transition.incoming_clip_id != clip.id)
                {
                    continue;
                }
                let Some(incoming) = project.clip(&transition.incoming_clip_id) else {
                    continue;
                };
                let cut = incoming.timeline_start_frame;
                let begin = cut - transition.frames_before_cut;
                if frame < begin || frame >= cut + transition.frames_after_cut {
                    continue;
                }
                let Some(job) = transition_job(project, transition)? else {
                    continue;
                };
                let Some(mask) = frame_of(library, &job, "transform", frame - begin)? else {
                    return Ok(None);
                };
                return Ok(Some(MathPaint {
                    layers: vec![PaintLayer { mask, color }],
                    background: 0,
                }));
            }
            let background = argb(&clip.math.background_color)?;
            if let Some(source) = math_intro_source_frame_at(clip, fps, local).map_err(to_error)? {
                let job = MathJob::Write {
                    tex: tex(clip),
                    frames: clip.math_animation.intro_frames,
                };
                let Some(mask) = frame_of(library, &job, "write", source)? else {
                    return Ok(None);
                };
                return Ok(Some(MathPaint {
                    layers: vec![PaintLayer { mask, color }],
                    background,
                }));
            }
            let job = MathJob::Static(tex(clip));
            let Some(mask) = frame_of(library, &job, "static", 0)? else {
                return Ok(None);
            };
            Ok(Some(MathPaint {
                layers: vec![PaintLayer { mask, color }],
                background,
            }))
        }
        TimelineClipKind::EquationSequence => {
            let data = &clip.equation_sequence;
            let evaluation = evaluate_equation_clip(clip, fps, local).map_err(to_error)?;
            let job = equation_job(project, data)?;
            let state_index = data
                .states
                .iter()
                .position(|state| state.id == evaluation.state)
                .ok_or_else(|| MathError::new("数式列の状態が見つかりません"))?;
            let color = argb(&data.states[state_index].equation.color)?;
            if evaluation.transition {
                let index = data
                    .transitions
                    .iter()
                    .position(|transition| transition.id == evaluation.transition_id)
                    .ok_or_else(|| MathError::new("数式列の変形が見つかりません"))?;
                let Some(mask) =
                    frame_of(library, &job, &format!("t{index}"), evaluation.local_frame)?
                else {
                    return Ok(None);
                };
                return Ok(Some(MathPaint {
                    layers: vec![PaintLayer { mask, color }],
                    background: 0,
                }));
            }
            if let Some(action_id) = &evaluation.active_action {
                let index = action_order(data)
                    .iter()
                    .position(|id| *id == action_id)
                    .ok_or_else(|| MathError::new("数式列の強調が見つかりません"))?;
                let action = data
                    .actions
                    .iter()
                    .find(|action| action.id == *action_id)
                    .ok_or_else(|| MathError::new("数式列の強調が見つかりません"))?;
                let step = evaluation.local_frame - action.start;
                let (Some(base), Some(accent)) = (
                    frame_of(library, &job, &format!("a{index}/base"), 0)?,
                    frame_of(library, &job, &format!("a{index}"), step)?,
                ) else {
                    return Ok(None);
                };
                let accent_color = match action.operation {
                    mvm_project::equation_sequence::EquationOperation::Outline => ACTION_ACCENT,
                    mvm_project::equation_sequence::EquationOperation::Pulse => {
                        let (numerator, denominator) = pulse_alpha(step, action.duration);
                        lerp_color(color, ACTION_ACCENT, numerator, denominator)
                    }
                };
                return Ok(Some(MathPaint {
                    layers: vec![
                        PaintLayer { mask: base, color },
                        PaintLayer {
                            mask: accent,
                            color: accent_color,
                        },
                    ],
                    background: 0,
                }));
            }
            let Some(mask) = frame_of(library, &job, &format!("s{state_index}"), 0)? else {
                return Ok(None);
            };
            Ok(Some(MathPaint {
                layers: vec![PaintLayer { mask, color }],
                background: 0,
            }))
        }
        _ => Err(MathError::new("数式・数式列の clip ではありません")),
    }
}

fn frame_of(
    library: &MathLibrary,
    job: &MathJob,
    name: &str,
    index: i64,
) -> Result<Option<Arc<Mask>>, MathError> {
    frame(library, job, name, index).map(|mask| mask.cloned())
}

/// 書き出しの前に、出力する数式・数式列・Graph の描画が全て済んでいるか確かめる。
pub fn require_ready(project: &Project, library: &MathLibrary) -> Result<(), MathError> {
    for clip in &project.timeline_clips {
        if !clip.enabled || !project.is_track_output_enabled(clip.track) {
            continue;
        }
        for job in clip_jobs(project, clip)
            .map_err(|error| MathError::new(format!("{}: {error}", clip.name)))?
        {
            if !library.contains(&job) {
                return Err(MathError::new(format!("{}: {}", clip.name, missing(&job))));
            }
        }
    }
    for transition in &project.timeline_transitions {
        let shown = |id: &str| {
            project
                .clip(id)
                .is_some_and(|clip| clip.enabled && project.is_track_output_enabled(clip.track))
        };
        if !shown(&transition.outgoing_clip_id) || !shown(&transition.incoming_clip_id) {
            continue;
        }
        if let Some(job) = transition_job(project, transition)?
            && !library.contains(&job)
        {
            return Err(missing(&job));
        }
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use mvm_core::Rational;
    use mvm_project::TrackRef;
    use mvm_project::math_clip::{MathClipAnimation, MathClipData};

    use super::*;

    fn math(id: &str, source: &str, start: i64, frames: i64) -> TimelineClip {
        TimelineClip {
            kind: TimelineClipKind::Math,
            name: id.to_owned(),
            id: id.to_owned(),
            source_fps: Rational::new(60, 1),
            source_frame_count: frames,
            source_in_frame: 0,
            source_out_frame: frames,
            timeline_start_frame: start,
            track: TrackRef::video(0),
            math: MathClipData {
                source: source.to_owned(),
                ..MathClipData::default()
            },
            ..TimelineClip::default()
        }
    }

    fn mask(width: u32) -> Arc<Mask> {
        Arc::new(Mask::new(width, 1, vec![255; width as usize]).unwrap())
    }

    fn artifact(name: &str, masks: Vec<Arc<Mask>>) -> Arc<MathArtifact> {
        let mut artifact = MathArtifact::default();
        artifact.sequences.insert(name.to_owned(), masks);
        Arc::new(artifact)
    }

    #[test]
    fn write_frames_come_first_then_the_static() {
        let mut clip = math("m", "x^2", 0, 60);
        clip.math_animation = MathClipAnimation {
            intro: MathIntroKind::Write,
            intro_frames: 10,
        };
        let project = Project {
            timeline_clips: vec![clip.clone()],
            ..Project::default()
        };
        let jobs = clip_jobs(&project, &clip).unwrap();
        assert_eq!(jobs.len(), 2);
        let writes: Vec<Arc<Mask>> = (1..=10).map(mask).collect();
        let library = MathLibrary::new(HashMap::from([
            (jobs[0].clone(), artifact("static", vec![mask(99)])),
            (jobs[1].clone(), artifact("write", writes)),
        ]));
        let at = |frame| {
            math_paint(&project, &library, &clip, frame)
                .unwrap()
                .unwrap()
                .layers[0]
                .mask
                .width
        };
        assert_eq!(at(0), 1, "frame 0 は Write の 0 枚目");
        assert_eq!(at(9), 10);
        assert_eq!(at(10), 99, "Write の後は静止");
        assert_eq!(at(59), 99);
        // 描けていなければ None（preview は描かず、書き出しは require_ready で止める）。
        let empty = MathLibrary::default();
        assert_eq!(math_paint(&project, &empty, &clip, 30).unwrap(), None);
        assert!(require_ready(&project, &empty).is_err());
        assert!(require_ready(&project, &library).is_ok());
    }

    #[test]
    fn wanted_jobs_put_the_clip_under_the_playhead_first_and_skip_disabled() {
        let far = math("far", "a", 600, 60);
        let near = math("near", "b", 100, 60);
        let mut off = math("off", "c", 0, 60);
        off.enabled = false;
        let project = Project {
            timeline_clips: vec![far, near, off],
            ..Project::default()
        };
        let jobs = wanted_jobs(&project, 120);
        let sources: Vec<String> = jobs
            .iter()
            .map(|job| match job {
                MathJob::Static(tex) => tex.source.clone(),
                _ => String::new(),
            })
            .collect();
        assert_eq!(sources, vec!["b".to_owned(), "a".to_owned()]);
    }

    #[test]
    fn transform_frames_cover_the_transition_region_on_both_sides_of_the_cut() {
        let a = math("a", "x + x = 2x", 0, 60);
        let b = math("b", "x + x + x = 3x", 60, 60);
        let transition = TimelineTransition {
            id: "t".to_owned(),
            outgoing_clip_id: "a".to_owned(),
            incoming_clip_id: "b".to_owned(),
            frames_before_cut: 0,
            frames_after_cut: 20,
            kind: TransitionKind::MathTransform,
        };
        let project = Project {
            timeline_clips: vec![a.clone(), b.clone()],
            timeline_transitions: vec![transition.clone()],
            ..Project::default()
        };
        let job = transition_job(&project, &transition).unwrap().unwrap();
        let MathJob::Transform {
            pairs,
            unmatched_target,
            frames,
            ..
        } = &job
        else {
            panic!("変形の要求ではない");
        };
        assert_eq!(*frames, 20);
        assert_eq!(pairs, &vec![(0, 0), (1, 1), (2, 2), (3, 5)]);
        assert_eq!(unmatched_target, &vec![3, 4, 6]);
        let transform: Vec<Arc<Mask>> = (1..=20).map(mask).collect();
        let library = MathLibrary::new(HashMap::from([
            (job.clone(), artifact("transform", transform)),
            (MathJob::Static(tex(&a)), artifact("static", vec![mask(50)])),
            (MathJob::Static(tex(&b)), artifact("static", vec![mask(70)])),
        ]));
        let width = |clip: &TimelineClip, frame| {
            math_paint(&project, &library, clip, frame)
                .unwrap()
                .unwrap()
                .layers[0]
                .mask
                .width
        };
        assert_eq!(width(&a, 59), 50, "cut の前は前の式の静止");
        assert_eq!(width(&b, 60), 1, "cut から変形の 0 枚目");
        assert_eq!(width(&b, 79), 20);
        assert_eq!(width(&b, 80), 70, "変形の後は後ろの式の静止");
    }

    #[test]
    fn pulse_accent_color_moves_toward_the_accent_by_the_weight() {
        assert_eq!(lerp_color(0xFF00_0000, 0xFFFF_FF00, 0, 4), 0xFF00_0000);
        assert_eq!(lerp_color(0xFF00_0000, 0xFFFF_FF00, 4, 4), 0xFFFF_FF00);
        assert_eq!(lerp_color(0xFF00_0000, 0xFFFF_FF00, 2, 4), 0xFF80_8000);
    }
}
