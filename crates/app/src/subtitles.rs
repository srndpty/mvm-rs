//! 字幕の編集 command。区間・重なり・リンクの判定は project の字幕 module が行う。
//!
//! 字幕の選択は clip の選択と同時に持たない。削除・コピー・貼り付け・複製は、選択している方に効く。

use std::path::Path;

use mvm_platform::atomic_write;
use mvm_project::subtitles::{
    apply_subtitle_cues, delete_subtitle, edit_subtitle, merge_subtitle, parse_srt, split_subtitle,
    validate_subtitles, write_srt,
};
use mvm_project::{SubtitleCue, SubtitleStyle, SubtitleTrack};

use crate::error::AppError;
use crate::session::Session;

impl Session {
    pub fn subtitle_selection(&self) -> &[String] {
        &self.subtitle_selection
    }

    /// 字幕を選ぶ。`additive` なら選択へ足し引きする。clip の選択は外す。
    pub fn select_subtitle(&mut self, id: &str, additive: bool) {
        self.selection.clear();
        self.selected_edge = None;
        self.selected_marker = None;
        if additive {
            if let Some(index) = self.subtitle_selection.iter().position(|item| item == id) {
                self.subtitle_selection.remove(index);
            } else {
                self.subtitle_selection.push(id.to_owned());
            }
        } else {
            self.subtitle_selection = vec![id.to_owned()];
        }
    }

    pub fn select_subtitles(&mut self, ids: Vec<String>) {
        self.selection.clear();
        self.selected_edge = None;
        self.selected_marker = None;
        self.subtitle_selection = ids;
    }

    pub fn clear_subtitle_selection(&mut self) {
        self.subtitle_selection.clear();
    }

    fn cue(&self, id: &str) -> Option<&SubtitleCue> {
        self.project
            .subtitles
            .as_ref()?
            .cues
            .iter()
            .find(|cue| cue.id == id)
    }

    fn fresh_cue_id(&mut self) -> String {
        let mut factory = self.id_factory();
        let id = factory.next();
        self.id_serial = factory.serial;
        id
    }

    /// 字幕を 1 つ足し、それを選ぶ。重なり・空の本文・0 frame は project が拒否する。
    pub fn add_subtitle(
        &mut self,
        start: i64,
        end: i64,
        content: &str,
    ) -> Result<String, AppError> {
        let id = self.fresh_cue_id();
        let cue = SubtitleCue {
            id: id.clone(),
            start_frame: start,
            end_frame: end,
            content: content.to_owned(),
            link_clip_id: String::new(),
        };
        self.transact(move |project| edit_subtitle(project, cue).map(|()| None))?;
        self.select_subtitles(vec![id.clone()]);
        Ok(id)
    }

    /// 区間と本文を変える。clip とのリンクは保つ。
    pub fn update_subtitle(
        &mut self,
        id: &str,
        start: i64,
        end: i64,
        content: &str,
    ) -> Result<(), AppError> {
        let Some(existing) = self.cue(id).cloned() else {
            return Err(AppError::new("更新する字幕がありません"));
        };
        let cue = SubtitleCue {
            start_frame: start,
            end_frame: end,
            content: content.to_owned(),
            ..existing
        };
        self.transact(move |project| edit_subtitle(project, cue).map(|()| None))
    }

    pub fn delete_subtitles(&mut self, ids: &[String]) -> Result<(), AppError> {
        if ids.is_empty() {
            return Err(AppError::new("削除する字幕がありません"));
        }
        let ids = ids.to_vec();
        self.transact(move |project| {
            for id in &ids {
                delete_subtitle(project, id)?;
            }
            Ok(None)
        })?;
        self.subtitle_selection.clear();
        Ok(())
    }

    /// 再生位置で分ける。`text_break` は本文の文字（char）の位置。無ければ句読点などで分ける。
    pub fn split_subtitle_at_playhead(
        &mut self,
        id: &str,
        text_break: Option<usize>,
    ) -> Result<(), AppError> {
        let frame = self.playhead_frame;
        let new_id = self.fresh_cue_id();
        let id = id.to_owned();
        self.transact(move |project| {
            split_subtitle(project, &id, frame, &new_id, text_break).map(|()| None)
        })
    }

