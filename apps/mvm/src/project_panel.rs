//! プロジェクトパネル。素材の読み込み・フォルダ・名前の変更・削除・移動と、timeline へのドラッグを受け持つ。
//! 見た目は C++ 版の ProjectPanel（項目数・検索・列の見出し・開閉できる行）に揃える。
//!
//! 描く間は Project を借りるだけにし、操作は集めてから描き終えた後に session へ渡す。

use std::collections::{BTreeSet, HashSet};
use std::path::PathBuf;

use eframe::egui::{self, Align2, Color32, FontId, Pos2, Rect, Sense, Stroke, Vec2};
use mvm_app::{RemovalPlan, Session};
use mvm_project::media_bin::media_items_in_use;
use mvm_project::{MediaFolder, MediaItem, MediaKind, Project};

use crate::icons::{self, MediaIcon};
use crate::theme;

/// プロジェクトパネルからドラッグしている素材・フォルダの ID。timeline へ落とすと素材を置き、フォルダへ落とすと
/// 移す。
#[derive(Clone, Debug)]
pub struct MediaDrag(pub Vec<String>);

#[derive(Default)]
pub struct ProjectPanel {
    selected: Vec<String>,
    /// 名前を変えている項目と、入力中の名前。
    renaming: Option<(String, String)>,
    /// 削除の確認。確認したときの計画（消える項目・一緒に消える clip の数・Project の revision）。
    removing: Option<RemovalPlan>,
    filter: String,
    /// 開いているフォルダ。開いたときは閉じている（C++ 版と同じ）。
    expanded: HashSet<String>,
}

enum Action {
    Import(String),
    NewFolder(String),
    AddToTimeline(String),
    Select(String, bool),
    BeginRename(String, String),
    Rename(String, String),
    AskRemove(Vec<String>),
    Remove(RemovalPlan),
    Move(Vec<String>, String),
    Toggle(String),
}

const ROW_HEIGHT: f32 = 22.0;
const INDENT: f32 = 14.0;
const RATE_WIDTH: f32 = 88.0;
const DURATION_WIDTH: f32 = 100.0;
const SIZE_WIDTH: f32 = 96.0;
const LIST_FILL: Color32 = Color32::from_rgb(0x15, 0x18, 0x1d);
const LIST_ALT: Color32 = Color32::from_rgb(0x1a, 0x1d, 0x22);
const LIST_BORDER: Color32 = Color32::from_rgb(0x2c, 0x31, 0x39);
const LIST_HEADER: Color32 = Color32::from_rgb(0x23, 0x27, 0x2e);
const ROW_SELECTED: Color32 = Color32::from_rgb(0x34, 0x50, 0x6e);
const ROW_DROP: Color32 = Color32::from_rgb(0x2d, 0x4a, 0x6b);
const COLUMN_TEXT: Color32 = Color32::from_rgb(0xb7, 0xbd, 0xc6);

/// 一覧の 1 行。フォルダか素材。
enum Entry<'a> {
    Folder(&'a MediaFolder),
    Item(&'a MediaItem),
}

impl Entry<'_> {
    fn id(&self) -> &str {
        match self {
            Self::Folder(folder) => &folder.id,
            Self::Item(item) => &item.id,
        }
    }

    fn name(&self) -> &str {
        match self {
            Self::Folder(folder) => &folder.name,
            Self::Item(item) => &item.name,
        }
    }
}

struct Row<'a> {
    entry: Entry<'a>,
    depth: usize,
    has_children: bool,
}

