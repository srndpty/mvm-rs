//! トランジションの編集（既定のトランジション・数式の変形・長さの吸着と変更・削除）。

use std::collections::HashMap;

use crate::error::{ProjectResult, fail};
use crate::model::{
    Project, TimelineClip, TimelineClipKind, TimelineTransition, TrackKind, TransitionKind,
};
use crate::render::timeline_render_segments;

use super::common::{EditResult, LinkMode, TrimEdge, commit_candidate, link_partner_index};
use super::edit::clip_with_edge_at;
use super::validate::{
    DISSOLVE_REQUIREMENT, MATH_TRANSFORM_LOOK_REQUIREMENT, MathTransformLook, TransitionClips,
    dissolve_clip_opaque_over, dissolve_clips_eligible, math_transform_clips_eligible,
    math_transform_look_constant_over, outgoing_head_usage, resolve_transition_clips,
    validate_timeline,
};

/// トランジションを置いた結果。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TransitionEdit {
    /// outgoing / incoming に置いたトランジション（リンク相手の分は含めない最初の 1 つ）。
    pub transition_id: String,
    /// 置いた長さ（余白が足りなければ求めた長さより短い）。
    pub frames: i64,
    /// リンク相手の分を含めて置いた数。
    pub transition_count: usize,
}

/// トランジションを置く編集点（outgoing の終端 = incoming の先頭）。
#[derive(Clone, Debug, PartialEq, Eq)]
struct EditPoint {
    outgoing: String,
    incoming: String,
}

const PROBE: &str = "probe";

/// 編集点にトランジションを置く準備。その編集点の既存のトランジションを外し、両 clip のその端のフェードを
/// 消して、cut の前後に置ける長さの上限（余白と、clip の反対側の端のトランジションが内側に使っていない分）
/// を求める。
fn prepare_edit_transition(
    candidate: &mut Project,
    point: &EditPoint,
    frames: i64,
) -> ProjectResult<(i64, i64)> {
    candidate.timeline_transitions.retain(|transition| {
        transition.outgoing_clip_id != point.outgoing
            && transition.incoming_clip_id != point.incoming
    });
    let clips = resolve_transition_clips(candidate, &point.outgoing, &point.incoming, PROBE, None)?;
    candidate.timeline_clips[clips.outgoing]
        .effects
        .fade_out_frames = 0;
    candidate.timeline_clips[clips.incoming]
        .effects
        .fade_in_frames = 0;
    let mut outgoing_head_transition = 0;
    let mut incoming_tail_inside = 0;
    for other in &candidate.timeline_transitions {
        if other.incoming_clip_id == point.outgoing {
            outgoing_head_transition = other.frames_after_cut;
        }
        if other.outgoing_clip_id == point.incoming {
            incoming_tail_inside = other.frames_before_cut;
        }
    }
    let outgoing_head_inside = outgoing_head_usage(
        candidate,
        &candidate.timeline_clips[clips.outgoing],
        outgoing_head_transition,
    )?;
    let max_before = clips
        .head_handle
        .min(clips.outgoing_duration - outgoing_head_inside)
        .max(0)
        .min(frames);
    let max_after = clips
        .tail_handle
        .min(clips.incoming_duration - incoming_tail_inside)
        .max(0)
        .min(frames);
    Ok((max_before, max_after))
}

/// 編集点と、Linked ならリンク相手どうしが同じ cut で接している編集点（ずらして置いた音声は別の編集点なので
/// 含めない）。
fn linked_edit_points(
    project: &Project,
    outgoing_id: &str,
    incoming_id: &str,
    link_mode: LinkMode,
) -> Vec<EditPoint> {
    let mut points = vec![EditPoint {
        outgoing: outgoing_id.to_owned(),
        incoming: incoming_id.to_owned(),
    }];
    if link_mode != LinkMode::Linked {
        return points;
    }
    let partner = |id: &str| {
        project
            .clip_index(id)
            .and_then(|index| link_partner_index(project, index))
    };
    let (Some(outgoing_partner), Some(incoming_partner)) =
        (partner(outgoing_id), partner(incoming_id))
    else {
        return points;
    };
    let partner_point = EditPoint {
        outgoing: project.timeline_clips[outgoing_partner].id.clone(),
        incoming: project.timeline_clips[incoming_partner].id.clone(),
    };
    let main = resolve_transition_clips(project, outgoing_id, incoming_id, PROBE, None);
    let other = resolve_transition_clips(
        project,
        &partner_point.outgoing,
        &partner_point.incoming,
        PROBE,
        None,
    );
    if let (Ok(main), Ok(other)) = (main, other)
        && main.cut == other.cut
    {
        points.push(partner_point);
    }
    points
}

