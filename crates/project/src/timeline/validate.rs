//! timeline 全体の検証と、トランジションを clip に合わせる処理。

use std::collections::{HashMap, HashSet};

use crate::clip_effects::evaluate_clip_opacity;
use crate::clip_effects::validate_effect_keys;
use crate::equation_sequence::EquationSequenceClipData;
use crate::error::{ProjectError, ProjectResult, fail};
use crate::graph_clip::{GraphClipData, GraphValidationStatus};
use crate::math_clip::{MathClipAnimation, MathClipData, MathIntroKind};
use crate::model::{
    MAX_CLIP_SPEED_PERCENT, MIN_CLIP_SPEED_PERCENT, Project, TextClipData, TimelineClip,
    TimelineClipKind, TimelineTransition, TrackKind, TrackRef, TransitionKind,
    is_canonical_frame_rate, is_configurable_timeline_frame_rate, is_valid_audio_mix,
    is_valid_project_output_size, parse_argb_color,
};
use crate::subtitles::validate_subtitles;

use super::common::{ClipIdIndex, TrimEdge, edge_range};
use super::mapping::{
    clip_fade_source_frame_at, clip_interval, clip_speed_in_range, clip_timebase,
    frame_hold_producer_position, math_intro_timeline_frames, timeline_clip_duration,
};

/// 検証の結果。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TimelineValidation {
    /// clip と字幕の終端の最大。
    pub total_frames: i64,
    /// 同じ track の重なりを調べるために区間どうしを比べた回数。clip 数に対して線形に留まる
    /// （全組を比べない）ことの回帰検査に使う。
    pub overlap_comparisons: u64,
}

fn valid_text_data(project: &Project, text: &TextClipData) -> bool {
    !text.content.is_empty()
        && !text.font_family.is_empty()
        && (1..=project.output_height).contains(&text.font_size)
        && (0..project.output_width).contains(&text.x)
        && (0..project.output_height).contains(&text.y)
        && matches!(text.alignment.as_str(), "left" | "center" | "right")
        && (0..=32).contains(&text.outline_width)
        && parse_argb_color(&text.color).is_some()
        && parse_argb_color(&text.outline_color).is_some()
        && parse_argb_color(&text.background_color).is_some()
}

/// トランジションが参照する 2 clip。cut と、トランジションに使える素材の余白・clip の尺（timeline frame）。
#[derive(Clone, Copy, Debug)]
pub(crate) struct TransitionClips {
    pub outgoing: usize,
    pub incoming: usize,
    pub cut: i64,
    pub outgoing_duration: i64,
    pub incoming_duration: i64,
    /// incoming の先頭より前に使える素材。
    pub head_handle: i64,
    /// outgoing の終端より後に使える素材。
    pub tail_handle: i64,
}

/// `index` は省略できる（`None` なら clip 列を走査する）。多数のトランジションを解決するときに渡す。
pub(crate) fn resolve_transition_clips(
    project: &Project,
    outgoing_id: &str,
    incoming_id: &str,
    transition_id: &str,
    index: Option<&ClipIdIndex>,
) -> ProjectResult<TransitionClips> {
    let find = |id: &str| index.map_or_else(|| project.clip_index(id), |index| index.find(id));
    let (Some(outgoing), Some(incoming)) = (find(outgoing_id), find(incoming_id)) else {
        return fail(format!(
            "トランジションの clip がありません: {transition_id}"
        ));
    };
    if outgoing == incoming {
        return fail(format!(
            "トランジションの clip がありません: {transition_id}"
        ));
    }
    let (out_clip, in_clip) = (
        &project.timeline_clips[outgoing],
        &project.timeline_clips[incoming],
    );
    if out_clip.track != in_clip.track {
        return fail(format!(
            "トランジションの clip が同じ track にありません: {transition_id}"
        ));
    }
    if out_clip.frame_hold.is_some() || in_clip.frame_hold.is_some() {
        return fail(format!(
            "フレーム保持 clip にはトランジションを置けません: {transition_id}"
        ));
    }
    let (outgoing_start, outgoing_end) = clip_interval(project, out_clip)?;
    let (incoming_start, incoming_end) = clip_interval(project, in_clip)?;
    if outgoing_end != incoming_start {
        return fail(format!(
            "トランジションの clip が接していません: {transition_id}"
        ));
    }
    let (head_lower, _) = edge_range(project, in_clip, TrimEdge::Left)?;
    let (_, tail_upper) = edge_range(project, out_clip, TrimEdge::Right)?;
    Ok(TransitionClips {
        outgoing,
        incoming,
        cut: outgoing_end,
        outgoing_duration: outgoing_end - outgoing_start,
        incoming_duration: incoming_end - incoming_start,
        head_handle: -head_lower,
        tail_handle: tail_upper,
    })
}

