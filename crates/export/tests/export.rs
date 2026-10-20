//! 実 GPU で書き出し、書いた MP4 を読み直して確かめる。
//!
//! 映像の期待値は frame marker（素材に焼いた frame 番号）を手計算した対応と比べ、画素は preview の
//! 合成（同じ Project を [`StillPreview`] で描いたもの）と比べる。音声は試験素材の正弦波の式から決める。

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use mvm_core::Rational;
use mvm_export::{
    ExportControl, ExportError, ExportJob, ExportQuality, ExportRequest, TEMPORARY_MARKER,
    plan_export, run_export,
};
use mvm_media::PcmStream;
use mvm_preview::{MathLibrary, PreviewImage, StillPreview, plan_frame};
use mvm_project::timeline::insert_frame_hold;
use mvm_project::{
    Project, SubtitleCue, SubtitleStyle, SubtitleTrack, TimelineClip, TimelineClipKind, TrackRef,
};
use mvm_testkit::{read_frame_marker, smoke_dir};

/// この file の試験は実 GPU の device と D3D12VA の decoder を作る。並行に作ると D3D12VA の初期化が
/// 時々 `Invalid argument` で失敗する（[事実] 2026-10-20、6 本を並行に走らせて 3 回中 2 回）ので、
/// 1 本ずつ走らせる。
static GPU: Mutex<()> = Mutex::new(());

fn gpu() -> std::sync::MutexGuard<'static, ()> {
    GPU.lock().unwrap_or_else(|poison| poison.into_inner())
}

/// 画素の平均差（0..=255）の上限。CRF 18 の H.264 と YUV420 の往復で生じる差だけを許す。
const PARITY_MEAN_LIMIT: f64 = 3.0;

fn smoke(name: &str) -> PathBuf {
    let path = smoke_dir().join(name);
    assert!(
        path.is_file(),
        "検証素材がありません ({}). 先に .\\dev.ps1 testmedia を実行してください",
        path.display()
    );
    path
}

/// `output` の隣に残った encode 中の file。
fn temporaries(output: &Path) -> Vec<PathBuf> {
    let prefix = format!(
        "{}{TEMPORARY_MARKER}",
        output.file_name().unwrap().to_string_lossy()
    );
    std::fs::read_dir(output.parent().unwrap())
        .unwrap()
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.file_name().to_string_lossy().starts_with(&prefix))
        .map(|entry| entry.path())
        .collect()
}

fn work_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("mvm-export-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("作業 directory");
    dir
}

fn video_clip(id: &str, path: &Path, start: i64, source_in: i64, source_out: i64) -> TimelineClip {
    TimelineClip {
        kind: TimelineClipKind::Video,
        media_path: path.to_path_buf(),
        name: id.to_owned(),
        id: id.to_owned(),
        source_fps: Rational::new(60, 1),
        source_frame_count: 300,
        source_in_frame: source_in,
        source_out_frame: source_out,
        timeline_start_frame: start,
        track: TrackRef::video(0),
        ..TimelineClip::default()
    }
}

fn request(path: &Path, quality: ExportQuality) -> ExportRequest {
    ExportRequest {
        output_path: path.to_path_buf(),
        quality,
        burn_subtitles: true,
    }
}

/// 取り消さず、最後の進捗を覚える。
#[derive(Default)]
struct Recorder {
    last: Mutex<Option<(i64, i64)>>,
    cancel_at: Option<i64>,
    cancel: AtomicBool,
}

impl ExportControl for Recorder {
    fn progress(&self, completed: i64, total: i64) {
        *self.last.lock().unwrap() = Some((completed, total));
        if self.cancel_at.is_some_and(|at| completed >= at) {
            self.cancel.store(true, Ordering::Release);
        }
    }

    fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Acquire)
    }
}

/// 書き出した MP4 を 1 本の clip として preview で描く。
fn read_back(path: &Path, frames: i64) -> Vec<PreviewImage> {
    let project = Project {
        timeline_clips: vec![TimelineClip {
            source_frame_count: frames,
            ..video_clip("exported", path, 0, 0, frames)
        }],
        ..Project::default()
    };
    render_all(&project, frames)
}

fn render_all(project: &Project, frames: i64) -> Vec<PreviewImage> {
    let mut preview = StillPreview::new().expect("DX12 device がありません");
    (0..frames)
        .map(|frame| {
            let plan = plan_frame(project, frame, &MathLibrary::default()).expect("計画");
            preview.render(project, frame, &plan).expect("合成")
        })
        .collect()
}

