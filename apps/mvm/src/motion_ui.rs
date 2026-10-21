//! エフェクトコントロールの文字の書式・キーフレーム（モーション・クロップ・不透明度・音量）・フェード。
//! 見た目と操作は C++ 版の TextClipInspector / TextStyleEditor / KeyframeInspector に揃える。
//!
//! 値は再生ヘッドの位置（clip の外なら先頭）で評価して見せる。数値はドラッグを離すか入力を確定したときに
//! 1 回だけ commit する。キーのある channel を変えると、その位置のキーを置き換える。
//!
//! ドラッグを始めたときの clip・channel・frame・revision を覚え、途中で再生ヘッドが動いても始めた frame へ
//! 書く。途中で Project が変われば app が確定を断る。

use eframe::egui::{self, Align2, Color32, FontId, Pos2, Rect, Sense, Stroke, Vec2};
use mvm_app::Session;
use mvm_project::clip_effects::{ClipKeyKind, ClipKeyframe, KeyInterpolation, evaluate_clip_keys};
use mvm_project::timeline::clip_interval;
use mvm_project::{TextClipData, TimelineClip, TimelineClipKind};

use crate::text_style_ui::{self, StyleChange, StyleValues};
use crate::widgets::{self, NumberEdit, NumberField};
use crate::{icons, theme};

macro_rules! report {
    ($session:ident, $call:expr $(,)?) => {{
        let result = $call;
        if let Err(error) = result {
            $session.set_alert(error.message());
        }
    }};
}

/// 映像系の clip に出す channel と表示名・単位。並びは C++ 版と同じ。
const VISUAL_ROWS: [(ClipKeyKind, &str, &str); 10] = [
    (ClipKeyKind::PositionX, "位置 X", " %"),
    (ClipKeyKind::PositionY, "位置 Y", " %"),
    (ClipKeyKind::ScaleX, "拡大率 X", " %"),
    (ClipKeyKind::ScaleY, "拡大率 Y", " %"),
    (ClipKeyKind::Rotation, "回転", " °"),
    (ClipKeyKind::CropLeft, "クロップ 左", " %"),
    (ClipKeyKind::CropRight, "クロップ 右", " %"),
    (ClipKeyKind::CropTop, "クロップ 上", " %"),
    (ClipKeyKind::CropBottom, "クロップ 下", " %"),
    (ClipKeyKind::Opacity, "不透明度", " %"),
];

const AUDIO_ROWS: [(ClipKeyKind, &str, &str); 2] = [
    (ClipKeyKind::Volume, "音量", " %"),
    (ClipKeyKind::Ducking, "ダッキング", " dB"),
];

const CHANNELS: usize = 12;
const BUTTON: f32 = 24.0;
const LANE_HEIGHT: f32 = 18.0;
const GRAPH_HEIGHT: f32 = 120.0;
/// 値の欄の左に置く channel 名の幅。
const VALUE_LABEL_WIDTH: f32 = 65.0;

/// ドラッグで編集中の effect の値と、編集を始めたときの対象。
#[derive(Clone, Debug, PartialEq)]
struct EffectDraft {
    clip_id: String,
    kind: ClipKeyKind,
    frame: i64,
    revision: u64,
    /// 編集を始めたときの値。これと変わらなければ確定しない。
    original_value: f64,
}

/// キーの列・グラフでドラッグしているキー。離したときに 1 回だけ確定する。始めたときの clip と
/// [`Session::edit_revision`] を覚え、途中で Project が変われば（undo・別の編集）preview も確定もしない。
#[derive(Clone, Debug, PartialEq)]
struct KeyDrag {
    clip_id: String,
    revision: u64,
    kind: ClipKeyKind,
    original_frame: i64,
    frame: i64,
    value: f64,
    /// グラフの縦の範囲。ドラッグ中は固定する（動かすたびに範囲が変わると key が逃げる）。
    low: f64,
    high: f64,
    /// スプラインのハンドル（0 か 1）を動かしているとき。
    handle: Option<usize>,
    moved: bool,
}

/// 選んだ channel と、その channel の中で選んだキーの frame。
#[derive(Clone, Debug, Default, PartialEq)]
struct KeySelection {
    kind: Option<ClipKeyKind>,
    frames: Vec<i64>,
}

impl KeySelection {
    fn contains(&self, kind: ClipKeyKind, frame: i64) -> bool {
        self.kind == Some(kind) && self.frames.contains(&frame)
    }

    /// グラフのハンドルを出す、最初に選んだキー。
    fn primary(&self, kind: ClipKeyKind) -> Option<(ClipKeyKind, i64)> {
        (self.kind == Some(kind))
            .then(|| self.frames.first().map(|frame| (kind, *frame)))
            .flatten()
    }

    /// キーを押した。Ctrl なら同じ channel の選択に足し引きし、そうでなければそれだけを選ぶ。
    fn press(&mut self, kind: ClipKeyKind, frame: i64, toggle: bool) {
        if toggle && self.kind == Some(kind) {
            if let Some(position) = self.frames.iter().position(|selected| *selected == frame) {
                self.frames.remove(position);
            } else {
                self.frames.push(frame);
            }
        } else {
            self.kind = Some(kind);
            self.frames = vec![frame];
        }
    }

    fn set(&mut self, kind: ClipKeyKind, frames: Vec<i64>) {
        self.kind = Some(kind);
        self.frames = frames;
    }
}

/// グラフで引いている選択の枠。`base` は始めたときの選択（Ctrl で足すとき）。
#[derive(Clone, Debug, PartialEq)]
struct Marquee {
    kind: ClipKeyKind,
    origin: Pos2,
    current: Pos2,
    base: Vec<i64>,
}

/// 表示の値の丸め以内の差は変更とみなさない（欄は整数で見せる）。
const UNCHANGED: f64 = 0.005;

pub struct MotionState {
    clip_id: Option<String>,
    revision: u64,
    frame: i64,
    /// 再生ヘッドの位置で評価した値（表示用）。
    values: [f64; CHANNELS],
    draft: Option<EffectDraft>,
    lock_aspect: bool,
    graph_open: [bool; CHANNELS],
    /// 選んだ channel とキー。Ctrl で足し引き、グラフの枠で囲んで選ぶ。
    selection: KeySelection,
    /// グラフで枠を引いて選んでいる途中。
    marquee: Option<Marquee>,
    /// キーを押した後は、Ctrl+C / X / V と Delete をキーへ向ける（clip へは向けない）。
    key_focus: bool,
    key_drag: Option<KeyDrag>,
    /// 文字 clip の本文の下書き。欄を抜けたときに確定する。
    content: String,
    /// clip を選び直すたびに増やし、欄の id に含める。入力途中の欄を別の clip・戻った clip へ持ち越さない。
    epoch: u64,
    scrubbing: bool,
}

