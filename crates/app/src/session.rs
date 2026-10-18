//! 開いている Project と、playhead・zoom・preview の再生。

use std::path::{Path, PathBuf};
use std::sync::Arc;

use mvm_core::timecode::format_timecode;
use mvm_platform::atomic_write;
use mvm_preview::{PreviewEngine, PreviewImage, plan_frame};
use mvm_project::Project;
use mvm_project::json::{parse_project_json, serialize_project_json};

use crate::error::AppError;
use crate::timeline_view::{DEFAULT_PIXELS_PER_FRAME, clamp_pixels_per_frame};

/// 編集 session。Project の正本はここだけが持ち、UI は借りて表示する。
pub struct Session {
    project: Project,
    path: Option<PathBuf>,
    dirty: bool,
    playhead_frame: i64,
    pixels_per_frame: f32,
    selected_clip: Option<String>,
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
    preview_wake: Option<Arc<dyn Fn() + Send + Sync>>,
}

impl Session {
    /// 空の新規 Project。まだ file には書いていない。
    pub fn new() -> Self {
        Self::from_project(Project::default())
    }

    /// メモリ上の Project を、未保存の session として開く。
    pub fn from_project(project: Project) -> Self {
        Self {
            project,
            path: None,
            dirty: false,
            playhead_frame: 0,
            pixels_per_frame: DEFAULT_PIXELS_PER_FRAME,
            selected_clip: None,
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
        self.selected_clip.as_deref()
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
        self.edit_epoch = self.edit_epoch.saturating_add(1);
        self.invalidate_preview();
        if self.playing {
            self.pause_playback();
        }
    }

    pub fn select_clip(&mut self, id: Option<String>) {
        self.selected_clip = id;
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
        write_project(&project, &path)?;
        self.replace(project, Some(path), false);
        Ok(())
    }

    /// `path` を読む。失敗しても今の session は残す。
    pub fn open_project(&mut self, path: &Path) -> Result<(), AppError> {
        let path = absolute_path(path)?;
        let text = std::fs::read_to_string(&path).map_err(|error| {
            AppError::new(format!(
                "Project を読めません ({}): {error}",
                path.display()
            ))
        })?;
        let project =
            parse_project_json(&text, &path).map_err(|error| AppError::new(error.to_string()))?;
        self.replace(project, Some(path), false);
        Ok(())
    }

    /// 保存先があるときだけ書く。無いときは [`Self::save_project_as`] を使う。
    pub fn save_project(&mut self) -> Result<(), AppError> {
        let Some(path) = self.path.clone() else {
            return Err(AppError::new("保存先がありません"));
        };
        write_project(&self.project, &path)?;
        self.dirty = false;
        Ok(())
    }

    pub fn save_project_as(&mut self, path: &Path) -> Result<(), AppError> {
        let path = absolute_path(path)?;
        write_project(&self.project, &path)?;
        self.path = Some(path);
        self.dirty = false;
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
        self.skipped_note = plan_frame(&self.project, frame).ok().and_then(|plan| {
            (plan.skipped_clips > 0)
                .then(|| "数式・グラフ・Manim はこの frame にありますが、まだ描きません".to_owned())
        });
    }

    fn ensure_engine(&mut self) -> Result<(), String> {
        if self.engine.is_none() {
            let engine = PreviewEngine::start().map_err(|error| error.to_string())?;
            if let Some(wake) = &self.preview_wake {
                engine.set_wake(Arc::clone(wake));
            }
            self.engine = Some(engine);
            self.sent_epoch = 0;
        }
        Ok(())
    }

    fn send_project(&mut self, generation: u64) -> Result<(), String> {
        let project = Arc::new(self.project.clone());
        let Some(engine) = &self.engine else {
            return Err("preview を用意できません".to_owned());
        };
        engine
            .set_project(project, generation)
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

    fn replace(&mut self, project: Project, path: Option<PathBuf>, dirty: bool) {
        self.project = project;
        self.path = path;
        self.dirty = dirty;
        self.playhead_frame = 0;
        self.selected_clip = None;
        self.scrubbing = false;
        self.playing = false;
        self.engine = None;
        self.preview_image = None;
        self.preview_error = None;
        self.skipped_note = None;
        self.preview_at = None;
        self.sent_epoch = 0;
        self.edit_epoch = self.edit_epoch.saturating_add(1);
        self.preview_generation = self.preview_generation.saturating_add(1);
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

fn write_project(project: &Project, path: &Path) -> Result<(), AppError> {
    let text =
        serialize_project_json(project, path).map_err(|error| AppError::new(error.to_string()))?;
    atomic_write(path, text.as_bytes()).map_err(|error| AppError::new(error.to_string()))
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

        let mut opened = Session::new();
        opened.open_project(&path).expect("開く");
        assert_eq!(opened.project().output_width, 1920);
        assert_eq!(opened.project().output_height, 1080);
        assert_eq!(opened.project().video_tracks.len(), 2);
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
        fs::remove_dir_all(&dir).expect("片付け");
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
