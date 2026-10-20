//! 開いている Project と、playhead・zoom・preview の再生。

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::SystemTime;

use mvm_core::timecode::format_timecode;
use mvm_platform::{ProjectLock, atomic_write, probe_file};
use mvm_preview::{PreviewEngine, PreviewImage, plan_frame};
use mvm_project::Project;
use mvm_project::json::{parse_project_json, serialize_project_json};
use mvm_project::model::{MediaItem, TimelineClip};
use mvm_project::recovery::{
    RecoveryDisposition, classify_recovery, parse_recovery_header, recovery_belongs_to,
    recovery_project,
};
use mvm_project::timeline::TimelineTool;

use crate::autosave::AutosaveSchedule;
use crate::background::BackgroundWork;
use crate::error::AppError;
use crate::files::{identity_key, lock_path, recovery_path, sha256_hex, utc_timestamp};
use crate::history::History;
use crate::recovery_pump::{PumpEvent, RecoveryPump, RecoveryWrite};
use crate::timeline_view::{DEFAULT_PIXELS_PER_FRAME, clamp_pixels_per_frame};

static SESSION_SERIAL: AtomicU64 = AtomicU64::new(1);

/// 外部変更を調べる間隔。毎 frame file を開かない。
const EXTERNAL_POLL_MS: u64 = 1_000;

/// 編集 session。Project の正本はここだけが持ち、UI は借りて表示する。
pub struct Session {
    pub(crate) project: Project,
    path: Option<PathBuf>,
    dirty: bool,
    pub(crate) playhead_frame: i64,
    pixels_per_frame: f32,
    scrubbing: bool,
    preview_at: Option<i64>,
    preview_generation: u64,
    preview_image: Option<Arc<PreviewImage>>,
    preview_error: Option<String>,
    skipped_note: Option<String>,
    engine: Option<PreviewEngine>,
    playing: bool,
    command_generation: u64,
    edit_epoch: u64,
    sent_epoch: u64,
    pub(crate) preview_wake: Option<Arc<dyn Fn() + Send + Sync>>,
    /// 走っている書き出し。drop すると取り消して worker を待つ。
    pub(crate) export: Option<mvm_export::ExportJob>,
    pub(crate) history: History,
    pub(crate) saved: Project,
    pub(crate) tool: TimelineTool,
    pub(crate) selection: Vec<String>,
    pub(crate) selected_edge: Option<(String, mvm_project::timeline::TrimEdge)>,
    pub(crate) selected_marker: Option<i64>,
    pub(crate) clipboard: Option<Clipboard>,
    pub(crate) autosave: AutosaveSchedule,
    pub(crate) autosave_note: bool,
    pub(crate) lock: Option<ProjectLock>,
    pub(crate) canonical_sha256: String,
    pub(crate) file_stamp: Option<(u64, i64)>,
    pub(crate) external_change: bool,
    pub(crate) recovery: Option<RecoveryOffer>,
    pub(crate) recovery_corrupt: bool,
    pub(crate) recovery_foreign: bool,
    pub(crate) pump: RecoveryPump,
    /// この session が今の path へ recovery を書いた（または書く job を積んだ）か。
    wrote_recovery: bool,
    next_external_poll_ms: u64,
    pub(crate) session_id: String,
    pub(crate) edit_revision: u64,
    pub(crate) id_serial: u64,
    pub(crate) alert: String,
    pub(crate) background: BackgroundWork,
    /// 再生の出口の音量。engine を作り直しても引き継ぐ。
    pub(crate) monitor_gain: f32,
    /// 選択中の字幕。clip の選択とは同時に持たない。
    pub(crate) subtitle_selection: Vec<String>,
    /// 最後にコピーしたのが字幕なら、その字幕（clip の clipboard は空にする）。
    pub(crate) subtitle_clipboard: Option<Vec<mvm_project::SubtitleCue>>,
    /// 数式の描画。数式の clip が現れたときに起こす。
    pub(crate) math: Option<crate::math::MathState>,
    /// Project を読み込む・保存先を変えるたびに増える。背景の作業の結果が同じ Project のものか確かめる。
    pub(crate) load_serial: u64,
    /// 端末の設定を読めないので数式の描画を始められない理由。「再試行」で読み直す。
    pub(crate) math_blocked: Option<String>,
    /// timeline に描く音声波形。素材 file ごとに背景で読む。
    pub(crate) waveforms: crate::waveforms::Waveforms,
    /// J / L の倍速・逆方向の送り。1 倍の正方向は通常の再生なので持たない。
    pub(crate) shuttle: Option<crate::shuttle::ShuttleState>,
}

#[derive(Clone)]
pub(crate) struct Clipboard {
    pub clips: Vec<TimelineClip>,
    pub media: Vec<MediaItem>,
    pub fps: mvm_core::Rational,
}

pub(crate) struct RecoveryOffer {
    project: Project,
    disposition: RecoveryDisposition,
}

/// 開いたときに見つかった自動復旧データ。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecoveryPrompt {
    Restorable,
    CanonicalChanged,
    Corrupt,
    Foreign,
}

impl Session {
    /// 空の新規 Project。まだ file には書いていない。
    pub fn new() -> Self {
        Self::from_project(Project::default())
    }

