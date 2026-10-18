//! IAudioClock の位置を、内部 48 kHz の media sample へ写す。
//!
//! デバイスの位置は「周波数あたり 1 秒」の整数で、sample 数そのものではない。
//! 商と余りに分けて掛け、`position * rate` の中間積で溢れないようにする。

use mvm_core::output_timebase::QUALIFIED_AUDIO_SAMPLE_RATE;

use crate::error::AudioError;

/// 音声 thread が最後に読んだ時計。映像 thread はこれと今の QPC から sample を足す。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ClockSnapshot {
    pub media_sample: i64,
    pub qpc_ticks: u64,
    pub qpc_frequency: u64,
    pub running: bool,
}

/// `device_position - device_start` を 48 kHz の sample 数にして `media_start` に足す。
pub fn map_device_media_sample(
    media_start: i64,
    device_start: u64,
    device_position: u64,
    device_frequency: u64,
) -> Result<i64, AudioError> {
    if media_start < 0 {
        return Err(AudioError::new("再生開始 sample が不正です"));
    }
    if device_frequency == 0 {
        return Err(AudioError::new("IAudioClock の周波数が 0 です"));
    }
    if device_position < device_start {
        return Err(AudioError::new("IAudioClock が戻りました"));
    }
    let advanced = scale_to_samples(device_position - device_start, device_frequency)?;
    media_start
        .checked_add(advanced)
        .ok_or_else(|| AudioError::new("media sample が overflow しました"))
}

/// 最後の時計読みに、QPC の経過を sample で足す。読み取りより前の QPC はその読みの sample を返す。
pub fn media_sample_at(snapshot: &ClockSnapshot, now_ticks: u64) -> Result<i64, AudioError> {
    if !snapshot.running || snapshot.qpc_frequency == 0 {
        return Err(AudioError::new("音声時計が動いていません"));
    }
    if now_ticks < snapshot.qpc_ticks {
        return Ok(snapshot.media_sample);
    }
    let advanced = scale_to_samples(now_ticks - snapshot.qpc_ticks, snapshot.qpc_frequency)?;
    snapshot
        .media_sample
        .checked_add(advanced)
        .ok_or_else(|| AudioError::new("media sample が overflow しました"))
}

fn scale_to_samples(delta: u64, frequency: u64) -> Result<i64, AudioError> {
    let whole = delta / frequency;
    let remainder = delta % frequency;
    let whole_samples = whole
        .checked_mul(
            u64::try_from(QUALIFIED_AUDIO_SAMPLE_RATE)
                .map_err(|_| AudioError::new("sample rate を時計の計算に使えません"))?,
        )
        .ok_or_else(|| AudioError::new("media sample が overflow しました"))?;
    let fraction = remainder
        .checked_mul(
            u64::try_from(QUALIFIED_AUDIO_SAMPLE_RATE)
                .map_err(|_| AudioError::new("sample rate を時計の計算に使えません"))?,
        )
        .ok_or_else(|| AudioError::new("media sample が overflow しました"))?
        / frequency;
    let samples = whole_samples
        .checked_add(fraction)
        .ok_or_else(|| AudioError::new("media sample が overflow しました"))?;
    i64::try_from(samples).map_err(|_| AudioError::new("media sample が overflow しました"))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn maps_device_ticks_onto_48khz_samples() {
        assert_eq!(map_device_media_sample(100, 1_000, 1_000, 100), Ok(100));
        // 1 秒ぶんの tick は 48000 sample。
        assert_eq!(map_device_media_sample(100, 0, 100, 100), Ok(100 + 48_000));
        // 半秒。50/100 * 48000 = 24000。
        assert_eq!(map_device_media_sample(0, 1_000, 1_050, 100), Ok(24_000));
    }

    #[test]
    fn rejects_a_clock_that_cannot_be_mapped() {
        assert!(map_device_media_sample(-1, 0, 0, 100).is_err());
        assert!(map_device_media_sample(0, 0, 0, 0).is_err());
        assert!(
            map_device_media_sample(0, 50, 49, 100)
                .unwrap_err()
                .message()
                .contains("戻りました")
        );
        assert!(map_device_media_sample(i64::MAX, 0, 100, 100).is_err());
    }

    #[test]
    fn extrapolates_from_the_last_reading_and_holds_when_qpc_is_earlier() {
        let snapshot = ClockSnapshot {
            media_sample: 800,
            qpc_ticks: 1_000,
            qpc_frequency: 48_000,
            running: true,
        };
        assert_eq!(media_sample_at(&snapshot, 1_000), Ok(800));
        // 周波数 48000 で 48000 tick は 1 秒 = 48000 sample。
        assert_eq!(media_sample_at(&snapshot, 1_000 + 48_000), Ok(800 + 48_000));
        assert_eq!(media_sample_at(&snapshot, 999), Ok(800));
        let stopped = ClockSnapshot {
            running: false,
            ..snapshot
        };
        assert!(media_sample_at(&stopped, 2_000).is_err());
    }
}
