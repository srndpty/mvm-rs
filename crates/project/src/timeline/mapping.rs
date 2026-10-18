//! clip の素材 frame と timeline frame の換算。timeline・preview・書き出しの全てがこれを使う。

use mvm_core::Rational;
use mvm_core::source_frame_mapping::{
    convert_frame_boundary, first_output_position_of_source_frame, source_frame_at_output_position,
};

use crate::error::{ProjectError, ProjectResult, fail};
use crate::math_clip::MathIntroKind;
use crate::model::{
    MAX_CLIP_SPEED_PERCENT, MIN_CLIP_SPEED_PERCENT, Project, TimelineClip, TimelineClipKind,
};

fn convert_boundary(
    frame: i64,
    from: Rational,
    to: Rational,
    round_up: bool,
) -> ProjectResult<i64> {
    if frame < 0 || !from.is_positive() || !to.is_positive() {
        return fail("frame または timebase が不正です");
    }
    convert_frame_boundary(frame, from, to, round_up)
        .ok_or_else(|| ProjectError::new("frame timebase 変換が overflow しました"))
}

/// 素材境界 → timeline 境界（ceil）。
pub fn source_boundary_to_timeline_boundary(
    source_frame: i64,
    source_fps: Rational,
    timeline_fps: Rational,
) -> ProjectResult<i64> {
    convert_boundary(source_frame, source_fps, timeline_fps, true)
}

/// timeline 境界 → 素材境界（floor）。
pub fn timeline_boundary_to_source_boundary(
    timeline_frame: i64,
    source_fps: Rational,
    timeline_fps: Rational,
) -> ProjectResult<i64> {
    convert_boundary(timeline_frame, timeline_fps, source_fps, false)
}

/// clip の実効 fps（素材 fps × 速度、約分済み）。速度 s の clip は素材 fps が f s の clip と同じに扱えるので、
/// timeline との換算は全てこれを使う。実際の素材 fps を使ってよいのは decode の検証と素材 frame の秒換算だけ。
pub fn clip_timebase(clip: &TimelineClip) -> Option<Rational> {
    clip.source_fps.multiply(clip.speed)
}

fn required_timebase(clip: &TimelineClip) -> ProjectResult<Rational> {
    clip_timebase(clip).ok_or_else(|| ProjectError::new("clip の速度と素材 fps の積を表せません"))
}

/// clip_timebase を使う境界換算。素材境界 → timeline は ceil。
pub fn clip_source_boundary_to_timeline(
    clip: &TimelineClip,
    source_frame: i64,
    timeline_fps: Rational,
) -> ProjectResult<i64> {
    source_boundary_to_timeline_boundary(source_frame, required_timebase(clip)?, timeline_fps)
}

/// clip_timebase を使う境界換算。timeline → 素材境界は floor。
pub fn clip_timeline_boundary_to_source(
    clip: &TimelineClip,
    timeline_frame: i64,
    timeline_fps: Rational,
) -> ProjectResult<i64> {
    timeline_boundary_to_source_boundary(timeline_frame, required_timebase(clip)?, timeline_fps)
}

/// clip の timeline 上の尺。1 frame 未満なら失敗する。
pub fn clip_duration_at(clip: &TimelineClip, timeline_fps: Rational) -> ProjectResult<i64> {
    let begin = clip_source_boundary_to_timeline(clip, clip.source_in_frame, timeline_fps)?;
    let end = clip_source_boundary_to_timeline(clip, clip.source_out_frame, timeline_fps)?;
    if end <= begin {
        return fail("clip の timeline duration が 1 frame 未満です");
    }
    Ok(end - begin)
}

pub fn timeline_clip_duration(project: &Project, clip: &TimelineClip) -> ProjectResult<i64> {
    clip_duration_at(clip, project.timeline_fps)
}

/// clip の timeline 区間 [start, end)。失敗の理由には clip 名を付ける。
pub fn clip_interval(project: &Project, clip: &TimelineClip) -> ProjectResult<(i64, i64)> {
    let duration = timeline_clip_duration(project, clip)
        .map_err(|error| ProjectError::new(format!("{}: {error}", clip.name)))?;
    if clip.timeline_start_frame < 0 {
        return fail("timeline duration が overflow しました");
    }
    let end = clip
        .timeline_start_frame
        .checked_add(duration)
        .ok_or_else(|| ProjectError::new("timeline duration が overflow しました"))?;
    Ok((clip.timeline_start_frame, end))
}

