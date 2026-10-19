//! 素材の取り込みと timeline への配置、プロジェクトパネルの編集。
//!
//! 素材の値は使う時点でファイルを調べ直して更新する（登録後に別の中身へ差し替わったファイルを、古い値のまま
//! 置かない）。配置の規則は project が決める。

use std::path::{Path, PathBuf};

use mvm_core::Rational;
use mvm_media::{ProbedMedia, probe_media_file};
use mvm_project::media_bin::{
    add_media_folder, add_media_item, media_clips_for_item, move_media_bin_entries,
    plan_media_bin_removal, refresh_media_item, remove_media_bin_entries, rename_media_bin_entry,
};
use mvm_project::path_identity::{PathSameness, compare_path_identity};
use mvm_project::timeline::{
    place_audio_clip_at, place_media_at_drop, place_still_clip_at, place_video_media_at,
    timeline_clip_duration,
};
use mvm_project::{MediaFolder, MediaItem, MediaKind, Project, ProjectError, TrackRef};

use crate::error::AppError;
use crate::files::identity_key;
use crate::session::Session;

/// ファイルを調べた結果。`has_audio` は素材の値には無いが、動画のリンク音声を作るかを決める。
#[derive(Clone, Debug)]
struct Probed {
    item: MediaItem,
    has_audio: bool,
}

fn probe(path: &Path) -> Result<Probed, AppError> {
    let probed = probe_media_file(path).map_err(|error| AppError::new(error.to_string()))?;
    probed_item(path, &probed)
}

fn probed_item(path: &Path, probed: &ProbedMedia) -> Result<Probed, AppError> {
    let base = MediaItem {
        media_path: path.to_path_buf(),
        name: file_name(path),
        ..MediaItem::default()
    };
    let probed = match probed {
        ProbedMedia::Video(video) => {
            let fps = Rational::new(i64::from(video.fps_num), i64::from(video.fps_den))
                .reduced()
                .ok_or_else(|| AppError::new("動画の fps が不正です"))?;
            Probed {
                item: MediaItem {
                    kind: MediaKind::Video,
                    fps,
                    frame_count: video.frame_count,
                    width: video.width,
                    height: video.height,
                    ..base
                },
                has_audio: video.has_audio,
            }
        }
        ProbedMedia::Audio(audio) => Probed {
            item: MediaItem {
                kind: MediaKind::Audio,
                sample_rate: audio.sample_rate,
                duration_samples: audio.duration_samples,
                ..base
            },
            has_audio: true,
        },
        ProbedMedia::Image(image) => Probed {
            item: MediaItem {
                kind: MediaKind::Image,
                width: image.width,
                height: image.height,
                ..base
            },
            has_audio: false,
        },
    };
    Ok(probed)
}

fn file_name(path: &Path) -> String {
    path.file_name().map_or_else(
        || path.display().to_string(),
        |name| name.to_string_lossy().into_owned(),
    )
}

/// 実体が同じファイルの素材（表記の違い・hard link も同じとみなす）。
fn item_for_path(project: &Project, path: &Path) -> Option<String> {
    let key = identity_key(path);
    project
        .media_items
        .iter()
        .find(|item| {
            compare_path_identity(&identity_key(&item.media_path), &key) == PathSameness::Same
        })
        .map(|item| item.id.clone())
}

fn local_file(path: &Path) -> Result<PathBuf, AppError> {
    if !path.is_file() {
        return Err(AppError::new(format!(
            "ファイルがありません: {}",
            path.display()
        )));
    }
    std::path::absolute(path).map_err(|error| AppError::new(error.to_string()))
}

fn project_error(name: &str) -> impl Fn(ProjectError) -> ProjectError + '_ {
    move |error| ProjectError::new(format!("{name}: {error}"))
}

/// 読み込みの結果。読めなかったファイルは名前と理由を持つ。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ImportReport {
    pub imported: usize,
    pub skipped: usize,
    pub failures: Vec<String>,
}

impl ImportReport {
    pub fn message(&self) -> String {
        let mut parts = Vec::new();
        if self.imported > 0 {
            parts.push(format!("{} 件の素材を読み込みました", self.imported));
        }
        if self.skipped > 0 {
            parts.push(format!("読み込み済みの {} 件は省略しました", self.skipped));
        }
        if !self.failures.is_empty() {
            parts.push(format!(
                "{} 件は読み込めません ({})",
                self.failures.len(),
                self.failures.join(" / ")
            ));
        }
        parts.join("。")
    }
}

