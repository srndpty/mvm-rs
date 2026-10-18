//! 字幕（1 本の字幕 track）。検証・編集・clip とのリンク・SRT の読み書き。

use std::collections::{HashMap, HashSet};

use mvm_core::Rational;

use crate::error::{ProjectError, ProjectResult, fail};
use crate::model::{Project, SubtitleCue, SubtitleTrack, TimelineClip};

/// 文字起こしの区間（ミリ秒）。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SubtitleTimedText {
    pub start_ms: i64,
    pub end_ms: i64,
    pub content: String,
}

/// value × num / den を四捨五入する（0 以上）。
fn convert(value: i64, num: i128, den: i128) -> ProjectResult<i64> {
    if value < 0 || num <= 0 || den <= 0 {
        return fail("字幕の時刻または時間基準が不正です");
    }
    let product = i128::from(value)
        .checked_mul(num)
        .and_then(|product| product.checked_add(den / 2))
        .ok_or_else(|| ProjectError::new("字幕の時間換算が範囲外です"))?;
    i64::try_from(product / den).map_err(|_| ProjectError::new("字幕の時刻が範囲外です"))
}

/// ミリ秒 → timeline frame（最近傍へ丸め、同率は後方へ送る）。
pub fn subtitle_time_to_frame(milliseconds: i64, fps: Rational) -> ProjectResult<i64> {
    convert(
        milliseconds,
        i128::from(fps.num),
        i128::from(fps.den) * 1000,
    )
}

/// timeline frame → ミリ秒（最近傍へ丸め、同率は後方へ送る）。
pub fn subtitle_frame_to_time(frame: i64, fps: Rational) -> ProjectResult<i64> {
    convert(frame, i128::from(fps.den) * 1000, i128::from(fps.num))
}

/// 本文に使える文字か（改行・tab 以外の制御文字を許さない）と、空白以外の文字があるか。
fn content_shape(text: &str) -> (bool, bool) {
    let mut non_blank = false;
    for character in text.chars() {
        let code = u32::from(character);
        if code < 32 && !matches!(character, '\n' | '\t' | '\r') || code == 127 {
            return (false, non_blank);
        }
        let space = if code < 128 {
            character.is_ascii_whitespace() || character == '\u{0b}'
        } else {
            matches!(
                code,
                0x85 | 0xa0 | 0x1680 | 0x2000..=0x200a | 0x2028 | 0x2029 | 0x202f | 0x205f | 0x3000
            )
        };
        non_blank |= !space;
    }
    (true, non_blank)
}

fn is_color(text: &str) -> bool {
    text.len() == 9
        && text.starts_with('#')
        && text.bytes().skip(1).all(|byte| byte.is_ascii_hexdigit())
}

fn sort_cues(track: &mut SubtitleTrack) {
    track.cues.sort_by_key(|cue| cue.start_frame);
}

fn has_overlap(cues: &[SubtitleCue]) -> bool {
    cues.windows(2)
        .any(|pair| pair[1].start_frame < pair[0].end_frame)
}

/// 字幕の書式・ID・区間・並び順・リンク先を検証する。
pub fn validate_subtitles(project: &Project) -> ProjectResult<()> {
    let Some(subtitles) = &project.subtitles else {
        return Ok(());
    };
    let style = &subtitles.style;
    let style_valid = !style.font_family.is_empty()
        && (1..=project.output_height).contains(&style.font_size)
        && (0..=32).contains(&style.outline_width)
        && is_color(&style.color)
        && is_color(&style.outline_color)
        && is_color(&style.background_color)
        && matches!(style.alignment.as_str(), "left" | "center" | "right")
        && style.side_margin.is_finite()
        && (0.0..0.5).contains(&style.side_margin)
        && style.bottom_margin.is_finite()
        && (0.0..1.0).contains(&style.bottom_margin);
    if !style_valid {
        return fail("字幕の共通書式が不正です");
    }
    let clip_ids: HashSet<&str> = project
        .timeline_clips
        .iter()
        .map(|clip| clip.id.as_str())
        .collect();
    let mut ids = HashSet::new();
    let mut previous_end = 0;
    for cue in &subtitles.cues {
        let (valid_text, non_blank) = content_shape(&cue.content);
        if cue.id.is_empty()
            || !ids.insert(cue.id.as_str())
            || cue.start_frame < previous_end
            || cue.end_frame <= cue.start_frame
            || cue.content.is_empty()
            || !valid_text
            || !non_blank
        {
            return fail(format!(
                "字幕のID・本文・区間または並び順が不正です: {}",
                cue.id
            ));
        }
        if !cue.link_clip_id.is_empty() && !clip_ids.contains(cue.link_clip_id.as_str()) {
            return fail(format!("字幕のリンク先 clip がありません: {}", cue.id));
        }
        previous_end = cue.end_frame;
    }
    Ok(())
}

