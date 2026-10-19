//! timeline 上のマウス操作を、どの編集として確定するかの判定。
//!
//! 画面は press / release でここを呼び、返った内容どおりに command を実行するだけにする。
//! 判定を widget に置かない。release 時の位置や modifier で、press で決めた分割位置や Alt を変えない。

use super::common::TrimEdge;

/// タイムラインのツール。キーは Premiere に合わせ、音量の上げ下げだけ `[` / `]` を逆にしている。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TimelineTool {
    Select,
    TrackForward,
    TrackBackward,
    Razor,
    Ripple,
    Rolling,
    Rate,
    Slip,
    Slide,
    Pen,
    Hand,
    Zoom,
    Text,
}

/// ツールの表示と shortcut。一覧はここだけが持つ。
pub struct ToolSpec {
    pub tool: TimelineTool,
    pub group: &'static str,
    pub key: &'static str,
    pub name: &'static str,
    pub hint: &'static str,
}

pub const TOOLS: &[ToolSpec] = &[
    ToolSpec {
        tool: TimelineTool::Select,
        group: "select",
        key: "V",
        name: "選択",
        hint: "クリックで選択、ドラッグで移動、端のドラッグでトリミング。Shift で追加選択",
    },
    ToolSpec {
        tool: TimelineTool::TrackForward,
        group: "select",
        key: "A",
        name: "前方選択",
        hint: "クリック位置以降の clip を選ぶ。Shift でその track だけ",
    },
    ToolSpec {
        tool: TimelineTool::TrackBackward,
        group: "select",
        key: "Shift+A",
        name: "後方選択",
        hint: "クリック位置以前の clip を選ぶ。Shift でその track だけ",
    },
    ToolSpec {
        tool: TimelineTool::Razor,
        group: "cut",
        key: "C",
        name: "レーザー",
        hint: "押した位置で分割する。Shift で全 track",
    },
    ToolSpec {
        tool: TimelineTool::Ripple,
        group: "cut",
        key: "B",
        name: "リップル",
        hint: "端をドラッグすると尺が変わり、後ろの clip が詰まる",
    },
    ToolSpec {
        tool: TimelineTool::Rolling,
        group: "cut",
        key: "N",
        name: "ローリング",
        hint: "接している 2 clip の境界を、合計尺を変えずに動かす",
    },
    ToolSpec {
        tool: TimelineTool::Rate,
        group: "cut",
        key: "R",
        name: "レート",
        hint: "端をドラッグすると素材範囲を変えずに速度が変わる",
    },
    ToolSpec {
        tool: TimelineTool::Slip,
        group: "view",
        key: "Y",
        name: "スリップ",
        hint: "位置と長さを保ったまま、素材のインとアウトをずらす",
    },
    ToolSpec {
        tool: TimelineTool::Slide,
        group: "view",
        key: "U",
        name: "スライド",
        hint: "長さを保って動かし、前後の clip の長さを追従させる",
    },
    ToolSpec {
        tool: TimelineTool::Pen,
        group: "view",
        key: "P",
        name: "ペン",
        hint: "線の近くでキーを追加し、キーをドラッグして動かす。Alt で削除",
    },
    ToolSpec {
        tool: TimelineTool::Hand,
        group: "view",
        key: "H",
        name: "ハンド",
        hint: "タイムラインの表示をスクロールする。素材は動かない",
    },
    ToolSpec {
        tool: TimelineTool::Zoom,
        group: "view",
        key: "Z",
        name: "ズーム",
        hint: "クリックで拡大、Alt で縮小",
    },
    ToolSpec {
        tool: TimelineTool::Text,
        group: "text",
        key: "T",
        name: "文字",
        hint: "プレビューで文字を入力し、確定で clip を置く",
    },
];

impl TimelineTool {
    pub fn spec(self) -> &'static ToolSpec {
        TOOLS
            .iter()
            .find(|spec| spec.tool == self)
            .unwrap_or(&TOOLS[0])
    }
}

/// press 時点の修飾キー。Alt はリンク相手へ適用しない。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct GestureModifiers {
    pub alt: bool,
    pub shift: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BodyGesture {
    Move,
    Razor,
    Slip,
    Slide,
    TrackSelect,
}

