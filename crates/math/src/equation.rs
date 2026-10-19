//! Equation Sequence の描画要求への compile。
//!
//! 各状態の式を、部分式（SemanticPart）の範囲とその間の自動の分け方で segment に分ける。変形の対は、利用者が
//! 指定した部分式の対応を先に使い、残りを自動の照合で組む。強調（action）の対象は部分式の segment。
//! 部分式の範囲は、TeX の字句を壊さない範囲だけを受け付ける（命令の途中・中括弧をまたぐ範囲などは拒否）。

use mvm_project::equation_sequence::{
    EquationOperation, EquationSequenceClipData, EquationState, EquationTargetStatus, PartId,
    equation_binding_matches_source,
};
use mvm_project::parse_argb_color;

use crate::error::MathError;
use crate::job::{
    ActionOperation, EquationActionJob, EquationJob, EquationStateJob, EquationTransitionJob,
    Parity,
};
use crate::segments::{TexSegment, match_segments, segment_key, segment_tex};

/// 状態を分けた segment と、各 segment の持ち主の部分式（自動の segment は None）。
pub struct Partition {
    pub segments: Vec<TexSegment>,
    pub owners: Vec<Option<PartId>>,
}

fn is_space(byte: u8) -> bool {
    matches!(byte, b' ' | b'\t' | b'\r' | b'\n')
}

const KNOWN_COMMANDS: [&str; 26] = [
    "\\frac",
    "\\sqrt",
    "\\alpha",
    "\\beta",
    "\\gamma",
    "\\delta",
    "\\Delta",
    "\\pi",
    "\\theta",
    "\\pm",
    "\\mp",
    "\\cdot",
    "\\times",
    "\\le",
    "\\ge",
    "\\leq",
    "\\geq",
    "\\ne",
    "\\neq",
    "\\approx",
    "\\equiv",
    "\\infty",
    "\\cancel",
    "\\bcancel",
    "\\xcancel",
    "\\cancelto",
];

/// 部分式の範囲 [begin, end) が TeX の字句を壊さないか。支持範囲を明示し、任意の macro の引数は推測しない。
fn safe_range(source: &str, begin: usize, end: usize) -> bool {
    let s = source.as_bytes();
    let size = s.len();
    let mut depth = 0i64;
    let mut begin_depth = -1i64;
    let mut end_depth = -1i64;
    let splits = |a: usize, b: usize| (a < begin && begin < b) || (a < end && end < b);
    let mut i = 0usize;
    loop {
        if i == begin {
            begin_depth = depth;
        }
        if i == end {
            end_depth = depth;
        }
        if i >= size {
            break;
        }
        let mut next = i + 1;
        match s[i] {
            b'%' => {
                next = s[i..]
                    .iter()
                    .position(|byte| *byte == b'\n')
                    .map_or(size, |at| i + at + 1);
                if begin < next && i < end {
                    return false;
                }
            }
            b'\\' => {
                if next == size {
                    return false;
                }
                if s[next].is_ascii_alphabetic() {
                    while next < size && s[next].is_ascii_alphabetic() {
                        next += 1;
                    }
                    let token = &source[i..next];
                    if !KNOWN_COMMANDS.contains(&token) {
                        return false;
                    }
                    let arguments = match token {
                        "\\frac" | "\\cancelto" => 2,
                        "\\sqrt" | "\\cancel" | "\\bcancel" | "\\xcancel" => 1,
                        _ => 0,
                    };
                    let mut argument = next;
                    for _ in 0..arguments {
                        while argument < size && is_space(s[argument]) {
                            argument += 1;
                        }
                        if argument == size || s[argument] != b'{' {
                            return false;
                        }
                        // 命令だけの範囲を作らない。引数の内側の範囲は許す。
                        if begin <= i && end > i && end <= argument {
                            return false;
                        }
                        let mut nesting = 1;
                        argument += 1;
                        while argument < size && nesting > 0 {
                            if s[argument] == b'\\' {
                                argument += 2;
                                continue;
                            }
                            if s[argument] == b'{' {
                                nesting += 1;
                            }
                            if s[argument] == b'}' {
                                nesting -= 1;
                            }
                            argument += 1;
                        }
                        if nesting != 0 {
                            return false;
                        }
                    }
                } else {
                    // 数式 mode を切り替える escape などは扱わない。
                    if !b"{}%_#$& ,;:!\\".contains(&s[next]) {
                        return false;
                    }
                    next += 1;
                }
            }
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth < 0 {
                    return false;
                }
            }
            b'^' | b'_' => {
                while next < size && is_space(s[next]) {
                    next += 1;
                }
                if next == size {
                    return false;
                }
                if s[next] != b'{' {
                    if s[next] == b'\\' || s[next] >= 0x80 {
                        return false;
                    }
                    next += 1;
                } else {
                    if begin <= i && end > i && end <= next {
                        return false;
                    }
                    next = i + 1;
                }
            }
            _ => {}
        }
        if splits(i, next) {
            return false;
        }
        i = next;
    }
    if depth != 0 || begin_depth < 0 || begin_depth != end_depth {
        return false;
    }
    // 同じ深さでも }{ をまたぐ範囲を許さないよう、内部の最低の深さも見る。
    let mut local = 0i64;
    let mut at = begin;
    while at < end {
        if s[at] == b'\\' {
            at += 2;
            continue;
        }
        if s[at] == b'{' {
            local += 1;
        }
        if s[at] == b'}' {
            local -= 1;
            if local < 0 {
                return false;
            }
        }
        at += 1;
    }
    local == 0
}

