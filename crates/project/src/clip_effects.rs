//! clip の effect（位置・拡大・回転・不透明度・音量・crop・fade）と keyframe。

use mvm_core::Rational;
use mvm_core::clip_fade::clip_fade_factor;
use mvm_core::layer_placement::{LayerRect, place_layer};
use mvm_core::source_frame_mapping::source_frame_at_output_position;

use crate::error::{ProjectResult, fail};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum KeyInterpolation {
    Linear,
    EaseIn,
    EaseOut,
    EaseInOut,
    Spline,
}

impl KeyInterpolation {
    pub const ALL: [Self; 5] = [
        Self::Linear,
        Self::EaseIn,
        Self::EaseOut,
        Self::EaseInOut,
        Self::Spline,
    ];
}

/// keyframe を持てる effect の種類。順序は crop を最後に置く（crop の値は上限未満）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ClipKeyKind {
    Opacity,
    Volume,
    Ducking,
    PositionX,
    PositionY,
    ScaleX,
    ScaleY,
    Rotation,
    CropLeft,
    CropTop,
    CropRight,
    CropBottom,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ClipKeyframe {
    pub frame: i64,
    pub value: f64,
    pub interpolation: KeyInterpolation,
    /// 元の曲線の部分区間。トリム・分割後も動きを保つ。
    pub curve_start: f64,
    pub curve_end: f64,
    /// 三次曲線の制御値。時間位置は区間の 1/3 と 2/3、値は端点間の 0..1。
    pub control1: f64,
    pub control2: f64,
}

impl ClipKeyframe {
    pub fn new(frame: i64, value: f64) -> Self {
        Self {
            frame,
            value,
            interpolation: KeyInterpolation::Linear,
            curve_start: 0.0,
            curve_end: 1.0,
            control1: 1.0 / 3.0,
            control2: 2.0 / 3.0,
        }
    }
}

impl Default for ClipKeyframe {
    fn default() -> Self {
        Self::new(0, 100.0)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ClipEffects {
    pub position_x_percent: f64,
    pub position_y_percent: f64,
    /// 縦横の拡大率は別々に持つ（preview の枠のハンドルで片方だけ伸縮できる）。
    pub scale_x_percent: f64,
    pub scale_y_percent: f64,
    pub rotation_degrees: f64,
    pub opacity_percent: f64,
    pub volume_percent: f64,
    pub normalization_gain_db: f64,
    pub ducking_db: f64,
    pub ducking_keys: Vec<ClipKeyframe>,
    /// 生成時の設定と入力の識別情報。再解析の必要性を判断するために保存する。
    pub audio_adjustment_settings: String,
    pub audio_adjustment_fingerprint: String,
    pub opacity_keys: Vec<ClipKeyframe>,
    pub volume_keys: Vec<ClipKeyframe>,
    pub position_x_keys: Vec<ClipKeyframe>,
    pub position_y_keys: Vec<ClipKeyframe>,
    pub scale_x_keys: Vec<ClipKeyframe>,
    pub scale_y_keys: Vec<ClipKeyframe>,
    pub rotation_keys: Vec<ClipKeyframe>,
    pub crop_left_keys: Vec<ClipKeyframe>,
    pub crop_top_keys: Vec<ClipKeyframe>,
    pub crop_right_keys: Vec<ClipKeyframe>,
    pub crop_bottom_keys: Vec<ClipKeyframe>,
    pub crop_left_percent: f64,
    pub crop_top_percent: f64,
    pub crop_right_percent: f64,
    pub crop_bottom_percent: f64,
    /// 素材固有の frame 数。
    pub fade_in_frames: i64,
    /// 素材固有の frame 数。
    pub fade_out_frames: i64,
}

impl Default for ClipEffects {
    fn default() -> Self {
        Self {
            position_x_percent: 0.0,
            position_y_percent: 0.0,
            scale_x_percent: 100.0,
            scale_y_percent: 100.0,
            rotation_degrees: 0.0,
            opacity_percent: 100.0,
            volume_percent: 100.0,
            normalization_gain_db: 0.0,
            ducking_db: 0.0,
            ducking_keys: Vec::new(),
            audio_adjustment_settings: String::new(),
            audio_adjustment_fingerprint: String::new(),
            opacity_keys: Vec::new(),
            volume_keys: Vec::new(),
            position_x_keys: Vec::new(),
            position_y_keys: Vec::new(),
            scale_x_keys: Vec::new(),
            scale_y_keys: Vec::new(),
            rotation_keys: Vec::new(),
            crop_left_keys: Vec::new(),
            crop_top_keys: Vec::new(),
            crop_right_keys: Vec::new(),
            crop_bottom_keys: Vec::new(),
            crop_left_percent: 0.0,
            crop_top_percent: 0.0,
            crop_right_percent: 0.0,
            crop_bottom_percent: 0.0,
            fade_in_frames: 0,
            fade_out_frames: 0,
        }
    }
}

/// keyframe を持てる effect の名前と値の範囲。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EffectChannel {
    pub kind: ClipKeyKind,
    pub name: &'static str,
    pub minimum: f64,
    pub maximum: f64,
}

/// 全ての effect channel。並びは JSON の読み書きと検証の順。
pub const EFFECT_CHANNELS: [EffectChannel; 12] = [
    channel(ClipKeyKind::Opacity, "opacity", 0.0, 100.0),
    channel(ClipKeyKind::Volume, "volume", 0.0, 200.0),
    channel(ClipKeyKind::Ducking, "ducking", -60.0, 0.0),
    channel(ClipKeyKind::PositionX, "positionX", -1000.0, 1000.0),
    channel(ClipKeyKind::PositionY, "positionY", -1000.0, 1000.0),
    channel(ClipKeyKind::ScaleX, "scaleX", 1.0, 1000.0),
    channel(ClipKeyKind::ScaleY, "scaleY", 1.0, 1000.0),
    channel(ClipKeyKind::Rotation, "rotation", -360.0, 360.0),
    channel(ClipKeyKind::CropLeft, "cropLeft", 0.0, 100.0),
    channel(ClipKeyKind::CropTop, "cropTop", 0.0, 100.0),
    channel(ClipKeyKind::CropRight, "cropRight", 0.0, 100.0),
    channel(ClipKeyKind::CropBottom, "cropBottom", 0.0, 100.0),
];

const fn channel(
    kind: ClipKeyKind,
    name: &'static str,
    minimum: f64,
    maximum: f64,
) -> EffectChannel {
    EffectChannel {
        kind,
        name,
        minimum,
        maximum,
    }
}

impl ClipKeyKind {
    pub fn channel(self) -> &'static EffectChannel {
        // EFFECT_CHANNELS は ClipKeyKind の宣言順に並んでいる（試験で検査する）。
        &EFFECT_CHANNELS[self as usize]
    }