    /// メモリ上の Project を、未保存の session として開く。
    pub fn from_project(project: Project) -> Self {
        let saved = project.clone();
        Self {
            project,
            path: None,
            dirty: false,
            playhead_frame: 0,
            pixels_per_frame: DEFAULT_PIXELS_PER_FRAME,
            scrubbing: false,
            preview_at: None,
            preview_generation: 0,
            preview_image: None,
            preview_error: None,
            skipped_note: None,
            engine: None,
            playing: false,
            command_generation: 0,
            edit_epoch: 1,
            sent_epoch: 0,
            preview_wake: None,
            export: None,
            history: History::default(),
            saved,
            tool: TimelineTool::Select,
            selection: Vec::new(),
            selected_edge: None,
            selected_marker: None,
            clipboard: None,
            autosave: AutosaveSchedule::new(),
            autosave_note: false,
            lock: None,
            canonical_sha256: String::new(),
            file_stamp: None,
            external_change: false,
            recovery: None,
            recovery_corrupt: false,
            recovery_foreign: false,
            pump: RecoveryPump::new(),
            wrote_recovery: false,
            next_external_poll_ms: 0,
            session_id: new_session_id(),
            edit_revision: 1,
            id_serial: 1,
            alert: String::new(),
            background: BackgroundWork::default(),
            monitor_gain: 1.0,
            subtitle_selection: Vec::new(),
            subtitle_clipboard: None,
            math: None,
            load_serial: 1,
            math_blocked: None,
            waveforms: crate::waveforms::Waveforms::default(),
            shuttle: None,
        }
    }

    /// 画像が届いたときに UI を起こす。egui の `request_repaint` を渡す。
    pub fn set_preview_wake(&mut self, wake: Arc<dyn Fn() + Send + Sync>) {
        if let Some(engine) = &self.engine {
            engine.set_wake(Arc::clone(&wake));
        }
        self.preview_wake = Some(wake);
    }

    pub fn project(&self) -> &Project {
        &self.project
    }

    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    pub fn dirty(&self) -> bool {
        self.dirty
    }

    pub fn playhead_frame(&self) -> i64 {
        self.playhead_frame
    }

    pub fn pixels_per_frame(&self) -> f32 {
        self.pixels_per_frame
    }

    pub fn selected_clip(&self) -> Option<&str> {
        self.selection.first().map(String::as_str)
    }

    pub fn tool(&self) -> TimelineTool {
        self.tool
    }

    pub fn set_tool(&mut self, tool: TimelineTool) {
        self.tool = tool;
    }

    pub fn selection(&self) -> &[String] {
        &self.selection
    }

    pub fn alert(&self) -> Option<&str> {
        if self.alert.is_empty() {
            None
        } else {
            Some(&self.alert)
        }
    }

    pub fn clear_alert(&mut self) {
        self.alert.clear();
    }

    pub fn external_change(&self) -> bool {
        self.external_change
    }

    pub fn recovery_prompt(&self) -> Option<RecoveryPrompt> {
        if self.recovery_foreign {
            return Some(RecoveryPrompt::Foreign);
        }
        if self.recovery_corrupt {
            return Some(RecoveryPrompt::Corrupt);
        }
        self.recovery.as_ref().map(|offer| match offer.disposition {
            RecoveryDisposition::Restorable => RecoveryPrompt::Restorable,
            RecoveryDisposition::CanonicalChanged => RecoveryPrompt::CanonicalChanged,
            RecoveryDisposition::Stale => RecoveryPrompt::Restorable,
        })
    }

    /// Project を変えるたびに進む番号。表示側が手元の下書きを読み直す合図に使う。
    pub fn edit_revision(&self) -> u64 {
        self.edit_revision
    }

    pub fn can_undo(&self) -> bool {
        self.history.can_undo()
    }

    pub fn can_redo(&self) -> bool {
        self.history.can_redo()
    }

    pub fn preview_image(&self) -> Option<&PreviewImage> {
        self.preview_image.as_deref()
    }

    pub fn preview_error(&self) -> Option<&str> {
        self.preview_error.as_deref()
    }

    pub fn skipped_note(&self) -> Option<&str> {
        self.skipped_note.as_deref()
    }

    pub fn preview_generation(&self) -> u64 {
        self.preview_generation
    }

    pub fn is_playing(&self) -> bool {
        self.playing
    }

    /// frame と秒を併記した playhead の表示。
    pub fn playhead_label(&self) -> String {
        let fps = self.project.timeline_fps;
        let timecode = format_timecode(self.playhead_frame, fps.num, fps.den)
            .unwrap_or_else(|| "--:--:--:--".to_owned());
        format!("{timecode}  ({} frame)", self.playhead_frame)
    }

    pub fn zoom_percent(&self) -> i32 {
        (self.pixels_per_frame / DEFAULT_PIXELS_PER_FRAME * 100.0).round() as i32
    }

    /// 編集 command が Project を変えたあとに呼ぶ。保存するまで dirty のままにする。
    pub fn mark_changed(&mut self) {
        self.dirty = true;
        self.autosave_note = true;
        self.note_project_edit();
    }

    pub fn select_clip(&mut self, id: Option<String>) {
        self.selection = id.into_iter().collect();
        self.selected_edge = None;
        self.selected_marker = None;
    }

    pub fn set_pixels_per_frame(&mut self, value: f32) {
        self.pixels_per_frame = clamp_pixels_per_frame(value);
    }

    pub fn zoom_by(&mut self, factor: f32) {
        if factor.is_finite() && factor > 0.0 {
            self.set_pixels_per_frame(self.pixels_per_frame * factor);
        }
    }

    /// ドラッグ中は playhead だけ動かし、離したときに preview を描く。
    pub fn set_scrubbing(&mut self, scrubbing: bool) {
        if scrubbing && self.playing {
            self.pause_playback();
        }
        self.scrubbing = scrubbing;
        if !scrubbing {
            self.preview_at = None;
        }
    }