/// 全編集点の既存のトランジションを外し、cut の前後に置ける長さの上限を編集点どうしの小さい方で求める。
/// 映像と音声は同じ長さ・同じ cut の前後で置くため。
fn edit_points_upper_bounds(
    prepared: &mut Project,
    points: &[EditPoint],
    frames: i64,
) -> ProjectResult<(i64, i64)> {
    let (mut max_before, mut max_after) = (frames, frames);
    for point in points {
        let (before, after) = prepare_edit_transition(prepared, point, frames)?;
        max_before = max_before.min(before);
        max_after = max_after.min(after);
    }
    Ok((max_before, max_after))
}

struct PointClips {
    outgoing: TimelineClip,
    incoming: TimelineClip,
    clips: TransitionClips,
    kind: TransitionKind,
    /// MathTransform で区間の見た目を揃える値（cut の両側の frame の見た目）。
    math_look: MathTransformLook,
}

/// 編集点の clip を引く。長さに依らない条件（Blend は映像の形と区間の端の frame の不透明度、MathTransform は
/// 数式 clip・Write・背景・cut の両側の見た目）はここで理由を付けて断る。
fn resolve_point_clips(
    prepared: &Project,
    points: &[EditPoint],
    kind: TransitionKind,
) -> ProjectResult<Vec<PointClips>> {
    let mut resolved = Vec::with_capacity(points.len());
    for point in points {
        let clips =
            resolve_transition_clips(prepared, &point.outgoing, &point.incoming, PROBE, None)?;
        let outgoing = prepared.timeline_clips[clips.outgoing].clone();
        let incoming = prepared.timeline_clips[clips.incoming].clone();
        // EquationSequence は内部の時間の正の外へ素材範囲を延ばせない。Blend・数式の変形のどちらも作らない。
        for clip in [&outgoing, &incoming] {
            if clip.kind == TimelineClipKind::EquationSequence {
                return fail(format!(
                    "数式 sequence には timeline のトランジションを置けません (内部の変形を使ってください): {}",
                    clip.name
                ));
            }
        }
        let mut math_look = [0.0; 10];
        if kind == TransitionKind::MathTransform {
            math_look = math_transform_clips_eligible(prepared, &clips)?;
        } else if outgoing.track.kind == TrackKind::Video {
            dissolve_clips_eligible(prepared, &clips, 0, 0)?;
        }
        resolved.push(PointClips {
            outgoing,
            incoming,
            clips,
            kind,
            math_look,
        });
    }
    Ok(resolved)
}

/// 区間の中の見た目の条件を満たさないときの理由。
fn span_look_requirement(kind: TransitionKind) -> String {
    match kind {
        TransitionKind::MathTransform => {
            format!("{MATH_TRANSFORM_LOOK_REQUIREMENT}区間の中で見た目が変わっています")
        }
        TransitionKind::Blend => {
            format!("{DISSOLVE_REQUIREMENT}トランジションの区間で不透明度が下がっています")
        }
    }
}

/// 置ける長さは cut の前（before）と後（after）で独立に決まる。
/// - before: incoming を cut - before まで延ばせる（素材 frame にちょうど乗る）こと、映像なら outgoing の最後の
///   before frame が不透明であること
/// - after: outgoing を cut + after まで延ばせること、映像なら incoming を cut + after で分けられることと、
///   incoming の最初の after frame が不透明であること
///
/// 長さごとに区間の不透明度を検査し直すと、長尺素材で候補ごとに数十万 frame を辿る。不透明度の条件は長さに
/// 対して単調（長いほど厳しい）なので、cut から連続して不透明な長さを最初に 1 回だけ数える。素材 frame の
/// 条件は長さごとに判定して覚える。
struct SpanFitter<'a> {
    prepared: &'a Project,
    resolved: &'a [PointClips],
    max_before: i64,
    max_after: i64,
    opaque_before: Option<i64>,
    opaque_after: Option<i64>,
    before_edge: HashMap<i64, bool>,
    after_edge: HashMap<i64, bool>,
    opacity_probes: u64,
    edge_probes: u64,
}