    pub fn from_name(name: &str) -> Option<Self> {
        EFFECT_CHANNELS
            .iter()
            .find(|channel| channel.name == name)
            .map(|channel| channel.kind)
    }

    pub fn is_audio(self) -> bool {
        matches!(self, Self::Volume | Self::Ducking)
    }
}

impl EffectChannel {
    /// 値が範囲内か。crop は上限ちょうどを許さない（全部を切り落とさない）。
    pub fn accepts(&self, value: f64) -> bool {
        in_range(value, self.minimum, self.maximum)
            && (self.kind < ClipKeyKind::CropLeft || value < self.maximum)
    }
}

impl ClipEffects {
    /// channel の固定値（keyframe が無いときの値）。
    pub fn base(&self, kind: ClipKeyKind) -> f64 {
        match kind {
            ClipKeyKind::Opacity => self.opacity_percent,
            ClipKeyKind::Volume => self.volume_percent,
            ClipKeyKind::Ducking => self.ducking_db,
            ClipKeyKind::PositionX => self.position_x_percent,
            ClipKeyKind::PositionY => self.position_y_percent,
            ClipKeyKind::ScaleX => self.scale_x_percent,
            ClipKeyKind::ScaleY => self.scale_y_percent,
            ClipKeyKind::Rotation => self.rotation_degrees,
            ClipKeyKind::CropLeft => self.crop_left_percent,
            ClipKeyKind::CropTop => self.crop_top_percent,
            ClipKeyKind::CropRight => self.crop_right_percent,
            ClipKeyKind::CropBottom => self.crop_bottom_percent,
        }
    }