fn empty_target(text: &str) -> bool {
    text.bytes()
        .all(|byte| is_space(byte) || byte == b'{' || byte == b'}')
}

pub fn partition(state: &EquationState) -> Result<Partition, MathError> {
    let source = &state.equation.source;
    let mut parts: Vec<_> = state.parts.iter().collect();
    for part in &parts {
        if !equation_binding_matches_source(state, &part.binding) {
            return Err(MathError::new(format!(
                "部分式「{}」の参照が式と一致しません",
                part.label
            )));
        }
    }
    parts.sort_by_key(|part| part.binding.begin);
    let mut result = Partition {
        segments: Vec::new(),
        owners: Vec::new(),
    };
    let mut position = 0usize;
    let auto_region = |result: &mut Partition, position: &mut usize, end: usize| {
        for segment in segment_tex(&source[*position..end]) {
            *position += segment.text.len();
            result.segments.push(segment);
            result.owners.push(None);
        }
    };
    for part in parts {
        let (Ok(begin), Ok(end)) = (
            usize::try_from(part.binding.begin),
            usize::try_from(part.binding.end),
        ) else {
            return Err(MathError::new("部分式の範囲が不正です"));
        };
        if begin < position {
            return Err(MathError::new("部分式の範囲が重なっています"));
        }
        if !safe_range(source, begin, end) {
            return Err(MathError::new(format!(
                "部分式「{}」の範囲が TeX の命令や中括弧をまたいでいます",
                part.label
            )));
        }
        if empty_target(&part.binding.expected_text) {
            return Err(MathError::new(format!(
                "部分式「{}」に描かれる文字がありません",
                part.label
            )));
        }
        auto_region(&mut result, &mut position, begin);
        result.segments.push(TexSegment {
            text: part.binding.expected_text.clone(),
            key: segment_key(&part.binding.expected_text),
        });
        result.owners.push(Some(part.id.clone()));
        position = end;
    }
    auto_region(&mut result, &mut position, source.len());
    if result.segments.is_empty() {
        return Err(MathError::new("式が空です"));
    }
    Ok(result)
}

fn owner_index(partition: &Partition, id: &PartId) -> Option<usize> {
    partition
        .owners
        .iter()
        .position(|owner| owner.as_ref() == Some(id))
}