impl<'a> SpanFitter<'a> {
    fn new(
        prepared: &'a Project,
        resolved: &'a [PointClips],
        max_before: i64,
        max_after: i64,
    ) -> Self {
        Self {
            prepared,
            resolved,
            max_before,
            max_after,
            opaque_before: None,
            opaque_after: None,
            before_edge: HashMap::new(),
            after_edge: HashMap::new(),
            opacity_probes: 0,
            edge_probes: 0,
        }
    }

    /// [0, max_before] の外は置けない。
    fn before_fits(&mut self, before: i64, check_opacity: bool) -> bool {
        if before < 0
            || before > self.max_before
            || (check_opacity && before > self.opaque_before())
        {
            return false;
        }
        if let Some(known) = self.before_edge.get(&before) {
            return *known;
        }
        self.edge_probes += 1;
        let fits = self.before_edge_fits(before);
        self.before_edge.insert(before, fits);
        fits
    }

    fn after_fits(&mut self, after: i64, check_opacity: bool) -> bool {
        if after < 0 || after > self.max_after || (check_opacity && after > self.opaque_after()) {
            return false;
        }
        if let Some(known) = self.after_edge.get(&after) {
            return *known;
        }
        self.edge_probes += 1;
        let fits = self.after_edge_fits(after);
        self.after_edge.insert(after, fits);
        fits
    }

    /// cut の前に置ける、不透明な長さの上限（余白の上限で頭打ち）。
    fn opaque_before(&mut self) -> i64 {
        if let Some(value) = self.opaque_before {
            return value;
        }
        let value = self.count_opaque(true);
        self.opaque_before = Some(value);
        value
    }

    fn opaque_after(&mut self) -> i64 {
        if let Some(value) = self.opaque_after {
            return value;
        }
        let value = self.count_opaque(false);
        self.opaque_after = Some(value);
        value
    }

    fn before_edge_fits(&self, before: i64) -> bool {
        before == 0
            || self.resolved.iter().all(|entry| {
                clip_with_edge_at(
                    self.prepared,
                    &entry.incoming,
                    TrimEdge::Left,
                    entry.clips.cut - before,
                )
                .is_ok()
            })
    }

    fn after_edge_fits(&self, after: i64) -> bool {
        after == 0
            || self.resolved.iter().all(|entry| {
                let cut = entry.clips.cut;
                // incoming を cut + after で分けるのはクロスディゾルブの lane 1 だけ。
                clip_with_edge_at(self.prepared, &entry.outgoing, TrimEdge::Right, cut + after)
                    .is_ok()
                    && !(entry.kind == TransitionKind::Blend
                        && entry.outgoing.track.kind == TrackKind::Video
                        && after < entry.clips.incoming_duration
                        && clip_with_edge_at(
                            self.prepared,
                            &entry.incoming,
                            TrimEdge::Right,
                            cut + after,
                        )
                        .is_err())
            })
    }

    /// 映像の編集点ごとに cut から連続して条件を満たす frame を数え、その最小値を返す。Blend は不透明である
    /// こと、MathTransform は見た目が cut の両側と等しいこと。cut の前は outgoing の終端から手前へ、後は
    /// incoming の先頭から奥へ数える。
    fn count_opaque(&mut self, before_cut: bool) -> i64 {
        let mut limit = if before_cut {
            self.max_before
        } else {
            self.max_after
        };
        for entry in self.resolved {
            if entry.outgoing.track.kind != TrackKind::Video {
                continue;
            }
            let clip = if before_cut {
                &entry.outgoing
            } else {
                &entry.incoming
            };
            let mut count = 0;
            while count < limit {
                let local = if before_cut {
                    entry.clips.outgoing_duration - 1 - count
                } else {
                    count
                };
                self.opacity_probes += 1;
                let fits = match entry.kind {
                    TransitionKind::MathTransform => math_transform_look_constant_over(
                        self.prepared,
                        clip,
                        local,
                        local + 1,
                        &entry.math_look,
                    )
                    .is_ok(),
                    TransitionKind::Blend => {
                        dissolve_clip_opaque_over(self.prepared, clip, local, local + 1).is_ok()
                    }
                };
                if !fits {
                    break;
                }
                count += 1;
            }
            limit = count;
        }
        limit
    }
}