    pub fn base_mut(&mut self, kind: ClipKeyKind) -> &mut f64 {
        match kind {
            ClipKeyKind::Opacity => &mut self.opacity_percent,
            ClipKeyKind::Volume => &mut self.volume_percent,
            ClipKeyKind::Ducking => &mut self.ducking_db,
            ClipKeyKind::PositionX => &mut self.position_x_percent,
            ClipKeyKind::PositionY => &mut self.position_y_percent,
            ClipKeyKind::ScaleX => &mut self.scale_x_percent,
            ClipKeyKind::ScaleY => &mut self.scale_y_percent,
            ClipKeyKind::Rotation => &mut self.rotation_degrees,
            ClipKeyKind::CropLeft => &mut self.crop_left_percent,
            ClipKeyKind::CropTop => &mut self.crop_top_percent,
            ClipKeyKind::CropRight => &mut self.crop_right_percent,
            ClipKeyKind::CropBottom => &mut self.crop_bottom_percent,
        }
    }

    pub fn keys(&self, kind: ClipKeyKind) -> &Vec<ClipKeyframe> {
        match kind {
            ClipKeyKind::Opacity => &self.opacity_keys,
            ClipKeyKind::Volume => &self.volume_keys,
            ClipKeyKind::Ducking => &self.ducking_keys,
            ClipKeyKind::PositionX => &self.position_x_keys,
            ClipKeyKind::PositionY => &self.position_y_keys,
            ClipKeyKind::ScaleX => &self.scale_x_keys,
            ClipKeyKind::ScaleY => &self.scale_y_keys,
            ClipKeyKind::Rotation => &self.rotation_keys,
            ClipKeyKind::CropLeft => &self.crop_left_keys,
            ClipKeyKind::CropTop => &self.crop_top_keys,
            ClipKeyKind::CropRight => &self.crop_right_keys,
            ClipKeyKind::CropBottom => &self.crop_bottom_keys,
        }
    }

    pub fn keys_mut(&mut self, kind: ClipKeyKind) -> &mut Vec<ClipKeyframe> {
        match kind {
            ClipKeyKind::Opacity => &mut self.opacity_keys,
            ClipKeyKind::Volume => &mut self.volume_keys,
            ClipKeyKind::Ducking => &mut self.ducking_keys,
            ClipKeyKind::PositionX => &mut self.position_x_keys,
            ClipKeyKind::PositionY => &mut self.position_y_keys,
            ClipKeyKind::ScaleX => &mut self.scale_x_keys,
            ClipKeyKind::ScaleY => &mut self.scale_y_keys,
            ClipKeyKind::Rotation => &mut self.rotation_keys,
            ClipKeyKind::CropLeft => &mut self.crop_left_keys,
            ClipKeyKind::CropTop => &mut self.crop_top_keys,
            ClipKeyKind::CropRight => &mut self.crop_right_keys,
            ClipKeyKind::CropBottom => &mut self.crop_bottom_keys,
        }
    }

    /// 全 channel の key 列に同じ変換を掛ける。
    pub fn for_each_keys_mut(
        &mut self,
        mut apply: impl FnMut(ClipKeyKind, &mut Vec<ClipKeyframe>),
    ) {
        for channel in &EFFECT_CHANNELS {
            apply(channel.kind, self.keys_mut(channel.kind));
        }
    }

    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }

    /// clip-local frame での値を固定値に入れた effects（key は空）。
    pub fn evaluated_at(&self, local_frame: i64) -> Self {
        let mut result = Self {
            fade_in_frames: self.fade_in_frames,
            fade_out_frames: self.fade_out_frames,
            normalization_gain_db: self.normalization_gain_db,
            audio_adjustment_settings: self.audio_adjustment_settings.clone(),
            audio_adjustment_fingerprint: self.audio_adjustment_fingerprint.clone(),
            ..Self::default()
        };
        for channel in &EFFECT_CHANNELS {
            *result.base_mut(channel.kind) = evaluate_clip_keys(
                self.keys(channel.kind),
                self.base(channel.kind),
                local_frame,
            );
        }
        result
    }

    /// 固定値と fade を検証する（key は [`validate_effect_keys`]）。
    pub fn validate(&self, source_native_duration: i64) -> ProjectResult<()> {
        if !in_range(self.normalization_gain_db, -96.0, 60.0) {
            return fail("ラウドネス補正は −96〜60 dB の範囲で指定してください");
        }
        if let Some(channel) = EFFECT_CHANNELS
            .iter()
            .find(|channel| !channel.accepts(self.base(channel.kind)))
        {
            return fail(format!("エフェクトの固定値が範囲外です: {}", channel.name));
        }
        if self.crop_left_percent + self.crop_right_percent >= 100.0
            || self.crop_top_percent + self.crop_bottom_percent >= 100.0
        {
            return fail("左右または上下の Crop 合計は 100% 未満である必要があります");
        }
        let fades_fit = source_native_duration > 0
            && (0..=source_native_duration).contains(&self.fade_in_frames)
            && (0..=source_native_duration).contains(&self.fade_out_frames)
            && self.fade_in_frames <= source_native_duration - self.fade_out_frames;
        if !fades_fit {
            return fail(
                "フェードイン/アウトは素材固有フレームのclip尺内で重ならない必要があります",
            );
        }
        Ok(())
    }
}