pub(crate) fn resolve_transition(
    project: &Project,
    transition: &TimelineTransition,
    index: Option<&ClipIdIndex>,
) -> ProjectResult<TransitionClips> {
    resolve_transition_clips(
        project,
        &transition.outgoing_clip_id,
        &transition.incoming_clip_id,
        &transition.id,
        index,
    )
}

// クロスディゾルブは outgoing を不透明のまま残し、incoming を「素材の余白を黒で埋めた出力全体の 1 枚」として
// 不透明度 p で上に重ねて作る（timeline_render）。incoming が透過・縮小・移動・切り抜き・回転されていると
// その形を出力全体の黒で囲むことになり、Premiere の見た目（下の track が見える）と違う。各 clip を別々に
// 描いてから混ぜる合成を持つまでは、これらの効果と区間の中の不透明度 1 未満は置かせない。
pub(crate) const DISSOLVE_REQUIREMENT: &str = "クロスディゾルブは画面全体を覆う不透明な映像 clip どうしでだけ使えます (文字・画像は不可。位置・拡大・回転・切り抜き・不透明度を既定値に戻してください): ";

pub(crate) const MATH_TRANSFORM_LOOK_REQUIREMENT: &str = "数式の変形の区間では、両端の数式 clip の位置・拡大・回転・切り抜き・不透明度を一定にして揃えてください: ";

fn dissolve_error(clip: &TimelineClip) -> ProjectError {
    ProjectError::new(format!("{DISSOLVE_REQUIREMENT}{}", clip.name))
}

/// 位置・拡大・回転・切り抜きが既定値か。
fn has_identity_geometry(
    position_x: f64,
    position_y: f64,
    scale_x: f64,
    scale_y: f64,
    rotation: f64,
    crops: [f64; 4],
) -> bool {
    position_x == 0.0
        && position_y == 0.0
        && scale_x == 100.0
        && scale_y == 100.0
        && rotation == 0.0
        && crops.iter().all(|crop| *crop == 0.0)
}

/// 長さに依らない条件（種別・位置・拡大・回転・切り抜き）。
fn dissolve_clip_shape_eligible(clip: &TimelineClip) -> ProjectResult<()> {
    let effects = &clip.effects;
    let eligible = matches!(clip.kind, TimelineClipKind::Video | TimelineClipKind::Manim)
        && clip.frame_hold.is_none()
        && has_identity_geometry(
            effects.position_x_percent,
            effects.position_y_percent,
            effects.scale_x_percent,
            effects.scale_y_percent,
            effects.rotation_degrees,
            [
                effects.crop_left_percent,
                effects.crop_top_percent,
                effects.crop_right_percent,
                effects.crop_bottom_percent,
            ],
        );
    if eligible {
        Ok(())
    } else {
        Err(dissolve_error(clip))
    }
}

/// local frame [local_begin, local_end) で不透明度（値・key・fade）が 1 で、形が既定値か。
pub(crate) fn dissolve_clip_opaque_over(
    project: &Project,
    clip: &TimelineClip,
    local_begin: i64,
    local_end: i64,
) -> ProjectResult<()> {
    for local in local_begin..local_end {
        let source_local = clip_fade_source_frame_at(clip, project.timeline_fps, local)?;
        let evaluated = clip.effects.evaluated_at(local);
        let shape = has_identity_geometry(
            evaluated.position_x_percent,
            evaluated.position_y_percent,
            evaluated.scale_x_percent,
            evaluated.scale_y_percent,
            evaluated.rotation_degrees,
            [
                evaluated.crop_left_percent,
                evaluated.crop_top_percent,
                evaluated.crop_right_percent,
                evaluated.crop_bottom_percent,
            ],
        );
        let opacity =
            evaluate_clip_opacity(&clip.effects, local, source_local, clip.source_duration());
        if !shape || opacity < 1.0 {
            return Err(dissolve_error(clip));
        }
    }
    Ok(())
}

