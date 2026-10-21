//! メニューと、プロジェクト・preview・timeline を並べた shell。

use std::path::PathBuf;
use std::time::Duration;

use eframe::egui::{self, Key, KeyboardShortcut, Modifiers, TextureHandle};
use mvm_app::{AppError, ExistingFile, RecoveryPrompt, ReplaceApproval, Session, replace_approval};
use mvm_project::timeline::TimelineTool;

use crate::dialog;
use crate::widgets::{self, ButtonKind};
use crate::{
    audio_adjust_ui, clip_dialogs, export_ui, inspector, math_ui, mixer_ui, preview_ui,
    project_panel, subtitle_panel, theme, timeline_ui, transcribe_ui,
};

/// 下段の timeline の初めの高さと、縮められる下限。
/// 初めの高さは窓の残りの高さに対する割合（C++ 版は上段 42%・timeline 48%）。
const TIMELINE_DEFAULT_SHARE: f32 = 0.53;
const TIMELINE_MINIMUM_HEIGHT: f32 = 200.0;
const LEFT_PANEL_DEFAULT_WIDTH: f32 = 520.0;
/// preview の右のマスターの帯（フェーダー 43 + メーター 29 + 余白）。
const MASTER_STRIP_WIDTH: f32 = 106.0;

/// 自動保存と外部変更の検査は入力が無くても進める。egui は入力が無いと描き直さないので、この間隔で起こす。
const MAINTENANCE_INTERVAL: Duration = Duration::from_millis(250);

/// 左パネルの表示。premiere と同じく、同じ領域をタブで切り替える。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LeftTab {
    /// 選択中の clip・編集点・マーカー（インスペクタ）。
    #[default]
    Effects,
    Project,
    Mixer,
    Subtitles,
}

#[derive(Clone, Copy)]
enum FileAction {
    New,
    Open,
    Close,
}

/// 新しい保存先へ Project を書く操作。
#[derive(Clone, Copy)]
enum SaveTarget {
    New,
    SaveAs,
}

/// 既にある file を置き換えてよいかの確認。確認を出した時点の実体を承認として持ち、
/// 承認の後に別の file へ変わっていれば session が置き換えずに失敗する。
struct PendingOverwrite {
    target: SaveTarget,
    approval: ReplaceApproval,
    /// 保存できたら続ける操作（未保存の確認から保存したとき）。
    then: Option<FileAction>,
}

enum SaveOutcome {
    Saved,
    Cancelled,
    AwaitingOverwrite,
}

pub struct MvmApp {
    session: Session,
    confirm: Option<FileAction>,
    overwrite: Option<PendingOverwrite>,
    notice: String,
    preview_texture: Option<(u64, TextureHandle)>,
    preview_drag: Option<preview_ui::TransformDrag>,
    shown_title: Option<String>,
    timeline: timeline_ui::TimelineState,
    subtitle_panel: subtitle_panel::SubtitlePanel,
    transcribe: transcribe_ui::TranscribeDialog,
    inspector: inspector::InspectorState,
    export: export_ui::ExportDialog,
    audio_adjust: audio_adjust_ui::AudioAdjustDialog,
    manim: math_ui::ManimDialog,
    mixer: mixer_ui::MixerState,
    project_panel: project_panel::ProjectPanel,
    speed: clip_dialogs::SpeedDialog,
    settings: clip_dialogs::SettingsDialog,
    left_tab: LeftTab,
    text_draft: String,
    recovery_closed: bool,
    external_closed: bool,
    /// 確認を済ませた終了。次の close 要求は止めない。
    close_approved: bool,
    close_now: bool,
}

impl MvmApp {
    /// 起動引数で渡された Project を開く。開けなければ空の Project のまま、理由を知らせる。
    pub fn open_at_start(&mut self, path: &std::path::Path) {
        if let Err(error) = self.session.open_project(path) {
            self.notice = format!("{} を開けません: {}", path.display(), error.message());
        }
    }

    pub fn new() -> Self {
        Self {
            session: Session::new(),
            confirm: None,
            overwrite: None,
            notice: String::new(),
            preview_texture: None,
            preview_drag: None,
            shown_title: None,
            timeline: timeline_ui::TimelineState::default(),
            subtitle_panel: subtitle_panel::SubtitlePanel::default(),
            transcribe: transcribe_ui::TranscribeDialog::default(),
            inspector: inspector::InspectorState::default(),
            export: export_ui::ExportDialog::default(),
            audio_adjust: audio_adjust_ui::AudioAdjustDialog::default(),
            manim: math_ui::ManimDialog::default(),
            mixer: mixer_ui::MixerState::default(),
            project_panel: project_panel::ProjectPanel::default(),
            speed: clip_dialogs::SpeedDialog::default(),
            settings: clip_dialogs::SettingsDialog::default(),
            left_tab: LeftTab::default(),
            text_draft: String::new(),
            recovery_closed: false,
            external_closed: false,
            close_approved: false,
            close_now: false,
        }
    }