pub fn show(ui: &mut egui::Ui, session: &mut Session, panel: &mut ProjectPanel) {
    let mut actions = Vec::new();
    let project = session.project();
    let entries = project.media_folders.len() + project.media_items.len();
    ui.horizontal(|ui| {
        ui.label(
            egui::RichText::new(format!("{entries} 項目"))
                .size(11.0)
                .color(theme::TEXT_SECONDARY),
        );
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let target = panel.target_folder(project);
            if ui
                .button(egui::RichText::new("新規フォルダ").size(11.0))
                .clicked()
            {
                actions.push(Action::NewFolder(target.clone()));
            }
            if ui
                .button(egui::RichText::new("読み込み…").size(11.0))
                .clicked()
            {
                actions.push(Action::Import(target));
            }
        });
    });
    search_field(ui, panel);
    let in_use = media_items_in_use(project);
    let rows = list_rows(project, panel);
    egui::Frame::new()
        .fill(LIST_FILL)
        .stroke(Stroke::new(1.0, LIST_BORDER))
        .show(ui, |ui| {
            ui.set_min_size(ui.available_size());
            column_header(ui);
            egui::ScrollArea::both()
                .id_salt("project_list")
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 0.0;
                    let width = ui
                        .available_width()
                        .max(columns_width(ui.available_width()));
                    for (index, row) in rows.iter().enumerate() {
                        show_row(ui, panel, &in_use, row, index, width, &mut actions);
                    }
                    if rows.is_empty() {
                        let text = if entries == 0 {
                            "ファイルをここへドロップするか「読み込み…」で素材を追加"
                        } else {
                            "一致する素材・フォルダはありません"
                        };
                        ui.add_space(12.0);
                        ui.vertical_centered(|ui| {
                            ui.label(egui::RichText::new(text).size(11.0).color(theme::TEXT_DIM));
                        });
                    }
                    // 残りの空白は root。素材をここへ落とすとフォルダから出す。
                    let height = ui.available_height().max(24.0);
                    let (_, response) = ui.allocate_exact_size(
                        egui::vec2(ui.available_width(), height),
                        Sense::click(),
                    );
                    if let Some(drag) = response.dnd_release_payload::<MediaDrag>() {
                        actions.push(Action::Move(drag.0.clone(), String::new()));
                    }
                    if response.clicked() {
                        panel.selected.clear();
                    }
                });
        });
    confirm_remove(ui, panel, &mut actions);
    for action in actions {
        apply(session, panel, action);
    }
}

impl ProjectPanel {
    /// 読み込みと新規フォルダの行き先。選んだフォルダ、選んだ素材のフォルダ、無ければ root。
    fn target_folder(&self, project: &Project) -> String {
        let [id] = self.selected.as_slice() else {
            return String::new();
        };
        if project.media_folders.iter().any(|folder| folder.id == *id) {
            return id.clone();
        }
        project
            .media_items
            .iter()
            .find(|item| item.id == *id)
            .map(|item| item.folder_id.clone())
            .unwrap_or_default()
    }

    /// ドラッグする項目。選択の中から始めたら選択の全部、そうでなければその 1 つ。
    fn drag_ids(&self, id: &str) -> Vec<String> {
        if self.selected.iter().any(|selected| selected == id) {
            self.selected.clone()
        } else {
            vec![id.to_owned()]
        }
    }

    fn remove_ids(&self, id: &str) -> Vec<String> {
        self.drag_ids(id)
    }
}

/// 検索欄。文字があれば右端に消すボタンを出す。
fn search_field(ui: &mut egui::Ui, panel: &mut ProjectPanel) {
    let response = ui.add(
        egui::TextEdit::singleline(&mut panel.filter)
            .hint_text("素材・フォルダを検索")
            .font(FontId::proportional(11.0))
            .margin(Vec2::new(8.0, 5.0))
            .desired_width(f32::INFINITY),
    );
    if panel.filter.is_empty() {
        return;
    }
    let rect = Rect::from_center_size(
        Pos2::new(response.rect.max.x - 14.0, response.rect.center().y),
        Vec2::splat(22.0),
    );
    let clear = ui.interact(rect, ui.id().with("clear_search"), Sense::click());
    clear.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, "検索をクリア"));
    icons::paint_cross(
        ui.painter(),
        rect.shrink(4.0),
        if clear.hovered() {
            theme::TEXT
        } else {
            theme::TEXT_SECONDARY
        },
    );
    if clear.clicked() {
        panel.filter.clear();
    }
}

/// 名前の列の幅。残りを他の列が使い、名前は最低 200 px。
fn name_width(width: f32) -> f32 {
    (width - RATE_WIDTH - DURATION_WIDTH - SIZE_WIDTH).max(200.0)
}

