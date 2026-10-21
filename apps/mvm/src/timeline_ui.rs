//! timeline の表示。区間の外にある clip は widget を作らない。
//!
//! ドラッグ中は Project を変えず、離したときに command を 1 回だけ実行する。

use eframe::egui::{self, Align2, Color32, Label, Pos2, Rect, Sense, Stroke, Vec2};
use mvm_app::{
    MIN_PIXELS_PER_FRAME, Session, TimeWindow, display_span_frames, ranges_overlap, ruler_step,
    visible_window,
};
use mvm_project::timeline::{
    BodyAction, BodyPress, ClipSpan, DragBounds, EdgeAction, GestureModifiers, PenGeometry, PenKey,
    PenPress, TOOLS, TimelineTool, TrimEdge, body_press, body_release, clip_ids_spanning_frame,
    clip_interval, drag_snap_frames, edge_release, group_drag_offset_x, group_drag_track_index,
    marker_near_ruler_x, pen_press, pen_release, pen_snap_value, pen_value_at, pen_visible_keys,
    pen_visible_line_points, snap_drag_offset_x,
};
use mvm_project::{Project, TrackKind, TrackRef};

use crate::project_panel::MediaDrag;
use crate::subtitle_lane::{self, SubtitleGesture};
use crate::{icons, theme, widgets};

macro_rules! report {
    ($session:ident, $call:expr $(,)?) => {{
        let result = $call;
        if let Err(error) = result {
            $session.set_alert(error.message());
        }
    }};
}

/// 左端のツール列と、track 名の列の幅。
const TOOL_PANEL_WIDTH: f32 = 34.0;
const HEADER_WIDTH: f32 = 96.0;
const RULER_HEIGHT: f32 = 26.0;
pub(crate) const ROW_HEIGHT: f32 = 54.0;
/// clip・字幕を行の上下から離す幅。
pub(crate) const CLIP_INSET: f32 = 3.0;
/// 最上段の video track の上に置く「+V」の行と、最下段の audio track の下の「+A」の余白。
const ADD_VIDEO_ROW: f32 = 28.0;
const ADD_AUDIO_SPACE: f32 = 34.0;
pub(crate) const EDGE_PIXELS: f32 = 8.0;

/// timeline の UI 状態。ドラッグ中の操作と、shell へ渡す依頼。
#[derive(Default)]
pub struct TimelineState {
    pub gesture: Option<TimelineGesture>,
    pub subtitle_gesture: Option<SubtitleGesture>,
    /// 右クリックの「自動字幕を生成」で選んだ clip。shell が文字起こしの dialog を開く。
    pub transcribe_clip: Option<String>,
    /// 右クリックの「速度・デュレーション」で選んだ clip。shell が dialog を開く。
    pub speed_clip: Option<String>,
    /// 右クリックの「プロジェクト設定をこの素材に合わせる」で選んだ clip。
    pub match_settings_clip: Option<String>,
    /// S1 の見出しを押した。shell が字幕のタブを開く。
    pub open_subtitles: bool,
    /// 倍率を変えたあと、次の frame で合わせる横スクロールの位置（px）。カーソル下の frame を動かさない。
    scroll_to_x: Option<f32>,
    /// 直前の frame の横スクロールの位置（px）。
    scroll_offset_x: f32,
    /// 目玉のドラッグ塗り。押した video track の表示を反転し、ドラッグで通った track も同じにする。
    eye_paint: Option<EyePaint>,
    /// 空白・ルーラーを右クリックした位置（track の行の上なら track も）。
    background_menu: Option<(Option<TrackRef>, i64)>,
}

/// 目玉のドラッグ塗りの途中。離したときに通った track をまとめて確定する（1 回の undo）。
#[derive(Clone, Debug, PartialEq)]
struct EyePaint {
    /// 塗る値（非表示なら true）。
    muted: bool,
    /// 通った video track の index。
    painted: Vec<usize>,
    last: usize,
}

impl EyePaint {
    /// 押した track の今の表示を反転した値で塗り始める。
    fn begin(index: usize, pressed_muted: bool) -> Self {
        Self {
            muted: !pressed_muted,
            painted: vec![index],
            last: index,
        }
    }

    /// `index` まで動いた。速く動かすと move が行を飛ばすので、間の track も通ったものとして足す。
    fn reach(&mut self, index: usize) {
        let (low, high) = (self.last.min(index), self.last.max(index));
        for passed in low..=high {
            if !self.painted.contains(&passed) {
                self.painted.push(passed);
            }
        }
        self.last = index;
    }

    /// 目玉に見せる表示。塗っている最中は、通った track に塗る値を先に見せる。
    fn shown_muted(&self, index: usize, actual: bool) -> bool {
        if self.painted.contains(&index) {
            self.muted
        } else {
            actual
        }
    }
}

/// 行の y（track 領域の上端から）にある video track の index。video の行の外は端の track に寄せる（audio の行や
/// ruler へはみ出しても塗りが途切れないように）。video track が無ければ None。
fn video_index_at(local_y: f32, video_count: usize) -> Option<usize> {
    if video_count == 0 {
        return None;
    }
    let row = (local_y / ROW_HEIGHT)
        .floor()
        .clamp(0.0, (video_count - 1) as f32) as usize;
    Some(video_count - 1 - row)
}

/// track 領域の縦の並び。y は content（スクロールする領域）の上端から。
struct Lanes {
    rows: Vec<(TrackKind, usize, String)>,
    /// 字幕の track があれば、その表示状態。
    subtitles: Option<bool>,
}

impl Lanes {
    fn new(project: &Project) -> Self {
        Self {
            rows: track_rows(project),
            subtitles: project.subtitles.as_ref().map(|track| track.visible),
        }
    }

    /// S1 の行の上端。「+V」の行の下。
    fn subtitle_top(&self) -> f32 {
        ADD_VIDEO_ROW
    }

    /// 最上段の track の上端。
    fn tracks_top(&self) -> f32 {
        ADD_VIDEO_ROW
            + if self.subtitles.is_some() {
                ROW_HEIGHT
            } else {
                0.0
            }
    }

    fn row_top(&self, row: usize) -> f32 {
        self.tracks_top() + row as f32 * ROW_HEIGHT
    }

    fn video_count(&self) -> usize {
        self.rows
            .iter()
            .filter(|(kind, _, _)| *kind == TrackKind::Video)
            .count()
    }

    fn height(&self) -> f32 {
        self.row_top(self.rows.len()) + ADD_AUDIO_SPACE
    }

    /// content の y にある track の行。
    fn row_at(&self, y: f32) -> Option<usize> {
        let local = y - self.tracks_top();
        if local < 0.0 {
            return None;
        }
        let row = (local / ROW_HEIGHT) as usize;
        (row < self.rows.len()).then_some(row)
    }
}

pub struct TimelineGesture {
    clip_id: String,
    press_frame: i64,
    origin: Pos2,
    pointer: Pos2,
    moved: bool,
    linked: bool,
    body: Option<BodyPress>,
    edge: Option<TrimEdge>,
    pen: Option<PenPress>,
}

pub fn show(ui: &mut egui::Ui, session: &mut Session, state: &mut TimelineState) {
    transport_row(ui, session);
    ui.add_space(4.0);
    egui::Frame::new()
        .fill(theme::TIMELINE)
        .stroke(Stroke::new(1.0, theme::TIMELINE_BORDER))
        .corner_radius(5)
        .inner_margin(egui::Margin::same(1))
        .show(ui, |ui| {
            // panel の高さいっぱいを使う。中身の高さで panel の大きさが決まると、境界をドラッグして広げた高さが
            // 離した次の frame で中身の高さへ戻ってしまう。
            let area = ui.available_rect_before_wrap();
            ui.expand_to_include_rect(area);
            let tools = Rect::from_min_size(area.min, Vec2::new(TOOL_PANEL_WIDTH, area.height()));
            ui.scope_builder(egui::UiBuilder::new().max_rect(tools), |ui| {
                tool_panel(ui, session);
            });
            let body = Rect::from_min_max(Pos2::new(tools.max.x, area.min.y), area.max);
            show_body(ui, session, state, body);
        });
}

