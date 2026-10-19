//! メニューと、プロジェクト・preview・timeline を並べた shell。

use std::path::PathBuf;
use std::time::Duration;

use eframe::egui::{self, Key, KeyboardShortcut, Modifiers, TextureHandle};
use mvm_app::{AppError, RecoveryPrompt, Session};
use mvm_project::timeline::TimelineTool;

use crate::{export_ui, inspector, preview_ui, project_panel, timeline_ui};

/// 自動保存と外部変更の検査は入力が無くても進める。egui は入力が無いと描き直さないので、この間隔で起こす。
const MAINTENANCE_INTERVAL: Duration = Duration::from_millis(250);

#[derive(Clone, Copy)]
enum FileAction {
    New,
    Open,
    Close,
}

pub struct MvmApp {
    session: Session,
    confirm: Option<FileAction>,
    notice: String,
    preview_texture: Option<(u64, TextureHandle)>,
    shown_title: Option<String>,
    gesture: Option<timeline_ui::TimelineGesture>,
    inspector: inspector::InspectorState,
    export: export_ui::ExportDialog,
    text_draft: String,
    recovery_closed: bool,
    external_closed: bool,
    /// 確認を済ませた終了。次の close 要求は止めない。
    close_approved: bool,
    close_now: bool,
}

impl MvmApp {
    pub fn new() -> Self {
        Self {
            session: Session::new(),
            confirm: None,
            notice: String::new(),
            preview_texture: None,
            shown_title: None,
            gesture: None,
            inspector: inspector::InspectorState::default(),
            export: export_ui::ExportDialog::default(),
            text_draft: String::new(),
            recovery_closed: false,
            external_closed: false,
            close_approved: false,
            close_now: false,
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
            FileAction::Close => {
                self.close_approved = true;
                self.close_now = true;
            }
        }
    }

    /// 窓の閉じるボタンと「終了」。未保存なら確認し、積んだ自動復旧の書き込みと削除を終えてから閉じる。
    fn handle_close_request(&mut self, ctx: &egui::Context) {
        if self.close_approved {
            return;
        }
        if self.session.is_exporting() {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.notice =
                "書き出し中は終了できません。取り消すか、終わってから閉じてください".to_owned();
            return;
        }
        if self.session.dirty() {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.confirm = Some(FileAction::Close);
            return;
        }
        if let Err(error) = self.session.finish_background() {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.notice = format!("{}。もう一度閉じると、そのまま終了します", error.message());
            self.close_approved = true;
        }
    }

    fn show_external_again_if_refused(&mut self) {
        if self.session.external_change() {
            self.external_closed = false;
        }
    }