/// 文字起こしの結果を字幕にする。clip の速度と配置位置で timeline frame へ換算する。
pub fn subtitle_cues_from_timed_text(
    segments: &[SubtitleTimedText],
    fps: Rational,
    speed: Rational,
    offset_frame: i64,
    new_id: &mut dyn FnMut() -> String,
) -> ProjectResult<Vec<SubtitleCue>> {
    if !fps.is_positive() || !speed.is_positive() || offset_frame < 0 || segments.is_empty() {
        return fail("認識結果の時間基準または件数が不正です");
    }
    // 設定済みレート・速度の範囲で積を制限し、128 bit の積の溢れも防ぐ。
    const LIMIT: i64 = 1_000_000;
    if fps.num > LIMIT || fps.den > LIMIT || speed.num > LIMIT || speed.den > LIMIT {
        return fail("認識結果の時間基準が範囲外です");
    }
    let num = i128::from(fps.num) * i128::from(speed.den);
    let den = i128::from(fps.den) * i128::from(speed.num) * 1000;
    let mut cues = Vec::with_capacity(segments.len());
    for segment in segments {
        let invalid = || ProjectError::new("認識結果の時刻が不正です");
        if segment.end_ms <= segment.start_ms {
            return Err(invalid());
        }
        let start = convert(segment.start_ms, num, den).map_err(|_| invalid())?;
        let end = convert(segment.end_ms, num, den).map_err(|_| invalid())?;
        let (Some(start), Some(end)) = (
            start.checked_add(offset_frame),
            end.checked_add(offset_frame),
        ) else {
            return fail("字幕の配置時刻が範囲外です");
        };
        cues.push(SubtitleCue {
            id: new_id(),
            start_frame: start,
            end_frame: end,
            content: segment.content.clone(),
            link_clip_id: String::new(),
        });
    }
    check_cues(&cues)?;
    Ok(cues)
}

/// 既定の Project の字幕として検証する（書式・出力の高さは既定値）。
fn check_cues(cues: &[SubtitleCue]) -> ProjectResult<()> {
    let check = Project {
        subtitles: Some(SubtitleTrack {
            cues: cues.to_vec(),
            ..SubtitleTrack::default()
        }),
        ..Project::default()
    };
    validate_subtitles(&check)
}

/// frame を含む表示中の字幕。字幕が非表示なら `None`。
pub fn active_subtitle_at(project: &Project, frame: i64) -> Option<&SubtitleCue> {
    let subtitles = project.subtitles.as_ref().filter(|track| track.visible)?;
    let after = subtitles
        .cues
        .partition_point(|cue| cue.start_frame <= frame);
    let cue = subtitles.cues.get(after.checked_sub(1)?)?;
    subtitle_contains_frame(cue, frame).then_some(cue)
}

pub fn subtitle_contains_frame(cue: &SubtitleCue, frame: i64) -> bool {
    (cue.start_frame..cue.end_frame).contains(&frame)
}

fn commit(project: &mut Project, candidate: Project) -> ProjectResult<()> {
    validate_subtitles(&candidate)?;
    *project = candidate;
    Ok(())
}