fn dissolve_clip_covers_opaque(
    project: &Project,
    clip: &TimelineClip,
    local_begin: i64,
    local_end: i64,
) -> ProjectResult<()> {
    dissolve_clip_shape_eligible(clip)?;
    dissolve_clip_opaque_over(project, clip, local_begin, local_end)
}

pub(crate) fn dissolve_clips_eligible(
    project: &Project,
    clips: &TransitionClips,
    before: i64,
    after: i64,
) -> ProjectResult<()> {
    let outgoing = &project.timeline_clips[clips.outgoing];
    let incoming = &project.timeline_clips[clips.incoming];
    if outgoing.track.kind != TrackKind::Video {
        return Ok(());
    }
    // 延ばした区間は端の frame の値なので、端の frame は必ず含める。
    let outgoing_begin = (clips.outgoing_duration - before)
        .min(clips.outgoing_duration - 1)
        .max(0);
    dissolve_clip_covers_opaque(project, outgoing, outgoing_begin, clips.outgoing_duration)?;
    dissolve_clip_covers_opaque(
        project,
        incoming,
        0,
        after.max(1).min(clips.incoming_duration),
    )
}

/// 数式 clip の先頭の Write が占める timeline frame 数（Write の無い clip は 0）。
pub(crate) fn math_write_timeline_frames(
    project: &Project,
    clip: &TimelineClip,
) -> ProjectResult<i64> {
    math_intro_timeline_frames(project, clip)
        .map_err(|error| ProjectError::new(format!("{}: {error}", clip.name)))
}

/// clip の先頭側で既に使われている frame 数（先頭のトランジションが内側に使う分と Write の大きい方）。
pub(crate) fn outgoing_head_usage(
    project: &Project,
    clip: &TimelineClip,
    head_transition_inside: i64,
) -> ProjectResult<i64> {
    Ok(head_transition_inside.max(math_write_timeline_frames(project, clip)?))
}

/// 数式の変形の区間で揃える見た目（評価した位置・拡大・回転・切り抜き・不透明度）。
/// 両端の clip でこれが一定かつ等しければ、変形の画素は両端の静止と同じ変換を受け、区間の境で段差が出ない。
pub(crate) type MathTransformLook = [f64; 10];

fn math_transform_look_at(
    project: &Project,
    clip: &TimelineClip,
    local: i64,
) -> ProjectResult<MathTransformLook> {
    let source_local = clip_fade_source_frame_at(clip, project.timeline_fps, local)
        .map_err(|error| ProjectError::new(format!("{}: {error}", clip.name)))?;
    let evaluated = clip.effects.evaluated_at(local);
    Ok([
        evaluated.position_x_percent,
        evaluated.position_y_percent,
        evaluated.scale_x_percent,
        evaluated.scale_y_percent,
        evaluated.rotation_degrees,
        evaluated.crop_left_percent,
        evaluated.crop_top_percent,
        evaluated.crop_right_percent,
        evaluated.crop_bottom_percent,
        evaluate_clip_opacity(&clip.effects, local, source_local, clip.source_duration()),
    ])
}

