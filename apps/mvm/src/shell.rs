//! メニューと、プロジェクト・preview・timeline を並べた shell。

use std::path::PathBuf;

use eframe::egui::{self, Key, KeyboardShortcut, Modifiers, TextureHandle};
use mvm_app::{AppError, Session};

use crate::{preview_ui, project_panel, timeline_ui};

#[derive(Clone, Copy)]
enum FileAction {
    New,
    Open,
}

pub struct MvmApp {
    session: Session,
    confirm: Option<FileAction>,
    notice: String,
    preview_texture: Option<(u64, TextureHandle)>,
    shown_title: Option<String>,
}

impl MvmApp {
    pub fn new() -> Self {
        Self {
            session: Session::new(),
            confirm: None,
            notice: String::new(),
            preview_texture: None,
            shown_title: None,
        }
    }

    /// 試験が、未保存の確認を出す操作を始めるときに使う。
    #[cfg(test)]
    pub fn request_new(&mut self) {
        self.request(FileAction::New);
    }

    fn request(&mut self, action: FileAction) {
        if self.session.dirty() {
            self.confirm = Some(action);
        } else {
            self.perform(action);
        }
    }

    fn perform(&mut self, action: FileAction) {
        self.confirm = None;
        match action {
            FileAction::New => {
                if let Some(path) = pick_save("新規プロジェクトの保存先") {
                    let result = self.session.create_project(&ensure_mvm(path));
                    self.note(result);
                }
            }
            FileAction::Open => {
                if let Some(path) = pick_open() {
                    let result = self.session.open_project(&path);
                    self.note(result);
                }
            }
        }
    }

    fn save(&mut self) {
        if !self.session.dirty() {
            return;
        }
        if self.session.path().is_some() {
            let result = self.session.save_project();
            self.note(result);
        } else if let Some(path) = pick_save("プロジェクトの保存先") {
            let result = self.session.save_project_as(&ensure_mvm(path));
            self.note(result);
        }
    }

    fn save_as(&mut self) {
        if let Some(path) = pick_save("名前を付けて保存") {
            let result = self.session.save_project_as(&ensure_mvm(path));
            self.note(result);
        }
    }

