//! Project JSON（.mvm、schema 22）の読み書き。ファイルの読み書き（atomic write など）は呼び出し側が行い、
//! ここは文字列と Project の変換だけを担う。
//!
//! 読み込みは厳しく行う: format marker と schema 22 を必須にし、重複した key・必須 field の欠落・kind に合わない
//! object を拒否する。未知の field は C++ 版と同じく、構造の固定された object（effects・keyframe・文字・数式・
//! 字幕・数式 sequence・グラフ）では拒否し、それ以外では読み飛ばす。旧 schema は読まない。

use std::collections::{BTreeMap, HashSet};
use std::fmt;
use std::path::{Component, Path, PathBuf};

use mvm_core::Rational;
use serde::de::{self, DeserializeSeed, IgnoredAny, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize};

use crate::clip_effects::{ClipEffects, ClipKeyframe, KeyInterpolation};
use crate::equation_sequence::{
    ActionId, BindingStatus, EquationAction, EquationOperation, EquationSequenceClipData,
    EquationState, EquationStepTransition, EquationTargetStatus, PartId, PartPair, SemanticPart,
    SourceBinding, StateId, TransitionId,
};
use crate::error::{ProjectError, ProjectResult, fail};
use crate::graph_clip::{
    GraphAxes, GraphClipData, GraphFunction, GraphFunctionId, GraphIntro, GraphIntroKind,
    GraphViewport,
};
use crate::math_clip::{MathClipAnimation, MathClipData, MathIntroKind};
use crate::media_bin::validate_media_bin;
use crate::model::{
    FrameHold, ManimAsset, ManimGenerationState, MediaFolder, MediaItem, MediaKind,
    PROJECT_SCHEMA_VERSION, Project, SubtitleCue, SubtitleStyle, SubtitleTrack, TextClipData,
    TimelineClip, TimelineClipKind, TimelineTransition, Track, TrackKind, TrackRef, TransitionKind,
    canonical_argb_color, is_sha256,
};
use crate::timeline::validate_timeline;

/// .mvm ファイルであることを識別する marker。拡張子だけを根拠にしない。
pub const FORMAT_MARKER: &str = "mvm-project";

// --- path ---

/// `.` と `..` を表記の上で畳む（I/O をしない）。
fn lexically_normal(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if !normalized.pop() {
                    normalized.push("..");
                }
            }
            other => normalized.push(other),
        }
    }
    normalized
}

/// `base` から見た `path` の相対 path（どちらも絶対・正規化済み）。drive などの root が違えば `None`。
fn lexically_relative(path: &Path, base: &Path) -> Option<PathBuf> {
    let root = |path: &Path| {
        path.components()
            .take_while(|component| matches!(component, Component::Prefix(_) | Component::RootDir))
            .collect::<PathBuf>()
    };
    if root(path) != root(base) {
        return None;
    }
    let path_parts: Vec<Component> = path.components().collect();
    let base_parts: Vec<Component> = base.components().collect();
    let common = path_parts
        .iter()
        .zip(&base_parts)
        .take_while(|(left, right)| left == right)
        .count();
    let mut relative = PathBuf::new();
    for _ in common..base_parts.len() {
        relative.push("..");
    }
    for part in &path_parts[common..] {
        relative.push(part);
    }
    if relative.as_os_str().is_empty() {
        relative.push(".");
    }
    Some(relative)
}

fn generic_string(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

/// project directory から見た相対 path（配下でなくても同じ drive なら `..` を使う）。drive が違えば絶対 path。
fn persisted_source_path(path: &Path, project_directory: &Path) -> String {
    let absolute = lexically_normal(&if path.is_absolute() {
        path.to_path_buf()
    } else {
        project_directory.join(path)
    });
    match lexically_relative(&absolute, project_directory) {
        Some(relative) => generic_string(&relative),
        None => generic_string(&absolute),
    }
}

/// 読み込み時に相対 path を project directory へ置き直す。
fn resolve_source_path(path: &str, project_directory: &Path) -> PathBuf {
    let path = PathBuf::from(path);
    if path.is_absolute() {
        lexically_normal(&path)
    } else {
        lexically_normal(&project_directory.join(path))
    }
}

/// project directory 配下を指す相対 path か（`..` で外へ出ない、`.` そのものではない）。
fn is_project_managed_relative_path(path: &Path) -> bool {
    if path.as_os_str().is_empty() || path.is_absolute() || path.has_root() {
        return false;
    }
    let normalized = lexically_normal(path);
    let mut components = normalized.components();
    matches!(components.next(), Some(Component::Normal(_)))
}

fn project_directory(project_path: &Path) -> ProjectResult<PathBuf> {
    let absolute = std::path::absolute(project_path)
        .map_err(|_| ProjectError::new("Project JSON path が不正です"))?;
    if absolute.file_name().is_none() {
        return fail("Project JSON path が不正です");
    }
    Ok(lexically_normal(absolute.parent().unwrap_or(Path::new(""))))
}

// --- 重複 key の検査 ---

/// JSON 全体を走査し、どの object にも同じ key が 2 回現れないことを確かめる。
struct UniqueKeys;

impl<'de> DeserializeSeed<'de> for UniqueKeys {
    type Value = ();

    fn deserialize<D: Deserializer<'de>>(self, deserializer: D) -> Result<(), D::Error> {
        deserializer.deserialize_any(self)
    }
}

impl<'de> Visitor<'de> for UniqueKeys {
    type Value = ();

    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        formatter.write_str("JSON value")
    }

    fn visit_bool<E>(self, _: bool) -> Result<(), E> {
        Ok(())
    }
    fn visit_i64<E>(self, _: i64) -> Result<(), E> {
        Ok(())
    }
    fn visit_u64<E>(self, _: u64) -> Result<(), E> {
        Ok(())
    }
    fn visit_f64<E>(self, _: f64) -> Result<(), E> {
        Ok(())
    }
    fn visit_str<E>(self, _: &str) -> Result<(), E> {
        Ok(())
    }
    fn visit_unit<E>(self) -> Result<(), E> {
        Ok(())
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<(), A::Error> {
        while seq.next_element_seed(UniqueKeys)?.is_some() {}
        Ok(())
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<(), A::Error> {
        let mut keys = HashSet::new();
        while let Some(key) = map.next_key::<String>()? {
            if !keys.insert(key.clone()) {
                return Err(de::Error::custom(format!("key が重複しています: {key}")));
            }
            map.next_value_seed(UniqueKeys)?;
        }
        Ok(())
    }
}

pub(crate) fn reject_duplicate_keys(text: &str) -> ProjectResult<()> {
    let mut deserializer = serde_json::Deserializer::from_str(text);
    UniqueKeys
        .deserialize(&mut deserializer)
        .and_then(|()| deserializer.end())
        .map_err(|error| ProjectError::new(format!("Project JSON を解析できません: {error}")))
}

/// null を許すが、key の省略は許さない（serde の Option の既定は省略を None にする）。
fn required_nullable<'de, D: Deserializer<'de>, T: Deserialize<'de>>(
    deserializer: D,
) -> Result<Option<T>, D::Error> {
    Option::<T>::deserialize(deserializer)
}