/// 認識結果と SRT の適用。同じ検証・並べ替えを通す。
pub fn apply_subtitle_cues(
    project: &mut Project,
    cues: Vec<SubtitleCue>,
    replace: bool,
) -> ProjectResult<()> {
    let mut candidate = project.clone();
    let track = candidate
        .subtitles
        .get_or_insert_with(SubtitleTrack::default);
    if replace {
        track.cues.clear();
    }
    track.cues.extend(cues);
    sort_cues(track);
    commit(project, candidate)
}

/// 同じ ID の字幕を置き換える（無ければ追加する）。
pub fn edit_subtitle(project: &mut Project, cue: SubtitleCue) -> ProjectResult<()> {
    let mut candidate = project.clone();
    let track = candidate
        .subtitles
        .get_or_insert_with(SubtitleTrack::default);
    match track.cues.iter_mut().find(|existing| existing.id == cue.id) {
        Some(existing) => *existing = cue,
        None => track.cues.push(cue),
    }
    sort_cues(track);
    commit(project, candidate)
}

pub fn delete_subtitle(project: &mut Project, id: &str) -> ProjectResult<()> {
    let mut candidate = project.clone();
    let removed = candidate.subtitles.as_mut().is_some_and(|track| {
        let before = track.cues.len();
        track.cues.retain(|cue| cue.id != id);
        track.cues.len() != before
    });
    if !removed {
        return fail("削除する字幕がありません");
    }
    commit(project, candidate)
}

/// 本文を前後に分ける。`text_break` は分ける文字位置（Unicode の文字数、両端を除く）。省略すると、
/// `ratio` の位置に最も近い句読点・空白・改行の直後で分け、それも無ければ比率の位置で分ける。
/// 1 文字の本文や、片方が空白だけになる位置では分けず、両方に元の本文を残す。
pub fn split_subtitle_text(
    content: &str,
    ratio: f64,
    text_break: Option<usize>,
) -> (String, String) {
    let starts: Vec<usize> = content.char_indices().map(|(index, _)| index).collect();
    let count = starts.len();
    if count < 2 {
        return (content.to_owned(), content.to_owned());
    }
    let at = match text_break {
        Some(at) if at > 0 && at < count => at,
        _ => {
            let target =
                ((ratio.clamp(0.0, 1.0) * count as f64).round() as usize).clamp(1, count - 1);
            const BREAKS: [char; 14] = [
                '、', '。', '，', '．', '！', '？', '!', '?', '…', ' ', '　', '\n', ',', '.',
            ];
            let characters: Vec<char> = content.chars().collect();
            (1..count)
                .filter(|index| BREAKS.contains(&characters[index - 1]))
                .min_by_key(|index| index.abs_diff(target))
                .unwrap_or(target)
        }
    };
    let trim = |text: &str| text.trim_matches([' ', '\n', '\r', '\t']).to_owned();
    let left = trim(&content[..starts[at]]);
    let right = trim(&content[starts[at]..]);
    if left.is_empty() || right.is_empty() {
        return (content.to_owned(), content.to_owned());
    }
    (left, right)
}

/// frame で字幕の区間を分け、本文も前後に分ける。
pub fn split_subtitle(
    project: &mut Project,
    id: &str,
    frame: i64,
    new_id: &str,
    text_break: Option<usize>,
) -> ProjectResult<()> {
    let mut candidate = project.clone();
    let Some(track) = candidate.subtitles.as_mut() else {
        return fail("字幕の区間内で分割してください");
    };
    let Some(cue) = track
        .cues
        .iter_mut()
        .find(|cue| cue.id == id && cue.start_frame < frame && frame < cue.end_frame)
    else {
        return fail("字幕の区間内で分割してください");
    };
    let ratio = (frame - cue.start_frame) as f64 / (cue.end_frame - cue.start_frame) as f64;
    let (left, right_content) = split_subtitle_text(&cue.content, ratio, text_break);
    let right = SubtitleCue {
        id: new_id.to_owned(),
        start_frame: frame,
        content: right_content,
        ..cue.clone()
    };
    cue.content = left;
    cue.end_frame = frame;
    track.cues.push(right);
    sort_cues(track);
    commit(project, candidate)
}