    fn save(&mut self) {
        if !self.session.dirty() {
            return;
        }
        if self.session.path().is_some() {
            let result = self.session.save_project();
            self.note(result);
            self.show_external_again_if_refused();
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

    fn perform_edit(&mut self, edit: impl FnOnce(&mut Session) -> Result<(), AppError>) {
        let result = edit(&mut self.session);
        self.note(result);
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
        if self.confirm.is_some()
            || self.recovery_visible()
            || self.external_visible()
            || self.export.is_open()
            || self.session.is_exporting()
        {
            return;
        }
        let new_project = KeyboardShortcut::new(Modifiers::COMMAND, Key::N);
        let open = KeyboardShortcut::new(Modifiers::COMMAND, Key::O);
        let save = KeyboardShortcut::new(Modifiers::COMMAND, Key::S);
        let save_as = KeyboardShortcut::new(Modifiers::COMMAND | Modifiers::SHIFT, Key::S);
        let export = KeyboardShortcut::new(Modifiers::COMMAND, Key::M);
        let action = ui.input_mut(|input| {
            if input.consume_shortcut(&new_project) {
                Some(Command::New)
            } else if input.consume_shortcut(&open) {
                Some(Command::Open)
            } else if input.consume_shortcut(&save_as) {
                Some(Command::SaveAs)
            } else if input.consume_shortcut(&save) {
                Some(Command::Save)
            } else if input.consume_shortcut(&export) {
                Some(Command::Export)
            } else {
                None
            }
        });
        match action {
            Some(Command::New) => self.request(FileAction::New),
            Some(Command::Open) => self.request(FileAction::Open),
            Some(Command::Save) => self.save(),
            Some(Command::SaveAs) => self.save_as(),
            Some(Command::Export) => self.export.open(&self.session),
            None => {}
        }
        if ui.ctx().egui_wants_keyboard_input() {
            return;
        }
        self.handle_edit_shortcuts(ui);
        let seek = ui.input_mut(|input| {
            if input.consume_key(Modifiers::NONE, Key::Home) {
                Seek::Start
            } else if input.consume_key(Modifiers::SHIFT, Key::ArrowRight) {
                Seek::By(5)
            } else if input.consume_key(Modifiers::SHIFT, Key::ArrowLeft) {
                Seek::By(-5)
            } else if input.consume_key(Modifiers::NONE, Key::ArrowRight) {
                Seek::By(1)
            } else if input.consume_key(Modifiers::NONE, Key::ArrowLeft) {
                Seek::By(-1)
            } else if input.consume_key(Modifiers::NONE, Key::ArrowUp) {
                Seek::Edit(-1)
            } else if input.consume_key(Modifiers::NONE, Key::ArrowDown) {
                Seek::Edit(1)
            } else if input.consume_key(Modifiers::NONE, Key::Space) {
                Seek::Play
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
            Seek::Play => self.session.toggle_playback(),
            Seek::Edit(direction) => self.session.jump_edit_point(direction > 0),
            Seek::None => {}
        }
    }

    fn handle_edit_shortcuts(&mut self, ui: &mut egui::Ui) {
        let command = ui.input_mut(|input| {
            if input.consume_shortcut(&KeyboardShortcut::new(
                Modifiers::COMMAND | Modifiers::SHIFT,
                Key::Z,
            )) || input.consume_shortcut(&KeyboardShortcut::new(Modifiers::COMMAND, Key::Y))
            {
                EditCommand::Redo
            } else if input.consume_shortcut(&KeyboardShortcut::new(Modifiers::COMMAND, Key::Z)) {
                EditCommand::Undo
            } else if input.consume_shortcut(&KeyboardShortcut::new(Modifiers::COMMAND, Key::C)) {
                EditCommand::Copy
            } else if input.consume_shortcut(&KeyboardShortcut::new(Modifiers::COMMAND, Key::X)) {
                EditCommand::Cut
            } else if input.consume_shortcut(&KeyboardShortcut::new(Modifiers::COMMAND, Key::V)) {
                EditCommand::Paste
            } else if input.consume_shortcut(&KeyboardShortcut::new(Modifiers::COMMAND, Key::D)) {
                EditCommand::Duplicate
            } else if input.consume_shortcut(&KeyboardShortcut::new(Modifiers::COMMAND, Key::A)) {
                EditCommand::SelectAll
            } else if input.consume_shortcut(&KeyboardShortcut::new(
                Modifiers::COMMAND | Modifiers::SHIFT,
                Key::K,
            )) {
                EditCommand::SplitAll
            } else if input.consume_shortcut(&KeyboardShortcut::new(Modifiers::COMMAND, Key::K)) {
                EditCommand::Split
            } else if input.consume_key(Modifiers::COMMAND | Modifiers::SHIFT, Key::M) {
                EditCommand::PreviousMarker
            } else {
                EditCommand::None
            }
        });
        self.run_edit(command);
        let command = ui.input_mut(|input| {
            if input.consume_key(Modifiers::SHIFT, Key::M) {
                EditCommand::NextMarker
            } else if input.consume_key(Modifiers::NONE, Key::M) {
                EditCommand::Marker
            } else if input.consume_key(Modifiers::SHIFT, Key::E) {
                EditCommand::ToggleEnabled
            } else if input.consume_key(Modifiers::SHIFT, Key::D) {
                EditCommand::Transition
            } else if input.consume_key(Modifiers::SHIFT, Key::I) {
                EditCommand::JumpIn
            } else if input.consume_key(Modifiers::SHIFT, Key::O) {
                EditCommand::JumpOut
            } else if input.consume_key(Modifiers::ALT, Key::X) {
                EditCommand::ClearMarks
            } else if input.consume_key(Modifiers::NONE, Key::I) {
                EditCommand::MarkIn
            } else if input.consume_key(Modifiers::NONE, Key::O) {
                EditCommand::MarkOut
            } else if input.consume_key(Modifiers::NONE, Key::OpenBracket) {
                EditCommand::VolumeUp
            } else if input.consume_key(Modifiers::NONE, Key::CloseBracket) {
                EditCommand::VolumeDown
            } else if input.consume_key(Modifiers::NONE, Key::Delete)
                || input.consume_key(Modifiers::NONE, Key::Backspace)
            {
                EditCommand::Delete
            } else if input.modifiers.command || input.modifiers.alt {
                EditCommand::None
            } else if input.consume_key(Modifiers::SHIFT, Key::A) {
                EditCommand::Tool(TimelineTool::TrackBackward)
            } else if input.consume_key(Modifiers::NONE, Key::A) {
                EditCommand::Tool(TimelineTool::TrackForward)
            } else if input.consume_key(Modifiers::NONE, Key::V) {
                EditCommand::Tool(TimelineTool::Select)
            } else if input.consume_key(Modifiers::NONE, Key::C) {
                EditCommand::Tool(TimelineTool::Razor)
            } else if input.consume_key(Modifiers::NONE, Key::B) {
                EditCommand::Tool(TimelineTool::Ripple)
            } else if input.consume_key(Modifiers::NONE, Key::N) {
                EditCommand::Tool(TimelineTool::Rolling)
            } else if input.consume_key(Modifiers::NONE, Key::R) {
                EditCommand::Tool(TimelineTool::Rate)
            } else if input.consume_key(Modifiers::NONE, Key::Y) {
                EditCommand::Tool(TimelineTool::Slip)
            } else if input.consume_key(Modifiers::NONE, Key::U) {
                EditCommand::Tool(TimelineTool::Slide)
            } else if input.consume_key(Modifiers::NONE, Key::P) {
                EditCommand::Tool(TimelineTool::Pen)
            } else if input.consume_key(Modifiers::NONE, Key::H) {
                EditCommand::Tool(TimelineTool::Hand)
            } else if input.consume_key(Modifiers::NONE, Key::Z) {
                EditCommand::Tool(TimelineTool::Zoom)
            } else if input.consume_key(Modifiers::NONE, Key::T) {
                EditCommand::Tool(TimelineTool::Text)
            } else {
                EditCommand::None
            }
        });
        self.run_edit(command);
    }

    fn run_edit(&mut self, command: EditCommand) {
        let result = match command {
            EditCommand::None => return,
            EditCommand::Undo => self.session.undo(),
            EditCommand::Redo => self.session.redo(),
            EditCommand::Copy => self.session.copy_selection(),
            EditCommand::Cut => self.session.cut_selection(),
            EditCommand::Paste => self.session.paste(),
            EditCommand::Duplicate => self.session.duplicate_selection(),
            EditCommand::SelectAll => {
                self.session.select_all();
                Ok(())
            }
            EditCommand::Split => self.session.split_selection_at_playhead(),
            EditCommand::SplitAll => self.session.split_all_at_playhead(),
            EditCommand::Delete => self.session.delete_selection(),
            EditCommand::VolumeUp => self.session.step_volume(1.0),
            EditCommand::VolumeDown => self.session.step_volume(-1.0),
            EditCommand::ToggleEnabled => self.session.toggle_enabled(),
            EditCommand::Transition => self.session.apply_default_transition(),
            EditCommand::Marker => self.session.add_marker(),
            EditCommand::NextMarker => {
                self.session.jump_marker(true);
                Ok(())
            }
            EditCommand::PreviousMarker => {
                self.session.jump_marker(false);
                Ok(())
            }
            EditCommand::MarkIn => self.session.mark_in(),
            EditCommand::MarkOut => self.session.mark_out(),
            EditCommand::JumpIn => {
                self.session.jump_to_in();
                Ok(())
            }
            EditCommand::JumpOut => {
                self.session.jump_to_out();
                Ok(())
            }
            EditCommand::ClearMarks => self.session.clear_marks(),
            EditCommand::Tool(tool) => {
                self.session.set_tool(tool);
                Ok(())
            }
        };
        self.note(result);
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
                if ui
                    .add_enabled(!self.session.is_exporting(), egui::Button::new("書き出し"))
                    .clicked()
                {
                    self.export.open(&self.session);
                    ui.close();
                }
                ui.separator();
                if ui.button("終了").clicked() {
                    ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                }
            });
            ui.menu_button("編集", |ui| {
                if ui
                    .add_enabled(self.session.can_undo(), egui::Button::new("元に戻す"))
                    .clicked()
                {
                    self.perform_edit(Session::undo);
                    ui.close();
                }
                if ui
                    .add_enabled(self.session.can_redo(), egui::Button::new("やり直し"))
                    .clicked()
                {
                    self.perform_edit(Session::redo);
                    ui.close();
                }
                if ui.button("コピー").clicked() {
                    self.perform_edit(Session::copy_selection);
                    ui.close();
                }
                if ui.button("ペースト").clicked() {
                    self.perform_edit(Session::paste);
                    ui.close();
                }
                if ui.button("削除").clicked() {
                    self.perform_edit(Session::delete_selection);
                    ui.close();
                }
            });
        });
        if !self.notice.is_empty() {
            ui.colored_label(egui::Color32::from_rgb(230, 120, 120), &self.notice);
        } else if !self.export.finished.is_empty() {
            ui.label(&self.export.finished);
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
                let discard = match action {
                    FileAction::Close => "破棄して終了",
                    FileAction::New | FileAction::Open => "破棄して続ける",
                };
                if ui.button(discard).clicked() {
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
                Err(error) => {
                    self.notice = error.message().to_owned();
                    self.show_external_again_if_refused();
                }
            },
            Some(UnsavedChoice::Discard) => {
                // 新規・開くは、Project を置き換えるときに session が recovery を片付ける。
                if matches!(action, FileAction::Close)
                    && let Err(error) = self.session.abandon_changes()
                {
                    self.confirm = None;
                    self.notice = error.message().to_owned();
                    return;
                }
                self.perform(action);
            }
            Some(UnsavedChoice::Cancel) => self.confirm = None,
            None => {}
        }
    }