impl Default for MotionState {
    fn default() -> Self {
        Self {
            clip_id: None,
            revision: 0,
            frame: 0,
            values: [0.0; CHANNELS],
            draft: None,
            lock_aspect: true,
            graph_open: [false; CHANNELS],
            selection: KeySelection::default(),
            marquee: None,
            key_focus: false,
            key_drag: None,
            content: String::new(),
            epoch: 0,
            scrubbing: false,
        }
    }
}

impl MotionState {
    /// clip を選んでいないとき。編集中の下書きを捨てる。
    pub fn forget(&mut self) {
        self.draft = None;
        self.key_drag = None;
    }

    /// Project・再生ヘッド・clip が変わったら、表示する値を Project から読み直す。
    fn load(&mut self, session: &Session, clip: &TimelineClip) {
        if self.clip_id.as_deref() != Some(clip.id.as_str()) {
            // 別の clip の下書きは捨てる（戻ってきたときに古い未確定の値を出さない）。
            self.draft = None;
            self.key_drag = None;
            self.selection = KeySelection::default();
            self.marquee = None;
            self.key_focus = false;
            self.epoch += 1;
        }
        let frame = session.effect_frame(&clip.id).unwrap_or(0);
        let same = self.clip_id.as_deref() == Some(clip.id.as_str())
            && self.revision == session.edit_revision();
        if same && self.frame == frame {
            return;
        }
        self.frame = frame;
        for (index, value) in self.values.iter_mut().enumerate() {
            if let Some(kind) = kind_at(index) {
                *value =
                    evaluate_clip_keys(clip.effects.keys(kind), clip.effects.base(kind), frame);
            }
        }
        if same {
            return;
        }
        self.clip_id = Some(clip.id.clone());
        self.revision = session.edit_revision();
        self.content = clip.text.content.clone();
    }
}

impl MotionState {
    /// `kind` でドラッグしているキー。始めた後に Project や clip が変わったドラッグは返さない。
    fn live_drag(&self, kind: ClipKeyKind) -> Option<&KeyDrag> {
        self.key_drag.as_ref().filter(|drag| {
            drag.kind == kind
                && drag.revision == self.revision
                && self.clip_id.as_deref() == Some(drag.clip_id.as_str())
        })
    }

    pub fn wants_key_shortcuts(&self) -> bool {
        self.key_focus && self.selection.kind.is_some()
    }
}

fn kind_at(index: usize) -> Option<ClipKeyKind> {
    mvm_project::clip_effects::EFFECT_CHANNELS
        .get(index)
        .map(|channel| channel.kind)
}

/// 文字 clip の書式（C++ 版の TextClipInspector）。
pub fn show_text(
    ui: &mut egui::Ui,
    session: &mut Session,
    state: &mut MotionState,
    clip: &TimelineClip,
) {
    state.load(session, clip);
    let id = egui::Id::new(("text_clip", &clip.id, state.epoch));
    let text = &clip.text;
    let mut edited: Option<TextClipData> = None;
    let mut edit = |change: &dyn Fn(&mut TextClipData)| {
        let mut next = edited.clone().unwrap_or_else(|| text.clone());
        change(&mut next);
        edited = Some(next);
    };

    // 本文。枠で入力範囲を示す。欄を抜けるか Ctrl+Enter で確定、Esc で取り消す。
    let lines = state.content.lines().count().max(1) as f32;
    let height = (lines * 18.0 + 12.0).clamp(44.0, 96.0);
    // Ctrl+Enter と Esc は TextEdit より先に取る（TextEdit は Enter で改行を入れる）。
    let editor_id = id.with("content");
    let (submit, cancel) = if ui.memory(|memory| memory.has_focus(editor_id)) {
        ui.input_mut(|input| {
            (
                input.consume_key(egui::Modifiers::COMMAND, egui::Key::Enter),
                input.consume_key(egui::Modifiers::NONE, egui::Key::Escape),
            )
        })
    } else {
        (false, false)
    };
    if cancel {
        state.content = text.content.clone();
    }
    let editor = egui::TextEdit::multiline(&mut state.content)
        .id(editor_id)
        .font(FontId::proportional(13.0))
        .hint_text("文字を入力")
        .margin(Vec2::splat(6.0))
        .desired_rows(1)
        .desired_width(f32::INFINITY);
    let response = ui.add_sized(Vec2::new(ui.available_width(), height), editor);
    if submit || cancel {
        response.surrender_focus();
    }
    if (submit || response.lost_focus())
        && state.content != text.content
        && !state.content.is_empty()
    {
        let content = state.content.clone();
        edit(&|next| next.content = content.clone());
    }

    ui.add_space(2.0);
    let values = StyleValues {
        font_family: &text.font_family,
        font_size: text.font_size,
        bold: text.bold,
        color: &text.color,
        outline_color: &text.outline_color,
        outline_width: text.outline_width,
        background_color: &text.background_color,
    };
    let output = (
        session.project().output_width,
        session.project().output_height,
    );
    let mut place = None;
    let mut position: Vec<(&str, NumberEdit)> = Vec::new();
    let mut style = text_style_ui::show(
        ui,
        id,
        &values,
        output.1,
        |ui| {
            // テロップの定位置へ置くボタン。押すたびに下から 15% の高さの左・中央・右へ置き直し、行揃えも
            // 同じ向きにする。置いた後は X / Y やドラッグで微調整できる。
            for (alignment, tip) in [
                ("left", "テロップ位置 (左下) へ置く"),
                ("center", "テロップ位置 (中央下) へ置く"),
                ("right", "テロップ位置 (右下) へ置く"),
            ] {
                if widgets::placement_button(ui, alignment, tip).clicked() {
                    place = Some(alignment);
                }
            }
        },
        |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                let half = ((ui.available_width() - 6.0) / 2.0).max(40.0);
                for (key, label, value, maximum) in
                    [("x", "X", text.x, output.0), ("y", "Y", text.y, output.1)]
                {
                    let edit_value = NumberField {
                        label,
                        inline_label: 20.0,
                        value: f64::from(value),
                        minimum: 0.0,
                        maximum: f64::from((maximum - 1).max(0)),
                        step: 1.0,
                        width: half,
                        ..NumberField::default()
                    }
                    .show(ui, id.with(key));
                    if let Some(edit_value) = edit_value {
                        position.push((key, edit_value));
                    }
                }
            });
        },
    );
    if let Some(alignment) = place {
        report!(session, session.place_text_preset(&clip.id, alignment));
    }
    // X / Y も書式と同じく、ドラッグ中は preview だけ、離したら確定する。
    let mut previews: Vec<TextPreview> = Vec::new();
    for (key, edit_value) in position {
        let setter = move |next: &mut TextClipData, value: i32| {
            if key == "x" {
                next.x = value;
            } else {
                next.y = value;
            }
        };
        match edit_value {
            NumberEdit::Commit(value) => edit(&|next| setter(next, value as i32)),
            NumberEdit::Preview(value) => {
                previews.push(Box::new(move |next| setter(next, value as i32)));
            }
            NumberEdit::Cancel => style.cancel = true,
            NumberEdit::Start => {}
        }
    }
    for change in &style.commits {
        edit(&|next| apply_style(next, change));
    }
    if let Some(next) = edited
        && next != *text
    {
        report!(session, session.edit_text(&clip.id, next));
    } else if !style.previews.is_empty() || !previews.is_empty() {
        let mut next = text.clone();
        for change in &style.previews {
            apply_style(&mut next, change);
        }
        for preview in &previews {
            preview(&mut next);
        }
        report!(session, session.preview_text(&clip.id, next));
    } else if style.cancel {
        session.cancel_preview();
    }
}