/// 次の字幕と結合する（本文は改行でつなぐ）。
pub fn merge_subtitle(project: &mut Project, id: &str) -> ProjectResult<()> {
    let mut candidate = project.clone();
    let Some(track) = candidate.subtitles.as_mut() else {
        return fail("結合する次の字幕がありません");
    };
    let Some(index) = track
        .cues
        .iter()
        .position(|cue| cue.id == id)
        .filter(|index| index + 1 < track.cues.len())
    else {
        return fail("結合する次の字幕がありません");
    };
    let next = track.cues.remove(index + 1);
    let cue = &mut track.cues[index];
    cue.end_frame = next.end_frame;
    cue.content.push('\n');
    cue.content.push_str(&next.content);
    commit(project, candidate)
}

/// 字幕へ時間の挿入・削除を適用する（[start, start + removed) を消して inserted を挿入する）。
/// 境界を跨ぐ字幕は境界で分け、右側には `new_id` の ID を付ける。
pub fn edit_subtitle_time(
    project: &mut Project,
    start: i64,
    removed: i64,
    inserted: i64,
    new_id: &mut dyn FnMut() -> String,
) -> ProjectResult<()> {
    let Some(end) = (start >= 0 && removed >= 0 && inserted >= 0)
        .then(|| start.checked_add(removed))
        .flatten()
    else {
        return fail("字幕の時間編集区間が不正です");
    };
    let Some(subtitles) = &project.subtitles else {
        return Ok(());
    };
    if removed == 0 && inserted == 0 {
        return Ok(());
    }
    let mut output = Vec::with_capacity(subtitles.cues.len());
    for cue in &subtitles.cues {
        if cue.start_frame < start {
            output.push(SubtitleCue {
                end_frame: cue.end_frame.min(start),
                ..cue.clone()
            });
        }
        if cue.end_frame > end {
            let mut right = cue.clone();
            right.start_frame = cue.start_frame.max(end);
            if right.start_frame != cue.start_frame && cue.start_frame < start {
                right.id = new_id();
            }
            let shift = |frame: i64| i128::from(frame) - i128::from(removed) + i128::from(inserted);
            let (Ok(new_start), Ok(new_end)) = (
                i64::try_from(shift(right.start_frame)),
                i64::try_from(shift(right.end_frame)),
            ) else {
                return fail("字幕の移動先が範囲外です");
            };
            right.start_frame = new_start;
            right.end_frame = new_end;
            output.push(right);
        }
    }
    let mut candidate = project.clone();
    if let Some(track) = candidate.subtitles.as_mut() {
        track.cues = output;
    }
    commit(project, candidate)
}

// --- clip とのリンク（timeline 編集の候補に対して呼ぶ。検証は呼び出し側の finalize で行う） ---

/// `clip_ids` のいずれかにリンクした字幕の最小開始 frame。
pub fn linked_subtitle_start(project: &Project, clip_ids: &HashSet<String>) -> Option<i64> {
    project
        .subtitles
        .as_ref()?
        .cues
        .iter()
        .filter(|cue| !cue.link_clip_id.is_empty() && clip_ids.contains(&cue.link_clip_id))
        .map(|cue| cue.start_frame)
        .min()
}

