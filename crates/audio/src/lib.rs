//! 再生の時計と、WASAPI への書き込み。ラウドネスの測定もここに置く。
//!
//! 映像を進める sample は IAudioClock から決める。壁時計は、音声が無いときだけ
//! preview が同じ timebase で使う。unsafe は [`sink`] だけ。

mod clock;
mod error;
mod loudness;
mod scheduler;
#[allow(unsafe_code)]
mod sink;

pub use clock::{ClockSnapshot, map_device_media_sample, media_sample_at};
pub use error::AudioError;
pub use loudness::{LoudnessMeasurement, LoudnessMeter};
pub use scheduler::{ScheduleAction, ScheduleDecision, schedule_video, video_is_ahead};
pub use sink::{AudioHost, WasapiSink};