/// 選んだ長さは描画区間を作れるはずである。作れなければ理由をそのまま返す（黙って縮めない）。
fn commit_transition_trial(
    project: &mut Project,
    trial: Project,
    selected_clip_id: &str,
) -> EditResult {
    validate_timeline(&trial)?;
    timeline_render_segments(&trial, TrackKind::Video)?;
    timeline_render_segments(&trial, TrackKind::Audio)?;
    let selected = project.clip_index(selected_clip_id);
    commit_candidate(project, trial, selected)
}

fn find_transition<'a>(project: &'a Project, id: &str) -> Option<&'a TimelineTransition> {
    project
        .timeline_transitions
        .iter()
        .find(|transition| transition.id == id)
}

/// 既存のトランジションの長さを変える編集点。Linked ならリンク相手の編集点のうち、既にトランジションがある
/// ものだけを含める（長さの変更で新しく作らない）。
fn span_edit_points(
    project: &Project,
    transition: &TimelineTransition,
    link_mode: LinkMode,
) -> Vec<EditPoint> {
    let mut points = linked_edit_points(
        project,
        &transition.outgoing_clip_id,
        &transition.incoming_clip_id,
        link_mode,
    );
    points.retain(|point| {
        project.timeline_transitions.iter().any(|other| {
            other.outgoing_clip_id == point.outgoing && other.incoming_clip_id == point.incoming
        })
    });
    points
}

/// 編集点（outgoing の終端 = incoming の先頭）に長さ `timeline_frames` の既定のトランジションを置く
/// （Shift+D を編集点で押したとき）。cut を中央にし、片側の余白が足りなければもう片側へ寄せる。素材 frame へ
/// 一意に換算できる長さになるまで縮め、0 frame になれば失敗する。その編集点の既存のトランジションは置き換え、
/// 両 clip のその端のフェードは消す。Linked で、リンク相手どうしも同じ cut で接していれば同じ長さで置く。
pub fn apply_default_edit_transition(
    project: &mut Project,
    outgoing_id: &str,
    incoming_id: &str,
    timeline_frames: i64,
    link_mode: LinkMode,
    new_id: &mut dyn FnMut() -> String,
) -> ProjectResult<TransitionEdit> {
    if timeline_frames < 1 {
        return fail("トランジションの長さが不正です");
    }
    let points = linked_edit_points(project, outgoing_id, incoming_id, link_mode);
    let mut prepared = project.clone();
    let (max_before, max_after) =
        edit_points_upper_bounds(&mut prepared, &points, timeline_frames)?;
    let resolved = resolve_point_clips(&prepared, &points, TransitionKind::Blend)?;
    // before と after のそれぞれで置ける長さを求めてから、合計が最大で cut に最も近い中央の組を選ぶ。
    let mut fitter = SpanFitter::new(&prepared, &resolved, max_before, max_after);
    let choose = |fitter: &mut SpanFitter, check_opacity: bool| -> Option<(i64, i64)> {
        let before_ok: Vec<bool> = (0..=max_before)
            .map(|value| fitter.before_fits(value, check_opacity))
            .collect();
        let after_ok: Vec<bool> = (0..=max_after)
            .map(|value| fitter.after_fits(value, check_opacity))
            .collect();
        for total in (1..=timeline_frames.min(max_before + max_after)).rev() {
            // 合計 total の組のうち、cut を中央に置く組から順に試す。
            for offset in 0..=total {
                for candidate in [
                    total / 2 + offset,
                    total / 2 - offset,
                    (total + 1) / 2 + offset,
                ] {
                    let other = total - candidate;
                    if candidate < 0 || other < 0 || candidate > max_before || other > max_after {
                        continue;
                    }
                    if before_ok[candidate as usize] && after_ok[other as usize] {
                        return Some((candidate, other));
                    }
                }
            }
        }
        None
    };
    let Some((before, after)) = choose(&mut fitter, true) else {
        // 不透明度を見なければ置けるなら、余白ではなく不透明度が理由である。
        return fail(if choose(&mut fitter, false).is_some() {
            span_look_requirement(TransitionKind::Blend)
        } else {
            "素材の余白が足りないためトランジションを作れません".to_owned()
        });
    };
    let mut trial = prepared.clone();
    let mut first_id = None;
    for point in &points {
        let id = new_id();
        if id.is_empty() {
            return fail("トランジションの ID を作れません");
        }
        first_id.get_or_insert_with(|| id.clone());
        trial.timeline_transitions.push(TimelineTransition {
            id,
            outgoing_clip_id: point.outgoing.clone(),
            incoming_clip_id: point.incoming.clone(),
            frames_before_cut: before,
            frames_after_cut: after,
            kind: TransitionKind::Blend,
        });
    }
    commit_transition_trial(project, trial, incoming_id)?;
    Ok(TransitionEdit {
        transition_id: first_id.unwrap_or_default(),
        frames: before + after,
        transition_count: points.len(),
    })
}