/// timeline の上の行。playhead の時刻・fps・倍率と、今のツールの使い方。
fn transport_row(ui: &mut egui::Ui, session: &Session) {
    ui.horizontal(|ui| {
        ui.label(
            egui::RichText::new(session.playhead_label())
                .monospace()
                .strong()
                .size(15.0)
                .color(theme::TEXT),
        );
        let fps = session.project().timeline_fps;
        let measured = mvm_project::is_measured_timeline_frame_rate(fps);
        let rate = if fps.den == 1 {
            format!("{} fps", fps.num)
        } else if fps.den > 0 {
            format!("{:.3} fps", fps.num as f64 / fps.den as f64)
        } else {
            "-- fps".to_owned()
        };
        let label = ui.label(
            egui::RichText::new(format!(
                "{rate}{}  |  zoom {}%",
                if measured { "" } else { " (未計測)" },
                session.zoom_percent()
            ))
            .color(if measured {
                theme::TEXT_SECONDARY
            } else {
                theme::WARNING
            }),
        );
        if !measured {
            label.on_hover_text("この frame rate の preview は実測していません");
        }
        let rate = session.shuttle_rate();
        if rate != 0 && rate != 1 {
            ui.label(
                egui::RichText::new(format!("シャトル {rate} 倍速（音声なし）"))
                    .color(theme::WARNING),
            );
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let spec = session.tool().spec();
            ui.add(
                Label::new(
                    egui::RichText::new(format!(
                        "{}: {}  |  Alt+操作: リンクの片方だけ / Space: 再生・一時停止 / Alt+ホイール: ズーム",
                        spec.name, spec.hint
                    ))
                    .size(11.0)
                    .color(theme::TEXT_DIM),
                )
                .truncate(),
            );
        });
    });
}

/// 左端に縦 1 列で並べるツール。種類が変わるところに区切り線を入れる。
fn tool_panel(ui: &mut egui::Ui, session: &mut Session) {
    let rect = ui.max_rect();
    ui.painter().rect(
        rect,
        0.0,
        theme::STRIP,
        Stroke::new(1.0, theme::TIMELINE_BORDER),
        egui::StrokeKind::Inside,
    );
    egui::ScrollArea::vertical()
        .id_salt("timeline_tools")
        .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden)
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.add_space(4.0);
            ui.vertical_centered(|ui| {
                ui.spacing_mut().item_spacing.y = 2.0;
                let mut group = None;
                for spec in TOOLS {
                    if group.is_some_and(|group| group != spec.group) {
                        let (line, _) =
                            ui.allocate_exact_size(Vec2::new(22.0, 1.0), Sense::hover());
                        ui.painter().rect_filled(line, 0.0, theme::TIMELINE_BORDER);
                    }
                    group = Some(spec.group);
                    let response = icons::button(
                        ui,
                        Vec2::new(28.0, 26.0),
                        spec.name,
                        session.tool() == spec.tool,
                        |painter, rect, color| icons::paint_tool(painter, rect, spec.tool, color),
                    )
                    .on_hover_text(format!("{} ({})\n{}", spec.name, spec.key, spec.hint));
                    if response.clicked() {
                        session.set_tool(spec.tool);
                    }
                }
            });
        });
}

/// ルーラー・track 名・スクロールする track 領域。ルーラーは縦に、track 名は横にスクロールしない。
fn show_body(ui: &mut egui::Ui, session: &mut Session, state: &mut TimelineState, area: Rect) {
    let lanes = Lanes::new(session.project());
    let ppf = session.pixels_per_frame();
    let span =
        display_span_frames(session.project()).max(session.playhead_frame().saturating_add(1));
    let content_width = (span as f32).mul_add(ppf, 48.0).max(1.0);

    let corner = Rect::from_min_size(area.min, Vec2::new(HEADER_WIDTH, RULER_HEIGHT));
    let ruler = Rect::from_min_max(
        Pos2::new(corner.max.x, area.min.y),
        Pos2::new(area.max.x, corner.max.y),
    );
    let headers = Rect::from_min_max(
        Pos2::new(area.min.x, corner.max.y),
        Pos2::new(corner.max.x, area.max.y),
    );
    let tracks = Rect::from_min_max(Pos2::new(ruler.min.x, ruler.max.y), area.max);
    alt_wheel_zoom(ui, session, state, tracks);

    let output = ui
        .scope_builder(egui::UiBuilder::new().max_rect(tracks), |ui| {
            let mut scroll = egui::ScrollArea::both()
                .id_salt("timeline_scroll")
                .auto_shrink([false, false])
                .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysVisible);
            if let Some(x) = state.scroll_to_x.take() {
                scroll = scroll.horizontal_scroll_offset(x);
            }
            scroll.show_viewport(ui, |ui, viewport| {
                let window = visible_window(viewport.min.x, viewport.width(), ppf);
                let height = lanes.height().max(viewport.height());
                draw_contents(
                    ui,
                    session,
                    state,
                    &lanes,
                    Vec2::new(content_width, height),
                    window,
                    ppf,
                );
            })
        })
        .inner;
    state.scroll_offset_x = output.state.offset.x;
    let offset = output.state.offset;
    let origin_x = tracks.min.x - offset.x;
    let window = visible_window(offset.x, tracks.width(), ppf);
    ui.scope_builder(egui::UiBuilder::new().max_rect(ruler), |ui| {
        ui.set_clip_rect(ruler.intersect(ui.clip_rect()));
        draw_ruler(ui, session, state, ruler, origin_x, window, ppf);
    });
    // track 名の列は track 領域と一緒に縦へスクロールし、見えている範囲の外にも部品を置く。親へ大きさを
    // 伝えない child にする（伝えると、全 track の高さまで panel が伸びる）。
    let mut header_ui = ui.new_child(egui::UiBuilder::new().max_rect(headers));
    header_ui.set_clip_rect(headers.intersect(ui.clip_rect()));
    let top = headers.min.y - offset.y;
    draw_headers(&mut header_ui, session, state, &lanes, headers, top);
    ui.scope_builder(egui::UiBuilder::new().max_rect(corner), |ui| {
        ui.painter().rect_filled(corner, 0.0, theme::STRIP);
        ui.horizontal_centered(|ui| {
            ui.add_space(6.0);
            ui.spacing_mut().item_spacing.x = 2.0;
            let size = Vec2::new(24.0, 22.0);
            let zoom_out = icons::button(ui, size, "縮小", false, |painter, rect, color| {
                icons::paint_zoom(painter, rect, false, color);
            });
            if zoom_out.on_hover_text("縮小").clicked() {
                zoom_at(session, state, -1, ruler.width() / 2.0);
            }
            let zoom_in = icons::button(ui, size, "拡大", false, |painter, rect, color| {
                icons::paint_zoom(painter, rect, true, color);
            });
            if zoom_in.on_hover_text("拡大").clicked() {
                zoom_at(session, state, 1, ruler.width() / 2.0);
            }
        });
    });
}

/// 倍率を `steps` 段変え、track 領域の左端から `anchor_x` px の位置にある frame を同じ位置に保つ。
fn zoom_at(session: &mut Session, state: &mut TimelineState, steps: i32, anchor_x: f32) {
    let before = session.pixels_per_frame();
    session.zoom_step(steps);
    let after = session.pixels_per_frame();
    if before == after {
        return;
    }
    let frame = (state.scroll_offset_x + anchor_x) / before;
    state.scroll_to_x = Some((frame * after - anchor_x).max(0.0));
}

/// Alt+ホイールで、カーソルの位置を中心に拡大・縮小する。ホイールの量はスクロールに回さない。
fn alt_wheel_zoom(
    ui: &mut egui::Ui,
    session: &mut Session,
    state: &mut TimelineState,
    tracks: Rect,
) {
    let Some(pointer) = ui.input(|input| input.pointer.hover_pos()) else {
        return;
    };
    if !tracks.contains(pointer) {
        return;
    }
    let delta = ui.input_mut(|input| {
        if !input.modifiers.alt {
            return 0.0;
        }
        input.smooth_scroll_delta = Vec2::ZERO;
        input
            .raw
            .events
            .iter()
            .map(|event| match event {
                egui::Event::MouseWheel { delta, .. } => delta.x + delta.y,
                _ => 0.0,
            })
            .sum::<f32>()
    });
    if delta != 0.0 {
        zoom_at(
            session,
            state,
            if delta > 0.0 { 1 } else { -1 },
            pointer.x - tracks.min.x,
        );
    }
}