/// 置く素材 1 つ。パネルの素材なら `item_id`、ファイルなら調べた結果を持つ。
struct Placement {
    path: PathBuf,
    item_id: Option<String>,
    probed: Probed,
}

impl Session {
    /// ファイルをプロジェクトパネルへ読み込む（timeline へは置かない）。読めたものだけを 1 つの undo で登録する。
    /// 1 件も読めず、読み込み済みでもなければ失敗にする。
    pub fn import_media_files(
        &mut self,
        paths: &[PathBuf],
        folder_id: &str,
    ) -> Result<ImportReport, AppError> {
        let mut candidate = self.project.clone();
        let mut factory = self.id_factory();
        let mut report = ImportReport::default();
        for path in paths {
            let name = file_name(path);
            let result = local_file(path).and_then(|path| {
                if item_for_path(&candidate, &path).is_some() {
                    return Ok(false);
                }
                let mut probed = probe(&path)?;
                probed.item.id = factory.next();
                probed.item.folder_id = folder_id.to_owned();
                add_media_item(&mut candidate, probed.item)
                    .map_err(|error| AppError::new(error.to_string()))?;
                Ok(true)
            });
            match result {
                Ok(true) => report.imported += 1,
                Ok(false) => report.skipped += 1,
                Err(error) => report.failures.push(format!("{name}: {}", error.message())),
            }
        }
        self.id_serial = factory.serial;
        if report.imported > 0 {
            self.transact(|project| {
                *project = candidate;
                Ok(None)
            })?;
        }
        if report.imported == 0 && report.skipped == 0 {
            return Err(AppError::new(if report.failures.is_empty() {
                "読み込む素材がありません".to_owned()
            } else {
                report.message()
            }));
        }
        Ok(report)
    }

    /// ファイル 1 つを素材として登録し、再生ヘッドの位置へ置く（「ファイル > メディアを追加」）。
    pub fn add_media_file_at_playhead(&mut self, path: &Path) -> Result<(), AppError> {
        let path = local_file(path)?;
        let probed = probe(&path)?;
        let frame = self.playhead_frame;
        self.edit_with_ids(|project, new_id| {
            let item_id = register(project, &path, &probed, new_id)?;
            place_item_at_playhead(project, &item_id, &probed, frame, new_id)
        })
    }

    /// パネルの素材を再生ヘッドの位置へ置く（ダブルクリック・「タイムラインに追加」）。
    pub fn add_media_item_at_playhead(&mut self, item_id: &str) -> Result<(), AppError> {
        let Some(path) = self
            .project
            .media_items
            .iter()
            .find(|item| item.id == item_id)
            .map(|item| item.media_path.clone())
        else {
            return Err(AppError::new("素材がありません"));
        };
        let probed = probe(&path)?;
        let frame = self.playhead_frame;
        let item_id = item_id.to_owned();
        self.edit_with_ids(|project, new_id| {
            place_item_at_playhead(project, &item_id, &probed, frame, new_id)
        })
    }

    /// パネルの素材を、ドロップした track・位置から順に並べて置く。フォルダは飛ばす。
    pub fn place_media_items_at(
        &mut self,
        item_ids: &[String],
        track: TrackRef,
        frame: i64,
    ) -> Result<(), AppError> {
        let mut placements = Vec::new();
        for id in item_ids {
            let Some(item) = self.project.media_items.iter().find(|item| item.id == *id) else {
                continue;
            };
            let path = item.media_path.clone();
            let probed = probe(&path)
                .map_err(|error| AppError::new(format!("{}: {}", item.name, error.message())))?;
            placements.push(Placement {
                path,
                item_id: Some(id.clone()),
                probed,
            });
        }
        self.place_at(placements, track, frame)
    }

    /// OS からドロップしたファイルを素材として登録し、ドロップした track・位置から順に並べて置く。
    pub fn place_media_files_at(
        &mut self,
        paths: &[PathBuf],
        track: TrackRef,
        frame: i64,
    ) -> Result<(), AppError> {
        let mut placements = Vec::new();
        for path in paths {
            let path = local_file(path)?;
            let probed = probe(&path).map_err(|error| {
                AppError::new(format!("{}: {}", file_name(&path), error.message()))
            })?;
            placements.push(Placement {
                path,
                item_id: None,
                probed,
            });
        }
        self.place_at(placements, track, frame)
    }

