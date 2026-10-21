//! Whisper（whisper.cpp の whisper-cli）による文字起こし。結果は編集できる候補で、適用するまで Project を変えない。
//!
//! - 対象は timeline の clip（trim 範囲の原音。速度と配置位置で timeline へ換算し、clip にリンクする）か、
//!   素材の全体（指定した frame へ置く）。
//! - 音声は 16 kHz mono の WAV にして whisper-cli へ渡し、JSON の区間（ミリ秒）を読む。
//! - GPU を指定して Vulkan を使えなかったら失敗する（CPU へ黙って切り替えない）。
//! - 認識の前後と適用の直前に素材の実体・大きさ・更新時刻を照合し、差し替えた素材の候補を適用しない。

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use mvm_core::Rational;
use mvm_media::{SPEECH_SAMPLE_RATE, decode_speech, write_pcm16_wav};
use mvm_platform::{FileProbe, ProcessOutcome, ProcessRequest, probe_file, run_process};
use mvm_project::subtitles::{
    SubtitleTimedText, apply_subtitle_cues, subtitle_cues_from_timed_text,
};
use mvm_project::{SubtitleCue, TimelineClipKind};
use serde::Deserialize;

use crate::background::{BackgroundJob, JobControl};
use crate::error::AppError;
use crate::session::Session;