    pub fn seek(&mut self, frame: i64) {
        let frame = frame.max(0);
        if frame == self.playhead_frame && self.preview_at == Some(frame) {
            return;
        }
        self.playhead_frame = frame;
        if self.playing {
            self.preview_at = Some(frame);
            self.command_generation = self.command_generation.saturating_add(1);
            let generation = self.command_generation;
            if let Err(message) = self.send_seek(frame, generation) {
                self.playing = false;
                self.fail_preview(frame, message);
            }
        } else {
            self.preview_at = None;
        }
    }

    pub fn toggle_playback(&mut self) {
        if self.playing {
            self.pause_playback();
        } else {
            self.start_playback();
        }
    }

    /// 世代を進めて止める。止める前に worker が描いた先の frame は、画面の playhead と
    /// 食い違うので受け取らない。
    pub fn pause_playback(&mut self) {
        self.playing = false;
        self.command_generation = self.command_generation.saturating_add(1);
        let generation = self.command_generation;
        if let Some(engine) = &self.engine
            && let Err(error) = engine.pause(generation)
        {
            let frame = self.playhead_frame;
            self.fail_preview(frame, error.to_string());
        }
    }

    /// 既定の Project を `path` へ書き、それを現在の session にする。失敗しても今の session は残す。
    pub fn create_project(&mut self, path: &Path) -> Result<(), AppError> {
        let path = absolute_path(path)?;
        let project = Project::default();
        let bytes = project_bytes(&project, &path)?;
        let lock = self.acquire_lock(&path)?;
        if let Err(error) = atomic_write(&path, &bytes) {
            self.restore_unused_lock(lock);
            return Err(AppError::new(error.to_string()));
        }
        // 前の Project の文字起こし・自動音量・Manim の作業は取り消して捨てる（別の Project へ適用しない）。
        self.background = BackgroundWork::default();
        self.install(project, path, false, lock, sha256_hex(&bytes));
        Ok(())
    }

    /// `path` を読む。失敗しても今の session は残す。
    pub fn open_project(&mut self, path: &Path) -> Result<(), AppError> {
        let path = absolute_path(path)?;
        let bytes = std::fs::read(&path).map_err(|error| {
            AppError::new(format!(
                "Project を読めません ({}): {error}",
                path.display()
            ))
        })?;
        let text = String::from_utf8(bytes.clone()).map_err(|_| {
            AppError::new(format!(
                "Project が UTF-8 ではありません ({})",
                path.display()
            ))
        })?;
        let project =
            parse_project_json(&text, &path).map_err(|error| AppError::new(error.to_string()))?;
        let lock = self.acquire_lock(&path)?;
        self.background = BackgroundWork::default();
        self.install(project, path, false, lock, sha256_hex(&bytes));
        Ok(())
    }

    /// 保存先があるときだけ書く。外部で変わっていたら書かず、今の session は残す。
    pub fn save_project(&mut self) -> Result<(), AppError> {
        self.save_project_forcing(false)
    }

    pub fn save_overwriting_external(&mut self) -> Result<(), AppError> {
        self.save_project_forcing(true)
    }

    pub fn save_project_as(&mut self, path: &Path) -> Result<(), AppError> {
        let path = absolute_path(path)?;
        let lock = self.acquire_lock(&path)?;
        let bytes = match project_bytes(&self.project, &path) {
            Ok(bytes) => bytes,
            Err(error) => {
                self.restore_unused_lock(lock);
                return Err(error);
            }
        };
        if let Err(error) = atomic_write(&path, &bytes) {
            self.restore_unused_lock(lock);
            return Err(AppError::new(error.to_string()));
        }
        self.install(self.project.clone(), path, false, lock, sha256_hex(&bytes));
        Ok(())
    }

    fn save_project_forcing(&mut self, force: bool) -> Result<(), AppError> {
        let Some(path) = self.path.clone() else {
            return Err(AppError::new("保存先がありません"));
        };
        if !force {
            self.confirm_canonical(&path)?;
        }
        let bytes = project_bytes(&self.project, &path)?;
        atomic_write(&path, &bytes).map_err(|error| AppError::new(error.to_string()))?;
        self.canonical_sha256 = sha256_hex(&bytes);
        self.file_stamp = stamp_of(&path);
        self.external_change = false;
        self.saved = self.project.clone();
        self.dirty = false;
        self.autosave.clear();
        self.autosave_note = false;
        self.delete_recovery(&path);
        Ok(())
    }

    /// worker が届けた frame を反映する。再生中か、まだ画像が無いときは再描画が要る。
    pub fn poll_preview(&mut self) -> bool {
        let generation = self.command_generation;
        let Some(engine) = &self.engine else {
            return false;
        };
        if let Some(delivery) = engine.poll()
            && delivery.generation == generation
        {
            if let Some(image) = delivery.image {
                let frame = image.frame;
                if self.playing {
                    self.playhead_frame = frame;
                    self.preview_at = Some(frame);
                }
                self.store_preview(frame, image, None);
            }
            if let Some(playing) = delivery.playing {
                self.playing = playing;
            }
            if let Some(message) = delivery.error {
                self.playing = false;
                self.fail_preview(self.playhead_frame, message);
            }
        }
        self.playing
            || (self.preview_error.is_none()
                && self.preview_at.is_some()
                && self.preview_image.as_ref().map(|image| image.frame) != self.preview_at)
    }