/// `clip_ids` にリンクした字幕を `delta` だけ横へ動かす。他の字幕と重なる場合は失敗する。
pub fn shift_linked_subtitles(
    project: &mut Project,
    clip_ids: &HashSet<String>,
    delta: i64,
) -> ProjectResult<()> {
    let Some(track) = project.subtitles.as_mut() else {
        return Ok(());
    };
    if delta == 0 {
        return Ok(());
    }
    let mut moved = false;
    for cue in &mut track.cues {
        if cue.link_clip_id.is_empty() || !clip_ids.contains(&cue.link_clip_id) {
            continue;
        }
        let (Some(start), Some(end)) = (
            cue.start_frame.checked_add(delta),
            cue.end_frame.checked_add(delta),
        ) else {
            return fail("リンクした字幕の移動先が範囲外です");
        };
        if start < 0 {
            return fail("リンクした字幕の移動先が範囲外です");
        }
        cue.start_frame = start;
        cue.end_frame = end;
        moved = true;
    }
    if !moved {
        return Ok(());
    }
    sort_cues(track);
    // 字幕は 1 本の track で重なりを持てない。どちらを優先するか決められないので、
    // 部分的に動かさず移動全体を拒否する。
    if has_overlap(&track.cues) {
        return fail("リンクした字幕が他の字幕と重なるため移動できません");
    }
    Ok(())
}

/// clip を削除・カットしたときに、その clip にリンクした字幕を消す。
pub fn erase_linked_subtitles(project: &mut Project, clip_ids: &HashSet<String>) {
    if let Some(track) = project.subtitles.as_mut() {
        track
            .cues
            .retain(|cue| cue.link_clip_id.is_empty() || !clip_ids.contains(&cue.link_clip_id));
    }
}

/// clip を frame で分割したときのリンクの付け替え。分割位置以降に始まる字幕は右半分の clip へ移す。
/// 分割位置を跨ぐ字幕は frame で 2 つに分け（本文も分ける）、前半を左、後半を右の clip へリンクする。
pub fn relink_subtitles_after_split(
    project: &mut Project,
    left_id: &str,
    right_id: &str,
    frame: i64,
    new_id: &mut dyn FnMut() -> String,
) {
    let Some(track) = project.subtitles.as_mut() else {
        return;
    };
    let mut rights = Vec::new();
    for cue in &mut track.cues {
        if cue.link_clip_id != left_id {
            continue;
        }
        if cue.start_frame >= frame {
            cue.link_clip_id = right_id.to_owned();
        } else if cue.end_frame > frame {
            let ratio = (frame - cue.start_frame) as f64 / (cue.end_frame - cue.start_frame) as f64;
            let (left, right_content) = split_subtitle_text(&cue.content, ratio, None);
            rights.push(SubtitleCue {
                id: new_id(),
                start_frame: frame,
                end_frame: cue.end_frame,
                content: right_content,
                link_clip_id: right_id.to_owned(),
            });
            cue.content = left;
            cue.end_frame = frame;
        }
    }
    track.cues.extend(rights);
    sort_cues(track);
}

/// clip の時間の対応。timeline の frame t と素材の秒 s の関係: s = in + (t - start) / timeline fps × speed。
#[derive(Clone, Copy, Debug, PartialEq)]
struct ClipMapping {
    start: f64,
    in_seconds: f64,
    out_seconds: f64,
    speed: f64,
}

fn clip_mapping(clip: &TimelineClip) -> ClipMapping {
    let source_fps = clip.source_fps.num as f64 / clip.source_fps.den.max(1) as f64;
    ClipMapping {
        start: clip.timeline_start_frame as f64,
        in_seconds: clip.source_in_frame as f64 / source_fps,
        out_seconds: clip.source_out_frame as f64 / source_fps,
        speed: clip.speed.num as f64 / clip.speed.den.max(1) as f64,
    }
}