    fn place_at(
        &mut self,
        placements: Vec<Placement>,
        track: TrackRef,
        frame: i64,
    ) -> Result<(), AppError> {
        if placements.is_empty() {
            return Err(AppError::new("タイムラインへ置く素材がありません"));
        }
        self.edit_with_ids(|project, new_id| {
            let mut start = frame.max(0);
            let mut first = None;
            for placement in &placements {
                let name = file_name(&placement.path);
                let item_id = match &placement.item_id {
                    Some(id) => id.clone(),
                    None => register(project, &placement.path, &placement.probed, new_id)?,
                };
                refresh_media_item(project, &item_id, &placement.probed.item)
                    .map_err(project_error(&name))?;
                let clips =
                    media_clips_for_item(project, &item_id, placement.probed.has_audio, new_id)
                        .map_err(project_error(&name))?;
                let placed =
                    place_media_at_drop(project, clips.primary, clips.linked_audio, track, start)
                        .map_err(project_error(&name))?;
                let Some(index) = placed else {
                    return Err(ProjectError::new("置いた clip がありません"));
                };
                let Some(clip) = project.timeline_clips.get(index) else {
                    return Err(ProjectError::new("置いた clip がありません"));
                };
                start += timeline_clip_duration(project, clip)?;
                first.get_or_insert(index);
            }
            Ok(first)
        })
    }

    /// プロジェクトパネルにフォルダを作る。名前は Project 全体で未使用の番号にする。作ったフォルダの ID を返す。
    pub fn create_media_folder(&mut self, parent_id: &str) -> Result<String, AppError> {
        let name = (1..)
            .map(|number| format!("フォルダ {number:02}"))
            .find(|name| {
                !self
                    .project
                    .media_folders
                    .iter()
                    .any(|folder| folder.name == *name)
            })
            .unwrap_or_default();
        let mut factory = self.id_factory();
        let id = factory.next();
        self.id_serial = factory.serial;
        let folder = MediaFolder {
            id: id.clone(),
            name,
            parent_id: parent_id.to_owned(),
        };
        self.transact(|project| add_media_folder(project, folder).map(|()| None))?;
        Ok(id)
    }

    pub fn rename_media_entry(&mut self, entry_id: &str, name: &str) -> Result<(), AppError> {
        let name = name.trim().to_owned();
        self.transact(|project| rename_media_bin_entry(project, entry_id, name).map(|()| None))
    }

    /// `folder_id` が空なら root へ移す。
    pub fn move_media_entries(
        &mut self,
        entry_ids: &[String],
        folder_id: &str,
    ) -> Result<(), AppError> {
        self.transact(|project| {
            move_media_bin_entries(project, entry_ids, folder_id).map(|()| None)
        })
    }

    /// 削除すると一緒に消える timeline の clip の数（確認の文に使う）。
    pub fn media_removal_clip_count(&self, entry_ids: &[String]) -> Result<usize, AppError> {
        plan_media_bin_removal(&self.project, entry_ids)
            .map(|plan| plan.clip_ids.len())
            .map_err(|error| AppError::new(error.to_string()))
    }

    /// 素材とフォルダを消す。使っている timeline の clip も一緒に消す（1 つの undo）。
    pub fn remove_media_entries(&mut self, entry_ids: &[String]) -> Result<(), AppError> {
        self.transact(|project| remove_media_bin_entries(project, entry_ids).map(|_| None))
    }
}

/// 素材を登録し、その ID を返す。実体が同じファイルの素材が既にあれば、それを使う。
fn register(
    project: &mut Project,
    path: &Path,
    probed: &Probed,
    new_id: &mut dyn FnMut() -> String,
) -> Result<String, ProjectError> {
    if let Some(id) = item_for_path(project, path) {
        return Ok(id);
    }
    let mut item = probed.item.clone();
    item.id = new_id();
    let id = item.id.clone();
    add_media_item(project, item).map_err(|error| {
        ProjectError::new(format!("素材をプロジェクトへ登録できません: {error}"))
    })?;
    Ok(id)
}