fn columns_width(width: f32) -> f32 {
    name_width(width) + RATE_WIDTH + DURATION_WIDTH + SIZE_WIDTH
}

fn column_header(ui: &mut egui::Ui) {
    let (rect, _) =
        ui.allocate_exact_size(Vec2::new(ui.available_width(), ROW_HEIGHT), Sense::hover());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 0.0, LIST_HEADER);
    painter.line_segment(
        [rect.left_bottom(), rect.right_bottom()],
        Stroke::new(1.0, LIST_BORDER),
    );
    let mut x = rect.min.x;
    for (title, width) in [
        ("名前", name_width(rect.width())),
        ("フレームレート", RATE_WIDTH),
        ("デュレーション", DURATION_WIDTH),
        ("解像度", SIZE_WIDTH),
    ] {
        painter.text(
            Pos2::new(x + 8.0, rect.center().y),
            Align2::LEFT_CENTER,
            title,
            FontId::proportional(11.0),
            theme::TEXT_SECONDARY,
        );
        x += width;
    }
}

/// 表示する行。絞り込みが無ければフォルダの木（開いたフォルダだけ中を出す）、あれば名前の一致する項目を平らに。
fn list_rows<'a>(project: &'a Project, panel: &ProjectPanel) -> Vec<Row<'a>> {
    let needle = panel.filter.trim().to_lowercase();
    let mut rows = Vec::new();
    if needle.is_empty() {
        let mut seen = HashSet::new();
        tree_rows(project, panel, "", 0, &mut seen, &mut rows);
        return rows;
    }
    let matches = |name: &str| name.to_lowercase().contains(&needle);
    for folder in &project.media_folders {
        if matches(&folder.name) {
            rows.push(Row {
                entry: Entry::Folder(folder),
                depth: 0,
                has_children: false,
            });
        }
    }
    for item in &project.media_items {
        if matches(&item.name) {
            rows.push(Row {
                entry: Entry::Item(item),
                depth: 0,
                has_children: false,
            });
        }
    }
    rows
}

fn tree_rows<'a>(
    project: &'a Project,
    panel: &ProjectPanel,
    parent_id: &str,
    depth: usize,
    seen: &mut HashSet<String>,
    rows: &mut Vec<Row<'a>>,
) {
    for folder in &project.media_folders {
        if folder.parent_id != parent_id || !seen.insert(folder.id.clone()) {
            continue;
        }
        let has_children = project
            .media_folders
            .iter()
            .any(|child| child.parent_id == folder.id)
            || project
                .media_items
                .iter()
                .any(|item| item.folder_id == folder.id);
        rows.push(Row {
            entry: Entry::Folder(folder),
            depth,
            has_children,
        });
        if panel.expanded.contains(&folder.id) {
            tree_rows(project, panel, &folder.id, depth + 1, seen, rows);
        }
    }
    for item in &project.media_items {
        if item.folder_id == parent_id {
            rows.push(Row {
                entry: Entry::Item(item),
                depth,
                has_children: false,
            });
        }
    }
}