/// X / Y をドラッグしている途中の値を、preview する文字 clip へ当てる。
type TextPreview = Box<dyn Fn(&mut TextClipData)>;

/// 書式の変更を文字 clip の値へ当てる。
fn apply_style(text: &mut TextClipData, change: &StyleChange) {
    match change {
        StyleChange::FontFamily(value) => text.font_family = value.clone(),
        StyleChange::FontSize(value) => text.font_size = *value,
        StyleChange::Bold(value) => text.bold = *value,
        StyleChange::Color(value) => text.color = value.clone(),
        StyleChange::OutlineColor(value) => text.outline_color = value.clone(),
        StyleChange::OutlineWidth(value) => text.outline_width = *value,
        StyleChange::BackgroundColor(value) => text.background_color = value.clone(),
    }
}

/// キーフレームの行と、縦横比の固定・フェード（C++ 版の KeyframeInspector と下の欄）。
pub fn show(
    ui: &mut egui::Ui,
    session: &mut Session,
    state: &mut MotionState,
    clip: &TimelineClip,
) {
    state.load(session, clip);
    // どこかを押したらキーへのキーボード操作をやめる（キーの列・グラフを押したなら、その中で戻す）。
    if ui.input(|input| input.pointer.primary_pressed() || input.pointer.secondary_pressed()) {
        state.key_focus = false;
    } else {
        key_shortcuts(ui, session, state, clip);
    }
    let audio = clip.kind == TimelineClipKind::Audio;
    let rows: &[(ClipKeyKind, &str, &str)] = if audio { &AUDIO_ROWS } else { &VISUAL_ROWS };
    let duration = clip_interval(session.project(), clip)
        .map(|(start, end)| end - start)
        .unwrap_or(1)
        .max(1);
    ui.spacing_mut().item_spacing.y = 2.0;
    for (kind, label, suffix) in rows {
        effect_row(ui, session, state, clip, *kind, label, suffix, duration);
    }
    if !audio {
        ui.add_space(2.0);
        ui.checkbox(&mut state.lock_aspect, "縦横比を固定");
    }
    ui.add_space(4.0);
    fade_rows(ui, session, clip);
}

#[allow(clippy::too_many_arguments)]
fn effect_row(
    ui: &mut egui::Ui,
    session: &mut Session,
    state: &mut MotionState,
    clip: &TimelineClip,
    kind: ClipKeyKind,
    label: &str,
    suffix: &str,
    duration: i64,
) {
    let channel = kind.channel();
    let keys = clip.effects.keys(kind);
    let animated = !keys.is_empty();
    let at_key = keys.iter().any(|key| key.frame == state.frame);
    let index = kind as usize;
    let id = egui::Id::new(("effect_row", &clip.id, index, state.epoch));
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 2.0;
        let stopwatch = key_button(
            ui,
            Vec2::splat(BUTTON),
            &format!("{label} のアニメーション"),
            animated,
            true,
            |painter, rect| {
                icons::paint_stopwatch(
                    painter,
                    rect,
                    if animated {
                        theme::KEY_ACCENT
                    } else {
                        theme::KEY_ICON
                    },
                );
            },
        );
        if stopwatch
            .on_hover_text("アニメーションを切り替える")
            .clicked()
        {
            report!(
                session,
                session.set_key_animation(&clip.id, kind, !animated)
            );
        }
        if animated {
            let open = state.graph_open[index];
            let chevron = key_button(
                ui,
                Vec2::new(18.0, BUTTON),
                &format!("{label} のカーブ"),
                false,
                true,
                |painter, rect| {
                    icons::paint_chevron(
                        painter,
                        Rect::from_center_size(rect.center(), Vec2::splat(11.0)),
                        if open {
                            icons::Chevron::Down
                        } else {
                            icons::Chevron::Right
                        },
                        theme::TEXT_ICON,
                    );
                },
            );
            if chevron.on_hover_text("カーブを表示・編集").clicked() {
                state.graph_open[index] = !open;
            }
        }
        let shown = state.values.get(index).copied().unwrap_or(0.0);
        let field_width = (ui.available_width() - 3.0 * (BUTTON + 2.0)).max(80.0);
        let edit = NumberField {
            label,
            inline_label: VALUE_LABEL_WIDTH,
            value: shown,
            minimum: channel.minimum,
            maximum: channel.maximum,
            step: 0.5,
            suffix,
            click_to_edit: true,
            width: field_width,
            ..NumberField::default()
        }
        .show(ui, id.with("value"));
        handle_value_edit(session, state, clip, kind, shown, edit);
        let previous = key_button(
            ui,
            Vec2::splat(BUTTON),
            &format!("{label} の前のキー"),
            false,
            animated,
            |painter, rect| {
                icons::paint_chevron(
                    painter,
                    Rect::from_center_size(rect.center(), Vec2::splat(11.0)),
                    icons::Chevron::Left,
                    if animated {
                        theme::TEXT_ICON
                    } else {
                        theme::TEXT_DISABLED
                    },
                );
            },
        );
        if previous.on_hover_text("前のキーへ移動").clicked() && animated {
            session.seek_key(&clip.id, kind, false);
        }
        let toggle = key_button(
            ui,
            Vec2::splat(BUTTON),
            &format!("{label} のキー"),
            at_key,
            true,
            |painter, rect| {
                icons::paint_diamond(painter, rect.center(), 5.5, at_key, theme::TEXT);
            },
        );
        if toggle.on_hover_text("現在位置のキーを追加・削除").clicked() {
            report!(session, session.toggle_key(&clip.id, kind));
        }
        let next = key_button(
            ui,
            Vec2::splat(BUTTON),
            &format!("{label} の次のキー"),
            false,
            animated,
            |painter, rect| {
                icons::paint_chevron(
                    painter,
                    Rect::from_center_size(rect.center(), Vec2::splat(11.0)),
                    icons::Chevron::Right,
                    if animated {
                        theme::TEXT_ICON
                    } else {
                        theme::TEXT_DISABLED
                    },
                );
            },
        );
        if next.on_hover_text("次のキーへ移動").clicked() && animated {
            session.seek_key(&clip.id, kind, true);
        }
    });
    if animated {
        key_lane(ui, session, state, clip, kind, duration);
        if state.graph_open[index] {
            key_graph(ui, session, state, clip, kind, duration);
        }
    }
}