    fn recovery_visible(&self) -> bool {
        self.session.recovery_prompt().is_some() && !self.recovery_closed
    }

    fn external_visible(&self) -> bool {
        self.session.external_change() && !self.external_closed
    }

    fn confirm_recovery(&mut self, ui: &mut egui::Ui) {
        let Some(prompt) = self.session.recovery_prompt() else {
            return;
        };
        if self.recovery_closed {
            return;
        }
        let mut choice = None;
        egui::Modal::new(egui::Id::new("recovery")).show(ui.ctx(), |ui| {
            ui.label(match prompt {
                RecoveryPrompt::Restorable => "自動復旧データがあります",
                RecoveryPrompt::CanonicalChanged => {
                    "Project file が変わったあとの自動復旧データがあります"
                }
                RecoveryPrompt::Corrupt => "自動復旧データを読めません",
                RecoveryPrompt::Foreign => "別の Project の自動復旧データがあります",
            });
            ui.horizontal(|ui| {
                match prompt {
                    RecoveryPrompt::Restorable | RecoveryPrompt::CanonicalChanged => {
                        if ui.button("復元する").clicked() {
                            choice = Some(RecoveryChoice::Restore);
                        }
                        if ui.button("破棄する").clicked() {
                            choice = Some(RecoveryChoice::Discard);
                        }
                    }
                    RecoveryPrompt::Corrupt | RecoveryPrompt::Foreign => {}
                }
                if ui.button("閉じる").clicked() {
                    choice = Some(RecoveryChoice::Close);
                }
            });
        });
        match choice {
            Some(RecoveryChoice::Restore) => self.perform_edit(Session::restore_recovery),
            Some(RecoveryChoice::Discard) => {
                self.perform_edit(|session| session.dismiss_recovery(true));
            }
            Some(RecoveryChoice::Close) => {
                let delete = false;
                if matches!(prompt, RecoveryPrompt::Corrupt | RecoveryPrompt::Foreign) {
                    self.perform_edit(move |session| session.dismiss_recovery(delete));
                } else {
                    self.recovery_closed = true;
                }
            }
            None => {}
        }
    }