fn marker(image: &PreviewImage) -> u16 {
    let stride = usize::try_from(image.width).unwrap() * 4;
    read_frame_marker(&image.rgba, image.width, image.height, stride).expect("帯")
}

/// RGB の平均差。alpha は両方とも不透明なので比べない。
fn mean_difference(left: &PreviewImage, right: &PreviewImage) -> f64 {
    assert_eq!((left.width, left.height), (right.width, right.height));
    let mut sum = 0u64;
    let mut count = 0u64;
    for (a, b) in left
        .rgba
        .as_chunks::<4>()
        .0
        .iter()
        .zip(right.rgba.as_chunks::<4>().0)
    {
        for channel in 0..3 {
            sum += u64::from(a[channel].abs_diff(b[channel]));
            count += 1;
        }
    }
    sum as f64 / count as f64
}

#[test]
#[ignore = "workstation"]
fn export_matches_the_preview_and_the_hand_computed_source_frames() {
    let _gpu = gpu();
    let media = smoke("v1080p60_h264.mp4");
    let dir = work_dir("parity");
    let output = dir.join("parity.mp4");
    // 正規名に古い file があっても置き換える。
    std::fs::write(&output, b"old").expect("古い file");

    // 0..30: 200% で素材 0..60。30..: 素材 100.. を等速で、40 に 20 frame の保持を挿入する。
    let mut fast = video_clip("fast", &media, 0, 0, 60);
    fast.speed = Rational::new(2, 1);
    let mut project = Project {
        timeline_clips: vec![fast, video_clip("hold", &media, 30, 100, 160)],
        ..Project::default()
    };
    let mut serial = 0;
    insert_frame_hold(&mut project, "hold", 40, 20, &mut || {
        serial += 1;
        format!("new-{serial}")
    })
    .expect("保持の挿入");
    let total = 110;
    let expected = |frame: i64| -> i64 {
        match frame {
            0..30 => frame * 2,
            30..40 => 100 + (frame - 30),
            40..60 => 110,
            _ => 110 + (frame - 60),
        }
    };

    let plan = plan_export(
        &project,
        &request(&output, ExportQuality::High),
        std::sync::Arc::new(MathLibrary::default()),
    )
    .expect("計画");
    assert_eq!(plan.total_frames, total);
    let recorder = Recorder::default();
    let summary = run_export(&project, &plan, &recorder).expect("書き出し");
    assert_eq!(summary.frames, total);
    assert_eq!(*recorder.last.lock().unwrap(), Some((total, total)));
    assert!(temporaries(&output).is_empty(), "一時 file が残っています");
    assert!(
        std::fs::metadata(&output).unwrap().len() > 1000,
        "古い file のままです"
    );

    let exported = read_back(&output, total);
    let previewed = render_all(&project, total);
    let mut worst = 0f64;
    for frame in 0..total {
        let index = usize::try_from(frame).unwrap();
        let want = u16::try_from(expected(frame)).unwrap();
        assert_eq!(marker(&previewed[index]), want, "preview の frame {frame}");
        assert_eq!(marker(&exported[index]), want, "書き出しの frame {frame}");
        let difference = mean_difference(&exported[index], &previewed[index]);
        worst = worst.max(difference);
        assert!(
            difference < PARITY_MEAN_LIMIT,
            "frame {frame} の画素が preview と離れています: 平均差 {difference:.3}"
        );
    }
    // 負の対照: 別の素材 frame どうしなら、この指標は上限を超える。
    let unrelated = mean_difference(&exported[0], &previewed[35]);
    assert!(
        unrelated > PARITY_MEAN_LIMIT,
        "別の frame との差 {unrelated:.3} が上限を下回り、指標が画素の違いを捉えていません"
    );
    eprintln!("書き出しと preview の平均差の最大 {worst:.3}、別の frame との差 {unrelated:.3}");
    std::fs::remove_dir_all(&dir).expect("片付け");
}