/// 値の欄のドラッグ・入力。ドラッグを始めたときの frame と revision を覚え、確定はそこへ書く。
fn handle_value_edit(
    session: &mut Session,
    state: &mut MotionState,
    clip: &TimelineClip,
    kind: ClipKeyKind,
    shown: f64,
    edit: Option<NumberEdit>,
) {
    let Some(edit) = edit else {
        return;
    };
    let ours = |draft: &EffectDraft| draft.clip_id == clip.id && draft.kind == kind;
    let begin = |state: &mut MotionState| {
        state.draft = Some(EffectDraft {
            clip_id: clip.id.clone(),
            kind,
            frame: state.frame,
            revision: session.edit_revision(),
            original_value: shown,
        });
    };
    let (value, commit) = match edit {
        // 始めたときの frame と値を覚える。途中で再生ヘッドが動いても、確定はそこへ書く。
        NumberEdit::Start => {
            begin(state);
            return;
        }
        NumberEdit::Cancel => {
            state.draft.take_if(|draft| ours(draft));
            session.cancel_preview();
            return;
        }
        NumberEdit::Preview(value) => (value, false),
        NumberEdit::Commit(value) => (value, true),
    };
    if !state.draft.as_ref().is_some_and(ours) {
        begin(state);
    }
    let Some(draft) = state.draft.clone() else {
        return;
    };
    let values = effect_values(state, kind, value, draft.original_value);
    if !commit {
        // ドラッグ中は Project を変えず、始めた frame の値として preview だけを描く。
        report!(
            session,
            session.preview_effects(&clip.id, draft.frame, &values)
        );
        return;
    }
    state.draft = None;
    // 始めたときの値から変わっていなければ確定しない（キーや undo を積まない）。
    if (value - draft.original_value).abs() < UNCHANGED {
        session.cancel_preview();
        return;
    }
    report!(
        session,
        session.commit_effects(&clip.id, draft.frame, draft.revision, &values)
    );
}

/// 欄の値と、縦横比を固定しているときに一緒に変える拡大率。
fn effect_values(
    state: &MotionState,
    kind: ClipKeyKind,
    value: f64,
    original: f64,
) -> Vec<(ClipKeyKind, f64)> {
    let mut values = vec![(kind, value)];
    let pair = match kind {
        ClipKeyKind::ScaleX if state.lock_aspect => Some(ClipKeyKind::ScaleY),
        ClipKeyKind::ScaleY if state.lock_aspect => Some(ClipKeyKind::ScaleX),
        _ => None,
    };
    if let Some(other) = pair {
        // 縦横比を保つ。元の値が 0 なら同じ値にする。
        let other_value = state.values.get(other as usize).copied().unwrap_or(value);
        let scaled = if original == 0.0 {
            value
        } else {
            other_value * value / original
        };
        let channel = other.channel();
        values.push((other, scaled.clamp(channel.minimum, channel.maximum)));
    }
    values
}

// キーの行の小さなボタン。`checked` なら地を明るくする。
fn key_button(
    ui: &mut egui::Ui,
    size: Vec2,
    label: &str,
    checked: bool,
    enabled: bool,
    paint: impl FnOnce(&egui::Painter, Rect),
) -> egui::Response {
    let sense = if enabled {
        Sense::click()
    } else {
        Sense::hover()
    };
    let (rect, response) = ui.allocate_exact_size(size, sense);
    response.widget_info(|| {
        egui::WidgetInfo::selected(egui::WidgetType::Button, enabled, checked, label)
    });
    let fill = if checked {
        theme::KEY_BUTTON_ON
    } else if response.hovered() && enabled {
        theme::CONTROL_HOVER
    } else {
        theme::KEY_BUTTON
    };
    ui.painter().rect_filled(rect, 3.0, fill);
    paint(ui.painter(), rect);
    response
}

/// 1 frame あたりの幅。clip の先頭と最後の frame が列の両端に来る。
fn frame_width(width: f32, duration: i64) -> f32 {
    width / (duration - 1).max(1) as f32
}

/// キーの列。押した位置へ再生ヘッドを動かし（ドラッグでスクラブ）、キーを掴むと時刻を動かす。
/// 右クリックでキーの削除と補間を選ぶ。
fn key_lane(
    ui: &mut egui::Ui,
    session: &mut Session,
    state: &mut MotionState,
    clip: &TimelineClip,
    kind: ClipKeyKind,
    duration: i64,
) {
    let (rect, response) = ui.allocate_exact_size(
        Vec2::new(ui.available_width(), LANE_HEIGHT),
        Sense::click_and_drag(),
    );
    let ppf = frame_width(rect.width(), duration);
    let frame_at = |x: f32| (((x - rect.min.x) / ppf).round() as i64).clamp(0, duration - 1);
    let keys = clip.effects.keys(kind);
    let key_near = |x: f32| {
        keys.iter()
            .min_by(|a, b| {
                let da = (rect.min.x + a.frame as f32 * ppf - x).abs();
                let db = (rect.min.x + b.frame as f32 * ppf - x).abs();
                da.total_cmp(&db)
            })
            .filter(|key| (rect.min.x + key.frame as f32 * ppf - x).abs() <= 6.0)
            .copied()
    };
    let pressed = ui.input(|input| input.pointer.primary_pressed());
    if pressed
        && response.is_pointer_button_down_on()
        && let Some(origin) = ui.input(|input| input.pointer.press_origin())
    {
        state.key_focus = true;
        if let Some(key) = key_near(origin.x) {
            let toggle = ui.input(|input| input.modifiers.command);
            state.selection.press(kind, key.frame, toggle);
            state.key_drag = Some(KeyDrag {
                clip_id: clip.id.clone(),
                revision: session.edit_revision(),
                kind,
                original_frame: key.frame,
                frame: key.frame,
                value: key.value,
                low: 0.0,
                high: 0.0,
                handle: None,
                moved: false,
            });
        } else {
            if state.selection.kind != Some(kind) {
                state.selection.set(kind, Vec::new());
            }
            session.seek_clip_frame(&clip.id, frame_at(origin.x));
        }
    }
    if response.secondary_clicked()
        && let Some(pointer) = response.interact_pointer_pos()
    {
        state.key_focus = true;
        match key_near(pointer.x) {
            Some(key) if !state.selection.contains(kind, key.frame) => {
                state.selection.press(kind, key.frame, false);
            }
            Some(_) => {}
            None => {
                if state.selection.kind != Some(kind) {
                    state.selection.set(kind, Vec::new());
                }
            }
        }
    }
    if response.dragged()
        && let Some(pointer) = response.interact_pointer_pos()
    {
        if let Some(drag) = state.key_drag.as_mut().filter(|drag| drag.kind == kind) {
            drag.frame = frame_at(pointer.x);
            drag.moved |= drag.frame != drag.original_frame;
            let drag = drag.clone();
            preview_key_drag(session, clip, &drag);
        } else {
            state.scrubbing = true;
            session.set_scrubbing(true);
            session.seek_clip_frame(&clip.id, frame_at(pointer.x));
        }
    }
    if response.drag_stopped() || ui.input(|input| input.pointer.primary_released()) {
        if state.scrubbing {
            state.scrubbing = false;
            session.set_scrubbing(false);
        }
        if let Some(drag) = state.key_drag.take_if(|drag| drag.kind == kind) {
            finish_key_drag(session, state, clip, drag);
        }
    }
    let selection = state.selection.clone();
    response.context_menu(|ui| key_menu(ui, session, clip, kind, &selection));

    let painter = ui.painter_at(rect.expand(1.0));
    painter.rect_filled(rect, 0.0, theme::KEY_LANE);
    let head = rect.min.x + (state.frame as f32 * ppf).clamp(0.0, rect.width() - 1.0);
    painter.line_segment(
        [Pos2::new(head, rect.min.y), Pos2::new(head, rect.max.y)],
        Stroke::new(1.0, theme::KEY_ACCENT),
    );
    for key in keys {
        let frame = moved_frame(state.live_drag(kind), key.frame);
        let color = if selection.contains(kind, key.frame) {
            theme::KEY_ACCENT
        } else {
            theme::KEY_MARK
        };
        icons::paint_diamond(
            &painter,
            Pos2::new(rect.min.x + frame as f32 * ppf, rect.center().y),
            5.0,
            true,
            color,
        );
    }
}

