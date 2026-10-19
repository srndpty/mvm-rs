//! 数式・数式列・Graph のラベルの描画（Manim / LaTeX）。session は描画の service を 1 つ持つ。
//!
//! - 数式の clip が初めて現れたときに service を起こす（Manim の確認は数秒かかるので、要るまで起こさない）。
//! - cache は `<Project の directory>/cache/math/<Project の file 名>/`（未保存なら一時 directory）。保存先が
//!   変われば service を作り直し、新しい場所で描き直す（cache は移さない）。
//! - 保存先がある Project は、Project lock を持っている instance だけが描く（他の instance の作業 directory を
//!   消さないため）。
//! - Project を変えるたびに全ての数式の描画を要求する（playhead に近い clip から）。描けたら preview を描き直す。

use std::path::PathBuf;
use std::sync::Arc;

use mvm_math::{
    MathJob, MathLibrary, MathService, MathStatus, clip_jobs, transition_job, wanted_jobs,
};
use mvm_project::{TimelineClip, TimelineClipKind};

use crate::preferences::Preferences;
use crate::session::Session;

pub(crate) struct MathState {
    service: MathService,
    cache: PathBuf,
    requested: Option<(u64, i64)>,
    library_revision: u64,
}

/// inspector に出す、clip の描画の状態。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MathClipStatus {
    /// 描画を使わない clip、またはまだ要求していない。
    Idle,
    Checking,
    /// 描画環境が無い・Project lock が無い（理由）。
    Unavailable(String),
    Rendering,
    Ready,
    /// 式の誤り（LaTeX の説明）。
    InvalidSource {
        message: String,
        log: String,
    },
    Failed {
        message: String,
        log: String,
    },
    /// Project の値から描画を組み立てられない（部分式の範囲など）。
    Invalid(String),
}

fn uses_math(clip: &TimelineClip) -> bool {
    matches!(
        clip.kind,
        TimelineClipKind::Math | TimelineClipKind::EquationSequence | TimelineClipKind::Graph
    )
}

impl Session {
    pub(crate) fn math_library(&self) -> Arc<MathLibrary> {
        self.math.as_ref().map_or_else(
            || Arc::new(MathLibrary::default()),
            |state| state.service.library(),
        )
    }