fn in_range(value: f64, minimum: f64, maximum: f64) -> bool {
    value.is_finite() && value >= minimum && value <= maximum
}

fn ease(interpolation: KeyInterpolation, t: f64, c1: f64, c2: f64) -> f64 {
    match interpolation {
        KeyInterpolation::Linear => t,
        KeyInterpolation::EaseIn => t * t,
        KeyInterpolation::EaseOut => t * (2.0 - t),
        KeyInterpolation::EaseInOut => t * t * (3.0 - 2.0 * t),
        KeyInterpolation::Spline => {
            3.0 * (1.0 - t) * (1.0 - t) * t * c1 + 3.0 * (1.0 - t) * t * t * c2 + t * t * t
        }
    }
}

fn key_ease(key: &ClipKeyframe, t: f64) -> f64 {
    ease(key.interpolation, t, key.control1, key.control2)
}

fn evaluate_keys_at(keys: &[ClipKeyframe], base: f64, frame: f64) -> f64 {
    let next = keys.partition_point(|key| (key.frame as f64) < frame);
    let (Some(first), Some(last)) = (keys.first(), keys.last()) else {
        return base;
    };
    if next == 0 {
        return first.value;
    }
    let Some(next_key) = keys.get(next) else {
        return last.value;
    };
    if next_key.frame as f64 == frame {
        return next_key.value;
    }
    let previous = &keys[next - 1];
    let t = (frame - previous.frame as f64) / (next_key.frame - previous.frame) as f64;
    let start = key_ease(previous, previous.curve_start);
    let end = key_ease(previous, previous.curve_end);
    let progress = key_ease(
        previous,
        previous.curve_start + (previous.curve_end - previous.curve_start) * t,
    );
    previous.value + (next_key.value - previous.value) * (progress - start) / (end - start)
}

/// key 列を clip-local frame で評価する。key が無ければ `base_percent`。
pub fn evaluate_clip_keys(keys: &[ClipKeyframe], base_percent: f64, local_frame: i64) -> f64 {
    evaluate_keys_at(keys, base_percent, local_frame as f64)
}

/// key を挿入する。同じ frame に key があれば値だけ変える。key 間へ入れるときは、前の key の曲線を
/// その位置で 2 つに分け、動きを変えない。
pub fn insert_clip_key(keys: &mut Vec<ClipKeyframe>, frame: i64, value: f64) {
    let next = keys.partition_point(|key| key.frame < frame);
    if let Some(existing) = keys.get_mut(next)
        && existing.frame == frame
    {
        existing.value = value;
        return;
    }
    let mut inserted = ClipKeyframe::new(frame, value);
    if next > 0 && next < keys.len() {
        let next_frame = keys[next].frame;
        let previous = &mut keys[next - 1];
        let t = (frame - previous.frame) as f64 / (next_frame - previous.frame) as f64;
        let split = previous.curve_start + (previous.curve_end - previous.curve_start) * t;
        inserted.interpolation = previous.interpolation;
        inserted.control1 = previous.control1;
        inserted.control2 = previous.control2;
        inserted.curve_start = split;
        inserted.curve_end = previous.curve_end;
        previous.curve_end = split;
    }
    keys.insert(next, inserted);
}

/// `frames` に一致する key を削除し、削除した個数を返す。[`insert_clip_key`] の逆で、削除した key の
/// 区間が直前に残る key の区間と同じ元曲線の連続部分なら、直前の区間を削除した区間の終端まで延ばす。
pub fn remove_clip_keys(keys: &mut Vec<ClipKeyframe>, frames: &[i64]) -> usize {
    let mut kept: Vec<ClipKeyframe> = Vec::with_capacity(keys.len());
    for key in keys.iter() {
        if !frames.contains(&key.frame) {
            kept.push(*key);
            continue;
        }
        // insert_clip_key が分けた境界は同じ値で書き込まれるので、完全一致で判定できる。
        if let Some(previous) = kept.last_mut()
            && previous.curve_end == key.curve_start
            && previous.interpolation == key.interpolation
            && previous.control1 == key.control1
            && previous.control2 == key.control2
        {
            previous.curve_end = key.curve_end;
        }
    }
    let removed = keys.len() - kept.len();
    *keys = kept;
    removed
}