/// ドラッグで動かしている key は、動かした先の frame に描く。
fn moved_frame(drag: Option<&KeyDrag>, frame: i64) -> i64 {
    match drag {
        Some(drag) if drag.original_frame == frame && drag.handle.is_none() => drag.frame,
        _ => frame,
    }
}

/// ハンドルを動かしているときの、区間の 2 つの制御値。`value` に動かした方の制御値が入っている。
fn spline_controls(clip: &TimelineClip, drag: &KeyDrag) -> Option<(f64, f64)> {
    let handle = drag.handle?;
    let key = clip
        .effects
        .keys(drag.kind)
        .iter()
        .find(|key| key.frame == drag.original_frame)?;
    let (control1, control2) = mvm_project::clip_effects::clip_key_spline_controls(key);
    Some(if handle == 0 {
        (drag.value, control2.clamp(0.0, 1.0))
    } else {
        (control1.clamp(0.0, 1.0), drag.value)
    })
}

/// キー・ハンドルを動かしている途中の値で preview だけを描く。確定は離したとき。
fn preview_key_drag(session: &mut Session, clip: &TimelineClip, drag: &KeyDrag) {
    if !drag.moved {
        return;
    }
    // 始めた後に Project が変わった（undo など）。古い位置の移動を新しい Project の上に描かない。
    if !drag_is_current(session, clip, drag) {
        session.cancel_preview();
        return;
    }
    let result = match drag.handle {
        Some(_) => {
            let Some((control1, control2)) = spline_controls(clip, drag) else {
                return;
            };
            session.preview_key_spline(&clip.id, drag.kind, drag.original_frame, control1, control2)
        }
        None => session.preview_key(
            &clip.id,
            drag.kind,
            drag.original_frame,
            drag.frame,
            drag.value,
        ),
    };
    report!(session, result);
}

fn drag_is_current(session: &Session, clip: &TimelineClip, drag: &KeyDrag) -> bool {
    drag.clip_id == clip.id && drag.revision == session.edit_revision()
}

/// 離したときに確定する。確定は始めたときの revision を Session が確かめ、変わっていれば何も書かずに
/// 失敗する。選択は確定できたときだけ動かした先へ移す。
fn finish_key_drag(
    session: &mut Session,
    state: &mut MotionState,
    clip: &TimelineClip,
    drag: KeyDrag,
) {
    if !drag.moved || drag.clip_id != clip.id {
        session.cancel_preview();
        return;
    }
    let result = match drag.handle {
        Some(_) => {
            let Some((control1, control2)) = spline_controls(clip, &drag) else {
                session.cancel_preview();
                return;
            };
            session.commit_key_spline(
                &clip.id,
                drag.kind,
                drag.original_frame,
                control1,
                control2,
                drag.revision,
            )
        }
        None => session.commit_key_move(
            &clip.id,
            drag.kind,
            drag.original_frame,
            drag.frame,
            drag.value,
            drag.revision,
        ),
    };
    match result {
        Ok(()) => {
            if drag.handle.is_none() {
                state.selection.set(drag.kind, vec![drag.frame]);
            }
        }
        Err(error) => {
            session.cancel_preview();
            session.set_alert(error.message());
        }
    }
}

/// キーの右クリック。選んだキーのコピー・カット・ペースト・削除と、補間（C++ 版と同じ並び）。
fn key_menu(
    ui: &mut egui::Ui,
    session: &mut Session,
    clip: &TimelineClip,
    kind: ClipKeyKind,
    selection: &KeySelection,
) {
    let frames: Vec<i64> = if selection.kind == Some(kind) {
        selection.frames.clone()
    } else {
        Vec::new()
    };
    let selected = !frames.is_empty();
    if ui
        .add_enabled(selected, egui::Button::new("キーをコピー"))
        .clicked()
    {
        report!(session, session.copy_keys(&clip.id, kind, &frames, false));
        ui.close();
    }
    if ui
        .add_enabled(selected, egui::Button::new("キーをカット"))
        .clicked()
    {
        report!(session, session.copy_keys(&clip.id, kind, &frames, true));
        ui.close();
    }
    if ui
        .add_enabled(
            session.has_key_clipboard(),
            egui::Button::new("キーをペースト"),
        )
        .clicked()
    {
        report!(session, session.paste_keys(&clip.id, kind));
        ui.close();
    }
    if ui
        .add_enabled(selected, egui::Button::new("キーを削除"))
        .clicked()
    {
        report!(session, session.delete_keys(&clip.id, kind, &frames));
        ui.close();
    }
    // 補間は、右クリックしたキー（選択の最初）から始まる区間に当てる。
    let Some(key) = frames.first().and_then(|frame| {
        clip.effects
            .keys(kind)
            .iter()
            .find(|key| key.frame == *frame)
            .copied()
    }) else {
        return;
    };
    ui.separator();
    for interpolation in KeyInterpolation::ALL {
        if ui
            .selectable_label(
                key.interpolation == interpolation,
                interpolation_name(interpolation),
            )
            .clicked()
        {
            report!(
                session,
                session.set_key_interpolation_at(&clip.id, kind, key.frame, interpolation)
            );
            ui.close();
        }
    }
}