/// track 名の列。video は目玉、audio は M / S。右クリックで track を削除する。
fn draw_headers(
    ui: &mut egui::Ui,
    session: &mut Session,
    state: &mut TimelineState,
    lanes: &Lanes,
    area: Rect,
    top: f32,
) {
    ui.painter().rect_filled(area, 0.0, theme::STRIP);
    let add_video = Rect::from_min_size(
        Pos2::new(area.min.x + 3.0, top + (ADD_VIDEO_ROW - 22.0) / 2.0),
        Vec2::new(42.0, 22.0),
    );
    if ui.is_rect_visible(add_video)
        && ui
            .put(add_video, egui::Button::new("+V"))
            .on_hover_text("video トラックを追加")
            .clicked()
    {
        report!(session, session.add_track(TrackKind::Video));
    }
    if let Some(visible) = lanes.subtitles {
        let row = Rect::from_min_size(
            Pos2::new(area.min.x, top + lanes.subtitle_top()),
            Vec2::new(HEADER_WIDTH, ROW_HEIGHT),
        );
        ui.painter().rect(
            row,
            0.0,
            theme::SUBTITLE_HEADER,
            Stroke::new(1.0, theme::SUBTITLE_HEADER_BORDER),
            egui::StrokeKind::Inside,
        );
        let name = Rect::from_min_max(
            row.min + Vec2::new(6.0, 0.0),
            Pos2::new(row.max.x - 28.0, row.max.y),
        );
        let label = ui.put(
            name,
            Label::new(egui::RichText::new("S1 字幕").color(theme::SUBTITLE_HEADER_TEXT))
                .sense(Sense::click()),
        );
        if label.on_hover_text("字幕の一覧を開く").clicked() {
            state.open_subtitles = true;
        }
        let eye = Rect::from_center_size(
            Pos2::new(row.max.x - 16.0, row.center().y),
            Vec2::splat(20.0),
        );
        if eye_button(ui, eye, "S1", !visible).clicked() {
            report!(session, session.toggle_subtitle_visible());
        }
    }
    for (row_index, (kind, index, name)) in lanes.rows.iter().enumerate() {
        let row = Rect::from_min_size(
            Pos2::new(area.min.x, top + lanes.row_top(row_index)),
            Vec2::new(HEADER_WIDTH, ROW_HEIGHT),
        );
        if !ui.is_rect_visible(row) {
            continue;
        }
        let track = TrackRef::new(*kind, *index);
        let Some(track_state) = session.project().tracks(*kind).get(*index) else {
            continue;
        };
        let (actual_muted, solo) = (track_state.muted, track_state.solo);
        let video = *kind == TrackKind::Video;
        let muted = match &state.eye_paint {
            Some(paint) if video => paint.shown_muted(*index, actual_muted),
            _ => actual_muted,
        };
        ui.painter().rect(
            row,
            0.0,
            if video {
                theme::VIDEO_HEADER
            } else {
                theme::AUDIO_HEADER
            },
            Stroke::new(1.0, theme::TIMELINE_BORDER),
            egui::StrokeKind::Inside,
        );
        let controls = if video { 24.0 } else { 46.0 };
        let name_rect = Rect::from_min_max(
            row.min + Vec2::new(6.0, 0.0),
            Pos2::new(row.max.x - controls - 4.0, row.max.y),
        );
        let label = ui
            .scope_builder(
                egui::UiBuilder::new()
                    .max_rect(name_rect)
                    .layout(egui::Layout::left_to_right(egui::Align::Center)),
                |ui| {
                    ui.add(
                        Label::new(egui::RichText::new(name).font(theme::bold(12.5)).color(
                            if muted {
                                theme::HEADER_TEXT_MUTED
                            } else {
                                theme::TEXT_ICON
                            },
                        ))
                        .truncate()
                        .sense(Sense::click()),
                    )
                },
            )
            .inner;
        label.context_menu(|ui| {
            if ui.button("このトラックを削除").clicked() {
                report!(session, session.remove_track(track));
                ui.close();
            }
        });
        // 名前のすぐ右に置く。
        let controls_x = label.rect.max.x + 4.0;
        if video {
            let eye = Rect::from_min_size(
                Pos2::new(controls_x, row.center().y - 10.0),
                Vec2::splat(20.0),
            );
            let response = eye_button(ui, eye, name, muted);
            if response.is_pointer_button_down_on()
                && ui.input(|input| input.pointer.primary_pressed())
            {
                state.eye_paint = Some(EyePaint::begin(*index, actual_muted));
            } else if response.clicked() && state.eye_paint.is_none() {
                // 読み上げ・キーボードからの押下はポインタを伴わない。その track だけを切り替える。
                report!(session, session.toggle_track_muted(track));
            }
        } else {
            let toggles = Rect::from_min_size(
                Pos2::new(controls_x, row.center().y - 10.0),
                Vec2::new(controls, 20.0),
            );
            ui.scope_builder(
                egui::UiBuilder::new()
                    .max_rect(toggles)
                    .layout(egui::Layout::left_to_right(egui::Align::Center)),
                |ui| {
                    ui.spacing_mut().item_spacing.x = 2.0;
                    let tip = if muted {
                        "ミュート解除"
                    } else {
                        "ミュート"
                    };
                    if widgets::track_toggle(ui, "M", muted, theme::MUTE_ON, tip).clicked() {
                        report!(session, session.toggle_track_muted(track));
                    }
                    let tip = if solo {
                        "ソロ解除"
                    } else {
                        "ソロ（このトラックだけを鳴らす）"
                    };
                    if widgets::track_toggle(ui, "S", solo, theme::SOLO_ON, tip).clicked() {
                        report!(session, session.toggle_track_solo(track));
                    }
                },
            );
        }
    }
    if let Some(paint) = state.eye_paint.as_mut() {
        if let Some(pointer) = ui.input(|input| input.pointer.interact_pos())
            && let Some(index) =
                video_index_at(pointer.y - top - lanes.tracks_top(), lanes.video_count())
        {
            paint.reach(index);
        }
        if ui.input(|input| input.pointer.primary_released()) {
            let paint = state.eye_paint.take();
            if let Some(paint) = paint {
                let changed: Vec<usize> = paint
                    .painted
                    .iter()
                    .copied()
                    .filter(|index| {
                        session
                            .project()
                            .video_tracks
                            .get(*index)
                            .is_some_and(|track| track.muted != paint.muted)
                    })
                    .collect();
                if !changed.is_empty() {
                    report!(
                        session,
                        session.set_tracks_muted(TrackKind::Video, &changed, paint.muted)
                    );
                }
            }
        } else if ui.input(|input| input.key_pressed(egui::Key::Escape)) {
            state.eye_paint = None;
        }
    }
    let divider = top + lanes.row_top(lanes.video_count()) - 2.0;
    ui.painter().rect_filled(
        Rect::from_min_size(Pos2::new(area.min.x, divider), Vec2::new(HEADER_WIDTH, 4.0)),
        0.0,
        theme::ROW_DIVIDER,
    );
    let add_audio = Rect::from_min_size(
        Pos2::new(
            area.min.x + 3.0,
            top + lanes.row_top(lanes.rows.len()) + 4.0,
        ),
        Vec2::new(42.0, 22.0),
    );
    if ui.is_rect_visible(add_audio)
        && ui
            .put(add_audio, egui::Button::new("+A"))
            .on_hover_text("audio トラックを追加")
            .clicked()
    {
        report!(session, session.add_track(TrackKind::Audio));
    }
}

/// 表示・非表示の目玉。読み上げ・試験からは「V1 表示」「V1 非表示」で引ける。
fn eye_button(ui: &mut egui::Ui, rect: Rect, name: &str, hidden: bool) -> egui::Response {
    let response = ui.interact(
        rect,
        ui.id().with(("track_eye", name)),
        Sense::click_and_drag(),
    );
    let label = if hidden {
        format!("{name} 非表示")
    } else {
        format!("{name} 表示")
    };
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, &label));
    if response.hovered() {
        ui.painter().rect_filled(rect, 3.0, theme::CONTROL_HOVER);
    }
    icons::paint_eye(
        ui.painter(),
        rect.shrink(2.0),
        hidden,
        if hidden {
            theme::EYE_HIDDEN
        } else {
            theme::TEXT_ICON
        },
    );
    response.on_hover_text(if hidden {
        "トラックを表示 (ドラッグで連続切り替え)"
    } else {
        "トラックを非表示 (ドラッグで連続切り替え)"
    })
}