#[test]
#[ignore = "workstation"]
fn exported_audio_follows_the_clip_volume_and_the_track_pan() {
    let _gpu = gpu();
    let media = smoke("wav_48k.wav");
    let dir = work_dir("audio");
    let output = dir.join("audio.mp4");
    let mut clip = TimelineClip {
        kind: TimelineClipKind::Audio,
        media_path: media,
        name: "音声".to_owned(),
        id: "audio".to_owned(),
        source_fps: Rational::new(60, 1),
        source_frame_count: 300,
        source_in_frame: 0,
        source_out_frame: 60,
        timeline_start_frame: 0,
        track: TrackRef::audio(0),
        ..TimelineClip::default()
    };
    clip.effects.volume_percent = 50.0;
    let mut project = Project {
        timeline_clips: vec![clip],
        ..Project::default()
    };
    // 全部左へ。素材の右（750 Hz）は出ない。
    project.audio_tracks[0].mixer_pan = -1.0;

    let plan = plan_export(
        &project,
        &request(&output, ExportQuality::Standard),
        std::sync::Arc::new(MathLibrary::default()),
    )
    .expect("計画");
    assert!(plan.has_audio());
    let summary = run_export(&project, &plan, &Recorder::default()).expect("書き出し");
    assert_eq!(summary.audio_samples, 60 * 800);

    let mut stream = PcmStream::open(&output).expect("音声");
    let mut left = vec![0f32; 24_000];
    let mut right = vec![0f32; 24_000];
    stream
        .copy_from(12_000, &mut left, &mut right)
        .expect("読む");
    // 素材の左は 0.5 sin(2π 1500 t)。音量 50% で振幅 0.25、RMS は 0.25 / √2。
    let expected_rms = 0.25 / 2f32.sqrt();
    let left_rms = rms(&left);
    assert!(
        (left_rms - expected_rms).abs() < expected_rms * 0.05,
        "左の RMS {left_rms}（期待 {expected_rms}）"
    );
    // 0.5 秒の 1500 Hz は 1500 回符号が変わる。750 Hz が混ざると数が変わる。
    let crossings = left
        .windows(2)
        .filter(|pair| (pair[0] < 0.0) != (pair[1] < 0.0))
        .count();
    assert!(crossings.abs_diff(1500) <= 6, "符号の変化 {crossings} 回");
    assert!(rms(&right) < 0.005, "右に音が出ています: {}", rms(&right));
    drop(stream);
    std::fs::remove_dir_all(&dir).expect("片付け");
}

#[test]
#[ignore = "workstation"]
fn burned_subtitles_appear_only_inside_the_cue() {
    let _gpu = gpu();
    let dir = work_dir("subtitle");
    let project = Project {
        subtitles: Some(SubtitleTrack {
            visible: true,
            style: SubtitleStyle::default(),
            cues: vec![SubtitleCue {
                id: "cue".to_owned(),
                start_frame: 5,
                end_frame: 15,
                content: "字幕の焼き込み".to_owned(),
                link_clip_id: String::new(),
            }],
        }),
        ..Project::default()
    };
    let burned = dir.join("burned.mp4");
    let plan = plan_export(
        &project,
        &request(&burned, ExportQuality::High),
        std::sync::Arc::new(MathLibrary::default()),
    )
    .expect("計画");
    // job 経由でも同じ書き出しになる。
    let summary = ExportJob::start(std::sync::Arc::new(project.clone()), plan, None)
        .expect("開始")
        .wait()
        .expect("書き出し");
    assert_eq!(summary.frames, 15);
    let frames = read_back(&burned, 15);
    for (frame, image) in frames.iter().enumerate() {
        let bright = bright_pixels(image);
        if (5..15).contains(&frame) {
            assert!(bright > 500, "frame {frame} に字幕がありません ({bright})");
        } else {
            assert_eq!(bright, 0, "frame {frame} に字幕があります");
        }
    }

    let plain = dir.join("plain.mp4");
    let mut unburned = request(&plain, ExportQuality::High);
    unburned.burn_subtitles = false;
    let plan = plan_export(
        &project,
        &unburned,
        std::sync::Arc::new(MathLibrary::default()),
    )
    .expect("計画");
    run_export(&project, &plan, &Recorder::default()).expect("焼き込まない書き出し");
    let frames = read_back(&plain, 15);
    assert!(
        frames.iter().all(|image| bright_pixels(image) == 0),
        "焼き込まない書き出しに字幕があります"
    );
    std::fs::remove_dir_all(&dir).expect("片付け");
}