    /// playhead の frame を、まだ描いていなければ worker へ頼む。スクラブ中は何もしない。
    pub fn refresh_preview(&mut self) {
        if self.scrubbing || self.playing {
            return;
        }
        if self.preview_at == Some(self.playhead_frame) && self.sent_epoch == self.edit_epoch {
            return;
        }
        let frame = self.playhead_frame;
        self.note_skipped(frame);
        if let Err(message) = self.ensure_engine() {
            self.fail_preview(frame, message);
            return;
        }
        self.command_generation = self.command_generation.saturating_add(1);
        let generation = self.command_generation;
        if self.sent_epoch != self.edit_epoch
            && let Err(message) = self.send_project(generation)
        {
            self.fail_preview(frame, message);
            return;
        }
        if let Err(message) = self.send_seek(frame, generation) {
            self.fail_preview(frame, message);
            return;
        }
        self.preview_at = Some(frame);
    }

    pub fn start_playback(&mut self) {
        let frame = self.playhead_frame;
        self.note_skipped(frame);
        if let Err(message) = self.ensure_engine() {
            self.fail_preview(frame, message);
            return;
        }
        self.command_generation = self.command_generation.saturating_add(1);
        let generation = self.command_generation;
        if self.sent_epoch != self.edit_epoch
            && let Err(message) = self.send_project(generation)
        {
            self.fail_preview(frame, message);
            return;
        }
        let Some(engine) = &self.engine else {
            self.fail_preview(frame, "preview を用意できません".to_owned());
            return;
        };
        if let Err(error) = engine.play(frame, generation) {
            self.fail_preview(frame, error.to_string());
            return;
        }
        self.playing = true;
        self.preview_at = Some(frame);
    }

    /// 同じ frame でも preview を描き直す。
    pub fn invalidate_preview(&mut self) {
        self.preview_at = None;
    }

    fn note_skipped(&mut self, frame: i64) {
        let library = self.math_library();
        self.skipped_note = plan_frame(&self.project, frame, &library)
            .ok()
            .and_then(|plan| (plan.skipped_clips > 0).then(|| plan.notes.join(" / ")));
    }

    fn ensure_engine(&mut self) -> Result<(), String> {
        if self.engine.is_none() {
            let engine = PreviewEngine::start().map_err(|error| error.to_string())?;
            if let Some(wake) = &self.preview_wake {
                engine.set_wake(Arc::clone(wake));
            }
            engine
                .set_monitor_volume(self.monitor_gain)
                .map_err(|error| error.to_string())?;
            self.engine = Some(engine);
            self.sent_epoch = 0;
        }
        Ok(())
    }

    pub(crate) fn meter_source(&self) -> Option<&PreviewEngine> {
        self.engine.as_ref()
    }

    fn send_project(&mut self, generation: u64) -> Result<(), String> {
        let project = Arc::new(self.project.clone());
        let library = self.math_library();
        let Some(engine) = &self.engine else {
            return Err("preview を用意できません".to_owned());
        };
        engine
            .set_project(project, library, generation)
            .map_err(|error| error.to_string())?;
        self.sent_epoch = self.edit_epoch;
        Ok(())
    }

    fn send_seek(&mut self, frame: i64, generation: u64) -> Result<(), String> {
        let Some(engine) = &self.engine else {
            return Err("preview を用意できません".to_owned());
        };
        engine
            .seek(frame, generation)
            .map_err(|error| error.to_string())
    }

    fn store_preview(&mut self, frame: i64, image: PreviewImage, error: Option<String>) {
        self.preview_image = Some(Arc::new(image));
        self.preview_error = error;
        self.preview_at = Some(frame);
        self.preview_generation = self.preview_generation.saturating_add(1);
    }

    fn fail_preview(&mut self, frame: i64, message: String) {
        self.preview_image = None;
        self.preview_error = Some(message);
        self.preview_at = Some(frame);
        self.preview_generation = self.preview_generation.saturating_add(1);
    }

    fn install(
        &mut self,
        project: Project,
        path: PathBuf,
        dirty: bool,
        lock: ProjectLock,
        hash: String,
    ) {
        // 置き換える Project の未保存の変更は、ここで捨てる（保存済みなら削除は積んである）。
        // 同じ path を開き直すときに、捨てた変更を recovery として出さないよう、調べる前に終える。
        if self.wrote_recovery
            && let Some(old) = self.path.clone()
        {
            self.delete_recovery(&old);
        }
        let flushed = self.pump.flush();
        self.load_serial = self.load_serial.saturating_add(1);
        let recovery = inspect_recovery(&path, &project, &hash);
        self.lock = Some(lock);
        self.project = project.clone();
        self.saved = project;
        self.path = Some(path);
        self.dirty = dirty;
        self.history.clear();
        self.selection.clear();
        self.subtitle_selection.clear();
        self.selected_edge = None;
        self.selected_marker = None;
        self.canonical_sha256 = hash;
        self.file_stamp = self.path.as_deref().and_then(stamp_of);
        self.external_change = false;
        self.recovery = recovery.offer;
        self.recovery_corrupt = recovery.corrupt;
        self.recovery_foreign = recovery.foreign;
        self.autosave.clear();
        self.autosave_note = false;
        self.wrote_recovery = false;
        self.next_external_poll_ms = 0;
        self.alert = match flushed {
            Ok(()) => String::new(),
            Err(error) => format!("前の Project の自動復旧データを片付けられません: {error}"),
        };
        self.playhead_frame = 0;
        self.scrubbing = false;
        self.playing = false;
        self.engine = None;
        self.preview_image = None;
        self.preview_error = None;
        self.skipped_note = None;
        self.preview_at = None;
        self.sent_epoch = 0;
        self.edit_epoch = self.edit_epoch.saturating_add(1);
        self.edit_revision = self.edit_revision.saturating_add(1);
        self.preview_generation = self.preview_generation.saturating_add(1);
    }

