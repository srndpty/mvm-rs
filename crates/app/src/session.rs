//! 開いている Project と、playhead・zoom・停止中の preview。

use std::path::{Path, PathBuf};
use std::sync::Arc;

use mvm_core::timecode::format_timecode;
use mvm_platform::atomic_write;
use mvm_preview::{FramePlan, PreviewImage, StillPreview, black_image, plan_frame};
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
    preview_engine: Option<StillPreview>,
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
            preview_engine: None,
        }
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
        self.invalidate_preview();
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
        self.scrubbing = scrubbing;
        if !scrubbing {
            self.preview_at = None;
        }
    }

    pub fn seek(&mut self, frame: i64) {
        let frame = frame.max(0);
        if frame != self.playhead_frame {
            self.playhead_frame = frame;
            self.preview_at = None;
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

    /// playhead の frame を、まだ描いていなければ描く。スクラブ中は何もしない。
    pub fn refresh_preview(&mut self) {
        if self.scrubbing || self.preview_at == Some(self.playhead_frame) {
            return;
        }
        let frame = self.playhead_frame;
        let plan = match plan_frame(&self.project, frame) {
            Ok(plan) => plan,
            Err(error) => {
                self.fail_preview(frame, error.to_string());
                return;
            }
        };
        self.skipped_note = (plan.skipped_clips > 0).then(|| {
            "文字・数式・グラフ・Manim はこの frame にありますが、まだ描きません".to_owned()
        });
        if plan.layers.is_empty() {
            match black_image(frame, &self.project) {
                Ok(image) => self.store_preview(frame, image, None),
                Err(error) => self.fail_preview(frame, error.to_string()),
            }
            return;
        }
        if let Err(error) = self.render_layers(frame, &plan) {
            self.fail_preview(frame, error);
        }
    }

    /// 同じ frame でも preview を描き直す。
    pub fn invalidate_preview(&mut self) {
        self.preview_at = None;
    }

    fn render_layers(&mut self, frame: i64, plan: &FramePlan) -> Result<(), String> {
        if self.preview_engine.is_none() {
            self.preview_engine = Some(StillPreview::new().map_err(|error| error.to_string())?);
        }
        let Some(engine) = self.preview_engine.as_mut() else {
            return Err("preview を用意できません".to_owned());
        };
        let image = engine
            .render(&self.project, frame, plan)
            .map_err(|error| error.to_string())?;
        self.store_preview(frame, image, None);
        Ok(())
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
        self.preview_engine = None;
        self.preview_image = None;
        self.preview_error = None;
        self.skipped_note = None;
        self.preview_at = None;
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