/// ルーラー。押す・ドラッグで playhead を動かし、マーカーの近くを押したらマーカーを選ぶ。
/// 右クリックでマーカーとイン・アウトを消せる。
fn draw_ruler(
    ui: &mut egui::Ui,
    session: &mut Session,
    state: &mut TimelineState,
    rect: Rect,
    origin_x: f32,
    window: TimeWindow,
    ppf: f32,
) {
    let response = ui
        .interact(
            rect,
            ui.id().with("timeline_ruler"),
            Sense::click_and_drag(),
        )
        .on_hover_cursor(egui::CursorIcon::ResizeHorizontal);
    if let Some(pointer) = response.interact_pointer_pos() {
        let frame = frame_at(pointer.x, origin_x, ppf);
        if response.secondary_clicked() {
            state.background_menu = Some((None, frame));
        } else if response.clicked() || response.dragged() {
            let markers = session.project().timeline_markers.clone();
            let local = f64::from(pointer.x - origin_x);
            let marker = response
                .clicked()
                .then(|| marker_near_ruler_x(local, f64::from(ppf), &markers, 8.0))
                .flatten();
            if let Some(marker) = marker {
                session.select_marker(Some(marker));
                session.set_scrubbing(false);
            } else {
                session.set_scrubbing(response.dragged());
                session.seek(frame);
            }
        }
    }
    if response.drag_stopped() {
        session.set_scrubbing(false);
    }
    response.context_menu(|ui| background_menu(ui, session, state));

    let project = session.project();
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 0.0, theme::STRIP);
    let x_of = |frame: i64| origin_x + frame as f32 * ppf;
    let fps = project.timeline_fps;
    let nominal = if fps.den > 0 { fps.num / fps.den } else { 1 }.max(1);
    let step = ruler_step(ppf, nominal);
    if step > 0 {
        let mut tick = window.first.saturating_div(step).saturating_mul(step);
        if tick < window.first {
            tick = tick.saturating_add(step);
        }
        let font = egui::FontId::proportional(10.0);
        while tick < window.last {
            let x = x_of(tick);
            painter.line_segment(
                [Pos2::new(x, rect.max.y - 10.0), Pos2::new(x, rect.max.y)],
                Stroke::new(1.0, theme::RULER_TICK),
            );
            painter.text(
                Pos2::new(x + 4.0, rect.min.y + 1.0),
                Align2::LEFT_TOP,
                ruler_label(tick, nominal),
                font.clone(),
                theme::RULER_TEXT,
            );
            let next = tick.saturating_add(step);
            if next <= tick {
                break;
            }
            tick = next;
        }
    }
    for mark in [project.in_frame, project.out_frame].into_iter().flatten() {
        painter.line_segment(
            [
                Pos2::new(x_of(mark), rect.min.y),
                Pos2::new(x_of(mark), rect.max.y),
            ],
            Stroke::new(1.0, theme::IN_OUT),
        );
    }
    if let (Some(mark_in), Some(mark_out)) = (project.in_frame, project.out_frame)
        && mark_out > mark_in
    {
        painter.rect_filled(
            Rect::from_min_max(
                Pos2::new(x_of(mark_in), rect.max.y - 4.0),
                Pos2::new(x_of(mark_out).max(x_of(mark_in) + 1.0), rect.max.y),
            ),
            0.0,
            theme::IN_OUT,
        );
    }
    for frame in &project.timeline_markers {
        let x = x_of(*frame);
        let selected = session.selected_marker() == Some(*frame);
        let points = vec![
            Pos2::new(x - 4.0, rect.min.y + 2.0),
            Pos2::new(x + 4.0, rect.min.y + 2.0),
            Pos2::new(x + 4.0, rect.min.y + 10.0),
            Pos2::new(x, rect.min.y + 15.0),
            Pos2::new(x - 4.0, rect.min.y + 10.0),
        ];
        let stroke = if selected {
            Stroke::new(1.5, Color32::WHITE)
        } else {
            Stroke::NONE
        };
        painter.add(egui::Shape::convex_polygon(points, theme::MARKER, stroke));
    }
    let head = x_of(session.playhead_frame());
    painter.rect_filled(
        Rect::from_min_size(Pos2::new(head - 6.0, rect.min.y), Vec2::new(12.0, 9.0)),
        0.0,
        theme::PLAYHEAD,
    );
    painter.line_segment(
        [Pos2::new(head, rect.min.y), Pos2::new(head, rect.max.y)],
        Stroke::new(2.0, theme::PLAYHEAD),
    );
}

/// ルーラーの目盛りの文字。秒の区切りは「分:秒」、その間は「分:秒+frame」。
fn ruler_label(frame: i64, nominal_fps: i64) -> String {
    let nominal_fps = nominal_fps.max(1);
    let seconds = frame / nominal_fps;
    let rest = frame % nominal_fps;
    let text = format!("{}:{:02}", seconds / 60, seconds % 60);
    if rest == 0 {
        text
    } else {
        format!("{text}+{rest}f")
    }
}

fn draw_contents(
    ui: &mut egui::Ui,
    session: &mut Session,
    state: &mut TimelineState,
    lanes: &Lanes,
    size: Vec2,
    window: TimeWindow,
    ppf: f32,
) {
    let (rect, response) = ui.allocate_exact_size(size, Sense::click_and_drag());
    background(ui, session, state, &response, rect, ppf);
    if response.secondary_clicked()
        && let Some(pointer) = response.interact_pointer_pos()
    {
        let frame = frame_at(pointer.x, rect.min.x, ppf);
        let track = drop_target(pointer, rect, lanes, ppf).map(|(track, _)| track);
        state.background_menu = Some((track, frame));
    }
    response.context_menu(|ui| background_menu(ui, session, state));

    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 0.0, theme::STRIP);
    let mut interval_failed = false;
    let mut visible = Vec::new();
    for (row_index, (kind, track_index, _)) in lanes.rows.iter().enumerate() {
        let y = rect.min.y + lanes.row_top(row_index);
        let row_rect = Rect::from_min_size(
            Pos2::new(rect.min.x, y),
            Vec2::new(rect.width(), ROW_HEIGHT),
        );
        let fill = match kind {
            TrackKind::Video if row_index % 2 == 0 => theme::VIDEO_ROW,
            TrackKind::Video => theme::VIDEO_ROW_ALT,
            TrackKind::Audio => theme::AUDIO_ROW,
        };
        painter.rect(
            row_rect,
            0.0,
            fill,
            Stroke::new(1.0, theme::ROW_BORDER),
            egui::StrokeKind::Inside,
        );
        for clip in &session.project().timeline_clips {
            if clip.track.kind != *kind || clip.track.index != *track_index {
                continue;
            }
            match clip_interval(session.project(), clip) {
                Ok((start, end)) if ranges_overlap(start, end, window) => {
                    visible.push(VisibleClip {
                        id: clip.id.clone(),
                        name: clip.name.clone(),
                        detail: clip_detail(clip, end - start),
                        enabled: clip.enabled,
                        kind: clip.track.kind,
                        track_index: clip.track.index,
                        y,
                        start,
                        end,
                    });
                }
                Ok(_) => {}
                Err(_) => interval_failed = true,
            }
        }
    }
    painter.rect_filled(
        Rect::from_min_size(
            Pos2::new(
                rect.min.x,
                rect.min.y + lanes.row_top(lanes.video_count()) - 2.0,
            ),
            Vec2::new(rect.width(), 4.0),
        ),
        0.0,
        theme::ROW_DIVIDER,
    );
    let project = session.project();
    if let (Some(mark_in), Some(mark_out)) = (project.in_frame, project.out_frame)
        && mark_out > mark_in
    {
        painter.rect_filled(
            Rect::from_min_max(
                Pos2::new(rect.min.x + mark_in as f32 * ppf, rect.min.y),
                Pos2::new(rect.min.x + mark_out as f32 * ppf, rect.max.y),
            ),
            0.0,
            Color32::from_white_alpha(18),
        );
    }
    for clip in visible {
        draw_clip(ui, session, state, &clip, rect.min.x, ppf, &lanes.rows);
    }
    if lanes.subtitles.is_some() {
        let lane = Rect::from_min_size(
            Pos2::new(rect.min.x, rect.min.y + lanes.subtitle_top()),
            Vec2::new(rect.width(), ROW_HEIGHT),
        );
        subtitle_lane::draw(ui, session, &mut state.subtitle_gesture, lane, window, ppf);
    }
    draw_playhead(ui, rect, session.playhead_frame(), ppf);
    receive_media(ui, session, rect, lanes, ppf);
    if interval_failed {
        ui.label("区間を計算できない clip があります");
    }
}

/// clip の 2 行目。速度を変えていればその割合と、素材の fps・長さ。
fn clip_detail(clip: &mvm_project::TimelineClip, frames: i64) -> String {
    let speed = clip.speed;
    let speed = if speed.is_positive() && speed.num != speed.den {
        format!(
            "{}%  |  ",
            (speed.num as f64 / speed.den as f64 * 10_000.0).round() / 100.0
        )
    } else {
        String::new()
    };
    let fps = clip.source_fps;
    format!("{speed}{}/{} fps  |  {frames}f", fps.num, fps.den)
}

// clip の右クリック。選択中の clip に対して働く（右クリックした clip が選択に無ければ、それだけを選ぶ）。
fn clip_menu(ui: &mut egui::Ui, session: &mut Session, state: &mut TimelineState, id: &str) {
    let Some(clip) = session.project().clip(id).cloned() else {
        return;
    };
    if ui.button("コピー").clicked() {
        report!(session, session.copy_selection());
        ui.close();
    }
    if ui.button("カット").clicked() {
        report!(session, session.cut_selection());
        ui.close();
    }
    if ui.button("複製").clicked() {
        report!(session, session.duplicate_selection());
        ui.close();
    }
    ui.separator();
    if !clip.has_synthetic_source_domain() && ui.button("速度・デュレーション…").clicked()
    {
        state.speed_clip = Some(clip.id.clone());
        ui.close();
    }
    if ui
        .add_enabled(
            session.can_insert_frame_hold(&clip.id),
            egui::Button::new("フレーム保持を挿入"),
        )
        .clicked()
    {
        report!(session, session.insert_frame_hold(&clip.id));
        ui.close();
    }
    if matches!(
        clip.kind,
        mvm_project::TimelineClipKind::Audio
            | mvm_project::TimelineClipKind::Video
            | mvm_project::TimelineClipKind::Manim
    ) && ui.button("自動字幕を生成").clicked()
    {
        state.transcribe_clip = Some(clip.id.clone());
        ui.close();
    }
    if matches!(
        clip.kind,
        mvm_project::TimelineClipKind::Video | mvm_project::TimelineClipKind::Manim
    ) && ui.button("プロジェクト設定をこの素材に合わせる").clicked()
    {
        state.match_settings_clip = Some(clip.id.clone());
        ui.close();
    }
    if clip.is_linked() && ui.button("リンクを解除").clicked() {
        report!(session, session.unlink_clip(&clip.id));
        ui.close();
    }
    ui.separator();
    if ui.button("削除").clicked() {
        report!(session, session.delete_selection());
        ui.close();
    }
    if ui.button("リップル削除（全トラック）").clicked() {
        report!(session, session.ripple_delete_selection());
        ui.close();
    }
}