fn show_row(
    ui: &mut egui::Ui,
    panel: &mut ProjectPanel,
    in_use: &BTreeSet<String>,
    row: &Row<'_>,
    index: usize,
    width: f32,
    actions: &mut Vec<Action>,
) {
    let id = row.entry.id().to_owned();
    let name = row.entry.name().to_owned();
    let (rect, response) =
        ui.allocate_exact_size(Vec2::new(width, ROW_HEIGHT), Sense::click_and_drag());
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, &name));
    let selected = panel.selected.contains(&id);
    let dropping = matches!(row.entry, Entry::Folder(_))
        && response.dnd_hover_payload::<MediaDrag>().is_some();
    let fill = if dropping {
        ROW_DROP
    } else if selected {
        ROW_SELECTED
    } else if index % 2 == 1 {
        LIST_ALT
    } else {
        LIST_FILL
    };
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 0.0, fill);
    let chevron_x = rect.min.x + 5.0 + row.depth as f32 * INDENT;
    if let Entry::Folder(_) = row.entry {
        let expanded = panel.expanded.contains(&id);
        let chevron = Rect::from_min_size(
            Pos2::new(chevron_x - 3.0, rect.min.y + 3.0),
            Vec2::splat(16.0),
        );
        let toggle = ui.interact(chevron, ui.id().with(("media_toggle", &id)), Sense::click());
        toggle.widget_info(|| {
            egui::WidgetInfo::labeled(egui::WidgetType::Button, true, format!("{name} を開閉"))
        });
        icons::paint_chevron(
            &painter,
            Rect::from_min_size(
                Pos2::new(chevron_x - 1.0, rect.center().y - 5.5),
                Vec2::splat(11.0),
            ),
            if expanded {
                icons::Chevron::Down
            } else {
                icons::Chevron::Right
            },
            if row.has_children {
                Color32::from_rgb(0xc3, 0xc8, 0xd0)
            } else {
                Color32::from_rgb(0x4a, 0x50, 0x5a)
            },
        );
        if toggle.clicked() {
            actions.push(Action::Toggle(id.clone()));
        }
    }
    let icon_x = chevron_x + 14.0;
    let kind = match &row.entry {
        Entry::Folder(_) => MediaIcon::Folder,
        Entry::Item(item) => match item.kind {
            MediaKind::Video => MediaIcon::Video,
            MediaKind::Audio => MediaIcon::Audio,
            MediaKind::Image => MediaIcon::Image,
        },
    };
    icons::paint_media_kind(&painter, Pos2::new(icon_x, rect.center().y - 7.0), kind);
    let name_x = icon_x + 21.0;
    let name_rect = Rect::from_min_max(
        Pos2::new(name_x, rect.min.y),
        Pos2::new(rect.min.x + name_width(rect.width()) - 4.0, rect.max.y),
    );
    if !rename_field(ui, panel, &id, name_rect, actions) {
        ui.painter_at(name_rect).text(
            name_rect.left_center(),
            Align2::LEFT_CENTER,
            &name,
            FontId::proportional(12.0),
            theme::TEXT,
        );
    }
    if let Entry::Item(item) = row.entry {
        let mut x = rect.min.x + name_width(rect.width());
        for (text, width, font) in [
            (media_rate(item), RATE_WIDTH, FontId::proportional(11.0)),
            (
                media_duration(item),
                DURATION_WIDTH,
                FontId::monospace(11.0),
            ),
            (media_size(item), SIZE_WIDTH, FontId::proportional(11.0)),
        ] {
            let cell = Rect::from_min_size(Pos2::new(x, rect.min.y), Vec2::new(width, ROW_HEIGHT));
            ui.painter_at(cell).text(
                cell.left_center() + Vec2::new(8.0, 0.0),
                Align2::LEFT_CENTER,
                text,
                font,
                COLUMN_TEXT,
            );
            x += width;
        }
    }
    let response = match &row.entry {
        Entry::Item(item) => {
            let tip = if in_use.contains(&item.id) {
                format!("{}\n(タイムラインで使用中)", item.media_path.display())
            } else {
                item.media_path.display().to_string()
            };
            response.on_hover_text(tip)
        }
        Entry::Folder(_) => response,
    };
    response.dnd_set_drag_payload(MediaDrag(panel.drag_ids(&id)));
    if let Entry::Folder(_) = row.entry
        && let Some(drag) = response.dnd_release_payload::<MediaDrag>()
    {
        actions.push(Action::Move(drag.0.clone(), id.clone()));
    }
    if response.double_clicked() {
        match row.entry {
            Entry::Item(_) => actions.push(Action::AddToTimeline(id.clone())),
            Entry::Folder(_) => actions.push(Action::Toggle(id.clone())),
        }
    } else if response.clicked() {
        let toggle = ui.input(|input| input.modifiers.command);
        actions.push(Action::Select(id.clone(), toggle));
    } else if response.secondary_clicked() && !selected {
        actions.push(Action::Select(id.clone(), false));
    }
    response.context_menu(|ui| {
        match row.entry {
            Entry::Item(_) => {
                if ui.button("タイムラインに追加").clicked() {
                    actions.push(Action::AddToTimeline(id.clone()));
                    ui.close();
                }
            }
            Entry::Folder(_) => {
                if ui.button("読み込み…").clicked() {
                    actions.push(Action::Import(id.clone()));
                    ui.close();
                }
                if ui.button("新規フォルダ").clicked() {
                    actions.push(Action::NewFolder(id.clone()));
                    ui.close();
                }
            }
        }
        if ui.button("名前を変更").clicked() {
            actions.push(Action::BeginRename(id.clone(), name.clone()));
            ui.close();
        }
        if ui.button("削除").clicked() {
            actions.push(Action::AskRemove(panel.remove_ids(&id)));
            ui.close();
        }
    });
}