/// キーへ向けた Ctrl+C / X / V と Delete。キーの列・グラフを押した後だけ、clip の操作の代わりに受ける。
fn key_shortcuts(
    ui: &mut egui::Ui,
    session: &mut Session,
    state: &mut MotionState,
    clip: &TimelineClip,
) {
    if !state.key_focus {
        return;
    }
    let Some(kind) = state.selection.kind else {
        return;
    };
    let frames = state.selection.frames.clone();
    let (copy, cut, paste, delete) = ui.input_mut(|input| {
        (
            input.consume_key(egui::Modifiers::COMMAND, egui::Key::C),
            input.consume_key(egui::Modifiers::COMMAND, egui::Key::X),
            input.consume_key(egui::Modifiers::COMMAND, egui::Key::V),
            input.consume_key(egui::Modifiers::NONE, egui::Key::Delete)
                || input.consume_key(egui::Modifiers::NONE, egui::Key::Backspace),
        )
    });
    if (copy || cut) && !frames.is_empty() {
        report!(session, session.copy_keys(&clip.id, kind, &frames, cut));
        if cut {
            state.selection.frames.clear();
        }
    } else if paste && session.has_key_clipboard() {
        report!(session, session.paste_keys(&clip.id, kind));
    } else if delete && !frames.is_empty() {
        report!(session, session.delete_keys(&clip.id, kind, &frames));
        state.selection.frames.clear();
    }
}

fn interpolation_name(interpolation: KeyInterpolation) -> &'static str {
    match interpolation {
        KeyInterpolation::Linear => "リニア",
        KeyInterpolation::EaseIn => "イーズイン",
        KeyInterpolation::EaseOut => "イーズアウト",
        KeyInterpolation::EaseInOut => "イーズインアウト",
        KeyInterpolation::Spline => "スプライン",
    }
}

/// グラフの縦の範囲。キーの値の幅に 2 割（最低 1）の余白を足す。
fn graph_range(keys: &[ClipKeyframe]) -> (f64, f64) {
    let low = keys
        .iter()
        .map(|key| key.value)
        .fold(f64::INFINITY, f64::min);
    let high = keys
        .iter()
        .map(|key| key.value)
        .fold(f64::NEG_INFINITY, f64::max);
    if !low.is_finite() || !high.is_finite() {
        return (0.0, 100.0);
    }
    let margin = ((high - low) * 0.2).max(1.0);
    (low - margin, high + margin)
}

