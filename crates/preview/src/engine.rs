//! 再生の状態機械。UI thread は command を送り、mailbox の最新 frame を読む。
//!
//! 止めるときは WASAPI の thread を join してから compositor を捨てる。
//! 鳴っている端点より先に GPU を落とさない。

use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use mvm_audio::{ScheduleAction, schedule_video};
use mvm_math::MathLibrary;
use mvm_project::Project;

use crate::audio_plan::{audio_spans, output_timebase, playback_end};
use crate::error::PreviewError;
use crate::mix::{AudioMeters, MeterPeaks, MonitorVolume, start_audio};
use crate::plan::{PreviewImage, plan_frame};
use crate::render::StillPreview;

enum Command {
    Project {
        project: Arc<Project>,
        math: Arc<MathLibrary>,
        generation: u64,
    },
    /// 描けた数式を差し替える。再生は止めず、次に描く frame から使う。
    Math {
        math: Arc<MathLibrary>,
    },
    Seek {
        frame: i64,
        generation: u64,
    },
    Play {
        frame: i64,
        generation: u64,
    },
    Pause {
        generation: u64,
    },
    Shutdown,
    /// worker の異常終了を試験で起こす。
    #[cfg(test)]
    Panic {
        generation: u64,
    },
}

impl Command {
    fn generation(&self) -> Option<u64> {
        match self {
            Self::Project { generation, .. }
            | Self::Seek { generation, .. }
            | Self::Play { generation, .. }
            | Self::Pause { generation } => Some(*generation),
            #[cfg(test)]
            Self::Panic { generation } => Some(*generation),
            Self::Shutdown | Self::Math { .. } => None,
        }
    }
}

/// 1 回に受けた command のうち、後に Seek / Play（どちらも描く）が続く Seek を除く。除いた Seek の frame と
/// 世代は後の command が上書きするので、描かないこと以外は変わらない。
fn coalesce_seeks(batch: Vec<Command>) -> Vec<Command> {
    let last_draw = batch
        .iter()
        .rposition(|command| matches!(command, Command::Seek { .. } | Command::Play { .. }));
    batch
        .into_iter()
        .enumerate()
        .filter(|(index, command)| {
            !matches!(command, Command::Seek { .. }) || Some(*index) == last_draw
        })
        .map(|(_, command)| command)
        .collect()
}

struct Mail {
    lock: Mutex<MailInner>,
    changed: Condvar,
    wake: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
    /// UI が最後に送った command の世代。worker が panic したら、この世代で知らせる。
    requested: AtomicU64,
}

struct MailInner {
    generation: u64,
    image: Option<PreviewImage>,
    error: Option<String>,
    playing: Option<bool>,
}

/// UI が 1 回の poll で受け取る、その時点の最新の通知。
#[derive(Clone, Debug)]
pub struct PreviewDelivery {
    pub generation: u64,
    pub image: Option<PreviewImage>,
    pub error: Option<String>,
    pub playing: Option<bool>,
}

impl Mail {
    fn new() -> Self {
        Self {
            lock: Mutex::new(MailInner {
                generation: 0,
                image: None,
                error: None,
                playing: None,
            }),
            changed: Condvar::new(),
            wake: Mutex::new(None),
            requested: AtomicU64::new(0),
        }
    }

    fn set_wake(&self, wake: Arc<dyn Fn() + Send + Sync>) {
        *self
            .wake
            .lock()
            .unwrap_or_else(|poison| poison.into_inner()) = Some(wake);
    }