    /// 保存できたとき `Ok(true)`。ダイアログを閉じたときは `Ok(false)`。
    fn save_current(&mut self) -> Result<bool, AppError> {
        if self.session.path().is_some() {
            self.session.save_project()?;
            Ok(true)
        } else if let Some(path) = pick_save("プロジェクトの保存先") {
            self.session.save_project_as(&ensure_mvm(path))?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    fn note(&mut self, result: Result<(), AppError>) {
        self.notice = match result {
            Ok(()) => String::new(),
            Err(error) => error.message().to_owned(),
        };
    }

    fn title(&self) -> String {
        let star = if self.session.dirty() { "*" } else { "" };
        let path = self
            .session
            .path()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| "無題".to_owned());
        format!("mvm{star} — {path}")
    }

    fn handle_shortcuts(&mut self, ui: &mut egui::Ui) {
        if self.confirm.is_some() {
            return;
        }
        let new_project = KeyboardShortcut::new(Modifiers::COMMAND, Key::N);
        let open = KeyboardShortcut::new(Modifiers::COMMAND, Key::O);
        let save = KeyboardShortcut::new(Modifiers::COMMAND, Key::S);
        let save_as = KeyboardShortcut::new(Modifiers::COMMAND | Modifiers::SHIFT, Key::S);
        let action = ui.input_mut(|input| {
            if input.consume_shortcut(&new_project) {
                Some(Command::New)
            } else if input.consume_shortcut(&open) {
                Some(Command::Open)
            } else if input.consume_shortcut(&save_as) {
                Some(Command::SaveAs)
            } else if input.consume_shortcut(&save) {
                Some(Command::Save)
            } else {
                None
            }
        });
        match action {
            Some(Command::New) => self.request(FileAction::New),
            Some(Command::Open) => self.request(FileAction::Open),
            Some(Command::Save) => self.save(),
            Some(Command::SaveAs) => self.save_as(),
            None => {}
        }
        let seek = ui.input_mut(|input| {
            if input.consume_key(Modifiers::NONE, Key::Home) {
                Seek::Start
            } else if input.consume_key(Modifiers::SHIFT, Key::ArrowRight) {
                Seek::By(10)
            } else if input.consume_key(Modifiers::SHIFT, Key::ArrowLeft) {
                Seek::By(-10)
            } else if input.consume_key(Modifiers::NONE, Key::ArrowRight) {
                Seek::By(1)
            } else if input.consume_key(Modifiers::NONE, Key::ArrowLeft) {
                Seek::By(-1)
            } else {
                Seek::None
            }
        });
        match seek {
            Seek::Start => {
                self.session.set_scrubbing(false);
                self.session.seek(0);
            }
            Seek::By(delta) => {
                self.session.set_scrubbing(false);
                self.session
                    .seek(self.session.playhead_frame().saturating_add(delta));
            }
            Seek::None => {}
        }
    }

    fn menu(&mut self, ui: &mut egui::Ui) {
        egui::MenuBar::new().ui(ui, |ui| {
            ui.menu_button("ファイル", |ui| {
                if ui.button("新規プロジェクト").clicked() {
                    self.request(FileAction::New);
                    ui.close();
                }
                if ui.button("開く").clicked() {
                    self.request(FileAction::Open);
                    ui.close();
                }
                if ui
                    .add_enabled(self.session.dirty(), egui::Button::new("保存"))
                    .clicked()
                {
                    self.save();
                    ui.close();
                }
                if ui.button("名前を付けて保存").clicked() {
                    self.save_as();
                    ui.close();
                }
                ui.separator();
                if ui.button("終了").clicked() {
                    ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                }
            });
        });
        if !self.notice.is_empty() {
            ui.colored_label(egui::Color32::from_rgb(230, 120, 120), &self.notice);
        }
    }

    fn confirm_unsaved(&mut self, ui: &mut egui::Ui) {
        let Some(action) = self.confirm else {
            return;
        };
        let mut choice = None;
        egui::Modal::new(egui::Id::new("unsaved")).show(ui.ctx(), |ui| {
            ui.label("保存していない変更があります");
            ui.horizontal(|ui| {
                if ui.button("変更を保存").clicked() {
                    choice = Some(UnsavedChoice::Save);
                }
                if ui.button("破棄して続ける").clicked() {
                    choice = Some(UnsavedChoice::Discard);
                }
                if ui.button("キャンセル").clicked() {
                    choice = Some(UnsavedChoice::Cancel);
                }
            });
        });
        match choice {
            Some(UnsavedChoice::Save) => match self.save_current() {
                Ok(true) => {
                    self.notice.clear();
                    self.perform(action);
                }
                Ok(false) => {}
                Err(error) => self.notice = error.message().to_owned(),
            },
            Some(UnsavedChoice::Discard) => self.perform(action),
            Some(UnsavedChoice::Cancel) => self.confirm = None,
            None => {}
        }
    }
}

enum Seek {
    None,
    Start,
    By(i64),
}

enum UnsavedChoice {
    Save,
    Discard,
    Cancel,
}

enum Command {
    New,
    Open,
    Save,
    SaveAs,
}

fn pick_open() -> Option<PathBuf> {
    rfd::FileDialog::new()
        .set_title("プロジェクトを開く")
        .add_filter("mvm project", &["mvm"])
        .pick_file()
}

fn pick_save(title: &str) -> Option<PathBuf> {
    rfd::FileDialog::new()
        .set_title(title)
        .add_filter("mvm project", &["mvm"])
        .save_file()
}

fn ensure_mvm(path: PathBuf) -> PathBuf {
    if path.extension().is_none() {
        let mut name = path.into_os_string();
        name.push(".mvm");
        PathBuf::from(name)
    } else {
        path
    }
}

impl eframe::App for MvmApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.show(ui);
    }
}

impl MvmApp {
    fn show(&mut self, ui: &mut egui::Ui) {
        let title = self.title();
        if self.shown_title.as_deref() != Some(title.as_str()) {
            ui.ctx()
                .send_viewport_cmd(egui::ViewportCommand::Title(title.clone()));
            self.shown_title = Some(title);
        }
        self.handle_shortcuts(ui);
        egui::Panel::top("menu").show(ui, |ui| self.menu(ui));
        egui::Panel::bottom("timeline")
            .resizable(true)
            .default_size(280.0)
            .show(ui, |ui| timeline_ui::show(ui, &mut self.session));
        egui::Panel::left("project")
            .resizable(true)
            .default_size(260.0)
            .show(ui, |ui| project_panel::show(ui, self.session.project()));
        egui::CentralPanel::default().show(ui, |ui| {
            preview_ui::show(ui, &mut self.session, &mut self.preview_texture);
        });
        self.confirm_unsaved(ui);
    }
}

#[cfg(test)]
mod tests {
    use eframe::egui;
    use egui_kittest::{Harness, kittest::Queryable};
    use mvm_core::Rational;
    use mvm_project::{
        MediaFolder, MediaItem, MediaKind, Project, TimelineClip, TimelineClipKind, TrackRef,
    };

    use super::*;