    pub fn merge_subtitle_with_next(&mut self, id: &str) -> Result<(), AppError> {
        let id = id.to_owned();
        self.transact(move |project| merge_subtitle(project, &id).map(|()| None))
    }

    /// 全ての字幕の共通書式。字幕 track が無ければ作る。
    pub fn set_subtitle_style(&mut self, style: SubtitleStyle) -> Result<(), AppError> {
        self.transact(move |project| {
            project
                .subtitles
                .get_or_insert_with(SubtitleTrack::default)
                .style = style;
            validate_subtitles(project).map(|()| None)
        })
    }

    /// 選択した字幕を `delta` frame 動かす（`duplicate` なら元を残して写す）。先頭より前へは 0 で止める。
    /// 他の字幕と重なる移動は全体を拒否する。
    pub fn move_subtitles(
        &mut self,
        ids: &[String],
        delta: i64,
        duplicate: bool,
    ) -> Result<(), AppError> {
        let Some(track) = self.project.subtitles.as_ref() else {
            return Err(AppError::new("動かす字幕がありません"));
        };
        let moving: Vec<SubtitleCue> = track
            .cues
            .iter()
            .filter(|cue| ids.contains(&cue.id))
            .cloned()
            .collect();
        if moving.is_empty() {
            return Err(AppError::new("動かす字幕がありません"));
        }
        let first = moving.iter().map(|cue| cue.start_frame).min().unwrap_or(0);
        let delta = delta.max(-first);
        if delta == 0 && !duplicate {
            return Ok(());
        }
        let mut cues: Vec<SubtitleCue> = if duplicate {
            track.cues.clone()
        } else {
            track
                .cues
                .iter()
                .filter(|cue| !ids.contains(&cue.id))
                .cloned()
                .collect()
        };
        let mut factory = self.id_factory();
        let mut moved_ids = Vec::new();
        for cue in moving {
            let id = if duplicate { factory.next() } else { cue.id };
            moved_ids.push(id.clone());
            cues.push(SubtitleCue {
                id,
                start_frame: cue.start_frame + delta,
                end_frame: cue.end_frame + delta,
                link_clip_id: if duplicate {
                    String::new()
                } else {
                    cue.link_clip_id
                },
                ..cue
            });
        }
        self.id_serial = factory.serial;
        self.transact(move |project| apply_subtitle_cues(project, cues, true).map(|()| None))?;
        self.select_subtitles(moved_ids);
        Ok(())
    }

    pub fn copy_subtitles(&mut self) -> Result<(), AppError> {
        let cues: Vec<SubtitleCue> = self
            .project
            .subtitles
            .iter()
            .flat_map(|track| track.cues.iter())
            .filter(|cue| self.subtitle_selection.contains(&cue.id))
            .cloned()
            .collect();
        if cues.is_empty() {
            return Err(AppError::new("選択している字幕がありません"));
        }
        self.subtitle_clipboard = Some(cues);
        self.clipboard = None;
        Ok(())
    }

    /// clipboard の字幕を、先頭からの相対位置を保って再生位置へ貼る。重なるなら全体を拒否する。
    pub fn paste_subtitles(&mut self) -> Result<(), AppError> {
        let Some(cues) = self.subtitle_clipboard.clone() else {
            return Err(AppError::new("貼り付ける字幕がありません"));
        };
        let frame = self.playhead_frame;
        self.place_copied_subtitles(&cues, frame)
    }

    pub fn duplicate_subtitles(&mut self) -> Result<(), AppError> {
        let cues: Vec<SubtitleCue> = self
            .project
            .subtitles
            .iter()
            .flat_map(|track| track.cues.iter())
            .filter(|cue| self.subtitle_selection.contains(&cue.id))
            .cloned()
            .collect();
        if cues.is_empty() {
            return Err(AppError::new("選択している字幕がありません"));
        }
        let frame = self.playhead_frame;
        self.place_copied_subtitles(&cues, frame)
    }

