//! WASAPI shared・event-driven の再生。内部の PCM は 48 kHz stereo float32。
//!
//! デバイスの mix format がそれと違うときは、黙って resample せず失敗させる。
//! 16 bit integer と float32 の書き込み方の違いだけは、shared mode が mix format を
//! 要求するための変換として明示する。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use crate::clock::{ClockSnapshot, map_device_media_sample, media_sample_at};
use crate::error::AudioError;
use windows::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0, WAIT_TIMEOUT};
use windows::Win32::Media::Audio::{
    AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_EVENTCALLBACK, IAudioClient, IAudioClock,
    IAudioRenderClient, IMMDeviceEnumerator, MMDeviceEnumerator, WAVE_FORMAT_PCM, WAVEFORMATEX,
    WAVEFORMATEXTENSIBLE, eConsole, eRender,
};
use windows::Win32::Media::KernelStreaming::KSDATAFORMAT_SUBTYPE_PCM;
use windows::Win32::System::Com::{
    CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemFree,
    CoUninitialize,
};
use windows::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};
use windows::Win32::System::Threading::{CreateEventW, WaitForSingleObject};
use windows::core::{GUID, PCWSTR};

const BUFFER_HUNDRED_NS: i64 = 1_000_000;
const WAIT: Duration = Duration::from_millis(50);
const WAVE_FORMAT_IEEE_FLOAT: u16 = 3;
const WAVE_FORMAT_EXTENSIBLE: u16 = 0xFFFE;
/// KSDATAFORMAT_SUBTYPE_IEEE_FLOAT。windows crate はこの GUID を出していない。
const SUBTYPE_IEEE_FLOAT: GUID = GUID::from_u128(0x0000_0003_0000_0010_8000_00aa_0038_9b71);

struct AudioShared {
    clock: Mutex<ClockSnapshot>,
    error: Mutex<Option<String>>,
}

impl AudioShared {
    fn new() -> Self {
        Self {
            clock: Mutex::new(ClockSnapshot::default()),
            error: Mutex::new(None),
        }
    }

    fn lock_clock(&self) -> std::sync::MutexGuard<'_, ClockSnapshot> {
        self.clock
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }

    fn lock_error(&self) -> std::sync::MutexGuard<'_, Option<String>> {
        self.error
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }
}

/// 音声 thread から使う再生セッション。PCM の decoder はこの thread で開く。
pub struct AudioHost {
    stop: Arc<AtomicBool>,
    shared: Arc<AudioShared>,
}

impl AudioHost {
    pub fn stopped(&self) -> bool {
        self.stop.load(Ordering::Relaxed)
    }

    pub fn report(&self, message: impl Into<String>) {
        *self.shared.lock_error() = Some(message.into());
    }

    /// `pull` は `start_sample` から連続した stereo interleaved の f32 を書く。
    pub fn run<P>(&self, start_sample: i64, mut pull: P) -> Result<(), AudioError>
    where
        P: FnMut(i64, usize, &mut [f32]) -> Result<(), String>,
    {
        if start_sample < 0 {
            return Err(AudioError::new("再生開始 sample が不正です"));
        }
        let _apartment = ComApartment::new()?;
        let endpoint = Endpoint::open()?;
        let media_start = start_sample;
        let mut cursor = endpoint.prefill(media_start, &mut pull)?;
        let anchor = endpoint.clock_anchor()?;
        endpoint.start()?;
        // 時計は端点が最初に位置を返してから公開する。Start の時点から QPC で外挿すると、
        // 出力の遅延ぶん先へ進み、最初の実測が戻ったように見える。
        while !self.stopped() {
            if !endpoint.wait_event()? {
                continue;
            }
            if self.stopped() {
                break;
            }
            cursor = endpoint.render_available(cursor, media_start, &mut pull, &anchor, self)?;
        }
        endpoint.stop()?;
        Ok(())
    }

    fn publish_device(
        &self,
        media_start: i64,
        anchor: &ClockAnchor,
        position: u64,
        qpc_100ns: u64,
    ) -> Result<(), AudioError> {
        let media =
            map_device_media_sample(media_start, anchor.position, position, anchor.frequency)?;
        let qpc = if qpc_100ns == 0 {
            performance_counter()?.0
        } else {
            qpc_ticks_from_100ns(qpc_100ns, anchor.qpc_frequency)?
        };
        let mut clock = self.shared.lock_clock();
        if clock.running && media < clock.media_sample {
            return Err(AudioError::new("IAudioClock が戻りました"));
        }
        *clock = ClockSnapshot {
            media_sample: media,
            qpc_ticks: qpc,
            qpc_frequency: anchor.qpc_frequency,
            running: true,
        };
        Ok(())
    }
}

