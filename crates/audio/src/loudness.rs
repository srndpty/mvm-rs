//! 48 kHz stereo のラウドネス（ITU-R BS.1770 / EBU R128 の integrated loudness）と true peak。
//!
//! - K 特性: BS.1770 の 48 kHz の 2 段の biquad（高域 shelf と高域通過）。
//! - 400 ms の block を 100 ms ずつずらす。block のラウドネスは -0.691 + 10 log10(左右の平均二乗の和)。
//! - gate: -70 LUFS 未満を捨て、残りの平均から 10 LU 下を相対 gate にする。
//! - true peak: 4 倍に補間した値の絶対値の最大（libebur128 と同じ 49 tap の Hann 窓 sinc）。
//!
//! 少量ずつ渡せる。PCM 全体を memory に置かない。

const SAMPLE_RATE: usize = 48_000;
const BLOCK: usize = SAMPLE_RATE * 400 / 1000;
const HOP: usize = SAMPLE_RATE * 100 / 1000;
const ABSOLUTE_GATE_LUFS: f64 = -70.0;
const RELATIVE_GATE_LU: f64 = -10.0;
const OVERSAMPLE: usize = 4;
const INTERPOLATION_TAPS: usize = 49;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct LoudnessMeasurement {
    /// gate を通る block が 1 つも無ければ false（無音・短すぎる音声）。
    pub measurable: bool,
    pub integrated_lufs: f64,
    /// dBTP。無音なら負の無限大。
    pub true_peak_db: f64,
    pub samples: i64,
}

#[derive(Clone, Copy, Debug, Default)]
struct Biquad {
    b: [f64; 3],
    a: [f64; 2],
    z: [f64; 2],
}

impl Biquad {
    fn new(b: [f64; 3], a: [f64; 2]) -> Self {
        Self { b, a, z: [0.0; 2] }
    }

    /// 転置直接形 II。
    fn process(&mut self, input: f64) -> f64 {
        let output = self.b[0] * input + self.z[0];
        self.z[0] = self.b[1] * input - self.a[0] * output + self.z[1];
        self.z[1] = self.b[2] * input - self.a[1] * output;
        output
    }
}

fn k_weighting() -> [Biquad; 2] {
    [
        Biquad::new(
            [
                1.535_124_859_586_97,
                -2.691_696_189_406_38,
                1.198_392_810_852_85,
            ],
            [-1.690_659_293_182_41, 0.732_480_774_215_85],
        ),
        Biquad::new(
            [1.0, -2.0, 1.0],
            [-1.990_047_454_833_98, 0.990_072_250_366_21],
        ),
    ]
}

/// 4 相の補間 filter。`phases[p][k]` は相 p の k 番目の係数。
fn interpolation_phases() -> Vec<Vec<f64>> {
    let mut phases = vec![Vec::new(); OVERSAMPLE];
    let center = (INTERPOLATION_TAPS - 1) as f64 / 2.0;
    for tap in 0..INTERPOLATION_TAPS {
        let m = tap as f64 - center;
        let x = m * std::f64::consts::PI / OVERSAMPLE as f64;
        let sinc = if m.abs() < 1e-9 { 1.0 } else { x.sin() / x };
        let window = 0.5
            * (1.0
                - (2.0 * std::f64::consts::PI * tap as f64 / (INTERPOLATION_TAPS - 1) as f64)
                    .cos());
        phases[tap % OVERSAMPLE].push(sinc * window);
    }
    phases
}

pub struct LoudnessMeter {
    filters: [[Biquad; 2]; 2],
    /// 100 ms ごとの K 特性後の二乗和（左右の和）。最後の 4 個で 1 block になる。
    hop_energy: Vec<f64>,
    current_energy: f64,
    current_count: usize,
    block_energies: Vec<f64>,
    phases: Vec<Vec<f64>>,
    history: [Vec<f64>; 2],
    history_at: usize,
    peak: f64,
    samples: i64,
}

impl Default for LoudnessMeter {
    fn default() -> Self {
        Self::new()
    }
}

impl LoudnessMeter {
    pub fn new() -> Self {
        let phases = interpolation_phases();
        let length = phases.iter().map(Vec::len).max().unwrap_or(0);
        Self {
            filters: [k_weighting(), k_weighting()],
            hop_energy: Vec::new(),
            current_energy: 0.0,
            current_count: 0,
            block_energies: Vec::new(),
            history: [vec![0.0; length], vec![0.0; length]],
            history_at: 0,
            phases,
            peak: 0.0,
            samples: 0,
        }
    }

    /// 左右を交互に並べた sample を足す。長さが奇数なら false。
    pub fn push(&mut self, interleaved: &[f32]) -> bool {
        if !interleaved.len().is_multiple_of(2) {
            return false;
        }
        for pair in interleaved.as_chunks::<2>().0 {
            let mut energy = 0.0;
            for (channel, sample) in pair.iter().enumerate() {
                let raw = f64::from(*sample);
                let [first, second] = &mut self.filters[channel];
                let weighted = second.process(first.process(raw));
                energy += weighted * weighted;
                self.track_peak(channel, raw);
            }
            self.history_at = (self.history_at + 1) % self.history[0].len().max(1);
            self.current_energy += energy;
            self.current_count += 1;
            self.samples += 1;
            if self.current_count == HOP {
                self.hop_energy.push(self.current_energy);
                self.current_energy = 0.0;
                self.current_count = 0;
                let hops = BLOCK / HOP;
                if self.hop_energy.len() >= hops {
                    let sum: f64 = self.hop_energy[self.hop_energy.len() - hops..].iter().sum();
                    self.block_energies.push(sum / BLOCK as f64);
                }
            }
        }
        true
    }