static JOB_SERIAL: AtomicU64 = AtomicU64::new(1);
/// 語彙ヒントの上限（文字）。
pub const MAXIMUM_PROMPT_CHARS: usize = 200;
/// 区間の終端が音声の末尾を越えてよい量。Whisper の窓の無音の分を末尾へ丸める範囲。
pub const TAIL_TOLERANCE_MS: i64 = 1_000;
/// 1 回の認識の上限時間。
const RECOGNITION_TIMEOUT: Duration = Duration::from_secs(4 * 60 * 60);

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TranscriptionSource {
    /// timeline の clip。字幕は clip にリンクする。
    Clip { clip_id: String },
    /// プロジェクトパネルの素材の全体を `place_frame` から置く。
    Media { item_id: String, place_frame: i64 },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TranscriptionRequest {
    pub source: TranscriptionSource,
    pub whisper_cli: PathBuf,
    pub model: PathBuf,
    pub language: String,
    pub gpu: bool,
    pub prompt: String,
}

#[derive(Clone, Debug)]
pub struct TranscriptionOutcome {
    pub cues: Vec<SubtitleCue>,
    base_revision: u64,
    media: PathBuf,
    media_probe: FileProbe,
}

/// 認識する音声と、結果を timeline へ置く換算。
struct Target {
    media: PathBuf,
    begin_ms: i64,
    end_ms: Option<i64>,
    speed: Rational,
    offset_frame: i64,
    link_clip_id: String,
}

fn frame_ms(frame: i64, fps: Rational) -> Result<i64, AppError> {
    if !fps.is_positive() || frame < 0 {
        return Err(AppError::new("素材の時間基準が不正です"));
    }
    let value = (i128::from(frame) * 1000 * i128::from(fps.den) + i128::from(fps.num) / 2)
        / i128::from(fps.num);
    i64::try_from(value).map_err(|_| AppError::new("素材の時刻が範囲外です"))
}

impl Session {
    fn transcription_target(&self, source: &TranscriptionSource) -> Result<Target, AppError> {
        match source {
            TranscriptionSource::Clip { clip_id } => {
                let clip = self
                    .project
                    .clip(clip_id)
                    .ok_or_else(|| AppError::new("文字起こしする clip がありません"))?;
                if !matches!(clip.kind, TimelineClipKind::Audio | TimelineClipKind::Video) {
                    return Err(AppError::new("音声を持つ clip を選んでください"));
                }
                if clip.frame_hold.is_some() {
                    return Err(AppError::new("フレーム保持の clip は文字起こしできません"));
                }
                Ok(Target {
                    media: clip.media_path.clone(),
                    begin_ms: frame_ms(clip.source_in_frame, clip.source_fps)?,
                    end_ms: Some(frame_ms(clip.source_out_frame, clip.source_fps)?),
                    speed: clip.speed,
                    offset_frame: clip.timeline_start_frame,
                    link_clip_id: clip.id.clone(),
                })
            }
            TranscriptionSource::Media {
                item_id,
                place_frame,
            } => {
                let item = self
                    .project
                    .media_items
                    .iter()
                    .find(|item| item.id == *item_id)
                    .ok_or_else(|| AppError::new("文字起こしする素材がありません"))?;
                if *place_frame < 0 {
                    return Err(AppError::new("置く位置が不正です"));
                }
                Ok(Target {
                    media: item.media_path.clone(),
                    begin_ms: 0,
                    end_ms: None,
                    speed: Rational::new(1, 1),
                    offset_frame: *place_frame,
                    link_clip_id: String::new(),
                })
            }
        }
    }

    pub fn start_transcription(&mut self, request: TranscriptionRequest) -> Result<(), AppError> {
        if self.background.transcription.is_some() {
            return Err(AppError::new("文字起こし中です"));
        }
        if self.is_playing() {
            self.pause_playback();
        }
        if !request.whisper_cli.is_file() {
            return Err(AppError::new(format!(
                "whisper-cli が見つかりません: {}",
                request.whisper_cli.display()
            )));
        }
        if !request.model.is_file() {
            return Err(AppError::new(format!(
                "認識 model が見つかりません: {}",
                request.model.display()
            )));
        }
        if request.language.trim().is_empty() {
            return Err(AppError::new("認識言語を指定してください"));
        }
        if request.prompt.chars().count() > MAXIMUM_PROMPT_CHARS {
            return Err(AppError::new(format!(
                "語彙ヒントは {MAXIMUM_PROMPT_CHARS} 文字までです"
            )));
        }
        let target = self.transcription_target(&request.source)?;
        let fps = self.project.timeline_fps;
        let revision = self.edit_revision;
        self.background.transcription_result = None;
        let job = BackgroundJob::start(
            "mvm-transcribe",
            self.preview_wake.clone(),
            move |control| transcribe(&request, &target, fps, revision, control),
        )?;
        self.background.transcription = Some(job);
        Ok(())
    }

    pub fn transcription_progress(&self) -> Option<f32> {
        self.background
            .transcription
            .as_ref()
            .map(BackgroundJob::progress)
    }

    pub fn cancel_transcription(&mut self) {
        if let Some(job) = &self.background.transcription {
            job.cancel();
        }
    }

    pub fn poll_transcription(&mut self) -> Option<Result<(), AppError>> {
        let finished = self.background.transcription.as_mut()?.take_finished()?;
        self.background.transcription = None;
        Some(match finished.and_then(|result| result) {
            Ok(outcome) => {
                self.background.transcription_result = Some(outcome);
                Ok(())
            }
            Err(error) => Err(error),
        })
    }

    pub fn transcription_candidates(&self) -> Option<&[SubtitleCue]> {
        self.background
            .transcription_result
            .as_ref()
            .map(|outcome| outcome.cues.as_slice())
    }

    /// 候補の 1 行を直す（適用前の確認）。
    pub fn edit_transcription_candidate(
        &mut self,
        index: usize,
        start: i64,
        end: i64,
        content: &str,
    ) -> Result<(), AppError> {
        let Some(cue) = self
            .background
            .transcription_result
            .as_mut()
            .and_then(|outcome| outcome.cues.get_mut(index))
        else {
            return Err(AppError::new("直す候補がありません"));
        };
        check_candidate(start, end, content)?;
        cue.start_frame = start;
        cue.end_frame = end;
        cue.content = content.to_owned();
        Ok(())
    }

    pub fn remove_transcription_candidate(&mut self, index: usize) {
        if let Some(outcome) = self.background.transcription_result.as_mut()
            && index < outcome.cues.len()
        {
            outcome.cues.remove(index);
        }
    }

    pub fn discard_transcription(&mut self) {
        self.background.transcription_result = None;
    }

    /// 画面で直した候補を字幕へ置換または追加する（1 回の undo）。`edits` は候補と同じ数・同じ並びで、
    /// 画面の欄の値そのもの。1 行でも不正なら何も適用せず、候補もそのまま残す。認識の後の編集・素材の
    /// 差し替えも拒否する。
    pub fn apply_transcription(
        &mut self,
        edits: &[CandidateEdit],
        replace: bool,
    ) -> Result<(), AppError> {
        let Some(outcome) = self.background.transcription_result.as_ref() else {
            return Err(AppError::new("適用する文字起こしの結果がありません"));
        };
        if edits.len() != outcome.cues.len() {
            return Err(AppError::new(
                "画面の候補と認識結果の数が合いません。候補を開き直してください",
            ));
        }
        if outcome.base_revision != self.edit_revision {
            return Err(AppError::new(
                "文字起こしの後に Project が変わりました。もう一度認識してください",
            ));
        }
        if probe_file(&outcome.media) != outcome.media_probe {
            return Err(AppError::new(
                "認識した後に素材が変わりました。もう一度認識してください",
            ));
        }
        if outcome.cues.is_empty() {
            return Err(AppError::new("適用する候補がありません"));
        }
        for (index, edit) in edits.iter().enumerate() {
            check_candidate(edit.start, edit.end, &edit.content).map_err(|error| {
                AppError::new(format!("{} 行目: {}", index + 1, error.message()))
            })?;
        }
        check_candidate_overlaps(edits)?;
        if !replace {
            let existing = self
                .project
                .subtitles
                .iter()
                .flat_map(|track| track.cues.iter());
            for cue in existing {
                if let Some(index) = edits
                    .iter()
                    .position(|edit| edit.start < cue.end_frame && cue.start_frame < edit.end)
                {
                    return Err(AppError::new(format!(
                        "{} 行目: 既にある字幕（{}–{} フレーム）と重なります",
                        index + 1,
                        cue.start_frame,
                        cue.end_frame
                    )));
                }
            }
        }
        // 候補の仮の ID を、今の Project で一意な ID に置き換える。
        let mut factory = self.id_factory();
        let cues: Vec<SubtitleCue> = outcome
            .cues
            .iter()
            .zip(edits)
            .map(|(cue, edit)| SubtitleCue {
                id: factory.next(),
                start_frame: edit.start,
                end_frame: edit.end,
                content: edit.content.clone(),
                ..cue.clone()
            })
            .collect();
        self.transact(move |project| apply_subtitle_cues(project, cues, replace).map(|()| None))?;
        // 発行した ID は適用できたときだけ使ったことにする。
        self.id_serial = factory.serial;
        self.background.transcription_result = None;
        Ok(())
    }
}

/// 画面で直した候補の 1 行。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CandidateEdit {
    pub start: i64,
    pub end: i64,
    pub content: String,
}