/// 映像 thread から時計を読む再生端点。`Drop` で callback を止めて thread を join する。
pub struct WasapiSink {
    stop: Arc<AtomicBool>,
    shared: Arc<AudioShared>,
    thread: Option<JoinHandle<()>>,
}

impl WasapiSink {
    pub fn spawn<F>(worker: F) -> Result<Self, AudioError>
    where
        F: FnOnce(AudioHost) + Send + 'static,
    {
        let stop = Arc::new(AtomicBool::new(false));
        let shared = Arc::new(AudioShared::new());
        let host = AudioHost {
            stop: Arc::clone(&stop),
            shared: Arc::clone(&shared),
        };
        let thread = std::thread::Builder::new()
            .name("mvm-audio".into())
            .spawn(move || worker(host))
            .map_err(|error| AudioError::new(format!("音声 thread を起動できません: {error}")))?;
        Ok(Self {
            stop,
            shared,
            thread: Some(thread),
        })
    }

    /// まだ anchor が無ければ `Ok(None)`。端点が失敗していたらそのメッセージで失敗する。
    pub fn media_sample(&self) -> Result<Option<i64>, AudioError> {
        if let Some(message) = self.shared.lock_error().take() {
            return Err(AudioError::new(message));
        }
        let snapshot = *self.shared.lock_clock();
        if !snapshot.running {
            return Ok(None);
        }
        let (now, _) = performance_counter()?;
        Ok(Some(media_sample_at(&snapshot, now)?))
    }
}

impl Drop for WasapiSink {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

struct ComApartment {
    uninit: bool,
}

impl ComApartment {
    fn new() -> Result<Self, AudioError> {
        // SAFETY: この thread で COM を MTA にする。成功したときだけ対になる CoUninitialize を呼ぶ。
        let hr = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        if hr.is_ok() {
            return Ok(Self { uninit: true });
        }
        if hr == windows::Win32::Foundation::RPC_E_CHANGED_MODE {
            return Ok(Self { uninit: false });
        }
        Err(AudioError::new(format!("COM を初期化できません: {hr:?}")))
    }
}

impl Drop for ComApartment {
    fn drop(&mut self) {
        if self.uninit {
            // SAFETY: new が CoInitializeEx に成功した thread で、1 回だけ解放する。
            unsafe { CoUninitialize() }
        }
    }
}

struct ClockAnchor {
    position: u64,
    frequency: u64,
    qpc_frequency: u64,
}

struct Endpoint {
    client: IAudioClient,
    render: IAudioRenderClient,
    clock: IAudioClock,
    event: OwnedEvent,
    format: DeviceFormat,
    buffer_frames: u32,
}

struct DeviceFormat {
    kind: SampleKind,
    block_align: u16,
}

#[derive(Clone, Copy)]
enum SampleKind {
    F32,
    S16,
}

struct OwnedEvent(HANDLE);

impl Drop for OwnedEvent {
    fn drop(&mut self) {
        // SAFETY: CreateEventW が返した kernel handle を 1 回だけ閉じる。
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

struct MixFormat(*mut WAVEFORMATEX);

impl Drop for MixFormat {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: GetMixFormat が CoTaskMemAlloc した形式を 1 回だけ解放する。
            unsafe { CoTaskMemFree(Some(self.0.cast())) }
        }
    }
}

impl Endpoint {
    fn open() -> Result<Self, AudioError> {
        // SAFETY: apartment を初期化した thread で、既定の render 端点を shared mode で開く。
        unsafe {
            let enumerator: IMMDeviceEnumerator =
                CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL).map_err(|error| {
                    AudioError::new(format!("音声デバイスの列挙を開始できません: {error}"))
                })?;
            let device = enumerator
                .GetDefaultAudioEndpoint(eRender, eConsole)
                .map_err(|error| {
                    AudioError::new(format!("既定の再生デバイスがありません: {error}"))
                })?;
            let client: IAudioClient = device.Activate(CLSCTX_ALL, None).map_err(|error| {
                AudioError::new(format!("IAudioClient を取得できません: {error}"))
            })?;
            let mix = MixFormat(client.GetMixFormat().map_err(|error| {
                AudioError::new(format!("再生デバイスの形式を取得できません: {error}"))
            })?);
            let format = describe_format(mix.0)?;
            client
                .Initialize(
                    AUDCLNT_SHAREMODE_SHARED,
                    AUDCLNT_STREAMFLAGS_EVENTCALLBACK,
                    BUFFER_HUNDRED_NS,
                    0,
                    mix.0,
                    None,
                )
                .map_err(|error| {
                    AudioError::new(format!(
                        "WASAPI shared event-driven を開始できません: {error}"
                    ))
                })?;
            let event = OwnedEvent(CreateEventW(None, false, false, PCWSTR::null()).map_err(
                |error| AudioError::new(format!("音声の event を作成できません: {error}")),
            )?);
            client.SetEventHandle(event.0).map_err(|error| {
                AudioError::new(format!("音声の event を設定できません: {error}"))
            })?;
            let buffer_frames = client.GetBufferSize().map_err(|error| {
                AudioError::new(format!("音声 buffer の大きさを取得できません: {error}"))
            })?;
            if buffer_frames == 0 {
                return Err(AudioError::new("音声 buffer の大きさが 0 です"));
            }
            let render: IAudioRenderClient = client.GetService().map_err(|error| {
                AudioError::new(format!("IAudioRenderClient を取得できません: {error}"))
            })?;
            let clock: IAudioClock = client.GetService().map_err(|error| {
                AudioError::new(format!("IAudioClock を取得できません: {error}"))
            })?;
            Ok(Self {
                client,
                render,
                clock,
                event,
                format,
                buffer_frames,
            })
        }
    }

