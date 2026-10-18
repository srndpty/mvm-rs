//! Project のデータ構造と、構造だけで決まる小さな判定。

use std::path::PathBuf;

use mvm_core::Rational;
use mvm_core::output_timebase::{is_configurable_output_frame_rate, is_measured_output_frame_rate};

use crate::clip_effects::ClipEffects;
use crate::equation_sequence::EquationSequenceClipData;
use crate::error::{ProjectResult, fail};
use crate::graph_clip::GraphClipData;
use crate::math_clip::{MathClipAnimation, MathClipData};

/// Project JSON の schema。timeline の検証と JSON の読み書きが同じ値を参照する。
pub const PROJECT_SCHEMA_VERSION: i32 = 22;

/// 速度の範囲（10%〜1000%）。
pub const MIN_CLIP_SPEED_PERCENT: i64 = 10;
pub const MAX_CLIP_SPEED_PERCENT: i64 = 1000;

pub const MAXIMUM_PROJECT_OUTPUT_DIMENSION: i32 = 16384;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ManimGenerationState {
    NotGenerated,
    Ready,
    SourceChanged,
    GenerationFailed,
}

impl ManimGenerationState {
    pub const ALL: [Self; 4] = [
        Self::NotGenerated,
        Self::Ready,
        Self::SourceChanged,
        Self::GenerationFailed,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Self::NotGenerated => "NotGenerated",
            Self::Ready => "Ready",
            Self::SourceChanged => "SourceChanged",
            Self::GenerationFailed => "GenerationFailed",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|state| state.name() == name)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ManimAsset {
    pub script_path: PathBuf,
    pub scene_name: String,
    pub generated_video_path: PathBuf,
    pub generation_state: ManimGenerationState,
    pub source_fingerprint: String,
}

impl ManimAsset {
    /// 生成済みの asset を作る。fingerprint は小文字 64 桁の SHA-256。
    pub fn ready(
        script_path: PathBuf,
        scene_name: String,
        generated_video_path: PathBuf,
        source_fingerprint: String,
    ) -> ProjectResult<Self> {
        if script_path.as_os_str().is_empty() {
            return fail("Manim script path が空です");
        }
        if scene_name.is_empty() {
            return fail("Manim Scene 名が空です");
        }
        if generated_video_path.as_os_str().is_empty() {
            return fail("生成済み Manim video path が空です");
        }
        if !is_sha256(&source_fingerprint) {
            return fail("Manim source fingerprint が小文字64桁の SHA-256 ではありません");
        }
        Ok(Self {
            script_path,
            scene_name,
            generated_video_path,
            generation_state: ManimGenerationState::Ready,
            source_fingerprint,
        })
    }

    /// Ready / SourceChanged の asset を現在の fingerprint で再評価する。
    /// NotGenerated / GenerationFailed は render 操作まで今の状態を保つ。
    pub fn refresh_generation_state(&mut self, current_fingerprint: &str) -> ProjectResult<()> {
        if matches!(
            self.generation_state,
            ManimGenerationState::NotGenerated | ManimGenerationState::GenerationFailed
        ) {
            return Ok(());
        }
        if !is_sha256(&self.source_fingerprint) || !is_sha256(current_fingerprint) {
            return fail("比較する Manim source fingerprint が正しい SHA-256 ではありません");
        }
        self.generation_state = if self.source_fingerprint == current_fingerprint {
            ManimGenerationState::Ready
        } else {
            ManimGenerationState::SourceChanged
        };
        Ok(())
    }
}

pub fn is_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TimelineClipKind {
    Video,
    Manim,
    Audio,
    Text,
    Image,
    Math,
    EquationSequence,
    Graph,
}

impl TimelineClipKind {
    pub const ALL: [Self; 8] = [
        Self::Video,
        Self::Manim,
        Self::Audio,
        Self::Text,
        Self::Image,
        Self::Math,
        Self::EquationSequence,
        Self::Graph,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Self::Video => "video",
            Self::Manim => "manim",
            Self::Audio => "audio",
            Self::Text => "text",
            Self::Image => "image",
            Self::Math => "math",
            Self::EquationSequence => "equation_sequence",
            Self::Graph => "graph",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.name() == name)
    }