/// 空白・ルーラーの右クリック。空白は詰められ、マーカーとイン・アウトを消せる。
fn background_menu(ui: &mut egui::Ui, session: &mut Session, state: &mut TimelineState) {
    let Some((track, frame)) = state.background_menu else {
        ui.close();
        return;
    };
    let mut shown = false;
    if let Some(track) = track {
        shown = true;
        if ui.button("リップル削除（空白を詰める）").clicked() {
            report!(session, session.ripple_delete_gap(track, frame));
            ui.close();
        }
    }
    if session.has_clipboard() {
        shown = true;
        if ui.button("ペースト").clicked() {
            session.seek(frame);
            report!(session, session.paste());
            ui.close();
        }
    }
    if let Some(marker) = session.selected_marker() {
        shown = true;
        if ui.button("マーカーを削除").clicked() {
            report!(session, session.delete_marker(marker));
            ui.close();
        }
    }
    let (has_in, has_out) = {
        let project = session.project();
        (project.in_frame.is_some(), project.out_frame.is_some())
    };
    if has_in || has_out {
        shown = true;
        if has_in && ui.button("インを消去").clicked() {
            report!(session, session.clear_mark_in());
            ui.close();
        }
        if has_out && ui.button("アウトを消去").clicked() {
            report!(session, session.clear_mark_out());
            ui.close();
        }
        if ui.button("イン・アウトを消去").clicked() {
            report!(session, session.clear_marks());
            ui.close();
        }
    }
    if !shown {
        ui.label("操作できる項目がありません");
    }
}

/// track の行の上の位置を、置き先の track と frame にする。字幕の行・外側は `None`。
fn drop_target(pointer: Pos2, rect: Rect, lanes: &Lanes, ppf: f32) -> Option<(TrackRef, i64)> {
    if !rect.contains(pointer) {
        return None;
    }
    let row = lanes.row_at(pointer.y - rect.min.y)?;
    let (kind, index, _) = lanes.rows.get(row)?;
    Some((
        TrackRef::new(*kind, *index),
        frame_at(pointer.x, rect.min.x, ppf),
    ))
}

/// プロジェクトパネルからのドラッグを受ける。ドラッグ中は置く位置を枠で見せる。
/// OS から落とされたファイルは受けない（ドラッグ中の pointer の位置が届かず、置く行を確かめられないため）。
fn receive_media(ui: &mut egui::Ui, session: &mut Session, rect: Rect, lanes: &Lanes, ppf: f32) {
    if !egui::DragAndDrop::has_payload_of_type::<MediaDrag>(ui.ctx()) {
        return;
    }
    let pointer = ui.input(|input| input.pointer.latest_pos());
    let target = pointer.and_then(|pointer| drop_target(pointer, rect, lanes, ppf));
    let Some((track, frame)) = target else {
        return;
    };
    let row = lanes
        .rows
        .iter()
        .position(|(kind, index, _)| *kind == track.kind && *index == track.index)
        .unwrap_or_default();
    let x = rect.min.x + frame as f32 * ppf;
    let y = rect.min.y + lanes.row_top(row);
    ui.painter().rect_stroke(
        Rect::from_min_size(Pos2::new(x, y + 2.0), Vec2::new(48.0, ROW_HEIGHT - 4.0)),
        3.0,
        Stroke::new(2.0, theme::SELECTED_BORDER),
        egui::StrokeKind::Inside,
    );
    if ui.input(|input| input.pointer.any_released())
        && let Some(drag) = egui::DragAndDrop::take_payload::<MediaDrag>(ui.ctx())
    {
        report!(session, session.place_media_items_at(&drag.0, track, frame));
    }
}

fn background(
    ui: &mut egui::Ui,
    session: &mut Session,
    state: &mut TimelineState,
    response: &egui::Response,
    rect: Rect,
    ppf: f32,
) {
    let tool = session.tool();
    if tool == TimelineTool::Hand && response.dragged() {
        ui.scroll_with_delta(response.drag_delta());
        return;
    }
    if tool == TimelineTool::Zoom && response.clicked() {
        let alt = ui.input(|input| input.modifiers.alt);
        let anchor = response
            .interact_pointer_pos()
            .map_or(0.0, |pointer| pointer.x - ui.clip_rect().min.x);
        zoom_at(session, state, if alt { -1 } else { 1 }, anchor);
        return;
    }
    if matches!(
        tool,
        TimelineTool::Text | TimelineTool::Pen | TimelineTool::Hand
    ) {
        return;
    }
    let Some(pointer) = response.interact_pointer_pos() else {
        if response.drag_stopped() {
            session.set_scrubbing(false);
        }
        return;
    };
    if !(response.dragged() || response.clicked()) {
        if response.drag_stopped() {
            session.set_scrubbing(false);
        }
        return;
    }
    let frame = frame_at(pointer.x, rect.min.x, ppf);
    if tool == TimelineTool::Razor && response.clicked() && ui.input(|input| input.modifiers.shift)
    {
        let ids = clip_ids_spanning_frame(session.project(), frame);
        let linked = !ui.input(|input| input.modifiers.alt);
        report!(session, session.split_at(&ids, frame, linked));
        return;
    }
    session.set_scrubbing(response.dragged());
    session.seek(frame);
    if response.drag_stopped() {
        session.set_scrubbing(false);
    }
}

struct VisibleClip {
    id: String,
    name: String,
    detail: String,
    enabled: bool,
    kind: TrackKind,
    track_index: usize,
    y: f32,
    start: i64,
    end: i64,
}

fn draw_clip(
    ui: &mut egui::Ui,
    session: &mut Session,
    state: &mut TimelineState,
    clip: &VisibleClip,
    origin_x: f32,
    ppf: f32,
    rows: &[(TrackKind, usize, String)],
) {
    let x = origin_x + clip.start as f32 * ppf;
    let width = ((clip.end - clip.start) as f32 * ppf).max(2.0);
    let rect = Rect::from_min_size(
        Pos2::new(x, clip.y + CLIP_INSET),
        Vec2::new(width, ROW_HEIGHT - 2.0 * CLIP_INSET),
    );
    let selected = session.selection().iter().any(|id| id == &clip.id);
    let response = ui.interact(
        rect,
        ui.id().with(("timeline_clip", &clip.id)),
        Sense::click_and_drag(),
    );
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, &clip.name));
    paint_clip(ui, session, clip, rect, selected, ppf);
    if response.secondary_clicked() && !selected {
        session.select_only(clip.id.clone());
    }
    response.context_menu(|ui| clip_menu(ui, session, state, &clip.id));
    let gesture = &mut state.gesture;
    draw_transitions(ui, session, clip, origin_x, ppf);
    draw_keys(ui, session, clip, rect, ppf);
    if let Some((id, edge)) = session.selected_edge()
        && id == clip.id
    {
        let x = match edge {
            TrimEdge::Left => rect.min.x,
            TrimEdge::Right => rect.max.x,
        };
        ui.painter().line_segment(
            [Pos2::new(x, rect.min.y), Pos2::new(x, rect.max.y)],
            Stroke::new(3.0, theme::KEY_LINE),
        );
    }
    if let Some(active) = gesture
        .as_ref()
        .filter(|active| active.moved && active.clip_id == clip.id)
    {
        draw_ghost(ui, active, rect);
    }
    // 押した瞬間に始める。drag_started はクリックの閾値を超えるまで来ないので、
    // それを待つとクリック（選択・レーザー）が gesture を持たず、ドラッグの起点もずれる。
    if response.is_pointer_button_down_on()
        && ui.input(|input| input.pointer.primary_pressed())
        && let Some(pointer) = ui.input(|input| input.pointer.press_origin())
    {
        *gesture = begin_gesture(ui, session, clip, pointer, origin_x, ppf, rect);
    }
    if response.dragged()
        && let Some(active) = gesture.as_mut().filter(|active| active.clip_id == clip.id)
        && let Some(pointer) = response.interact_pointer_pos()
    {
        active.pointer = pointer;
        if active.origin.distance(pointer) > 4.0 {
            active.moved = true;
        }
    }
    // 離した frame は全 clip で真になるので、自分の gesture だけを取り出す。
    if ui.input(|input| input.pointer.primary_released())
        && let Some(active) = gesture.take_if(|active| active.clip_id == clip.id)
    {
        commit_gesture(ui, session, active, origin_x, ppf, rows, rect);
    }
}