#[test]
#[ignore = "workstation"]
fn cancelling_or_failing_leaves_no_file_behind() {
    let _gpu = gpu();
    let media = smoke("v1080p60_h264.mp4");
    let dir = work_dir("cancel");
    let output = dir.join("cancelled.mp4");
    let project = Project {
        timeline_clips: vec![video_clip("clip", &media, 0, 0, 120)],
        ..Project::default()
    };
    let plan = plan_export(
        &project,
        &request(&output, ExportQuality::Compact),
        std::sync::Arc::new(MathLibrary::default()),
    )
    .expect("計画");
    let recorder = Recorder {
        cancel_at: Some(5),
        ..Recorder::default()
    };
    let error = run_export(&project, &plan, &recorder).unwrap_err();
    assert_eq!(error, ExportError::Cancelled);
    assert!(
        !output.exists() && temporaries(&output).is_empty(),
        "取消で file が残りました"
    );

    // job を drop すると取り消して待つ。
    let job =
        ExportJob::start(std::sync::Arc::new(project.clone()), plan.clone(), None).expect("開始");
    job.cancel();
    assert!(job.wait().unwrap_err().is_cancelled());
    assert!(
        !output.exists() && temporaries(&output).is_empty(),
        "job の取消で file が残りました"
    );

    let missing = Project {
        timeline_clips: vec![video_clip("missing", &dir.join("no-such.mp4"), 0, 0, 30)],
        ..Project::default()
    };
    let failed = dir.join("failed.mp4");
    let plan = plan_export(
        &missing,
        &request(&failed, ExportQuality::Compact),
        std::sync::Arc::new(MathLibrary::default()),
    )
    .expect("計画");
    let error = run_export(&missing, &plan, &Recorder::default()).unwrap_err();
    assert!(
        matches!(&error, ExportError::Failed(message) if message.contains("frame 0")),
        "{error}"
    );
    assert!(
        !failed.exists() && temporaries(&failed).is_empty(),
        "失敗で file が残りました"
    );
    std::fs::remove_dir_all(&dir).expect("片付け");
}

fn bright_pixels(image: &PreviewImage) -> usize {
    image
        .rgba
        .as_chunks::<4>()
        .0
        .iter()
        .filter(|pixel| pixel[0] > 200 && pixel[1] > 200 && pixel[2] > 200)
        .count()
}

fn rms(samples: &[f32]) -> f32 {
    (samples.iter().map(|sample| sample * sample).sum::<f32>() / samples.len() as f32).sqrt()
}

/// 赤（R が高く G・B が低い）の画素の数。数式と Graph の曲線を赤で描いて数える。
fn red_pixels(image: &PreviewImage) -> usize {
    image
        .rgba
        .as_chunks::<4>()
        .0
        .iter()
        .filter(|pixel| pixel[0] > 150 && pixel[1] < 90 && pixel[2] < 90)
        .count()
}

fn render_with(project: &Project, frames: i64, math: &MathLibrary) -> Vec<PreviewImage> {
    let mut preview = StillPreview::new().expect("DX12 device がありません");
    (0..frames)
        .map(|frame| {
            let plan = plan_frame(project, frame, math).expect("計画");
            assert_eq!(plan.skipped_clips, 0, "frame {frame}: {:?}", plan.notes);
            preview.render(project, frame, &plan).expect("合成")
        })
        .collect()
}