/// 長さに依らない条件。両端が数式 clip、incoming に Write が無い、両方の背景が透明、cut の両側の frame で
/// 見た目が等しい。揃える見た目を返す。描けるかどうか（式・backend）は見ない。
pub(crate) fn math_transform_clips_eligible(
    project: &Project,
    clips: &TransitionClips,
) -> ProjectResult<MathTransformLook> {
    let outgoing = &project.timeline_clips[clips.outgoing];
    let incoming = &project.timeline_clips[clips.incoming];
    if outgoing.kind != TimelineClipKind::Math || incoming.kind != TimelineClipKind::Math {
        let name = if outgoing.kind != TimelineClipKind::Math {
            &outgoing.name
        } else {
            &incoming.name
        };
        return fail(format!(
            "数式の変形は隣り合う数式 clip どうしにだけ置けます: {name}"
        ));
    }
    // 変形は incoming の先頭を使うので、incoming の Write と両立させない。
    if incoming.math_animation.intro != MathIntroKind::None {
        return fail(format!(
            "数式の変形の後ろの数式 clip には Write を付けられません: {}",
            incoming.name
        ));
    }
    // 背景の矩形は式の大きさで変わり、変形の端で段差になる（矩形の補間は未対応）。
    for clip in [outgoing, incoming] {
        let transparent =
            parse_argb_color(&clip.math.background_color).is_some_and(|argb| argb >> 24 == 0);
        if !transparent {
            return fail(format!(
                "数式の変形の両端の数式 clip は背景を透明にしてください: {}",
                clip.name
            ));
        }
    }
    let reference = math_transform_look_at(project, outgoing, clips.outgoing_duration - 1)?;
    if reference != math_transform_look_at(project, incoming, 0)? {
        return fail(format!(
            "{MATH_TRANSFORM_LOOK_REQUIREMENT}{}",
            incoming.name
        ));
    }
    Ok(reference)
}

/// clip の local frame [local_begin, local_end) で見た目が reference と等しいか。
pub(crate) fn math_transform_look_constant_over(
    project: &Project,
    clip: &TimelineClip,
    local_begin: i64,
    local_end: i64,
    reference: &MathTransformLook,
) -> ProjectResult<()> {
    for local in local_begin..local_end {
        if math_transform_look_at(project, clip, local)? != *reference {
            return fail(format!("{MATH_TRANSFORM_LOOK_REQUIREMENT}{}", clip.name));
        }
    }
    Ok(())
}

/// 長さを含めた数式の変形の条件。区間で見た目が一定であることと、outgoing の Write が区間に掛からないこと。
fn math_transform_clips_fit(
    project: &Project,
    clips: &TransitionClips,
    before: i64,
    after: i64,
) -> ProjectResult<()> {
    let reference = math_transform_clips_eligible(project, clips)?;
    let outgoing = &project.timeline_clips[clips.outgoing];
    let incoming = &project.timeline_clips[clips.incoming];
    let write = math_write_timeline_frames(project, outgoing)?;
    if before > clips.outgoing_duration - write {
        return fail(format!(
            "数式の変形の区間が前の数式 clip の Write と重なっています: {}",
            outgoing.name
        ));
    }
    math_transform_look_constant_over(
        project,
        outgoing,
        clips.outgoing_duration - before,
        clips.outgoing_duration,
        &reference,
    )?;
    math_transform_look_constant_over(project, incoming, 0, after, &reference)
}