    fn track_peak(&mut self, channel: usize, sample: f64) {
        let history = &mut self.history[channel];
        let length = history.len();
        if length == 0 {
            return;
        }
        history[self.history_at] = sample;
        for phase in &self.phases {
            let mut value = 0.0;
            for (k, coefficient) in phase.iter().enumerate() {
                let index = (self.history_at + length - k) % length;
                value += coefficient * history[index];
            }
            self.peak = self.peak.max(value.abs());
        }
        self.peak = self.peak.max(sample.abs());
    }

    pub fn finish(&self) -> LoudnessMeasurement {
        let loudness = |energy: f64| -0.691 + 10.0 * energy.log10();
        let absolute: Vec<f64> = self
            .block_energies
            .iter()
            .copied()
            .filter(|energy| *energy > 0.0 && loudness(*energy) >= ABSOLUTE_GATE_LUFS)
            .collect();
        let true_peak_db = if self.peak > 0.0 {
            20.0 * self.peak.log10()
        } else {
            f64::NEG_INFINITY
        };
        if absolute.is_empty() {
            return LoudnessMeasurement {
                measurable: false,
                integrated_lufs: f64::NEG_INFINITY,
                true_peak_db,
                samples: self.samples,
            };
        }
        let mean = absolute.iter().sum::<f64>() / absolute.len() as f64;
        let relative_gate = loudness(mean) + RELATIVE_GATE_LU;
        let gated: Vec<f64> = absolute
            .into_iter()
            .filter(|energy| loudness(*energy) >= relative_gate)
            .collect();
        if gated.is_empty() {
            return LoudnessMeasurement {
                measurable: false,
                integrated_lufs: f64::NEG_INFINITY,
                true_peak_db,
                samples: self.samples,
            };
        }
        LoudnessMeasurement {
            measurable: true,
            integrated_lufs: loudness(gated.iter().sum::<f64>() / gated.len() as f64),
            true_peak_db,
            samples: self.samples,
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    fn sine(amplitude: f64, frequency: f64, seconds: f64, phase: f64) -> Vec<f32> {
        let count = (SAMPLE_RATE as f64 * seconds) as usize;
        (0..count)
            .flat_map(|index| {
                let value = amplitude
                    * (index as f64 * frequency * std::f64::consts::TAU / SAMPLE_RATE as f64
                        + phase)
                        .sin();
                [value as f32, value as f32]
            })
            .collect()
    }

    fn measure(samples: &[f32]) -> LoudnessMeasurement {
        let mut meter = LoudnessMeter::new();
        for chunk in samples.chunks(9600) {
            assert!(meter.push(chunk));
        }
        meter.finish()
    }

    #[test]
    fn stereo_1khz_sine_at_minus_23_dbfs_reads_minus_23_lufs() {
        // EBU Tech 3341 の 1 番: 1 kHz、-23 dBFS の stereo の正弦波は -23 LUFS。
        let amplitude = 10f64.powf(-23.0 / 20.0);
        let measured = measure(&sine(amplitude, 1000.0, 20.0, 0.0));
        assert!(measured.measurable);
        assert!(
            (measured.integrated_lufs + 23.0).abs() < 0.1,
            "{}",
            measured.integrated_lufs
        );
        assert_eq!(measured.samples, 20 * 48_000);
    }

    #[test]
    fn silence_after_the_tone_is_gated_out() {
        // EBU Tech 3341 の考え方: -70 LUFS 未満の block は integrated に入らない。
        let amplitude = 10f64.powf(-20.0 / 20.0);
        let mut samples = sine(amplitude, 1000.0, 10.0, 0.0);
        samples.extend(std::iter::repeat_n(0.0f32, 48_000 * 2 * 10));
        let measured = measure(&samples);
        assert!(
            (measured.integrated_lufs + 20.0).abs() < 0.1,
            "{}",
            measured.integrated_lufs
        );
        // 負の対照: 無音だけなら測れない。
        assert!(!measure(&vec![0.0; 48_000 * 2 * 5]).measurable);
    }

    #[test]
    fn relative_gate_drops_quiet_passages() {
        // -20 と -40 の 2 区間。-40 は相対 gate（約 -33 LUFS）より下なので外れ、-20 付近になる。
        let mut samples = sine(10f64.powf(-20.0 / 20.0), 1000.0, 10.0, 0.0);
        samples.extend(sine(10f64.powf(-40.0 / 20.0), 1000.0, 10.0, 0.0));
        let measured = measure(&samples);
        assert!(
            (measured.integrated_lufs + 20.0).abs() < 0.2,
            "{}",
            measured.integrated_lufs
        );
    }

    #[test]
    fn true_peak_finds_the_peak_between_samples() {
        // 12 kHz を 45 度ずらして標本化すると、標本の最大は 0.5 * sin(45°) ≈ 0.354 だが、
        // 実際の山は 0.5（-6.02 dBTP）。
        let measured = measure(&sine(0.5, 12_000.0, 1.0, std::f64::consts::FRAC_PI_4));
        assert!(
            (measured.true_peak_db + 6.02).abs() < 0.6,
            "{}",
            measured.true_peak_db
        );
        let sample_peak = 20.0 * (0.5 * std::f64::consts::FRAC_PI_4.sin()).log10();
        assert!(measured.true_peak_db > sample_peak + 2.0);
    }

    #[test]
    fn odd_input_is_rejected() {
        let mut meter = LoudnessMeter::new();
        assert!(!meter.push(&[0.0]));
        assert_eq!(meter.finish().samples, 0);
    }
}
