//! 書き出しの開始・進捗・取消。合成と encode は export crate の worker が行う。
//!
//! worker は開始時の Project の snapshot を使う。書き出し中の編集は書き出しに入らない。

use std::path::PathBuf;
use std::sync::Arc;

use mvm_export::{
    ExportError, ExportJob, ExportProgress, ExportRequest, ExportStatus, ExportSummary, plan_export,
};

use crate::error::AppError;
use crate::session::Session;

impl Session {
    /// dialog が最初に出す書き出し先。保存済みなら Project と同じ場所・同じ名前の `.mp4`。
    pub fn default_export_path(&self) -> PathBuf {
        self.path()
            .map(|path| path.with_extension("mp4"))
            .unwrap_or_default()
    }

    /// 書き出しを始める。書き出せない Project は、再生を止める前にここで失敗させる。
    pub fn start_export(&mut self, request: &ExportRequest) -> Result<(), AppError> {
        if self.export.is_some() {
            return Err(AppError::new(
                "書き出し中です。終わるか取り消してから始めてください",
            ));
        }
        let plan = plan_export(&self.project, request, self.math_library())
            .map_err(|error| AppError::new(error.to_string()))?;
        if self.is_playing() {
            self.pause_playback();
        }
        let job = ExportJob::start(
            Arc::new(self.project.clone()),
            plan,
            self.preview_wake.clone(),
        )
        .map_err(|error| AppError::new(error.to_string()))?;
        self.export = Some(job);
        Ok(())
    }

    pub fn is_exporting(&self) -> bool {
        self.export.is_some()
    }

    /// 走っている書き出しの進み具合。終わっていれば [`Self::poll_export`] が結果を返す。
    pub fn export_progress(&self) -> Option<ExportProgress> {
        match self.export.as_ref()?.status() {
            ExportStatus::Running(progress) => Some(progress),
            ExportStatus::Finished(_) => None,
        }
    }

    pub fn cancel_export(&self) {
        if let Some(job) = &self.export {
            job.cancel();
        }
    }

    /// 書き出しが終わっていれば、その結果を 1 回だけ返す。
    pub fn poll_export(&mut self) -> Option<Result<ExportSummary, ExportError>> {
        let ExportStatus::Finished(result) = self.export.as_ref()?.status() else {
            return None;
        };
        self.export = None;
        Some(result)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use mvm_export::ExportQuality;

    use super::*;
    use crate::session::ExistingFile;

    fn request(path: &str) -> ExportRequest {
        ExportRequest {
            output_path: PathBuf::from(path),
            quality: ExportQuality::Standard,
            burn_subtitles: true,
            replace: None,
        }
    }

    #[test]
    fn a_project_with_nothing_to_output_does_not_start() {
        let mut session = Session::new();
        let error = session.start_export(&request("out.mp4")).unwrap_err();
        assert!(error.message().contains("有効な clip"), "{error}");
        assert!(!session.is_exporting());
        assert!(session.export_progress().is_none());
        assert!(session.poll_export().is_none());
    }

    #[test]
    fn the_project_cannot_be_switched_while_exporting() {
        let dir =
            std::env::temp_dir().join(format!("mvm-app-export-switch-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let other = dir.join("other.mvm");
        let mut creator = Session::new();
        creator
            .create_project(&other, ExistingFile::Keep)
            .expect("切り替え先");
        drop(creator);
        let mut session = Session::new();
        session.place_text("書き出す").expect("clip");
        session
            .start_export(&request(&dir.join("out.mp4").to_string_lossy()))
            .expect("開始");
        let error = session
            .open_project(&other)
            .expect_err("書き出し中に開きました");
        assert!(error.message().contains("書き出し中"), "{error}");
        let error = session
            .create_project(&dir.join("new.mvm"), ExistingFile::Keep)
            .expect_err("書き出し中に作りました");
        assert!(error.message().contains("書き出し中"), "{error}");
        assert!(!dir.join("new.mvm").exists());
        assert!(session.path().is_none(), "画面の Project が変わりました");
        session.cancel_export();
        while session.is_exporting() {
            let _ = session.poll_export();
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        // 書き出しが終われば切り替えられる（負の対照）。
        session.open_project(&other).expect("書き出しの後");
        drop(session);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_default_path_follows_the_saved_project() {
        let session = Session::new();
        assert_eq!(session.default_export_path(), PathBuf::new());
        let dir = std::env::temp_dir().join(format!("mvm-app-export-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut saved = Session::new();
        saved
            .create_project(&dir.join("講義.mvm"), ExistingFile::Keep)
            .expect("保存");
        assert_eq!(saved.default_export_path(), dir.join("講義.mp4"));
        drop(saved);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