/// 動画は fps（小数 3 桁、末尾の 0 を 1 つ落とす）、音声は sample rate。C++ 版と同じ書き方。
fn media_rate(item: &MediaItem) -> String {
    match item.kind {
        MediaKind::Video if item.fps.is_positive() => {
            let mut text = format!("{:.3}", item.fps.num as f64 / item.fps.den as f64);
            if text.ends_with('0') {
                text.pop();
            }
            format!("{text} fps")
        }
        MediaKind::Audio if item.sample_rate > 0 => format!("{} Hz", item.sample_rate),
        _ => String::new(),
    }
}

fn two_digits(value: i64) -> String {
    format!("{value:02}")
}

fn hms(seconds: i64) -> String {
    format!(
        "{}:{}:{}",
        two_digits(seconds / 3600),
        two_digits(seconds / 60 % 60),
        two_digits(seconds % 60)
    )
}

/// 動画は non-drop の timecode（frame 桁は公称 fps）、音声はミリ秒まで。画像は空。
fn media_duration(item: &MediaItem) -> String {
    match item.kind {
        MediaKind::Video if item.fps.is_positive() => {
            let nominal = item.fps.num / item.fps.den + i64::from(item.fps.num % item.fps.den != 0);
            format!(
                "{}:{}",
                hms(item.frame_count / nominal),
                two_digits(item.frame_count % nominal)
            )
        }
        MediaKind::Audio if item.sample_rate > 0 => {
            let rate = i64::from(item.sample_rate);
            let seconds = item.duration_samples / rate;
            let milliseconds = item.duration_samples % rate * 1000 / rate;
            format!("{}.{milliseconds:03}", hms(seconds))
        }
        _ => String::new(),
    }
}

fn media_size(item: &MediaItem) -> String {
    if item.kind == MediaKind::Audio || item.width <= 0 || item.height <= 0 {
        String::new()
    } else {
        format!("{} × {}", item.width, item.height)
    }
}

/// 名前を変えている項目なら入力欄を出して true を返す。Enter か focus を外すと確定し、Esc で取り消す。
fn rename_field(
    ui: &mut egui::Ui,
    panel: &mut ProjectPanel,
    id: &str,
    rect: Rect,
    actions: &mut Vec<Action>,
) -> bool {
    let Some((renaming, text)) = panel.renaming.as_mut() else {
        return false;
    };
    if renaming != id {
        return false;
    }
    let response = ui.put(
        rect.shrink2(Vec2::new(0.0, 1.0)),
        egui::TextEdit::singleline(text)
            .id_salt(("media_rename", id))
            .font(FontId::proportional(12.0))
            .margin(Vec2::new(2.0, 1.0)),
    );
    if !response.has_focus() && !response.lost_focus() {
        response.request_focus();
    }
    if response.lost_focus() {
        if ui.input(|input| input.key_pressed(egui::Key::Escape)) {
            panel.renaming = None;
        } else {
            actions.push(Action::Rename(id.to_owned(), text.clone()));
        }
    }
    true
}