/// 時間の対応を変える clip の編集の後で、リンクした字幕を音声に合わせて置き直す。字幕は素材の時刻を保つ:
/// 編集前の clip で字幕の区間を素材の時刻へ直し、trim で切り落とした部分を除き、編集後の clip で timeline の
/// 区間へ戻す。素材の範囲から全部外れた字幕は消す。編集前の位置から作り直すので二重にはずらさない。
/// 他の字幕と重なる、または丸めると 0 frame になる字幕は編集全体を拒否する。
pub fn remap_linked_subtitles(before: &Project, candidate: &mut Project) -> ProjectResult<()> {
    let (Some(before_track), Some(_)) = (&before.subtitles, &candidate.subtitles) else {
        return Ok(());
    };
    let timeline_fps = candidate.timeline_fps.num as f64 / candidate.timeline_fps.den as f64;
    let before_by_id: HashMap<&str, &TimelineClip> = before
        .timeline_clips
        .iter()
        .map(|clip| (clip.id.as_str(), clip))
        .collect();
    // 時間の対応が変わった clip。素材の時間軸を持たない clip（文字・静止画）は対象外。
    let mut changed: HashMap<String, (ClipMapping, ClipMapping)> = HashMap::new();
    for after in &candidate.timeline_clips {
        if after.has_synthetic_source_domain() || after.source_fps.num <= 0 {
            continue;
        }
        let Some(found) = before_by_id.get(after.id.as_str()) else {
            continue;
        };
        if found.source_fps != after.source_fps {
            continue;
        }
        let (old, now) = (clip_mapping(found), clip_mapping(after));
        if old != now {
            changed.insert(after.id.clone(), (old, now));
        }
    }
    if changed.is_empty() {
        return Ok(());
    }
    let mut cues: Vec<SubtitleCue> = candidate
        .subtitles
        .as_ref()
        .map(|track| {
            track
                .cues
                .iter()
                .filter(|cue| !changed.contains_key(&cue.link_clip_id))
                .cloned()
                .collect()
        })
        .unwrap_or_default();
    for cue in &before_track.cues {
        let Some((old, now)) = changed.get(&cue.link_clip_id) else {
            continue;
        };
        let to_source =
            |frame: f64| old.in_seconds + (frame - old.start) / timeline_fps * old.speed;
        let mut start_seconds = to_source(cue.start_frame as f64);
        let mut end_seconds = to_source(cue.end_frame as f64);
        // trim で素材の範囲から外した部分を除く。元から clip の外にはみ出していた部分は残す。
        if now.in_seconds > old.in_seconds
            && start_seconds < now.in_seconds
            && end_seconds > old.in_seconds
        {
            start_seconds = start_seconds.max(now.in_seconds);
        }
        if now.out_seconds < old.out_seconds
            && end_seconds > now.out_seconds
            && start_seconds < old.out_seconds
        {
            end_seconds = end_seconds.min(now.out_seconds);
        }
        if end_seconds <= start_seconds {
            continue;
        }
        let to_timeline = |seconds: f64| {
            (now.start + (seconds - now.in_seconds) / now.speed * timeline_fps).round() as i64
        };
        let remapped = SubtitleCue {
            start_frame: to_timeline(start_seconds).max(0),
            end_frame: to_timeline(end_seconds),
            ..cue.clone()
        };
        // 音声は残っているのに frame へ丸めると 0 frame になる。黙って消さずに拒否する。
        if remapped.end_frame <= remapped.start_frame {
            return fail(format!(
                "この速度ではリンクした字幕の区間を 1 frame 以上で表せません: {}",
                cue.content
            ));
        }
        cues.push(remapped);
    }
    cues.sort_by_key(|cue| cue.start_frame);
    if has_overlap(&cues) {
        return fail("リンクした字幕が他の字幕と重なるため、この編集はできません");
    }
    if let Some(track) = candidate.subtitles.as_mut() {
        track.cues = cues;
    }
    Ok(())
}

/// `clip_ids` にリンクした字幕のリンクを外す。外した件数を返す。
pub fn unlink_subtitles(project: &mut Project, clip_ids: &HashSet<String>) -> usize {
    let Some(track) = project.subtitles.as_mut() else {
        return 0;
    };
    let mut count = 0;
    for cue in &mut track.cues {
        if !cue.link_clip_id.is_empty() && clip_ids.contains(&cue.link_clip_id) {
            cue.link_clip_id.clear();
            count += 1;
        }
    }
    count
}