    fn math_cache_directory(&self) -> PathBuf {
        match self.path() {
            Some(path) => {
                let name = path
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "project".to_owned());
                path.parent()
                    .map(PathBuf::from)
                    .unwrap_or_default()
                    .join("cache")
                    .join("math")
                    .join(name)
            }
            None => std::env::temp_dir()
                .join("mvm-math-cache")
                .join(&self.session_id),
        }
    }

    /// 保存先があるのに Project lock を持っていなければ描かない。
    fn math_authority(&self) -> Result<(), String> {
        if self.path().is_some() && self.lock.is_none() {
            return Err(
                "他のプロセスがこの Project を編集しているので、数式を描けません".to_owned(),
            );
        }
        Ok(())
    }

    /// 毎 frame 呼ぶ。要る描画を要求し、描けた数式を preview へ渡す。何か変わったら true。
    pub fn poll_math(&mut self) -> bool {
        let needed = self.project.timeline_clips.iter().any(uses_math);
        if !needed && self.math.is_none() {
            return false;
        }
        if self.math_authority().is_err() {
            if self.math.take().is_some() {
                self.note_math_changed();
                return true;
            }
            return false;
        }
        let cache = self.math_cache_directory();
        if self.math.as_ref().is_some_and(|state| state.cache != cache) {
            self.math = None;
        }
        if self.math.is_none() {
            let manim = Preferences::load()
                .ok()
                .map(|preferences| preferences.manim_executable)
                .filter(|path| !path.trim().is_empty())
                .map(PathBuf::from);
            match MathService::start(cache.clone(), manim) {
                Ok(service) => {
                    if let Some(wake) = &self.preview_wake {
                        let wake = Arc::clone(wake);
                        service.set_wake(Arc::new(move || wake()));
                    }
                    self.math = Some(MathState {
                        service,
                        cache,
                        requested: None,
                        library_revision: 0,
                    });
                }
                Err(error) => {
                    self.alert = error.to_string();
                    return false;
                }
            }
        }
        let revision = self.edit_revision;
        let playhead = self.playhead_frame;
        let project = &self.project;
        let Some(state) = self.math.as_mut() else {
            return false;
        };
        // playhead だけの変化は 1 秒（60 frame）ずれるまで並べ直さない。
        let stale = match state.requested {
            None => true,
            Some((requested, at)) => requested != revision || (at - playhead).abs() > 60,
        };
        if stale {
            state.service.request(wanted_jobs(project, playhead));
            state.requested = Some((revision, playhead));
        }
        let current = state.service.revision();
        if current == state.library_revision {
            return false;
        }
        state.library_revision = current;
        self.note_math_changed();
        true
    }

    /// 描けた数式が変わった。再生を止めずに engine へ渡し、止まっていれば今の frame を描き直す。
    fn note_math_changed(&mut self) {
        let library = self.math_library();
        if let Some(engine) = self.meter_source()
            && let Err(error) = engine.set_math(library)
        {
            self.alert = error.to_string();
        }
        if !self.is_playing() {
            self.invalidate_preview();
        }
    }

    /// 描画環境を確かめ直し、失敗を忘れて描き直す（MiKTeX の導入後など）。
    pub fn retry_math(&mut self) {
        if let Some(state) = &self.math {
            state.service.retry();
        }
    }

    /// clip の描画の状態。変形の状態は前・後ろのどちらの clip でも見せる。
    pub fn math_status(&self, clip: &TimelineClip) -> MathClipStatus {
        if !uses_math(clip) {
            return MathClipStatus::Idle;
        }
        if let Err(reason) = self.math_authority() {
            return MathClipStatus::Unavailable(reason);
        }
        let mut jobs = match clip_jobs(&self.project, clip) {
            Ok(jobs) => jobs,
            Err(error) => return MathClipStatus::Invalid(error.to_string()),
        };
        for transition in &self.project.timeline_transitions {
            if transition.outgoing_clip_id != clip.id && transition.incoming_clip_id != clip.id {
                continue;
            }
            match transition_job(&self.project, transition) {
                Ok(Some(job)) => jobs.push(job),
                Ok(None) => {}
                Err(error) => return MathClipStatus::Invalid(error.to_string()),
            }
        }
        let Some(state) = &self.math else {
            return if jobs.is_empty() {
                MathClipStatus::Ready
            } else {
                MathClipStatus::Idle
            };
        };
        summarize(jobs.iter().map(|job: &MathJob| state.service.status(job)))
    }
}

/// 複数の描画の状態を 1 つにまとめる。失敗 > 利用不可 > 確認中 > 描画中 > 完了。
fn summarize(statuses: impl Iterator<Item = MathStatus>) -> MathClipStatus {
    let mut result = MathClipStatus::Ready;
    let rank = |status: &MathClipStatus| match status {
        MathClipStatus::InvalidSource { .. } | MathClipStatus::Failed { .. } => 5,
        MathClipStatus::Unavailable(_) => 4,
        MathClipStatus::Checking => 3,
        MathClipStatus::Rendering => 2,
        _ => 0,
    };
    for status in statuses {
        let next = match status {
            MathStatus::Checking => MathClipStatus::Checking,
            MathStatus::Unavailable(reason) => MathClipStatus::Unavailable(reason),
            MathStatus::Pending | MathStatus::Rendering => MathClipStatus::Rendering,
            MathStatus::Ready => MathClipStatus::Ready,
            MathStatus::InvalidSource { message, log } => {
                MathClipStatus::InvalidSource { message, log }
            }
            MathStatus::Failed { message, log } => MathClipStatus::Failed { message, log },
        };
        if rank(&next) > rank(&result) {
            result = next;
        }
    }
    result
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn the_worst_status_wins() {
        assert_eq!(summarize(std::iter::empty()), MathClipStatus::Ready);
        assert_eq!(
            summarize([MathStatus::Ready, MathStatus::Pending].into_iter()),
            MathClipStatus::Rendering
        );
        let failed = MathStatus::Failed {
            message: "m".to_owned(),
            log: String::new(),
        };
        assert!(matches!(
            summarize([MathStatus::Rendering, failed, MathStatus::Checking].into_iter()),
            MathClipStatus::Failed { .. }
        ));
    }

    #[test]
    fn a_project_without_math_never_starts_the_service() {
        let mut session = Session::new();
        assert!(!session.poll_math());
        assert!(session.math.is_none());
    }
}