/// clip の本体。無効にした clip は timeline に残したまま暗くする。
fn paint_clip(
    ui: &egui::Ui,
    session: &mut Session,
    clip: &VisibleClip,
    rect: Rect,
    selected: bool,
    ppf: f32,
) {
    let opacity = if clip.enabled { 1.0 } else { 0.4 };
    let fill = if selected {
        theme::SELECTED
    } else if clip.kind == TrackKind::Audio {
        theme::CLIP_AUDIO
    } else {
        theme::CLIP_VIDEO
    };
    ui.painter().rect(
        rect,
        3.0,
        fill.gamma_multiply(opacity),
        Stroke::new(1.0, theme::SELECTED_BORDER.gamma_multiply(opacity)),
        egui::StrokeKind::Inside,
    );
    if clip.kind == TrackKind::Audio {
        draw_waveform(ui, session, &clip.id, rect, ppf, selected);
    }
    let tool = session.tool();
    if edge_tool(tool) && rect.width() >= 24.0 {
        let color = edge_handle_color(tool).gamma_multiply(opacity);
        let handle = Vec2::new(EDGE_PIXELS, rect.height());
        ui.painter()
            .rect_filled(Rect::from_min_size(rect.min, handle), 2.0, color);
        ui.painter().rect_filled(
            Rect::from_min_size(Pos2::new(rect.max.x - EDGE_PIXELS, rect.min.y), handle),
            2.0,
            color,
        );
    }
    if rect.width() < 40.0 {
        return;
    }
    let text = ui.painter_at(rect.shrink2(Vec2::new(12.0, 0.0)));
    let origin = rect.min + Vec2::new(12.0, 5.0);
    text.text(
        origin,
        Align2::LEFT_TOP,
        &clip.name,
        egui::FontId::proportional(12.0),
        Color32::WHITE.gamma_multiply(opacity),
    );
    if clip.kind != TrackKind::Audio {
        text.text(
            origin + Vec2::new(0.0, 17.0),
            Align2::LEFT_TOP,
            &clip.detail,
            egui::FontId::proportional(10.0),
            theme::CLIP_DETAIL.gamma_multiply(opacity),
        );
    }
}

/// 端のハンドルの色。ツールごとに端の操作が違うことを色で見せる。
fn edge_handle_color(tool: TimelineTool) -> Color32 {
    match tool {
        TimelineTool::Ripple => Color32::from_rgb(0xe8, 0xc1, 0x5a),
        TimelineTool::Rolling => Color32::from_rgb(0xe2, 0x7d, 0x6a),
        TimelineTool::Rate => Color32::from_rgb(0xb9, 0x9a, 0xf0),
        _ => Color32::from_rgb(0x85, 0xc4, 0xee),
    }
}

/// 音声 clip の波形。見えている列だけを 2 pixel ごとに描く。素材の時刻は clip の素材 in と速度から求める。
fn draw_waveform(
    ui: &egui::Ui,
    session: &mut Session,
    clip_id: &str,
    rect: Rect,
    ppf: f32,
    selected: bool,
) {
    let Some(clip) = session.project().clip(clip_id) else {
        return;
    };
    let timeline_fps = session.project().timeline_fps;
    let (source_fps, speed) = (clip.source_fps, clip.speed);
    if !timeline_fps.is_positive() || !source_fps.is_positive() || !speed.is_positive() {
        return;
    }
    let source_in = clip.source_in_frame as f64 * source_fps.den as f64 / source_fps.num as f64;
    let speed = speed.num as f64 / speed.den as f64;
    let seconds_per_frame = timeline_fps.den as f64 / timeline_fps.num as f64;
    let path = clip.media_path.clone();
    let Some(peaks) = session.waveform(&path) else {
        return;
    };
    let visible = rect.intersect(ui.clip_rect());
    if visible.width() <= 0.0 {
        return;
    }
    let painter = ui.painter_at(rect);
    let color = if selected {
        theme::WAVEFORM_SELECTED
    } else {
        theme::WAVEFORM
    };
    let middle = rect.center().y;
    let half = rect.height() / 2.0 - 2.0;
    let mut x = visible.min.x.floor();
    while x < visible.max.x {
        let local = |px: f32| f64::from((px - rect.min.x) / ppf) * seconds_per_frame;
        let begin = source_in + local(x) * speed;
        let end = source_in + local(x + 2.0) * speed;
        let mut column: Option<(f32, f32)> = None;
        for channel in 0..peaks.channels {
            if let Some(found) =
                mvm_core::waveform_peaks::waveform_column(&peaks, channel, begin, end)
            {
                let (low, high) = column.unwrap_or((found.minimum, found.maximum));
                column = Some((low.min(found.minimum), high.max(found.maximum)));
            }
        }
        if let Some((low, high)) = column {
            painter.line_segment(
                [
                    Pos2::new(x, middle - high * half),
                    Pos2::new(x, middle - low * half),
                ],
                Stroke::new(1.5, color),
            );
        }
        x += 2.0;
    }
}

/// ドラッグ中の行き先。Project は変えず、離したときの command と同じ形を枠で見せる。
fn draw_ghost(ui: &egui::Ui, active: &TimelineGesture, rect: Rect) {
    let delta = active.pointer - active.origin;
    let ghost = if active.pen.is_some() {
        ui.painter()
            .circle_filled(active.pointer, 4.0, theme::KEY_LINE);
        return;
    } else if let Some(edge) = active.edge {
        match edge {
            TrimEdge::Left => {
                Rect::from_min_max(Pos2::new(rect.min.x + delta.x, rect.min.y), rect.max)
            }
            TrimEdge::Right => {
                Rect::from_min_max(rect.min, Pos2::new(rect.max.x + delta.x, rect.max.y))
            }
        }
    } else {
        rect.translate(delta)
    };
    ui.painter().rect_stroke(
        ghost,
        3.0,
        Stroke::new(1.5, Color32::from_rgba_unmultiplied(230, 230, 230, 200)),
        egui::StrokeKind::Inside,
    );
}

/// 出ていく clip の行に、切れ目の前後の区間を重ねる。
fn draw_transitions(ui: &egui::Ui, session: &Session, clip: &VisibleClip, origin_x: f32, ppf: f32) {
    for transition in &session.project().timeline_transitions {
        if transition.outgoing_clip_id != clip.id {
            continue;
        }
        let first = clip.end.saturating_sub(transition.frames_before_cut);
        let last = clip.end.saturating_add(transition.frames_after_cut);
        let band = Rect::from_min_max(
            Pos2::new(origin_x + first as f32 * ppf, clip.y + CLIP_INSET),
            Pos2::new(
                origin_x + last as f32 * ppf,
                clip.y + ROW_HEIGHT - CLIP_INSET,
            ),
        );
        ui.painter().rect(
            band,
            2.0,
            Color32::from_rgba_unmultiplied(0xb5, 0x8a, 0x43, 110),
            Stroke::new(1.0, Color32::from_rgb(0xb5, 0x8a, 0x43)),
            egui::StrokeKind::Inside,
        );
        ui.painter().line_segment(
            [band.left_bottom(), band.right_top()],
            Stroke::new(1.0, Color32::from_rgb(0xf0, 0xc3, 0x6a)),
        );
    }
}

/// 不透明度（音声は音量）の線とキー。画面に入っている区間だけを描く。
fn draw_keys(ui: &egui::Ui, session: &Session, clip: &VisibleClip, rect: Rect, ppf: f32) {
    let Some(item) = session.project().clip(&clip.id) else {
        return;
    };
    let kind = key_kind(item);
    let stored = item.effects.keys(kind);
    let keys: Vec<PenKey> = stored
        .iter()
        .map(|key| PenKey {
            frame: key.frame,
            value: key.value,
            samples: Vec::new(),
        })
        .collect();
    let geometry = pen_geometry(&rect, ppf, kind);
    let visible = ui.clip_rect();
    let left = f64::from(visible.min.x - rect.min.x);
    let right = f64::from(visible.max.x - rect.min.x);
    let to_screen = |x: f64, y: f64| Pos2::new(rect.min.x + x as f32, rect.min.y + y as f32);
    let line: Vec<Pos2> =
        pen_visible_line_points(&keys, item.effects.base(kind), &geometry, left, right)
            .iter()
            .map(|point| to_screen(point.x, point.y))
            .collect();
    // 暗い縁取りで、明るい波形の上でも読めるようにする。
    ui.painter().add(egui::Shape::line(
        line.clone(),
        Stroke::new(4.0, Color32::from_black_alpha(166)),
    ));
    ui.painter()
        .add(egui::Shape::line(line, Stroke::new(2.0, theme::KEY_LINE)));
    for key in pen_visible_keys(&keys, &geometry, left, right) {
        let x = key.frame as f64 * geometry.pixels_per_frame;
        let y = mvm_project::timeline::pen_y(&geometry, key.value);
        ui.painter().rect_filled(
            Rect::from_center_size(to_screen(x, y), Vec2::splat(8.0)),
            0.0,
            Color32::from_rgb(0x3d, 0x8b, 0xff),
        );
    }
}