/// 値のカーブ。キーを掴むと時刻と値を、選んだキーのハンドルを掴むと区間の曲線を変える。
fn key_graph(
    ui: &mut egui::Ui,
    session: &mut Session,
    state: &mut MotionState,
    clip: &TimelineClip,
    kind: ClipKeyKind,
    duration: i64,
) {
    let (rect, response) = ui.allocate_exact_size(
        Vec2::new(ui.available_width(), GRAPH_HEIGHT),
        Sense::click_and_drag(),
    );
    let keys = clip.effects.keys(kind);
    let base = clip.effects.base(kind);
    let channel = kind.channel();
    let (low, high) = match state.live_drag(kind) {
        Some(drag) => (drag.low, drag.high),
        None => graph_range(keys),
    };
    let ppf = frame_width(rect.width(), duration);
    let y_of = |value: f64| rect.max.y - ((value - low) / (high - low)) as f32 * rect.height();
    let value_at = |y: f32| low + f64::from((rect.max.y - y) / rect.height()) * (high - low);
    let frame_at = |x: f32| (((x - rect.min.x) / ppf).round() as i64).clamp(0, duration - 1);
    let selected = state.selection.primary(kind);
    let handles = selected_handles(keys, selected, |frame, value| {
        Pos2::new(rect.min.x + frame as f32 * ppf, y_of(value))
    });

    if ui.input(|input| input.pointer.primary_pressed())
        && response.is_pointer_button_down_on()
        && let Some(origin) = ui.input(|input| input.pointer.press_origin())
    {
        let handle = handles
            .iter()
            .position(|(point, _)| point.distance(origin) <= 6.0);
        let key = keys.iter().find(|key| {
            Pos2::new(rect.min.x + key.frame as f32 * ppf, y_of(key.value)).distance(origin) <= 6.0
        });
        if let (Some(handle), Some((_, first))) = (handle, selected) {
            state.key_drag = Some(KeyDrag {
                clip_id: clip.id.clone(),
                revision: session.edit_revision(),
                kind,
                original_frame: first,
                frame: first,
                value: handles[handle].1,
                low,
                high,
                handle: Some(handle),
                moved: false,
            });
        } else if let Some(key) = key {
            let toggle = ui.input(|input| input.modifiers.command);
            state.selection.press(kind, key.frame, toggle);
            state.key_drag = Some(KeyDrag {
                clip_id: clip.id.clone(),
                revision: session.edit_revision(),
                kind,
                original_frame: key.frame,
                frame: key.frame,
                value: key.value,
                low,
                high,
                handle: None,
                moved: false,
            });
        } else {
            // 空いた所から枠を引いて、囲んだキーを選ぶ（Ctrl なら今の選択に足す）。
            let additive = ui.input(|input| input.modifiers.command);
            let base = if additive && state.selection.kind == Some(kind) {
                state.selection.frames.clone()
            } else {
                Vec::new()
            };
            state.selection.set(kind, base.clone());
            state.marquee = Some(Marquee {
                kind,
                origin,
                current: origin,
                base,
            });
        }
        state.key_focus = true;
    }
    if response.dragged()
        && let Some(pointer) = response.interact_pointer_pos()
        && let Some(marquee) = state
            .marquee
            .as_mut()
            .filter(|marquee| marquee.kind == kind)
    {
        marquee.current = pointer;
        let area = Rect::from_two_pos(marquee.origin, marquee.current);
        let mut frames = marquee.base.clone();
        for key in keys {
            let point = Pos2::new(rect.min.x + key.frame as f32 * ppf, y_of(key.value));
            if area.contains(point) && !frames.contains(&key.frame) {
                frames.push(key.frame);
            }
        }
        state.selection.set(kind, frames);
    }
    if ui.input(|input| input.pointer.primary_released()) {
        state.marquee.take_if(|marquee| marquee.kind == kind);
    }
    if response.dragged()
        && let Some(pointer) = response.interact_pointer_pos()
        && let Some(drag) = state.key_drag.as_mut().filter(|drag| drag.kind == kind)
    {
        drag.moved = true;
        match drag.handle {
            Some(_) => {
                // 端点の間の割合へ直す。値の同じ区間にはハンドルを出さない。
                let segment = segment_values(keys, drag.original_frame);
                if let Some((first, next)) = segment {
                    let ratio = (value_at(pointer.y) - first) / (next - first);
                    drag.value = ratio.clamp(0.0, 1.0);
                }
            }
            None => {
                drag.frame = frame_at(pointer.x);
                drag.value = value_at(pointer.y).clamp(channel.minimum, channel.maximum);
            }
        }
        let drag = drag.clone();
        preview_key_drag(session, clip, &drag);
    }
    if (response.drag_stopped() || ui.input(|input| input.pointer.primary_released()))
        && let Some(drag) = state.key_drag.take_if(|drag| drag.kind == kind)
    {
        finish_key_drag(session, state, clip, drag);
    }

    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 0.0, theme::KEY_GRAPH);
    for step in 1..4 {
        let y = rect.min.y + rect.height() * step as f32 / 4.0;
        painter.line_segment(
            [Pos2::new(rect.min.x, y), Pos2::new(rect.max.x, y)],
            Stroke::new(1.0, theme::KEY_GRID),
        );
    }
    // 曲線は確定済みの値で描く。ドラッグ中のキーは点だけを動かして見せる。
    let samples = (rect.width() / 2.0).ceil().max(2.0) as usize;
    let points: Vec<Pos2> = (0..=samples)
        .map(|index| {
            let x = rect.min.x + rect.width() * index as f32 / samples as f32;
            let frame = ((x - rect.min.x) / ppf).round() as i64;
            Pos2::new(x, y_of(evaluate_clip_keys(keys, base, frame)))
        })
        .collect();
    painter.add(egui::Shape::line(
        points,
        Stroke::new(1.5, theme::KEY_ACCENT),
    ));
    let head = rect.min.x + (state.frame as f32 * ppf).clamp(0.0, rect.width() - 1.0);
    painter.line_segment(
        [Pos2::new(head, rect.min.y), Pos2::new(head, rect.max.y)],
        Stroke::new(1.0, theme::KEY_ACCENT.gamma_multiply(0.6)),
    );
    if let Some(marquee) = state
        .marquee
        .as_ref()
        .filter(|marquee| marquee.kind == kind)
    {
        painter.rect(
            Rect::from_two_pos(marquee.origin, marquee.current),
            0.0,
            Color32::from_rgba_unmultiplied(0x4b, 0x72, 0x9a, 0x30),
            Stroke::new(1.0, theme::KEY_ACCENT),
            egui::StrokeKind::Inside,
        );
    }
    let drag = state.live_drag(kind).cloned();
    for (index, (point, _)) in handles.iter().enumerate() {
        let point = match drag.as_ref().filter(|drag| drag.handle == Some(index)) {
            Some(drag) => handle_point(keys, drag.original_frame, index, drag.value, &|f, v| {
                Pos2::new(rect.min.x + f as f32 * ppf, y_of(v))
            })
            .unwrap_or(*point),
            None => *point,
        };
        let anchor = if index == 0 {
            handles_anchor(keys, selected, true)
        } else {
            handles_anchor(keys, selected, false)
        };
        if let Some((frame, value)) = anchor {
            painter.line_segment(
                [
                    Pos2::new(rect.min.x + frame as f32 * ppf, y_of(value)),
                    point,
                ],
                Stroke::new(1.0, Color32::from_rgb(0x47, 0x62, 0x7c)),
            );
        }
        painter.circle(
            point,
            4.0,
            theme::KEY_GRAPH,
            Stroke::new(2.0, theme::KEY_ACCENT),
        );
    }
    for key in keys {
        let (frame, value) = match drag
            .as_ref()
            .filter(|drag| drag.handle.is_none() && drag.original_frame == key.frame)
        {
            Some(drag) => (drag.frame, drag.value),
            None => (key.frame, key.value),
        };
        let color = if state.selection.contains(kind, key.frame) {
            theme::KEY_ACCENT
        } else {
            theme::KEY_MARK
        };
        painter.circle_filled(
            Pos2::new(rect.min.x + frame as f32 * ppf, y_of(value)),
            4.0,
            color,
        );
    }
    let font = FontId::proportional(10.0);
    let label = Color32::from_rgb(0x89, 0x95, 0xa4);
    painter.text(
        rect.min + Vec2::new(5.0, 2.0),
        Align2::LEFT_TOP,
        format!("{high:.1}"),
        font.clone(),
        label,
    );
    painter.text(
        Pos2::new(rect.min.x + 5.0, rect.max.y - 2.0),
        Align2::LEFT_BOTTOM,
        format!("{low:.1}"),
        font,
        label,
    );
}

/// `frame` のキーから始まる区間の、始めと終わりの値。最後のキー・値の同じ区間は None。
fn segment_values(keys: &[ClipKeyframe], frame: i64) -> Option<(f64, f64)> {
    let position = keys.iter().position(|key| key.frame == frame)?;
    let first = keys.get(position)?;
    let next = keys.get(position + 1)?;
    (first.value != next.value).then_some((first.value, next.value))
}

/// ハンドル `index`（0 は区間の 1/3、1 は 2/3）を制御値 `control` に置いたときの位置。
fn handle_point(
    keys: &[ClipKeyframe],
    frame: i64,
    index: usize,
    control: f64,
    to_screen: &dyn Fn(f64, f64) -> Pos2,
) -> Option<Pos2> {
    let position = keys.iter().position(|key| key.frame == frame)?;
    let first = keys.get(position)?;
    let next = keys.get(position + 1)?;
    let at = first.frame as f64 + (next.frame - first.frame) as f64 * (index as f64 + 1.0) / 3.0;
    Some(to_screen(
        at,
        first.value + (next.value - first.value) * control,
    ))
}

/// 選んだキーから始まる区間のハンドル 2 つの位置と制御値。
fn selected_handles(
    keys: &[ClipKeyframe],
    selected: Option<(ClipKeyKind, i64)>,
    to_screen: impl Fn(f64, f64) -> Pos2,
) -> Vec<(Pos2, f64)> {
    let Some((_, frame)) = selected else {
        return Vec::new();
    };
    if segment_values(keys, frame).is_none() {
        return Vec::new();
    }
    let Some(key) = keys.iter().find(|key| key.frame == frame) else {
        return Vec::new();
    };
    let (control1, control2) = mvm_project::clip_effects::clip_key_spline_controls(key);
    [control1, control2]
        .into_iter()
        .enumerate()
        .filter_map(|(index, control)| {
            handle_point(keys, frame, index, control, &|f, v| to_screen(f, v))
                .map(|point| (point, control))
        })
        .collect()
}