    fn prefill<P>(&self, start_sample: i64, pull: &mut P) -> Result<i64, AudioError>
    where
        P: FnMut(i64, usize, &mut [f32]) -> Result<(), String>,
    {
        self.write(self.buffer_frames, start_sample, pull)
    }

    fn render_available<P>(
        &self,
        cursor: i64,
        media_start: i64,
        pull: &mut P,
        anchor: &ClockAnchor,
        host: &AudioHost,
    ) -> Result<i64, AudioError>
    where
        P: FnMut(i64, usize, &mut [f32]) -> Result<(), String>,
    {
        // SAFETY: client は open からこの thread が Drop するまで有効。
        let padding = unsafe { self.client.GetCurrentPadding() }.map_err(|error| {
            AudioError::new(format!("音声 buffer の使用量を取得できません: {error}"))
        })?;
        let available = self.buffer_frames.saturating_sub(padding);
        if available == 0 {
            return Ok(cursor);
        }
        let cursor = self.write(available, cursor, pull)?;
        let (position, qpc_100ns) = self.position()?;
        host.publish_device(media_start, anchor, position, qpc_100ns)?;
        Ok(cursor)
    }

    fn write<P>(&self, frames: u32, start_sample: i64, pull: &mut P) -> Result<i64, AudioError>
    where
        P: FnMut(i64, usize, &mut [f32]) -> Result<(), String>,
    {
        let count =
            usize::try_from(frames).map_err(|_| AudioError::new("音声 buffer が大きすぎます"))?;
        let mut interleaved = vec![0f32; count.saturating_mul(2)];
        pull(start_sample, count, &mut interleaved).map_err(AudioError::new)?;
        // SAFETY: GetBuffer が貸した領域へ、要求した frame 数だけ書いてから必ず ReleaseBuffer する。
        unsafe {
            let destination = self.render.GetBuffer(frames).map_err(|error| {
                AudioError::new(format!("音声 buffer を取得できません: {error}"))
            })?;
            let wrote = write_device(
                destination,
                &interleaved,
                self.format.kind,
                self.format.block_align,
            );
            let released = self
                .render
                .ReleaseBuffer(frames, 0)
                .map_err(|error| AudioError::new(format!("音声 buffer を返せません: {error}")));
            wrote?;
            released?;
        }
        start_sample
            .checked_add(i64::from(frames))
            .ok_or_else(|| AudioError::new("media sample が overflow しました"))
    }

    fn start(&self) -> Result<(), AudioError> {
        // SAFETY: client は open 済みで、prefill のあと 1 回だけ Start する。
        unsafe { self.client.Start() }
            .map_err(|error| AudioError::new(format!("再生デバイスを開始できません: {error}")))
    }

    fn stop(&self) -> Result<(), AudioError> {
        // SAFETY: Start した client を、この thread で止める。
        unsafe { self.client.Stop() }
            .map_err(|error| AudioError::new(format!("再生デバイスを停止できません: {error}")))
    }

