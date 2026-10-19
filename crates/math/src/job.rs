//! 描画の要求（job）と、その cache key。
//!
//! key は「script の版・要求の JSON・toolchain の fingerprint」の SHA-256。色・背景・ClipEffects・尺・clip ID は
//! 要求に入れない（白の mask を mvm 側で着色・配置する）。進み具合（alpha）は要求に入るので、frame 数が
//! 変われば別の key になる。

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::error::MathError;

/// script（assets/mvm_math.py）と要求の形の版。どちらかを変えたら上げる。
pub const SCRIPT_VERSION: &str = "mvm-math-script/1";
/// 1 本の連番の上限。
pub const MAXIMUM_FRAMES: i64 = 9_999;

/// 1 em の px（拡大 100% のとき）から Manim の font_size へ。
pub fn manim_font_size(em_pixels: i32) -> f64 {
    f64::from(em_pixels) * 12.0 / 17.0
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct TexSpec {
    pub source: String,
    /// 1 em の px。
    pub font_size: i32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Parity {
    pub width_odd: bool,
    pub height_odd: bool,
}

impl Parity {
    pub fn of(width: i32, height: i32) -> Self {
        Self {
            width_odd: width % 2 != 0,
            height_odd: height % 2 != 0,
        }
    }
}

/// 進み具合の分子・分母。
pub type Alpha = (i64, i64);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ActionOperation {
    Outline,
    Pulse,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct EquationStateJob {
    pub segments: Vec<String>,
    pub font_size: i32,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct EquationTransitionJob {
    pub frames: i64,
    pub pairs: Vec<(usize, usize)>,
    pub unmatched_source: Vec<usize>,
    pub unmatched_target: Vec<usize>,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct EquationActionJob {
    pub state: usize,
    pub segment: usize,
    pub frames: i64,
    pub operation: ActionOperation,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct EquationJob {
    pub states: Vec<EquationStateJob>,
    pub transitions: Vec<EquationTransitionJob>,
    pub actions: Vec<EquationActionJob>,
    pub parity: Parity,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum MathJob {
    Static(TexSpec),
    /// 先頭で式を書いていく。frame i は進み具合 i / frames。
    Write {
        tex: TexSpec,
        frames: i64,
    },
    /// 隣り合う 2 つの数式 clip の間の変形。frame i は i / frames。
    Transform {
        source: TexSpec,
        target: TexSpec,
        frames: i64,
        pairs: Vec<(usize, usize)>,
        unmatched_source: Vec<usize>,
        unmatched_target: Vec<usize>,
        source_segments: Vec<String>,
        target_segments: Vec<String>,
        parity: Parity,
    },
    Equation(EquationJob),
}

fn alphas(frames: i64, rule: impl Fn(i64) -> Alpha) -> Vec<Alpha> {
    (0..frames).map(rule).collect()
}

/// 変形・Write: frame i は i / N。
pub fn linear_alpha(index: i64, frames: i64) -> Alpha {
    (index, frames)
}

/// outline: frame i は (2i + 1) / (2N)。区間の全 frame が (0, 1) の内側。
pub fn outline_alpha(index: i64, frames: i64) -> Alpha {
    (2 * index + 1, 2 * frames)
}

/// pulse: 強調の重み (N − |2i + 1 − N|) / N（三角波）。拡大と色の重みに同じ値を使う。
pub fn pulse_alpha(index: i64, frames: i64) -> Alpha {
    (frames - (2 * index + 1 - frames).abs(), frames)
}

fn check_frames(frames: i64, what: &str) -> Result<(), MathError> {
    if !(1..=MAXIMUM_FRAMES).contains(&frames) {
        return Err(MathError::new(format!(
            "{what}の枚数は 1〜{MAXIMUM_FRAMES} です（{frames}）"
        )));
    }
    Ok(())
}

impl MathJob {
    /// script へ渡す JSON。
    pub fn request(&self) -> Result<Value, MathError> {
        Ok(match self {
            Self::Static(tex) => json!({
                "kind": "static",
                "source": tex.source,
                "font_size": manim_font_size(tex.font_size),
            }),
            Self::Write { tex, frames } => {
                check_frames(*frames, "Write ")?;
                json!({
                    "kind": "write",
                    "source": tex.source,
                    "font_size": manim_font_size(tex.font_size),
                    "alpha": alphas(*frames, |index| linear_alpha(index, *frames)),
                })
            }
            Self::Transform {
                source,
                target,
                frames,
                pairs,
                unmatched_source,
                unmatched_target,
                source_segments,
                target_segments,
                parity,
            } => {
                check_frames(*frames, "変形の")?;
                json!({
                    "kind": "transform",
                    "source": {"segments": source_segments, "font_size": manim_font_size(source.font_size)},
                    "target": {"segments": target_segments, "font_size": manim_font_size(target.font_size)},
                    "pairs": pairs,
                    "unmatched_source": unmatched_source,
                    "unmatched_target": unmatched_target,
                    "alpha": alphas(*frames, |index| linear_alpha(index, *frames)),
                    "width_odd": parity.width_odd,
                    "height_odd": parity.height_odd,
                })
            }
            Self::Equation(job) => {
                let mut transitions = Vec::new();
                for (index, transition) in job.transitions.iter().enumerate() {
                    check_frames(transition.frames, "数式列の変形の")?;
                    transitions.push(json!({
                        "from": index,
                        "to": index + 1,
                        "pairs": transition.pairs,
                        "unmatched_source": transition.unmatched_source,
                        "unmatched_target": transition.unmatched_target,
                        "alpha": alphas(transition.frames, |at| linear_alpha(at, transition.frames)),
                    }));
                }
                let mut actions = Vec::new();
                for action in &job.actions {
                    check_frames(action.frames, "強調の")?;
                    let (name, rule): (&str, fn(i64, i64) -> Alpha) = match action.operation {
                        ActionOperation::Outline => ("outline", outline_alpha),
                        ActionOperation::Pulse => ("pulse", pulse_alpha),
                    };
                    actions.push(json!({
                        "state": action.state,
                        "segment": action.segment,
                        "operation": name,
                        "alpha": alphas(action.frames, |at| rule(at, action.frames)),
                    }));
                }
                let states: Vec<Value> = job
                    .states
                    .iter()
                    .map(|state| {
                        json!({
                            "segments": state.segments,
                            "font_size": manim_font_size(state.font_size),
                        })
                    })
                    .collect();
                json!({
                    "kind": "equation",
                    "states": states,
                    "transitions": transitions,
                    "actions": actions,
                    "width_odd": job.parity.width_odd,
                    "height_odd": job.parity.height_odd,
                })
            }
        })
    }

    /// cache key（小文字 16 進 64 桁）。
    pub fn key(&self, fingerprint: &str) -> Result<String, MathError> {
        let request = self.request()?.to_string();
        let mut hasher = Sha256::new();
        for part in [SCRIPT_VERSION, request.as_str(), fingerprint] {
            hasher.update((part.len() as u64).to_le_bytes());
            hasher.update(part.as_bytes());
        }
        Ok(hasher
            .finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect())
    }

    /// 描いた結果に必ずある連番と枚数。script の manifest と照合する。
    pub fn expected_sequences(&self) -> Vec<(String, usize)> {
        match self {
            Self::Static(_) => vec![("static".to_owned(), 1)],
            Self::Write { frames, .. } => vec![("write".to_owned(), *frames as usize)],
            Self::Transform { frames, .. } => vec![("transform".to_owned(), *frames as usize)],
            Self::Equation(job) => {
                let mut list: Vec<(String, usize)> = (0..job.states.len())
                    .map(|index| (format!("s{index}"), 1))
                    .collect();
                for (index, transition) in job.transitions.iter().enumerate() {
                    list.push((format!("t{index}"), transition.frames as usize));
                }
                for (index, action) in job.actions.iter().enumerate() {
                    list.push((format!("a{index}"), action.frames as usize));
                    list.push((format!("a{index}/base"), 1));
                }
                list
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    fn tex(source: &str) -> TexSpec {
        TexSpec {
            source: source.to_owned(),
            font_size: 96,
        }
    }

    #[test]
    fn key_changes_with_drawing_fields_and_toolchain_only() {
        let base = MathJob::Static(tex("x^2"));
        let key = base.key("toolchain-a").unwrap();
        assert_eq!(key.len(), 64);
        assert_eq!(key, base.key("toolchain-a").unwrap(), "key が決定的でない");
        assert_ne!(key, base.key("toolchain-b").unwrap());
        assert_ne!(key, MathJob::Static(tex("x^3")).key("toolchain-a").unwrap());
        let larger = MathJob::Static(TexSpec {
            font_size: 97,
            ..tex("x^2")
        });
        assert_ne!(key, larger.key("toolchain-a").unwrap());
        let write = MathJob::Write {
            tex: tex("x^2"),
            frames: 30,
        };
        assert_ne!(key, write.key("toolchain-a").unwrap());
        let longer = MathJob::Write {
            tex: tex("x^2"),
            frames: 31,
        };
        assert_ne!(write.key("t").unwrap(), longer.key("t").unwrap());
    }

    #[test]
    fn progress_rules_match_the_written_formulas() {
        // 期待値は規則から手で数えたもの（N = 4）。
        let outline: Vec<Alpha> = (0..4).map(|index| outline_alpha(index, 4)).collect();
        assert_eq!(outline, vec![(1, 8), (3, 8), (5, 8), (7, 8)]);
        let pulse: Vec<Alpha> = (0..4).map(|index| pulse_alpha(index, 4)).collect();
        assert_eq!(pulse, vec![(1, 4), (3, 4), (3, 4), (1, 4)]);
        assert_eq!(pulse_alpha(0, 1), (1, 1), "1 枚なら最大の強調");
    }

    #[test]
    fn frame_limits_are_enforced() {
        let zero = MathJob::Write {
            tex: tex("x"),
            frames: 0,
        };
        assert!(zero.request().is_err());
        let huge = MathJob::Write {
            tex: tex("x"),
            frames: MAXIMUM_FRAMES + 1,
        };
        assert!(huge.key("t").is_err());
    }

    #[test]
    fn manim_font_size_follows_the_em_ratio() {
        assert!((manim_font_size(68) - 48.0).abs() < 1e-12);
    }
}