/// 実 Manim で数式（先頭 10 frame は Write）を描き、書き出した MP4 で Write の進みと静止の式が出ること、
/// preview の合成と画素が一致することを確かめる。
#[test]
#[ignore = "workstation"]
fn math_clips_are_exported_with_the_rendered_masks() {
    let _gpu = gpu();
    use mvm_math::{MathService, MathStatus, wanted_jobs};
    use mvm_project::math_clip::{MathClipAnimation, MathClipData, MathIntroKind};

    let dir = work_dir("math");
    let clip = TimelineClip {
        kind: TimelineClipKind::Math,
        name: "式".to_owned(),
        id: "math".to_owned(),
        source_fps: Rational::new(60, 1),
        source_frame_count: 20,
        source_in_frame: 0,
        source_out_frame: 20,
        track: TrackRef::video(0),
        math: MathClipData {
            source: "x^2 + y^2 = r^2".to_owned(),
            font_size: 120,
            color: "#FFFF0000".to_owned(),
            ..MathClipData::default()
        },
        math_animation: MathClipAnimation {
            intro: MathIntroKind::Write,
            intro_frames: 10,
        },
        ..TimelineClip::default()
    };
    let project = Project {
        timeline_clips: vec![clip],
        ..Project::default()
    };
    let service = MathService::start(dir.join("cache"), None).expect("描画の service");
    let jobs = wanted_jobs(&project, 0);
    assert_eq!(jobs.len(), 2, "静止と Write");
    service.request(jobs.clone());
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(300);
    while jobs
        .iter()
        .any(|job| service.status(job) != MathStatus::Ready)
    {
        assert!(std::time::Instant::now() < deadline, "描画が終わりません");
        for job in &jobs {
            if let status @ (MathStatus::Failed { .. }
            | MathStatus::InvalidSource { .. }
            | MathStatus::Unavailable(_)) = service.status(job)
            {
                panic!("描けません: {status:?}");
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    let library = service.library();

    // 描画が無ければ計画の時点で止まる（負の対照）。
    let output = dir.join("math.mp4");
    assert!(
        plan_export(
            &project,
            &request(&output, ExportQuality::High),
            std::sync::Arc::new(MathLibrary::default()),
        )
        .is_err()
    );
    let plan = plan_export(
        &project,
        &request(&output, ExportQuality::High),
        std::sync::Arc::clone(&library),
    )
    .expect("計画");
    let summary = run_export(&project, &plan, &Recorder::default()).expect("書き出し");
    assert_eq!(summary.frames, 20);

    let exported = read_back(&output, 20);
    let expected = render_with(&project, 20, &library);
    let counts: Vec<usize> = exported.iter().map(red_pixels).collect();
    // Write の 0 枚目は何も書いていない。進むほど増え、Write の後は静止の式。
    assert!(counts[0] < 50, "frame 0 に式があります ({})", counts[0]);
    assert!(counts[5] > counts[1], "Write が進みません {counts:?}");
    assert!(
        counts[15] > counts[5],
        "静止の式が Write の途中より少ない {counts:?}"
    );
    assert!(
        counts[15] > 2_000,
        "静止の式が小さすぎます ({})",
        counts[15]
    );
    for frame in [0usize, 5, 15, 19] {
        let difference = mean_difference(&exported[frame], &expected[frame]);
        assert!(
            difference < PARITY_MEAN_LIMIT,
            "frame {frame}: preview との平均差 {difference:.3}"
        );
    }
    // 指標の負の対照: Write の途中と静止は区別できる。
    assert!(mean_difference(&expected[2], &expected[15]) > 0.05);
    drop(service);
    std::fs::remove_dir_all(&dir).expect("片付け");
}

/// Graph（ラベル無し）を書き出し、曲線が plot の中に出ることと preview との一致を確かめる。
#[test]
#[ignore = "workstation"]
fn graph_clips_are_exported_like_the_preview() {
    let _gpu = gpu();
    use mvm_project::graph_clip::{GraphClipData, GraphFunction, GraphFunctionId};

    let dir = work_dir("graph");
    let mut graph = GraphClipData::with_default_function(GraphFunctionId("f".to_owned()));
    graph.axes.x_label = String::new();
    graph.axes.y_label = String::new();
    graph.functions[0] = GraphFunction {
        id: GraphFunctionId("f".to_owned()),
        expression: "x".to_owned(),
        color: "#FFFF0000".to_owned(),
        stroke_width: 8.0,
        ..GraphFunction::default()
    };
    let project = Project {
        timeline_clips: vec![TimelineClip {
            kind: TimelineClipKind::Graph,
            name: "グラフ".to_owned(),
            id: "graph".to_owned(),
            source_fps: Rational::new(60, 1),
            source_frame_count: 6,
            source_in_frame: 0,
            source_out_frame: 6,
            track: TrackRef::video(0),
            graph,
            ..TimelineClip::default()
        }],
        ..Project::default()
    };
    let output = dir.join("graph.mp4");
    let library = std::sync::Arc::new(MathLibrary::default());
    let plan =
        plan_export(&project, &request(&output, ExportQuality::High), library).expect("計画");
    run_export(&project, &plan, &Recorder::default()).expect("書き出し");
    let exported = read_back(&output, 6);
    let expected = render_with(&project, 6, &MathLibrary::default());
    // 1920x1080 の plot は (240, 135) から 1440x810。y = x は plot の対角線で、(5, 5) の端は (1680, 135)。
    let red = red_pixels(&exported[3]);
    assert!(red > 3_000, "曲線が見えません ({red})");
    let at = |image: &PreviewImage, x: u32, y: u32| {
        let index = ((y * image.width + x) * 4) as usize;
        [
            image.rgba[index],
            image.rgba[index + 1],
            image.rgba[index + 2],
        ]
    };
    let center = at(&exported[3], 960, 540);
    assert!(center[0] > 150 && center[1] < 90, "中央 {center:?}");
    assert!(
        mean_difference(&exported[3], &expected[3]) < PARITY_MEAN_LIMIT,
        "preview と違います"
    );
    std::fs::remove_dir_all(&dir).expect("片付け");
}