    fn place_copied_subtitles(&mut self, cues: &[SubtitleCue], frame: i64) -> Result<(), AppError> {
        let first = cues.iter().map(|cue| cue.start_frame).min().unwrap_or(0);
        let mut factory = self.id_factory();
        let placed: Vec<SubtitleCue> = cues
            .iter()
            .map(|cue| SubtitleCue {
                id: factory.next(),
                start_frame: cue.start_frame - first + frame,
                end_frame: cue.end_frame - first + frame,
                content: cue.content.clone(),
                link_clip_id: String::new(),
            })
            .collect();
        self.id_serial = factory.serial;
        let ids: Vec<String> = placed.iter().map(|cue| cue.id.clone()).collect();
        self.transact(move |project| apply_subtitle_cues(project, placed, false).map(|()| None))?;
        self.select_subtitles(ids);
        Ok(())
    }

    /// SRT を読み、置換または追加する。不正な行があれば全体を取り消す。
    pub fn import_srt(&mut self, path: &Path, replace: bool) -> Result<usize, AppError> {
        let bytes = std::fs::read(path)
            .map_err(|error| AppError::new(format!("SRT を読めません: {error}")))?;
        let text = String::from_utf8(bytes)
            .map_err(|_| AppError::new("SRT は UTF-8 で保存してください"))?;
        let mut factory = self.id_factory();
        let cues = parse_srt(&text, self.project.timeline_fps, &mut || factory.next())
            .map_err(|error| AppError::new(error.to_string()))?;
        self.id_serial = factory.serial;
        let count = cues.len();
        self.transact(move |project| apply_subtitle_cues(project, cues, replace).map(|()| None))?;
        Ok(count)
    }