fn check_candidate(start: i64, end: i64, content: &str) -> Result<(), AppError> {
    if start < 0 || end <= start {
        return Err(AppError::new(
            "候補の区間が不正です（開始は 0 以上、終了は開始より後）",
        ));
    }
    if content.trim().is_empty() {
        return Err(AppError::new("候補の本文が空です"));
    }
    Ok(())
}

/// 候補どうしの重なりを、画面の行番号で示して拒否する。行の並びは時刻順とは限らないので、元の行番号を
/// 持ったまま開始で並べ、隣どうしを比べる（どこかが重なっていれば、並べた隣どうしのどこかが重なる）。
fn check_candidate_overlaps(edits: &[CandidateEdit]) -> Result<(), AppError> {
    let mut order: Vec<usize> = (0..edits.len()).collect();
    order.sort_by_key(|index| (edits[*index].start, edits[*index].end));
    for pair in order.windows(2) {
        let (earlier, later) = (&edits[pair[0]], &edits[pair[1]]);
        if later.start < earlier.end {
            let (first, second) = (pair[0].min(pair[1]) + 1, pair[0].max(pair[1]) + 1);
            return Err(AppError::new(format!(
                "{first} 行目と {second} 行目の区間が重なっています"
            )));
        }
    }
    Ok(())
}

#[derive(Deserialize)]
struct WhisperJson {
    model: WhisperModel,
    transcription: Vec<WhisperSegment>,
}

#[derive(Deserialize)]
struct WhisperModel {
    multilingual: bool,
}

#[derive(Deserialize)]
struct WhisperSegment {
    offsets: WhisperOffsets,
    text: String,
}

#[derive(Deserialize)]
struct WhisperOffsets {
    from: i64,
    to: i64,
}

/// whisper-cli の JSON から区間を読む。
///
/// Whisper は 30 秒の窓へ無音を足して処理するので、最後の区間の終端が音声の末尾を少し越えることがある
/// （[事実] C++ 版の実測で 7850 ms の音声に 8360 ms）。終端の超過が [`TAIL_TOLERANCE_MS`] 以内なら末尾へ
/// 丸め、それより大きい超過は誤った時刻として拒否する。開始が音声の外・終端が開始以前・前の区間との重なり・
/// 空の結果も失敗にする。
fn parse_whisper_json(
    text: &str,
    audio_ms: i64,
    language: &str,
) -> Result<Vec<SubtitleTimedText>, AppError> {
    let parsed: WhisperJson = serde_json::from_str(text)
        .map_err(|error| AppError::new(format!("認識結果を読めません: {error}")))?;
    if !parsed.model.multilingual && language != "en" {
        return Err(AppError::new(
            "日本語・自動検出には多言語の model を指定してください",
        ));
    }
    let mut segments: Vec<SubtitleTimedText> = Vec::new();
    for segment in parsed.transcription {
        let content = segment.text.trim().to_owned();
        if content.is_empty() {
            continue;
        }
        let start = segment.offsets.from;
        let reported_end = segment.offsets.to;
        let overlaps = segments.last().is_some_and(|last| start < last.end_ms);
        if start < 0 || start >= audio_ms || reported_end <= start || overlaps {
            return Err(AppError::new(format!(
                "認識結果の時刻区間が不正です（{start}〜{reported_end} ms、音声は {audio_ms} ms）"
            )));
        }
        if reported_end > audio_ms + TAIL_TOLERANCE_MS {
            return Err(AppError::new(format!(
                "認識結果の終端が音声の末尾を {} ms 越えています（{start}〜{reported_end} ms、音声は {audio_ms} ms）",
                reported_end - audio_ms
            )));
        }
        let end = reported_end.min(audio_ms);
        segments.push(SubtitleTimedText {
            start_ms: start,
            end_ms: end,
            content,
        });
    }
    if segments.is_empty() {
        return Err(AppError::new("音声認識の結果が 0 件でした"));
    }
    Ok(segments)
}