/// 数式 sequence を描画要求にする。背景は透明だけを受け付ける（区間ごとに大きさが変わる背景は段差になる）。
pub fn compile(data: &EquationSequenceClipData, parity: Parity) -> Result<EquationJob, MathError> {
    if data.states.is_empty() || data.transitions.len() + 1 != data.states.len() {
        return Err(MathError::new("数式列の状態と変形の数が合いません"));
    }
    let mut partitions = Vec::with_capacity(data.states.len());
    let mut states = Vec::with_capacity(data.states.len());
    for state in &data.states {
        let background = parse_argb_color(&state.equation.background_color)
            .ok_or_else(|| MathError::new("数式列の背景色が不正です"))?;
        if background >> 24 != 0 {
            return Err(MathError::new(
                "数式列は背景色を持てません（透明にしてください）",
            ));
        }
        let partition = partition(state)?;
        states.push(EquationStateJob {
            segments: partition
                .segments
                .iter()
                .map(|segment| segment.text.clone())
                .collect(),
            font_size: state.equation.font_size,
        });
        partitions.push(partition);
    }
    let mut transitions = Vec::with_capacity(data.transitions.len());
    for (index, transition) in data.transitions.iter().enumerate() {
        let from = &partitions[index];
        let to = &partitions[index + 1];
        if transition.from != data.states[index].id || transition.to != data.states[index + 1].id {
            return Err(MathError::new("数式列の変形の端点が並びと一致しません"));
        }
        let mut used_from = vec![false; from.segments.len()];
        let mut used_to = vec![false; to.segments.len()];
        let mut pairs = Vec::new();
        for pair in &transition.correspondence {
            let (Some(a), Some(b)) = (owner_index(from, &pair.from), owner_index(to, &pair.to))
            else {
                return Err(MathError::new("部分式の対応が見つかりません"));
            };
            if used_from[a] || used_to[b] {
                return Err(MathError::new("部分式の対応が重複しています"));
            }
            used_from[a] = true;
            used_to[b] = true;
            pairs.push((a, b));
        }
        let remaining = |partition: &Partition, used: &[bool]| -> (Vec<TexSegment>, Vec<usize>) {
            partition
                .segments
                .iter()
                .enumerate()
                .filter(|(index, _)| !used[*index])
                .map(|(index, segment)| (segment.clone(), index))
                .unzip()
        };
        let (a_segments, a_indices) = remaining(from, &used_from);
        let (b_segments, b_indices) = remaining(to, &used_to);
        let matching = match_segments(&a_segments, &b_segments);
        for pair in &matching.pairs {
            pairs.push((a_indices[pair.source], b_indices[pair.target]));
        }
        pairs.sort_unstable();
        transitions.push(EquationTransitionJob {
            frames: transition.frames,
            pairs,
            unmatched_source: matching
                .unmatched_source
                .iter()
                .map(|index| a_indices[*index])
                .collect(),
            unmatched_target: matching
                .unmatched_target
                .iter()
                .map(|index| b_indices[*index])
                .collect(),
        });
    }
    let mut actions = Vec::with_capacity(data.actions.len());
    let mut ordered: Vec<_> = data.actions.iter().collect();
    ordered.sort_by_key(|action| {
        (
            data.states
                .iter()
                .position(|state| state.id == action.state),
            action.start,
        )
    });
    for action in ordered {
        let Some(state) = data
            .states
            .iter()
            .position(|state| state.id == action.state)
        else {
            return Err(MathError::new("強調の状態が見つかりません"));
        };
        if action.target_status == EquationTargetStatus::Missing {
            return Err(MathError::new("強調の対象の部分式がありません"));
        }
        let Some(segment) = owner_index(&partitions[state], &action.target) else {
            return Err(MathError::new("強調の対象の部分式がありません"));
        };
        actions.push(EquationActionJob {
            state,
            segment,
            frames: action.duration,
            operation: match action.operation {
                EquationOperation::Outline => ActionOperation::Outline,
                EquationOperation::Pulse => ActionOperation::Pulse,
            },
        });
    }
    Ok(EquationJob {
        states,
        transitions,
        actions,
        parity,
    })
}

