//! プロジェクトパネル（素材とフォルダ）の検証と編集。
//!
//! どの操作も candidate を検証してから置き換え、失敗時は Project を変えない。プロジェクトパネルを素材の唯一の
//! 出どころにするため、timeline の動画・音声・画像の clip はパネルの素材を ID で指す。

use std::collections::{BTreeSet, HashMap, HashSet};

use crate::error::{ProjectResult, fail};
use crate::model::{
    MediaFolder, MediaItem, MediaKind, Project, TimelineClipKind, is_canonical_frame_rate,
};
use crate::path_identity::canonical_path_key;
use crate::timeline::finalize_timeline_candidate;

fn validate_item_values(item: &MediaItem) -> ProjectResult<()> {
    let label = format!("素材 \"{}\" ", item.name);
    let no_video_values = item.fps == mvm_core::Rational::new(0, 1)
        && item.frame_count == 0
        && item.width == 0
        && item.height == 0;
    let no_audio_values = item.sample_rate == 0 && item.duration_samples == 0;
    match item.kind {
        MediaKind::Video => {
            if !is_canonical_frame_rate(item.fps)
                || item.frame_count <= 0
                || item.width <= 0
                || item.height <= 0
            {
                return fail(format!("{label}の動画情報 (fps / 尺 / 解像度) が不正です"));
            }
            if !no_audio_values {
                return fail(format!("{label}は動画なのに音声専用の値を持っています"));
            }
        }
        MediaKind::Audio => {
            if item.sample_rate <= 0 || item.duration_samples <= 0 {
                return fail(format!("{label}の音声情報 (sample rate / 尺) が不正です"));
            }
            if !no_video_values {
                return fail(format!("{label}は音声なのに映像の値を持っています"));
            }
        }
        MediaKind::Image => {
            if item.width <= 0 || item.height <= 0 {
                return fail(format!("{label}の解像度が不正です"));
            }
            let no_duration = item.fps == mvm_core::Rational::new(0, 1)
                && item.frame_count == 0
                && no_audio_values;
            if !no_duration {
                return fail(format!("{label}は静止画なのに尺を持っています"));
            }
        }
    }
    Ok(())
}

pub fn find_media_item<'a>(project: &'a Project, item_id: &str) -> Option<&'a MediaItem> {
    project.media_items.iter().find(|item| item.id == item_id)
}

pub fn find_media_folder<'a>(project: &'a Project, folder_id: &str) -> Option<&'a MediaFolder> {
    project
        .media_folders
        .iter()
        .find(|folder| folder.id == folder_id)
}

/// folder・item の id の一意性、親子関係（循環なし）、種別ごとの値と、timeline clip の素材参照を検査する。
/// Project JSON の読み書きと全ての編集の確定がこれを通る。
pub fn validate_media_bin(project: &Project) -> ProjectResult<()> {
    let mut ids = HashSet::new();
    for folder in &project.media_folders {
        if folder.id.is_empty() || !ids.insert(folder.id.as_str()) {
            return fail(format!(
                "素材フォルダの id が空または重複しています: {}",
                folder.id
            ));
        }
        if folder.name.is_empty() {
            return fail("素材フォルダの名前が空です");
        }
    }
    let parents: HashMap<&str, &str> = project
        .media_folders
        .iter()
        .map(|folder| (folder.id.as_str(), folder.parent_id.as_str()))
        .collect();
    for folder in &project.media_folders {
        if !folder.parent_id.is_empty() && !parents.contains_key(folder.parent_id.as_str()) {
            return fail(format!(
                "素材フォルダ \"{}\" の親フォルダがありません",
                folder.name
            ));
        }
    }
    // 全 folder の親が実在することを確かめてから辿る。folder 数を超えて続くなら循環している。
    for folder in &project.media_folders {
        let mut cursor = folder.parent_id.as_str();
        let mut depth = 0;
        while !cursor.is_empty() {
            if cursor == folder.id || depth > project.media_folders.len() {
                return fail(format!(
                    "素材フォルダ \"{}\" の親子関係が循環しています",
                    folder.name
                ));
            }
            cursor = parents.get(cursor).copied().unwrap_or("");
            depth += 1;
        }
    }
    // 重複は表記上の key で判定する。検証の結果をファイルの有無や実体に依存させると、保存済みの Project が
    // disk 側の変化だけで開けなくなる。
    let mut path_keys = HashSet::new();
    for item in &project.media_items {
        if item.id.is_empty() || !ids.insert(item.id.as_str()) {
            return fail(format!("素材の id が空または重複しています: {}", item.id));
        }
        if item.name.is_empty() || item.media_path.as_os_str().is_empty() {
            return fail("素材の名前または media_path が空です");
        }
        if !item.folder_id.is_empty() && !parents.contains_key(item.folder_id.as_str()) {
            return fail(format!("素材 \"{}\" のフォルダがありません", item.name));
        }
        let Some(key) = canonical_path_key(&item.media_path) else {
            return fail(format!(
                "素材 \"{}\" の media_path を解決できません",
                item.name
            ));
        };
        if !path_keys.insert(key) {
            return fail(format!("同じファイルの素材が重複しています: {}", item.name));
        }
        validate_item_values(item)?;
    }
    validate_media_references(project)
}