/// stderr の進捗行（`progress = NN%`）の最後の値。
fn last_progress(log: &str) -> Option<u32> {
    log.lines()
        .rev()
        .find_map(|line| line.split("progress =").nth(1))
        .and_then(|rest| rest.trim().trim_end_matches('%').trim().parse().ok())
}

fn transcribe(
    request: &TranscriptionRequest,
    target: &Target,
    fps: Rational,
    base_revision: u64,
    control: &JobControl,
) -> Result<TranscriptionOutcome, AppError> {
    let cancelled = || AppError::new("文字起こしを取り消しました");
    let probe_before = probe_file(&target.media);
    let cancel = control.cancel_flag();
    let samples =
        decode_speech(&target.media, target.begin_ms, target.end_ms, &cancel).map_err(|error| {
            if control.is_cancelled() {
                cancelled()
            } else {
                AppError::new(error.to_string())
            }
        })?;
    control.set_progress(0.05);
    let audio_ms = i64::try_from(samples.len()).map_err(|_| AppError::new("音声が長すぎます"))?
        * 1000
        / SPEECH_SAMPLE_RATE;
    let directory = std::env::temp_dir().join(format!(
        "mvm-whisper-{}-{}",
        std::process::id(),
        JOB_SERIAL.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&directory)
        .map_err(|error| AppError::new(format!("作業 directory を作れません: {error}")))?;
    let result = run_whisper(request, &directory, &samples, audio_ms, control);
    let _ = std::fs::remove_dir_all(&directory);
    let segments = result?;
    if probe_file(&target.media) != probe_before {
        return Err(AppError::new(
            "認識中に素材が変わりました。もう一度認識してください",
        ));
    }
    let mut serial = 0u64;
    let mut next_id = || {
        serial += 1;
        format!("candidate-{serial}")
    };
    let mut cues = subtitle_cues_from_timed_text(
        &segments,
        fps,
        target.speed,
        target.offset_frame,
        &mut next_id,
    )
    .map_err(|error| AppError::new(error.to_string()))?;
    for cue in &mut cues {
        cue.link_clip_id = target.link_clip_id.clone();
    }
    control.set_progress(1.0);
    Ok(TranscriptionOutcome {
        cues,
        base_revision,
        media: target.media.clone(),
        media_probe: probe_before,
    })
}

fn run_whisper(
    request: &TranscriptionRequest,
    directory: &Path,
    samples: &[f32],
    audio_ms: i64,
    control: &JobControl,
) -> Result<Vec<SubtitleTimedText>, AppError> {
    let wav = directory.join("input.wav");
    write_pcm16_wav(&wav, SPEECH_SAMPLE_RATE as u32, 1, samples)
        .map_err(|error| AppError::new(error.to_string()))?;
    let output = directory.join("result");
    let stderr = directory.join("stderr.txt");
    let threads = std::thread::available_parallelism()
        .map(|count| count.get())
        .unwrap_or(4)
        .to_string();
    let mut arguments: Vec<&OsStr> = vec![
        OsStr::new("-m"),
        request.model.as_os_str(),
        OsStr::new("-f"),
        wav.as_os_str(),
        OsStr::new("-l"),
        OsStr::new(request.language.as_str()),
        OsStr::new("-oj"),
        OsStr::new("-of"),
        output.as_os_str(),
        // 精度を優先する（beam 5・best_of 5、温度の再試行は既定のまま）。
        OsStr::new("-bs"),
        OsStr::new("5"),
        OsStr::new("-bo"),
        OsStr::new("5"),
        OsStr::new("-t"),
        OsStr::new(threads.as_str()),
        // 前区間の本文を文脈にしない（同じ文の繰り返しに陥ったとき後続を巻き込む）。
        OsStr::new("-mc"),
        OsStr::new("0"),
        // 「(拍手)」「♪」など発話でない注記を出さない。
        OsStr::new("-sns"),
        OsStr::new("-nfa"),
        OsStr::new("-pp"),
    ];
    if !request.gpu {
        arguments.push(OsStr::new("-ng"));
    }
    let prompt = request.prompt.trim();
    if !prompt.is_empty() {
        arguments.push(OsStr::new("--prompt"));
        arguments.push(OsStr::new(prompt));
        arguments.push(OsStr::new("--carry-initial-prompt"));
    }
    let mut checked = std::time::Instant::now();
    let outcome = run_process(
        &ProcessRequest {
            executable: &request.whisper_cli,
            arguments: &arguments,
            working_directory: Some(directory),
            stdout_path: Some(&directory.join("stdout.txt")),
            stderr_path: Some(&stderr),
            timeout: Some(RECOGNITION_TIMEOUT),
        },
        &mut || {
            if checked.elapsed() >= Duration::from_millis(250) {
                checked = std::time::Instant::now();
                if let Some(percent) = std::fs::read_to_string(&stderr)
                    .ok()
                    .as_deref()
                    .and_then(last_progress)
                {
                    control.set_progress(0.05 + 0.95 * f64::from(percent.min(100)) / 100.0);
                }
            }
            control.is_cancelled()
        },
    )
    .map_err(|error| AppError::new(error.to_string()))?;
    let log = std::fs::read_to_string(&stderr).unwrap_or_default();
    match outcome {
        ProcessOutcome::Cancelled => return Err(AppError::new("文字起こしを取り消しました")),
        ProcessOutcome::TimedOut => {
            return Err(AppError::new("文字起こしが時間内に終わりませんでした"));
        }
        ProcessOutcome::Exited(0) => {}
        ProcessOutcome::Exited(code) => {
            let reason = log
                .lines()
                .rev()
                .find(|line| line.contains("error"))
                .unwrap_or("");
            return Err(AppError::new(format!(
                "whisper-cli が終了コード {code} で失敗しました {reason}"
            )));
        }
    }
    if request.gpu && !log.contains("using Vulkan") {
        return Err(AppError::new(
            "GPU（Vulkan）で認識できませんでした。CPU で認識するなら GPU の指定を外してください",
        ));
    }
    let json = std::fs::read_to_string(directory.join("result.json"))
        .map_err(|error| AppError::new(format!("認識結果がありません: {error}")))?;
    parse_whisper_json(&json, audio_ms, &request.language)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use mvm_project::{MediaItem, MediaKind, Project, SubtitleCue, TimelineClip, TrackRef};

    use super::*;

    fn json(segments: &[(i64, i64, &str)], multilingual: bool) -> String {
        let items: Vec<String> = segments
            .iter()
            .map(|(from, to, text)| {
                format!(
                    "{{\"timestamps\":{{}},\"offsets\":{{\"from\":{from},\"to\":{to}}},\"text\":\"{text}\"}}"
                )
            })
            .collect();
        format!(
            "{{\"model\":{{\"multilingual\":{multilingual}}},\"transcription\":[{}]}}",
            items.join(",")
        )
    }

    #[test]
    fn whisper_json_is_trimmed_clamped_and_checked() {
        let text = json(&[(0, 2760, " いい挨拶。"), (2760, 8360, " はい。")], true);
        let segments = parse_whisper_json(&text, 7850, "ja").unwrap();
        assert_eq!(segments.len(), 2);
        assert_eq!(segments[0].content, "いい挨拶。");
        // 最後の区間の終端は音声の長さへ丸める。
        assert_eq!(segments[1].end_ms, 7850);

        let overlap = json(&[(0, 3000, "a"), (2000, 4000, "b")], true);
        assert!(parse_whisper_json(&overlap, 7850, "ja").is_err());
        let outside = json(&[(8000, 9000, "a")], true);
        assert!(parse_whisper_json(&outside, 7850, "ja").is_err());
        // 末尾の超過は 1000 ms まで丸め、それより大きい超過は誤りとして拒否する。
        let edge = json(&[(9000, 11000, "a")], true);
        assert_eq!(
            parse_whisper_json(&edge, 10000, "ja").unwrap()[0].end_ms,
            10000
        );
        let runaway = json(&[(9000, 90000, "a")], true);
        let error = parse_whisper_json(&runaway, 10000, "ja").unwrap_err();
        assert!(error.message().contains("越えています"), "{error}");
        let starts_at_end = json(&[(10000, 10500, "a")], true);
        assert!(parse_whisper_json(&starts_at_end, 10000, "ja").is_err());
        let blank = json(&[(0, 1000, "  ")], true);
        assert!(parse_whisper_json(&blank, 7850, "ja").is_err());
        let english_only = json(&[(0, 1000, "a")], false);
        assert!(parse_whisper_json(&english_only, 7850, "ja").is_err());
        assert!(parse_whisper_json(&english_only, 7850, "en").is_ok());
    }

    /// 候補をそのまま画面の値にしたもの。
    fn edits_of(session: &Session) -> Vec<CandidateEdit> {
        session
            .transcription_candidates()
            .unwrap_or_default()
            .iter()
            .map(|cue| CandidateEdit {
                start: cue.start_frame,
                end: cue.end_frame,
                content: cue.content.clone(),
            })
            .collect()
    }

    fn edit(start: i64, end: i64, content: &str) -> CandidateEdit {
        CandidateEdit {
            start,
            end,
            content: content.to_owned(),
        }
    }

    /// 2 行の候補を持つ Session。素材は確かめられるよう実在の file にする。
    fn two_candidates(name: &str) -> (Session, PathBuf) {
        let directory = std::env::temp_dir().join(format!("mvm-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let media = directory.join("voice.wav");
        std::fs::write(&media, b"voice").unwrap();
        let mut session = Session::from_project(Project::default());
        let cue = |id: &str, start, end, content: &str| SubtitleCue {
            id: id.to_owned(),
            start_frame: start,
            end_frame: end,
            content: content.to_owned(),
            link_clip_id: String::new(),
        };
        session.background.transcription_result = Some(TranscriptionOutcome {
            cues: vec![
                cue("candidate-1", 0, 30, "誤認識"),
                cue("candidate-2", 40, 70, "二行目"),
            ],
            base_revision: session.edit_revision,
            media: media.clone(),
            media_probe: probe_file(&media),
        });
        (session, directory)
    }

    #[test]
    fn applying_uses_the_values_on_screen_even_without_the_row_update() {
        let (mut session, directory) = two_candidates("transcribe-screen");
        // 1 行目は本文と区間を直し、行の「更新」は押していない（Session の候補は元のまま）。
        let edits = [edit(5, 35, "直した本文"), edit(40, 70, "二行目")];
        session.apply_transcription(&edits, true).unwrap();
        let cues = &session.project().subtitles.as_ref().unwrap().cues;
        let applied: Vec<(i64, i64, &str)> = cues
            .iter()
            .map(|cue| (cue.start_frame, cue.end_frame, cue.content.as_str()))
            .collect();
        assert_eq!(applied, [(5, 35, "直した本文"), (40, 70, "二行目")]);
        assert!(session.transcription_candidates().is_none());
        std::fs::remove_dir_all(&directory).unwrap();
    }

    #[test]
    fn one_bad_row_applies_nothing_and_keeps_the_candidates() {
        let (mut session, directory) = two_candidates("transcribe-bad");
        let original = edits_of(&session);
        let refused = [
            // 2 行目の終了が開始より前。
            (
                vec![edit(0, 30, "直した"), edit(40, 39, "二行目")],
                "2 行目",
            ),
            // 1 行目の開始が負。
            (
                vec![edit(-1, 30, "直した"), edit(40, 70, "二行目")],
                "1 行目",
            ),
            // 2 行目の本文が空白だけ。
            (vec![edit(0, 30, "直した"), edit(40, 70, "  ")], "2 行目"),
        ];
        for (edits, row) in refused {
            let error = session.apply_transcription(&edits, true).unwrap_err();
            assert!(error.message().starts_with(row), "{error}");
            assert!(session.project().subtitles.is_none());
            assert_eq!(edits_of(&session), original, "失敗で候補が変わりました");
        }
        // 直した結果、2 行が重なる。行の順と時刻の順が逆でも、画面の行番号で示す。
        for overlap in [
            [edit(0, 50, "直した"), edit(40, 70, "二行目")],
            [edit(40, 70, "直した"), edit(0, 50, "二行目")],
        ] {
            let error = session.apply_transcription(&overlap, true).unwrap_err();
            assert_eq!(error.message(), "1 行目と 2 行目の区間が重なっています");
            assert!(session.project().subtitles.is_none());
            assert_eq!(edits_of(&session), original);
        }
        // 画面の行数が候補と合わない（削除が画面と Session でずれた）。
        let error = session
            .apply_transcription(&[edit(0, 30, "直した")], true)
            .unwrap_err();
        assert!(error.message().contains("数が合いません"), "{error}");
        assert!(session.project().subtitles.is_none());
        std::fs::remove_dir_all(&directory).unwrap();
    }

    #[test]
    fn the_overlap_names_the_rows_even_when_they_are_far_apart() {
        // 1 行目が長く、3 行目と重なる（2 行目とは重ならない並び）。
        let edits = [edit(0, 100, "a"), edit(200, 230, "b"), edit(90, 95, "c")];
        let error = check_candidate_overlaps(&edits).unwrap_err();
        assert_eq!(error.message(), "1 行目と 3 行目の区間が重なっています");
        assert!(check_candidate_overlaps(&[edit(0, 30, "a"), edit(30, 60, "b")]).is_ok());
    }

    #[test]
    fn a_refused_append_changes_nothing_including_the_id_serial() {
        let (mut session, directory) = two_candidates("transcribe-serial");
        // 認識の前から字幕がある。追加すると 2 行目と重なる。
        let candidates = session.background.transcription_result.take();
        session.add_subtitle(60, 90, "既存").unwrap();
        let mut outcome = candidates.unwrap();
        outcome.base_revision = session.edit_revision;
        session.background.transcription_result = Some(outcome);
        let serial = session.id_serial;
        let project = session.project().clone();
        let can_undo = session.can_undo();
        let original = edits_of(&session);

        let error = session
            .apply_transcription(&original, false)
            .expect_err("既存の字幕と重なる追加を受け付けました");
        assert_eq!(
            error.message(),
            "2 行目: 既にある字幕（60–90 フレーム）と重なります"
        );
        assert_eq!(
            session.id_serial, serial,
            "失敗で ID の serial が進みました"
        );
        assert_eq!(session.project(), &project);
        assert_eq!(session.can_undo(), can_undo);
        assert_eq!(edits_of(&session), original);
        // 置き換えなら既存の字幕は消えるので適用でき、serial はそこで初めて進む。
        session.apply_transcription(&original, true).unwrap();
        assert!(session.id_serial > serial);
        std::fs::remove_dir_all(&directory).unwrap();
    }

    #[test]
    fn a_removed_row_and_an_edited_row_apply_together() {
        let (mut session, directory) = two_candidates("transcribe-remove");
        session.remove_transcription_candidate(0);
        session
            .apply_transcription(&[edit(45, 75, "直した二行目")], false)
            .unwrap();
        let cues = &session.project().subtitles.as_ref().unwrap().cues;
        assert_eq!(cues.len(), 1);
        assert_eq!(
            (
                cues[0].start_frame,
                cues[0].end_frame,
                cues[0].content.as_str()
            ),
            (45, 75, "直した二行目")
        );
        std::fs::remove_dir_all(&directory).unwrap();
    }

    #[test]
    fn candidates_are_refused_after_an_edit_or_a_replaced_source() {
        let directory =
            std::env::temp_dir().join(format!("mvm-transcribe-swap-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let media = directory.join("voice.wav");
        std::fs::write(&media, b"first").unwrap();
        let mut session = Session::from_project(Project::default());
        let outcome = |session: &Session| TranscriptionOutcome {
            cues: vec![SubtitleCue {
                id: "candidate-1".to_owned(),
                start_frame: 0,
                end_frame: 30,
                content: "候補".to_owned(),
                link_clip_id: String::new(),
            }],
            base_revision: session.edit_revision,
            media: media.clone(),
            media_probe: probe_file(&media),
        };
        // 認識の後に素材を差し替えた（大きさが変わった）。
        session.background.transcription_result = Some(outcome(&session));
        std::fs::write(&media, b"replaced").unwrap();
        let error = session
            .apply_transcription(&edits_of(&session), true)
            .unwrap_err();
        assert!(error.message().contains("素材が変わりました"), "{error}");
        assert!(session.project().subtitles.is_none());
        // 認識の後に Project を編集した。
        session.background.transcription_result = Some(outcome(&session));
        session.add_marker().unwrap();
        assert!(
            session
                .apply_transcription(&edits_of(&session), true)
                .is_err()
        );
        assert!(session.project().subtitles.is_none());
        // どちらも変わっていなければ適用できる（候補の仮の ID は発行し直す）。
        session.background.transcription_result = Some(outcome(&session));
        session
            .apply_transcription(&edits_of(&session), true)
            .unwrap();
        let cues = &session.project().subtitles.as_ref().unwrap().cues;
        assert_eq!(cues.len(), 1);
        assert_ne!(cues[0].id, "candidate-1");
        std::fs::remove_dir_all(&directory).unwrap();
    }

    #[test]
    fn progress_lines_are_read_from_the_end() {
        let log = "load\nwhisper_print_progress_callback: progress =  10%\nx: progress = 55%\n";
        assert_eq!(last_progress(log), Some(55));
        assert_eq!(last_progress("nothing"), None);
    }

    #[test]
    fn clip_targets_use_the_trim_range_and_link_to_the_clip() {
        let clip = TimelineClip {
            kind: TimelineClipKind::Audio,
            media_path: PathBuf::from("voice.wav"),
            name: "voice".to_owned(),
            id: "voice".to_owned(),
            media_item_id: "item".to_owned(),
            source_fps: Rational::new(30, 1),
            source_frame_count: 300,
            source_in_frame: 60,
            source_out_frame: 150,
            timeline_start_frame: 120,
            track: TrackRef::audio(0),
            speed: Rational::new(1, 2),
            ..TimelineClip::default()
        };
        let session = Session::from_project(Project {
            timeline_clips: vec![clip],
            media_items: vec![MediaItem {
                id: "item".to_owned(),
                kind: MediaKind::Audio,
                media_path: PathBuf::from("voice.wav"),
                ..MediaItem::default()
            }],
            ..Project::default()
        });
        let target = session
            .transcription_target(&TranscriptionSource::Clip {
                clip_id: "voice".to_owned(),
            })
            .unwrap();
        assert_eq!((target.begin_ms, target.end_ms), (2000, Some(5000)));
        assert_eq!(target.link_clip_id, "voice");
        assert_eq!(target.offset_frame, 120);
        // 50% 速度の clip で素材の 1 秒は timeline の 2 秒（60 fps で 120 frame）。
        let cues = subtitle_cues_from_timed_text(
            &[SubtitleTimedText {
                start_ms: 0,
                end_ms: 1000,
                content: "a".to_owned(),
            }],
            Rational::new(60, 1),
            target.speed,
            target.offset_frame,
            &mut || "s1".to_owned(),
        )
        .unwrap();
        assert_eq!((cues[0].start_frame, cues[0].end_frame), (120, 240));

        let media = session
            .transcription_target(&TranscriptionSource::Media {
                item_id: "item".to_owned(),
                place_frame: 30,
            })
            .unwrap();
        assert_eq!(
            (media.begin_ms, media.end_ms, media.offset_frame),
            (0, None, 30)
        );
        assert!(media.link_clip_id.is_empty());
    }

    #[test]
    fn missing_tools_are_rejected_before_starting() {
        let mut session = Session::from_project(Project::default());
        let error = session
            .start_transcription(TranscriptionRequest {
                source: TranscriptionSource::Clip {
                    clip_id: "x".to_owned(),
                },
                whisper_cli: PathBuf::from("C:\\mvm-surely-missing\\whisper-cli.exe"),
                model: PathBuf::from("C:\\mvm-surely-missing\\model.bin"),
                language: "ja".to_owned(),
                gpu: false,
                prompt: String::new(),
            })
            .unwrap_err();
        assert!(error.message().contains("whisper-cli"), "{error}");
        assert!(session.transcription_progress().is_none());
    }

    /// 実 whisper.cpp と model で英語の合成音声を認識する。環境変数で tool と model と音声を指定する。
    #[test]
    #[ignore = "workstation"]
    fn recognizes_speech_with_the_real_whisper() {
        let (Some(cli), Some(model), Some(audio)) = (
            std::env::var_os("MVM_WHISPER_CLI"),
            std::env::var_os("MVM_WHISPER_MODEL"),
            std::env::var_os("MVM_TRANSCRIBE_AUDIO"),
        ) else {
            panic!("MVM_WHISPER_CLI・MVM_WHISPER_MODEL・MVM_TRANSCRIBE_AUDIO を指定してください");
        };
        let request = TranscriptionRequest {
            source: TranscriptionSource::Media {
                item_id: "item".to_owned(),
                place_frame: 0,
            },
            whisper_cli: PathBuf::from(cli),
            model: PathBuf::from(model),
            language: "en".to_owned(),
            gpu: false,
            prompt: String::new(),
        };
        let target = Target {
            media: PathBuf::from(audio),
            begin_ms: 0,
            end_ms: None,
            speed: Rational::new(1, 1),
            offset_frame: 0,
            link_clip_id: String::new(),
        };
        let outcome = transcribe(
            &request,
            &target,
            Rational::new(60, 1),
            1,
            &JobControl::default(),
        )
        .unwrap();
        let text: String = outcome
            .cues
            .iter()
            .map(|cue| cue.content.as_str())
            .collect::<Vec<_>>()
            .join(" ");
        println!("{text}");
        assert!(text.to_lowercase().contains("caption"), "{text}");
        assert!(
            outcome
                .cues
                .iter()
                .all(|cue| cue.end_frame > cue.start_frame)
        );
    }
}