fn place_item_at_playhead(
    project: &mut Project,
    item_id: &str,
    probed: &Probed,
    frame: i64,
    new_id: &mut dyn FnMut() -> String,
) -> Result<Option<usize>, ProjectError> {
    refresh_media_item(project, item_id, &probed.item)?;
    let clips = media_clips_for_item(project, item_id, probed.has_audio, new_id)?;
    match probed.item.kind {
        MediaKind::Video => place_video_media_at(project, clips.primary, clips.linked_audio, frame),
        MediaKind::Audio => place_audio_clip_at(project, clips.primary, frame),
        MediaKind::Image => place_still_clip_at(project, clips.primary, frame),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mvm_media::{DecodedImage, ProbedAudio, ProbedVideo};

    #[test]
    fn a_probed_video_keeps_its_reduced_rate_and_sound() {
        let probed = ProbedMedia::Video(ProbedVideo {
            width: 1920,
            height: 1080,
            fps_num: 60_000,
            fps_den: 2_000,
            frame_count: 300,
            has_audio: true,
            sar_num: 1,
            sar_den: 1,
            duration_sec: 10.0,
        });
        let result = probed_item(Path::new("C:/media/clip.mp4"), &probed).expect("動画");
        assert_eq!(result.item.kind, MediaKind::Video);
        assert_eq!(result.item.fps, Rational::new(30, 1));
        assert_eq!(result.item.frame_count, 300);
        assert_eq!(result.item.name, "clip.mp4");
        assert!(result.has_audio);
    }

    #[test]
    fn a_probed_video_with_a_zero_rate_is_rejected() {
        let probed = ProbedMedia::Video(ProbedVideo {
            width: 16,
            height: 16,
            fps_num: 30,
            fps_den: 0,
            frame_count: 2,
            has_audio: false,
            sar_num: 1,
            sar_den: 1,
            duration_sec: 1.0,
        });
        assert!(probed_item(Path::new("bad.mp4"), &probed).is_err());
    }

    #[test]
    fn probed_audio_and_images_carry_only_their_own_values() {
        let audio = probed_item(
            Path::new("a.wav"),
            &ProbedMedia::Audio(ProbedAudio {
                sample_rate: 48_000,
                duration_samples: 96_000,
            }),
        )
        .expect("音声");
        assert_eq!(audio.item.kind, MediaKind::Audio);
        assert_eq!(audio.item.duration_samples, 96_000);
        assert_eq!(audio.item.width, 0);
        let image = probed_item(
            Path::new("i.png"),
            &ProbedMedia::Image(DecodedImage {
                width: 640,
                height: 480,
                rgba: Vec::new(),
                orientation: 1,
            }),
        )
        .expect("画像");
        assert_eq!(image.item.kind, MediaKind::Image);
        assert_eq!((image.item.width, image.item.height), (640, 480));
        assert!(!image.has_audio);
    }

    #[test]
    fn folders_get_the_first_unused_number_across_the_project() {
        let mut session = Session::new();
        let first = session.create_media_folder("").expect("1 つ目");
        let second = session.create_media_folder(&first).expect("2 つ目");
        let names: Vec<_> = session
            .project()
            .media_folders
            .iter()
            .map(|folder| folder.name.clone())
            .collect();
        assert_eq!(names, ["フォルダ 01", "フォルダ 02"]);
        assert_ne!(first, second);
        session
            .rename_media_entry(&first, "  素材  ")
            .expect("名前");
        assert_eq!(session.project().media_folders[0].name, "素材");
        assert!(session.rename_media_entry(&first, "   ").is_err());
        session
            .move_media_entries(std::slice::from_ref(&second), "")
            .expect("root へ");
        assert_eq!(session.project().media_folders[1].parent_id, "");
        session.undo().expect("移動を戻す");
        assert_eq!(session.project().media_folders[1].parent_id, first);
    }

    #[test]
    fn missing_files_are_reported_and_nothing_is_imported() {
        let mut session = Session::new();
        let error = session
            .import_media_files(&[PathBuf::from("Z:/無い/素材.mp4")], "")
            .expect_err("無いファイル");
        assert!(error.message().contains("素材.mp4"), "{}", error.message());
        assert!(session.project().media_items.is_empty());
        assert!(!session.can_undo());
    }

    #[test]
    fn removing_a_folder_counts_and_removes_its_contents() {
        let mut session = Session::new();
        let folder = session.create_media_folder("").expect("フォルダ");
        assert_eq!(
            session
                .media_removal_clip_count(std::slice::from_ref(&folder))
                .expect("数"),
            0
        );
        session.remove_media_entries(&[folder]).expect("削除");
        assert!(session.project().media_folders.is_empty());
        assert!(
            session
                .media_removal_clip_count(&["無い".to_owned()])
                .is_err()
        );
    }
}