fn mapped_source_frame_at(
    clip: &TimelineClip,
    timeline_fps: Rational,
    clip_local_frame: i64,
) -> ProjectResult<i64> {
    let timebase = required_timebase(clip)?;
    let origin = clip_source_boundary_to_timeline(clip, clip.source_in_frame, timeline_fps)?;
    let end = clip_source_boundary_to_timeline(clip, clip.source_out_frame, timeline_fps)?;
    if !(0..end - origin).contains(&clip_local_frame) {
        return fail("clip の範囲外の frame です");
    }
    // EquationSequence と Graph は標本を frame 始点へ置く（切り捨て）。四捨五入では可視範囲の
    // exclusive end を左片が表示しうるため、内部区間と分割の正には使わない。既存素材は四捨五入。
    let position = origin + clip_local_frame;
    let source = if matches!(
        clip.kind,
        TimelineClipKind::EquationSequence | TimelineClipKind::Graph
    ) {
        convert_frame_boundary(position, timeline_fps, timebase, false)
    } else {
        source_frame_at_output_position(position, timebase, timeline_fps)
    };
    let source =
        source.ok_or_else(|| ProjectError::new("timeline frame を素材 frame へ換算できません"))?;
    Ok(source.min(clip.source_frame_count - 1))
}

/// clip 先頭から `clip_local_frame` 番目の timeline frame が表示する素材 frame（素材の絶対 frame）。
/// 四捨五入で書き出しと同じ frame を返す。丸めで source_out_frame（trim で外した次の frame）を返すことが
/// あるのも書き出しと同じ。素材の末尾を越える分だけは最終 frame に止める。フレーム保持は保持する frame。
pub fn clip_source_frame_at(
    clip: &TimelineClip,
    timeline_fps: Rational,
    clip_local_frame: i64,
) -> ProjectResult<i64> {
    let frame = mapped_source_frame_at(clip, timeline_fps, clip_local_frame)?;
    Ok(clip
        .frame_hold
        .as_ref()
        .map_or(frame, |hold| hold.source_frame))
}

/// 速度が約分済みの正の有理数で、10%〜1000% の範囲にあるか。clip と保持元の速度で共有する。
pub fn clip_speed_in_range(speed: Rational) -> bool {
    let (num, den) = (i128::from(speed.num), i128::from(speed.den));
    speed.is_positive()
        && mvm_core::rational::gcd(speed.num, speed.den) == 1
        && num * 100 >= den * i128::from(MIN_CLIP_SPEED_PERCENT)
        && num * 100 <= den * i128::from(MAX_CLIP_SPEED_PERCENT)
}

/// フレーム保持の、保持元の実効 fps での出力位置。元 clip で表示できた frame は、同じ実効 fps なら
/// 必ず出力位置を持つ。
pub fn frame_hold_producer_position(
    clip: &TimelineClip,
    timeline_fps: Rational,
) -> ProjectResult<i64> {
    let Some(hold) = &clip.frame_hold else {
        return fail("フレーム保持がありません");
    };
    if hold.source_frame < 0
        || !hold.source_fps.is_positive()
        || hold.source_frame_count <= hold.source_frame
        || !clip_speed_in_range(hold.speed)
    {
        return fail("保持する素材 frame が不正です");
    }
    let Some(source) = hold.source_fps.multiply(hold.speed) else {
        return fail("保持元の実効 fps を表せません");
    };
    let position = first_output_position_of_source_frame(hold.source_frame, source, timeline_fps);
    let reverse = position
        .and_then(|position| source_frame_at_output_position(position, source, timeline_fps));
    match (position, reverse) {
        (Some(position), Some(reverse)) if reverse == hold.source_frame => Ok(position),
        _ => fail("保持する素材 frame を出力位置へ一意に換算できません"),
    }
}

/// decode に使う素材の範囲。フレーム保持は保持する 1 frame だけの元素材の clip にする。
pub fn clip_video_source(clip: &TimelineClip) -> TimelineClip {
    let mut source = clip.clone();
    if let Some(hold) = source.frame_hold.take() {
        source.source_fps = hold.source_fps;
        source.source_frame_count = hold.source_frame_count;
        source.source_in_frame = hold.source_frame;
        source.source_out_frame = hold.source_frame + 1;
    }
    source
}

