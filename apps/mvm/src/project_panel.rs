//! プロジェクトパネル。素材の読み込み・フォルダ・名前の変更・削除・移動と、timeline へのドラッグを受け持つ。
//!
//! 描く間は Project を借りるだけにし、操作は集めてから描き終えた後に session へ渡す。

use std::collections::{BTreeSet, HashSet};
use std::path::PathBuf;

use eframe::egui::{self, Sense};
use mvm_app::{RemovalPlan, Session};
use mvm_project::media_bin::media_items_in_use;
use mvm_project::{MediaKind, Project};

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
}

pub fn show(ui: &mut egui::Ui, session: &mut Session, panel: &mut ProjectPanel) {
    let mut actions = Vec::new();
    ui.heading("プロジェクト");
    ui.horizontal(|ui| {
        let target = panel.target_folder(session.project());
        if ui.button("読み込み…").clicked() {
            actions.push(Action::Import(target.clone()));
        }
        if ui.button("新規フォルダ").clicked() {
            actions.push(Action::NewFolder(target));
        }
    });
    ui.add(egui::TextEdit::singleline(&mut panel.filter).hint_text("素材・フォルダを検索"));
    let project = session.project();
    let in_use = media_items_in_use(project);
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            if project.media_folders.is_empty() && project.media_items.is_empty() {
                ui.label("素材がありません");
                ui.weak("「読み込み…」か、ファイルのドロップで追加します");
            } else if panel.filter.trim().is_empty() {
                let mut seen = HashSet::new();
                show_folder(ui, project, panel, &in_use, "", &mut seen, &mut actions);
            } else {
                show_matches(ui, project, panel, &in_use, &mut actions);
            }
            // 残りの空白は root。素材をここへ落とすとフォルダから出す。
            let height = ui.available_height().max(24.0);
            let (_, response) =
                ui.allocate_exact_size(egui::vec2(ui.available_width(), height), Sense::click());
            if let Some(drag) = response.dnd_release_payload::<MediaDrag>() {
                actions.push(Action::Move(drag.0.clone(), String::new()));
            }
            if response.clicked() {
                panel.selected.clear();
            }
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

fn show_folder(
    ui: &mut egui::Ui,
    project: &Project,
    panel: &mut ProjectPanel,
    in_use: &BTreeSet<String>,
    parent_id: &str,
    seen: &mut HashSet<String>,
    actions: &mut Vec<Action>,
) {
    for folder in &project.media_folders {
        if folder.parent_id != parent_id || !seen.insert(folder.id.clone()) {
            continue;
        }
        let selected = panel.selected.contains(&folder.id);
        if rename_field(ui, panel, &folder.id, actions) {
            continue;
        }
        let header = egui::CollapsingHeader::new(folder.name.as_str())
            .id_salt(("media_folder", folder.id.as_str()))
            .default_open(true)
            .show(ui, |ui| {
                show_folder(ui, project, panel, in_use, &folder.id, seen, actions);
            });
        // 見出しもドラッグで動かせる（別のフォルダや root の空白へ落として移す）。
        let response = header.header_response.interact(Sense::drag());
        response.dnd_set_drag_payload(MediaDrag(panel.drag_ids(&folder.id)));
        if selected {
            ui.painter().rect_stroke(
                response.rect,
                2.0,
                ui.visuals().selection.stroke,
                egui::StrokeKind::Inside,
            );
        }
        if response.secondary_clicked() && !selected {
            actions.push(Action::Select(folder.id.clone(), false));
        }
        if let Some(drag) = response.dnd_release_payload::<MediaDrag>() {
            actions.push(Action::Move(drag.0.clone(), folder.id.clone()));
        }
        response.context_menu(|ui| {
            if ui.button("読み込み…").clicked() {
                actions.push(Action::Import(folder.id.clone()));
                ui.close();
            }
            if ui.button("新規フォルダ").clicked() {
                actions.push(Action::NewFolder(folder.id.clone()));
                ui.close();
            }
            if ui.button("名前を変更").clicked() {
                actions.push(Action::BeginRename(folder.id.clone(), folder.name.clone()));
                ui.close();
            }
            if ui.button("削除").clicked() {
                actions.push(Action::AskRemove(panel.remove_ids(&folder.id)));
                ui.close();
            }
        });
    }
    for item in &project.media_items {
        if item.folder_id == parent_id {
            item_row(ui, panel, in_use, item, actions);
        }
    }
}

fn show_matches(
    ui: &mut egui::Ui,
    project: &Project,
    panel: &mut ProjectPanel,
    in_use: &BTreeSet<String>,
    actions: &mut Vec<Action>,
) {
    let needle = panel.filter.trim().to_lowercase();
    let mut found = false;
    for item in &project.media_items {
        if item.name.to_lowercase().contains(&needle) {
            found = true;
            item_row(ui, panel, in_use, item, actions);
        }
    }
    if !found {
        ui.label(format!(
            "「{}」に一致する素材はありません",
            panel.filter.trim()
        ));
    }
}

fn item_row(
    ui: &mut egui::Ui,
    panel: &mut ProjectPanel,
    in_use: &BTreeSet<String>,
    item: &mvm_project::MediaItem,
    actions: &mut Vec<Action>,
) {
    if rename_field(ui, panel, &item.id, actions) {
        return;
    }
    let selected = panel.selected.contains(&item.id);
    let response = ui
        .horizontal(|ui| {
            let response = ui
                .add(
                    egui::Button::selectable(selected, item.name.as_str())
                        .sense(Sense::click_and_drag()),
                )
                .on_hover_text(item.media_path.display().to_string());
            ui.weak(kind_name(item.kind));
            if in_use.contains(&item.id) {
                ui.weak("使用中");
            }
            response
        })
        .inner;
    response.dnd_set_drag_payload(MediaDrag(panel.drag_ids(&item.id)));
    if response.double_clicked() {
        actions.push(Action::AddToTimeline(item.id.clone()));
    } else if response.clicked() {
        let toggle = ui.input(|input| input.modifiers.command);
        actions.push(Action::Select(item.id.clone(), toggle));
    } else if response.secondary_clicked() && !selected {
        actions.push(Action::Select(item.id.clone(), false));
    }
    response.context_menu(|ui| {
        if ui.button("タイムラインに追加").clicked() {
            actions.push(Action::AddToTimeline(item.id.clone()));
            ui.close();
        }
        if ui.button("名前を変更").clicked() {
            actions.push(Action::BeginRename(item.id.clone(), item.name.clone()));
            ui.close();
        }
        if ui.button("削除").clicked() {
            actions.push(Action::AskRemove(panel.remove_ids(&item.id)));
            ui.close();
        }
    });
}

/// 名前を変えている項目なら入力欄を出して true を返す。Enter か focus を外すと確定し、Esc で取り消す。
fn rename_field(
    ui: &mut egui::Ui,
    panel: &mut ProjectPanel,
    id: &str,
    actions: &mut Vec<Action>,
) -> bool {
    let Some((renaming, text)) = panel.renaming.as_mut() else {
        return false;
    };
    if renaming != id {
        return false;
    }
    let response = ui.add(egui::TextEdit::singleline(text).id_salt(("media_rename", id)));
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
    egui::Window::new("素材を削除")
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
        .show(ui.ctx(), |ui| {
            if clips > 0 {
                ui.label(format!(
                    "選択した素材を削除すると、タイムライン上の {clips} 個の clip も削除されます。"
                ));
            } else {
                ui.label(format!(
                    "{} 件をプロジェクトから削除します。",
                    plan.entry_ids.len()
                ));
            }
            ui.horizontal(|ui| {
                if ui.button("キャンセル").clicked() {
                    panel.removing = None;
                }
                if ui.button("削除").clicked() {
                    panel.removing = None;
                    actions.push(Action::Remove(plan.clone()));
                }
            });
        });
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
        Action::NewFolder(parent) => session.create_media_folder(&parent).map(|id| {
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

fn kind_name(kind: MediaKind) -> &'static str {
    match kind {
        MediaKind::Video => "映像",
        MediaKind::Audio => "音声",
        MediaKind::Image => "画像",
    }
}