    fn acquire_lock(&mut self, path: &Path) -> Result<ProjectLock, AppError> {
        if self
            .lock
            .as_ref()
            .is_some_and(|lock| lock.path() == lock_path(path))
        {
            return self
                .lock
                .take()
                .ok_or_else(|| AppError::new("Project lock を保持していません"));
        }
        mvm_platform::acquire_project_lock(path).map_err(|error| AppError::new(error.to_string()))
    }

    fn confirm_canonical(&mut self, path: &Path) -> Result<(), AppError> {
        if !path.exists() {
            if self.canonical_sha256.is_empty() {
                return Ok(());
            }
            self.external_change = true;
            return Err(AppError::new("Project file が外部で変更されています"));
        }
        let bytes = std::fs::read(path)
            .map_err(|error| AppError::new(format!("Project file を照合できません ({error})")))?;
        if sha256_hex(&bytes) != self.canonical_sha256 {
            self.external_change = true;
            return Err(AppError::new("Project file が外部で変更されています"));
        }
        Ok(())
    }

    pub(crate) fn note_project_edit(&mut self) {
        self.edit_epoch = self.edit_epoch.saturating_add(1);
        self.edit_revision = self.edit_revision.saturating_add(1);
        self.invalidate_preview();
        if self.playing {
            self.pause_playback();
        }
    }

    pub(crate) fn finish_edit(&mut self) {
        self.dirty = self.project != self.saved;
        if self.dirty {
            self.autosave_note = true;
        } else {
            self.autosave.clear();
            self.autosave_note = false;
            if let Some(path) = self.path.clone() {
                self.delete_recovery(&path);
            }
        }
        self.scrub_selection();
        self.note_project_edit();
    }

    fn scrub_selection(&mut self) {
        self.selection.retain(|id| self.project.clip(id).is_some());
        let cues = self.project.subtitles.as_ref();
        self.subtitle_selection
            .retain(|id| cues.is_some_and(|track| track.cues.iter().any(|cue| cue.id == *id)));
        if self
            .selected_edge
            .as_ref()
            .is_some_and(|(id, _)| self.project.clip(id).is_none())
        {
            self.selected_edge = None;
        }
        if self
            .selected_marker
            .is_some_and(|frame| !self.project.timeline_markers.contains(&frame))
        {
            self.selected_marker = None;
        }
    }

    pub fn poll_maintenance(&mut self, now_ms: u64) {
        if now_ms >= self.next_external_poll_ms {
            self.next_external_poll_ms = now_ms.saturating_add(EXTERNAL_POLL_MS);
            self.poll_external();
        }
        if let Some(event) = self.pump.poll() {
            self.finish_recovery(event, now_ms);
        }
        if self.autosave_note {
            self.autosave.note_edit(now_ms);
            self.autosave_note = false;
        }
        if self.dirty && self.autosave.due(now_ms) {
            if self.ready_to_autosave() {
                self.enqueue_recovery();
                self.autosave.mark_written();
            } else if self.path.is_some() && self.lock.is_some() && self.canonical_sha256.is_empty()
            {
                self.alert =
                    "自動復旧データを保存できません: Project file の照合値がありません".to_owned();
                self.autosave.note_failure(now_ms);
            }
        }
    }

    fn ready_to_autosave(&self) -> bool {
        self.path.is_some() && self.lock.is_some() && !self.canonical_sha256.is_empty()
    }

    fn enqueue_recovery(&mut self) {
        let Some(path) = self.path.clone() else {
            return;
        };
        self.wrote_recovery = true;
        self.pump.enqueue_write(RecoveryWrite {
            revision: self.edit_revision,
            project: Arc::new(self.project.clone()),
            recovery_path: recovery_path(&path),
            canonical_path: path,
            canonical_sha256: self.canonical_sha256.clone(),
            saved_at: utc_timestamp(SystemTime::now()),
            session_id: self.session_id.clone(),
        });
    }

    fn finish_recovery(&mut self, event: PumpEvent, now_ms: u64) {
        if event.write && event.revision != self.edit_revision {
            return;
        }
        if let Err(error) = event.result {
            self.alert = if event.write {
                format!("自動復旧データを保存できません: {error}")
            } else {
                error
            };
            if event.write {
                self.autosave.note_failure(now_ms);
            }
        }
    }

    fn restore_unused_lock(&mut self, lock: ProjectLock) {
        if self
            .path
            .as_deref()
            .is_some_and(|path| lock_path(path) == lock.path())
        {
            self.lock = Some(lock);
        }
    }

    /// 外部変更を一度見つけたら、読み直すか上書きするまで file を読み直さない。
    fn poll_external(&mut self) {
        if self.external_change {
            return;
        }
        let Some(path) = self.path.clone() else {
            return;
        };
        let probe = probe_file(&path);
        let stamp = (probe.status == mvm_platform::FileProbeStatus::File)
            .then_some((probe.size, probe.last_write_100ns));
        if stamp == self.file_stamp {
            return;
        }
        if stamp.is_none() && self.canonical_sha256.is_empty() {
            return;
        }
        match std::fs::read(&path) {
            Ok(bytes) if sha256_hex(&bytes) == self.canonical_sha256 => {
                self.file_stamp = stamp;
            }
            Ok(_) => self.external_change = true,
            Err(_) if !path.exists() && !self.canonical_sha256.is_empty() => {
                self.external_change = true;
            }
            Err(_) => {}
        }
    }