    /// 試験が、字幕パネルの本文へ入力する。
    #[cfg(test)]
    pub fn subtitle_panel_draft(&mut self, text: &str) {
        self.subtitle_panel.set_content(text);
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
                    let result = self.write_new_target(SaveTarget::New, path, None);
                    self.note(result.map(|_| ()));
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
            let result = self.write_new_target(SaveTarget::SaveAs, path, None);
            self.note(result.map(|_| ()));
        }
    }

    fn save_as(&mut self) {
        if let Some(path) = pick_save("名前を付けて保存") {
            let result = self.write_new_target(SaveTarget::SaveAs, path, None);
            self.note(result.map(|_| ()));
        }
    }

    /// 保存して `then` へ続ける。保存先を選び直して既存の file を置き換えるときは、確認の後に続ける。
    fn save_current(&mut self, then: FileAction) -> Result<SaveOutcome, AppError> {
        if self.session.path().is_some() {
            self.session.save_project()?;
            Ok(SaveOutcome::Saved)
        } else if let Some(path) = pick_save("プロジェクトの保存先") {
            self.write_new_target(SaveTarget::SaveAs, path, Some(then))
        } else {
            Ok(SaveOutcome::Cancelled)
        }
    }

    /// 選んだ保存先へ書く。file が既にあれば書かずに置き換えの確認を出す（OS の dialog の確認は、
    /// 書き込みまでの間に現れた file を承認しないので頼らない）。無ければ、既存を置き換えない書き込みにする。
    fn write_new_target(
        &mut self,
        target: SaveTarget,
        path: PathBuf,
        then: Option<FileAction>,
    ) -> Result<SaveOutcome, AppError> {
        match replace_approval(&path)? {
            None => {
                self.write_target(target, &path, ExistingFile::Keep)?;
                Ok(SaveOutcome::Saved)
            }
            Some(approval) => {
                self.overwrite = Some(PendingOverwrite {
                    target,
                    approval,
                    then,
                });
                Ok(SaveOutcome::AwaitingOverwrite)
            }
        }
    }

    fn write_target(
        &mut self,
        target: SaveTarget,
        path: &std::path::Path,
        existing: ExistingFile,
    ) -> Result<(), AppError> {
        match target {
            SaveTarget::New => self.session.create_project(path, existing),
            SaveTarget::SaveAs => self.session.save_project_as(path, existing),
        }
    }

    /// 確認した file を置き換え、続ける操作があれば続ける。
    fn approve_overwrite(&mut self) {
        let Some(pending) = self.overwrite.take() else {
            return;
        };
        let path = pending.approval.path().to_path_buf();
        match self.write_target(
            pending.target,
            &path,
            ExistingFile::Replace(pending.approval),
        ) {
            Ok(()) => {
                self.notice.clear();
                if let Some(then) = pending.then {
                    self.perform(then);
                }
            }
            Err(error) => self.notice = error.message().to_owned(),
        }
    }

    fn confirm_overwrite(&mut self, ui: &mut egui::Ui) {
        let Some(pending) = &self.overwrite else {
            return;
        };
        let path = pending.approval.path().display().to_string();
        let mut replace = None;
        let width = (ui.ctx().content_rect().width() - 40.0).clamp(300.0, 520.0);
        let ((), close) = dialog::show(
            ui.ctx(),
            "overwrite",
            "ファイルを置き換え",
            width,
            |ui| {
                ui.label(format!("{path} は既にあります。置き換えますか？"));
            },
            |ui| {
                if widgets::dialog_button(ui, "置き換える", ButtonKind::Destructive).clicked()
                {
                    replace = Some(true);
                }
                if widgets::dialog_button(ui, "キャンセル", ButtonKind::Normal).clicked() {
                    replace = Some(false);
                }
            },
        );
        match replace {
            Some(true) => self.approve_overwrite(),
            Some(false) => self.overwrite = None,
            None if close => self.overwrite = None,
            None => {}
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
            || self.overwrite.is_some()
            || self.recovery_visible()
            || self.external_visible()
            || self.export.is_open()
            || self.speed.is_open()
            || self.settings.is_open()
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
            } else if input.consume_key(Modifiers::NONE, Key::J) {
                Seek::Shuttle(-1)
            } else if input.consume_key(Modifiers::NONE, Key::K) {
                Seek::Stop
            } else if input.consume_key(Modifiers::NONE, Key::L) {
                Seek::Shuttle(1)
            } else {
                Seek::None
            }
        });
        let now = now_ms(ui);
        self.run_seek(seek, now);
    }

    fn run_seek(&mut self, seek: Seek, now: u64) {
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
            Seek::Shuttle(direction) => {
                let result = self.session.shuttle(direction, now);
                self.note(result);
            }
            Seek::Stop => self.session.stop_shuttle(),
            Seek::Edit(direction) => self.session.jump_edit_point(direction > 0),
            Seek::None => {}
        }
    }

    fn handle_edit_shortcuts(&mut self, ui: &mut egui::Ui) {
        // エフェクトコントロールでキーを押した後は、コピー・カット・ペースト・削除をキーへ向ける。
        let keys = self.left_tab == LeftTab::Effects && self.inspector.wants_key_shortcuts();
        let command = ui.input_mut(|input| {
            if input.consume_shortcut(&KeyboardShortcut::new(
                Modifiers::COMMAND | Modifiers::SHIFT,
                Key::Z,
            )) || input.consume_shortcut(&KeyboardShortcut::new(Modifiers::COMMAND, Key::Y))
            {
                EditCommand::Redo
            } else if input.consume_shortcut(&KeyboardShortcut::new(Modifiers::COMMAND, Key::Z)) {
                EditCommand::Undo
            } else if !keys
                && input.consume_shortcut(&KeyboardShortcut::new(Modifiers::COMMAND, Key::C))
            {
                EditCommand::Copy
            } else if !keys
                && input.consume_shortcut(&KeyboardShortcut::new(Modifiers::COMMAND, Key::X))
            {
                EditCommand::Cut
            } else if !keys
                && input.consume_shortcut(&KeyboardShortcut::new(Modifiers::COMMAND, Key::V))
            {
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
            } else if input.consume_shortcut(&KeyboardShortcut::new(Modifiers::COMMAND, Key::R)) {
                EditCommand::SpeedDuration
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
            } else if !keys
                && (input.consume_key(Modifiers::NONE, Key::Delete)
                    || input.consume_key(Modifiers::NONE, Key::Backspace))
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
            EditCommand::SpeedDuration => {
                self.open_speed();
                Ok(())
            }
        };
        self.note(result);
    }

    /// 選択中の clip の速度・デュレーションの dialog を開く。
    fn open_speed(&mut self) {
        match self.session.selected_clip().map(str::to_owned) {
            Some(id) => self.speed.open(&mut self.session, &id),
            None => self.session.set_alert("速度を変える clip を選んでください"),
        }
    }

    /// メニュー。項目の名前・並び・ショートカットの表示は C++ 版と同じにする。
    fn menu(&mut self, ui: &mut egui::Ui) {
        egui::MenuBar::new().ui(ui, |ui| {
            ui.menu_button("ファイル(F)", |ui| self.file_menu(ui));
            ui.menu_button("編集(E)", |ui| self.edit_menu(ui));
            ui.menu_button("再生(L)", |ui| self.playback_menu(ui));
            ui.menu_button("プロジェクト(P)", |ui| {
                if ui.button("プロジェクト設定").clicked() {
                    self.settings.open(&self.session);
                    ui.close();
                }
            });
        });
    }

    fn file_menu(&mut self, ui: &mut egui::Ui) {
        // 書き出し中は Project を切り替えない（書き出しは開始時の Project を書き続ける）。
        // 名前を付けて保存も session を入れ直すので同じ扱いにする。
        let switchable = !self.session.is_exporting();
        if ui
            .add_enabled(switchable, egui::Button::new("新規プロジェクト"))
            .clicked()
        {
            self.request(FileAction::New);
            ui.close();
        }
        if ui
            .add_enabled(
                switchable,
                egui::Button::new("プロジェクトを開く").shortcut_text("Ctrl+O"),
            )
            .clicked()
        {
            self.request(FileAction::Open);
            ui.close();
        }
        ui.separator();
        if ui
            .add_enabled(
                self.session.dirty(),
                egui::Button::new("保存").shortcut_text("Ctrl+S"),
            )
            .clicked()
        {
            self.save();
            ui.close();
        }
        if ui
            .add_enabled(
                switchable,
                egui::Button::new("名前を付けて保存").shortcut_text("Ctrl+Shift+S"),
            )
            .clicked()
        {
            self.save_as();
            ui.close();
        }
        ui.separator();
        if ui.button("メディアを追加").clicked() {
            ui.close();
            if let Some(path) = pick_media_file() {
                self.perform_edit(|session| session.add_media_file_at_playhead(&path));
            }
        }
        if ui.button("Manim script clip…").clicked() {
            self.manim.open();
            ui.close();
        }
        for (label, add) in [
            (
                "数式 clip を追加",
                Session::add_math_clip as fn(&mut Session) -> _,
            ),
            ("数式 sequence を追加", Session::add_equation_sequence_clip),
            ("グラフ clip を追加", Session::add_graph_clip),
        ] {
            if ui.button(label).clicked() {
                self.perform_edit(add);
                // 置いた clip の中身はエフェクトコントロールで書く。
                self.left_tab = LeftTab::Effects;
                ui.close();
            }
        }
        ui.separator();
        if ui
            .add_enabled(
                !self.session.is_exporting(),
                egui::Button::new("メディアを書き出し").shortcut_text("Ctrl+M"),
            )
            .clicked()
        {
            self.export.open(&self.session);
            ui.close();
        }
        ui.separator();
        if ui.button("終了").clicked() {
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }

    fn edit_menu(&mut self, ui: &mut egui::Ui) {
        let mut command = EditCommand::None;
        let mut item =
            |ui: &mut egui::Ui, label: &str, key: &str, enabled: bool, run: EditCommand| {
                if ui
                    .add_enabled(enabled, egui::Button::new(label).shortcut_text(key))
                    .clicked()
                {
                    command = run;
                    ui.close();
                }
            };
        let selected = !self.session.selection().is_empty();
        item(
            ui,
            "元に戻す",
            "Ctrl+Z",
            self.session.can_undo(),
            EditCommand::Undo,
        );
        item(
            ui,
            "やり直し",
            "Ctrl+Shift+Z",
            self.session.can_redo(),
            EditCommand::Redo,
        );
        ui.separator();
        item(
            ui,
            "クリップをコピー",
            "Ctrl+C",
            selected,
            EditCommand::Copy,
        );
        item(ui, "クリップをカット", "Ctrl+X", selected, EditCommand::Cut);
        item(
            ui,
            "クリップをペースト",
            "Ctrl+V",
            self.session.has_clipboard(),
            EditCommand::Paste,
        );
        item(
            ui,
            "クリップを複製",
            "Ctrl+D",
            selected,
            EditCommand::Duplicate,
        );
        item(
            ui,
            "速度・デュレーション...",
            "Ctrl+R",
            selected,
            EditCommand::SpeedDuration,
        );
        if ui
            .add_enabled(
                !self.audio_adjust.is_open(),
                egui::Button::new("自動音量調整…"),
            )
            .clicked()
        {
            self.audio_adjust.open(&self.session);
            ui.close();
        }
        ui.separator();
        item(ui, "編集点を追加", "Ctrl+K", selected, EditCommand::Split);
        item(
            ui,
            "編集点をすべてのトラックに追加",
            "Ctrl+Shift+K",
            true,
            EditCommand::SplitAll,
        );
        item(ui, "すべてを選択", "Ctrl+A", true, EditCommand::SelectAll);
        ui.separator();
        item(
            ui,
            "クリップの音量を上げる",
            "[",
            selected,
            EditCommand::VolumeUp,
        );
        item(
            ui,
            "クリップの音量を下げる",
            "]",
            selected,
            EditCommand::VolumeDown,
        );
        item(
            ui,
            "有効/無効を切り換え",
            "Shift+E",
            selected,
            EditCommand::ToggleEnabled,
        );
        item(
            ui,
            "デフォルトのトランジションを適用",
            "Shift+D",
            true,
            EditCommand::Transition,
        );
        if ui
            .add_enabled(
                self.session.selected_edge().is_some(),
                egui::Button::new("数式の変形を適用"),
            )
            .clicked()
        {
            self.perform_edit(Session::apply_math_transform);
            ui.close();
        }
        ui.separator();
        item(
            ui,
            "クリップを削除",
            "Delete",
            selected,
            EditCommand::Delete,
        );
        self.run_edit(command);
    }

    fn playback_menu(&mut self, ui: &mut egui::Ui) {
        let now = now_ms(ui);
        let mut seek = Seek::None;
        let verb = if self.session.is_playing() {
            "一時停止"
        } else {
            "再生"
        };
        if ui
            .add(egui::Button::new(verb).shortcut_text("Space"))
            .clicked()
        {
            seek = Seek::Play;
            ui.close();
        }
        ui.separator();
        let mut command = EditCommand::None;
        for (label, key, run) in [
            ("マーカーを追加", "M", EditCommand::Marker),
            ("次のマーカーへ移動", "Shift+M", EditCommand::NextMarker),
            (
                "前のマーカーへ移動",
                "Ctrl+Shift+M",
                EditCommand::PreviousMarker,
            ),
            ("インをマーク", "I", EditCommand::MarkIn),
            ("アウトをマーク", "O", EditCommand::MarkOut),
            ("インへ移動", "Shift+I", EditCommand::JumpIn),
            ("アウトへ移動", "Shift+O", EditCommand::JumpOut),
            ("インとアウトを消去", "Alt+X", EditCommand::ClearMarks),
        ] {
            if ui
                .add(egui::Button::new(label).shortcut_text(key))
                .clicked()
            {
                command = run;
                ui.close();
            }
        }
        ui.separator();
        for (label, key, action) in [
            ("左へシャトル", "J", Seek::Shuttle(-1)),
            ("停止", "K", Seek::Stop),
            ("右へシャトル", "L", Seek::Shuttle(1)),
        ] {
            if ui
                .add(egui::Button::new(label).shortcut_text(key))
                .clicked()
            {
                seek = action;
                ui.close();
            }
        }
        ui.separator();
        for (label, key, action) in [
            ("1フレーム前へ", "←", Seek::By(-1)),
            ("1フレーム先へ", "→", Seek::By(1)),
            ("5フレーム前へ", "Shift+←", Seek::By(-5)),
            ("5フレーム先へ", "Shift+→", Seek::By(5)),
        ] {
            if ui
                .add(egui::Button::new(label).shortcut_text(key))
                .clicked()
            {
                seek = action;
                ui.close();
            }
        }
        ui.separator();
        for (label, key, action) in [
            ("前の編集点へ", "↑", Seek::Edit(-1)),
            ("次の編集点へ", "↓", Seek::Edit(1)),
        ] {
            if ui
                .add(egui::Button::new(label).shortcut_text(key))
                .clicked()
            {
                seek = action;
                ui.close();
            }
        }
        self.run_edit(command);
        self.run_seek(seek, now);
    }

    /// 窓の上端の作業状態。知らせ（失敗・書き出しの完了）が無ければ、preview に見えているもの。
    fn status(&mut self, ui: &mut egui::Ui) {
        let (message, color) = if !self.notice.is_empty() {
            (self.notice.clone(), theme::ERROR)
        } else if !self.export.finished.is_empty() {
            (self.export.finished.clone(), theme::TEXT)
        } else if self.session.is_playing() {
            ("再生中".to_owned(), theme::TEXT)
        } else {
            let shown = self
                .session
                .effect_target()
                .map(|clip| format!("{} を表示しています", clip.name))
                .unwrap_or_else(|| "timeline gapを表示しています".to_owned());
            (shown, theme::TEXT)
        };
        ui.add(egui::Label::new(egui::RichText::new(message).color(color)).truncate());
    }
    /// 左パネルのタブ。選んでいるタブは太字と下線で示す。
    fn tabs(&mut self, ui: &mut egui::Ui) {
        let project = self
            .session
            .path()
            .and_then(|path| path.file_name())
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "無題".to_owned());
        let tabs = [
            (LeftTab::Effects, "エフェクトコントロール".to_owned()),
            (LeftTab::Project, format!("プロジェクト: {project}")),
            (LeftTab::Mixer, "オーディオミキサー".to_owned()),
            (LeftTab::Subtitles, "字幕".to_owned()),
        ];
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            // 収まるなら各タブを名前の幅に余りを等分して足した幅にし、収まらなければ名前の幅の比で縮める。
            // 等分すると、短い名前の余りを使えずに長い名前だけが切れる。
            let font = egui::FontId::proportional(12.0);
            let natural: Vec<f32> = tabs
                .iter()
                .map(|(_, label)| {
                    ui.painter()
                        .layout_no_wrap(label.clone(), font.clone(), theme::TEXT)
                        .size()
                        .x
                        + 16.0
                })
                .collect();
            let total: f32 = natural.iter().sum();
            let available = ui.available_width();
            let extra = (available - total).max(0.0) / natural.len() as f32;
            let scale = (available / total).min(1.0);
            for ((tab, label), natural) in tabs.into_iter().zip(natural) {
                let active = self.left_tab == tab;
                let width = natural * scale + extra;
                let text = egui::RichText::new(label).size(12.0).color(if active {
                    theme::TEXT
                } else {
                    theme::TEXT_TAB
                });
                let text = if active { text.strong() } else { text };
                let response = ui.add_sized(
                    [width, 24.0],
                    egui::Label::new(text)
                        .truncate()
                        .sense(egui::Sense::click()),
                );
                if active {
                    let line = egui::Rect::from_min_max(
                        egui::pos2(response.rect.min.x, response.rect.max.y - 2.0),
                        response.rect.max,
                    );
                    ui.painter().rect_filled(line, 0.0, theme::ACCENT);
                }
                if response
                    .on_hover_cursor(egui::CursorIcon::PointingHand)
                    .clicked()
                {
                    self.left_tab = tab;
                }
            }
        });
        let y = ui.cursor().min.y;
        ui.painter().hline(
            ui.max_rect().x_range(),
            y,
            egui::Stroke::new(1.0, theme::PANEL_BORDER),
        );
        ui.add_space(6.0);
    }

    fn confirm_unsaved(&mut self, ui: &mut egui::Ui) {
        let Some(action) = self.confirm else {
            return;
        };
        let mut choice = None;
        let width = (ui.ctx().content_rect().width() - 40.0).clamp(300.0, 420.0);
        dialog::show(
            ui.ctx(),
            "unsaved",
            "未保存の変更",
            width,
            |ui| {
                ui.label("プロジェクトへの変更を保存しますか？");
            },
            |ui| {
                if widgets::dialog_button(ui, "保存", ButtonKind::Prominent).clicked() {
                    choice = Some(UnsavedChoice::Save);
                }
                if widgets::dialog_button(ui, "保存しない", ButtonKind::Destructive).clicked()
                {
                    choice = Some(UnsavedChoice::Discard);
                }
                if widgets::dialog_button(ui, "キャンセル", ButtonKind::Normal).clicked() {
                    choice = Some(UnsavedChoice::Cancel);
                }
            },
        );
        match choice {
            Some(UnsavedChoice::Save) => match self.save_current(action) {
                Ok(SaveOutcome::Saved) => {
                    self.notice.clear();
                    self.perform(action);
                }
                Ok(SaveOutcome::AwaitingOverwrite) => self.confirm = None,
                Ok(SaveOutcome::Cancelled) => {}
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
        let path = self
            .session
            .path()
            .map(|path| path.display().to_string())
            .unwrap_or_default();
        let (title, text) = match prompt {
            RecoveryPrompt::Corrupt => (
                "自動復旧データを読めません",
                format!(
                    "自動復旧データが壊れているため、最後に保存したProjectを開きました。復旧fileは残しています。\n{path}"
                ),
            ),
            RecoveryPrompt::Foreign => (
                "別のProjectの自動復旧データです",
                format!(
                    "この自動復旧データは、今開いているProjectのものではありません。fileは残しています。\n{path}"
                ),
            ),
            RecoveryPrompt::CanonicalChanged => (
                "Project fileが外部で変更されています",
                format!(
                    "自動保存のあとでProject fileの内容が変わっています。復元すると、その変更は明示保存するまでfileへ書き込まれません。\n{path}"
                ),
            ),
            RecoveryPrompt::Restorable => (
                "自動保存された編集があります",
                format!(
                    "前回、正常に保存されなかった編集が見つかりました。\n{path}\n\n自動保存された編集を復元しますか？"
                ),
            ),
        };
        let mut choice = None;
        let width = (ui.ctx().content_rect().width() - 40.0).clamp(300.0, 620.0);
        dialog::show(
            ui.ctx(),
            "recovery",
            title,
            width,
            |ui| {
                ui.label(&text);
            },
            |ui| match prompt {
                RecoveryPrompt::Restorable | RecoveryPrompt::CanonicalChanged => {
                    if widgets::dialog_button(ui, "復元する", ButtonKind::Prominent).clicked() {
                        choice = Some(RecoveryChoice::Restore);
                    }
                    let discard = if prompt == RecoveryPrompt::CanonicalChanged {
                        "現在のProjectを開く"
                    } else {
                        "最後の保存状態を使う"
                    };
                    if widgets::dialog_button(ui, discard, ButtonKind::Destructive).clicked() {
                        choice = Some(RecoveryChoice::Discard);
                    }
                    if prompt == RecoveryPrompt::CanonicalChanged
                        && widgets::dialog_button(ui, "キャンセル", ButtonKind::Normal).clicked()
                    {
                        choice = Some(RecoveryChoice::Close);
                    }
                }
                RecoveryPrompt::Corrupt | RecoveryPrompt::Foreign => {
                    if widgets::dialog_button(ui, "OK", ButtonKind::Prominent).clicked() {
                        choice = Some(RecoveryChoice::Close);
                    }
                }
            },
        );
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
        let width = (ui.ctx().content_rect().width() - 40.0).clamp(300.0, 620.0);
        dialog::show(
            ui.ctx(),
            "external",
            "Project fileが外部で変更されています",
            width,
            |ui| {
                ui.label(
                    "このProjectを開いたあとで、fileの内容が変わっています。このまま保存すると、その変更を上書きします。",
                );
            },
            |ui| {
                if widgets::dialog_button(ui, "上書きする", ButtonKind::Destructive).clicked()
                {
                    choice = Some(ExternalChoice::Overwrite);
                }
                if widgets::dialog_button(ui, "名前を付けて保存", ButtonKind::Normal).clicked()
                {
                    choice = Some(ExternalChoice::SaveAs);
                }
                if widgets::dialog_button(ui, "読み直す", ButtonKind::Normal).clicked() {
                    choice = Some(ExternalChoice::Reload);
                }
                if widgets::dialog_button(ui, "キャンセル", ButtonKind::Normal).clicked() {
                    choice = Some(ExternalChoice::Cancel);
                }
            },
        );
        match choice {
            Some(ExternalChoice::Reload) => self.perform_edit(Session::reload_project),
            Some(ExternalChoice::Overwrite) => {
                self.perform_edit(Session::save_overwriting_external);
            }
            Some(ExternalChoice::SaveAs) => {
                self.external_closed = true;
                self.save_as();
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
    Shuttle(i32),
    Stop,
}

/// egui の入力時刻（ミリ秒）。シャトルと自動保存の時計に使う。
fn now_ms(ui: &egui::Ui) -> u64 {
    let seconds = ui.input(|input| input.time);
    if seconds.is_finite() && seconds > 0.0 {
        (seconds * 1000.0) as u64
    } else {
        0
    }
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
    SpeedDuration,
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
    SaveAs,
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

fn pick_media_file() -> Option<PathBuf> {
    rfd::FileDialog::new()
        .set_title("メディアを追加")
        .pick_file()
}

/// 保存先を選ぶ。既にある file を置き換えるかは、書く直前に [`MvmApp::write_new_target`] が確かめる。
fn pick_save(title: &str) -> Option<PathBuf> {
    rfd::FileDialog::new()
        .set_title(title)
        .add_filter("mvm project", &["mvm"])
        .save_file()
        .map(ensure_mvm)
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
        let now_ms = now_ms(ui);
        if ui.input(|input| input.viewport().close_requested()) {
            self.handle_close_request(ui.ctx());
        }
        self.session.poll_maintenance(now_ms);
        let math = self.session.poll_math();
        let waveforms = self.session.poll_waveforms(now_ms);
        let shuttle = self.session.poll_shuttle(now_ms);
        if math || waveforms || shuttle {
            ui.ctx().request_repaint();
        }
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
        egui::Panel::top("menu")
            .frame(
                egui::Frame::new()
                    .fill(theme::WINDOW)
                    .inner_margin(egui::Margin::symmetric(6, 2)),
            )
            .show(ui, |ui| self.menu(ui));
        egui::Panel::top("status")
            .frame(
                egui::Frame::new()
                    .fill(theme::WINDOW)
                    .inner_margin(egui::Margin::symmetric(10, 4)),
            )
            .show(ui, |ui| self.status(ui));
        self.mixer.update(ui, &mut self.session);
        egui::Panel::bottom("timeline")
            .resizable(true)
            .default_size(
                (ui.available_height() * TIMELINE_DEFAULT_SHARE).max(TIMELINE_MINIMUM_HEIGHT),
            )
            .size_range(TIMELINE_MINIMUM_HEIGHT..=f32::INFINITY)
            .frame(
                egui::Frame::new()
                    .fill(theme::WINDOW)
                    .inner_margin(egui::Margin {
                        left: 10,
                        right: 10,
                        top: 6,
                        bottom: 10,
                    }),
            )
            .show(ui, |ui| {
                timeline_ui::show(ui, &mut self.session, &mut self.timeline);
            });
        if std::mem::take(&mut self.timeline.open_subtitles) {
            self.left_tab = LeftTab::Subtitles;
        }
        // OS から落とされたファイルはプロジェクトパネルへ読み込む。Windows の OLE のドラッグ中は窓に pointer の
        // 位置が届かず、どの行に落としたかを確かめられないので、timeline へは置かない。
        let dropped: Vec<PathBuf> = ui.input(|input| {
            input
                .raw
                .dropped_files
                .iter()
                .map(|file| file.path().to_path_buf())
                .collect()
        });
        if !dropped.is_empty() {
            project_panel::import(&mut self.session, &dropped, "");
            self.left_tab = LeftTab::Project;
        }
        egui::Panel::left("left_panel")
            .resizable(true)
            .default_size(LEFT_PANEL_DEFAULT_WIDTH)
            .size_range(240.0..=f32::INFINITY)
            .frame(theme::panel_frame().outer_margin(egui::Margin {
                left: 10,
                right: 4,
                top: 0,
                bottom: 4,
            }))
            .show(ui, |ui| {
                self.tabs(ui);
                match self.left_tab {
                    LeftTab::Effects => {
                        egui::ScrollArea::vertical()
                            .id_salt("inspector_scroll")
                            .show(ui, |ui| {
                                inspector::show(ui, &mut self.session, &mut self.inspector);
                            });
                    }
                    LeftTab::Project => {
                        project_panel::show(ui, &mut self.session, &mut self.project_panel);
                    }
                    LeftTab::Mixer => mixer_ui::show(ui, &mut self.session, &mut self.mixer),
                    LeftTab::Subtitles => {
                        egui::ScrollArea::vertical()
                            .id_salt("subtitle_scroll")
                            .show(ui, |ui| {
                                subtitle_panel::show(
                                    ui,
                                    &mut self.session,
                                    &mut self.subtitle_panel,
                                    &mut self.transcribe,
                                );
                            });
                    }
                }
            });
        egui::Panel::right("master")
            .resizable(false)
            .exact_size(MASTER_STRIP_WIDTH)
            .frame(
                egui::Frame::new()
                    .fill(theme::WINDOW)
                    .inner_margin(egui::Margin {
                        left: 8,
                        right: 10,
                        top: 0,
                        bottom: 4,
                    }),
            )
            .show(ui, |ui| {
                mixer_ui::show_master_strip(ui, &mut self.session, &mut self.mixer);
            });
        egui::CentralPanel::default()
            .frame(
                egui::Frame::new()
                    .fill(theme::PREVIEW)
                    .stroke(egui::Stroke::new(1.0, theme::PANEL_BORDER))
                    .outer_margin(egui::Margin {
                        left: 0,
                        right: 0,
                        top: 0,
                        bottom: 4,
                    }),
            )
            .show(ui, |ui| {
                preview_ui::show(
                    ui,
                    &mut self.session,
                    &mut self.preview_texture,
                    &mut self.text_draft,
                    &mut self.notice,
                    &mut self.preview_drag,
                );
            });
        self.confirm_unsaved(ui);
        self.confirm_overwrite(ui);
        self.confirm_recovery(ui);
        self.confirm_external(ui);
        export_ui::show(ui, &mut self.session, &mut self.export);
        audio_adjust_ui::show(ui, &mut self.session, &mut self.audio_adjust);
        if let Some(clip_id) = self.timeline.transcribe_clip.take() {
            self.transcribe.open(&self.session, Some(clip_id));
        }
        transcribe_ui::show(ui, &mut self.session, &mut self.transcribe);
        if let Some(clip_id) = self.timeline.speed_clip.take() {
            self.speed.open(&mut self.session, &clip_id);
        }
        if let Some(clip_id) = self.timeline.match_settings_clip.take() {
            self.settings.open_for_clip(&mut self.session, &clip_id);
        }
        clip_dialogs::show_speed(ui, &mut self.session, &mut self.speed);
        clip_dialogs::show_settings(ui, &mut self.session, &mut self.settings);
        math_ui::show_manim_dialog(ui, &mut self.session, &mut self.manim);
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

    /// 左パネルで tab を開いた app。既定はエフェクトコントロール。
    fn on_tab(tab: LeftTab) -> MvmApp {
        let mut app = MvmApp::new();
        app.left_tab = tab;
        app
    }

    fn harness(app: MvmApp) -> Harness<'static, MvmApp> {
        harness_sized(app, egui::vec2(1200.0, 800.0))
    }

    fn harness_sized(app: MvmApp, size: egui::Vec2) -> Harness<'static, MvmApp> {
        // preview の worker は描き終えるたびに画面を起こす。描き直しが続く間も run が待ち切れるよう、step の
        // 上限を既定（4）より広げる（試験の時計は進まないので、待っても遅くならない）。
        let mut harness = Harness::builder()
            .with_size(size)
            .with_max_steps(64)
            .build_ui_state(
                |ui, app| {
                    // フォントは次の pass から効く。太字の family が揃うまでは描かない。
                    let ready = ui.ctx().fonts(|fonts| {
                        fonts
                            .families()
                            .contains(&egui::FontFamily::Name("bold".into()))
                    });
                    if ready {
                        app.show(ui);
                    } else {
                        theme::install_test_fonts(ui.ctx());
                        ui.ctx().request_repaint();
                    }
                },
                app,
            );
        harness.run();
        harness
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
        harness.get_by_label("ファイル(F)");
        harness.get_by_label("V1");
        harness.get_by_label("V2");
        harness.get_by_label("A1");
        harness.get_by_label("timeline gapを表示しています");
        harness.get_by_label_contains("zoom 150%");
        assert!(
            harness
                .query_all_by_label_contains("00:00:00:00")
                .next()
                .is_some()
        );
        // 左パネルは既定でエフェクトコントロール。プロジェクトのタブへ切り替えられる。
        harness.get_by_label("エフェクトコントロール");
        harness.get_by_label("プロジェクト: 無題").click();
        harness.run();
        harness.get_by_label("ファイルをここへドロップするか「読み込み…」で素材を追加");

        harness.get_by_label("ファイル(F)").click();
        harness.run();
        for label in [
            "新規プロジェクト",
            "プロジェクトを開く",
            "保存",
            "名前を付けて保存",
            "メディアを書き出し",
            "終了",
        ] {
            assert!(
                harness.query_all_by_label_contains(label).next().is_some(),
                "{label} がありません"
            );
        }
    }

    #[test]
    fn export_dialog_offers_the_three_qualities_and_reports_an_empty_timeline() {
        use egui_kittest::kittest::NodeT as _;

        let mut harness = harness(MvmApp::new());
        harness.get_by_label("ファイル(F)").click();
        harness.run();
        harness.get_by_label_contains("メディアを書き出し").click();
        harness.run();
        assert!(harness.state().export.is_open(), "dialog が開きません");
        // 書き出し設定は入力映像と出力の仕様、映像の無い Project の注意を出す（C++ 版と同じ）。
        harness.get_by_label("入力映像: なし");
        harness.get_by_label("出力: 1920×1080 / 60 fps");
        harness.get_by_label("映像クリップがないため、映像は黒になります。");
        // 品質は一覧から選ぶ。既定は標準。
        harness.get_by_label("標準（CRF 23）").click();
        harness.run();
        harness.get_by_label("容量優先（CRF 28・画質低下）");
        harness.get_by_label("高品質（CRF 18・容量大）").click();
        harness.run();
        assert!(
            harness.state().export.is_open(),
            "品質を選んで dialog が閉じました"
        );
        harness.get_by_label("高品質（CRF 18・容量大）");
        // 字幕の無い Project では焼き込みを選べない。
        assert!(
            harness
                .get_by_label("字幕を焼き込む")
                .accesskit_node()
                .is_disabled()
        );

        harness.get_by_label("キャンセル").click();
        harness.run();
        assert!(!harness.state().export.is_open());
    }

    #[test]
    fn export_without_an_output_path_keeps_the_dialog_and_says_why() {
        let mut harness = harness(MvmApp::new());
        harness.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::M);
        harness.run();
        // OS のダイアログを経由せず、そのまま書き出し設定が開く。保存前の Project では書き出し先は空。
        assert!(harness.state().export.is_open());
        harness.get_by_label("書き出す").click();
        harness.run();
        harness.get_by_label("書き出し先が指定されていません");
        assert!(harness.state().export.is_open());
        assert!(!harness.state().session.is_exporting());
    }

    #[test]
    fn export_to_an_existing_file_asks_before_replacing_it() {
        let dir = std::env::temp_dir().join(format!("mvm-export-ask-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("試験用の folder");
        let existing = dir.join("既存.mp4");
        std::fs::write(&existing, b"keep").expect("試験用の file");

        let mut harness = harness(MvmApp::new());
        harness.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::M);
        harness.run();
        let field = harness.get_by_role(egui::accesskit::Role::TextInput);
        field.click();
        harness.run();
        harness
            .get_by_role(egui::accesskit::Role::TextInput)
            .type_text(&existing.display().to_string());
        harness.run();
        harness.get_by_label("書き出す").click();
        harness.run();
        // 書き出さずに確認を出す。
        harness.get_by_label(&format!(
            "{} は既にあります。置き換えて書き出しますか？",
            existing.display()
        ));
        assert!(!harness.state().session.is_exporting());
        harness.get_by_label("置き換えて書き出す");
        harness.get_by_label("やめる").click();
        harness.run();
        // やめると元の設定に戻り、file はそのまま残る。
        assert!(harness.state().export.is_open());
        harness.get_by_label("書き出す");
        assert!(harness.query_by_label("置き換えて書き出す").is_none());
        assert_eq!(std::fs::read(&existing).expect("残った file"), b"keep");
        std::fs::remove_dir_all(&dir).expect("試験用の folder の片付け");
    }

    #[test]
    fn ctrl_m_opens_the_export_dialog() {
        let mut harness = harness(MvmApp::new());
        harness.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::M);
        harness.run();
        assert!(harness.state().export.is_open());
        harness.get_by_label("書き出す");
        harness.get_by_label("書き出し設定");
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
        // 2200 frame は 150% で 3300 px。最初の画面には入らず、3000 px 送ると入る。
        let project = Project {
            timeline_clips: vec![text_clip("Near", 0), text_clip("Far", 2200)],
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
        let mut harness = harness(on_tab(LeftTab::Project));
        harness.state_mut().session = Session::from_project(project);
        harness.run();
        // フォルダは閉じて始まる（C++ 版と同じ）。開閉の印で開く。
        assert!(harness.query_by_label("背景").is_none());
        harness.get_by_label("素材フォルダ を開閉").click();
        harness.run();
        let folder = harness.get_by_label("素材フォルダ").rect();
        let item = harness.get_by_label("背景").rect();
        assert!(item.min.y > folder.min.y);
        // 字下げは名前の位置で見る（行の矩形は列の幅いっぱい）。開閉の印も字下げされる。
        harness.get_by_label("素材フォルダ を開閉").click();
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
        let mut harness = harness(on_tab(LeftTab::Project));
        harness.state_mut().session = Session::from_project(project);
        harness.run();
        harness.get_by_label("導入");
        harness.get_by_label("1 項目");
        assert!(harness.query_by_label("無い素材").is_none());
    }

    #[test]
    fn zoom_button_doubles_the_scale() {
        let mut harness = harness(MvmApp::new());
        harness.get_by_label("拡大").click();
        harness.run();
        harness.get_by_label_contains("zoom 200%");
        assert!(harness.query_by_label_contains("zoom 100%").is_none());
    }

    #[test]
    fn dirty_new_asks_before_replacing_the_project() {
        let mut harness = harness(MvmApp::new());
        harness.state_mut().session.mark_changed();
        harness.state_mut().request_new();
        harness.run();
        harness.get_by_label("未保存の変更");
        harness.get_by_label("保存しない");
        harness.get_by_label("キャンセル");
    }

    #[test]
    fn saving_onto_an_existing_file_asks_and_replaces_only_the_confirmed_file() {
        let dir = std::env::temp_dir().join(format!("mvm-shell-overwrite-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("existing.mvm");
        std::fs::write(&path, b"another project").unwrap();
        let mut harness = harness(MvmApp::new());
        let outcome = harness
            .state_mut()
            .write_new_target(SaveTarget::SaveAs, path.clone(), None);
        assert!(matches!(outcome, Ok(SaveOutcome::AwaitingOverwrite)));
        assert_eq!(
            std::fs::read(&path).unwrap(),
            b"another project",
            "確認の前に書きました"
        );
        harness.run();
        harness.get_by_label("キャンセル").click();
        harness.run();
        assert!(harness.state().overwrite.is_none());
        assert_eq!(std::fs::read(&path).unwrap(), b"another project");
        assert!(harness.state().session.path().is_none());

        // 確認を出した後に別の tool が書き換えた file は、承認しても置き換えない。
        let _ = harness
            .state_mut()
            .write_new_target(SaveTarget::SaveAs, path.clone(), None);
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(&path, b"rewritten meanwhile").unwrap();
        harness.state_mut().approve_overwrite();
        assert!(
            harness.state().notice.contains("変わりました"),
            "{}",
            harness.state().notice
        );
        assert_eq!(std::fs::read(&path).unwrap(), b"rewritten meanwhile");

        // 確認した file は置き換える（負の対照）。
        let _ = harness
            .state_mut()
            .write_new_target(SaveTarget::SaveAs, path.clone(), None);
        harness.run();
        harness.get_by_label("置き換える").click();
        harness.run();
        assert_ne!(std::fs::read(&path).unwrap(), b"rewritten meanwhile");
        assert_eq!(harness.state().session.path(), Some(path.as_path()));
        drop(harness);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn closing_a_dirty_project_asks_first_and_a_clean_one_closes() {
        let mut harness = harness(MvmApp::new());
        harness.state_mut().session.mark_changed();
        let ctx = harness.ctx.clone();
        harness.state_mut().handle_close_request(&ctx);
        harness.run();
        harness.get_by_label("未保存の変更");
        harness.get_by_label("プロジェクトへの変更を保存しますか？");
        harness.get_by_label("保存しない");
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

    #[test]
    fn the_timeline_keeps_the_height_it_was_dragged_to() {
        // 境界を上へドラッグして広げた timeline が、離した次の frame で中身の高さへ戻らない。
        let mut harness = harness(MvmApp::new());
        let top = |harness: &Harness<'static, MvmApp>| harness.get_by_label("+V").rect().min.y;
        let before = top(&harness);
        // timeline の panel の上端は、時刻の行の上の余白（6 px）の上。
        let edge = harness
            .query_all_by_label_contains("00:00:00:00")
            .next()
            .expect("時刻の表示")
            .rect()
            .min
            .y
            - 6.0;
        press_and_release(
            &mut harness,
            egui::pos2(600.0, edge),
            egui::pos2(600.0, edge - 150.0),
        );
        harness.run_steps(5);
        let after = top(&harness);
        assert!(
            (before - after - 150.0).abs() < 4.0,
            "timeline の上端が {before} から {after} へ動きました（150 上がるはず）"
        );
    }

    #[test]
    fn many_tracks_scroll_inside_the_timeline_instead_of_growing_it() {
        // track 名の列は全 track の分の部品を置く。それが panel の大きさに数えられると、track が多いほど
        // timeline が伸びて preview を押しつぶす。
        let top = |harness: &Harness<'static, MvmApp>| harness.get_by_label("+V").rect().min.y;
        let mut few = harness(MvmApp::new());
        few.run();
        let mut project = Project::default();
        let track = project.video_tracks[0].clone();
        project.video_tracks = vec![track; 12];
        let mut many = harness(MvmApp::new());
        many.state_mut().session = Session::from_project(project);
        many.run_steps(5);
        assert_eq!(
            top(&many),
            top(&few),
            "track の数で timeline の高さが変わりました"
        );
        // 下の track は見えていないので、スクロールしないと出ない。
        assert!(many.query_by_label("+A").is_none());
    }

    #[test]
    fn dragging_across_the_eyes_hides_every_passed_track_in_one_undo() {
        // 押した V2 の表示を反転し、ドラッグで通った V1 も同じにする（Photoshop の目玉と同じ）。
        let mut harness = harness(MvmApp::new());
        let from = harness.get_by_label("V2 表示").rect().center();
        let to = harness.get_by_label("V1 表示").rect().center();
        press_and_release(&mut harness, from, to);
        let muted: Vec<bool> = harness
            .state()
            .session
            .project()
            .video_tracks
            .iter()
            .map(|track| track.muted)
            .collect();
        assert_eq!(muted, [true, true]);
        harness.get_by_label("V1 非表示");
        harness.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Z);
        harness.run();
        let muted: Vec<bool> = harness
            .state()
            .session
            .project()
            .video_tracks
            .iter()
            .map(|track| track.muted)
            .collect();
        assert_eq!(muted, [false, false], "1 回の undo で戻りません");
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
        // 150% は 1 frame 1.5 px。80 px 右は 53 frame（切り捨て）。
        press_and_release(&mut harness, at, at + egui::vec2(80.0, 0.0));
        let session = &harness.state().session;
        assert_eq!(
            session.project().timeline_clips[0].timeline_start_frame,
            53,
            "{:?}",
            session.alert()
        );
    }

    #[test]
    fn tool_button_track_visibility_and_undo_are_reachable() {
        let mut harness = harness(MvmApp::new());
        // ツールはアイコンだけのボタンで、名前で引ける。押したツールに切り替わる。
        assert_eq!(harness.state().session.tool(), TimelineTool::Select);
        harness.get_by_label("レーザー").click();
        harness.run();
        assert_eq!(harness.state().session.tool(), TimelineTool::Razor);
        harness.get_by_label_contains("レーザー: ");

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

    #[test]
    fn the_subtitle_panel_adds_a_cue_that_shows_on_the_s1_lane() {
        let mut harness = harness(MvmApp::new());
        harness.get_by_label("字幕").click();
        harness.run();
        harness.get_by_label("字幕がありません");
        harness.get_by_label("自動文字起こし");
        harness.get_by_label("SRT読込");
        // 本文を入れて「追加」すると、一覧と S1 レーンに出る。
        harness.state_mut().subtitle_panel_draft("最初の字幕");
        harness.run();
        harness.get_by_label("追加").click();
        // 字幕を足すと preview の描き直しが非同期に届いて画面を起こす。その到着は待たないので、決まった回数
        // だけ進める（run は repaint が止むまで待ち、届く時機しだいで上限を超える）。
        harness.run_steps(3);
        let cues = harness
            .state()
            .session
            .project()
            .subtitles
            .as_ref()
            .map(|track| track.cues.len());
        assert_eq!(cues, Some(1), "{:?}", harness.state().session.alert());
        harness.get_by_label("S1 表示");
        assert!(
            harness.query_all_by_label("最初の字幕").count() >= 1,
            "S1 レーンに字幕が出ません"
        );
    }

    #[test]
    fn the_mixer_tab_shows_one_strip_per_audio_track_and_the_master() {
        let mut harness = harness(MvmApp::new());
        harness.get_by_label("オーディオミキサー").click();
        harness.run();
        // A1 はタイムラインの行名にも出るので、ミキサー側の名前を 1 つ以上あることで数える。
        assert!(harness.query_all_by_label("A1").count() >= 2);
        harness.get_by_label("マスター");
        harness.get_by_label("再生の出口");
        // M / S は timeline の行と、ミキサーの帯の両方にある。
        assert_eq!(harness.query_all_by_label("M").count(), 2);
        assert_eq!(harness.query_all_by_label("S").count(), 2);
    }

    #[test]
    fn the_insert_menu_places_a_math_clip_and_the_inspector_edits_it() {
        let mut harness = harness(MvmApp::new());
        harness.get_by_label("ファイル(F)").click();
        harness.run();
        harness.get_by_label("数式 clip を追加").click();
        // 数式を置くと描画の service が動き、確認・描画のたびに画面を起こす。描画の完了は待たないので、
        // 決まった回数だけ進める（run は repaint が止むまで待つ）。
        harness.run_steps(3);
        let session = &harness.state().session;
        let clip = &session.project().timeline_clips[0];
        assert_eq!(clip.kind, TimelineClipKind::Math);
        assert_eq!(session.selected_clip(), Some(clip.id.as_str()));
        harness.get_by_label("式（LaTeX）");
        harness.get_by_label("先頭で書いていく（Write）");
    }

    #[test]
    fn the_audio_menu_opens_the_automatic_adjustment_dialog() {
        let mut harness = harness(MvmApp::new());
        harness.get_by_label("編集(E)").click();
        harness.run();
        harness.get_by_label("自動音量調整…").click();
        harness.run();
        harness.get_by_label("BGM の自動音量調整");
        harness.get_by_label("ラウドネス正規化");
        harness.get_by_label("解析・カーブ生成");
        harness.get_by_label("音声を試聴");
    }

    fn video_clip(name: &str, start: i64) -> TimelineClip {
        TimelineClip {
            kind: TimelineClipKind::Video,
            media_path: format!("{name}.mp4").into(),
            name: name.to_owned(),
            id: format!("id-{name}"),
            media_item_id: "item".to_owned(),
            source_fps: Rational::new(60, 1),
            source_frame_count: 120,
            source_out_frame: 120,
            timeline_start_frame: start,
            ..TimelineClip::default()
        }
    }

    #[test]
    fn the_project_panel_creates_a_folder_and_starts_renaming_it() {
        let mut harness = harness(on_tab(LeftTab::Project));
        harness.get_by_label("読み込み…");
        harness.get_by_label("新規フォルダ").click();
        harness.run();
        let folders = &harness.state().session.project().media_folders;
        assert_eq!(folders.len(), 1);
        assert_eq!(folders[0].name, "フォルダ 01");
        // 作った直後は名前の入力欄になる。
        let field = harness
            .query_all_by_value("フォルダ 01")
            .next()
            .expect("名前の入力欄");
        field.type_text("素材");
        harness.run();
        harness.key_press(egui::Key::Enter);
        harness.run();
        let name = &harness.state().session.project().media_folders[0].name;
        assert_eq!(name, "フォルダ 01素材");
        harness.get_by_label("フォルダ 01素材");
    }

    #[test]
    fn the_track_buttons_add_video_and_audio_tracks() {
        let mut harness = harness(MvmApp::new());
        harness.get_by_label("+V").click();
        harness.run();
        harness.get_by_label("+A").click();
        harness.run();
        let project = harness.state().session.project();
        assert_eq!(project.video_tracks.len(), 3);
        assert_eq!(project.audio_tracks.len(), 2);
        harness.get_by_label("V3");
        harness.get_by_label("A2");
    }

    #[test]
    fn the_settings_dialog_changes_the_frame_rate_in_one_undo() {
        let mut harness = harness(MvmApp::new());
        harness.get_by_label("プロジェクト(P)").click();
        harness.run();
        harness.get_by_label("プロジェクト設定").click();
        harness.run();
        harness.get_by_label("プロジェクト設定");
        harness.get_by_value("60 fps").click();
        harness.run();
        harness.get_by_label("30 fps").click();
        harness.run();
        harness.get_by_label("適用").click();
        harness.run();
        assert!(harness.query_by_label("プロジェクト設定").is_none());
        let fps = harness.state().session.project().timeline_fps;
        assert_eq!(fps, Rational::new(30, 1));
        harness.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Z);
        harness.run();
        assert_eq!(
            harness.state().session.project().timeline_fps,
            Rational::new(60, 1)
        );
    }

    #[test]
    fn ctrl_r_opens_the_speed_dialog_only_for_a_clip_with_a_source() {
        let mut harness = harness_with(vec![video_clip("映像", 0), text_clip("文字", 200)]);
        harness
            .state_mut()
            .session
            .select_only("id-映像".to_owned());
        harness.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::R);
        harness.run();
        harness.get_by_label("速度・デュレーション");
        harness.get_by_label("オーディオのピッチを維持");
        harness.get_by_label("キャンセル").click();
        harness.run();
        assert!(harness.query_by_label("速度・デュレーション").is_none());

        harness
            .state_mut()
            .session
            .select_only("id-文字".to_owned());
        harness.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::R);
        harness.run();
        assert!(harness.query_by_label("速度・デュレーション").is_none());
        assert!(harness.state().notice.contains("速度を変えられません"));
    }

    #[test]
    fn the_clip_menu_offers_the_edits_of_the_clip() {
        let mut harness = harness_with(vec![video_clip("映像", 0)]);
        harness.state_mut().session.seek(30);
        harness.run();
        harness
            .get_by(|node| {
                node.role() == egui::accesskit::Role::Button
                    && node.label().as_deref() == Some("映像")
            })
            .click_secondary();
        harness.run();
        harness.get_by_label("速度・デュレーション…");
        harness.get_by_label("プロジェクト設定をこの素材に合わせる");
        harness.get_by_label("リップル削除（全トラック）");
        assert!(harness.query_by_label("リンクを解除").is_none());
        harness.get_by_label("フレーム保持を挿入").click();
        harness.run();
        let holds = harness
            .state()
            .session
            .project()
            .timeline_clips
            .iter()
            .filter(|clip| clip.frame_hold.is_some())
            .count();
        assert_eq!(holds, 1);
    }

    #[test]
    fn the_inspector_animates_a_motion_channel_at_the_playhead() {
        let mut harness = harness_with(vec![video_clip("映像", 0)]);
        harness
            .state_mut()
            .session
            .select_only("id-映像".to_owned());
        harness.state_mut().session.seek(30);
        harness.run();
        harness.get_by_label("位置 X");
        harness.get_by_label("縦横比を固定");
        assert!(harness.query_by_label("音量").is_none());
        harness.get_by_label("位置 X のアニメーション").click();
        harness.run();
        assert_eq!(harness.state().notice, "");
        let keys = &harness.state().session.project().timeline_clips[0]
            .effects
            .position_x_keys;
        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0].frame, 30);
        // キーのある channel は前後の移動とキーの切り替えを出す。
        harness.get_by_label("位置 X のキー").click();
        harness.run();
        let effects = &harness.state().session.project().timeline_clips[0].effects;
        assert!(effects.position_x_keys.is_empty());
    }

    #[test]
    fn the_inspector_edits_a_text_clip_in_one_commit() {
        let mut heading = text_clip("見出し", 0);
        heading.text.content = "見出し".to_owned();
        // 書式の欄が全て見えるよう、縦に長い窓で開く。
        let mut harness = harness_sized(MvmApp::new(), egui::vec2(1200.0, 1600.0));
        harness.state_mut().session = Session::from_project(Project {
            timeline_clips: vec![heading],
            ..Project::default()
        });
        harness
            .state_mut()
            .session
            .select_only("id-見出し".to_owned());
        harness.run();
        // timeline の clip 名も同じ文字を持つので、入力欄を役割で選ぶ。
        let body = |node: &egui_kittest::kittest::AccessKitNode<'_>| {
            node.role() == egui::accesskit::Role::MultilineTextInput
                && node.value().as_deref() == Some("見出し")
        };
        harness.get_by(body).focus();
        harness.run();
        harness.key_press(egui::Key::End);
        harness.get_by(body).type_text("を直す");
        harness.run();
        assert_eq!(
            harness.state().session.project().timeline_clips[0]
                .text
                .content,
            "見出し"
        );
        harness.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Enter);
        harness.run();
        let clip = &harness.state().session.project().timeline_clips[0];
        assert_eq!(clip.text.content, "見出しを直す");
        assert_eq!(clip.name, "見出しを直す");
    }

    #[test]
    fn an_audio_clip_shows_volume_and_fades_but_no_motion() {
        let mut audio = video_clip("音", 0);
        audio.kind = TimelineClipKind::Audio;
        audio.track = TrackRef::audio(0);
        let mut harness = harness_with(vec![audio]);
        harness.state_mut().session.select_only("id-音".to_owned());
        harness.run();
        harness.get_by_label("音量");
        harness.get_by_label("ダッキング");
        harness.get_by_label("フェードイン (素材f)");
        assert!(harness.query_by_label("位置 X").is_none());
    }

    #[test]
    fn j_and_k_run_and_stop_a_reverse_shuttle() {
        // 試験の時計はすぐ進むので、先頭に着かない長さの clip の終わり近くから戻す。
        let mut clip = text_clip("文字", 0);
        clip.text.content = "文字".to_owned();
        clip.source_frame_count = 100_000;
        clip.source_out_frame = 100_000;
        let mut harness = harness_with(vec![clip]);
        harness.state_mut().session.seek(90_000);
        harness.key_press(egui::Key::J);
        harness.run_steps(2);
        assert!(harness.state().session.playhead_frame() < 90_000);
        assert_eq!(harness.state().session.shuttle_rate(), -1);
        harness.get_by_label("シャトル -1 倍速（音声なし）");
        harness.key_press(egui::Key::K);
        harness.run_steps(2);
        assert_eq!(harness.state().session.shuttle_rate(), 0);
        assert!(
            harness
                .query_by_label("シャトル -1 倍速（音声なし）")
                .is_none()
        );
    }

    #[test]
    fn the_playback_menu_lists_the_transport_with_its_keys() {
        let mut harness = harness(MvmApp::new());
        harness
            .query_all_by_label("再生(L)")
            .next()
            .expect("再生のメニュー")
            .click();
        harness.run();
        harness.get_by_label_contains("左へシャトル");
        harness.get_by_label_contains("次の編集点へ");
        harness.get_by_label_contains("マーカーを追加");
    }

    #[test]
    fn an_inspector_edit_lands_on_the_frame_where_it_started() {
        let mut clip = video_clip("映像", 0);
        clip.effects.position_x_keys = vec![
            mvm_project::clip_effects::ClipKeyframe::new(0, 0.0),
            mvm_project::clip_effects::ClipKeyframe::new(100, 50.0),
        ];
        let mut harness = harness_with(vec![clip]);
        harness
            .state_mut()
            .session
            .select_only("id-映像".to_owned());
        harness.state_mut().session.seek(30);
        harness.run();
        // 位置 X は 30 frame で 15%。
        let field = |node: &egui_kittest::kittest::AccessKitNode<'_>| {
            node.role() == egui::accesskit::Role::SpinButton
                && node.value().is_some_and(|value| value.starts_with("15"))
        };
        harness.get_by(field).click();
        harness.run();
        harness.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
        harness.get_by(|node| node.is_focused()).type_text("12");
        harness.run();
        // 入力の途中で再生ヘッドが動いても、下書きは消えず、始めた 30 frame へ書く。
        harness.state_mut().session.seek(80);
        harness.run();
        harness.key_press(egui::Key::Enter);
        harness.run();
        let keys: Vec<_> = harness.state().session.project().timeline_clips[0]
            .effects
            .position_x_keys
            .iter()
            .map(|key| (key.frame, key.value))
            .collect();
        assert_eq!(keys, [(0, 0.0), (30, 12.0), (100, 50.0)]);
    }

    /// 試験で OS から落とすファイル。中身は読まない。
    #[derive(Debug)]
    struct DroppedPath(PathBuf);

    impl egui::DroppedFile for DroppedPath {
        fn path(&self) -> &std::path::Path {
            &self.0
        }

        fn bytes(&self) -> Result<Vec<u8>, String> {
            Err("試験では読まない".to_owned())
        }
    }

    #[test]
    fn a_file_dropped_from_the_os_goes_to_the_panel_not_the_timeline() {
        let mut harness = harness(MvmApp::new());
        // timeline の行の上に pointer を置いてから落としても、timeline には置かない。
        let row = harness.get_by_label("V1").rect().center() + egui::vec2(400.0, 0.0);
        harness.hover_at(row);
        harness.run();
        harness
            .input_mut()
            .dropped_files
            .push(std::sync::Arc::new(DroppedPath(PathBuf::from(
                "Z:/無い/素材.mp4",
            ))));
        harness.run();
        let state = harness.state();
        assert!(state.session.project().timeline_clips.is_empty());
        // 読み込もうとして、読めなかったことを知らせる。
        assert!(state.notice.contains("素材.mp4"), "{}", state.notice);
        assert_eq!(state.left_tab, LeftTab::Project);
    }

    fn animated_clip(name: &str, start: i64) -> TimelineClip {
        let mut clip = video_clip(name, start);
        clip.effects.position_x_keys = vec![
            mvm_project::clip_effects::ClipKeyframe::new(0, 0.0),
            mvm_project::clip_effects::ClipKeyframe::new(100, 50.0),
        ];
        clip
    }

    /// 値が `prefix` で始まる、inspector の数値欄。
    fn spin_starting_with(
        prefix: &str,
    ) -> impl Fn(&egui_kittest::kittest::AccessKitNode<'_>) -> bool {
        move |node| {
            node.role() == egui::accesskit::Role::SpinButton
                && node.value().is_some_and(|value| value.starts_with(prefix))
        }
    }

    #[test]
    fn focusing_a_field_and_moving_the_playhead_adds_no_key() {
        let mut harness = harness_with(vec![animated_clip("映像", 0)]);
        harness
            .state_mut()
            .session
            .select_only("id-映像".to_owned());
        harness.state_mut().session.seek(30);
        harness.run();
        // 30 frame の位置 X（15%）の欄に入り、値を変えずに再生ヘッドを動かしてから抜ける。
        harness.get_by(spin_starting_with("15")).click();
        harness.run();
        harness.state_mut().session.seek(80);
        harness.run();
        harness.key_press(egui::Key::Enter);
        harness.run();
        let session = &harness.state().session;
        assert_eq!(
            session.project().timeline_clips[0]
                .effects
                .position_x_keys
                .len(),
            2,
            "値を変えていないのにキーが増えました"
        );
        assert!(!session.can_undo());
    }

    #[test]
    fn an_unfinished_edit_is_dropped_when_another_clip_is_selected() {
        let mut harness = harness_with(vec![animated_clip("一", 0), animated_clip("二", 200)]);
        harness.state_mut().session.select_only("id-一".to_owned());
        harness.state_mut().session.seek(30);
        harness.run();
        harness.get_by(spin_starting_with("15")).click();
        harness.run();
        harness.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
        harness.get_by(|node| node.is_focused()).type_text("40");
        harness.run();
        // 確定せずに別の clip を選び、元の clip へ戻る。
        harness.state_mut().session.select_only("id-二".to_owned());
        harness.run();
        harness.state_mut().session.select_only("id-一".to_owned());
        harness.run();
        // 戻っても未確定の 40 は出ず、Project の 15% を見せる。
        assert!(harness.query_by(spin_starting_with("40")).is_none());
        harness.get_by(spin_starting_with("15"));
        assert!(!harness.state().session.can_undo());
    }

    #[test]
    fn a_folder_is_dragged_into_another_folder_and_back_to_the_root() {
        let folder = |id: &str, name: &str| MediaFolder {
            id: id.to_owned(),
            name: name.to_owned(),
            parent_id: String::new(),
        };
        let project = Project {
            media_folders: vec![folder("a", "フォルダA"), folder("b", "フォルダB")],
            ..Project::default()
        };
        let mut harness = harness(on_tab(LeftTab::Project));
        harness.state_mut().session = Session::from_project(project);
        harness.run();
        let from = harness.get_by_label("フォルダA").rect().center();
        let to = harness.get_by_label("フォルダB").rect().center();
        press_and_release(&mut harness, from, to);
        let parent = |harness: &Harness<'static, MvmApp>| {
            harness.state().session.project().media_folders[0]
                .parent_id
                .clone()
        };
        assert_eq!(parent(&harness), "b", "フォルダをフォルダへ移せません");
        // 開いたフォルダB の中の A を、パネル下の空白（root）へ戻す。
        let from = harness.get_by_label("フォルダA").rect().center();
        let empty = harness.get_by_label("フォルダB").rect().center() + egui::vec2(0.0, 120.0);
        press_and_release(&mut harness, from, empty);
        assert_eq!(parent(&harness), "", "root へ戻せません");
    }
}