/// [lower, upper] のうち fits を満たし target に最も近い値。同じ距離なら towards（+1 / -1）の向きを選ぶ。
fn nearest_in_range(
    target: i64,
    lower: i64,
    upper: i64,
    towards: i64,
    mut fits: impl FnMut(i64) -> bool,
) -> Option<i64> {
    if lower > upper {
        return None;
    }
    let target = target.clamp(lower, upper);
    let inside = |value: i64| (lower..=upper).contains(&value);
    for distance in 0.. {
        let first = target + towards * distance;
        let second = target - towards * distance;
        if !inside(first) && !inside(second) {
            return None;
        }
        if inside(first) && fits(first) {
            return Some(first);
        }
        if inside(second) && fits(second) {
            return Some(second);
        }
    }
    None
}

fn towards_change(requested: i64, current: i64) -> i64 {
    if requested >= current { 1 } else { -1 }
}

/// 隣り合う 2 つの数式 clip の編集点に数式の変形を置く。cut から始め（cut の前 0、後 timeline_frames）、
/// 後ろの clip の尺や区間の見た目の条件で置けない分は縮める（0 frame になれば失敗する）。その編集点の既存の
/// トランジションは置き換え、両 clip のその端のフェードは消す。条件を満たさなければ理由付きで失敗する。
pub fn apply_math_transform_transition(
    project: &mut Project,
    outgoing_id: &str,
    incoming_id: &str,
    timeline_frames: i64,
    new_id: &mut dyn FnMut() -> String,
) -> ProjectResult<TransitionEdit> {
    if timeline_frames < 1 {
        return fail("数式の変形の長さが不正です");
    }
    // 数式 clip はリンクを持たないので、編集点は 1 つだけ。
    let points = [EditPoint {
        outgoing: outgoing_id.to_owned(),
        incoming: incoming_id.to_owned(),
    }];
    let mut prepared = project.clone();
    let (max_before, max_after) =
        edit_points_upper_bounds(&mut prepared, &points, timeline_frames)?;
    let resolved = resolve_point_clips(&prepared, &points, TransitionKind::MathTransform)?;
    // cut から始める（incoming を置いた所から変形が始まる）。incoming が短い・区間の中で見た目が変わるなら、
    // その手前まで縮める。
    let mut fitter = SpanFitter::new(&prepared, &resolved, max_before, max_after);
    let Some(after) = nearest_in_range(max_after, 1, max_after, -1, |value| {
        fitter.after_fits(value, true)
    }) else {
        return fail(if max_after < 1 {
            "数式の変形に使える後ろの数式 clip の尺がありません".to_owned()
        } else {
            span_look_requirement(TransitionKind::MathTransform)
        });
    };
    let id = new_id();
    if id.is_empty() {
        return fail("数式の変形の ID を作れません");
    }
    let mut trial = prepared.clone();
    trial.timeline_transitions.push(TimelineTransition {
        id: id.clone(),
        outgoing_clip_id: outgoing_id.to_owned(),
        incoming_clip_id: incoming_id.to_owned(),
        frames_before_cut: 0,
        frames_after_cut: after,
        kind: TransitionKind::MathTransform,
    });
    commit_transition_trial(project, trial, incoming_id)?;
    Ok(TransitionEdit {
        transition_id: id,
        frames: after,
        transition_count: 1,
    })
}