    fn wait_event(&self) -> Result<bool, AudioError> {
        // SAFETY: event は CreateEventW が返した handle で、この thread だけが待つ。
        let status = unsafe { WaitForSingleObject(self.event.0, WAIT.as_millis() as u32) };
        if status == WAIT_OBJECT_0 {
            Ok(true)
        } else if status == WAIT_TIMEOUT {
            Ok(false)
        } else {
            Err(AudioError::new("音声の event を待てません"))
        }
    }

    fn clock_anchor(&self) -> Result<ClockAnchor, AudioError> {
        let (position, _) = self.position()?;
        let (_, qpc_frequency) = performance_counter()?;
        // SAFETY: clock は GetService で取得し、この thread が保持する。
        let frequency = unsafe { self.clock.GetFrequency() }.map_err(|error| {
            AudioError::new(format!("IAudioClock の周波数を取得できません: {error}"))
        })?;
        if frequency == 0 {
            return Err(AudioError::new("IAudioClock の周波数が 0 です"));
        }
        Ok(ClockAnchor {
            position,
            frequency,
            qpc_frequency,
        })
    }

    /// 端点の位置と、それを読んだ瞬間の QPC（100 ns 単位。QPC の tick ではない）。
    fn position(&self) -> Result<(u64, u64), AudioError> {
        let mut position = 0u64;
        let mut qpc = 0u64;
        // SAFETY: clock は GetService で取得し、この thread が保持する。
        unsafe { self.clock.GetPosition(&mut position, Some(&mut qpc)) }.map_err(|error| {
            AudioError::new(format!("IAudioClock の位置を取得できません: {error}"))
        })?;
        Ok((position, qpc))
    }
}

struct CopiedFormat {
    tag: u16,
    channels: u16,
    samples_per_sec: u32,
    block_align: u16,
    bits: u16,
}

fn describe_format(format: *const WAVEFORMATEX) -> Result<DeviceFormat, AudioError> {
    if format.is_null() {
        return Err(AudioError::new("再生デバイスの形式が空です"));
    }
    // SAFETY: GetMixFormat が返す形式は MixFormat の Drop まで有効。packed なので field は unaligned に読む。
    let header = unsafe {
        CopiedFormat {
            tag: std::ptr::addr_of!((*format).wFormatTag).read_unaligned(),
            channels: std::ptr::addr_of!((*format).nChannels).read_unaligned(),
            samples_per_sec: std::ptr::addr_of!((*format).nSamplesPerSec).read_unaligned(),
            block_align: std::ptr::addr_of!((*format).nBlockAlign).read_unaligned(),
            bits: std::ptr::addr_of!((*format).wBitsPerSample).read_unaligned(),
        }
    };
    if header.samples_per_sec != 48_000 || header.channels != 2 {
        return Err(AudioError::new(format!(
            "再生デバイスは {} Hz {} ch です。48000 Hz stereo だけを使います",
            header.samples_per_sec, header.channels
        )));
    }
    let kind = match header.tag {
        WAVE_FORMAT_IEEE_FLOAT if header.bits == 32 => SampleKind::F32,
        tag if tag == WAVE_FORMAT_PCM as u16 && header.bits == 16 => SampleKind::S16,
        WAVE_FORMAT_EXTENSIBLE => extensible_kind(format)?,
        _ => {
            return Err(AudioError::new(format!(
                "再生デバイスの sample 形式（tag {} / {} bit）は float32 でも 16 bit でもありません",
                header.tag, header.bits
            )));
        }
    };
    if header.block_align == 0 {
        return Err(AudioError::new("再生デバイスの block サイズが 0 です"));
    }
    Ok(DeviceFormat {
        kind,
        block_align: header.block_align,
    })
}

fn extensible_kind(format: *const WAVEFORMATEX) -> Result<SampleKind, AudioError> {
    let extended = format.cast::<WAVEFORMATEXTENSIBLE>();
    // SAFETY: wFormatTag が EXTENSIBLE のとき、header の直後に SubFormat がある。packed なので unaligned に読む。
    let (bits, subtype) = unsafe {
        (
            std::ptr::addr_of!((*extended).Format.wBitsPerSample).read_unaligned(),
            std::ptr::addr_of!((*extended).SubFormat).read_unaligned(),
        )
    };
    if bits == 32 && subtype == SUBTYPE_IEEE_FLOAT {
        Ok(SampleKind::F32)
    } else if bits == 16 && subtype == KSDATAFORMAT_SUBTYPE_PCM {
        Ok(SampleKind::S16)
    } else {
        Err(AudioError::new(
            "再生デバイスの拡張形式は 48000 Hz stereo の float32 / 16 bit ではありません",
        ))
    }
}