/// 素材の削除・上書きなどで消えた clip へのリンクを外す（字幕自体は残す）。
pub fn reconcile_subtitle_links(project: &mut Project) {
    let clip_ids: HashSet<String> = project
        .timeline_clips
        .iter()
        .map(|clip| clip.id.clone())
        .collect();
    if let Some(track) = project.subtitles.as_mut() {
        for cue in &mut track.cues {
            if !cue.link_clip_id.is_empty() && !clip_ids.contains(&cue.link_clip_id) {
                cue.link_clip_id.clear();
            }
        }
    }
}

// --- SRT ---

/// `HH:MM:SS,mmm` をミリ秒にする。時は 2 桁以上。
fn parse_srt_timestamp(text: &str) -> Option<i64> {
    let colon = text.find(':')?;
    let bytes = text.as_bytes();
    if colon < 2 || text.len() != colon + 10 || bytes[colon + 3] != b':' || bytes[colon + 6] != b','
    {
        return None;
    }
    let digits = |range: std::ops::Range<usize>| -> Option<i64> {
        let part = text.get(range)?;
        if !part.bytes().all(|byte| byte.is_ascii_digit()) {
            return None;
        }
        part.parse().ok()
    };
    let hours = digits(0..colon).filter(|hours| *hours <= 1_000_000_000)?;
    let minutes = digits(colon + 1..colon + 3).filter(|minutes| *minutes <= 59)?;
    let seconds = digits(colon + 4..colon + 6).filter(|seconds| *seconds <= 59)?;
    let millis = digits(colon + 7..colon + 10)?;
    Some(((hours * 60 + minutes) * 60 + seconds) * 1000 + millis)
}

fn format_srt_time(ms: i64) -> String {
    format!(
        "{:02}:{:02}:{:02},{:03}",
        ms / 3_600_000,
        ms / 60_000 % 60,
        ms / 1000 % 60,
        ms % 1000
    )
}

/// SRT の装飾記法か。対応していないので読み込みを拒否する。実際の記法だけを見る:
/// `<b>` `<i>` `<u>` `<s>` `<font ...>` とその閉じタグ、ASS の上書き `{\an8}` など。
/// "a < b" や "std::vector<int>" や "{ x > 0 }" は普通の本文として通す。
fn has_srt_markup(line: &str) -> bool {
    let bytes = line.as_bytes();
    for (index, byte) in bytes.iter().enumerate() {
        if *byte == b'{' && bytes.get(index + 1) == Some(&b'\\') {
            return true;
        }
        if *byte != b'<' {
            continue;
        }
        let mut cursor = index + 1;
        if bytes.get(cursor) == Some(&b'/') {
            cursor += 1;
        }
        let name_start = cursor;
        while bytes.get(cursor).is_some_and(u8::is_ascii_alphabetic) {
            cursor += 1;
        }
        let Some(next) = bytes.get(cursor) else {
            continue;
        };
        let name = line[name_start..cursor].to_ascii_lowercase();
        if matches!(name.as_str(), "b" | "i" | "u" | "s") && *next == b'>' {
            return true;
        }
        if name == "font" && matches!(next, b'>' | b' ') && line[cursor..].contains('>') {
            return true;
        }
    }
    false
}

