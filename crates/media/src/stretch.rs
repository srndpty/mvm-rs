//! 音程を保った time stretch（Rubber Band）。48 kHz stereo の interleaved float を受け取る。
//!
//! 出力の先頭は入力の先頭に揃う（FFmpeg の filter が開始の遅延を補う）。途中から始めるときは、
//! 始めたい素材の位置から入れ直す。

use mvm_core::output_timebase::QUALIFIED_AUDIO_SAMPLE_RATE;

use crate::error::MediaError;
use crate::stretch_ffi::StretchGraph;

pub struct TimeStretcher {
    graph: StretchGraph,
    sample_rate: i32,
    finished: bool,
}

impl TimeStretcher {
    /// `tempo` は素材の進む速さ（2.0 なら出力は半分の長さ）。
    pub fn new(tempo: f64) -> Result<Self, MediaError> {
        let sample_rate = i32::try_from(QUALIFIED_AUDIO_SAMPLE_RATE)
            .map_err(|_| MediaError::new("sample rate が不正です"))?;
        Ok(Self {
            graph: StretchGraph::new(sample_rate, tempo)?,
            sample_rate,
            finished: false,
        })
    }

    pub fn push(&mut self, interleaved: &[f32]) -> Result<(), MediaError> {
        if self.finished {
            return Err(MediaError::new("閉じた time stretch へ音声を渡せません"));
        }
        self.graph.push(interleaved, self.sample_rate)
    }

    /// 入力の終わりを知らせる。2 回目以降は何もしない。
    pub fn finish(&mut self) -> Result<(), MediaError> {
        if self.finished {
            return Ok(());
        }
        self.finished = true;
        self.graph.finish()
    }

    /// 出せる分を `out` の末尾へ足す。全て出し終えたら true。
    pub fn pull(&mut self, out: &mut Vec<f32>) -> Result<bool, MediaError> {
        self.graph.pull(out)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    fn tone(frequency: f64, seconds: f64) -> Vec<f32> {
        let count = (48_000.0 * seconds) as usize;
        let mut samples = Vec::with_capacity(count * 2);
        for index in 0..count {
            let value = (index as f64 * frequency * std::f64::consts::TAU / 48_000.0).sin() * 0.5;
            samples.push(value as f32);
            samples.push(value as f32);
        }
        samples
    }

    fn run(tempo: f64, input: &[f32]) -> Vec<f32> {
        let mut stretcher = TimeStretcher::new(tempo).expect("time stretch");
        let mut out = Vec::new();
        for chunk in input.chunks(4096 * 2) {
            stretcher.push(chunk).expect("push");
            stretcher.pull(&mut out).expect("pull");
        }
        stretcher.finish().expect("finish");
        while !stretcher.pull(&mut out).expect("pull") {}
        out
    }

    /// 左 channel の上向きの 0 交差の数。音程（周波数）を長さに依らず数える。
    fn rising_crossings(samples: &[f32]) -> usize {
        let left: Vec<f32> = samples.iter().step_by(2).copied().collect();
        left.windows(2)
            .filter(|pair| pair[0] < 0.0 && pair[1] >= 0.0)
            .count()
    }

    #[test]
    fn doubles_the_speed_without_raising_the_pitch() {
        let input = tone(440.0, 2.0);
        let output = run(2.0, &input);
        // 2 倍速で長さは半分（48000 sample の stereo）。
        assert_eq!(output.len(), 48_000 * 2, "出力の長さ");
        // 1 秒で 440 回の交差。音程を上げていれば 880 回前後になる（負の対照は下の速度変更）。
        let crossings = rising_crossings(&output);
        assert!((430..=450).contains(&crossings), "交差 {crossings}");
    }

    #[test]
    fn halving_the_speed_keeps_the_pitch_and_doubles_the_length() {
        let input = tone(440.0, 1.0);
        let output = run(0.5, &input);
        assert_eq!(output.len(), 96_000 * 2, "出力の長さ");
        let crossings = rising_crossings(&output);
        assert!((860..=900).contains(&crossings), "交差 {crossings}");
    }

    #[test]
    fn plain_resampling_would_change_the_crossing_count() {
        // 指標の負の対照: 素材を 1 つおきに間引く（音程が上がる 2 倍速）と 1 秒の交差は 880 前後になる。
        let input = tone(440.0, 2.0);
        let decimated: Vec<f32> = input
            .chunks(4)
            .flat_map(|pair| [pair[0], pair[1]])
            .collect();
        let crossings = rising_crossings(&decimated);
        assert!((870..=890).contains(&crossings), "交差 {crossings}");
    }

    #[test]
    fn rejects_invalid_tempo_and_odd_input() {
        assert!(TimeStretcher::new(0.0).is_err());
        assert!(TimeStretcher::new(f64::NAN).is_err());
        let mut stretcher = TimeStretcher::new(1.5).unwrap();
        assert!(stretcher.push(&[0.0, 0.0, 0.0]).is_err());
        stretcher.finish().unwrap();
        assert!(stretcher.push(&[0.0, 0.0]).is_err());
    }
}