/// 全 channel の key を検証する。`audio` は音声 clip の key（volume / ducking だけを許す）か。
pub fn validate_effect_keys(
    effects: &ClipEffects,
    duration: i64,
    audio: bool,
) -> ProjectResult<()> {
    for channel in &EFFECT_CHANNELS {
        let keys = effects.keys(channel.kind);
        if !keys.is_empty() && channel.kind.is_audio() != audio {
            return fail("素材種別に適用できないキーフレームです");
        }
        let mut previous = -1;
        for key in keys {
            let valid = key.frame > previous
                && (0..duration).contains(&key.frame)
                && channel.accepts(key.value)
                && in_range(key.curve_start, 0.0, 1.0)
                && in_range(key.curve_end, 0.0, 1.0)
                && key.curve_start < key.curve_end
                && in_range(key.control1, 0.0, 1.0)
                && in_range(key.control2, 0.0, 1.0)
                && key_ease(key, key.curve_end) > key_ease(key, key.curve_start);
            if !valid {
                return fail("キーフレームの位置・値・補間が不正です");
            }
            previous = key.frame;
        }
    }
    validate_crop_sums(effects, duration)
}

/// 左右・上下の crop の合計が全区間で 100% 未満か。区間ごとの三次多項式の極値まで調べるので、
/// 補間の違う 2 channel の合計でも見落とさない。
fn validate_crop_sums(effects: &ClipEffects, duration: i64) -> ProjectResult<()> {
    for (a, b) in [
        (ClipKeyKind::CropLeft, ClipKeyKind::CropRight),
        (ClipKeyKind::CropTop, ClipKeyKind::CropBottom),
    ] {
        let mut boundaries = vec![0, (duration - 1).max(0)];
        boundaries.extend(effects.keys(a).iter().map(|key| key.frame));
        boundaries.extend(effects.keys(b).iter().map(|key| key.frame));
        boundaries.sort_unstable();
        boundaries.dedup();
        let sum = |frame: f64| {
            evaluate_keys_at(effects.keys(a), effects.base(a), frame)
                + evaluate_keys_at(effects.keys(b), effects.base(b), frame)
        };
        for (index, boundary) in boundaries.iter().enumerate() {
            if sum(*boundary as f64) >= 100.0 {
                return fail("クロップ合計は全区間で100%未満である必要があります");
            }
            let Some(next) = boundaries.get(index + 1) else {
                break;
            };
            let start = *boundary as f64;
            let span = (next - boundary) as f64;
            let d = sum(start);
            let y1 = sum(start + span / 3.0) - d;
            let y2 = sum(start + span * 2.0 / 3.0) - d;
            let y3 = sum(start + span) - d;
            let c3 = 4.5 * y3 - 13.5 * y2 + 13.5 * y1;
            let c2 = -4.5 * y3 + 18.0 * y2 - 22.5 * y1;
            let c1 = y3 - c3 - c2;
            let invalid_at = |t: f64| t > 0.0 && t < 1.0 && sum(start + span * t) >= 100.0;
            let exceeds = if c3.abs() < 1e-10 {
                c2.abs() > 1e-10 && invalid_at(-c1 / (2.0 * c2))
            } else {
                let discriminant = 4.0 * c2 * c2 - 12.0 * c3 * c1;
                discriminant >= 0.0
                    && (invalid_at((-2.0 * c2 + discriminant.sqrt()) / (6.0 * c3))
                        || invalid_at((-2.0 * c2 - discriminant.sqrt()) / (6.0 * c3)))
            };
            if exceeds {
                return fail("キー間でクロップ合計が100%以上になります");
            }
        }
    }
    Ok(())
}