fn validate_timeline_transitions(project: &Project) -> ProjectResult<()> {
    let mut ids = HashSet::new();
    // clip ID → その clip の内側に入るトランジションの frame 数（先頭側 / 終端側）。
    let mut head_inside: HashMap<&str, i64> = HashMap::new();
    let mut tail_inside: HashMap<&str, i64> = HashMap::new();
    let mut durations: HashMap<&str, i64> = HashMap::new();
    let index = ClipIdIndex::new(project);
    for transition in &project.timeline_transitions {
        if transition.id.is_empty() || !ids.insert(transition.id.as_str()) {
            return fail("トランジション ID が空または重複しています");
        }
        let clips = resolve_transition(project, transition, Some(&index))?;
        let (before, after) = (transition.frames_before_cut, transition.frames_after_cut);
        let total = before.checked_add(after);
        if before < 0 || after < 0 || total.is_none_or(|total| total < 1) {
            return fail(format!("トランジションの長さが不正です: {}", transition.id));
        }
        if before > clips.head_handle || after > clips.tail_handle {
            return fail(format!(
                "トランジションに使う素材の余白が足りません: {}",
                transition.id
            ));
        }
        if before > clips.outgoing_duration || after > clips.incoming_duration {
            return fail(format!(
                "トランジションが clip の尺を超えています: {}",
                transition.id
            ));
        }
        let outgoing = &project.timeline_clips[clips.outgoing];
        let incoming = &project.timeline_clips[clips.incoming];
        if tail_inside.insert(&outgoing.id, before).is_some()
            || head_inside.insert(&incoming.id, after).is_some()
        {
            return fail(format!(
                "同じ clip 端に複数のトランジションがあります: {}",
                transition.id
            ));
        }
        // 同じ clip 端の減衰を 2 通りに表さない（掛け合わさって二重に暗くなる）。
        if outgoing.effects.fade_out_frames != 0 || incoming.effects.fade_in_frames != 0 {
            return fail(format!(
                "トランジションのある clip 端にはフェードを設定できません: {}",
                transition.id
            ));
        }
        match transition.kind {
            TransitionKind::MathTransform => {
                math_transform_clips_fit(project, &clips, before, after)?;
            }
            TransitionKind::Blend => dissolve_clips_eligible(project, &clips, before, after)?,
        }
        durations.insert(&outgoing.id, clips.outgoing_duration);
        durations.insert(&incoming.id, clips.incoming_duration);
    }
    for (clip_id, head) in &head_inside {
        if let Some(tail) = tail_inside.get(clip_id)
            && *head > durations.get(clip_id).copied().unwrap_or(0) - tail
        {
            return fail("clip の前後のトランジションが重なっています");
        }
    }
    Ok(())
}

/// 編集後の candidate のトランジションを clip に合わせる。clip が無い・同じ track で接していない・
/// フレーム保持を含むトランジションは消し、余白や尺が足りなければ縮める（0 frame になれば消す）。
/// track ごとに cut の昇順で処理するので結果は決まる。JSON の読み込みでは呼ばない（読み込みは検証で失敗させる）。
pub fn reconcile_timeline_transitions(candidate: &mut Project) {
    struct Entry {
        index: usize,
        clips: TransitionClips,
        order: (TrackKind, usize, i64),
    }
    let mut entries: Vec<Entry> = {
        let clip_index = ClipIdIndex::new(candidate);
        candidate
            .timeline_transitions
            .iter()
            .enumerate()
            .filter_map(|(index, transition)| {
                let clips = resolve_transition(candidate, transition, Some(&clip_index)).ok()?;
                let track = candidate.timeline_clips[clips.outgoing].track;
                Some(Entry {
                    index,
                    clips,
                    order: (track.kind, track.index, clips.cut),
                })
            })
            .collect()
    };
    entries.sort_by_key(|entry| (entry.order.0 as u8, entry.order.1, entry.order.2));
    let mut keep = vec![false; candidate.timeline_transitions.len()];
    let mut outgoing_used = HashSet::new();
    let mut incoming_used = HashSet::new();
    // clip index → 先頭側のトランジションが clip の内側に使う frame 数。
    let mut head_inside: HashMap<usize, i64> = HashMap::new();
    for entry in &entries {
        let clips = entry.clips;
        if outgoing_used.contains(&clips.outgoing) || incoming_used.contains(&clips.incoming) {
            continue;
        }
        // 先頭のトランジションに加えて、数式 clip の Write も先頭側を使う（Write の後でだけ変形を始める）。
        // 換算できなければ縮められないので、そのまま残して検証に理由を出させる。
        let Ok(used_head) = outgoing_head_usage(
            candidate,
            &candidate.timeline_clips[clips.outgoing],
            head_inside.get(&clips.outgoing).copied().unwrap_or(0),
        ) else {
            keep[entry.index] = true;
            continue;
        };
        let max_before = clips
            .head_handle
            .min(clips.outgoing_duration - used_head)
            .max(0);
        let max_after = clips.tail_handle.min(clips.incoming_duration).max(0);
        let transition = &mut candidate.timeline_transitions[entry.index];
        transition.frames_before_cut = transition.frames_before_cut.clamp(0, max_before);
        transition.frames_after_cut = transition.frames_after_cut.clamp(0, max_after);
        if transition.frames_before_cut + transition.frames_after_cut < 1 {
            continue;
        }
        outgoing_used.insert(clips.outgoing);
        incoming_used.insert(clips.incoming);
        head_inside.insert(clips.incoming, transition.frames_after_cut);
        keep[entry.index] = true;
    }
    let mut kept = keep.into_iter();
    candidate
        .timeline_transitions
        .retain(|_| kept.next().unwrap_or(false));
}