    pub fn restore_recovery(&mut self) -> Result<(), AppError> {
        let Some(offer) = self.recovery.take() else {
            return Err(AppError::new("復元する自動復旧データがありません"));
        };
        let before = self.project.clone();
        self.project = offer.project;
        self.history.push_undo(before);
        self.recovery_corrupt = false;
        self.recovery_foreign = false;
        self.finish_edit();
        Ok(())
    }

    pub fn dismiss_recovery(&mut self, delete_file: bool) -> Result<(), AppError> {
        if self.recovery_foreign {
            self.recovery_foreign = false;
            return Ok(());
        }
        self.recovery = None;
        self.recovery_corrupt = false;
        if delete_file && let Some(path) = self.path.clone() {
            self.delete_recovery(&path);
        }
        Ok(())
    }

    /// 保存せずに閉じる。この session が書いた recovery を消し、積んだ job を終えてから返す。
    pub fn abandon_changes(&mut self) -> Result<(), AppError> {
        if self.wrote_recovery
            && let Some(path) = self.path.clone()
        {
            self.delete_recovery(&path);
        }
        self.autosave.clear();
        self.autosave_note = false;
        self.finish_background()
    }

    /// 終了の前に呼ぶ。積んだ recovery の書き込みと削除を終える。
    pub fn finish_background(&mut self) -> Result<(), AppError> {
        self.pump
            .flush()
            .map_err(|error| AppError::new(format!("自動復旧データを片付けられません: {error}")))
    }

    fn delete_recovery(&mut self, path: &Path) {
        self.wrote_recovery = false;
        self.pump.enqueue_delete(recovery_path(path));
    }

    pub fn reload_project(&mut self) -> Result<(), AppError> {
        let Some(path) = self.path.clone() else {
            return Err(AppError::new("開き直す Project がありません"));
        };
        self.open_project(&path)
    }
}

impl Default for Session {
    fn default() -> Self {
        Self::new()
    }
}

fn absolute_path(path: &Path) -> Result<PathBuf, AppError> {
    std::path::absolute(path).map_err(|error| {
        AppError::new(format!(
            "パスを絶対パスにできません ({}): {error}",
            path.display()
        ))
    })
}

fn project_bytes(project: &Project, path: &Path) -> Result<Vec<u8>, AppError> {
    let text =
        serialize_project_json(project, path).map_err(|error| AppError::new(error.to_string()))?;
    Ok(text.into_bytes())
}

fn new_session_id() -> String {
    let millis = SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0);
    format!(
        "{:x}-{:x}-{:x}",
        std::process::id(),
        millis,
        SESSION_SERIAL.fetch_add(1, Ordering::Relaxed)
    )
}

fn stamp_of(path: &Path) -> Option<(u64, i64)> {
    let probe = probe_file(path);
    (probe.status == mvm_platform::FileProbeStatus::File)
        .then_some((probe.size, probe.last_write_100ns))
}

struct RecoveryInspect {
    offer: Option<RecoveryOffer>,
    corrupt: bool,
    foreign: bool,
}