/// ハンドルの線の付け根。始めのハンドルは区間の始めのキー、終わりのハンドルは次のキー。
fn handles_anchor(
    keys: &[ClipKeyframe],
    selected: Option<(ClipKeyKind, i64)>,
    first: bool,
) -> Option<(i64, f64)> {
    let (_, frame) = selected?;
    let position = keys.iter().position(|key| key.frame == frame)?;
    let key = keys.get(if first { position } else { position + 1 })?;
    Some((key.frame, key.value))
}

/// フェードイン・アウトの長さ（素材の frame）。
fn fade_rows(ui: &mut egui::Ui, session: &mut Session, clip: &TimelineClip) {
    let fades = (clip.effects.fade_in_frames, clip.effects.fade_out_frames);
    let mut edited = None;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        let half = ((ui.available_width() - 6.0) / 2.0).max(40.0);
        for (index, label) in ["フェードイン (素材f)", "フェードアウト (素材f)"]
            .into_iter()
            .enumerate()
        {
            let value = if index == 0 { fades.0 } else { fades.1 };
            let edit = NumberField {
                label,
                value: value as f64,
                minimum: 0.0,
                maximum: 1_000_000.0,
                step: 1.0,
                width: half,
                ..NumberField::default()
            }
            .show(ui, egui::Id::new(("fade", &clip.id, index)));
            let pair = |value: f64| {
                let value = value as i64;
                if index == 0 {
                    (value, fades.1)
                } else {
                    (fades.0, value)
                }
            };
            edited = match edit {
                Some(NumberEdit::Preview(value)) => Some((pair(value), false)),
                Some(NumberEdit::Commit(value)) => Some((pair(value), true)),
                Some(NumberEdit::Cancel) => Some((fades, true)),
                Some(NumberEdit::Start) | None => edited,
            };
        }
    });
    // ドラッグ中は preview だけ、離したら確定する（C++ 版と同じ）。
    match edited {
        Some(((fade_in, fade_out), false)) => {
            report!(session, session.preview_fades(&clip.id, fade_in, fade_out));
        }
        Some(((fade_in, fade_out), true)) if (fade_in, fade_out) != fades => {
            report!(session, session.set_fades(&clip.id, fade_in, fade_out));
        }
        Some((_, true)) => session.cancel_preview(),
        None => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 位置 X に 0 と 100 frame のキーを持つ clip。
    fn keyed_session() -> Session {
        let mut clip = TimelineClip {
            kind: TimelineClipKind::Video,
            media_path: "v.mp4".into(),
            name: "v".to_owned(),
            id: "v".to_owned(),
            source_fps: mvm_core::Rational::new(60, 1),
            source_frame_count: 120,
            source_out_frame: 120,
            track: mvm_project::TrackRef::video(0),
            ..TimelineClip::default()
        };
        clip.effects.position_x_keys =
            vec![ClipKeyframe::new(0, 0.0), ClipKeyframe::new(100, 50.0)];
        Session::from_project(mvm_project::Project {
            timeline_clips: vec![clip],
            ..mvm_project::Project::default()
        })
    }

    fn drag_of(session: &Session, frame: i64) -> KeyDrag {
        KeyDrag {
            clip_id: "v".to_owned(),
            revision: session.edit_revision(),
            kind: ClipKeyKind::PositionX,
            original_frame: 100,
            frame,
            value: 40.0,
            low: 0.0,
            high: 0.0,
            handle: None,
            moved: true,
        }
    }

    #[test]
    fn a_finished_key_drag_moves_the_key_and_the_selection() {
        let mut session = keyed_session();
        let mut state = MotionState::default();
        state.selection.set(ClipKeyKind::PositionX, vec![100]);
        let drag = drag_of(&session, 90);
        let clip = session.project().timeline_clips[0].clone();
        finish_key_drag(&mut session, &mut state, &clip, drag);
        let keys = &session.project().timeline_clips[0].effects.position_x_keys;
        let frames: Vec<_> = keys.iter().map(|key| (key.frame, key.value)).collect();
        assert_eq!(frames, [(0, 0.0), (90, 40.0)]);
        assert_eq!(state.selection.frames, [90]);
    }

    #[test]
    fn a_key_drag_across_an_undo_writes_nothing_and_keeps_the_selection() {
        let mut session = keyed_session();
        session
            .set_effect("v", ClipKeyKind::PositionY, 5.0)
            .expect("編集");
        let mut state = MotionState::default();
        state.selection.set(ClipKeyKind::PositionX, vec![100]);
        let drag = drag_of(&session, 90);
        // ドラッグの途中で undo した。
        session.undo().expect("戻す");
        let before = session.project().clone();
        let clip = before.timeline_clips[0].clone();
        preview_key_drag(&mut session, &clip, &drag);
        finish_key_drag(&mut session, &mut state, &clip, drag);
        assert_eq!(session.project(), &before);
        assert_eq!(
            state.selection.frames,
            [100],
            "失敗した移動へ選択が動きました"
        );
        assert!(
            session
                .alert()
                .is_some_and(|alert| alert.contains("取り消しました")),
            "{:?}",
            session.alert()
        );
    }

    #[test]
    fn the_graph_leaves_room_around_the_key_values() {
        let keys = [ClipKeyframe::new(0, 0.0), ClipKeyframe::new(60, 100.0)];
        assert_eq!(graph_range(&keys), (-20.0, 120.0));
        // 値が 1 つでも最低 1 の余白を取る（範囲 0 で割らない）。
        assert_eq!(graph_range(&[ClipKeyframe::new(0, 85.0)]), (84.0, 86.0));
        assert_eq!(graph_range(&[]), (0.0, 100.0));
    }

    #[test]
    fn handles_sit_at_a_third_and_two_thirds_of_the_segment() {
        let keys = [ClipKeyframe::new(0, 0.0), ClipKeyframe::new(90, 60.0)];
        let screen = |frame: f64, value: f64| Pos2::new(frame as f32, value as f32);
        // 既定の制御値は 1/3 と 2/3（直線）。
        let handles = selected_handles(&keys, Some((ClipKeyKind::Opacity, 0)), screen);
        assert_eq!(handles.len(), 2);
        assert_eq!(handles[0].0, Pos2::new(30.0, 20.0));
        assert_eq!(handles[1].0, Pos2::new(60.0, 40.0));
        // 最後のキーと、値の同じ区間にはハンドルが無い。
        assert!(selected_handles(&keys, Some((ClipKeyKind::Opacity, 90)), screen).is_empty());
        let flat = [ClipKeyframe::new(0, 50.0), ClipKeyframe::new(90, 50.0)];
        assert!(selected_handles(&flat, Some((ClipKeyKind::Opacity, 0)), screen).is_empty());
    }

    #[test]
    fn the_lane_spans_the_first_to_the_last_frame() {
        assert_eq!(frame_width(300.0, 301), 1.0);
        // 1 frame の clip でも 0 で割らない。
        assert_eq!(frame_width(300.0, 1), 300.0);
    }
}