    fn confirm_external(&mut self, ui: &mut egui::Ui) {
        if !self.external_visible() {
            return;
        }
        let mut choice = None;
        egui::Modal::new(egui::Id::new("external")).show(ui.ctx(), |ui| {
            ui.label("Project file が外部で変更されています");
            ui.horizontal(|ui| {
                if ui.button("読み直す").clicked() {
                    choice = Some(ExternalChoice::Reload);
                }
                if ui.button("上書き保存").clicked() {
                    choice = Some(ExternalChoice::Overwrite);
                }
                if ui.button("キャンセル").clicked() {
                    choice = Some(ExternalChoice::Cancel);
                }
            });
        });
        match choice {
            Some(ExternalChoice::Reload) => self.perform_edit(Session::reload_project),
            Some(ExternalChoice::Overwrite) => {
                self.perform_edit(Session::save_overwriting_external);
            }
            Some(ExternalChoice::Cancel) => self.external_closed = true,
            None => {}
        }
    }
}

enum Seek {
    None,
    Start,
    By(i64),
    Edit(i32),
    Play,
}

enum EditCommand {
    None,
    Undo,
    Redo,
    Copy,
    Cut,
    Paste,
    Duplicate,
    SelectAll,
    Split,
    SplitAll,
    Delete,
    VolumeUp,
    VolumeDown,
    ToggleEnabled,
    Transition,
    Marker,
    NextMarker,
    PreviousMarker,
    MarkIn,
    MarkOut,
    JumpIn,
    JumpOut,
    ClearMarks,
    Tool(TimelineTool),
}