    /// lock を離してから UI を起こす。起こした先が poll で同じ lock を取る。
    fn wake_ui(&self) {
        self.changed.notify_all();
        let wake = self
            .wake
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .clone();
        if let Some(wake) = wake {
            wake();
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, MailInner> {
        self.lock
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }

    /// 世代が変わったら、前の世代の通知を捨ててから書く。古い画像を新しい世代として渡さない。
    fn enter(&self, generation: u64) -> std::sync::MutexGuard<'_, MailInner> {
        let mut inner = self.lock();
        if inner.generation != generation {
            inner.generation = generation;
            inner.image = None;
            inner.error = None;
            inner.playing = None;
        }
        inner
    }

    fn publish_image(&self, generation: u64, image: PreviewImage) {
        let mut inner = self.enter(generation);
        inner.image = Some(image);
        inner.error = None;
        drop(inner);
        self.wake_ui();
    }

    fn publish_error(&self, generation: u64, message: String) {
        let mut inner = self.enter(generation);
        inner.error = Some(message);
        inner.playing = Some(false);
        drop(inner);
        self.wake_ui();
    }

    fn publish_playing(&self, generation: u64, playing: bool) {
        let mut inner = self.enter(generation);
        inner.playing = Some(playing);
        drop(inner);
        self.wake_ui();
    }

    fn take(&self) -> Option<PreviewDelivery> {
        let mut inner = self.lock();
        if inner.image.is_none() && inner.error.is_none() && inner.playing.is_none() {
            return None;
        }
        Some(PreviewDelivery {
            generation: inner.generation,
            image: inner.image.take(),
            error: inner.error.take(),
            playing: inner.playing.take(),
        })
    }
}

struct Worker {
    project: Arc<Project>,
    math: Arc<MathLibrary>,
    generation: u64,
    frame: i64,
    playing: bool,
    wall: Option<(Instant, i64)>,
    audio: Option<mvm_audio::WasapiSink>,
    preview: Option<StillPreview>,
    last_displayed: i64,
    mail: Arc<Mail>,
    meters: Arc<AudioMeters>,
    monitor: Arc<MonitorVolume>,
}

impl Worker {
    fn new(mail: Arc<Mail>, meters: Arc<AudioMeters>, monitor: Arc<MonitorVolume>) -> Self {
        Self {
            project: Arc::new(Project::default()),
            math: Arc::new(MathLibrary::default()),
            generation: 0,
            frame: 0,
            playing: false,
            wall: None,
            audio: None,
            preview: None,
            last_displayed: -1,
            mail,
            meters,
            monitor,
        }
    }

    fn run(mut self, commands: Receiver<Command>) {
        loop {
            let command = if self.playing {
                match commands.recv_timeout(Duration::from_millis(2)) {
                    Ok(command) => Some(command),
                    Err(RecvTimeoutError::Timeout) => None,
                    Err(RecvTimeoutError::Disconnected) => break,
                }
            } else {
                match commands.recv() {
                    Ok(command) => Some(command),
                    Err(_) => break,
                }
            };
            if let Some(command) = command {
                // 溜まった command をまとめて受け、後で描き直す Seek は描かずに飛ばす（シャトルやスクラブで seek が
                // 描画より速く届いても、待ち行列を伸ばさず最新の frame だけを描く）。
                let mut batch = vec![command];
                while let Ok(command) = commands.try_recv() {
                    batch.push(command);
                }
                if !coalesce_seeks(batch)
                    .into_iter()
                    .all(|command| self.handle(command))
                {
                    break;
                }
            }
            if self.playing
                && let Err(error) = self.tick()
            {
                self.fail(error);
            }
        }
        self.shutdown();
    }

    fn handle(&mut self, command: Command) -> bool {
        match command {
            Command::Shutdown => false,
            #[cfg(test)]
            #[allow(clippy::panic)]
            Command::Panic { .. } => panic!("試験で起こした異常終了"),
            Command::Project {
                project,
                math,
                generation,
            } => {
                // 描くのは続く Seek / Play。ここで描くと同じ frame を 2 回合成する。
                self.generation = generation;
                self.project = project;
                self.math = math;
                self.stop_playback();
                true
            }
            Command::Math { math } => {
                self.math = math;
                true
            }
            Command::Seek { frame, generation } => {
                self.generation = generation;
                let resume = self.playing;
                self.frame = frame;
                self.stop_playback();
                self.last_displayed = -1;
                if let Err(error) = self.render_current() {
                    self.fail(error);
                    return true;
                }
                self.last_displayed = self.frame;
                if resume && let Err(error) = self.start_playback() {
                    self.fail(error);
                }
                true
            }
            Command::Play { frame, generation } => {
                self.generation = generation;
                self.frame = frame;
                if let Err(error) = self.start_playback() {
                    self.fail(error);
                }
                true
            }
            Command::Pause { generation } => {
                self.generation = generation;
                self.stop_playback();
                true
            }
        }
    }