fn write_device(
    destination: *mut u8,
    interleaved: &[f32],
    kind: SampleKind,
    block_align: u16,
) -> Result<(), AudioError> {
    if destination.is_null() {
        return Err(AudioError::new("音声 buffer が空です"));
    }
    let frames = interleaved.len() / 2;
    let bytes = frames
        .checked_mul(usize::from(block_align))
        .ok_or_else(|| AudioError::new("音声 buffer が大きすぎます"))?;
    match kind {
        SampleKind::F32 => {
            if interleaved.len() * 4 != bytes {
                return Err(AudioError::new(
                    "再生デバイスの float block サイズが sample 数と一致しません",
                ));
            }
            // SAFETY: destination は frames ぶんの float stereo。呼び出し側が Release するまで有効。
            unsafe {
                std::ptr::copy_nonoverlapping(
                    interleaved.as_ptr().cast::<u8>(),
                    destination,
                    bytes,
                );
            }
        }
        SampleKind::S16 => {
            if frames * 4 != bytes {
                return Err(AudioError::new(
                    "再生デバイスの 16 bit block サイズが sample 数と一致しません",
                ));
            }
            // SAFETY: destination は frames 個の stereo i16。範囲外には書かない。
            unsafe {
                let output = std::slice::from_raw_parts_mut(destination.cast::<i16>(), frames * 2);
                for (slot, sample) in output.iter_mut().zip(interleaved) {
                    let clamped = sample.clamp(-1.0, 1.0);
                    let quantized = (clamped * 32767.0).round();
                    *slot = i16::try_from(quantized as i32).unwrap_or(0);
                }
            }
        }
    }
    Ok(())
}

/// IAudioClock::GetPosition の QPC（100 ns 単位）を、QueryPerformanceCounter の tick へ直す。
fn qpc_ticks_from_100ns(value: u64, qpc_frequency: u64) -> Result<u64, AudioError> {
    if qpc_frequency == 0 {
        return Err(AudioError::new("QPC の周波数が 0 です"));
    }
    let ticks = u128::from(value) * u128::from(qpc_frequency) / 10_000_000;
    u64::try_from(ticks).map_err(|_| AudioError::new("QPC が overflow しました"))
}

fn performance_counter() -> Result<(u64, u64), AudioError> {
    let mut counter = 0i64;
    let mut frequency = 0i64;
    // SAFETY: QueryPerformanceCounter / Frequency は呼び出し元の整数へ書くだけで、失敗は HRESULT で返す。
    unsafe {
        QueryPerformanceCounter(&mut counter)
            .map_err(|error| AudioError::new(format!("QPC を読めません: {error}")))?;
        QueryPerformanceFrequency(&mut frequency)
            .map_err(|error| AudioError::new(format!("QPC の周波数を読めません: {error}")))?;
    }
    if counter < 0 || frequency <= 0 {
        return Err(AudioError::new("QPC の値が不正です"));
    }
    Ok((counter as u64, frequency as u64))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn device_qpc_is_converted_from_100ns_units() {
        // 10 MHz の QPC では 100 ns 単位と tick が一致する。
        assert_eq!(qpc_ticks_from_100ns(12_345, 10_000_000), Ok(12_345));
        // 3 MHz では 1 秒（10^7）が 3*10^6 tick。
        assert_eq!(qpc_ticks_from_100ns(10_000_000, 3_000_000), Ok(3_000_000));
        assert!(qpc_ticks_from_100ns(1, 0).is_err());
    }

    #[test]
    #[ignore = "workstation"]
    fn shared_endpoint_clock_advances_while_silence_plays() {
        let sink = WasapiSink::spawn(|host| {
            let result = host.run(0, |_start, count, samples| {
                samples[..count * 2].fill(0.0);
                Ok(())
            });
            if let Err(error) = result {
                host.report(error.to_string());
            }
        })
        .expect("音声 thread");
        let started = std::time::Instant::now();
        let mut seen = None;
        while started.elapsed() < Duration::from_secs(2) {
            match sink.media_sample() {
                Ok(Some(sample)) => {
                    seen = Some(sample);
                    if sample > 4_800 {
                        break;
                    }
                }
                Ok(None) => {}
                Err(error) => panic!("端点: {error}"),
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let sample = seen.expect("時計が動きません");
        assert!(
            sample > 4_800,
            "200 ms 分の sample が進んでいません: {sample}"
        );
    }
}