    /// 素材の時間軸を持たない clip（文字・静止画・数式）。尺は timeline 上で自由に伸縮し、
    /// 素材 frame domain は in = 0・out = 尺 の合成値にする（fps は置いたときの timeline の値）。
    /// 速度・リンク・スリップ・レート調整を持たない。
    pub fn is_still(self) -> bool {
        matches!(self, Self::Text | Self::Image | Self::Math)
    }

    /// プロジェクトパネルの素材から作る clip か（動画・音声・画像）。これらは media_item_id が必須。
    pub fn uses_media_item(self) -> bool {
        matches!(self, Self::Video | Self::Audio | Self::Image)
    }

    /// media_path（実ファイル）を持つ clip か。文字・数式・グラフは Project のデータだけから描く。
    pub fn has_media_path(self) -> bool {
        !matches!(
            self,
            Self::Text | Self::Math | Self::EquationSequence | Self::Graph
        )
    }

    /// この kind の clip がその track に載ってよいか。audio clip を video track へ置かせない。
    pub fn fits_track(self, track: TrackKind) -> bool {
        (self == Self::Audio) == (track == TrackKind::Audio)
    }
}

/// "#AARRGGBB"（16 進 8 桁）を 0xAARRGGBB にする。文字・数式の色の形式はこれだけで判定する。
pub fn parse_argb_color(text: &str) -> Option<u32> {
    let digits = text.strip_prefix('#')?;
    if digits.len() != 8 || !digits.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    u32::from_str_radix(digits, 16).ok()
}

/// 色を大文字の "#AARRGGBB" に揃える。不正な色は `None`。
pub fn canonical_argb_color(text: &str) -> Option<String> {
    parse_argb_color(text).map(|value| format!("#{value:08X}"))
}

fn rounded_clip_frames(seconds: i64, fps: Rational) -> i64 {
    if !fps.is_positive() {
        return 1;
    }
    let frames =
        (i128::from(seconds) * i128::from(fps.num) + i128::from(fps.den / 2)) / i128::from(fps.den);
    i64::try_from(frames).unwrap_or(i64::MAX).max(1)
}

/// 文字・静止画を置いたときの既定の尺（5 秒、最低 1 frame）。
pub fn default_still_clip_frames(timeline_fps: Rational) -> i64 {
    rounded_clip_frames(5, timeline_fps)
}

/// フレーム保持を挿入したときの既定の尺（2 秒、最低 1 frame）。
pub fn default_frame_hold_frames(timeline_fps: Rational) -> i64 {
    rounded_clip_frames(2, timeline_fps)
}

#[derive(Clone, Debug, PartialEq)]
pub struct TextClipData {
    pub content: String,
    pub font_family: String,
    pub font_size: i32,
    pub x: i32,
    pub y: i32,
    pub color: String,
    pub bold: bool,
    pub alignment: String,
    pub outline_color: String,
    pub outline_width: i32,
    pub background_color: String,
}