    pub fn export_srt(&self, path: &Path) -> Result<(), AppError> {
        let text = write_srt(&self.project).map_err(|error| AppError::new(error.to_string()))?;
        atomic_write(path, text.as_bytes()).map_err(|error| AppError::new(error.to_string()))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use mvm_project::Project;

    use super::*;

    fn cues(session: &Session) -> Vec<(i64, i64, String)> {
        session
            .project()
            .subtitles
            .as_ref()
            .map(|track| {
                track
                    .cues
                    .iter()
                    .map(|cue| (cue.start_frame, cue.end_frame, cue.content.clone()))
                    .collect()
            })
            .unwrap_or_default()
    }

    #[test]
    fn add_update_split_merge_and_delete_are_each_one_undo_step() {
        let mut session = Session::from_project(Project::default());
        let id = session.add_subtitle(10, 70, "こんにちは。世界").unwrap();
        assert_eq!(session.subtitle_selection(), std::slice::from_ref(&id));
        session
            .update_subtitle(&id, 10, 80, "こんにちは。世界")
            .unwrap();
        session.seek(40);
        session.split_subtitle_at_playhead(&id, Some(6)).unwrap();
        assert_eq!(
            cues(&session),
            vec![
                (10, 40, "こんにちは。".to_owned()),
                (40, 80, "世界".to_owned())
            ]
        );
        session.merge_subtitle_with_next(&id).unwrap();
        assert_eq!(
            cues(&session),
            vec![(10, 80, "こんにちは。\n世界".to_owned())]
        );
        session.undo().unwrap();
        assert_eq!(cues(&session).len(), 2);
        let ids: Vec<String> = session
            .project()
            .subtitles
            .as_ref()
            .unwrap()
            .cues
            .iter()
            .map(|cue| cue.id.clone())
            .collect();
        session.delete_subtitles(&ids).unwrap();
        assert!(cues(&session).is_empty());
        session.undo().unwrap();
        assert_eq!(cues(&session).len(), 2);
    }

    #[test]
    fn overlapping_subtitles_are_rejected_without_a_history_entry() {
        let mut session = Session::from_project(Project::default());
        session.add_subtitle(0, 30, "a").unwrap();
        let second = session.add_subtitle(30, 60, "b").unwrap();
        let before = session.project().clone();
        assert!(session.add_subtitle(20, 40, "c").is_err());
        assert!(session.update_subtitle(&second, 10, 60, "b").is_err());
        assert!(
            session
                .move_subtitles(std::slice::from_ref(&second), -5, false)
                .is_err()
        );
        assert_eq!(session.project(), &before);
        // 先頭より前への移動は 0 で止まる（a を動かす余地がある場合）。
        let first = session.project().subtitles.as_ref().unwrap().cues[0]
            .id
            .clone();
        session.move_subtitles(&[first], -100, false).unwrap();
        assert_eq!(cues(&session)[0].0, 0);
    }

    #[test]
    fn copy_and_paste_keep_relative_positions_and_take_the_clip_clipboard_slot() {
        let mut session = Session::from_project(Project::default());
        let a = session.add_subtitle(0, 10, "a").unwrap();
        let b = session.add_subtitle(20, 30, "b").unwrap();
        session.select_subtitles(vec![a, b]);
        session.copy_selection().unwrap();
        assert!(session.has_clipboard());
        session.seek(100);
        session.paste().unwrap();
        assert_eq!(
            cues(&session)[2..],
            [(100, 110, "a".to_owned()), (120, 130, "b".to_owned())]
        );
        assert_eq!(session.subtitle_selection().len(), 2);
        // 同じ位置へもう一度貼ると重なるので全体を拒否する。
        assert!(session.paste().is_err());
        assert_eq!(cues(&session).len(), 4);
    }

    #[test]
    fn alt_drag_duplicates_and_leaves_the_originals() {
        let mut session = Session::from_project(Project::default());
        let a = session.add_subtitle(0, 10, "a").unwrap();
        session
            .move_subtitles(std::slice::from_ref(&a), 50, true)
            .unwrap();
        assert_eq!(
            cues(&session),
            vec![(0, 10, "a".to_owned()), (50, 60, "a".to_owned())]
        );
        assert_ne!(session.subtitle_selection(), [a]);
    }

    #[test]
    fn srt_round_trips_through_files() {
        let directory = std::env::temp_dir().join(format!("mvm-srt-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let mut session = Session::from_project(Project::default());
        session.add_subtitle(60, 120, "一行目\n二行目").unwrap();
        let path = directory.join("out.srt");
        session.export_srt(&path).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            text.starts_with("1\n00:00:01,000 --> 00:00:02,000\n一行目\n二行目"),
            "{text}"
        );

        let mut other = Session::from_project(Project::default());
        assert_eq!(other.import_srt(&path, true).unwrap(), 1);
        assert_eq!(cues(&other), vec![(60, 120, "一行目\n二行目".to_owned())]);
        // 不正な SRT は全体を取り消す。
        std::fs::write(&path, "1\nbad\n").unwrap();
        assert!(other.import_srt(&path, true).is_err());
        assert_eq!(cues(&other).len(), 1);
        std::fs::remove_dir_all(&directory).unwrap();
    }

    #[test]
    fn style_changes_are_validated() {
        let mut session = Session::from_project(Project::default());
        let mut style = SubtitleStyle {
            font_size: 80,
            ..SubtitleStyle::default()
        };
        session.set_subtitle_style(style.clone()).unwrap();
        assert_eq!(
            session
                .project()
                .subtitles
                .as_ref()
                .unwrap()
                .style
                .font_size,
            80
        );
        style.color = "red".to_owned();
        assert!(session.set_subtitle_style(style).is_err());
    }

    #[test]
    fn opening_another_project_clears_the_subtitle_selection() {
        let directory = std::env::temp_dir().join(format!("mvm-sub-switch-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let mut session = Session::from_project(Project::default());
        session.add_subtitle(0, 10, "a").unwrap();
        assert_eq!(session.subtitle_selection().len(), 1);
        session
            .create_project(&directory.join("other.mvm"))
            .unwrap();
        assert!(
            session.subtitle_selection().is_empty(),
            "前の Project の字幕が選ばれたまま"
        );
        assert!(session.delete_selection().is_err());
        drop(session);
        std::fs::remove_dir_all(&directory).unwrap();
    }

    #[test]
    fn selecting_a_clip_clears_the_subtitle_selection() {
        let mut session = Session::from_project(Project::default());
        let id = session.add_subtitle(0, 10, "a").unwrap();
        assert_eq!(session.subtitle_selection(), [id]);
        session.select_only("clip".to_owned());
        assert!(session.subtitle_selection().is_empty());
    }
}