    fn harness(app: MvmApp) -> Harness<'static, MvmApp> {
        Harness::builder()
            .with_size(egui::vec2(1200.0, 800.0))
            .build_ui_state(|ui, app| app.show(ui), app)
    }

    fn text_clip(name: &str, start: i64) -> TimelineClip {
        TimelineClip {
            kind: TimelineClipKind::Text,
            name: name.to_owned(),
            id: format!("id-{name}"),
            source_fps: Rational::new(60, 1),
            source_frame_count: 30,
            source_in_frame: 0,
            source_out_frame: 30,
            timeline_start_frame: start,
            track: TrackRef::video(0),
            ..TimelineClip::default()
        }
    }

    #[test]
    fn shell_shows_the_default_project_and_the_file_menu() {
        let mut harness = harness(MvmApp::new());
        harness.get_by_label("ファイル");
        harness.get_by_label("V1");
        harness.get_by_label("V2");
        harness.get_by_label("A1");
        harness.get_by_label("停止");
        harness.get_by_label("100%");
        assert!(
            harness
                .query_all_by_label_contains("00:00:00:00")
                .next()
                .is_some()
        );
        harness.get_by_label("素材がありません");

        harness.get_by_label("ファイル").click();
        harness.run();
        for label in [
            "新規プロジェクト",
            "開く",
            "保存",
            "名前を付けて保存",
            "終了",
        ] {
            harness.get_by_label(label);
        }
    }

    #[test]
    fn timeline_omits_clips_outside_the_visible_window() {
        let project = Project {
            timeline_clips: vec![text_clip("Near", 0), text_clip("Far", 100_000)],
            ..Project::default()
        };
        let mut harness = harness(MvmApp::new());
        harness.state_mut().session = Session::from_project(project);
        harness.run();
        harness.get_by_label("Near");
        assert!(harness.query_by_label("Far").is_none());
    }

    #[test]
    fn scrolling_the_timeline_draws_the_clips_that_come_into_view() {
        // 400 frame は 100% で 3200px。最初の画面には入らない。
        let project = Project {
            timeline_clips: vec![text_clip("Near", 0), text_clip("Far", 400)],
            ..Project::default()
        };
        let mut harness = harness(MvmApp::new());
        harness.state_mut().session = Session::from_project(project);
        harness.run();
        assert!(harness.query_by_label("Far").is_none());
        let near = harness.get_by_label("Near").rect().center();
        harness.hover_at(near);
        harness.event(egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Point,
            delta: egui::vec2(-3000.0, 0.0),
            phase: egui::TouchPhase::Move,
            modifiers: egui::Modifiers::NONE,
        });
        // wheel のスクロールは数 frame かけて滑らかに進むので、止まるまで回す。
        harness.run_steps(120);
        harness.get_by_label("Far");
        assert!(harness.query_by_label("Near").is_none());
    }

    #[test]
    fn project_panel_nests_items_under_their_folder() {
        let project = Project {
            media_folders: vec![MediaFolder {
                id: "folder".to_owned(),
                name: "素材フォルダ".to_owned(),
                parent_id: String::new(),
            }],
            media_items: vec![MediaItem {
                id: "item".to_owned(),
                kind: MediaKind::Image,
                name: "背景".to_owned(),
                folder_id: "folder".to_owned(),
                ..MediaItem::default()
            }],
            ..Project::default()
        };
        let mut harness = harness(MvmApp::new());
        harness.state_mut().session = Session::from_project(project);
        harness.run();
        let folder = harness.get_by_label("素材フォルダ").rect();
        let item = harness.get_by_label("背景").rect();
        assert!(
            item.min.x > folder.min.x,
            "素材がフォルダの中に字下げされていません"
        );
        assert!(item.min.y > folder.min.y);

        harness.get_by_label("素材フォルダ").click();
        harness.run();
        assert!(
            harness.query_by_label("背景").is_none(),
            "閉じたフォルダの素材が見えています"
        );
    }

    #[test]
    fn project_panel_lists_only_items_that_exist() {
        let project = Project {
            media_items: vec![MediaItem {
                id: "item".to_owned(),
                kind: MediaKind::Video,
                name: "導入".to_owned(),
                ..MediaItem::default()
            }],
            ..Project::default()
        };
        let mut harness = harness(MvmApp::new());
        harness.state_mut().session = Session::from_project(project);
        harness.run();
        harness.get_by_label("導入");
        harness.get_by_label("映像");
        assert!(harness.query_by_label("無い素材").is_none());
    }

    #[test]
    fn zoom_button_doubles_the_scale() {
        let mut harness = harness(MvmApp::new());
        harness.get_by_label("拡大").click();
        harness.run();
        harness.get_by_label("200%");
        assert!(harness.query_by_label("100%").is_none());
    }

    #[test]
    fn dirty_new_asks_before_replacing_the_project() {
        let mut harness = harness(MvmApp::new());
        harness.state_mut().session.mark_changed();
        harness.state_mut().request_new();
        harness.run();
        harness.get_by_label("保存していない変更があります");
        harness.get_by_label("破棄して続ける");
        harness.get_by_label("キャンセル");
    }
}