#[derive(Default)]
struct LinkGroupSummary {
    count: usize,
    has_video: bool,
    has_audio: bool,
}

impl LinkGroupSummary {
    fn is_pair(&self) -> bool {
        self.count == 2 && self.has_audio && self.has_video
    }
}

fn validate_clip(project: &Project, clip: &TimelineClip) -> ProjectResult<()> {
    if clip.kind.has_media_path() == clip.media_path.as_os_str().is_empty() || clip.name.is_empty()
    {
        return fail("timeline clip の path または name が空です");
    }
    let is_text = clip.kind == TimelineClipKind::Text;
    if (is_text && !valid_text_data(project, &clip.text))
        || (!is_text && clip.text != TextClipData::default())
    {
        return fail(format!("text clip のデータが不正です: {}", clip.name));
    }
    if clip.kind == TimelineClipKind::Math {
        clip.math.validate(project.output_height).map_err(|error| {
            ProjectError::new(format!(
                "数式 clip のデータが不正です ({error}): {}",
                clip.name
            ))
        })?;
    } else if clip.math != MathClipData::default() {
        return fail(format!(
            "数式 clip 以外が数式のデータを持っています: {}",
            clip.name
        ));
    }
    let unit_speed_unlinked = clip.speed == mvm_core::Rational::new(1, 1)
        && !clip.preserve_pitch
        && clip.frame_hold.is_none()
        && !clip.is_linked();
    if clip.kind == TimelineClipKind::Graph {
        if clip.graph.validate(clip.source_frame_count) != GraphValidationStatus::Valid
            || !unit_speed_unlinked
        {
            return fail("Graph の構造・素材尺・速度・リンクが不正です");
        }
    } else if clip.graph != GraphClipData::default() {
        return fail("Graph 以外が Graph データを持っています");
    }
    if clip.kind == TimelineClipKind::EquationSequence {
        clip.equation_sequence.validate(project.output_height)?;
        let (_, length) = clip.equation_sequence.intervals()?;
        if length != clip.source_frame_count || !unit_speed_unlinked {
            return fail("数式 sequence の素材尺・速度・リンクが不正です");
        }
    } else if clip.equation_sequence != EquationSequenceClipData::default() {
        return fail("数式 sequence 以外が sequence データを持っています");
    }
    if clip.kind == TimelineClipKind::Math {
        clip.math_animation
            .validate(clip.source_duration())
            .map_err(|error| {
                ProjectError::new(format!(
                    "数式 clip の animation が不正です ({error}): {}",
                    clip.name
                ))
            })?;
    } else if clip.math_animation != MathClipAnimation::default() {
        return fail(format!(
            "数式 clip 以外が数式の animation を持っています: {}",
            clip.name
        ));
    }
    if clip.kind.is_still() && !unit_speed_unlinked {
        return fail(format!(
            "text / image / math clip の速度またはリンクが不正です: {}",
            clip.name
        ));
    }
    // 素材の時間軸を持たないので、素材 frame domain は in = 0・out = 尺。fps は置いたときの timeline の値で、
    // timeline の fps を変えても振り直さない（尺は clip の fps で換算される）。trim すると今の fps へ揃う。
    if clip.has_synthetic_source_domain()
        && (clip.source_in_frame != 0 || clip.source_out_frame != clip.source_frame_count)
    {
        return fail(format!(
            "text / image / math clip の素材範囲が尺と一致しません: {}",
            clip.name
        ));
    }
    if !clip.source_fps.is_positive()
        || clip.source_frame_count <= 0
        || clip.source_in_frame < 0
        || clip.source_out_frame <= clip.source_in_frame
        || clip.source_out_frame > clip.source_frame_count
    {
        return fail(format!(
            "timeline clip の source range または FPS が不正です: {}",
            clip.name
        ));
    }
    if !clip_speed_in_range(clip.speed) || clip_timebase(clip).is_none() {
        return fail(format!(
            "timeline clip の速度が不正です (約分済みの {MIN_CLIP_SPEED_PERCENT}%〜{MAX_CLIP_SPEED_PERCENT}%): {}",
            clip.name
        ));
    }
    // 等速のピッチ保持は preview だけが stretcher を通し書き出しは通さない、という食い違いの元になるので
    // 持たせない（速度を変える編集は等速へ戻すときに落とす）。
    if clip.preserve_pitch && clip.speed.num == clip.speed.den {
        return fail(format!(
            "等速の clip はピッチ保持を持てません: {}",
            clip.name
        ));
    }
    if clip.frame_hold.is_some() {
        let hold_valid = clip.kind == TimelineClipKind::Video
            && clip.speed == mvm_core::Rational::new(1, 1)
            && !clip.is_linked()
            && !clip.preserve_pitch
            && frame_hold_producer_position(clip, project.timeline_fps).is_ok();
        if !hold_valid {
            return fail(format!("フレーム保持 clip が不正です: {}", clip.name));
        }
    }
    clip.effects
        .validate(clip.source_duration())
        .map_err(|error| ProjectError::new(format!("{}: {error}", clip.name)))?;
    if !project.is_valid_track_ref(clip.track) {
        return fail(format!(
            "timeline clip の track が存在しません: {}",
            clip.name
        ));
    }
    if !clip.kind.fits_track(clip.track.kind) {
        return fail(format!(
            "timeline clip の種別と track 種別が一致しません: {}",
            clip.name
        ));
    }
    let audio = clip.kind == TimelineClipKind::Audio;
    let effects = &clip.effects;
    let keys_fit = timeline_clip_duration(project, clip)
        .is_ok_and(|duration| validate_effect_keys(effects, duration, audio).is_ok());
    let kind_fits = if audio {
        effects.opacity_keys.is_empty() && effects.opacity_percent == 100.0
    } else {
        effects.volume_keys.is_empty()
            && effects.volume_percent == 100.0
            && effects.normalization_gain_db == 0.0
            && effects.ducking_db == 0.0
            && effects.ducking_keys.is_empty()
            && effects.audio_adjustment_settings.is_empty()
            && effects.audio_adjustment_fingerprint.is_empty()
    };
    if !keys_fit || !kind_fits {
        return fail(format!(
            "{}: キーフレームが clip の種別または尺に合いません",
            clip.name
        ));
    }
    if clip.timeline_start_frame < 0 {
        return fail(format!(
            "timeline clip の start frame が負です: {}",
            clip.name
        ));
    }
    Ok(())
}