    fn start_playback(&mut self) -> Result<(), PreviewError> {
        self.stop_playback();
        let end = playback_end(&self.project)?;
        if self.frame < 0 || self.frame >= end {
            return Err(PreviewError::new("再生する区間がありません"));
        }
        self.render_current()?;
        self.last_displayed = self.frame;
        let spans = audio_spans(&self.project)?;
        if spans.is_empty() {
            self.wall = Some((Instant::now(), self.frame));
        } else {
            let timebase = output_timebase(&self.project)?;
            let start_sample = timebase
                .first_audio_sample(self.frame)
                .map_err(|error| PreviewError::new(error.to_string()))?;
            self.audio = Some(start_audio(
                spans,
                start_sample,
                timebase,
                self.project.timeline_fps,
                Arc::clone(&self.meters),
                Arc::clone(&self.monitor),
            )?);
        }
        self.playing = true;
        self.mail.publish_playing(self.generation, true);
        Ok(())
    }

    fn stop_playback(&mut self) {
        let was = self.playing;
        self.playing = false;
        self.wall = None;
        self.audio = None;
        if was {
            self.mail.publish_playing(self.generation, false);
        }
    }

    fn tick(&mut self) -> Result<(), PreviewError> {
        let timebase = output_timebase(&self.project)?;
        let end = playback_end(&self.project)?;
        let sample = if let Some(audio) = &self.audio {
            match audio
                .media_sample()
                .map_err(|error| PreviewError::new(error.to_string()))?
            {
                Some(sample) => sample,
                None => return Ok(()),
            }
        } else {
            let Some((origin, start_frame)) = self.wall else {
                return Err(PreviewError::new("再生の時計がありません"));
            };
            let elapsed = i64::try_from(origin.elapsed().as_nanos())
                .map_err(|_| PreviewError::new("再生時刻が overflow しました"))?;
            let delta = timebase
                .output_frame_for_nanoseconds(elapsed)
                .map_err(|error| PreviewError::new(error.to_string()))?;
            let target = start_frame
                .checked_add(delta)
                .ok_or_else(|| PreviewError::new("frame 番号が overflow しました"))?;
            timebase
                .first_audio_sample(target)
                .map_err(|error| PreviewError::new(error.to_string()))?
        };
        let decision = schedule_video(
            timebase,
            sample,
            self.last_displayed,
            self.last_displayed,
            end,
            -1,
        );
        match decision.action {
            ScheduleAction::Hold => Ok(()),
            ScheduleAction::Request | ScheduleAction::CatchUp => {
                self.frame = decision.target_frame;
                self.render_current()?;
                self.last_displayed = decision.target_frame;
                Ok(())
            }
            ScheduleAction::End => {
                self.stop_playback();
                Ok(())
            }
            // 外挿した時計より実測が少し遅れて届くことがある。表示済みの frame を保ち、
            // 時計が追いつくのを待つ（C++ 版も数えるだけで止めない）。
            ScheduleAction::ClockRegression => Ok(()),
            ScheduleAction::Invalid => Err(PreviewError::new("再生の時刻が不正です")),
        }
    }

    fn render_current(&mut self) -> Result<(), PreviewError> {
        let plan = plan_frame(&self.project, self.frame, &self.math)?;
        let image = if plan.layers.is_empty() {
            crate::plan::black_image(self.frame, &self.project)?
        } else {
            if self.preview.is_none() {
                self.preview = Some(StillPreview::new()?);
            }
            let preview = self
                .preview
                .as_mut()
                .ok_or_else(|| PreviewError::new("preview を用意できません"))?;
            preview.render(&self.project, self.frame, &plan)?
        };
        self.mail.publish_image(self.generation, image);
        Ok(())
    }

    fn fail(&mut self, error: PreviewError) {
        self.stop_playback();
        self.mail.publish_error(self.generation, error.to_string());
    }