/// key の省略は許すが、null は許さない（serde の Option の既定は null も None にする）。
fn omittable<'de, D: Deserializer<'de>, T: Deserialize<'de>>(
    deserializer: D,
) -> Result<Option<T>, D::Error> {
    T::deserialize(deserializer).map(Some)
}

// --- DTO（JSON の形） ---

#[derive(Serialize, Deserialize)]
struct ProjectDto {
    schema_version: i32,
    format: String,
    timeline_fps_num: i64,
    timeline_fps_den: i64,
    output_width: i32,
    output_height: i32,
    timeline_markers: Vec<i64>,
    #[serde(deserialize_with = "required_nullable")]
    in_frame: Option<i64>,
    #[serde(deserialize_with = "required_nullable")]
    out_frame: Option<i64>,
    #[serde(deserialize_with = "required_nullable")]
    subtitles: Option<SubtitlesDto>,
    video_tracks: Vec<TrackDto>,
    audio_tracks: Vec<TrackDto>,
    manim_assets: Vec<ManimAssetDto>,
    timeline_clips: Vec<ClipDto>,
    timeline_transitions: Vec<TransitionDto>,
    media_folders: Vec<MediaFolderDto>,
    media_items: Vec<MediaItemDto>,
    last_audio_adjustment_settings: String,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SubtitlesDto {
    visible: bool,
    style: SubtitleStyleDto,
    cues: Vec<CueDto>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SubtitleStyleDto {
    font_family: String,
    font_size: i32,
    bold: bool,
    color: String,
    outline_color: String,
    outline_width: i32,
    background_color: String,
    alignment: String,
    side_margin: f64,
    bottom_margin: f64,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CueDto {
    id: String,
    start_frame: i64,
    end_frame: i64,
    content: String,
    /// 未リンクの字幕は項目自体を書かない。空文字列は受け付けない。
    #[serde(
        default,
        deserialize_with = "omittable",
        skip_serializing_if = "Option::is_none"
    )]
    link_clip_id: Option<String>,
}

#[derive(Serialize, Deserialize)]
struct TrackDto {
    name: String,
    muted: bool,
    solo: bool,
    mixer_name: String,
    mixer_gain_db: f64,
    mixer_pan: f64,
}

#[derive(Serialize, Deserialize)]
struct ManimAssetDto {
    script_path: String,
    scene_name: String,
    generated_video_path: String,
    generation_state: String,
    source_fingerprint: String,
}

#[derive(Serialize, Deserialize)]
struct ClipDto {
    kind: String,
    media_path: String,
    name: String,
    id: String,
    source_fps_num: i64,
    source_fps_den: i64,
    source_frame_count: i64,
    source_in_frame: i64,
    source_out_frame: i64,
    timeline_start_frame: i64,
    speed_num: i64,
    speed_den: i64,
    preserve_pitch: bool,
    enabled: bool,
    #[serde(deserialize_with = "required_nullable")]
    frame_hold: Option<FrameHoldDto>,
    track_kind: String,
    track_index: i64,
    media_item_id: String,
    link_group_id: String,
    effects: EffectsDto,
    #[serde(
        default,
        deserialize_with = "omittable",
        skip_serializing_if = "Option::is_none"
    )]
    text: Option<TextDto>,
    #[serde(
        default,
        deserialize_with = "omittable",
        skip_serializing_if = "Option::is_none"
    )]
    math: Option<MathDto>,
    /// 省略は intro 無し。数式 clip 以外には書けない。
    #[serde(
        default,
        deserialize_with = "omittable",
        skip_serializing_if = "Option::is_none"
    )]
    math_animation: Option<MathAnimationDto>,
    #[serde(
        default,
        deserialize_with = "omittable",
        skip_serializing_if = "Option::is_none"
    )]
    graph: Option<GraphDto>,
    #[serde(
        default,
        deserialize_with = "omittable",
        skip_serializing_if = "Option::is_none"
    )]
    equation_sequence: Option<EquationSequenceDto>,
    /// 未知の field。数式 sequence・グラフの clip では拒否する。
    #[serde(flatten, skip_serializing)]
    unknown: BTreeMap<String, IgnoredAny>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct FrameHoldDto {
    source_frame: i64,
    source_fps_num: i64,
    source_fps_den: i64,
    source_frame_count: i64,
    speed_num: i64,
    speed_den: i64,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct EffectsDto {
    position_x_percent: f64,
    position_y_percent: f64,
    scale_x_percent: f64,
    scale_y_percent: f64,
    rotation_degrees: f64,
    opacity_percent: f64,
    volume_percent: f64,
    crop_left_percent: f64,
    crop_top_percent: f64,
    crop_right_percent: f64,
    crop_bottom_percent: f64,
    fade_in_frames: i64,
    fade_out_frames: i64,
    opacity_keys: Vec<KeyDto>,
    volume_keys: Vec<KeyDto>,
    normalization_gain_db: f64,
    ducking_db: f64,
    audio_adjustment_settings: String,
    audio_adjustment_fingerprint: String,
    ducking_keys: Vec<KeyDto>,
    position_x_keys: Vec<KeyDto>,
    position_y_keys: Vec<KeyDto>,
    scale_x_keys: Vec<KeyDto>,
    scale_y_keys: Vec<KeyDto>,
    rotation_keys: Vec<KeyDto>,
    crop_left_keys: Vec<KeyDto>,
    crop_top_keys: Vec<KeyDto>,
    crop_right_keys: Vec<KeyDto>,
    crop_bottom_keys: Vec<KeyDto>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct KeyDto {
    frame: i64,
    value: f64,
    interpolation: String,
    curve_start: f64,
    curve_end: f64,
    /// spline だけが書く（spline では必須）。
    #[serde(
        default,
        deserialize_with = "omittable",
        skip_serializing_if = "Option::is_none"
    )]
    control1: Option<f64>,
    #[serde(
        default,
        deserialize_with = "omittable",
        skip_serializing_if = "Option::is_none"
    )]
    control2: Option<f64>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct TextDto {
    content: String,
    font_family: String,
    font_size: i32,
    x: i32,
    y: i32,
    color: String,
    bold: bool,
    alignment: String,
    outline_color: String,
    outline_width: i32,
    background_color: String,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct MathDto {
    syntax: String,
    source: String,
    font_size: i32,
    color: String,
    background_color: String,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct MathAnimationDto {
    intro: String,
    intro_frames: i64,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct GraphDto {
    viewport: ViewportDto,
    axes: AxesDto,
    functions: Vec<GraphFunctionDto>,
    intro: GraphIntroDto,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ViewportDto {
    x_min: f64,
    x_max: f64,
    y_min: f64,
    y_max: f64,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct AxesDto {
    show_axes: bool,
    show_grid: bool,
    x_label: String,
    y_label: String,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct GraphFunctionDto {
    id: String,
    expression: String,
    label: String,
    color: String,
    stroke_width: f64,
    #[serde(
        default,
        deserialize_with = "omittable",
        skip_serializing_if = "Option::is_none"
    )]
    domain_min: Option<f64>,
    #[serde(
        default,
        deserialize_with = "omittable",
        skip_serializing_if = "Option::is_none"
    )]
    domain_max: Option<f64>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct GraphIntroDto {
    kind: String,
    frames: i64,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct EquationSequenceDto {
    states: Vec<EquationStateDto>,
    transitions: Vec<EquationTransitionDto>,
    actions: Vec<EquationActionDto>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct EquationStateDto {
    id: String,
    equation: MathDto,
    revision: String,
    hold_frames: i64,
    parts: Vec<EquationPartDto>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct EquationPartDto {
    id: String,
    label: String,
    binding: EquationBindingDto,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct EquationBindingDto {
    revision: String,
    begin: i64,
    end: i64,
    expected_text: String,
    status: String,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct EquationTransitionDto {
    id: String,
    from: String,
    to: String,
    frames: i64,
    correspondence: Vec<PartPairDto>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PartPairDto {
    from: String,
    to: String,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct EquationActionDto {
    id: String,
    state: String,
    target: String,
    target_status: String,
    start: i64,
    duration: i64,
    operation: String,
}

#[derive(Serialize, Deserialize)]
struct TransitionDto {
    id: String,
    outgoing_clip_id: String,
    incoming_clip_id: String,
    frames_before_cut: i64,
    frames_after_cut: i64,
    kind: String,
}

#[derive(Serialize, Deserialize)]
struct MediaFolderDto {
    id: String,
    name: String,
    parent_id: String,
}

/// 種別に関係なく全 field を必須にする。該当しない値は 0 で保存し、0 であることは検証が見る。
#[derive(Serialize, Deserialize)]
struct MediaItemDto {
    id: String,
    kind: String,
    media_path: String,
    name: String,
    folder_id: String,
    fps_num: i64,
    fps_den: i64,
    frame_count: i64,
    width: i32,
    height: i32,
    sample_rate: i32,
    duration_samples: i64,
}

// --- 名前の対応 ---

fn interpolation_name(interpolation: KeyInterpolation) -> &'static str {
    match interpolation {
        KeyInterpolation::Linear => "linear",
        KeyInterpolation::EaseIn => "ease_in",
        KeyInterpolation::EaseOut => "ease_out",
        KeyInterpolation::EaseInOut => "ease_in_out",
        KeyInterpolation::Spline => "spline",
    }
}

fn parse_interpolation(name: &str) -> ProjectResult<KeyInterpolation> {
    KeyInterpolation::ALL
        .into_iter()
        .find(|interpolation| interpolation_name(*interpolation) == name)
        .ok_or_else(|| ProjectError::new("未知のキーフレーム補間です"))
}

fn binding_status_name(status: BindingStatus) -> &'static str {
    match status {
        BindingStatus::Bound => "bound",
        BindingStatus::Invalid => "invalid",
    }
}

fn target_status_name(status: EquationTargetStatus) -> &'static str {
    match status {
        EquationTargetStatus::Present => "present",
        EquationTargetStatus::Missing => "missing",
    }
}

fn operation_name(operation: EquationOperation) -> &'static str {
    match operation {
        EquationOperation::Outline => "outline",
        EquationOperation::Pulse => "pulse",
    }
}

// --- Project → DTO ---

fn keys_to_dto(keys: &[ClipKeyframe]) -> Vec<KeyDto> {
    keys.iter()
        .map(|key| {
            let spline = key.interpolation == KeyInterpolation::Spline;
            KeyDto {
                frame: key.frame,
                value: key.value,
                interpolation: interpolation_name(key.interpolation).to_owned(),
                curve_start: key.curve_start,
                curve_end: key.curve_end,
                control1: spline.then_some(key.control1),
                control2: spline.then_some(key.control2),
            }
        })
        .collect()
}

fn effects_to_dto(effects: &ClipEffects) -> EffectsDto {
    EffectsDto {
        position_x_percent: effects.position_x_percent,
        position_y_percent: effects.position_y_percent,
        scale_x_percent: effects.scale_x_percent,
        scale_y_percent: effects.scale_y_percent,
        rotation_degrees: effects.rotation_degrees,
        opacity_percent: effects.opacity_percent,
        volume_percent: effects.volume_percent,
        crop_left_percent: effects.crop_left_percent,
        crop_top_percent: effects.crop_top_percent,
        crop_right_percent: effects.crop_right_percent,
        crop_bottom_percent: effects.crop_bottom_percent,
        fade_in_frames: effects.fade_in_frames,
        fade_out_frames: effects.fade_out_frames,
        opacity_keys: keys_to_dto(&effects.opacity_keys),
        volume_keys: keys_to_dto(&effects.volume_keys),
        normalization_gain_db: effects.normalization_gain_db,
        ducking_db: effects.ducking_db,
        audio_adjustment_settings: effects.audio_adjustment_settings.clone(),
        audio_adjustment_fingerprint: effects.audio_adjustment_fingerprint.clone(),
        ducking_keys: keys_to_dto(&effects.ducking_keys),
        position_x_keys: keys_to_dto(&effects.position_x_keys),
        position_y_keys: keys_to_dto(&effects.position_y_keys),
        scale_x_keys: keys_to_dto(&effects.scale_x_keys),
        scale_y_keys: keys_to_dto(&effects.scale_y_keys),
        rotation_keys: keys_to_dto(&effects.rotation_keys),
        crop_left_keys: keys_to_dto(&effects.crop_left_keys),
        crop_top_keys: keys_to_dto(&effects.crop_top_keys),
        crop_right_keys: keys_to_dto(&effects.crop_right_keys),
        crop_bottom_keys: keys_to_dto(&effects.crop_bottom_keys),
    }
}

fn math_to_dto(math: &MathClipData) -> MathDto {
    MathDto {
        syntax: math.syntax.clone(),
        source: math.source.clone(),
        font_size: math.font_size,
        color: math.color.clone(),
        background_color: math.background_color.clone(),
    }
}

fn graph_to_dto(graph: &GraphClipData) -> GraphDto {
    GraphDto {
        viewport: ViewportDto {
            x_min: graph.viewport.x_min,
            x_max: graph.viewport.x_max,
            y_min: graph.viewport.y_min,
            y_max: graph.viewport.y_max,
        },
        axes: AxesDto {
            show_axes: graph.axes.show_axes,
            show_grid: graph.axes.show_grid,
            x_label: graph.axes.x_label.clone(),
            y_label: graph.axes.y_label.clone(),
        },
        functions: graph
            .functions
            .iter()
            .map(|function| GraphFunctionDto {
                id: function.id.0.clone(),
                expression: function.expression.clone(),
                label: function.label.clone(),
                color: canonical_argb_color(&function.color)
                    .unwrap_or_else(|| function.color.clone()),
                stroke_width: function.stroke_width,
                domain_min: function.domain_min,
                domain_max: function.domain_max,
            })
            .collect(),
        intro: GraphIntroDto {
            kind: graph.intro.kind.name().to_owned(),
            frames: graph.intro.frames,
        },
    }
}

fn sequence_to_dto(sequence: &EquationSequenceClipData) -> EquationSequenceDto {
    EquationSequenceDto {
        states: sequence
            .states
            .iter()
            .map(|state| EquationStateDto {
                id: state.id.0.clone(),
                equation: math_to_dto(&state.equation),
                revision: state.revision.clone(),
                hold_frames: state.hold_frames,
                parts: state
                    .parts
                    .iter()
                    .map(|part| EquationPartDto {
                        id: part.id.0.clone(),
                        label: part.label.clone(),
                        binding: EquationBindingDto {
                            revision: part.binding.revision.clone(),
                            begin: part.binding.begin,
                            end: part.binding.end,
                            expected_text: part.binding.expected_text.clone(),
                            status: binding_status_name(part.binding.status).to_owned(),
                        },
                    })
                    .collect(),
            })
            .collect(),
        transitions: sequence
            .transitions
            .iter()
            .map(|edge| EquationTransitionDto {
                id: edge.id.0.clone(),
                from: edge.from.0.clone(),
                to: edge.to.0.clone(),
                frames: edge.frames,
                correspondence: edge
                    .correspondence
                    .iter()
                    .map(|pair| PartPairDto {
                        from: pair.from.0.clone(),
                        to: pair.to.0.clone(),
                    })
                    .collect(),
            })
            .collect(),
        actions: sequence
            .actions
            .iter()
            .map(|action| EquationActionDto {
                id: action.id.0.clone(),
                state: action.state.0.clone(),
                target: action.target.0.clone(),
                target_status: target_status_name(action.target_status).to_owned(),
                start: action.start,
                duration: action.duration,
                operation: operation_name(action.operation).to_owned(),
            })
            .collect(),
    }
}

fn clip_to_dto(clip: &TimelineClip, directory: &Path) -> ClipDto {
    ClipDto {
        kind: clip.kind.name().to_owned(),
        media_path: if clip.kind.has_media_path() {
            persisted_source_path(&clip.media_path, directory)
        } else {
            String::new()
        },
        name: clip.name.clone(),
        id: clip.id.clone(),
        source_fps_num: clip.source_fps.num,
        source_fps_den: clip.source_fps.den,
        source_frame_count: clip.source_frame_count,
        source_in_frame: clip.source_in_frame,
        source_out_frame: clip.source_out_frame,
        timeline_start_frame: clip.timeline_start_frame,
        speed_num: clip.speed.num,
        speed_den: clip.speed.den,
        preserve_pitch: clip.preserve_pitch,
        enabled: clip.enabled,
        frame_hold: clip.frame_hold.as_ref().map(|hold| FrameHoldDto {
            source_frame: hold.source_frame,
            source_fps_num: hold.source_fps.num,
            source_fps_den: hold.source_fps.den,
            source_frame_count: hold.source_frame_count,
            speed_num: hold.speed.num,
            speed_den: hold.speed.den,
        }),
        track_kind: clip.track.kind.name().to_owned(),
        track_index: clip.track.index as i64,
        media_item_id: clip.media_item_id.clone(),
        link_group_id: clip.link_group_id.clone(),
        effects: effects_to_dto(&clip.effects),
        text: (clip.kind == TimelineClipKind::Text).then(|| TextDto {
            content: clip.text.content.clone(),
            font_family: clip.text.font_family.clone(),
            font_size: clip.text.font_size,
            x: clip.text.x,
            y: clip.text.y,
            color: clip.text.color.clone(),
            bold: clip.text.bold,
            alignment: clip.text.alignment.clone(),
            outline_color: clip.text.outline_color.clone(),
            outline_width: clip.text.outline_width,
            background_color: clip.text.background_color.clone(),
        }),
        math: (clip.kind == TimelineClipKind::Math).then(|| math_to_dto(&clip.math)),
        math_animation: (clip.kind == TimelineClipKind::Math
            && clip.math_animation != MathClipAnimation::default())
        .then(|| MathAnimationDto {
            intro: clip.math_animation.intro.name().to_owned(),
            intro_frames: clip.math_animation.intro_frames,
        }),
        graph: (clip.kind == TimelineClipKind::Graph).then(|| graph_to_dto(&clip.graph)),
        equation_sequence: (clip.kind == TimelineClipKind::EquationSequence)
            .then(|| sequence_to_dto(&clip.equation_sequence)),
        unknown: BTreeMap::new(),
    }
}

fn manim_asset_to_dto(asset: &ManimAsset, directory: &Path) -> ProjectResult<ManimAssetDto> {
    if asset.script_path.as_os_str().is_empty() {
        return fail("Manim script_path が空です");
    }
    let script_path = persisted_source_path(&asset.script_path, directory);
    let generated_video_path = if asset.generated_video_path.as_os_str().is_empty() {
        String::new()
    } else {
        let absolute = lexically_normal(&if asset.generated_video_path.is_absolute() {
            asset.generated_video_path.clone()
        } else {
            directory.join(&asset.generated_video_path)
        });
        match lexically_relative(&absolute, directory)
            .filter(|relative| is_project_managed_relative_path(relative))
        {
            Some(relative) => generic_string(&relative),
            None => {
                return fail(format!(
                    "generated_video_path は project directory 配下である必要があります: {}",
                    generic_string(&absolute)
                ));
            }
        }
    };
    let generated = matches!(
        asset.generation_state,
        ManimGenerationState::Ready | ManimGenerationState::SourceChanged
    );
    if asset.scene_name.is_empty()
        || (generated && (generated_video_path.is_empty() || !is_sha256(&asset.source_fingerprint)))
    {
        return fail("Manim asset の必須値が不正です");
    }
    Ok(ManimAssetDto {
        script_path,
        scene_name: asset.scene_name.clone(),
        generated_video_path,
        generation_state: asset.generation_state.name().to_owned(),
        source_fingerprint: asset.source_fingerprint.clone(),
    })
}

fn track_to_dto(track: &Track) -> TrackDto {
    TrackDto {
        name: track.name.clone(),
        muted: track.muted,
        solo: track.solo,
        mixer_name: track.mixer_name.clone(),
        mixer_gain_db: track.mixer_gain_db,
        mixer_pan: track.mixer_pan,
    }
}

/// Project を schema 22 の JSON にする。`project_path` は保存先（相対 path の基準になる directory を決める）。
/// timeline と素材の検証に通らない Project は書かない。
pub fn serialize_project_json(project: &Project, project_path: &Path) -> ProjectResult<String> {
    validate_timeline(project)?;
    validate_media_bin(project)?;
    let directory = project_directory(project_path)?;
    let subtitles = project.subtitles.as_ref().map(|track| SubtitlesDto {
        visible: track.visible,
        style: SubtitleStyleDto {
            font_family: track.style.font_family.clone(),
            font_size: track.style.font_size,
            bold: track.style.bold,
            color: track.style.color.clone(),
            outline_color: track.style.outline_color.clone(),
            outline_width: track.style.outline_width,
            background_color: track.style.background_color.clone(),
            alignment: track.style.alignment.clone(),
            side_margin: track.style.side_margin,
            bottom_margin: track.style.bottom_margin,
        },
        cues: track
            .cues
            .iter()
            .map(|cue| CueDto {
                id: cue.id.clone(),
                start_frame: cue.start_frame,
                end_frame: cue.end_frame,
                content: cue.content.clone(),
                link_clip_id: (!cue.link_clip_id.is_empty()).then(|| cue.link_clip_id.clone()),
            })
            .collect(),
    });
    let dto = ProjectDto {
        schema_version: PROJECT_SCHEMA_VERSION,
        format: FORMAT_MARKER.to_owned(),
        timeline_fps_num: project.timeline_fps.num,
        timeline_fps_den: project.timeline_fps.den,
        output_width: project.output_width,
        output_height: project.output_height,
        timeline_markers: project.timeline_markers.clone(),
        in_frame: project.in_frame,
        out_frame: project.out_frame,
        subtitles,
        video_tracks: project.video_tracks.iter().map(track_to_dto).collect(),
        audio_tracks: project.audio_tracks.iter().map(track_to_dto).collect(),
        manim_assets: project
            .manim_assets
            .iter()
            .map(|asset| manim_asset_to_dto(asset, &directory))
            .collect::<ProjectResult<_>>()?,
        timeline_clips: project
            .timeline_clips
            .iter()
            .map(|clip| clip_to_dto(clip, &directory))
            .collect(),
        timeline_transitions: project
            .timeline_transitions
            .iter()
            .map(|transition| TransitionDto {
                id: transition.id.clone(),
                outgoing_clip_id: transition.outgoing_clip_id.clone(),
                incoming_clip_id: transition.incoming_clip_id.clone(),
                frames_before_cut: transition.frames_before_cut,
                frames_after_cut: transition.frames_after_cut,
                kind: transition.kind.name().to_owned(),
            })
            .collect(),
        media_folders: project
            .media_folders
            .iter()
            .map(|folder| MediaFolderDto {
                id: folder.id.clone(),
                name: folder.name.clone(),
                parent_id: folder.parent_id.clone(),
            })
            .collect(),
        media_items: project
            .media_items
            .iter()
            .map(|item| MediaItemDto {
                id: item.id.clone(),
                kind: item.kind.name().to_owned(),
                media_path: persisted_source_path(&item.media_path, &directory),
                name: item.name.clone(),
                folder_id: item.folder_id.clone(),
                fps_num: item.fps.num,
                fps_den: item.fps.den,
                frame_count: item.frame_count,
                width: item.width,
                height: item.height,
                sample_rate: item.sample_rate,
                duration_samples: item.duration_samples,
            })
            .collect(),
        last_audio_adjustment_settings: project.last_audio_adjustment_settings.clone(),
    };
    let mut json = serde_json::to_string_pretty(&dto)
        .map_err(|error| ProjectError::new(format!("Project JSON を作れません: {error}")))?;
    json.push('\n');
    Ok(json)
}

// --- DTO → Project ---

fn keys_from_dto(keys: Vec<KeyDto>) -> ProjectResult<Vec<ClipKeyframe>> {
    keys.into_iter()
        .map(|key| {
            let interpolation = parse_interpolation(&key.interpolation)?;
            if interpolation == KeyInterpolation::Spline
                && (key.control1.is_none() || key.control2.is_none())
            {
                return fail("スプラインには両方の制御値が必要です");
            }
            let defaults = ClipKeyframe::default();
            Ok(ClipKeyframe {
                frame: key.frame,
                value: key.value,
                interpolation,
                curve_start: key.curve_start,
                curve_end: key.curve_end,
                control1: key.control1.unwrap_or(defaults.control1),
                control2: key.control2.unwrap_or(defaults.control2),
            })
        })
        .collect()
}

fn effects_from_dto(dto: EffectsDto) -> ProjectResult<ClipEffects> {
    Ok(ClipEffects {
        position_x_percent: dto.position_x_percent,
        position_y_percent: dto.position_y_percent,
        scale_x_percent: dto.scale_x_percent,
        scale_y_percent: dto.scale_y_percent,
        rotation_degrees: dto.rotation_degrees,
        opacity_percent: dto.opacity_percent,
        volume_percent: dto.volume_percent,
        normalization_gain_db: dto.normalization_gain_db,
        ducking_db: dto.ducking_db,
        ducking_keys: keys_from_dto(dto.ducking_keys)?,
        audio_adjustment_settings: dto.audio_adjustment_settings,
        audio_adjustment_fingerprint: dto.audio_adjustment_fingerprint,
        opacity_keys: keys_from_dto(dto.opacity_keys)?,
        volume_keys: keys_from_dto(dto.volume_keys)?,
        position_x_keys: keys_from_dto(dto.position_x_keys)?,
        position_y_keys: keys_from_dto(dto.position_y_keys)?,
        scale_x_keys: keys_from_dto(dto.scale_x_keys)?,
        scale_y_keys: keys_from_dto(dto.scale_y_keys)?,
        rotation_keys: keys_from_dto(dto.rotation_keys)?,
        crop_left_keys: keys_from_dto(dto.crop_left_keys)?,
        crop_top_keys: keys_from_dto(dto.crop_top_keys)?,
        crop_right_keys: keys_from_dto(dto.crop_right_keys)?,
        crop_bottom_keys: keys_from_dto(dto.crop_bottom_keys)?,
        crop_left_percent: dto.crop_left_percent,
        crop_top_percent: dto.crop_top_percent,
        crop_right_percent: dto.crop_right_percent,
        crop_bottom_percent: dto.crop_bottom_percent,
        fade_in_frames: dto.fade_in_frames,
        fade_out_frames: dto.fade_out_frames,
    })
}

fn math_from_dto(dto: MathDto) -> MathClipData {
    MathClipData {
        syntax: dto.syntax,
        source: dto.source,
        font_size: dto.font_size,
        color: dto.color,
        background_color: dto.background_color,
    }
}

fn graph_from_dto(dto: GraphDto) -> ProjectResult<GraphClipData> {
    let Some(kind) = GraphIntroKind::from_name(&dto.intro.kind) else {
        return fail("Graph の intro の種類が不正です");
    };
    Ok(GraphClipData {
        viewport: GraphViewport {
            x_min: dto.viewport.x_min,
            x_max: dto.viewport.x_max,
            y_min: dto.viewport.y_min,
            y_max: dto.viewport.y_max,
        },
        axes: GraphAxes {
            show_axes: dto.axes.show_axes,
            show_grid: dto.axes.show_grid,
            x_label: dto.axes.x_label,
            y_label: dto.axes.y_label,
        },
        functions: dto
            .functions
            .into_iter()
            .map(|function| GraphFunction {
                id: GraphFunctionId(function.id),
                expression: function.expression,
                domain_min: function.domain_min,
                domain_max: function.domain_max,
                label: function.label,
                color: function.color,
                stroke_width: function.stroke_width,
            })
            .collect(),
        intro: GraphIntro {
            kind,
            frames: dto.intro.frames,
        },
    })
}

fn sequence_from_dto(dto: EquationSequenceDto) -> ProjectResult<EquationSequenceClipData> {
    let states = dto
        .states
        .into_iter()
        .map(|state| {
            let parts = state
                .parts
                .into_iter()
                .map(|part| {
                    let status = match part.binding.status.as_str() {
                        "bound" => BindingStatus::Bound,
                        "invalid" => BindingStatus::Invalid,
                        other => return fail(format!("未知の binding 状態です: {other}")),
                    };
                    Ok(SemanticPart {
                        id: PartId(part.id),
                        label: part.label,
                        binding: SourceBinding {
                            revision: part.binding.revision,
                            begin: part.binding.begin,
                            end: part.binding.end,
                            expected_text: part.binding.expected_text,
                            status,
                        },
                    })
                })
                .collect::<ProjectResult<_>>()?;
            Ok(EquationState {
                id: StateId(state.id),
                equation: math_from_dto(state.equation),
                revision: state.revision,
                hold_frames: state.hold_frames,
                parts,
            })
        })
        .collect::<ProjectResult<_>>()?;
    let transitions = dto
        .transitions
        .into_iter()
        .map(|edge| EquationStepTransition {
            id: TransitionId(edge.id),
            from: StateId(edge.from),
            to: StateId(edge.to),
            frames: edge.frames,
            correspondence: edge
                .correspondence
                .into_iter()
                .map(|pair| PartPair {
                    from: PartId(pair.from),
                    to: PartId(pair.to),
                })
                .collect(),
        })
        .collect();
    let actions = dto
        .actions
        .into_iter()
        .map(|action| {
            let target_status = match action.target_status.as_str() {
                "present" => EquationTargetStatus::Present,
                "missing" => EquationTargetStatus::Missing,
                other => return fail(format!("未知の action 対象状態です: {other}")),
            };
            let operation = match action.operation.as_str() {
                "outline" => EquationOperation::Outline,
                "pulse" => EquationOperation::Pulse,
                other => return fail(format!("未知の数式 operation です: {other}")),
            };
            Ok(EquationAction {
                id: ActionId(action.id),
                state: StateId(action.state),
                target: PartId(action.target),
                target_status,
                start: action.start,
                duration: action.duration,
                operation,
            })
        })
        .collect::<ProjectResult<_>>()?;
    Ok(EquationSequenceClipData {
        states,
        transitions,
        actions,
    })
}

fn clip_from_dto(dto: ClipDto, directory: &Path) -> ProjectResult<TimelineClip> {
    let Some(kind) = TimelineClipKind::from_name(&dto.kind) else {
        return fail(format!("未知の timeline clip kind です: {}", dto.kind));
    };
    let present = |has: bool, expected: TimelineClipKind, name: &str| -> ProjectResult<()> {
        if has != (kind == expected) {
            return fail(format!("timeline clip の {name} と kind が一致しません"));
        }
        Ok(())
    };
    present(dto.text.is_some(), TimelineClipKind::Text, "text")?;
    present(dto.math.is_some(), TimelineClipKind::Math, "math")?;
    present(
        dto.equation_sequence.is_some(),
        TimelineClipKind::EquationSequence,
        "equation_sequence",
    )?;
    present(dto.graph.is_some(), TimelineClipKind::Graph, "graph")?;
    if matches!(
        kind,
        TimelineClipKind::EquationSequence | TimelineClipKind::Graph
    ) && let Some(field) = dto.unknown.keys().next()
    {
        return fail(format!(
            "EquationSequence clip に未知の field があります: {field}"
        ));
    }
    if dto.math_animation.is_some() && kind != TimelineClipKind::Math {
        return fail("数式 clip 以外に math_animation は指定できません");
    }
    if dto.media_path.is_empty() == kind.has_media_path() {
        return fail(if kind.has_media_path() {
            "timeline clip の media_path が空です".to_owned()
        } else {
            format!("{} clip に media_path は指定できません", dto.kind)
        });
    }
    if dto.name.is_empty() {
        return fail("timeline clip の name が空です");
    }
    let Some(track_kind) = TrackKind::from_name(&dto.track_kind) else {
        return fail(format!("未知の track kind です: {}", dto.track_kind));
    };
    let Some(track_index) = usize::try_from(dto.track_index)
        .ok()
        .filter(|index| *index <= i32::MAX as usize)
    else {
        return fail("timeline clip の track_index が重複または不正です");
    };
    let math_animation = match dto.math_animation {
        None => MathClipAnimation::default(),
        Some(animation) => {
            let Some(intro) = MathIntroKind::from_name(&animation.intro) else {
                return fail(format!(
                    "math_animation の intro が不正です: {}",
                    animation.intro
                ));
            };
            MathClipAnimation {
                intro,
                intro_frames: animation.intro_frames,
            }
        }
    };
    Ok(TimelineClip {
        kind,
        media_path: if kind.has_media_path() {
            resolve_source_path(&dto.media_path, directory)
        } else {
            PathBuf::new()
        },
        name: dto.name,
        id: dto.id,
        media_item_id: dto.media_item_id,
        source_fps: Rational::new(dto.source_fps_num, dto.source_fps_den),
        source_frame_count: dto.source_frame_count,
        source_in_frame: dto.source_in_frame,
        source_out_frame: dto.source_out_frame,
        timeline_start_frame: dto.timeline_start_frame,
        effects: effects_from_dto(dto.effects)?,
        track: TrackRef::new(track_kind, track_index),
        link_group_id: dto.link_group_id,
        speed: Rational::new(dto.speed_num, dto.speed_den),
        preserve_pitch: dto.preserve_pitch,
        frame_hold: dto.frame_hold.map(|hold| FrameHold {
            source_frame: hold.source_frame,
            source_fps: Rational::new(hold.source_fps_num, hold.source_fps_den),
            source_frame_count: hold.source_frame_count,
            speed: Rational::new(hold.speed_num, hold.speed_den),
        }),
        text: dto
            .text
            .map_or_else(TextClipData::default, |text| TextClipData {
                content: text.content,
                font_family: text.font_family,
                font_size: text.font_size,
                x: text.x,
                y: text.y,
                color: text.color,
                bold: text.bold,
                alignment: text.alignment,
                outline_color: text.outline_color,
                outline_width: text.outline_width,
                background_color: text.background_color,
            }),
        math: dto.math.map_or_else(MathClipData::default, math_from_dto),
        math_animation,
        equation_sequence: dto
            .equation_sequence
            .map(sequence_from_dto)
            .transpose()?
            .unwrap_or_default(),
        graph: dto
            .graph
            .map(graph_from_dto)
            .transpose()?
            .unwrap_or_default(),
        enabled: dto.enabled,
    })
}

fn manim_asset_from_dto(dto: ManimAssetDto, directory: &Path) -> ProjectResult<ManimAsset> {
    if dto.script_path.is_empty() || dto.scene_name.is_empty() {
        return fail("Manim asset の script_path または scene_name が空です");
    }
    let Some(generation_state) = ManimGenerationState::from_name(&dto.generation_state) else {
        return fail(format!(
            "未知の Manim generation_state です: {}",
            dto.generation_state
        ));
    };
    let generated_video_path = if dto.generated_video_path.is_empty() {
        PathBuf::new()
    } else {
        let relative = PathBuf::from(&dto.generated_video_path);
        if !is_project_managed_relative_path(&relative) {
            return fail("generated_video_path が project-relative ではありません");
        }
        lexically_normal(&directory.join(relative))
    };
    let generated = matches!(
        generation_state,
        ManimGenerationState::Ready | ManimGenerationState::SourceChanged
    );
    if generated
        && (generated_video_path.as_os_str().is_empty() || !is_sha256(&dto.source_fingerprint))
    {
        return fail("生成済み Manim asset の video path または fingerprint が不正です");
    }
    Ok(ManimAsset {
        script_path: resolve_source_path(&dto.script_path, directory),
        scene_name: dto.scene_name,
        generated_video_path,
        generation_state,
        source_fingerprint: dto.source_fingerprint,
    })
}

fn track_from_dto(dto: TrackDto, kind: TrackKind) -> ProjectResult<Track> {
    if kind == TrackKind::Video && dto.solo {
        return fail("video track は solo を持てません");
    }
    if dto.name.is_empty() {
        return fail("track の name が空です");
    }
    Ok(Track {
        name: dto.name,
        muted: dto.muted,
        solo: dto.solo,
        mixer_name: dto.mixer_name,
        mixer_gain_db: dto.mixer_gain_db,
        mixer_pan: dto.mixer_pan,
    })
}

fn subtitles_from_dto(dto: SubtitlesDto) -> ProjectResult<SubtitleTrack> {
    let cues = dto
        .cues
        .into_iter()
        .map(|cue| {
            if cue.link_clip_id.as_deref() == Some("") {
                return fail("字幕行のリンク先が不正です");
            }
            Ok(SubtitleCue {
                id: cue.id,
                start_frame: cue.start_frame,
                end_frame: cue.end_frame,
                content: cue.content,
                link_clip_id: cue.link_clip_id.unwrap_or_default(),
            })
        })
        .collect::<ProjectResult<_>>()?;
    let style = dto.style;
    Ok(SubtitleTrack {
        visible: dto.visible,
        style: SubtitleStyle {
            font_family: style.font_family,
            font_size: style.font_size,
            bold: style.bold,
            color: style.color,
            outline_color: style.outline_color,
            outline_width: style.outline_width,
            background_color: style.background_color,
            alignment: style.alignment,
            side_margin: style.side_margin,
            bottom_margin: style.bottom_margin,
        },
        cues,
    })
}

/// 版を先に読む。版が違うファイルは後続の field の形も違うので、原因の分からない「必須 field がありません」
/// ではなく版の違いとして断る。
fn check_header(text: &str) -> ProjectResult<()> {
    #[derive(Deserialize)]
    struct Header {
        schema_version: Option<serde_json::Value>,
        format: Option<serde_json::Value>,
    }
    let header: Header = serde_json::from_str(text)
        .map_err(|error| ProjectError::new(format!("Project JSON を解析できません: {error}")))?;
    let Some(version) = header.schema_version else {
        return fail("schema_version がありません");
    };
    if version.as_i64() != Some(i64::from(PROJECT_SCHEMA_VERSION)) {
        return fail(format!(
            "対応していない schema_version です: {version}。schema {PROJECT_SCHEMA_VERSION} の .mvm を開いてください"
        ));
    }
    if header.format.as_ref().and_then(serde_json::Value::as_str) != Some(FORMAT_MARKER) {
        return fail("mvm project ファイルではありません (format marker 不一致)");
    }
    Ok(())
}

/// schema 22 の JSON を Project にする。`project_path` はファイルの場所（相対 path の基準）。
/// 読み込んだ Project は timeline と素材の検証を通っている。
pub fn parse_project_json(text: &str, project_path: &Path) -> ProjectResult<Project> {
    let directory = project_directory(project_path)?;
    reject_duplicate_keys(text)?;
    check_header(text)?;
    let dto: ProjectDto = serde_json::from_str(text).map_err(|error| {
        ProjectError::new(format!(
            "Project schema {PROJECT_SCHEMA_VERSION} の JSON として読めません: {error}"
        ))
    })?;
    let project = Project {
        timeline_fps: Rational::new(dto.timeline_fps_num, dto.timeline_fps_den),
        output_width: dto.output_width,
        output_height: dto.output_height,
        video_tracks: dto
            .video_tracks
            .into_iter()
            .map(|track| track_from_dto(track, TrackKind::Video))
            .collect::<ProjectResult<_>>()?,
        audio_tracks: dto
            .audio_tracks
            .into_iter()
            .map(|track| track_from_dto(track, TrackKind::Audio))
            .collect::<ProjectResult<_>>()?,
        manim_assets: dto
            .manim_assets
            .into_iter()
            .map(|asset| manim_asset_from_dto(asset, &directory))
            .collect::<ProjectResult<_>>()?,
        timeline_clips: dto
            .timeline_clips
            .into_iter()
            .map(|clip| clip_from_dto(clip, &directory))
            .collect::<ProjectResult<_>>()?,
        timeline_transitions: dto
            .timeline_transitions
            .into_iter()
            .map(|transition| {
                let Some(kind) = TransitionKind::from_name(&transition.kind) else {
                    return fail("timeline transition の kind が重複または不正です");
                };
                Ok(TimelineTransition {
                    id: transition.id,
                    outgoing_clip_id: transition.outgoing_clip_id,
                    incoming_clip_id: transition.incoming_clip_id,
                    frames_before_cut: transition.frames_before_cut,
                    frames_after_cut: transition.frames_after_cut,
                    kind,
                })
            })
            .collect::<ProjectResult<_>>()?,
        timeline_markers: dto.timeline_markers,
        in_frame: dto.in_frame,
        out_frame: dto.out_frame,
        media_folders: dto
            .media_folders
            .into_iter()
            .map(|folder| MediaFolder {
                id: folder.id,
                name: folder.name,
                parent_id: folder.parent_id,
            })
            .collect(),
        media_items: dto
            .media_items
            .into_iter()
            .map(|item| {
                if item.media_path.is_empty() {
                    return fail("media item の media_path が空です");
                }
                let Some(kind) = MediaKind::from_name(&item.kind) else {
                    return fail(format!("未知の media item kind です: {}", item.kind));
                };
                Ok(MediaItem {
                    id: item.id,
                    kind,
                    media_path: resolve_source_path(&item.media_path, &directory),
                    name: item.name,
                    folder_id: item.folder_id,
                    fps: Rational::new(item.fps_num, item.fps_den),
                    frame_count: item.frame_count,
                    width: item.width,
                    height: item.height,
                    sample_rate: item.sample_rate,
                    duration_samples: item.duration_samples,
                })
            })
            .collect::<ProjectResult<_>>()?,
        subtitles: dto.subtitles.map(subtitles_from_dto).transpose()?,
        last_audio_adjustment_settings: dto.last_audio_adjustment_settings,
    };
    validate_timeline(&project)?;
    validate_media_bin(&project)?;
    Ok(project)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_paths_follow_cpp_rules() {
        let directory = Path::new(r"C:\work\project");
        assert_eq!(
            persisted_source_path(Path::new(r"C:\work\project\media\v.mp4"), directory),
            "media/v.mp4"
        );
        assert_eq!(
            persisted_source_path(Path::new(r"C:\work\shared\a.wav"), directory),
            "../shared/a.wav"
        );
        assert_eq!(
            persisted_source_path(Path::new(r"D:\other\a.wav"), directory),
            "D:/other/a.wav"
        );
        assert_eq!(
            resolve_source_path("../shared/a.wav", directory),
            PathBuf::from(r"C:\work\shared\a.wav")
        );
        assert!(is_project_managed_relative_path(Path::new(
            "cache/manim/a.mp4"
        )));
        assert!(!is_project_managed_relative_path(Path::new("../a.mp4")));
        assert!(!is_project_managed_relative_path(Path::new(".")));
        assert!(!is_project_managed_relative_path(Path::new(r"C:\a.mp4")));
    }

    #[test]
    fn duplicate_keys_are_rejected_anywhere() {
        assert!(reject_duplicate_keys(r#"{"a": 1, "b": {"c": 1, "d": [ {"e": 1} ]}}"#).is_ok());
        assert!(reject_duplicate_keys(r#"{"a": 1, "a": 2}"#).is_err());
        assert!(reject_duplicate_keys(r#"{"b": [{"e": 1, "e": 1}]}"#).is_err());
        assert!(
            reject_duplicate_keys(r#"{"a": 1} x"#).is_err(),
            "末尾の余分な値"
        );
    }
}