/// 不透明度（`maximum_percent` <= 100）または音量の key 列だけを検証する。
pub fn validate_clip_keyframes(
    keys: &[ClipKeyframe],
    timeline_duration: i64,
    maximum_percent: f64,
) -> ProjectResult<()> {
    let audio = maximum_percent > 100.0;
    let mut effects = ClipEffects::default();
    if audio {
        effects.volume_keys = keys.to_vec();
    } else {
        effects.opacity_keys = keys.to_vec();
    }
    validate_effect_keys(&effects, timeline_duration, audio)?;
    if keys.iter().any(|key| key.value > maximum_percent) {
        return fail("キーフレームの値が上限を超えています");
    }
    Ok(())
}

/// 切り出された区間も含め、表示・編集用の三次曲線ハンドルへ換算する。
pub fn clip_key_spline_controls(key: &ClipKeyframe) -> (f64, f64) {
    let start = key_ease(key, key.curve_start);
    let end = key_ease(key, key.curve_end);
    let value = |t: f64| {
        (key_ease(key, key.curve_start + (key.curve_end - key.curve_start) * t) - start)
            / (end - start)
    };
    let a = 27.0 * value(1.0 / 3.0) - 1.0;
    let b = 27.0 * value(2.0 / 3.0) - 8.0;
    ((2.0 * a - b) / 18.0, (2.0 * b - a) / 18.0)
}

pub fn evaluate_clip_opacity(
    effects: &ClipEffects,
    timeline_local_frame: i64,
    source_local_frame: i64,
    source_duration: i64,
) -> f64 {
    evaluate_clip_keys(
        &effects.opacity_keys,
        effects.opacity_percent,
        timeline_local_frame,
    ) / 100.0
        * clip_fade_factor(
            source_local_frame,
            source_duration,
            effects.fade_in_frames,
            effects.fade_out_frames,
        )
}

pub fn evaluate_clip_volume(
    effects: &ClipEffects,
    timeline_local_frame: i64,
    source_local_frame: i64,
    source_duration: i64,
) -> f64 {
    evaluate_clip_keys(
        &effects.volume_keys,
        effects.volume_percent,
        timeline_local_frame,
    ) / 100.0
        * clip_fade_factor(
            source_local_frame,
            source_duration,
            effects.fade_in_frames,
            effects.fade_out_frames,
        )
}

/// 尺 `old_duration` の key を尺 `new_duration` へ伸縮する（レート調整）。key は素材の内容に付いて動き、
/// 先頭と末尾の frame はそれぞれ先頭と末尾へ写る。丸めで同じ frame に重なった key は先の 1 つを残す。
pub fn rescale_clip_keys(keys: &mut Vec<ClipKeyframe>, old_duration: i64, new_duration: i64) {
    if keys.is_empty() || old_duration <= 0 || new_duration <= 0 || old_duration == new_duration {
        return;
    }
    let old = std::mem::take(keys);
    for key in old {
        // 端から端へ: frame k → round(k (new - 1) / (old - 1))。
        let frame = if old_duration == 1 {
            0
        } else {
            let numerator = i128::from(key.frame) * i128::from(new_duration - 1) * 2
                + i128::from(old_duration - 1);
            let denominator = i128::from(old_duration - 1) * 2;
            i64::try_from(numerator / denominator).unwrap_or(i64::MAX)
        };
        if keys.last().is_none_or(|last| last.frame != frame) {
            keys.push(ClipKeyframe { frame, ..key });
        }
    }
}

/// 尺 `old_duration` の key 列を、元の frame `new_start_in_old_frames` から始まる尺 `new_duration` の
/// 区間へ切り出す（トリム・分割）。区間の端には元の曲線上の値で key を置き、曲線の部分区間を保つ。
pub fn reframe_clip_keys(
    keys: &mut Vec<ClipKeyframe>,
    old_duration: i64,
    new_duration: i64,
    new_start_in_old_frames: i64,
) {
    let Some(first_value) = keys.first().map(|key| key.value) else {
        return;
    };
    if old_duration <= 0 || new_duration <= 0 {
        return;
    }
    let old = std::mem::take(keys);
    let last_in_new = new_start_in_old_frames + new_duration - 1;
    let mut points = vec![new_start_in_old_frames];
    points.extend(
        old.iter()
            .map(|key| key.frame)
            .filter(|frame| *frame > new_start_in_old_frames && *frame < last_in_new),
    );
    if new_duration > 1 {
        points.push(last_in_new);
    }
    for (index, point) in points.iter().enumerate() {
        let mut key = ClipKeyframe::new(
            point - new_start_in_old_frames,
            evaluate_clip_keys(&old, first_value, *point),
        );
        if let Some(next_point) = points.get(index + 1) {
            let next = old.partition_point(|candidate| candidate.frame <= *point);
            if next > 0 && next < old.len() {
                let previous = &old[next - 1];
                let span = (old[next].frame - previous.frame) as f64;
                let curve = previous.curve_end - previous.curve_start;
                key.interpolation = previous.interpolation;
                key.control1 = previous.control1;
                key.control2 = previous.control2;
                key.curve_start =
                    previous.curve_start + curve * (point - previous.frame) as f64 / span;
                key.curve_end =
                    previous.curve_start + curve * (next_point - previous.frame) as f64 / span;
            }
        }
        keys.push(key);
    }
}