/// SRT（UTF-8、BOM 可）を字幕にする。装飾記法・位置指定・重なりは拒否する。
pub fn parse_srt(
    text: &str,
    fps: Rational,
    new_id: &mut dyn FnMut() -> String,
) -> ProjectResult<Vec<SubtitleCue>> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    if !content_shape(text).0 {
        return fail("SRTはUTF-8で保存してください");
    }
    let lines: Vec<&str> = text
        .split('\n')
        .map(|line| line.strip_suffix('\r').unwrap_or(line))
        .collect();
    // 末尾の改行の後ろは行として数えない（getline と同じ）。
    let lines = match lines.split_last() {
        Some((&"", rest)) => rest.to_vec(),
        _ => lines,
    };
    let fail_at =
        |line: usize, reason: &str| ProjectError::new(format!("SRT {}行目: {reason}", line + 1));
    let mut parsed: Vec<SubtitleCue> = Vec::new();
    let mut index = 0;
    while index < lines.len() {
        if lines[index].is_empty() {
            index += 1;
            continue;
        }
        let first = index;
        if !lines[index].bytes().all(|byte| byte.is_ascii_digit()) {
            return Err(fail_at(index, "字幕番号が不正です"));
        }
        index += 1;
        let Some(times) = lines.get(index) else {
            return Err(fail_at(first, "時刻がありません"));
        };
        let range = times.find(" --> ").and_then(|arrow| {
            Some((
                parse_srt_timestamp(&times[..arrow])?,
                parse_srt_timestamp(&times[arrow + 5..])?,
            ))
        });
        let Some((start_ms, end_ms)) = range.filter(|(start, end)| start < end) else {
            return Err(fail_at(index, "時刻または未対応の位置指定です"));
        };
        let start_frame = subtitle_time_to_frame(start_ms, fps)
            .map_err(|error| fail_at(index, error.message()))?;
        let end_frame =
            subtitle_time_to_frame(end_ms, fps).map_err(|error| fail_at(index, error.message()))?;
        let mut content = String::new();
        index += 1;
        while let Some(line) = lines.get(index).filter(|line| !line.is_empty()) {
            if has_srt_markup(line) {
                return Err(fail_at(index, "装飾記法には対応していません"));
            }
            if !content.is_empty() {
                content.push('\n');
            }
            content.push_str(line);
            index += 1;
        }
        if content.is_empty()
            || end_frame <= start_frame
            || parsed
                .last()
                .is_some_and(|last| start_frame < last.end_frame)
        {
            return Err(fail_at(
                first,
                "空本文・重複・並び順または丸め後の尺が不正です",
            ));
        }
        parsed.push(SubtitleCue {
            id: new_id(),
            start_frame,
            end_frame,
            content,
            link_clip_id: String::new(),
        });
    }
    if parsed.is_empty() {
        return fail("SRTに字幕がありません");
    }
    check_cues(&parsed)?;
    Ok(parsed)
}

/// 字幕を SRT にする。SRT のミリ秒で区間・本文を保てない（読み戻すと変わる）なら失敗する。
pub fn write_srt(project: &Project) -> ProjectResult<String> {
    validate_subtitles(project)?;
    let Some(track) = &project.subtitles else {
        return Ok(String::new());
    };
    let mut text = String::new();
    for (index, cue) in track.cues.iter().enumerate() {
        let start = subtitle_frame_to_time(cue.start_frame, project.timeline_fps)?;
        let end = subtitle_frame_to_time(cue.end_frame, project.timeline_fps)?;
        text.push_str(&format!(
            "{}\n{} --> {}\n{}\n\n",
            index + 1,
            format_srt_time(start),
            format_srt_time(end),
            cue.content
        ));
    }
    if text.is_empty() {
        return Ok(text);
    }
    let mut ids = track.cues.iter().map(|cue| cue.id.clone());
    let mut extra = 0;
    let verified = parse_srt(&text, project.timeline_fps, &mut || {
        ids.next().unwrap_or_else(|| {
            extra += 1;
            format!("srt-extra-{extra}")
        })
    })
    .map_err(|error| ProjectError::new(format!("SRTを書き出せません: {error}")))?;
    // SRT が表せるのは区間と本文だけ。clip とのリンクは mvm の Project の情報なので比べない。
    let kept = verified.len() == track.cues.len()
        && verified.iter().zip(&track.cues).all(|(reparsed, cue)| {
            reparsed.id == cue.id
                && reparsed.start_frame == cue.start_frame
                && reparsed.end_frame == cue.end_frame
                && reparsed.content == cue.content
        });
    if !kept {
        return fail("SRTのミリ秒単位で本文・字幕区間を保持できません");
    }
    Ok(text)
}