fn key_kind(clip: &mvm_project::TimelineClip) -> mvm_project::clip_effects::ClipKeyKind {
    if clip.kind == mvm_project::TimelineClipKind::Audio {
        mvm_project::clip_effects::ClipKeyKind::Volume
    } else {
        mvm_project::clip_effects::ClipKeyKind::Opacity
    }
}

fn begin_gesture(
    ui: &egui::Ui,
    session: &mut Session,
    clip: &VisibleClip,
    pointer: Pos2,
    origin_x: f32,
    ppf: f32,
    rect: Rect,
) -> Option<TimelineGesture> {
    let tool = session.tool();
    let mods = gesture_modifiers(ui);
    let frame = frame_at(pointer.x, origin_x, ppf);
    if matches!(
        tool,
        TimelineTool::TrackForward | TimelineTool::TrackBackward
    ) {
        let track = mods
            .shift
            .then_some(TrackRef::new(clip.kind, clip.track_index));
        session.select_track_from_playhead(frame, tool == TimelineTool::TrackForward, track);
        return None;
    }
    if matches!(
        tool,
        TimelineTool::Hand | TimelineTool::Zoom | TimelineTool::Text
    ) {
        return None;
    }
    if tool == TimelineTool::Pen {
        return begin_pen(session, clip, pointer, rect, ppf, mods);
    }
    let edge = edge_tool(tool).then(|| edge_at(pointer.x, rect)).flatten();
    let body = edge.is_none().then(|| body_press(tool, mods, frame));
    Some(TimelineGesture {
        clip_id: clip.id.clone(),
        press_frame: frame,
        origin: pointer,
        pointer,
        moved: false,
        linked: !mods.alt,
        body,
        edge,
        pen: None,
    })
}

fn begin_pen(
    session: &mut Session,
    clip: &VisibleClip,
    pointer: Pos2,
    rect: Rect,
    ppf: f32,
    mods: GestureModifiers,
) -> Option<TimelineGesture> {
    let item = session.project().clip(&clip.id).cloned()?;
    let kind = key_kind(&item);
    let geometry = pen_geometry(&rect, ppf, kind);
    let keys: Vec<PenKey> = item
        .effects
        .keys(kind)
        .iter()
        .map(|key| PenKey {
            frame: key.frame,
            value: key.value,
            samples: Vec::new(),
        })
        .collect();
    let local_x = f64::from(pointer.x - rect.min.x);
    let local_y = f64::from(pointer.y - rect.min.y);
    let frame = (local_x / f64::from(ppf.max(MIN_PIXELS_PER_FRAME))).floor() as i64;
    let press = pen_press(
        &keys,
        item.effects.base(kind),
        frame.max(0),
        local_x,
        local_y,
        &geometry,
        mods,
    );
    match press {
        PenPress::DeleteKey { frame } => {
            report!(session, session.delete_key(&clip.id, kind, frame));
            None
        }
        PenPress::Select => {
            session.select_only(clip.id.clone());
            None
        }
        PenPress::None => None,
        PenPress::Edit { .. } => Some(TimelineGesture {
            clip_id: clip.id.clone(),
            press_frame: frame.max(0),
            origin: pointer,
            pointer,
            moved: false,
            linked: !mods.alt,
            body: None,
            edge: None,
            pen: Some(press),
        }),
    }
}

fn commit_gesture(
    ui: &egui::Ui,
    session: &mut Session,
    gesture: TimelineGesture,
    origin_x: f32,
    ppf: f32,
    rows: &[(TrackKind, usize, String)],
    rect: Rect,
) {
    let Some(clip) = session.project().clip(&gesture.clip_id).cloned() else {
        return;
    };
    let mods = gesture_modifiers(ui);
    if let Some(pen) = &gesture.pen {
        commit_pen(session, &clip, pen, &gesture, rect, ppf, mods);
        return;
    }
    if let Some(edge) = gesture.edge {
        let release = frame_at(gesture.pointer.x, origin_x, ppf);
        let delta = release.saturating_sub(gesture.press_frame);
        match edge_release(session.tool(), edge, delta, gesture.linked) {
            EdgeAction::SelectEdit { edge } => session.select_edge(clip.id, edge),
            EdgeAction::Trim {
                edge,
                delta,
                linked,
            } => {
                report!(session, session.trim_edge(&clip.id, edge, delta, linked));
            }
            EdgeAction::RippleTrim {
                edge,
                delta,
                linked,
            } => {
                report!(session, session.ripple_edge(&clip.id, edge, delta, linked));
            }
            EdgeAction::Roll {
                edge,
                delta,
                linked,
            } => {
                report!(session, session.roll_edge(&clip.id, edge, delta, linked));
            }
            EdgeAction::RateStretch {
                edge,
                delta,
                linked,
            } => {
                report!(session, session.rate_edge(&clip.id, edge, delta, linked));
            }
            EdgeAction::None => {}
        }
        return;
    }
    let Some(body) = gesture.body else {
        return;
    };
    let release = frame_at(gesture.pointer.x, origin_x, ppf);
    let drag = release.saturating_sub(gesture.press_frame);
    let (destination, start) = moved_destination(session, &clip, &gesture, ppf, rows);
    let action = body_release(&body, gesture.moved, start, release, drag);
    match action {
        BodyAction::Split {
            frame,
            all_tracks,
            linked,
        } => {
            let ids = if all_tracks {
                clip_ids_spanning_frame(session.project(), frame)
            } else {
                vec![clip.id.clone()]
            };
            report!(session, session.split_at(&ids, frame, linked));
        }
        BodyAction::Slip { delta, linked } => {
            report!(session, session.slip_clip(&clip.id, delta, linked));
        }
        BodyAction::Slide { delta, linked } => {
            report!(session, session.slide_clip(&clip.id, delta, linked));
        }
        BodyAction::Move { frame, linked } => {
            report!(
                session,
                session.move_selection(&clip.id, destination, frame, linked),
            );
        }
        BodyAction::Duplicate { frame, .. } => {
            report!(session, session.duplicate_at(&clip.id, destination, frame));
        }
        BodyAction::Toggle { .. } => session.toggle_selected(clip.id),
        BodyAction::Select { linked, .. } => {
            if linked && !clip.link_group_id.is_empty() {
                let ids = session
                    .project()
                    .timeline_clips
                    .iter()
                    .filter(|other| other.link_group_id == clip.link_group_id)
                    .map(|other| other.id.clone())
                    .collect();
                session.select_ids(ids);
            } else {
                session.select_only(clip.id);
            }
        }
        BodyAction::None => {}
    }
}

fn commit_pen(
    session: &mut Session,
    clip: &mvm_project::TimelineClip,
    pen: &PenPress,
    gesture: &TimelineGesture,
    rect: Rect,
    ppf: f32,
    mods: GestureModifiers,
) {
    let PenPress::Edit {
        grab_offset_x,
        grab_offset_y,
        ..
    } = pen
    else {
        return;
    };
    let kind = key_kind(clip);
    let geometry = pen_geometry(&rect, ppf, kind);
    let x = f64::from(gesture.pointer.x - rect.min.x) + grab_offset_x;
    let y = f64::from(gesture.pointer.y - rect.min.y) + grab_offset_y;
    let frame = (x / f64::from(ppf.max(MIN_PIXELS_PER_FRAME))).round() as i64;
    let value = pen_snap_value(pen_value_at(&geometry, y), mods);
    if let Some(release) = pen_release(pen, frame, value) {
        report!(
            session,
            session.edit_key(
                &clip.id,
                kind,
                release.original_frame,
                release.frame,
                release.value,
            ),
        );
    }
}