/// timeline fps の違う Project へ clip を移すときの key 換算。key は clip 先頭からの timeline frame なので、
/// 同じ秒位置に最も近い移し先の frame へ写す（ちょうど 1/2 は後ろへ）。尺 `new_duration` の外へ出た key は
/// 末尾 frame に寄せ、同じ frame に重なった key は先の 1 つを残す。fps が不正・換算が溢れたら失敗（keys は変えない）。
pub fn retime_clip_keys(
    keys: &mut Vec<ClipKeyframe>,
    from_fps: Rational,
    to_fps: Rational,
    new_duration: i64,
) -> ProjectResult<()> {
    if new_duration <= 0 {
        return fail("keyframe の換算先の尺が不正です");
    }
    let mut retimed: Vec<ClipKeyframe> = Vec::with_capacity(keys.len());
    for key in keys.iter() {
        // floor(k to / from + 1/2)。表示 frame の四捨五入と同じ有理数計算を使うため、
        // 移し先を「素材」、移し元を「output」として渡す。
        let Some(frame) = source_frame_at_output_position(key.frame, to_fps, from_fps) else {
            return fail("keyframe を新しい fps へ換算できません");
        };
        let clamped = frame.min(new_duration - 1);
        if retimed.last().is_none_or(|last| last.frame != clamped) {
            retimed.push(ClipKeyframe {
                frame: clamped,
                ..*key
            });
        }
    }
    *keys = retimed;
    Ok(())
}

/// 音量を dB 単位で 1 段上げ下げするときの下限（-60 dB）。0% からは倍率で上げられないので、
/// 上げるときは下限から始める。下げても下限より下（0% を含む）へは行かない。
pub const VOLUME_STEP_FLOOR_PERCENT: f64 = 0.1;
pub const VOLUME_MAXIMUM_PERCENT: f64 = 200.0;

/// `percent` を `step_db` だけ変えた値。上げるときは下限未満を下限へ寄せてから掛け、上限で止める。
/// 下げるときは下限で止め、既に下限以下の値は変えない。範囲外の入力は `None`。
pub fn step_volume_percent_by_db(percent: f64, step_db: f64) -> Option<f64> {
    if !in_range(percent, 0.0, VOLUME_MAXIMUM_PERCENT) || !step_db.is_finite() || step_db == 0.0 {
        return None;
    }
    let gain = 10.0_f64.powf(step_db / 20.0);
    if step_db > 0.0 {
        return Some(VOLUME_MAXIMUM_PERCENT.min(percent.max(VOLUME_STEP_FLOOR_PERCENT) * gain));
    }
    if percent <= VOLUME_STEP_FLOOR_PERCENT {
        return Some(percent);
    }
    Some(VOLUME_STEP_FLOOR_PERCENT.max(percent * gain))
}

/// effect を正規化した矩形へ写した結果。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ClipEffectMapping {
    pub source_rect: LayerRect,
    pub destination_rect: LayerRect,
    pub rotation_degrees: f64,
    pub base_opacity: f64,
    pub fade_in_frames: i64,
    pub fade_out_frames: i64,
}

pub fn map_clip_effects(effects: &ClipEffects) -> ClipEffectMapping {
    let left = effects.crop_left_percent / 100.0;
    let top = effects.crop_top_percent / 100.0;
    let width = 1.0 - left - effects.crop_right_percent / 100.0;
    let height = 1.0 - top - effects.crop_bottom_percent / 100.0;
    let center_x = left + width * 0.5;
    let center_y = top + height * 0.5;
    let scaled_width = width * effects.scale_x_percent / 100.0;
    let scaled_height = height * effects.scale_y_percent / 100.0;
    ClipEffectMapping {
        source_rect: LayerRect {
            x: left,
            y: top,
            width,
            height,
        },
        destination_rect: LayerRect {
            x: center_x - scaled_width * 0.5 + effects.position_x_percent / 100.0,
            y: center_y - scaled_height * 0.5 + effects.position_y_percent / 100.0,
            width: scaled_width,
            height: scaled_height,
        },
        rotation_degrees: effects.rotation_degrees,
        base_opacity: effects.opacity_percent / 100.0,
        fade_in_frames: effects.fade_in_frames,
        fade_out_frames: effects.fade_out_frames,
    }
}