fn confirm_remove(ui: &mut egui::Ui, panel: &mut ProjectPanel, actions: &mut Vec<Action>) {
    let Some(plan) = panel.removing.clone() else {
        return;
    };
    let clips = plan.clip_count;
    let width = (ui.ctx().content_rect().width() - 40.0).clamp(300.0, 460.0);
    let mut remove = false;
    let mut cancel = false;
    let ((), close) = crate::dialog::show(
        ui.ctx(),
        "media_remove",
        "素材を削除",
        width,
        |ui| {
            if clips > 0 {
                ui.label(format!(
                    "選択した素材を削除すると、タイムライン上の {clips} 個のクリップも削除されます。"
                ));
            } else {
                ui.label(format!(
                    "{} 件をプロジェクトから削除します。",
                    plan.entry_ids.len()
                ));
            }
        },
        |ui| {
            remove =
                crate::widgets::dialog_button(ui, "削除", crate::widgets::ButtonKind::Prominent)
                    .clicked();
            cancel =
                crate::widgets::dialog_button(ui, "キャンセル", crate::widgets::ButtonKind::Normal)
                    .clicked();
        },
    );
    if remove {
        panel.removing = None;
        actions.push(Action::Remove(plan.clone()));
    } else if cancel || close {
        panel.removing = None;
    }
}

fn apply(session: &mut Session, panel: &mut ProjectPanel, action: Action) {
    let result = match action {
        Action::Import(folder) => {
            let Some(paths) = pick_media() else {
                return;
            };
            import(session, &paths, &folder);
            Ok(())
        }
        Action::Toggle(id) => {
            if !panel.expanded.remove(&id) {
                panel.expanded.insert(id);
            }
            Ok(())
        }
        Action::NewFolder(parent) => session.create_media_folder(&parent).map(|id| {
            // 作ったフォルダが見えるよう、親を開く。
            panel.expanded.insert(parent.clone());
            panel.selected = vec![id.clone()];
            panel.renaming = session
                .project()
                .media_folders
                .iter()
                .find(|folder| folder.id == id)
                .map(|folder| (id, folder.name.clone()));
        }),
        Action::AddToTimeline(id) => session.add_media_item_at_playhead(&id),
        Action::Select(id, toggle) => {
            if toggle {
                if let Some(index) = panel.selected.iter().position(|selected| *selected == id) {
                    panel.selected.remove(index);
                } else {
                    panel.selected.push(id);
                }
            } else {
                panel.selected = vec![id];
            }
            Ok(())
        }
        Action::BeginRename(id, name) => {
            panel.renaming = Some((id, name));
            Ok(())
        }
        Action::Rename(id, name) => {
            panel.renaming = None;
            session.rename_media_entry(&id, &name)
        }
        Action::AskRemove(ids) => session.plan_media_removal(&ids).map(|plan| {
            panel.removing = Some(plan);
        }),
        Action::Remove(plan) => match session.remove_media_entries(&plan) {
            Ok(()) => {
                panel
                    .selected
                    .retain(|selected| !plan.entry_ids.contains(selected));
                Ok(())
            }
            // 確認の後で Project が変わった。今の内容で確認し直す。
            Err(error) => {
                if let Ok(again) = session.plan_media_removal(&plan.entry_ids) {
                    panel.removing = Some(again);
                }
                Err(error)
            }
        },
        Action::Move(ids, folder) => {
            // 自分の上へ落としただけなら何もしない。
            if ids.iter().all(|id| *id == folder) {
                return;
            }
            // 移した先を開き、移した項目が見えるようにする。
            panel.expanded.insert(folder.clone());
            session.move_media_entries(&ids, &folder)
        }
    };
    if let Err(error) = result {
        session.set_alert(error.message());
    }
}

/// ファイルを読み込み、読めなかったものがあれば知らせる。
pub fn import(session: &mut Session, paths: &[PathBuf], folder: &str) {
    match session.import_media_files(paths, folder) {
        Ok(report) if !report.failures.is_empty() => session.set_alert(report.message()),
        Ok(_) => {}
        Err(error) => session.set_alert(error.message()),
    }
}

pub fn pick_media() -> Option<Vec<PathBuf>> {
    rfd::FileDialog::new()
        .set_title("素材を読み込み")
        .add_filter(
            "動画・音声・画像",
            &[
                "mp4", "mov", "mkv", "webm", "avi", "m4v", "mts", "wav", "mp3", "m4a", "aac",
                "flac", "ogg", "opus", "png", "jpg", "jpeg", "bmp", "gif", "webp", "tif", "tiff",
                "tga", "qoi",
            ],
        )
        .add_filter("すべてのファイル", &["*"])
        .pick_files()
}