enum UnsavedChoice {
    Save,
    Discard,
    Cancel,
}

enum RecoveryChoice {
    Restore,
    Discard,
    Close,
}

enum ExternalChoice {
    Reload,
    Overwrite,
    Cancel,
}

enum Command {
    New,
    Open,
    Save,
    SaveAs,
    Export,
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
        // 試験の harness は show を直接回すので、時刻で起こすのは実窓だけにする。
        ui.ctx().request_repaint_after(MAINTENANCE_INTERVAL);
    }
}

impl MvmApp {
    fn show(&mut self, ui: &mut egui::Ui) {
        let seconds = ui.input(|input| input.time);
        let now_ms = if seconds.is_finite() && seconds > 0.0 {
            (seconds * 1000.0) as u64
        } else {
            0
        };
        if ui.input(|input| input.viewport().close_requested()) {
            self.handle_close_request(ui.ctx());
        }
        self.session.poll_maintenance(now_ms);
        if self.session.recovery_prompt().is_none() {
            self.recovery_closed = false;
        }
        if !self.session.external_change() {
            self.external_closed = false;
        }
        if let Some(alert) = self.session.alert() {
            if self.notice.is_empty() {
                self.notice = alert.to_owned();
            }
            self.session.clear_alert();
        }
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
            .show(ui, |ui| {
                timeline_ui::show(ui, &mut self.session, &mut self.gesture);
            });
        egui::Panel::left("project")
            .resizable(true)
            .default_size(260.0)
            .show(ui, |ui| project_panel::show(ui, self.session.project()));
        egui::Panel::right("inspector")
            .resizable(true)
            .default_size(280.0)
            .show(ui, |ui| {
                inspector::show(ui, &mut self.session, &mut self.inspector);
            });
        egui::CentralPanel::default().show(ui, |ui| {
            preview_ui::show(
                ui,
                &mut self.session,
                &mut self.preview_texture,
                &mut self.text_draft,
                &mut self.notice,
            );
        });
        self.confirm_unsaved(ui);
        self.confirm_recovery(ui);
        self.confirm_external(ui);
        export_ui::show(ui, &mut self.session, &mut self.export, &mut self.notice);
        if std::mem::take(&mut self.close_now) {
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
        }
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
            "書き出し",
            "終了",
        ] {
            harness.get_by_label(label);
        }
    }

    #[test]
    fn export_dialog_offers_the_three_qualities_and_reports_an_empty_timeline() {
        use egui_kittest::kittest::NodeT as _;

        let mut harness = harness(MvmApp::new());
        harness.get_by_label("ファイル").click();
        harness.run();
        harness.get_by_label("書き出し").click();
        harness.run();
        assert!(harness.state().export.is_open(), "dialog が開きません");
        harness.get_by_label_contains("1920×1080・60 fps・0 frame（0.00 秒）");
        for label in [
            "高品質（CRF 18・容量大）",
            "標準（CRF 23）",
            "容量優先（CRF 28・画質低下）",
        ] {
            harness.get_by_label(label);
        }
        // 字幕の無い Project では焼き込みを選べない。
        assert!(
            harness
                .get_by_label("字幕を焼き込む")
                .accesskit_node()
                .is_disabled()
        );

        harness.get_by_label("書き出す").click();
        harness.run();
        harness.get_by_label_contains("書き出し先が指定されていません");
        assert!(!harness.state().session.is_exporting());
        assert!(
            harness.state().export.is_open(),
            "失敗で dialog が閉じました"
        );

        harness.get_by_label("キャンセル").click();
        harness.run();
        assert!(!harness.state().export.is_open());
    }

    #[test]
    fn ctrl_m_opens_the_export_dialog() {
        let mut harness = harness(MvmApp::new());
        harness.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::M);
        harness.run();
        assert!(harness.state().export.is_open());
        harness.get_by_label("書き出す");
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

    #[test]
    fn closing_a_dirty_project_asks_first_and_a_clean_one_closes() {
        let mut harness = harness(MvmApp::new());
        harness.state_mut().session.mark_changed();
        let ctx = harness.ctx.clone();
        harness.state_mut().handle_close_request(&ctx);
        harness.run();
        harness.get_by_label("保存していない変更があります");
        harness.get_by_label("破棄して終了");
        assert!(
            !harness.state().close_approved,
            "確認の前に終了を認めました"
        );

        let mut clean = MvmApp::new();
        clean.handle_close_request(&ctx);
        assert!(clean.confirm.is_none(), "未変更なのに確認を出しました");
    }

    /// 実際の mouse と同じく、押す・離すを別の frame で送る（accesskit の click は pointer を通らない）。
    fn press_and_release(harness: &mut Harness<'static, MvmApp>, from: egui::Pos2, to: egui::Pos2) {
        harness.hover_at(from);
        harness.event(egui::Event::PointerButton {
            pos: from,
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: egui::Modifiers::NONE,
        });
        harness.run();
        if from != to {
            harness.hover_at(egui::pos2((from.x + to.x) / 2.0, (from.y + to.y) / 2.0));
            harness.run();
            harness.hover_at(to);
            harness.run();
        }
        harness.event(egui::Event::PointerButton {
            pos: to,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: egui::Modifiers::NONE,
        });
        harness.run();
    }

    fn harness_with(clips: Vec<TimelineClip>) -> Harness<'static, MvmApp> {
        let project = Project {
            timeline_clips: clips,
            ..Project::default()
        };
        let mut harness = harness(MvmApp::new());
        harness.state_mut().session = Session::from_project(project);
        harness.run();
        harness
    }

    #[test]
    fn clicking_a_clip_selects_it() {
        let mut harness = harness_with(vec![text_clip("A", 0), text_clip("B", 40)]);
        let b = harness.get_by_label("B").rect().center();
        press_and_release(&mut harness, b, b);
        assert_eq!(harness.state().session.selection(), ["id-B".to_owned()]);
    }

    /// 検証を通る文字 clip を playhead（0 frame）に置いた画面。clip は画面より長いので、中心ではなく左寄りを押す。
    fn harness_with_placed_text(name: &str) -> Harness<'static, MvmApp> {
        let mut harness = harness(MvmApp::new());
        harness.state_mut().session.place_text(name).expect("文字");
        harness.run();
        harness
    }

    fn clip_range(clip: &TimelineClip) -> (i64, i64) {
        let start = clip.timeline_start_frame;
        (start, start + clip.source_out_frame - clip.source_in_frame)
    }

    #[test]
    fn a_razor_click_without_shift_splits_the_clip() {
        let mut harness = harness_with_placed_text("切る文字");
        let (_, end) = clip_range(&harness.state().session.project().timeline_clips[0]);
        harness.state_mut().session.set_tool(TimelineTool::Razor);
        harness.run();
        let rect = harness.get_by_label("切る文字").rect();
        let at = egui::pos2(rect.min.x + 80.0, rect.center().y);
        press_and_release(&mut harness, at, at);
        let session = &harness.state().session;
        let mut ranges: Vec<(i64, i64)> = session
            .project()
            .timeline_clips
            .iter()
            .map(clip_range)
            .collect();
        ranges.sort();
        assert_eq!(
            ranges.len(),
            2,
            "レーザーのクリックで分割されません: {:?}",
            session.alert()
        );
        assert_eq!(ranges[0].0, 0);
        assert_eq!(ranges[0].1, ranges[1].0);
        assert_eq!(ranges[1].1, end);
        assert!(ranges[0].1 > 0 && ranges[0].1 < end);
    }

    #[test]
    fn dragging_a_clip_moves_it_by_the_pointer_delta() {
        let mut harness = harness_with_placed_text("動かす文字");
        let rect = harness.get_by_label("動かす文字").rect();
        let at = egui::pos2(rect.min.x + 80.0, rect.center().y);
        // 100% は 1 frame 8px。80px 右は 10 frame。
        press_and_release(&mut harness, at, at + egui::vec2(80.0, 0.0));
        let session = &harness.state().session;
        assert_eq!(
            session.project().timeline_clips[0].timeline_start_frame,
            10,
            "{:?}",
            session.alert()
        );
    }

    #[test]
    fn tool_button_track_visibility_and_undo_are_reachable() {
        let mut harness = harness(MvmApp::new());
        harness.get_by_label("ツール 選択");
        harness.get_by_label("レーザー").click();
        harness.run();
        harness.get_by_label("ツール レーザー");

        harness.get_by_label("V1 表示").click();
        harness.run();
        harness.get_by_label("V1 非表示");
        assert!(harness.state().session.project().video_tracks[0].muted);

        harness
            .state_mut()
            .session
            .place_text("戻す文字")
            .expect("文字");
        harness.run();
        harness.get_by_label("戻す文字");
        harness.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Z);
        harness.run();
        assert!(
            harness.query_by_label("戻す文字").is_none(),
            "元に戻すで文字 clip が消えません"
        );
        assert!(harness.state().session.project().timeline_clips.is_empty());
    }
}