fn validate_project_settings(project: &Project) -> ProjectResult<()> {
    if !is_configurable_timeline_frame_rate(project.timeline_fps) {
        return fail("Project timeline FPS が対応外です");
    }
    // 永続化された fps は約分済みの 1 つの表現だけを正にする。120/2 と 60/1 が両方存在すると、
    // UI の一致判定も比較も二重定義になる。
    if !is_canonical_frame_rate(project.timeline_fps) {
        return fail("Project timeline FPS が約分されていません");
    }
    if !is_valid_project_output_size(project.output_width, project.output_height) {
        return fail("Project output size が不正です");
    }
    let valid_point =
        |frame: Option<i64>| frame.is_none_or(|frame| frame >= 0 && frame != i64::MAX);
    if !valid_point(project.in_frame)
        || !valid_point(project.out_frame)
        || matches!((project.in_frame, project.out_frame), (Some(start), Some(end)) if start >= end)
    {
        return fail("イン・アウトの範囲が不正です");
    }
    let mut previous_marker = -1;
    for marker in &project.timeline_markers {
        if *marker <= previous_marker || *marker == i64::MAX {
            return fail("マーカーは重複せず昇順である必要があります");
        }
        previous_marker = *marker;
    }
    if project.video_tracks.is_empty() {
        return fail("video track が 1 本もありません");
    }
    let mut names = HashSet::new();
    for track in project.video_tracks.iter().chain(&project.audio_tracks) {
        if track.name.is_empty() || !names.insert(track.name.as_str()) {
            return fail("track 名が空または重複しています");
        }
    }
    // solo は audio だけが持つ。保存と読み込みの両方で止め、自分で書いたファイルを自分で読めない状態を作らない。
    for track in &project.video_tracks {
        if track.solo {
            return fail("video track は solo を持てません");
        }
        if !track.mixer_name.is_empty() || track.mixer_gain_db != 0.0 || track.mixer_pan != 0.0 {
            return fail("video track はミキサー設定を持てません");
        }
    }
    if project
        .audio_tracks
        .iter()
        .any(|track| !is_valid_audio_mix(track.mixer_gain_db, track.mixer_pan))
    {
        return fail("トラック音量またはパンが範囲外です");
    }
    Ok(())
}