/// 動画・音声・画像の clip がプロジェクトパネルの素材を media_item_id で指し、その素材と種別・ファイルが
/// 一致すること。文字・数式・Manim の clip は素材を指さないこと。
pub fn validate_media_references(project: &Project) -> ProjectResult<()> {
    // clip ごとに素材列を走査しない。素材の表記上の key も素材ごとに 1 回だけ作る。
    let mut items: HashMap<&str, (&MediaItem, Option<Option<String>>)> = HashMap::new();
    for item in &project.media_items {
        items.entry(item.id.as_str()).or_insert((item, None));
    }
    for clip in &project.timeline_clips {
        if !clip.kind.uses_media_item() {
            if !clip.media_item_id.is_empty() {
                return fail(format!(
                    "文字・数式・Manim の clip は素材を参照できません: {}",
                    clip.name
                ));
            }
            continue;
        }
        let Some((item, key)) = items.get_mut(clip.media_item_id.as_str()) else {
            return fail(format!(
                "clip \"{}\" の素材がプロジェクトパネルにありません",
                clip.name
            ));
        };
        // 映像のある素材からは音声 clip も作れる（リンクした音声）。
        let kind_matches = matches!(
            (clip.kind, item.kind),
            (TimelineClipKind::Video, MediaKind::Video)
                | (TimelineClipKind::Audio, MediaKind::Audio | MediaKind::Video)
                | (TimelineClipKind::Image, MediaKind::Image)
        );
        if !kind_matches {
            return fail(format!(
                "clip \"{}\" の種別が素材の種別と一致しません",
                clip.name
            ));
        }
        // clip は素材と同じファイルを指す。表記で比べ、ファイルの有無には依存させない。
        let item_key = key.get_or_insert_with(|| canonical_path_key(&item.media_path));
        if canonical_path_key(&clip.media_path) != *item_key {
            return fail(format!(
                "clip \"{}\" のファイルが素材と一致しません",
                clip.name
            ));
        }
    }
    Ok(())
}

fn commit(project: &mut Project, candidate: Project) -> ProjectResult<()> {
    validate_media_bin(&candidate)?;
    *project = candidate;
    Ok(())
}

pub fn add_media_folder(project: &mut Project, folder: MediaFolder) -> ProjectResult<()> {
    let mut candidate = project.clone();
    candidate.media_folders.push(folder);
    commit(project, candidate)
}

/// 素材を追加する。同じ表記の path の素材があれば失敗する。実体での重複（大文字小文字・junction など）は
/// 呼び出し側が platform の file identity で先に確かめる。
pub fn add_media_item(project: &mut Project, item: MediaItem) -> ProjectResult<()> {
    let key = canonical_path_key(&item.media_path);
    if key.is_some()
        && project
            .media_items
            .iter()
            .any(|existing| canonical_path_key(&existing.media_path) == key)
    {
        return fail(format!("既に読み込み済みの素材です: {}", item.name));
    }
    let mut candidate = project.clone();
    candidate.media_items.push(item);
    commit(project, candidate)
}

/// timeline のどこかの clip が使っている素材の id（パネルの「使用中」表示用）。
pub fn media_items_in_use(project: &Project) -> BTreeSet<String> {
    project
        .timeline_clips
        .iter()
        .filter(|clip| !clip.media_item_id.is_empty())
        .map(|clip| clip.media_item_id.clone())
        .collect()
}

/// 素材の技術的な値（種別・fps・尺・解像度・sample rate）を、調べ直した値へ置き換える。id・名前・フォルダ・
/// ファイルは変えない。ただし timeline の clip が使っている素材の時間軸（種別・fps・尺・sample rate）が変わって
/// いれば失敗する（使っている clip の素材範囲は元の時間軸で決めてあるため）。解像度だけの変化は更新する。
pub fn refresh_media_item(
    project: &mut Project,
    item_id: &str,
    probed: &MediaItem,
) -> ProjectResult<()> {
    let in_use = media_items_in_use(project).contains(item_id);
    let mut candidate = project.clone();
    let Some(item) = candidate
        .media_items
        .iter_mut()
        .find(|item| item.id == item_id)
    else {
        return fail("調べ直す素材がありません");
    };
    let timing_changed = item.kind != probed.kind
        || item.fps != probed.fps
        || item.frame_count != probed.frame_count
        || item.sample_rate != probed.sample_rate
        || item.duration_samples != probed.duration_samples;
    if timing_changed && in_use {
        return fail(format!(
            "タイムラインで使用中の素材 \"{}\" のファイルが別の長さ・fps・種類のものに差し替わっています",
            item.name
        ));
    }
    item.kind = probed.kind;
    item.fps = probed.fps;
    item.frame_count = probed.frame_count;
    item.width = probed.width;
    item.height = probed.height;
    item.sample_rate = probed.sample_rate;
    item.duration_samples = probed.duration_samples;
    commit(project, candidate)
}