/// 書き出しの producer に渡す cut の範囲。位置は素材の 0 frame から数えた output 位置で、
/// [begin, end) が clip_source_frame_at と同じ frame を出す。素材の末尾で、丸めると存在しない frame を指す
/// 位置は cut に含めず、その分の tail_frames は最終 frame を繰り返して埋める。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ClipProducerRange {
    pub begin: i64,
    pub end: i64,
    pub tail_frames: i64,
}

pub fn clip_producer_range(
    clip: &TimelineClip,
    timeline_fps: Rational,
) -> ProjectResult<ClipProducerRange> {
    let error = || ProjectError::new("clip の cut 範囲を換算できません");
    let begin = clip_source_boundary_to_timeline(clip, clip.source_in_frame, timeline_fps)
        .map_err(|_| error())?;
    let end = clip_source_boundary_to_timeline(clip, clip.source_out_frame, timeline_fps)
        .map_err(|_| error())?;
    let timebase = required_timebase(clip).map_err(|_| error())?;
    let beyond_source =
        first_output_position_of_source_frame(clip.source_frame_count, timebase, timeline_fps)
            .ok_or_else(error)?;
    let clipped_end = end.min(beyond_source);
    if clipped_end <= begin {
        return fail("clip の cut 範囲が 1 frame 未満です");
    }
    Ok(ClipProducerRange {
        begin,
        end: clipped_end,
        tail_frames: end - clipped_end,
    })
}

/// フェードと音量カーブの評価に渡す素材 local frame（clip_source_frame_at - in）。
/// 丸めで素材範囲の外を指す分は [0, out - in) に収める。
pub fn clip_fade_source_frame_at(
    clip: &TimelineClip,
    timeline_fps: Rational,
    clip_local_frame: i64,
) -> ProjectResult<i64> {
    let frame = mapped_source_frame_at(clip, timeline_fps, clip_local_frame)?;
    Ok((frame - clip.source_in_frame).clamp(0, clip.source_duration() - 1))
}

/// 数式 clip の intro（Write）のどの素材 local frame を見せるか。intro の外と intro の無い clip は `None`。
/// 素材 frame への換算は fade と同じ四捨五入で、intro は素材 local frame < intro_frames の間だけ見える。
/// preview・書き出しとトランジションの条件は全てこれを通し、境界の丸めを別々に持たない。
pub fn math_intro_source_frame_at(
    clip: &TimelineClip,
    timeline_fps: Rational,
    clip_local_frame: i64,
) -> ProjectResult<Option<i64>> {
    if clip.kind != TimelineClipKind::Math || clip.math_animation.intro == MathIntroKind::None {
        return Ok(None);
    }
    let frame = clip_fade_source_frame_at(clip, timeline_fps, clip_local_frame.max(0))?;
    Ok((frame < clip.math_animation.intro_frames).then_some(frame))
}

/// intro が見える timeline local frame の数（local frame 0 から連続し、intro が見える範囲）。intro の無い clip は 0。
pub fn math_intro_timeline_frames(project: &Project, clip: &TimelineClip) -> ProjectResult<i64> {
    if clip.kind != TimelineClipKind::Math || clip.math_animation.intro == MathIntroKind::None {
        return Ok(0);
    }
    // intro の素材 local frame は timeline local frame に対して単調なので、intro が見えなくなる
    // 最初の frame を二分探索で求める。判定は math_intro_source_frame_at だけで行う。
    let (mut low, mut high) = (0, timeline_clip_duration(project, clip)?);
    while low < high {
        let middle = low + (high - low) / 2;
        if math_intro_source_frame_at(clip, project.timeline_fps, middle)?.is_none() {
            high = middle;
        } else {
            low = middle + 1;
        }
    }
    Ok(low)
}

/// 速度込みの実効 fps が timeline fps と同じか。等速でない clip は timeline frame と素材 frame が 1:1 にならない。
pub fn source_rate_matches_timeline_rate(project: &Project, clip: &TimelineClip) -> bool {
    match (clip_timebase(clip), project.timeline_fps.reduced()) {
        (Some(timebase), Some(timeline)) => timebase == timeline,
        _ => false,
    }
}