/// action の並び（compile と同じ順）での番号。描画の連番の名前 `a{index}` に使う。
pub fn action_order(
    data: &EquationSequenceClipData,
) -> Vec<&mvm_project::equation_sequence::ActionId> {
    let mut ordered: Vec<_> = data.actions.iter().collect();
    ordered.sort_by_key(|action| {
        (
            data.states
                .iter()
                .position(|state| state.id == action.state),
            action.start,
        )
    });
    ordered.into_iter().map(|action| &action.id).collect()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use mvm_project::equation_sequence::{
        BindingStatus, EquationStepTransition, PartPair, SemanticPart, SourceBinding, StateId,
        TransitionId,
    };
    use mvm_project::math_clip::MathClipData;

    use super::*;

    fn state(id: &str, source: &str, parts: &[(&str, usize, usize)]) -> EquationState {
        EquationState {
            id: StateId::new(id),
            equation: MathClipData {
                source: source.to_owned(),
                ..MathClipData::default()
            },
            revision: "r1".to_owned(),
            hold_frames: 30,
            parts: parts
                .iter()
                .map(|(part, begin, end)| SemanticPart {
                    id: PartId::new(*part),
                    label: (*part).to_owned(),
                    binding: SourceBinding {
                        revision: "r1".to_owned(),
                        begin: *begin as i64,
                        end: *end as i64,
                        expected_text: source[*begin..*end].to_owned(),
                        status: BindingStatus::Bound,
                    },
                })
                .collect(),
        }
    }

    #[test]
    fn safe_range_accepts_whole_terms_and_rejects_broken_tokens() {
        let source = "x^2 + \\frac{b}{a}x = 0";
        assert!(safe_range(source, 0, 3), "x^2");
        assert!(safe_range(source, 6, 17), "分数の全体");
        assert!(safe_range(source, 12, 13), "分子の b");
        assert!(!safe_range(source, 6, 10), "命令の途中");
        assert!(!safe_range(source, 0, 2), "^ と引数を分ける");
        assert!(!safe_range(source, 12, 15), "中括弧の閉じと開きをまたぐ");
        assert!(!safe_range("\\mathrm{x}", 0, 10), "未知の命令");
    }

    #[test]
    fn partition_puts_parts_between_automatic_segments() {
        let state = state("s", "a + bc = d", &[("p", 4, 6)]);
        let partition = partition(&state).unwrap();
        let texts: Vec<&str> = partition
            .segments
            .iter()
            .map(|segment| segment.text.as_str())
            .collect();
        assert_eq!(texts, vec!["a", " + ", "bc", " = ", "d"]);
        assert_eq!(partition.owners[2], Some(PartId::new("p")));
        assert!(partition.owners[0].is_none());
    }

    #[test]
    fn explicit_correspondence_wins_over_automatic_matching() {
        let data = EquationSequenceClipData {
            states: vec![
                state("s1", "x = a", &[("p", 4, 5)]),
                state("s2", "a = x", &[("q", 0, 1)]),
            ],
            transitions: vec![EquationStepTransition {
                id: TransitionId::new("t"),
                from: StateId::new("s1"),
                to: StateId::new("s2"),
                frames: 20,
                correspondence: vec![PartPair {
                    from: PartId::new("p"),
                    to: PartId::new("q"),
                }],
            }],
            actions: Vec::new(),
        };
        let job = compile(&data, Parity::default()).unwrap();
        let transition = &job.transitions[0];
        // 部分式の外は自動で分ける（"x", " = " と " = ", "x"）。p(2) → q(0) は指定、= と x は自動の照合。
        assert_eq!(transition.pairs, vec![(0, 2), (1, 1), (2, 0)]);
        assert!(transition.unmatched_source.is_empty());
        assert_eq!(transition.frames, 20);
    }

    #[test]
    fn a_visible_background_is_refused() {
        let mut first = state("s1", "a", &[]);
        first.equation.background_color = "#FF000000".to_owned();
        let data = EquationSequenceClipData {
            states: vec![first],
            transitions: Vec::new(),
            actions: Vec::new(),
        };
        assert!(compile(&data, Parity::default()).is_err());
    }
}