/// 既存のトランジションの cut 前後に置ける長さの上限 (max_before, max_after)。素材の余白と、clip の反対側の端の
/// トランジションが内側に使っていない分で決まる。Linked ならリンク相手の既存トランジションの上限との小さい方。
pub fn transition_span_limits(
    project: &Project,
    transition_id: &str,
    link_mode: LinkMode,
) -> ProjectResult<(i64, i64)> {
    let Some(transition) = find_transition(project, transition_id) else {
        return fail("トランジションがありません");
    };
    let mut prepared = project.clone();
    edit_points_upper_bounds(
        &mut prepared,
        &span_edit_points(project, transition, link_mode),
        i64::MAX,
    )
}

/// 吸着で何を保つか。
/// - EachSide: 前と後を独立に、それぞれ指定に最も近い値へ（片側の端のドラッグ）
/// - KeepTotal: 総尺（前 + 後）を第一に保ち、その総尺の組のうち前が指定に最も近いもの（長さ・配置の変更と
///   本体のドラッグ）。その総尺で置けなければ最も近い総尺へ落とす
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SpanFitMode {
    EachSide,
    KeepTotal,
}

/// 吸着した長さと、探索の回数（長尺素材で候補ごとに区間を辿り直さないことの回帰検査に使う）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TransitionSpanFit {
    pub frames_before_cut: i64,
    pub frames_after_cut: i64,
    pub opacity_probes: u64,
    pub edge_probes: u64,
}

/// 指定した cut 前後の長さに最も近い、置ける長さ（上限・素材 frame・不透明度を満たす）。同じ距離なら今の値から
/// 離れる側（変えようとした向き）を選ぶ。数値欄やドラッグの値を、素材 frame に乗る長さへ吸着させるのに使う。
pub fn nearest_transition_span(
    project: &Project,
    transition_id: &str,
    frames_before_cut: i64,
    frames_after_cut: i64,
    mode: SpanFitMode,
    link_mode: LinkMode,
) -> ProjectResult<TransitionSpanFit> {
    let Some(transition) = find_transition(project, transition_id) else {
        return fail("トランジションがありません");
    };
    let points = span_edit_points(project, transition, link_mode);
    let mut prepared = project.clone();
    let (max_before, max_after) = edit_points_upper_bounds(&mut prepared, &points, i64::MAX)?;
    let resolved = resolve_point_clips(&prepared, &points, transition.kind)?;
    let mut fitter = SpanFitter::new(&prepared, &resolved, max_before, max_after);
    // 不透明度の上限より外は置けないので、探索をその内側に限る。
    let before_limit = max_before.min(fitter.opaque_before());
    let after_limit = max_after.min(fitter.opaque_after());
    let (current_before, current_after) =
        (transition.frames_before_cut, transition.frames_after_cut);
    let chosen = match mode {
        SpanFitMode::EachSide => {
            // 片側の端のドラッグ。動かさなかった側は今の値のまま（置けるので変わらない）。
            let before = nearest_in_range(
                frames_before_cut,
                0,
                before_limit,
                towards_change(frames_before_cut, current_before),
                |value| fitter.before_fits(value, true),
            );
            let after = nearest_in_range(
                frames_after_cut,
                0,
                after_limit,
                towards_change(frames_after_cut, current_after),
                |value| fitter.after_fits(value, true),
            );
            before.zip(after)
        }
        SpanFitMode::KeepTotal => {
            let top_before = nearest_in_range(before_limit, 0, before_limit, -1, |value| {
                fitter.before_fits(value, true)
            });
            let top_after = nearest_in_range(after_limit, 0, after_limit, -1, |value| {
                fitter.after_fits(value, true)
            });
            match (top_before, top_after) {
                (Some(top_before), Some(top_after)) => {
                    let requested_total = frames_before_cut + frames_after_cut;
                    let mut chosen = None;
                    nearest_in_range(
                        requested_total,
                        1,
                        top_before + top_after,
                        towards_change(requested_total, current_before + current_after),
                        |total| {
                            let before = nearest_in_range(
                                frames_before_cut,
                                (total - top_after).max(0),
                                top_before.min(total),
                                towards_change(frames_before_cut, current_before),
                                |value| {
                                    fitter.before_fits(value, true)
                                        && fitter.after_fits(total - value, true)
                                },
                            );
                            chosen = before.map(|before| (before, total - before));
                            chosen.is_some()
                        },
                    );
                    chosen
                }
                _ => None,
            }
        }
    };
    let Some((before, after)) = chosen.filter(|(before, after)| before + after >= 1) else {
        return fail("素材の余白と不透明度の範囲に置ける長さがありません");
    };
    Ok(TransitionSpanFit {
        frames_before_cut: before,
        frames_after_cut: after,
        opacity_probes: fitter.opacity_probes,
        edge_probes: fitter.edge_probes,
    })
}