fn moved_destination(
    session: &Session,
    clip: &mvm_project::TimelineClip,
    gesture: &TimelineGesture,
    ppf: f32,
    rows: &[(TrackKind, usize, String)],
) -> (TrackRef, i64) {
    let offset_x = f64::from(gesture.pointer.x - gesture.origin.x);
    let bounds = selection_bounds(session, &clip.id);
    let bounded = group_drag_offset_x(offset_x, f64::from(ppf), bounds);
    let spans = clip_spans(session);
    let moving = moving_ids(session, &clip.id);
    let moving_refs: Vec<&str> = moving.iter().map(String::as_str).collect();
    let snap = drag_snap_frames(&spans, &moving_refs, session.playhead_frame());
    let snapped = snap_drag_offset_x(bounded, f64::from(ppf), &snap, 8.0);
    let delta = if ppf.is_finite() && ppf > 0.0 {
        (snapped.offset_x / f64::from(ppf)).round() as i64
    } else {
        0
    };
    let start = clip.timeline_start_frame.saturating_add(delta).max(0);
    let Some(anchor_row) = rows
        .iter()
        .position(|(kind, index, _)| *kind == clip.track.kind && *index == clip.track.index)
    else {
        return (clip.track, start);
    };
    let row_delta = ((gesture.pointer.y - gesture.origin.y) / ROW_HEIGHT).round() as isize;
    let target = anchor_row as isize + row_delta;
    let Some((kind, snapped_index, _)) = target
        .try_into()
        .ok()
        .and_then(|index: usize| rows.get(index))
        .filter(|(kind, _, _)| *kind == clip.track.kind)
    else {
        return (clip.track, start);
    };
    let count = i64::try_from(session.project().tracks(*kind).len()).unwrap_or(0);
    let anchor = i64::try_from(clip.track.index).unwrap_or(0);
    let snapped_index = i64::try_from(*snapped_index).unwrap_or(anchor);
    let index = group_drag_track_index(
        *kind == TrackKind::Video,
        anchor,
        snapped_index,
        count,
        bounds,
    );
    let index = usize::try_from(index).unwrap_or(clip.track.index);
    let kind = *kind;
    if session
        .project()
        .is_valid_track_ref(TrackRef::new(kind, index))
    {
        (TrackRef::new(kind, index), start)
    } else {
        (clip.track, start)
    }
}

fn selection_bounds(session: &Session, anchor_id: &str) -> DragBounds {
    let use_selection = session.selection().iter().any(|id| id == anchor_id);
    let mut min_start = None;
    let mut video: Option<(i64, i64)> = None;
    let mut audio: Option<(i64, i64)> = None;
    for clip in &session.project().timeline_clips {
        let included = if use_selection {
            session.selection().iter().any(|id| id == &clip.id)
        } else {
            clip.id == anchor_id
        };
        if !included {
            continue;
        }
        min_start = Some(
            min_start
                .unwrap_or(clip.timeline_start_frame)
                .min(clip.timeline_start_frame),
        );
        let index = i64::try_from(clip.track.index).unwrap_or(0);
        let range = match clip.track.kind {
            TrackKind::Video => &mut video,
            TrackKind::Audio => &mut audio,
        };
        *range = Some(match *range {
            Some((low, high)) => (low.min(index), high.max(index)),
            None => (index, index),
        });
    }
    DragBounds {
        min_start_frame: min_start,
        video,
        audio,
    }
}

fn clip_spans(session: &Session) -> Vec<ClipSpan> {
    session
        .project()
        .timeline_clips
        .iter()
        .filter_map(|clip| {
            let (start, end) = clip_interval(session.project(), clip).ok()?;
            Some(ClipSpan {
                clip_id: clip.id.clone(),
                start,
                end,
            })
        })
        .collect()
}

fn moving_ids(session: &Session, anchor_id: &str) -> Vec<String> {
    if session.selection().iter().any(|id| id == anchor_id) {
        session.selection().to_vec()
    } else {
        vec![anchor_id.to_owned()]
    }
}

fn pen_geometry(
    rect: &Rect,
    ppf: f32,
    kind: mvm_project::clip_effects::ClipKeyKind,
) -> PenGeometry {
    let maximum = if kind == mvm_project::clip_effects::ClipKeyKind::Volume {
        200.0
    } else {
        100.0
    };
    PenGeometry {
        pixels_per_frame: f64::from(ppf),
        maximum,
        width: f64::from(rect.width()),
        height: f64::from(rect.height()),
        inset: 4.0,
        key_pixels: 8.0,
        line_pixels: 12.0,
    }
}

fn edge_tool(tool: TimelineTool) -> bool {
    matches!(
        tool,
        TimelineTool::Select | TimelineTool::Ripple | TimelineTool::Rolling | TimelineTool::Rate
    )
}

fn edge_at(x: f32, rect: Rect) -> Option<TrimEdge> {
    let left = (x - rect.min.x).abs();
    let right = (x - rect.max.x).abs();
    if left <= EDGE_PIXELS && left <= right {
        Some(TrimEdge::Left)
    } else if right <= EDGE_PIXELS {
        Some(TrimEdge::Right)
    } else {
        None
    }
}

fn gesture_modifiers(ui: &egui::Ui) -> GestureModifiers {
    let modifiers = ui.input(|input| input.modifiers);
    GestureModifiers {
        alt: modifiers.alt,
        shift: modifiers.shift,
    }
}

fn draw_playhead(ui: &mut egui::Ui, rect: Rect, frame: i64, ppf: f32) {
    let x = rect.min.x + frame as f32 * ppf;
    ui.painter().line_segment(
        [Pos2::new(x, rect.min.y), Pos2::new(x, rect.max.y)],
        Stroke::new(2.0, theme::PLAYHEAD),
    );
}

fn track_rows(project: &Project) -> Vec<(TrackKind, usize, String)> {
    let mut rows = Vec::new();
    for (index, track) in project.video_tracks.iter().enumerate().rev() {
        rows.push((TrackKind::Video, index, track.name.clone()));
    }
    for (index, track) in project.audio_tracks.iter().enumerate() {
        rows.push((TrackKind::Audio, index, track.name.clone()));
    }
    rows
}

fn frame_at(x: f32, origin: f32, pixels_per_frame: f32) -> i64 {
    let local = f64::from((x - origin).max(0.0));
    let frame = local / f64::from(pixels_per_frame.max(MIN_PIXELS_PER_FRAME));
    if !frame.is_finite() || frame <= 0.0 {
        0
    } else if frame >= i64::MAX as f64 {
        i64::MAX
    } else {
        frame.floor() as i64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eye_paint_sets_every_passed_track_to_the_opposite_of_the_pressed_one() {
        // V3 を押す（今は表示）→ 非表示で塗る。V1 まで一気に動いても V2 を飛ばさない。
        let mut paint = EyePaint::begin(2, false);
        assert!(paint.muted);
        paint.reach(0);
        assert_eq!(paint.painted, [2, 0, 1]);
        assert!(paint.shown_muted(1, false));
        // 通っていない track は今の表示のまま。
        assert!(!paint.shown_muted(3, false));
        paint.reach(2);
        assert_eq!(paint.painted.len(), 3, "同じ track を 2 回足しました");
    }

    #[test]
    fn the_eye_row_under_the_pointer_counts_from_the_top_video_track() {
        // 行は上から V3・V2・V1。範囲の外は端の track。
        assert_eq!(video_index_at(10.0, 3), Some(2));
        assert_eq!(video_index_at(ROW_HEIGHT * 2.0 + 1.0, 3), Some(0));
        assert_eq!(video_index_at(-40.0, 3), Some(2));
        assert_eq!(video_index_at(ROW_HEIGHT * 9.0, 3), Some(0));
        assert_eq!(video_index_at(10.0, 0), None);
    }

    #[test]
    fn the_ruler_shows_minutes_and_seconds_and_frames_between_them() {
        assert_eq!(ruler_label(0, 60), "0:00");
        assert_eq!(ruler_label(120, 60), "0:02");
        assert_eq!(ruler_label(3_660, 60), "1:01");
        assert_eq!(ruler_label(75, 60), "0:01+15f");
        // fps が 0 でも割らない。
        assert_eq!(ruler_label(5, 0), "0:05");
    }

    #[test]
    fn lanes_put_the_subtitles_above_the_tracks() {
        let mut project = Project::default();
        let without = Lanes::new(&project);
        assert_eq!(without.tracks_top(), 28.0);
        // 既定は V2・V1・A1 の 3 行。行の外（+V の行・+A の余白）は track ではない。
        assert_eq!(without.row_at(27.0), None);
        assert_eq!(without.row_at(28.0), Some(0));
        assert_eq!(without.row_at(28.0 + 54.0 * 2.0 + 1.0), Some(2));
        assert_eq!(without.row_at(28.0 + 54.0 * 3.0 + 1.0), None);
        assert_eq!(without.video_count(), 2);

        project.subtitles = Some(mvm_project::SubtitleTrack::default());
        let with = Lanes::new(&project);
        assert_eq!(with.subtitle_top(), 28.0);
        assert_eq!(with.tracks_top(), 82.0);
        assert_eq!(with.row_at(60.0), None, "字幕の行を track として扱いました");
        assert_eq!(with.row_at(82.0), Some(0));
        assert_eq!(with.height(), 82.0 + 54.0 * 3.0 + 34.0);
    }
}