fn inspect_recovery(path: &Path, project: &Project, hash: &str) -> RecoveryInspect {
    let recovery = recovery_path(path);
    let Ok(bytes) = std::fs::read(&recovery) else {
        return RecoveryInspect {
            offer: None,
            corrupt: false,
            foreign: false,
        };
    };
    let Ok(text) = String::from_utf8(bytes) else {
        return RecoveryInspect {
            offer: None,
            corrupt: true,
            foreign: false,
        };
    };
    let header = match parse_recovery_header(&text) {
        Ok(header) => header,
        Err(_) => {
            return RecoveryInspect {
                offer: None,
                corrupt: true,
                foreign: false,
            };
        }
    };
    let recorded = identity_key(Path::new(&header.canonical_path));
    let opened = identity_key(path);
    if !recovery_belongs_to(&recorded, &opened) {
        return RecoveryInspect {
            offer: None,
            corrupt: false,
            foreign: true,
        };
    }
    let loaded = match recovery_project(&header, path) {
        Ok(project) => project,
        Err(_) => {
            return RecoveryInspect {
                offer: None,
                corrupt: true,
                foreign: false,
            };
        }
    };
    match classify_recovery(&loaded, project, &header.canonical_sha256, hash) {
        RecoveryDisposition::Stale => {
            let _ = std::fs::remove_file(recovery);
            RecoveryInspect {
                offer: None,
                corrupt: false,
                foreign: false,
            }
        }
        disposition => RecoveryInspect {
            offer: Some(RecoveryOffer {
                project: loaded,
                disposition,
            }),
            corrupt: false,
            foreign: false,
        },
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use std::fs;
    use std::sync::atomic::{AtomicU64, Ordering};

    use mvm_core::Rational;
    use mvm_project::{Project, TimelineClip, TimelineClipKind, TrackRef};

    use super::*;

    static SCRATCH: AtomicU64 = AtomicU64::new(1);

    fn scratch_dir() -> PathBuf {
        let n = SCRATCH.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("mvm-session-{}-{n}", std::process::id()));
        fs::create_dir_all(&dir).expect("作業 directory");
        dir
    }

    fn text_clip(name: &str) -> TimelineClip {
        TimelineClip {
            kind: TimelineClipKind::Text,
            name: name.to_owned(),
            id: format!("id-{name}"),
            source_fps: Rational::new(60, 1),
            source_frame_count: 30,
            source_in_frame: 0,
            source_out_frame: 30,
            timeline_start_frame: 0,
            track: TrackRef::video(0),
            ..TimelineClip::default()
        }
    }

    fn wait_for(
        session: &mut Session,
        mut now: u64,
        mut done: impl FnMut(&Session) -> bool,
    ) -> bool {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        while std::time::Instant::now() < deadline {
            session.poll_maintenance(now);
            if done(session) {
                return true;
            }
            now = now.saturating_add(20);
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        false
    }

    fn poll_until(session: &mut Session, mut done: impl FnMut(&Session) -> bool) -> bool {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        while std::time::Instant::now() < deadline {
            session.poll_preview();
            if done(session) {
                return true;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        false
    }

    #[test]
    fn playback_starts_at_the_playhead_and_pause_keeps_the_shown_frame() {
        let mut clip = text_clip("間");
        clip.source_frame_count = 600;
        clip.source_out_frame = 600;
        let mut session = Session::from_project(Project {
            timeline_clips: vec![clip],
            ..Project::default()
        });
        session.seek(120);
        session.refresh_preview();
        assert!(
            poll_until(&mut session, |session| session
                .preview_image()
                .is_some_and(|image| image.frame == 120)),
            "停止中の frame が届きません"
        );

        session.toggle_playback();
        assert!(session.is_playing());
        assert!(
            poll_until(&mut session, |session| session.playhead_frame() > 121),
            "playhead が進みません: {}",
            session.playhead_frame()
        );
        assert!(session.playhead_frame() < 600);

        session.pause_playback();
        let paused = session.playhead_frame();
        std::thread::sleep(std::time::Duration::from_millis(100));
        session.poll_preview();
        assert!(!session.is_playing());
        assert_eq!(
            session.playhead_frame(),
            paused,
            "停止後に playhead が動きました"
        );
        assert_eq!(
            session.preview_image().map(|image| image.frame),
            Some(paused),
            "表示と playhead が食い違っています"
        );
    }

    #[test]
    fn new_project_has_the_default_tracks_and_round_trips_through_a_file() {
        let dir = scratch_dir();
        let path = dir.join("new.mvm");
        let mut session = Session::new();
        assert_eq!(
            session
                .project()
                .video_tracks
                .iter()
                .map(|track| track.name.as_str())
                .collect::<Vec<_>>(),
            ["V1", "V2"]
        );
        assert_eq!(session.project().audio_tracks[0].name, "A1");
        session.create_project(&path).expect("作成");
        assert!(!session.dirty());
        assert_eq!(session.playhead_label(), "00:00:00:00  (0 frame)");
        drop(session);

        let mut opened = Session::new();
        opened.open_project(&path).expect("開く");
        assert_eq!(opened.project().output_width, 1920);
        assert_eq!(opened.project().output_height, 1080);
        assert_eq!(opened.project().video_tracks.len(), 2);
        drop(opened);
        fs::remove_dir_all(&dir).expect("片付け");
    }

    #[test]
    fn failed_open_keeps_the_current_project() {
        let project = Project {
            timeline_clips: vec![text_clip("残す")],
            ..Project::default()
        };
        let mut session = Session::from_project(project);
        let dir = scratch_dir();
        let missing = dir.join("missing.mvm");
        let error = session.open_project(&missing).expect_err("無い file");
        assert!(error.message().contains("読めません"), "{error}");
        assert_eq!(session.project().timeline_clips[0].name, "残す");

        let garbage = dir.join("bad.mvm");
        fs::write(&garbage, "not json").expect("ゴミ");
        let error = session.open_project(&garbage).expect_err("不正な JSON");
        assert!(error.message().contains("JSON"), "{error}");
        assert_eq!(session.project().timeline_clips[0].name, "残す");
        assert!(
            !lock_path(&garbage).exists(),
            "読めなかった file の lock が残っています"
        );
        drop(session);
        fs::remove_dir_all(&dir).expect("片付け");
    }

    #[test]
    fn undo_restores_the_previous_project_and_a_new_edit_drops_redo() {
        let mut session = Session::new();
        let error = session.undo().expect_err("履歴が無い");
        assert!(error.message().contains("戻す"), "{error}");
        session.place_text("あ").expect("文字");
        assert_eq!(session.project().timeline_clips.len(), 1);
        session.undo().expect("戻す");
        assert!(session.project().timeline_clips.is_empty());
        assert!(session.can_redo());
        session.place_text("い").expect("次の文字");
        assert!(!session.can_redo());
        assert_eq!(session.project().timeline_clips[0].name, "い");
        session.undo().expect("戻す");
        session.redo().expect("やり直す");
        assert_eq!(session.project().timeline_clips[0].name, "い");
    }

    #[test]
    fn a_second_session_cannot_open_a_locked_project() {
        let dir = scratch_dir();
        let path = dir.join("locked.mvm");
        let mut first = Session::new();
        first.create_project(&path).expect("作成");
        let mut second = Session::new();
        let error = second.open_project(&path).expect_err("使用中");
        assert!(error.message().contains("他の mvm"), "{error}");
        drop(first);
        second.open_project(&path).expect("lock を離したあと");
        drop(second);
        fs::remove_dir_all(&dir).expect("片付け");
    }

    #[test]
    fn autosave_writes_a_recovery_envelope_and_undo_to_clean_removes_it() {
        let dir = scratch_dir();
        let path = dir.join("auto.mvm");
        let recovery = recovery_path(&path);
        let mut session = Session::new();
        session.create_project(&path).expect("作成");
        session.place_text("復旧").expect("編集");
        session.poll_maintenance(0);
        assert!(!recovery.exists(), "debounce 前に recovery を書いた");
        assert!(
            wait_for(&mut session, 2_000, |session| recovery_path(
                session.path().unwrap()
            )
            .exists()),
            "recovery が書かれません: {}",
            session.alert().unwrap_or("")
        );
        let text = fs::read_to_string(&recovery).expect("recovery");
        mvm_project::recovery::parse_recovery_header(&text).expect("envelope");
        session.undo().expect("clean に戻す");
        assert!(!session.dirty());
        assert!(
            wait_for(&mut session, 2_000, |session| {
                !recovery_path(session.path().unwrap()).exists()
            }),
            "clean にしたあとも recovery が残っています"
        );
        drop(session);
        fs::remove_dir_all(&dir).expect("片付け");
    }

    #[test]
    fn save_refuses_an_external_change_until_overwrite() {
        let dir = scratch_dir();
        let path = dir.join("external.mvm");
        let mut session = Session::new();
        session.create_project(&path).expect("作成");
        session.place_text("手元").expect("編集");
        fs::write(&path, b"{}\n").expect("外部の書き換え");
        let error = session.save_project().expect_err("外部変更");
        assert!(error.message().contains("外部で変更"), "{error}");
        assert!(session.dirty());
        session.save_overwriting_external().expect("上書き");
        assert!(!session.dirty());
        assert!(!session.external_change());
        drop(session);
        fs::remove_dir_all(&dir).expect("片付け");
    }

    #[test]
    fn opening_another_project_removes_only_the_recovery_this_session_wrote() {
        let dir = scratch_dir();
        let first = dir.join("first.mvm");
        let second = dir.join("second.mvm");
        let mut session = Session::new();
        session.create_project(&second).expect("2 つ目を作る");
        session.create_project(&first).expect("1 つ目を作る");
        session.place_text("捨てる").expect("編集");
        assert!(
            wait_for(&mut session, 2_000, |_| recovery_path(&first).exists()),
            "recovery が書かれません"
        );
        session.open_project(&second).expect("切り替え");
        assert!(
            !recovery_path(&first).exists(),
            "捨てた変更の recovery が残っています"
        );

        // crash した session が残した recovery は、この session が書いたものではないので残す。
        let crashed = mvm_project::recovery::write_recovery_envelope(
            &Project {
                output_width: 1280,
                ..Project::default()
            },
            &first,
            &sha256_hex(&fs::read(&first).expect("canonical")),
            "2026-10-19T00:00:00.000Z",
            "crashed",
        )
        .expect("envelope");
        fs::write(recovery_path(&first), crashed).expect("crash の recovery");
        session.open_project(&first).expect("crash 後に開く");
        assert_eq!(session.recovery_prompt(), Some(RecoveryPrompt::Restorable));
        session.open_project(&second).expect("復元せずに切り替え");
        assert!(
            recovery_path(&first).exists(),
            "この session が書いていない recovery を消しました"
        );
        drop(session);
        fs::remove_dir_all(&dir).expect("片付け");
    }

    #[test]
    fn abandoning_changes_removes_the_recovery_before_returning() {
        let dir = scratch_dir();
        let path = dir.join("abandon.mvm");
        let mut session = Session::new();
        session.create_project(&path).expect("作成");
        session.place_text("閉じる").expect("編集");
        assert!(
            wait_for(&mut session, 2_000, |_| recovery_path(&path).exists()),
            "recovery が書かれません"
        );
        session.abandon_changes().expect("破棄");
        assert!(
            !recovery_path(&path).exists(),
            "破棄して閉じたのに recovery が残っています"
        );
        drop(session);
        fs::remove_dir_all(&dir).expect("片付け");
    }

    #[test]
    fn external_change_is_checked_once_a_second() {
        let dir = scratch_dir();
        let path = dir.join("poll.mvm");
        let mut session = Session::new();
        session.create_project(&path).expect("作成");
        session.poll_maintenance(10_000);
        assert!(!session.external_change());
        fs::write(
            &path, b"{}
",
        )
        .expect("外部の書き換え");
        session.poll_maintenance(10_999);
        assert!(
            !session.external_change(),
            "1 秒たつ前に file を読み直しました"
        );
        session.poll_maintenance(11_000);
        assert!(session.external_change(), "外部変更を見つけません");
        drop(session);
        fs::remove_dir_all(&dir).expect("片付け");
    }

    #[test]
    fn a_failed_command_keeps_the_project_and_the_history() {
        let mut session = Session::new();
        let error = session
            .transact(|project| {
                project.output_width = 640;
                Err(mvm_project::ProjectError::new("後半で失敗"))
            })
            .expect_err("失敗する編集");
        assert!(error.message().contains("後半"), "{error}");
        assert_eq!(session.project().output_width, 1920);
        assert!(!session.can_undo());
        assert!(!session.dirty());
    }

    #[test]
    fn seek_and_zoom_stay_in_range() {
        let mut session = Session::new();
        session.seek(-5);
        assert_eq!(session.playhead_frame(), 0);
        session.seek(12);
        assert_eq!(session.playhead_frame(), 12);
        session.set_pixels_per_frame(f32::NAN);
        assert_eq!(session.zoom_percent(), 100);
        session.zoom_by(2.0);
        assert_eq!(session.zoom_percent(), 200);
        session.set_pixels_per_frame(10_000.0);
        assert_eq!(session.pixels_per_frame(), 64.0);
    }
}