/// 素材を canvas に置いたときに見えている矩形（出力画素、回転前）。preview の枠とハンドルが使う。
/// 幾何は preview / 書き出しと同じ `place_layer` で決める。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ClipVisualGeometry {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
    /// 回転の中心（出力画素）。
    pub pivot_x: f64,
    pub pivot_y: f64,
    pub rotation_degrees: f64,
}

/// 寸法が不正、または crop で何も見えていなければ `None`。
pub fn clip_visual_geometry(
    effects: &ClipEffects,
    source_width: i32,
    source_height: i32,
    canvas_width: i32,
    canvas_height: i32,
) -> Option<ClipVisualGeometry> {
    let mapped = map_clip_effects(effects);
    let placed = place_layer(
        source_width,
        source_height,
        canvas_width,
        canvas_height,
        &mapped.source_rect,
        &mapped.destination_rect,
    )?;
    let (canvas_width, canvas_height) = (f64::from(canvas_width), f64::from(canvas_height));
    Some(ClipVisualGeometry {
        x: placed.destination.x * canvas_width,
        y: placed.destination.y * canvas_height,
        width: placed.destination.width * canvas_width,
        height: placed.destination.height * canvas_height,
        pivot_x: placed.pivot_x * canvas_width,
        pivot_y: placed.pivot_y * canvas_height,
        rotation_degrees: mapped.rotation_degrees,
    })
}

/// [`clip_visual_geometry`] の逆。見えている矩形を (x, y, width, height) にする位置と拡大率を設定する。
/// crop と回転は変えない。拡大率は 1..1000%、位置は -1000..1000% に収める（収めた分だけ指定の矩形から
/// ずれる）。寸法が不正なら失敗し、effects を変えない。
#[allow(clippy::too_many_arguments)]
pub fn set_effects_for_visual_rect(
    effects: &mut ClipEffects,
    source_width: i32,
    source_height: i32,
    canvas_width: i32,
    canvas_height: i32,
    x: f64,
    y: f64,
    width: f64,
    height: f64,
) -> ProjectResult<()> {
    if ![x, y, width, height].iter().all(|value| value.is_finite()) || width <= 0.0 || height <= 0.0
    {
        return fail("枠の位置・大きさが不正です");
    }
    let crop = map_clip_effects(effects).source_rect;
    // crop 範囲をそのまま置いたときに見える範囲（letterbox との重なり）。拡縮と位置に依らない。
    let Some(visible) = place_layer(
        source_width,
        source_height,
        canvas_width,
        canvas_height,
        &crop,
        &crop,
    ) else {
        return fail("素材の寸法が不正か、crop で何も見えていません");
    };
    let seen = visible.destination;
    let (canvas_width, canvas_height) = (f64::from(canvas_width), f64::from(canvas_height));
    let scale_x = (width / canvas_width / seen.width).clamp(0.01, 10.0);
    let scale_y = (height / canvas_height / seen.height).clamp(0.01, 10.0);
    // 見える範囲の左上が x, y に来るように、crop 範囲の置き場所（destination）を決める。
    let destination_x = x / canvas_width - (seen.x - crop.x) * scale_x;
    let destination_y = y / canvas_height - (seen.y - crop.y) * scale_y;
    // map_clip_effects: destination = crop 中心 - 拡縮後の半分 + 位置。
    let position_x = destination_x - (crop.x + crop.width * 0.5) + crop.width * scale_x * 0.5;
    let position_y = destination_y - (crop.y + crop.height * 0.5) + crop.height * scale_y * 0.5;
    effects.scale_x_percent = scale_x * 100.0;
    effects.scale_y_percent = scale_y * 100.0;
    effects.position_x_percent = (position_x * 100.0).clamp(-1000.0, 1000.0);
    effects.position_y_percent = (position_y * 100.0).clamp(-1000.0, 1000.0);
    Ok(())
}