/// timeline 全体を検証する。保存・読み込み・全ての編集の確定がこれを通る。
pub fn validate_timeline(project: &Project) -> ProjectResult<TimelineValidation> {
    validate_project_settings(project)?;
    let mut result = TimelineValidation::default();
    let mut ids = HashSet::new();
    let mut link_groups: HashMap<&str, LinkGroupSummary> = HashMap::new();
    // (track, start, clip index, end)
    let mut intervals: Vec<(TrackRef, i64, usize, i64)> =
        Vec::with_capacity(project.timeline_clips.len());
    for (index, clip) in project.timeline_clips.iter().enumerate() {
        if clip.id.is_empty() || !ids.insert(clip.id.as_str()) {
            return fail("timeline clip ID が空または重複しています");
        }
        validate_clip(project, clip)?;
        if clip.is_linked() {
            let group = link_groups.entry(&clip.link_group_id).or_default();
            group.count += 1;
            group.has_audio |= clip.kind == TimelineClipKind::Audio;
            group.has_video |= clip.kind != TimelineClipKind::Audio;
            if group.count > 2 || (group.count == 2 && !group.is_pair()) {
                return fail(format!(
                    "clipリンクはvideo/audioの1組である必要があります: {}",
                    clip.link_group_id
                ));
            }
        }
        let (start, end) = clip_interval(project, clip)?;
        intervals.push((clip.track, start, index, end));
        result.total_frames = result.total_frames.max(end);
    }
    // 同じ track の重なり。track ごとに開始位置で並べ、それまでの区間の終端の最大とだけ比べる
    // （全組を比べると clip 数の 2 乗になり、大きな timeline の編集が毎回重くなる）。
    intervals
        .sort_by_key(|(track, start, index, _)| (track.kind as u8, track.index, *start, *index));
    let mut reach: Option<(TrackRef, usize, i64)> = None;
    for (track, start, index, end) in &intervals {
        let Some((reach_track, reach_index, reach_end)) = reach.filter(|reach| reach.0 == *track)
        else {
            reach = Some((*track, *index, *end));
            continue;
        };
        result.overlap_comparisons += 1;
        if *start < reach_end {
            // timeline_clips の並びで前にある clip を先に書く。
            let (first, second) = (reach_index.min(*index), reach_index.max(*index));
            return fail(format!(
                "同じ track の timeline clip が重複しています: {} / {}",
                project.timeline_clips[first].name, project.timeline_clips[second].name
            ));
        }
        if *end > reach_end {
            reach = Some((reach_track, *index, *end));
        }
    }
    if let Some((group_id, _)) = link_groups.iter().find(|(_, group)| !group.is_pair()) {
        return fail(format!(
            "clipリンクはvideo/audioの1組である必要があります: {group_id}"
        ));
    }
    validate_subtitles(project)?;
    if let Some(last) = project
        .subtitles
        .as_ref()
        .and_then(|track| track.cues.last())
    {
        result.total_frames = result.total_frames.max(last.end_frame);
    }
    validate_timeline_transitions(project)?;
    Ok(result)
}