pub fn rename_media_bin_entry(
    project: &mut Project,
    entry_id: &str,
    name: String,
) -> ProjectResult<()> {
    if name.is_empty() {
        return fail("名前を空にはできません");
    }
    let mut candidate = project.clone();
    if let Some(folder) = candidate
        .media_folders
        .iter_mut()
        .find(|folder| folder.id == entry_id)
    {
        folder.name = name;
    } else if let Some(item) = candidate
        .media_items
        .iter_mut()
        .find(|item| item.id == entry_id)
    {
        item.name = name;
    } else {
        return fail("名前を変更する素材がありません");
    }
    commit(project, candidate)
}

/// target_folder_id が空なら root へ移す。folder を自分自身や子孫へは移せない。
pub fn move_media_bin_entries(
    project: &mut Project,
    entry_ids: &[String],
    target_folder_id: &str,
) -> ProjectResult<()> {
    if entry_ids.is_empty() {
        return fail("移動する素材がありません");
    }
    if !target_folder_id.is_empty() && find_media_folder(project, target_folder_id).is_none() {
        return fail("移動先のフォルダがありません");
    }
    let mut candidate = project.clone();
    for id in entry_ids {
        let mut found = false;
        if let Some(folder) = candidate
            .media_folders
            .iter_mut()
            .find(|folder| folder.id == *id)
        {
            // 自分自身や子孫の下へ入れると循環する。validate でも弾けるが、理由を明示する。
            let mut cursor = target_folder_id;
            let mut depth = 0;
            while !cursor.is_empty() && depth <= project.media_folders.len() {
                if cursor == id {
                    return fail("フォルダをそれ自身の中へは移動できません");
                }
                cursor = find_media_folder(project, cursor)
                    .map_or("", |folder| folder.parent_id.as_str());
                depth += 1;
            }
            folder.parent_id = target_folder_id.to_owned();
            found = true;
        }
        if let Some(item) = candidate.media_items.iter_mut().find(|item| item.id == *id) {
            item.folder_id = target_folder_id.to_owned();
            found = true;
        }
        if !found {
            return fail("移動する素材がありません");
        }
    }
    commit(project, candidate)
}

/// 削除で消えるもの。folder は子孫と中身ごと、素材はそれを参照する timeline clip（とそのリンク相手）ごと消す。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MediaBinRemovalPlan {
    pub folder_ids: BTreeSet<String>,
    pub item_ids: BTreeSet<String>,
    /// timeline の並び順。
    pub clip_ids: Vec<String>,
}

/// 消える clip は media_item_id で決まる（ファイルの実体は調べない）。
pub fn plan_media_bin_removal(
    project: &Project,
    entry_ids: &[String],
) -> ProjectResult<MediaBinRemovalPlan> {
    if entry_ids.is_empty() {
        return fail("削除する素材がありません");
    }
    let mut plan = MediaBinRemovalPlan::default();
    for id in entry_ids {
        if find_media_folder(project, id).is_some() {
            plan.folder_ids.insert(id.clone());
        } else if find_media_item(project, id).is_some() {
            plan.item_ids.insert(id.clone());
        } else {
            return fail("削除する素材がありません");
        }
    }
    // 指定 folder の子孫を広げて、削除対象の folder を確定させる。
    let mut grew = true;
    while grew {
        grew = false;
        for folder in &project.media_folders {
            if plan.folder_ids.contains(&folder.parent_id)
                && plan.folder_ids.insert(folder.id.clone())
            {
                grew = true;
            }
        }
    }
    for item in &project.media_items {
        if plan.folder_ids.contains(&item.folder_id) {
            plan.item_ids.insert(item.id.clone());
        }
    }
    let mut link_groups = HashSet::new();
    let mut clip_ids = HashSet::new();
    for clip in &project.timeline_clips {
        if clip.media_item_id.is_empty() || !plan.item_ids.contains(&clip.media_item_id) {
            continue;
        }
        clip_ids.insert(clip.id.as_str());
        if clip.is_linked() {
            link_groups.insert(clip.link_group_id.as_str());
        }
    }
    // リンク相手も消す。片側だけ残すと clip の削除と挙動が食い違う。
    plan.clip_ids = project
        .timeline_clips
        .iter()
        .filter(|clip| {
            clip_ids.contains(clip.id.as_str())
                || (clip.is_linked() && link_groups.contains(clip.link_group_id.as_str()))
        })
        .map(|clip| clip.id.clone())
        .collect();
    Ok(plan)
}

/// plan_media_bin_removal の内容を 1 つの candidate として適用する。消した timeline clip の ID を返す。
pub fn remove_media_bin_entries(
    project: &mut Project,
    entry_ids: &[String],
) -> ProjectResult<Vec<String>> {
    let plan = plan_media_bin_removal(project, entry_ids)?;
    let mut candidate = project.clone();
    candidate
        .media_folders
        .retain(|folder| !plan.folder_ids.contains(&folder.id));
    candidate
        .media_items
        .retain(|item| !plan.item_ids.contains(&item.id));
    candidate
        .timeline_clips
        .retain(|clip| !plan.clip_ids.contains(&clip.id));
    finalize_timeline_candidate(&mut candidate)?;
    commit(project, candidate)?;
    Ok(plan.clip_ids)
}