/// 既存のトランジションの cut 前後の長さを変える（エフェクトコントロールの長さ・配置）。ID は変えない。
/// Linked ならリンク相手の編集点の既存トランジションも同じ値にする（無ければ作らない）。合計 1 frame 未満・
/// 上限超え・素材 frame に乗らない・不透明度が下がる・値が変わらない場合は理由付きで失敗する
/// （置ける長さへ黙って丸めない）。
pub fn set_timeline_transition_span(
    project: &mut Project,
    transition_id: &str,
    frames_before_cut: i64,
    frames_after_cut: i64,
    link_mode: LinkMode,
) -> ProjectResult<TransitionEdit> {
    let Some(transition) = find_transition(project, transition_id).cloned() else {
        return fail("変更するトランジションがありません");
    };
    let total = frames_before_cut.checked_add(frames_after_cut);
    if frames_before_cut < 0 || frames_after_cut < 0 || total.is_none_or(|total| total < 1) {
        return fail("トランジションの長さは 1 フレーム以上にしてください");
    }
    if transition.frames_before_cut == frames_before_cut
        && transition.frames_after_cut == frames_after_cut
    {
        return fail("トランジションの長さは変わっていません");
    }
    let points = span_edit_points(project, &transition, link_mode);
    let mut prepared = project.clone();
    let (max_before, max_after) = edit_points_upper_bounds(&mut prepared, &points, i64::MAX)?;
    if frames_before_cut > max_before || frames_after_cut > max_after {
        return fail(format!(
            "素材の余白が足りません (cut の前は最大 {max_before}、後は最大 {max_after} フレーム)"
        ));
    }
    let resolved = resolve_point_clips(&prepared, &points, transition.kind)?;
    // 余白の内側でも、素材 frame に乗らない長さ（速度変更）と不透明度の下がる区間は断る。
    let mut fitter = SpanFitter::new(&prepared, &resolved, max_before, max_after);
    if !fitter.before_fits(frames_before_cut, false) || !fitter.after_fits(frames_after_cut, false)
    {
        return fail("この長さは素材のフレームに合わないため設定できません");
    }
    if !fitter.before_fits(frames_before_cut, true) || !fitter.after_fits(frames_after_cut, true) {
        return fail(span_look_requirement(transition.kind));
    }
    // ID と並び順を保つため、元の project の中で値だけを置き換える。
    let mut trial = project.clone();
    for entry in &mut trial.timeline_transitions {
        let target = points.iter().any(|point| {
            entry.outgoing_clip_id == point.outgoing && entry.incoming_clip_id == point.incoming
        });
        if target {
            entry.frames_before_cut = frames_before_cut;
            entry.frames_after_cut = frames_after_cut;
        }
    }
    commit_transition_trial(project, trial, &transition.incoming_clip_id)?;
    Ok(TransitionEdit {
        transition_id: transition_id.to_owned(),
        frames: frames_before_cut + frames_after_cut,
        transition_count: points.len(),
    })
}

pub fn delete_timeline_transition(project: &mut Project, transition_id: &str) -> EditResult {
    let mut candidate = project.clone();
    let before = candidate.timeline_transitions.len();
    candidate
        .timeline_transitions
        .retain(|transition| transition.id != transition_id);
    if candidate.timeline_transitions.len() == before {
        return fail("削除するトランジションがありません");
    }
    commit_candidate(project, candidate, None)
}