/// press の時点で確定する操作。release の座標では分割位置も修飾キーも変えない。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BodyPress {
    pub gesture: BodyGesture,
    pub press_frame: i64,
    pub linked: bool,
    pub duplicate: bool,
    pub all_tracks: bool,
    pub additive: bool,
    pub shift: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BodyAction {
    Split {
        frame: i64,
        all_tracks: bool,
        linked: bool,
    },
    Slip {
        delta: i64,
        linked: bool,
    },
    Slide {
        delta: i64,
        linked: bool,
    },
    Move {
        frame: i64,
        linked: bool,
    },
    Duplicate {
        frame: i64,
        linked: bool,
    },
    Toggle {
        frame: i64,
    },
    Select {
        frame: i64,
        linked: bool,
    },
    None,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EdgeAction {
    SelectEdit {
        edge: TrimEdge,
    },
    Trim {
        edge: TrimEdge,
        delta: i64,
        linked: bool,
    },
    RippleTrim {
        edge: TrimEdge,
        delta: i64,
        linked: bool,
    },
    Roll {
        edge: TrimEdge,
        delta: i64,
        linked: bool,
    },
    RateStretch {
        edge: TrimEdge,
        delta: i64,
        linked: bool,
    },
    None,
}

/// 一緒にドラッグする clip 群の端。無い種別は丸めない。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DragBounds {
    pub min_start_frame: Option<i64>,
    pub video: Option<(i64, i64)>,
    pub audio: Option<(i64, i64)>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClipSpan {
    pub clip_id: String,
    pub start: i64,
    pub end: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SnapFrames {
    pub moving: Vec<i64>,
    pub targets: Vec<i64>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SnapOffset {
    pub offset_x: f64,
    pub frame: Option<i64>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PenGeometry {
    pub pixels_per_frame: f64,
    pub maximum: f64,
    pub width: f64,
    pub height: f64,
    pub inset: f64,
    pub key_pixels: f64,
    pub line_pixels: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PenSample {
    pub frame: i64,
    pub value: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PenKey {
    pub frame: i64,
    pub value: f64,
    pub samples: Vec<PenSample>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PenPoint {
    pub x: f64,
    pub y: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub enum PenPress {
    DeleteKey {
        frame: i64,
    },
    None,
    Select,
    Edit {
        original_frame: Option<i64>,
        frame: i64,
        value: f64,
        grab_offset_x: f64,
        grab_offset_y: f64,
    },
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PenRelease {
    pub original_frame: Option<i64>,
    pub frame: i64,
    pub value: f64,
}

pub fn linked_for(modifiers: GestureModifiers) -> bool {
    !modifiers.alt
}

/// ルーラー上のマーカー。半径の中で最も近い frame。同じ距離なら先に並んでいる方。
pub fn marker_near_ruler_x(
    x: f64,
    pixels_per_frame: f64,
    markers: &[i64],
    radius: f64,
) -> Option<i64> {
    let mut nearest = radius + 1.0;
    let mut result = None;
    for frame in markers {
        let distance = (x - *frame as f64 * pixels_per_frame).abs();
        if distance <= radius && distance < nearest {
            nearest = distance;
            result = Some(*frame);
        }
    }
    result
}

/// 群の最も左の clip が 0 frame に接したところで横ドラッグを止める。
pub fn group_drag_offset_x(offset_x: f64, pixels_per_frame: f64, bounds: DragBounds) -> f64 {
    let Some(min_start) = bounds.min_start_frame else {
        return offset_x;
    };
    (-min_start as f64 * pixels_per_frame).max(offset_x)
}

pub fn drag_snap_frames(
    spans: &[ClipSpan],
    moving_ids: &[&str],
    playhead_frame: i64,
) -> SnapFrames {
    let mut moving = Vec::new();
    let mut targets = vec![0, playhead_frame];
    for span in spans {
        if moving_ids.contains(&span.clip_id.as_str()) {
            moving.push(span.start);
            moving.push(span.end);
        } else {
            targets.push(span.start);
            targets.push(span.end);
        }
    }
    SnapFrames { moving, targets }
}

/// 動かす端のどれかが吸着先から `threshold_px` 以内なら、その端が重なる量へ寄せる。
/// 同じ距離なら後から見た組を採用する。吸着しなければ frame は `None`。
pub fn snap_drag_offset_x(
    offset_x: f64,
    pixels_per_frame: f64,
    snap: &SnapFrames,
    threshold_px: f64,
) -> SnapOffset {
    if !pixels_per_frame.is_finite() || pixels_per_frame <= 0.0 {
        return SnapOffset {
            offset_x,
            frame: None,
        };
    }
    let mut result = SnapOffset {
        offset_x,
        frame: None,
    };
    let mut nearest = threshold_px;
    for edge in &snap.moving {
        for target in &snap.targets {
            let candidate = (*target - *edge) as f64 * pixels_per_frame;
            let distance = (candidate - offset_x).abs();
            if distance <= nearest {
                nearest = distance;
                result = SnapOffset {
                    offset_x: candidate,
                    frame: Some(*target),
                };
            }
        }
    }
    result
}

/// 群の最上段・最下段が既存 track からはみ出さない index へ縦移動を丸める。
pub fn group_drag_track_index(
    kind_is_video: bool,
    anchor_index: i64,
    snapped_index: i64,
    track_count: i64,
    bounds: DragBounds,
) -> i64 {
    let Some((min_track, max_track)) = (if kind_is_video {
        bounds.video
    } else {
        bounds.audio
    }) else {
        return snapped_index;
    };
    let room = track_count - 1 - max_track;
    let delta = (-min_track).max((snapped_index - anchor_index).min(room));
    anchor_index + delta
}

pub fn body_press(tool: TimelineTool, modifiers: GestureModifiers, press_frame: i64) -> BodyPress {
    let gesture = match tool {
        TimelineTool::Razor => BodyGesture::Razor,
        TimelineTool::Slip => BodyGesture::Slip,
        TimelineTool::Slide => BodyGesture::Slide,
        TimelineTool::TrackForward | TimelineTool::TrackBackward => BodyGesture::TrackSelect,
        _ => BodyGesture::Move,
    };
    BodyPress {
        gesture,
        press_frame,
        linked: linked_for(modifiers),
        duplicate: tool == TimelineTool::Select && modifiers.alt,
        all_tracks: gesture == BodyGesture::Razor && modifiers.shift,
        additive: gesture == BodyGesture::Move && modifiers.shift,
        shift: modifiers.shift,
    }
}

pub fn body_release(
    state: &BodyPress,
    moved: bool,
    moved_to_frame: i64,
    release_frame: i64,
    tool_drag_frames: i64,
) -> BodyAction {
    match state.gesture {
        BodyGesture::Razor => {
            return BodyAction::Split {
                frame: state.press_frame,
                all_tracks: state.all_tracks,
                linked: state.linked,
            };
        }
        BodyGesture::Slip | BodyGesture::Slide => {
            if tool_drag_frames == 0 {
                return BodyAction::None;
            }
            return if state.gesture == BodyGesture::Slip {
                BodyAction::Slip {
                    delta: tool_drag_frames,
                    linked: state.linked,
                }
            } else {
                BodyAction::Slide {
                    delta: tool_drag_frames,
                    linked: state.linked,
                }
            };
        }
        BodyGesture::Move | BodyGesture::TrackSelect => {}
    }
    if moved {
        return if state.duplicate {
            BodyAction::Duplicate {
                frame: moved_to_frame,
                linked: state.linked,
            }
        } else {
            BodyAction::Move {
                frame: moved_to_frame,
                linked: state.linked,
            }
        };
    }
    if state.additive {
        return BodyAction::Toggle {
            frame: release_frame,
        };
    }
    if state.gesture == BodyGesture::TrackSelect {
        return BodyAction::None;
    }
    BodyAction::Select {
        frame: release_frame,
        linked: state.linked,
    }
}

/// 端を動かさずに離したら、選択・リップル・ローリングは編集点を選ぶ。レートは何もしない。
pub fn edge_release(tool: TimelineTool, edge: TrimEdge, delta: i64, linked: bool) -> EdgeAction {
    if delta == 0 {
        return if matches!(
            tool,
            TimelineTool::Select | TimelineTool::Ripple | TimelineTool::Rolling
        ) {
            EdgeAction::SelectEdit { edge }
        } else {
            EdgeAction::None
        };
    }
    match tool {
        TimelineTool::Ripple => EdgeAction::RippleTrim {
            edge,
            delta,
            linked,
        },
        TimelineTool::Rolling => EdgeAction::Roll {
            edge,
            delta,
            linked,
        },
        TimelineTool::Rate => EdgeAction::RateStretch {
            edge,
            delta,
            linked,
        },
        _ => EdgeAction::Trim {
            edge,
            delta,
            linked,
        },
    }
}

pub fn pen_usable_height(geometry: &PenGeometry) -> f64 {
    (geometry.height - 2.0 * geometry.inset).max(1.0)
}

pub fn pen_y(geometry: &PenGeometry, value_percent: f64) -> f64 {
    if !geometry.maximum.is_finite() || geometry.maximum <= 0.0 {
        return geometry.inset;
    }
    let clamped = value_percent.clamp(0.0, geometry.maximum);
    geometry.inset + (1.0 - clamped / geometry.maximum) * pen_usable_height(geometry)
}

pub fn pen_value_at(geometry: &PenGeometry, y: f64) -> f64 {
    let top = geometry.inset;
    let bottom = top + pen_usable_height(geometry);
    let span = bottom - top;
    if span <= 0.0 || !geometry.maximum.is_finite() {
        return 0.0;
    }
    let normalized = 1.0 - (y.clamp(top, bottom) - top) / span;
    (normalized * geometry.maximum).round()
}

pub fn pen_line_points(keys: &[PenKey], base_value: f64, geometry: &PenGeometry) -> Vec<PenPoint> {
    let Some(first) = keys.first() else {
        let y = pen_y(geometry, base_value);
        return vec![
            PenPoint { x: 0.0, y },
            PenPoint {
                x: geometry.width,
                y,
            },
        ];
    };
    let mut points = vec![PenPoint {
        x: 0.0,
        y: pen_y(geometry, first.value),
    }];
    for key in keys {
        points.push(PenPoint {
            x: key.frame as f64 * geometry.pixels_per_frame,
            y: pen_y(geometry, key.value),
        });
        for sample in &key.samples {
            points.push(PenPoint {
                x: sample.frame as f64 * geometry.pixels_per_frame,
                y: pen_y(geometry, sample.value),
            });
        }
    }
    if let Some(last) = keys.last() {
        points.push(PenPoint {
            x: geometry.width,
            y: pen_y(geometry, last.value),
        });
    }
    points
}

pub fn pen_line_y_at(points: &[PenPoint], x: f64) -> Option<f64> {
    let first = points.first()?;
    if x <= first.x {
        return Some(first.y);
    }
    for pair in points.windows(2) {
        let left = pair[0];
        let right = pair[1];
        if x <= right.x {
            if right.x <= left.x {
                return Some(right.y);
            }
            return Some(left.y + (right.y - left.y) * (x - left.x) / (right.x - left.x));
        }
    }
    points.last().map(|point| point.y)
}

/// `frame` 以上の最初のキー。`frame` は画面座標からの実数。
pub fn pen_key_lower_bound(keys: &[PenKey], frame: f64) -> usize {
    let mut left = 0;
    let mut right = keys.len();
    while left < right {
        let middle = left + (right - left) / 2;
        if (keys[middle].frame as f64) < frame {
            left = middle + 1;
        } else {
            right = middle;
        }
    }
    left
}

pub fn pen_visible_keys(
    keys: &[PenKey],
    geometry: &PenGeometry,
    left: f64,
    right: f64,
) -> Vec<PenKey> {
    if right <= left || geometry.pixels_per_frame <= 0.0 {
        return Vec::new();
    }
    let first = pen_key_lower_bound(
        keys,
        (left - geometry.key_pixels) / geometry.pixels_per_frame,
    );
    let end = pen_key_lower_bound(
        keys,
        (right + geometry.key_pixels) / geometry.pixels_per_frame,
    );
    keys[first..end].to_vec()
}

pub fn pen_visible_line_points(
    keys: &[PenKey],
    base_value: f64,
    geometry: &PenGeometry,
    mut left: f64,
    mut right: f64,
) -> Vec<PenPoint> {
    left = left.max(0.0);
    right = right.min(geometry.width);
    if right <= left || geometry.pixels_per_frame <= 0.0 {
        return Vec::new();
    }
    let first = pen_key_lower_bound(keys, left / geometry.pixels_per_frame).saturating_sub(1);
    let end = pen_key_lower_bound(keys, right / geometry.pixels_per_frame)
        .saturating_add(1)
        .min(keys.len());
    let points = pen_line_points(&keys[first..end], base_value, geometry);
    let mut visible = vec![PenPoint {
        x: left,
        y: pen_line_y_at(&points, left).unwrap_or(0.0),
    }];
    for point in &points {
        if point.x > left && point.x < right {
            visible.push(*point);
        }
    }
    visible.push(PenPoint {
        x: right,
        y: pen_line_y_at(&points, right).unwrap_or(0.0),
    });
    visible
}

pub fn pen_nearest_key<'a>(
    keys: &'a [PenKey],
    geometry: &PenGeometry,
    x: f64,
    value_percent: f64,
) -> Option<&'a PenKey> {
    if geometry.maximum <= 0.0 {
        return None;
    }
    let pixels_per_value = pen_usable_height(geometry) / geometry.maximum;
    let mut nearest = None;
    let mut nearest_distance = f64::INFINITY;
    for key in keys {
        let dx = (key.frame as f64 * geometry.pixels_per_frame - x).abs();
        let dy = (key.value - value_percent).abs() * pixels_per_value;
        if dx <= geometry.key_pixels && dy <= geometry.line_pixels && dx + dy < nearest_distance {
            nearest = Some(key);
            nearest_distance = dx + dy;
        }
    }
    nearest
}

pub fn pen_press(
    keys: &[PenKey],
    base_value: f64,
    frame: i64,
    x: f64,
    y: f64,
    geometry: &PenGeometry,
    modifiers: GestureModifiers,
) -> PenPress {
    let value_percent = pen_value_at(geometry, y);
    let nearest = pen_nearest_key(keys, geometry, x, value_percent);
    if modifiers.alt {
        return if let Some(key) = nearest {
            PenPress::DeleteKey { frame: key.frame }
        } else {
            PenPress::None
        };
    }
    let points = pen_line_points(keys, base_value, geometry);
    let near_line =
        pen_line_y_at(&points, x).is_some_and(|line_y| (line_y - y).abs() <= geometry.line_pixels);
    if nearest.is_none() && !near_line {
        return PenPress::Select;
    }
    if let Some(key) = nearest {
        PenPress::Edit {
            original_frame: Some(key.frame),
            frame: key.frame,
            value: key.value,
            grab_offset_x: key.frame as f64 * geometry.pixels_per_frame - x,
            grab_offset_y: pen_y(geometry, key.value) - y,
        }
    } else {
        PenPress::Edit {
            original_frame: None,
            frame,
            value: value_percent,
            grab_offset_x: 0.0,
            grab_offset_y: 0.0,
        }
    }
}

/// Shift を押している間は 100%（0 dB、または不透明）へ吸着する。
pub fn pen_snap_value(value_percent: f64, modifiers: GestureModifiers) -> f64 {
    if modifiers.shift {
        100.0
    } else {
        value_percent
    }
}

pub fn pen_release(state: &PenPress, frame: i64, value_percent: f64) -> Option<PenRelease> {
    let PenPress::Edit { original_frame, .. } = state else {
        return None;
    };
    Some(PenRelease {
        original_frame: *original_frame,
        frame,
        value: value_percent,
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    fn mods(alt: bool, shift: bool) -> GestureModifiers {
        GestureModifiers { alt, shift }
    }

    fn video_geometry(pixels_per_frame: f64) -> PenGeometry {
        PenGeometry {
            pixels_per_frame,
            maximum: 100.0,
            width: 800.0,
            height: 54.0,
            inset: 4.0,
            key_pixels: 8.0,
            line_pixels: 12.0,
        }
    }

    fn key(frame: i64, value: f64) -> PenKey {
        PenKey {
            frame,
            value,
            samples: Vec::new(),
        }
    }

    #[test]
    fn ruler_marker_hit_is_the_nearest_inside_the_radius() {
        let markers = [10, 100, 110];
        assert_eq!(marker_near_ruler_x(201.0, 2.0, &markers, 9.0), Some(100));
        assert_eq!(marker_near_ruler_x(212.0, 2.0, &markers, 9.0), Some(110));
        assert_eq!(marker_near_ruler_x(180.0, 2.0, &markers, 9.0), None);
        assert_eq!(marker_near_ruler_x(200.0, 2.0, &[], 9.0), None);
    }

    #[test]
    fn group_drag_stops_at_the_earliest_clip() {
        let bounds = DragBounds {
            min_start_frame: Some(0),
            video: Some((0, 1)),
            ..DragBounds::default()
        };
        assert_eq!(group_drag_offset_x(-200.0, 2.0, bounds), 0.0);
        assert_eq!(group_drag_offset_x(80.0, 2.0, bounds), 80.0);
        let later = DragBounds {
            min_start_frame: Some(30),
            ..DragBounds::default()
        };
        assert_eq!(group_drag_offset_x(-200.0, 2.0, later), -60.0);
        assert_eq!(
            group_drag_offset_x(-200.0, 2.0, DragBounds::default()),
            -200.0
        );
    }

    #[test]
    fn group_drag_track_stays_inside_existing_tracks() {
        let bounds = DragBounds {
            video: Some((0, 1)),
            ..DragBounds::default()
        };
        assert_eq!(group_drag_track_index(true, 1, 0, 3, bounds), 1);
        assert_eq!(group_drag_track_index(true, 1, 2, 3, bounds), 2);
        assert_eq!(group_drag_track_index(true, 0, 2, 3, bounds), 1);
        assert_eq!(group_drag_track_index(false, 0, 1, 2, bounds), 1);
    }

    #[test]
    fn razor_splits_at_the_press_frame_even_if_the_pointer_moves() {
        let state = body_press(TimelineTool::Razor, mods(false, false), 150);
        let action = body_release(&state, false, 100, 153, 0);
        assert_eq!(
            action,
            BodyAction::Split {
                frame: 150,
                all_tracks: false,
                linked: true,
            }
        );
    }

    #[test]
    fn razor_modifiers_stay_as_they_were_at_press() {
        let all_tracks = body_press(TimelineTool::Razor, mods(false, true), 40);
        assert!(matches!(
            body_release(&all_tracks, false, 0, 40, 0),
            BodyAction::Split {
                all_tracks: true,
                ..
            }
        ));
        let single = body_press(TimelineTool::Razor, mods(true, false), 40);
        assert!(matches!(
            body_release(&single, false, 0, 40, 0),
            BodyAction::Split { linked: false, .. }
        ));
    }

    #[test]
    fn move_uses_the_drag_target_and_alt_click_stays_a_selection() {
        let linked = body_press(TimelineTool::Select, mods(false, false), 10);
        assert_eq!(
            body_release(&linked, true, 240, 12, 0),
            BodyAction::Move {
                frame: 240,
                linked: true,
            }
        );
        let single = body_press(TimelineTool::Select, mods(true, false), 10);
        assert_eq!(
            body_release(&single, true, 240, 12, 0),
            BodyAction::Duplicate {
                frame: 240,
                linked: false,
            }
        );
        assert_eq!(
            body_release(&single, false, 10, 12, 0),
            BodyAction::Select {
                frame: 12,
                linked: false,
            }
        );
    }

    #[test]
    fn click_selects_at_the_release_frame_and_shift_toggles() {
        let state = body_press(TimelineTool::Ripple, mods(false, false), 10);
        assert_eq!(
            body_release(&state, false, 0, 12, 0),
            BodyAction::Select {
                frame: 12,
                linked: true,
            }
        );
        let additive = body_press(TimelineTool::Select, mods(false, true), 10);
        assert_eq!(
            body_release(&additive, false, 0, 12, 0),
            BodyAction::Toggle { frame: 12 }
        );
    }

    #[test]
    fn track_select_release_does_not_collapse_the_selection() {
        let state = body_press(TimelineTool::TrackForward, mods(false, true), 10);
        assert_eq!(state.gesture, BodyGesture::TrackSelect);
        assert!(!state.additive);
        assert_eq!(body_release(&state, false, 0, 12, 0), BodyAction::None);
        assert_eq!(
            body_release(&state, true, 80, 12, 0),
            BodyAction::Move {
                frame: 80,
                linked: true,
            }
        );
    }

    #[test]
    fn slip_and_slide_use_the_drag_frame_count() {
        let slip = body_press(TimelineTool::Slip, mods(true, false), 10);
        assert_eq!(
            body_release(&slip, false, 0, 10, -7),
            BodyAction::Slip {
                delta: -7,
                linked: false,
            }
        );
        let slide = body_press(TimelineTool::Slide, mods(false, false), 10);
        assert_eq!(body_release(&slide, false, 0, 10, 0), BodyAction::None);
        assert_eq!(
            body_release(&slide, false, 0, 10, 5),
            BodyAction::Slide {
                delta: 5,
                linked: true,
            }
        );
    }

    #[test]
    fn drag_snaps_edges_within_the_threshold_and_ignores_its_own_edges() {
        let spans = vec![
            ClipSpan {
                clip_id: "moving".to_owned(),
                start: 100,
                end: 160,
            },
            ClipSpan {
                clip_id: "other".to_owned(),
                start: 300,
                end: 420,
            },
        ];
        let snap = drag_snap_frames(&spans, &["moving"], 250);
        assert_eq!(snap.moving, vec![100, 160]);
        assert_eq!(snap.targets, vec![0, 250, 300, 420]);
        let near = snap_drag_offset_x(275.0, 2.0, &snap, 8.0);
        assert_eq!(near.offset_x, 280.0);
        assert_eq!(near.frame, Some(300));
        let far = snap_drag_offset_x(270.0, 2.0, &snap, 8.0);
        assert_eq!(far.offset_x, 270.0);
        assert_eq!(far.frame, None);
        let playhead = snap_drag_offset_x(297.0, 2.0, &snap, 8.0);
        assert_eq!(playhead.offset_x, 300.0);
        assert_eq!(playhead.frame, Some(250));
        assert_eq!(snap_drag_offset_x(3.0, 2.0, &snap, 8.0).frame, None);
    }

    #[test]
    fn edge_release_follows_the_tool_and_a_still_rate_edge_does_nothing() {
        assert!(matches!(
            edge_release(TimelineTool::Select, TrimEdge::Left, 3, true),
            EdgeAction::Trim { delta: 3, .. }
        ));
        assert!(matches!(
            edge_release(TimelineTool::Ripple, TrimEdge::Right, -4, true),
            EdgeAction::RippleTrim { delta: -4, .. }
        ));
        assert_eq!(
            edge_release(TimelineTool::Rolling, TrimEdge::Right, 2, false),
            EdgeAction::Roll {
                edge: TrimEdge::Right,
                delta: 2,
                linked: false,
            }
        );
        assert_eq!(
            edge_release(TimelineTool::Ripple, TrimEdge::Left, 0, true),
            EdgeAction::SelectEdit {
                edge: TrimEdge::Left
            }
        );
        assert!(matches!(
            edge_release(TimelineTool::Select, TrimEdge::Right, 0, true),
            EdgeAction::SelectEdit { .. }
        ));
        assert!(matches!(
            edge_release(TimelineTool::Rolling, TrimEdge::Right, 0, true),
            EdgeAction::SelectEdit { .. }
        ));
        assert_eq!(
            edge_release(TimelineTool::Rate, TrimEdge::Left, 0, true),
            EdgeAction::None
        );
        assert_eq!(
            edge_release(TimelineTool::Rate, TrimEdge::Left, -5, true),
            EdgeAction::RateStretch {
                edge: TrimEdge::Left,
                delta: -5,
                linked: true,
            }
        );
    }

    #[test]
    fn pen_value_and_y_round_trip_and_clamp_outside_the_inset() {
        let video = video_geometry(4.0);
        assert_eq!(pen_y(&video, 100.0), 4.0);
        assert_eq!(pen_y(&video, 0.0), 50.0);
        for value in [0.0, 25.0, 50.0, 93.0, 100.0] {
            assert_eq!(pen_value_at(&video, pen_y(&video, value)), value);
        }
        for value in [0.0, 50.0, 100.0] {
            assert_eq!(pen_value_at(&video, pen_y(&video, value).round()), value);
        }
        let mut audio = video_geometry(4.0);
        audio.maximum = 200.0;
        for value in [0.0, 100.0, 200.0] {
            assert_eq!(pen_value_at(&audio, pen_y(&audio, value)), value);
        }
        assert_eq!(pen_value_at(&video, 0.0), 100.0);
        assert_eq!(pen_value_at(&video, 54.0), 0.0);
        assert_eq!(pen_value_at(&audio, -10.0), 200.0);
        let flat = PenGeometry {
            height: 4.0,
            ..video
        };
        assert!(pen_value_at(&flat, 2.0).is_finite());
    }

    #[test]
    fn pen_line_click_adds_a_key_and_a_far_click_selects() {
        let video = video_geometry(4.0);
        let added = pen_press(&[], 100.0, 20, 80.0, 10.0, &video, mods(false, false));
        assert_eq!(
            added,
            PenPress::Edit {
                original_frame: None,
                frame: 20,
                value: 87.0,
                grab_offset_x: 0.0,
                grab_offset_y: 0.0,
            }
        );
        assert!(matches!(
            pen_press(&[], 100.0, 20, 80.0, 16.0, &video, mods(false, false)),
            PenPress::Edit { .. }
        ));
        assert_eq!(
            pen_press(&[], 100.0, 20, 80.0, 17.0, &video, mods(false, false)),
            PenPress::Select
        );
    }

    #[test]
    fn pen_line_holds_the_end_values_outside_the_keys() {
        let video = video_geometry(4.0);
        let flat = pen_line_points(&[], 50.0, &video);
        assert_eq!(flat.len(), 2);
        assert_eq!(flat[0].x, 0.0);
        assert_eq!(flat[1].x, 800.0);
        assert_eq!(flat[0].y, pen_y(&video, 50.0));
        let points = pen_line_points(&[key(10, 0.0), key(20, 100.0)], 50.0, &video);
        assert_eq!(points.len(), 4);
        assert_eq!(points[0].y, pen_y(&video, 0.0));
        assert_eq!(points[1].x, 40.0);
        assert_eq!(points[3].x, 800.0);
        assert_eq!(points[3].y, pen_y(&video, 100.0));
        assert_eq!(pen_line_y_at(&points, 20.0), Some(pen_y(&video, 0.0)));
        assert_eq!(pen_line_y_at(&points, 60.0), Some(pen_y(&video, 50.0)));
        assert_eq!(pen_line_y_at(&points, 900.0), Some(pen_y(&video, 100.0)));
    }

    #[test]
    fn visible_automation_keeps_the_slope_and_the_sample() {
        let mut geometry = video_geometry(1.0);
        geometry.width = 800.0;
        let keys = [
            PenKey {
                frame: 10,
                value: 0.0,
                samples: vec![PenSample {
                    frame: 15,
                    value: 100.0,
                }],
            },
            key(20, 0.0),
        ];
        let points = pen_visible_line_points(&keys, 50.0, &geometry, 12.0, 18.0);
        assert_eq!(points.len(), 3);
        assert_eq!(points[0].x, 12.0);
        assert_eq!(points[1].x, 15.0);
        assert_eq!(points[1].y, 4.0);
        assert_eq!(points[2].x, 18.0);
        assert!((points[0].y - 31.6).abs() < 0.000_001);
        assert!((points[2].y - 31.6).abs() < 0.000_001);
        assert!(pen_visible_line_points(&keys, 50.0, &geometry, 18.0, 12.0).is_empty());
        assert!(pen_visible_keys(&keys, &geometry, 900.0, 1000.0).is_empty());
        let flat = pen_visible_line_points(&[], 50.0, &geometry, 200.0, 300.0);
        assert_eq!(flat.len(), 2);
        assert_eq!(flat[0].x, 200.0);
        assert_eq!(flat[1].x, 300.0);
        assert_eq!(flat[0].y, 27.0);
    }

    #[test]
    fn long_automation_draws_only_the_visible_range() {
        let mut geometry = video_geometry(0.5);
        geometry.maximum = 200.0;
        geometry.width = 150_000.0;
        let keys: Vec<PenKey> = (0..600)
            .map(|index| key(index * 500, if index % 2 == 1 { 40.0 } else { 100.0 }))
            .collect();
        let visible = pen_visible_keys(&keys, &geometry, 70_000.0, 70_900.0);
        assert!(!visible.is_empty() && visible.len() <= 5);
        let points = pen_visible_line_points(&keys, 100.0, &geometry, 70_000.0, 70_900.0);
        assert!(points.len() <= 6);
        assert_eq!(points[0].x, 70_000.0);
        assert_eq!(points.last().unwrap().x, 70_900.0);
        geometry.pixels_per_frame = 0.002;
        geometry.width = 600.0;
        assert_eq!(
            pen_visible_keys(&keys, &geometry, 0.0, 600.0).len(),
            keys.len()
        );
        assert_eq!(keys.len(), 600);
    }

    #[test]
    fn pen_hit_follows_the_sloped_line_between_frames() {
        let zoomed = video_geometry(32.0);
        let keys = [key(10, 0.0), key(11, 100.0)];
        let y = pen_y(&zoomed, 50.0);
        assert_eq!(
            pen_line_y_at(&pen_line_points(&keys, 100.0, &zoomed), 336.0),
            Some(y)
        );
        let added = pen_press(&keys, 100.0, 11, 336.0, y, &zoomed, mods(false, false));
        assert_eq!(
            added,
            PenPress::Edit {
                original_frame: None,
                frame: 11,
                value: 50.0,
                grab_offset_x: 0.0,
                grab_offset_y: 0.0,
            }
        );
        assert_eq!(
            pen_press(
                &keys,
                100.0,
                11,
                336.0,
                y + 13.0,
                &zoomed,
                mods(false, false)
            ),
            PenPress::Select
        );
    }

    #[test]
    fn pen_drag_keeps_the_grabbed_value_until_the_pointer_moves() {
        let video = video_geometry(4.0);
        let keys = [key(10, 80.0), key(30, 20.0)];
        let pressed = pen_press(&keys, 80.0, 11, 43.0, 15.0, &video, mods(false, false));
        let PenPress::Edit {
            original_frame,
            frame,
            value,
            grab_offset_x,
            grab_offset_y,
        } = pressed
        else {
            panic!("キーを掴めていません");
        };
        assert_eq!(original_frame, Some(10));
        assert_eq!(frame, 10);
        assert_eq!(value, 80.0);
        assert_eq!(grab_offset_x, -3.0);
        assert!((grab_offset_y - (pen_y(&video, 80.0) - 15.0)).abs() < 1e-9);
        assert_eq!(pen_value_at(&video, 15.0 + grab_offset_y), 80.0);
        let moved = pen_release(&pressed, 19, 40.0).unwrap();
        assert_eq!(moved.original_frame, Some(10));
        assert_eq!(moved.frame, 19);
        assert_eq!(moved.value, 40.0);
    }

    #[test]
    fn pen_picks_the_nearest_key_inside_the_pixel_box() {
        let video = video_geometry(4.0);
        let keys = [key(10, 50.0), key(12, 50.0)];
        let y = pen_y(&video, 50.0);
        assert_eq!(
            pen_nearest_key(&keys, &video, 45.0, 50.0).map(|key| key.frame),
            Some(12)
        );
        assert_eq!(
            pen_nearest_key(&keys, &video, 43.0, 50.0).map(|key| key.frame),
            Some(10)
        );
        assert!(matches!(
            pen_press(&keys, 50.0, 11, 45.0, y, &video, mods(false, false)),
            PenPress::Edit {
                original_frame: Some(12),
                ..
            }
        ));
        assert!(pen_nearest_key(&keys, &video, 40.0, pen_value_at(&video, y + 13.0)).is_none());
    }

    #[test]
    fn pen_key_hit_stays_within_pixels_at_high_zoom() {
        let zoomed = video_geometry(32.0);
        let keys = [key(10, 50.0)];
        let y = pen_y(&zoomed, 50.0);
        assert_eq!(
            pen_nearest_key(&keys, &zoomed, 326.0, 50.0).map(|key| key.frame),
            Some(10)
        );
        assert!(pen_nearest_key(&keys, &zoomed, 329.0, 50.0).is_none());
        assert!(pen_nearest_key(&keys, &zoomed, 352.0, 50.0).is_none());
        assert_eq!(
            pen_press(&keys, 50.0, 11, 352.0, y, &zoomed, mods(true, false)),
            PenPress::None
        );
        assert_eq!(
            pen_press(&keys, 50.0, 10, 324.0, y, &zoomed, mods(true, false)),
            PenPress::DeleteKey { frame: 10 }
        );
    }

    #[test]
    fn pen_alt_deletes_only_a_key_and_shift_snaps_to_100() {
        let video = video_geometry(4.0);
        let keys = [key(10, 80.0)];
        let key_y = pen_y(&video, 80.0);
        assert_eq!(
            pen_press(
                &keys,
                80.0,
                11,
                42.0,
                key_y + 2.0,
                &video,
                mods(true, false)
            ),
            PenPress::DeleteKey { frame: 10 }
        );
        assert_eq!(
            pen_press(&keys, 80.0, 30, 120.0, key_y, &video, mods(true, false)),
            PenPress::None
        );
        assert_eq!(
            pen_press(&keys, 80.0, 30, 120.0, 50.0, &video, mods(true, false)),
            PenPress::None
        );
        assert!(matches!(
            pen_press(&keys, 80.0, 11, 42.0, key_y, &video, mods(false, false)),
            PenPress::Edit { .. }
        ));
        assert_eq!(pen_snap_value(143.0, mods(false, true)), 100.0);
        assert_eq!(pen_snap_value(143.0, mods(true, true)), 100.0);
        assert_eq!(pen_snap_value(143.0, mods(false, false)), 143.0);
    }
}