    fn shutdown(&mut self) {
        self.stop_playback();
        self.preview = None;
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// preview の worker。`Drop` で command を閉じ、thread の終了を待つ。
pub struct PreviewEngine {
    commands: Sender<Command>,
    mail: Arc<Mail>,
    meters: Arc<AudioMeters>,
    monitor: Arc<MonitorVolume>,
    thread: Option<JoinHandle<()>>,
}

impl PreviewEngine {
    pub fn set_wake(&self, wake: Arc<dyn Fn() + Send + Sync>) {
        self.mail.set_wake(wake);
    }

    pub fn start() -> Result<Self, PreviewError> {
        let (commands, incoming) = mpsc::channel();
        let mail = Arc::new(Mail::new());
        let worker_mail = Arc::clone(&mail);
        let panic_mail = Arc::clone(&mail);
        let meters = Arc::new(AudioMeters::default());
        let monitor = Arc::new(MonitorVolume::default());
        let worker_meters = Arc::clone(&meters);
        let worker_monitor = Arc::clone(&monitor);
        let thread = std::thread::Builder::new()
            .name("mvm-preview".into())
            .spawn(move || {
                // decoder は thread の中で作る。FFmpeg の生ポインタを thread 越しに送らない。
                let finished = std::panic::catch_unwind(AssertUnwindSafe(move || {
                    Worker::new(worker_mail, worker_meters, worker_monitor).run(incoming);
                }));
                if finished.is_err() {
                    // UI は今の世代の通知だけを受け取る。0 などの古い世代で送ると捨てられ、
                    // 再生中のまま止まった worker が残る。
                    let generation = panic_mail.requested.load(Ordering::Acquire);
                    panic_mail.publish_error(generation, "preview が異常終了しました".to_owned());
                }
            })
            .map_err(|error| PreviewError::new(format!("preview を起動できません: {error}")))?;
        Ok(Self {
            commands,
            mail,
            meters,
            monitor,
            thread: Some(thread),
        })
    }

    /// 直前の呼び出しから後に再生した音の track ごとの peak（倍率）。
    pub fn take_meters(&self) -> MeterPeaks {
        self.meters.take()
    }

    /// 再生の出口の音量（倍率）。書き出しには掛けない。
    pub fn set_monitor_volume(&self, gain: f32) -> Result<(), PreviewError> {
        self.monitor.set(gain)
    }

    pub fn set_project(
        &self,
        project: Arc<Project>,
        math: Arc<MathLibrary>,
        generation: u64,
    ) -> Result<(), PreviewError> {
        self.send(Command::Project {
            project,
            math,
            generation,
        })
    }

    /// 描けた数式を差し替える。再生中も止めない。
    pub fn set_math(&self, math: Arc<MathLibrary>) -> Result<(), PreviewError> {
        self.send(Command::Math { math })
    }

    pub fn seek(&self, frame: i64, generation: u64) -> Result<(), PreviewError> {
        self.send(Command::Seek { frame, generation })
    }

    /// `frame` から再生する。worker が最後に描いた frame ではなく、UI の playhead を正にする。
    pub fn play(&self, frame: i64, generation: u64) -> Result<(), PreviewError> {
        self.send(Command::Play { frame, generation })
    }

    pub fn pause(&self, generation: u64) -> Result<(), PreviewError> {
        self.send(Command::Pause { generation })
    }

    pub fn poll(&self) -> Option<PreviewDelivery> {
        self.mail.take()
    }

    /// `accept` を満たす frame が届くまで待つ。期限を過ぎたら `None`。
    pub fn wait_image_until(
        &self,
        timeout: Duration,
        mut accept: impl FnMut(&PreviewImage) -> bool,
    ) -> Option<PreviewImage> {
        let deadline = Instant::now() + timeout;
        let mut guard = self.mail.lock();
        loop {
            if let Some(image) = &guard.image
                && accept(image)
            {
                return Some(image.clone());
            }
            let now = Instant::now();
            if now >= deadline {
                return None;
            }
            let waited = self.mail.changed.wait_timeout(guard, deadline - now);
            let (next, status) = match waited {
                Ok(pair) => pair,
                Err(poison) => poison.into_inner(),
            };
            guard = next;
            if status.timed_out() && Instant::now() >= deadline {
                return None;
            }
        }
    }

    pub fn wait_error(&self, timeout: Duration) -> Option<String> {
        let deadline = Instant::now() + timeout;
        let mut guard = self.mail.lock();
        loop {
            if let Some(error) = guard.error.take() {
                return Some(error);
            }
            let now = Instant::now();
            if now >= deadline {
                return None;
            }
            let waited = self.mail.changed.wait_timeout(guard, deadline - now);
            let (next, status) = match waited {
                Ok(pair) => pair,
                Err(poison) => poison.into_inner(),
            };
            guard = next;
            if status.timed_out() && Instant::now() >= deadline {
                return None;
            }
        }
    }

    fn send(&self, command: Command) -> Result<(), PreviewError> {
        if let Some(generation) = command.generation() {
            self.mail.requested.store(generation, Ordering::Release);
        }
        self.commands
            .send(command)
            .map_err(|_| PreviewError::new("preview が停止しています"))
    }
}

impl Drop for PreviewEngine {
    fn drop(&mut self) {
        let _ = self.commands.send(Command::Shutdown);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use std::time::Duration;

    use mvm_core::Rational;
    use mvm_project::{Project, TimelineClip, TimelineClipKind, TrackRef};

    use super::*;

    fn span(frames: i64) -> TimelineClip {
        TimelineClip {
            kind: TimelineClipKind::Text,
            name: "間".to_owned(),
            id: "gap".to_owned(),
            source_fps: Rational::new(60, 1),
            source_frame_count: frames,
            source_in_frame: 0,
            source_out_frame: frames,
            timeline_start_frame: 0,
            track: TrackRef::video(0),
            ..TimelineClip::default()
        }
    }

    #[test]
    fn wall_clock_advances_black_frames_without_a_device() {
        let engine = PreviewEngine::start().expect("起動");
        let project = Project {
            timeline_clips: vec![span(180)],
            ..Project::default()
        };
        engine
            .set_project(Arc::new(project), Arc::new(MathLibrary::default()), 1)
            .unwrap();
        engine.play(0, 1).unwrap();
        let image = engine
            .wait_image_until(Duration::from_secs(2), |image| image.frame >= 2)
            .expect("frame が進みません");
        assert!(image.frame >= 2);
        assert_eq!(image.width, 1920);
        assert_eq!(image.rgba.len(), 1920 * 1080 * 4);
        assert_eq!(&image.rgba[..4], &[0, 0, 0, 255]);
    }

    #[test]
    fn play_starts_at_the_requested_frame() {
        let engine = PreviewEngine::start().expect("起動");
        let project = Project {
            timeline_clips: vec![span(600)],
            ..Project::default()
        };
        engine
            .set_project(Arc::new(project), Arc::new(MathLibrary::default()), 1)
            .unwrap();
        engine.play(300, 2).unwrap();
        let image = engine
            .wait_image_until(Duration::from_secs(2), |_| true)
            .expect("frame が届きません");
        assert!(
            (300..600).contains(&image.frame),
            "再生が 300 から始まっていません: {}",
            image.frame
        );
    }

    fn names(batch: &[Command]) -> Vec<String> {
        batch
            .iter()
            .map(|command| match command {
                Command::Seek { frame, .. } => format!("seek {frame}"),
                Command::Play { frame, .. } => format!("play {frame}"),
                Command::Pause { .. } => "pause".to_owned(),
                Command::Shutdown => "shutdown".to_owned(),
                _ => "other".to_owned(),
            })
            .collect()
    }

    #[test]
    fn only_the_last_queued_seek_is_drawn() {
        let seek = |frame| Command::Seek {
            frame,
            generation: 1,
        };
        let kept = coalesce_seeks(vec![
            seek(1),
            seek(2),
            Command::Pause { generation: 2 },
            seek(3),
        ]);
        assert_eq!(names(&kept), ["pause", "seek 3"]);
        // 後に Play があれば、Play が描くので Seek は描かない。
        let kept = coalesce_seeks(vec![
            seek(4),
            Command::Play {
                frame: 5,
                generation: 3,
            },
        ]);
        assert_eq!(names(&kept), ["play 5"]);
        // Play の後の Seek は描く。Shutdown は残す。
        let kept = coalesce_seeks(vec![
            Command::Play {
                frame: 5,
                generation: 3,
            },
            seek(6),
            Command::Shutdown,
        ]);
        assert_eq!(names(&kept), ["play 5", "seek 6", "shutdown"]);
    }

    #[test]
    fn a_flood_of_seeks_ends_on_the_last_requested_frame() {
        let engine = PreviewEngine::start().expect("起動");
        let project = Project {
            timeline_clips: vec![span(600)],
            ..Project::default()
        };
        engine
            .set_project(Arc::new(project), Arc::new(MathLibrary::default()), 1)
            .unwrap();
        // シャトルのように、描くより速く seek を送る。世代は送るたびに進める。
        for frame in 0..300_u64 {
            engine.seek(frame as i64, frame + 2).unwrap();
        }
        let image = engine
            .wait_image_until(Duration::from_secs(5), |image| image.frame == 299)
            .expect("最後に要求した frame が届きません");
        assert_eq!(image.frame, 299);
        // 最後の画像の後に、古い seek の画像が届かない。
        std::thread::sleep(Duration::from_millis(100));
        let delivery = engine.poll().expect("通知");
        assert_eq!(delivery.generation, 301);
        if let Some(image) = delivery.image {
            assert_eq!(image.frame, 299);
        }
    }

    #[test]
    fn a_new_generation_drops_the_previous_image() {
        let mail = Mail::new();
        let image = crate::plan::black_image(7, &Project::default()).unwrap();
        mail.publish_image(1, image);
        mail.publish_playing(2, false);
        let delivery = mail.take().expect("通知");
        assert_eq!(delivery.generation, 2);
        assert!(delivery.image.is_none(), "前の世代の画像が残っています");
        assert_eq!(delivery.playing, Some(false));

        let image = crate::plan::black_image(8, &Project::default()).unwrap();
        mail.publish_image(3, image);
        mail.publish_playing(3, true);
        let delivery = mail.take().expect("通知");
        assert_eq!(delivery.image.map(|image| image.frame), Some(8));
        assert_eq!(delivery.playing, Some(true));
    }

    #[test]
    fn a_worker_panic_is_reported_on_the_latest_generation() {
        let engine = PreviewEngine::start().expect("起動");
        engine
            .set_project(
                Arc::new(Project::default()),
                Arc::new(MathLibrary::default()),
                1,
            )
            .unwrap();
        engine.send(Command::Panic { generation: 5 }).unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        let delivery = loop {
            if let Some(delivery) = engine.poll()
                && delivery.error.is_some()
            {
                break delivery;
            }
            assert!(Instant::now() < deadline, "異常終了が届きません");
            std::thread::sleep(Duration::from_millis(5));
        };
        assert_eq!(delivery.generation, 5, "古い世代で届くと UI が捨てます");
        assert_eq!(delivery.playing, Some(false));
        assert!(delivery.error.unwrap().contains("異常終了"));
        let deadline = Instant::now() + Duration::from_secs(2);
        while engine.play(0, 6).is_ok() {
            assert!(
                Instant::now() < deadline,
                "止まった worker が command を受け取ります"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn negative_seek_and_an_empty_timeline_fail_closed() {
        let engine = PreviewEngine::start().expect("起動");
        engine
            .set_project(
                Arc::new(Project::default()),
                Arc::new(MathLibrary::default()),
                1,
            )
            .unwrap();
        engine.play(0, 1).unwrap();
        let error = engine
            .wait_error(Duration::from_secs(2))
            .expect("失敗しない");
        assert!(error.contains("区間がありません"), "{error}");

        engine.seek(-1, 2).unwrap();
        let error = engine
            .wait_error(Duration::from_secs(2))
            .expect("負の frame");
        assert!(error.contains("frame"), "{error}");
    }
}