impl Default for TextClipData {
    fn default() -> Self {
        Self {
            content: String::new(),
            font_family: "Meiryo".to_owned(),
            font_size: 64,
            x: 0,
            y: 0,
            color: "#FFFFFFFF".to_owned(),
            bold: false,
            alignment: "left".to_owned(),
            outline_color: "#FF000000".to_owned(),
            outline_width: 0,
            background_color: "#00000000".to_owned(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TrackKind {
    Video,
    Audio,
}

impl TrackKind {
    pub fn name(self) -> &'static str {
        match self {
            Self::Video => "video",
            Self::Audio => "audio",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        [Self::Video, Self::Audio]
            .into_iter()
            .find(|kind| kind.name() == name)
    }

    /// UI 表示名の既定値（"V1" / "A1" など）。
    pub fn default_track_name(self, index: usize) -> String {
        let prefix = match self {
            Self::Video => "V",
            Self::Audio => "A",
        };
        format!("{prefix}{}", index + 1)
    }
}

/// track は video / audio それぞれ独立した Vec で持つ。片方へ track を足しても、
/// もう片方の clip の index を振り直さずに済む。
#[derive(Clone, Debug, PartialEq)]
pub struct Track {
    /// UI 表示名。"V1" / "A1" など。
    pub name: String,
    /// video では非表示（layer から外す）、audio では消音。
    pub muted: bool,
    /// audio だけが持つ。どれか 1 つでも solo なら、solo の track だけが鳴る。video では常に false。
    pub solo: bool,
    pub mixer_name: String,
    /// -96 は無音。上限は +15 dB。
    pub mixer_gain_db: f64,
    /// -1（左）〜 +1（右）、中央は減衰なし。
    pub mixer_pan: f64,
}

impl Track {
    pub fn named(name: String) -> Self {
        Self {
            name,
            muted: false,
            solo: false,
            mixer_name: String::new(),
            mixer_gain_db: 0.0,
            mixer_pan: 0.0,
        }
    }
}

pub fn is_valid_audio_mix(gain_db: f64, pan: f64) -> bool {
    gain_db.is_finite()
        && (-96.0..=15.0).contains(&gain_db)
        && pan.is_finite()
        && (-1.0..=1.0).contains(&pan)
}

/// track の gain と pan から左右の倍率を決める。-96 dB 以下は無音。
pub fn audio_mix_gains(gain_db: f64, pan: f64) -> (f64, f64) {
    let gain = if gain_db <= -96.0 {
        0.0
    } else {
        10.0_f64.powf(gain_db / 20.0)
    };
    let left = if pan > 0.0 { 1.0 - pan } else { 1.0 };
    let right = if pan < 0.0 { 1.0 + pan } else { 1.0 };
    (gain * left, gain * right)
}

/// clip がどの track に載っているか。kind と index を必ず組で扱い、
/// 「V1 と A1 が同じ 0」という取り違えを型で防ぐ。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TrackRef {
    pub kind: TrackKind,
    pub index: usize,
}

impl TrackRef {
    pub const fn new(kind: TrackKind, index: usize) -> Self {
        Self { kind, index }
    }

    pub const fn video(index: usize) -> Self {
        Self::new(TrackKind::Video, index)
    }

    pub const fn audio(index: usize) -> Self {
        Self::new(TrackKind::Audio, index)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FrameHold {
    pub source_frame: i64,
    pub source_fps: Rational,
    pub source_frame_count: i64,
    /// 保持元 clip の速度。高 fps 素材を slow motion にした clip では、素材 fps のままでは
    /// timeline へ出せない frame があるため、同じ速度の位置で保持する。
    pub speed: Rational,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TimelineClip {
    pub kind: TimelineClipKind,
    /// 解決済みの実ファイル。
    pub media_path: PathBuf,
    /// UI 表示名。
    pub name: String,
    /// Project 内で一意な永続 ID。
    pub id: String,
    /// 素材の出どころ（プロジェクトパネルの MediaItem の id）。動画・音声・画像の clip は必須。
    pub media_item_id: String,
    pub source_fps: Rational,
    pub source_frame_count: i64,
    /// inclusive、素材固有の frame domain。
    pub source_in_frame: i64,
    /// exclusive、素材固有の frame domain。
    pub source_out_frame: i64,
    /// Project の timebase。
    pub timeline_start_frame: i64,
    pub effects: ClipEffects,
    pub track: TrackRef,
    /// 同じ値を持つ video / audio clip はリンクされている。空文字列は未リンク。
    /// リンクは横移動と削除だけを同期し、track と trim は各 clip 固有に保つ。
    pub link_group_id: String,
    /// 再生速度（約分済みの正の有理数）。1/1 が等速、1/2 が 50%。
    /// timeline との換算は必ず `clip_timebase` を通す。
    pub speed: Rational,
    pub preserve_pitch: bool,
    pub frame_hold: Option<FrameHold>,
    pub text: TextClipData,
    /// kind が Math のときだけ意味を持つ。
    pub math: MathClipData,
    /// kind が Math のときだけ意味を持つ時間の振る舞い（Write など）。
    pub math_animation: MathClipAnimation,
    pub equation_sequence: EquationSequenceClipData,
    pub graph: GraphClipData,
    /// 無効にした clip は timeline に残るが、preview・書き出し・音声に出さない（Shift+E）。
    pub enabled: bool,
}

impl Default for TimelineClip {
    fn default() -> Self {
        Self {
            kind: TimelineClipKind::Video,
            media_path: PathBuf::new(),
            name: String::new(),
            id: String::new(),
            media_item_id: String::new(),
            source_fps: Rational::new(0, 1),
            source_frame_count: 0,
            source_in_frame: 0,
            source_out_frame: 0,
            timeline_start_frame: 0,
            effects: ClipEffects::default(),
            track: TrackRef::video(0),
            link_group_id: String::new(),
            speed: Rational::new(1, 1),
            preserve_pitch: false,
            frame_hold: None,
            text: TextClipData::default(),
            math: MathClipData::default(),
            math_animation: MathClipAnimation::default(),
            equation_sequence: EquationSequenceClipData::default(),
            graph: GraphClipData::default(),
            enabled: true,
        }
    }
}

impl TimelineClip {
    /// 素材 frame domain が合成値（in = 0・out = 尺）の clip か。
    pub fn has_synthetic_source_domain(&self) -> bool {
        self.kind.is_still() || self.frame_hold.is_some()
    }

    pub fn source_duration(&self) -> i64 {
        self.source_out_frame - self.source_in_frame
    }

    pub fn is_linked(&self) -> bool {
        !self.link_group_id.is_empty()
    }
}

/// トランジションの種類。
/// - Blend: 映像はクロスディゾルブ、音声は等パワーのクロスフェード（track の種別で決まる）
/// - MathTransform: 隣り合う 2 つの数式 clip の間で、前の式を後ろの式へ変形する
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TransitionKind {
    Blend,
    MathTransform,
}

impl TransitionKind {
    pub fn name(self) -> &'static str {
        match self {
            Self::Blend => "blend",
            Self::MathTransform => "math_transform",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        [Self::Blend, Self::MathTransform]
            .into_iter()
            .find(|kind| kind.name() == name)
    }
}

/// 同じ track で接している 2 clip の編集点に置くトランジション。区間は timeline frame で
/// [cut - frames_before_cut, cut + frames_after_cut)。cut は outgoing の終端 = incoming の先頭。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TimelineTransition {
    pub id: String,
    pub outgoing_clip_id: String,
    pub incoming_clip_id: String,
    pub frames_before_cut: i64,
    pub frames_after_cut: i64,
    pub kind: TransitionKind,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SubtitleCue {
    pub id: String,
    pub start_frame: i64,
    pub end_frame: i64,
    pub content: String,
    /// 文字起こし元の timeline clip。空は未リンク。clip を横へ動かすと同じ量だけ追従し、
    /// clip を削除・カットすると一緒に消える。
    pub link_clip_id: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SubtitleStyle {
    pub font_family: String,
    pub font_size: i32,
    pub bold: bool,
    pub color: String,
    pub outline_color: String,
    pub outline_width: i32,
    pub background_color: String,
    pub alignment: String,
    pub side_margin: f64,
    pub bottom_margin: f64,
}

impl Default for SubtitleStyle {
    fn default() -> Self {
        Self {
            font_family: "Meiryo".to_owned(),
            font_size: 64,
            bold: false,
            color: "#FFFFFFFF".to_owned(),
            outline_color: "#FF000000".to_owned(),
            outline_width: 3,
            background_color: "#00000000".to_owned(),
            alignment: "center".to_owned(),
            side_margin: 0.05,
            bottom_margin: 0.15,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct SubtitleTrack {
    pub visible: bool,
    pub style: SubtitleStyle,
    pub cues: Vec<SubtitleCue>,
}

impl Default for SubtitleTrack {
    fn default() -> Self {
        Self {
            visible: true,
            style: SubtitleStyle::default(),
            cues: Vec::new(),
        }
    }
}

/// プロジェクトパネルの素材の種別。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MediaKind {
    Video,
    Audio,
    Image,
}

impl MediaKind {
    pub fn name(self) -> &'static str {
        match self {
            Self::Video => "video",
            Self::Audio => "audio",
            Self::Image => "image",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        [Self::Video, Self::Audio, Self::Image]
            .into_iter()
            .find(|kind| kind.name() == name)
    }
}

/// parent_id が空なら root 直下。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MediaFolder {
    /// folder と media item で共有する名前空間の中で一意。
    pub id: String,
    pub name: String,
    pub parent_id: String,
}

/// 種別ごとに意味を持つ field だけを埋め、それ以外は既定値のままにする。
/// - Video: fps / frame_count / width / height
/// - Audio: sample_rate / duration_samples
/// - Image: width / height
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MediaItem {
    pub id: String,
    pub kind: MediaKind,
    /// 解決済みの実ファイル。bin 内で一意。
    pub media_path: PathBuf,
    pub name: String,
    /// 空なら root 直下。
    pub folder_id: String,
    pub fps: Rational,
    pub frame_count: i64,
    pub width: i32,
    pub height: i32,
    pub sample_rate: i32,
    pub duration_samples: i64,
}

impl Default for MediaItem {
    fn default() -> Self {
        Self {
            id: String::new(),
            kind: MediaKind::Video,
            media_path: PathBuf::new(),
            name: String::new(),
            folder_id: String::new(),
            fps: Rational::new(0, 1),
            frame_count: 0,
            width: 0,
            height: 0,
            sample_rate: 0,
            duration_samples: 0,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Project {
    pub timeline_fps: Rational,
    /// Project の出力 raster。preview と書き出しが共有する基準寸法。
    pub output_width: i32,
    pub output_height: i32,
    /// index 0 が最下層（V1）。合成順は index 昇順で bottom → top。
    pub video_tracks: Vec<Track>,
    pub audio_tracks: Vec<Track>,
    pub manim_assets: Vec<ManimAsset>,
    pub timeline_clips: Vec<TimelineClip>,
    pub timeline_transitions: Vec<TimelineTransition>,
    pub timeline_markers: Vec<i64>,
    pub in_frame: Option<i64>,
    pub out_frame: Option<i64>,
    pub media_folders: Vec<MediaFolder>,
    pub media_items: Vec<MediaItem>,
    pub subtitles: Option<SubtitleTrack>,
    /// 自動音量調整ダイアログが次に開くときに出す、最後に適用した設定。
    pub last_audio_adjustment_settings: String,
}

impl Default for Project {
    /// 新規 Project の初期構成（V1・V2・A1）。track が 0 本の Project を作らせない。
    fn default() -> Self {
        Self {
            timeline_fps: Rational::new(60, 1),
            output_width: 1920,
            output_height: 1080,
            video_tracks: vec![
                Track::named(TrackKind::Video.default_track_name(0)),
                Track::named(TrackKind::Video.default_track_name(1)),
            ],
            audio_tracks: vec![Track::named(TrackKind::Audio.default_track_name(0))],
            manim_assets: Vec::new(),
            timeline_clips: Vec::new(),
            timeline_transitions: Vec::new(),
            timeline_markers: Vec::new(),
            in_frame: None,
            out_frame: None,
            media_folders: Vec::new(),
            media_items: Vec::new(),
            subtitles: None,
            last_audio_adjustment_settings: String::new(),
        }
    }
}

impl Project {
    pub fn tracks(&self, kind: TrackKind) -> &Vec<Track> {
        match kind {
            TrackKind::Video => &self.video_tracks,
            TrackKind::Audio => &self.audio_tracks,
        }
    }

    pub fn tracks_mut(&mut self, kind: TrackKind) -> &mut Vec<Track> {
        match kind {
            TrackKind::Video => &mut self.video_tracks,
            TrackKind::Audio => &mut self.audio_tracks,
        }
    }

    pub fn track(&self, track: TrackRef) -> Option<&Track> {
        self.tracks(track.kind).get(track.index)
    }

    pub fn is_valid_track_ref(&self, track: TrackRef) -> bool {
        self.track(track).is_some()
    }

    /// track の clip を出すか。video は mute（非表示）でなければ出す。audio は mute でなく、
    /// かつ solo の track が 1 つも無いか自分が solo なら鳴らす。描画・音声の経路は全てこれで判定する。
    pub fn is_track_output_enabled(&self, track: TrackRef) -> bool {
        let Some(target) = self.track(track) else {
            return false;
        };
        if target.muted {
            return false;
        }
        track.kind == TrackKind::Video
            || target.solo
            || !self.tracks(track.kind).iter().any(|other| other.solo)
    }

    pub fn clip(&self, id: &str) -> Option<&TimelineClip> {
        self.timeline_clips.iter().find(|clip| clip.id == id)
    }

    pub fn clip_index(&self, id: &str) -> Option<usize> {
        self.timeline_clips.iter().position(|clip| clip.id == id)
    }
}

pub fn is_valid_project_output_size(width: i32, height: i32) -> bool {
    (2..=MAXIMUM_PROJECT_OUTPUT_DIMENSION).contains(&width)
        && (2..=MAXIMUM_PROJECT_OUTPUT_DIMENSION).contains(&height)
        && width % 2 == 0
        && height % 2 == 0
}

/// timeline fps として設定できる rate か。core の configurable 表を参照し、ここに別表を持たない。
pub fn is_configurable_timeline_frame_rate(fps: Rational) -> bool {
    is_configurable_output_frame_rate(fps)
}

/// 実測して qualify した rate か。UI はこれで「設定はできるが未計測」を利用者へ出す。
pub fn is_measured_timeline_frame_rate(fps: Rational) -> bool {
    is_measured_output_frame_rate(fps)
}

/// 約分済みの正の組か。Project へ永続化する fps は canonical だけを認める。
pub fn is_canonical_frame_rate(fps: Rational) -> bool {
    fps.is_positive() && mvm_core::rational::gcd(fps.num, fps.den) == 1
}

/// Undo / Redo 履歴から捨てる件数。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct EditHistoryDrop {
    pub undo: usize,
    pub redo: usize,
}

/// Undo / Redo 履歴から捨てる件数を決める。
///
/// どちらの配列も「現在の状態から遠い順」に並べた各世代の byte 数（Undo は古い順、Redo は
/// 最後にやり直す編集から順）。両方の合計の件数が `max_entries` を超えるか、合計の byte 数が
/// `max_bytes` を超える間、現在の状態から最も遠い世代から捨てる（距離が同じなら Undo 側）。
/// ただし現在の状態に隣り合う Undo と Redo の 1 件ずつは予算を超えても残す。
pub fn edit_history_entries_to_drop(
    undo_farthest_first: &[usize],
    redo_farthest_first: &[usize],
    max_entries: usize,
    max_bytes: usize,
) -> EditHistoryDrop {
    let mut total: usize = undo_farthest_first
        .iter()
        .chain(redo_farthest_first)
        .fold(0, |sum, bytes| sum.saturating_add(*bytes));
    let mut drop = EditHistoryDrop::default();
    let mut undo_remaining = undo_farthest_first.len();
    let mut redo_remaining = redo_farthest_first.len();
    while undo_remaining + redo_remaining > max_entries || total > max_bytes {
        // 残っている中で最も遠い世代は、Undo 側なら undo_remaining 番目、Redo 側なら
        // redo_remaining 番目の距離にある。隣り合う 1 件（残り 1 件）は捨てない。
        let undo_droppable = undo_remaining > 1;
        let redo_droppable = redo_remaining > 1;
        if undo_droppable && (!redo_droppable || undo_remaining >= redo_remaining) {
            total = total.saturating_sub(undo_farthest_first[drop.undo]);
            drop.undo += 1;
            undo_remaining -= 1;
        } else if redo_droppable {
            total = total.saturating_sub(redo_farthest_first[drop.redo]);
            drop.redo += 1;
            redo_remaining -= 1;
        } else {
            break;
        }
    }
    drop
}
