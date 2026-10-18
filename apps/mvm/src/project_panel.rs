//! プロジェクトパネル。フォルダと素材の名前を出す。

use std::collections::HashSet;

use eframe::egui;
use mvm_project::{MediaKind, Project};

pub fn show(ui: &mut egui::Ui, project: &Project) {
    ui.heading("プロジェクト");
    if project.media_folders.is_empty() && project.media_items.is_empty() {
        ui.label("素材がありません");
        return;
    }
    let mut seen = HashSet::new();
    show_folder(ui, project, "", &mut seen);
}

fn show_folder(ui: &mut egui::Ui, project: &Project, parent_id: &str, seen: &mut HashSet<String>) {
    for folder in &project.media_folders {
        if folder.parent_id != parent_id || !seen.insert(folder.id.clone()) {
            continue;
        }
        egui::CollapsingHeader::new(folder.name.as_str())
            .id_salt(("media_folder", folder.id.as_str()))
            .default_open(true)
            .show(ui, |ui| show_folder(ui, project, &folder.id, seen));
    }
    for item in &project.media_items {
        if item.folder_id == parent_id {
            ui.label(item.name.as_str());
            ui.weak(kind_name(item.kind));
        }
    }
}

fn kind_name(kind: MediaKind) -> &'static str {
    match kind {
        MediaKind::Video => "映像",
        MediaKind::Audio => "音声",
        MediaKind::Image => "画像",
    }
}
