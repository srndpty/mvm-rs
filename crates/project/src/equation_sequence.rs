//! 数式 sequence（EquationSequence）。状態（式）と状態間の変形を、内部の素材時間軸に並べる。
//!
//! 時間の正は整数の区間と progress の有理数で、浮動小数は持たない。
//! どの編集も候補を作って全体を検証してから確定する（失敗時は data を変えない）。

use std::collections::{HashMap, HashSet};

use mvm_core::text_offsets::{utf8_to_utf16_offset, utf16_to_utf8_offset};

use crate::error::{ProjectError, ProjectResult, fail};
use crate::math_clip::MathClipData;
use crate::model::parse_argb_color;

macro_rules! equation_id {
    ($name:ident) => {
        #[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
        pub struct $name(pub String);

        impl $name {
            pub fn new(value: impl Into<String>) -> Self {
                Self(value.into())
            }
        }
    };
}

equation_id!(StateId);
equation_id!(TransitionId);
equation_id!(ActionId);
equation_id!(PartId);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum BindingStatus {
    #[default]
    Bound,
    Invalid,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum EquationOperation {
    #[default]
    Outline,
    Pulse,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum EquationTargetStatus {
    #[default]
    Present,
    Missing,
}

/// 部分式が式のどの範囲（UTF-8 の byte）を指すか。`expected_text` はその範囲の文字列の証人。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SourceBinding {
    pub revision: String,
    pub begin: i64,
    pub end: i64,
    pub expected_text: String,
    pub status: BindingStatus,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SemanticPart {
    pub id: PartId,
    pub label: String,
    pub binding: SourceBinding,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EquationState {
    pub id: StateId,
    pub equation: MathClipData,
    pub revision: String,
    pub hold_frames: i64,
    pub parts: Vec<SemanticPart>,
}

impl Default for EquationState {
    fn default() -> Self {
        Self {
            id: StateId::default(),
            equation: MathClipData::default(),
            revision: String::new(),
            hold_frames: 1,
            parts: Vec::new(),
        }
    }
}

impl EquationState {
    fn part(&self, id: &PartId) -> Option<&SemanticPart> {
        self.parts.iter().find(|part| part.id == *id)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PartPair {
    pub from: PartId,
    pub to: PartId,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EquationStepTransition {
    pub id: TransitionId,
    pub from: StateId,
    pub to: StateId,
    pub frames: i64,
    pub correspondence: Vec<PartPair>,
}

impl Default for EquationStepTransition {
    fn default() -> Self {
        Self {
            id: TransitionId::default(),
            from: StateId::default(),
            to: StateId::default(),
            frames: 1,
            correspondence: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EquationAction {
    pub id: ActionId,
    pub state: StateId,
    pub target: PartId,
    pub target_status: EquationTargetStatus,
    pub start: i64,
    pub duration: i64,
    pub operation: EquationOperation,
}

impl Default for EquationAction {
    fn default() -> Self {
        Self {
            id: ActionId::default(),
            state: StateId::default(),
            target: PartId::default(),
            target_status: EquationTargetStatus::Present,
            start: 0,
            duration: 1,
            operation: EquationOperation::Outline,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EquationSequenceClipData {
    pub states: Vec<EquationState>,
    pub transitions: Vec<EquationStepTransition>,
    pub actions: Vec<EquationAction>,
}

/// 内部の素材時間軸上の区間。状態の hold か、状態 `index` から次の状態への変形か。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EquationInterval {
    pub transition: bool,
    pub index: usize,
    pub begin: i64,
    pub end: i64,
}

/// ある素材 frame の評価。時間の正は整数の区間と progress の有理数。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EquationEvaluation {
    pub transition: bool,
    pub state: StateId,
    pub to_state: StateId,
    pub transition_id: TransitionId,
    pub local_frame: i64,
    pub frames: i64,
    pub progress_numerator: i64,
    pub progress_denominator: i64,
    pub active_action: Option<ActionId>,
}

/// UTF-8 の境界を変えずに式を書き換えた編集（入力欄からの差分）。offset は UTF-16。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TrustedEquationEdit {
    pub old_source: String,
    pub begin_utf16: usize,
    pub end_utf16: usize,
    pub replacement: String,
    pub new_source: String,
}

const ID_ATTEMPTS: usize = 128;

fn checked_end(start: i64, length: i64) -> Option<i64> {
    if start < 0 || length < 0 {
        return None;
    }
    start.checked_add(length)
}

fn byte_boundary(source: &str, offset: i64) -> bool {
    usize::try_from(offset).is_ok_and(|offset| utf8_to_utf16_offset(source, offset).is_some())
}

fn insert_unique(value: &str, ids: &mut HashSet<String>) -> bool {
    !value.is_empty() && ids.insert(value.to_owned())
}

/// 部分式の binding が今の式（revision と範囲の文字列）に一致するか。domain と compiler で共有する。
pub fn equation_binding_matches_source(state: &EquationState, binding: &SourceBinding) -> bool {
    binding.status == BindingStatus::Bound
        && binding.begin >= 0
        && binding.end > binding.begin
        && binding.revision == state.revision
        && byte_boundary(&state.equation.source, binding.begin)
        && byte_boundary(&state.equation.source, binding.end)
        && state
            .equation
            .source
            .get(binding.begin as usize..binding.end as usize)
            == Some(binding.expected_text.as_str())
}

impl EquationSequenceClipData {
    fn state_index(&self, id: &StateId) -> Option<usize> {
        self.states.iter().position(|state| state.id == *id)
    }

    /// 状態の hold と変形を時間順に並べた区間と、全体の尺。
    pub fn intervals(&self) -> ProjectResult<(Vec<EquationInterval>, i64)> {
        if self.states.is_empty() || self.transitions.len() + 1 != self.states.len() {
            return fail("数式 sequence の状態数と辺数が不正です");
        }
        let mut intervals = Vec::with_capacity(self.states.len() * 2);
        let mut length: i64 = 0;
        let overflow = || ProjectError::new("数式 sequence の尺が 0 以下または overflow です");
        for (index, state) in self.states.iter().enumerate() {
            let mut append = |transition: bool, frames: i64| -> ProjectResult<()> {
                if frames < 1 {
                    return Err(overflow());
                }
                let end = checked_end(length, frames).ok_or_else(overflow)?;
                intervals.push(EquationInterval {
                    transition,
                    index,
                    begin: length,
                    end,
                });
                length = end;
                Ok(())
            };
            append(false, state.hold_frames)?;
            if let Some(transition) = self.transitions.get(index) {
                append(true, transition.frames)?;
            }
        }
        Ok((intervals, length))
    }

    pub fn validate(&self, output_height: i32) -> ProjectResult<()> {
        self.intervals()?;
        let mut state_ids = HashSet::new();
        let mut part_ids = HashSet::new();
        for state in &self.states {
            if !insert_unique(&state.id.0, &mut state_ids) {
                return fail("StateId が空または重複しています");
            }
            state.equation.validate(output_height)?;
            let transparent = parse_argb_color(&state.equation.background_color)
                .is_some_and(|background| background >> 24 == 0);
            if !transparent {
                return fail("数式 sequence の背景は透明でなければなりません");
            }
            if state.revision.is_empty() {
                return fail("数式の revision が空です");
            }
            let mut bound_ranges = Vec::new();
            for part in &state.parts {
                if !insert_unique(&part.id.0, &mut part_ids) {
                    return fail("PartId が空または重複しています");
                }
                let binding = &part.binding;
                if binding.revision.is_empty() || binding.begin < 0 || binding.end <= binding.begin
                {
                    return fail("部分式の範囲または revision が不正です");
                }
                if binding.expected_text.is_empty()
                    || u64::try_from(binding.end - binding.begin).ok()
                        != u64::try_from(binding.expected_text.len()).ok()
                {
                    return fail("部分式の範囲長と expectedText が一致しません");
                }
                // invalid は旧 revision の証人を保持する。新 source の範囲へ黙って移さない。
                if binding.status == BindingStatus::Bound {
                    if !equation_binding_matches_source(state, binding) {
                        return fail("部分式の binding の証人が一致しません");
                    }
                    bound_ranges.push((binding.begin, binding.end));
                }
            }
            bound_ranges.sort_unstable();
            if bound_ranges.windows(2).any(|pair| pair[1].0 < pair[0].1) {
                return fail("部分式の範囲が重複しています");
            }
        }
        let mut transition_ids = HashSet::new();
        for (index, transition) in self.transitions.iter().enumerate() {
            if !insert_unique(&transition.id.0, &mut transition_ids) {
                return fail("TransitionId が空または重複しています");
            }
            let (from_state, to_state) = (&self.states[index], &self.states[index + 1]);
            if transition.from != from_state.id || transition.to != to_state.id {
                return fail("数式の辺が vector 順の隣接状態を結んでいません");
            }
            let (mut from, mut to) = (HashSet::new(), HashSet::new());
            for pair in &transition.correspondence {
                let bound = |part: Option<&SemanticPart>| {
                    part.is_some_and(|part| part.binding.status == BindingStatus::Bound)
                };
                if !bound(from_state.part(&pair.from))
                    || !bound(to_state.part(&pair.to))
                    || !from.insert(&pair.from)
                    || !to.insert(&pair.to)
                {
                    return fail(
                        "部分式の対応は隣接状態の有効な部分式どうしの一対一に限定されます",
                    );
                }
            }
        }
        let mut action_ids = HashSet::new();
        let mut action_ranges: HashMap<&StateId, Vec<(i64, i64)>> = HashMap::new();
        let mut missing_owners: HashMap<&PartId, &StateId> = HashMap::new();
        for action in &self.actions {
            if !insert_unique(&action.id.0, &mut action_ids) {
                return fail("ActionId が空または重複しています");
            }
            let Some(index) = self.state_index(&action.state) else {
                return fail("action の StateId が存在しません");
            };
            let end = checked_end(action.start, action.duration);
            let Some(end) =
                end.filter(|end| action.duration >= 1 && *end <= self.states[index].hold_frames)
            else {
                return fail("action の区間が hold 内に収まりません");
            };
            if action.target.0.is_empty() {
                return fail("action の PartId が空です");
            }
            let target = self.states[index].part(&action.target);
            let target_valid = match target {
                Some(_) => action.target_status == EquationTargetStatus::Present,
                None => {
                    action.target_status == EquationTargetStatus::Missing
                        && !part_ids.contains(&action.target.0)
                }
            };
            if !target_valid {
                return fail("action の対象状態または明示的な missing が不正です");
            }
            if target.is_none() {
                let owner = missing_owners
                    .entry(&action.target)
                    .or_insert(&action.state);
                if **owner != action.state {
                    return fail("欠落 PartId の所有状態が一致しません");
                }
            }
            action_ranges
                .entry(&action.state)
                .or_default()
                .push((action.start, end));
        }
        for ranges in action_ranges.values_mut() {
            ranges.sort_unstable();
            if ranges.windows(2).any(|pair| pair[1].0 < pair[0].1) {
                return fail("初期 P3 では同時 action を指定できません");
            }
        }
        Ok(())
    }

    /// 内部の素材 frame での評価。出力の高さに依らない検証だけを行う。
    pub fn evaluate(&self, frame: i64) -> ProjectResult<EquationEvaluation> {
        self.validate(i32::MAX)?;
        let (intervals, length) = self.intervals()?;
        if !(0..length).contains(&frame) {
            return fail("数式 sequence の frame が範囲外です");
        }
        let Some(interval) = intervals
            .iter()
            .find(|interval| (interval.begin..interval.end).contains(&frame))
        else {
            return fail("数式 sequence の frame が範囲外です");
        };
        let state = self.states[interval.index].id.clone();
        let local_frame = frame - interval.begin;
        let frames = interval.end - interval.begin;
        let mut result = EquationEvaluation {
            transition: interval.transition,
            state,
            to_state: StateId::default(),
            transition_id: TransitionId::default(),
            local_frame,
            frames,
            progress_numerator: 0,
            progress_denominator: 1,
            active_action: None,
        };
        if interval.transition {
            result.to_state = self.states[interval.index + 1].id.clone();
            result.transition_id = self.transitions[interval.index].id.clone();
            result.progress_numerator = local_frame;
            result.progress_denominator = frames;
        } else {
            for action in &self.actions {
                let Some(end) = checked_end(action.start, action.duration) else {
                    return fail("action の終端が overflow です");
                };
                if action.state == result.state && (action.start..end).contains(&local_frame) {
                    result.active_action = Some(action.id.clone());
                }
            }
        }
        Ok(result)
    }

    /// 複製のために全ての内部 ID を発行し直す。実在 ID と保持中の欠落 ID を両方予約し、
    /// 発行は ID ごとに有限回で失敗する。
    pub fn remap_ids(&mut self, generate: &mut dyn FnMut() -> String) -> ProjectResult<()> {
        self.validate(i32::MAX)?;
        let mut reserved: HashSet<String> = HashSet::new();
        for state in &self.states {
            reserved.insert(state.id.0.clone());
            reserved.extend(state.parts.iter().map(|part| part.id.0.clone()));
        }
        reserved.extend(self.transitions.iter().map(|edge| edge.id.0.clone()));
        for action in &self.actions {
            reserved.insert(action.id.0.clone());
            reserved.insert(action.target.0.clone());
        }
        let mut fresh = || -> ProjectResult<String> {
            (0..ID_ATTEMPTS)
                .map(|_| generate())
                .find(|id| !id.is_empty() && reserved.insert(id.clone()))
                .ok_or_else(|| ProjectError::new("内部 ID を発行できません"))
        };
        let mut candidate = self.clone();
        let mut states: HashMap<String, String> = HashMap::new();
        let mut parts: HashMap<String, String> = HashMap::new();
        for state in &mut candidate.states {
            let id = fresh()?;
            states.insert(std::mem::replace(&mut state.id.0, id.clone()), id);
            for part in &mut state.parts {
                let id = fresh()?;
                parts.insert(std::mem::replace(&mut part.id.0, id.clone()), id);
            }
        }
        for edge in &mut candidate.transitions {
            edge.id.0 = fresh()?;
            let (Some(from), Some(to)) = (states.get(&edge.from.0), states.get(&edge.to.0)) else {
                return fail("辺の状態が存在しません");
            };
            (edge.from.0, edge.to.0) = (from.clone(), to.clone());
            for pair in &mut edge.correspondence {
                let (Some(from), Some(to)) = (parts.get(&pair.from.0), parts.get(&pair.to.0))
                else {
                    return fail("対応の部分式が存在しません");
                };
                (pair.from.0, pair.to.0) = (from.clone(), to.clone());
            }
        }
        for action in &mut candidate.actions {
            action.id.0 = fresh()?;
            let Some(state) = states.get(&action.state.0) else {
                return fail("action の状態が存在しません");
            };
            action.state.0 = state.clone();
            if !parts.contains_key(&action.target.0) {
                if action.target_status != EquationTargetStatus::Missing {
                    return fail("action の対象が存在しません");
                }
                let id = fresh()?;
                parts.insert(action.target.0.clone(), id);
            }
            if let Some(target) = parts.get(&action.target.0) {
                action.target.0 = target.clone();
            }
        }
        *self = candidate;
        Ok(())
    }

    /// 候補に `apply` を掛け、前後とも検証に通るときだけ確定する。
    fn edit(
        &mut self,
        output_height: i32,
        apply: impl FnOnce(&mut Self) -> ProjectResult<()>,
    ) -> ProjectResult<()> {
        self.validate(output_height)?;
        let mut candidate = self.clone();
        apply(&mut candidate)?;
        candidate.validate(output_height)?;
        *self = candidate;
        Ok(())
    }

    /// `position` に状態を挿入する。端なら新しい辺 1 本、間なら前後の辺 2 本で置き換える。
    pub fn insert_state(
        &mut self,
        position: usize,
        state: EquationState,
        new_edges: &[EquationStepTransition],
        output_height: i32,
    ) -> ProjectResult<()> {
        self.edit(output_height, |candidate| {
            let middle = position > 0 && position < candidate.states.len();
            if position > candidate.states.len() || new_edges.len() != if middle { 2 } else { 1 } {
                return fail("挿入する状態と新しい辺の数が不正です");
            }
            if middle {
                candidate.transitions.remove(position - 1);
            }
            candidate.states.insert(position, state);
            let at = position.saturating_sub(1);
            candidate
                .transitions
                .splice(at..at, new_edges.iter().cloned());
            Ok(())
        })
    }

    /// 状態を削除する。間の状態なら前後をつなぐ新しい辺（対応なし）が要る。
    pub fn delete_state(
        &mut self,
        id: &StateId,
        new_edge: Option<EquationStepTransition>,
        output_height: i32,
    ) -> ProjectResult<()> {
        self.edit(output_height, |candidate| {
            let Some(index) = candidate
                .state_index(id)
                .filter(|_| candidate.states.len() > 1)
            else {
                return fail("最後の状態または存在しない状態は削除できません");
            };
            let middle = index > 0 && index + 1 < candidate.states.len();
            if middle != new_edge.is_some()
                || new_edge
                    .as_ref()
                    .is_some_and(|edge| !edge.correspondence.is_empty())
            {
                return fail("新しい隣接辺は対応を持たず明示的に作成してください");
            }
            candidate.actions.retain(|action| action.state != *id);
            candidate
                .transitions
                .retain(|edge| edge.from != *id && edge.to != *id);
            candidate.states.remove(index);
            if let Some(edge) = new_edge {
                candidate.transitions.insert(index - 1, edge);
            }
            Ok(())
        })
    }

    pub fn change_hold(
        &mut self,
        id: &StateId,
        frames: i64,
        output_height: i32,
    ) -> ProjectResult<()> {
        self.edit(output_height, |candidate| {
            let Some(index) = candidate.state_index(id) else {
                return fail("状態が存在しません");
            };
            candidate.states[index].hold_frames = frames;
            Ok(())
        })
    }

    pub fn change_transition(
        &mut self,
        id: &TransitionId,
        frames: i64,
        output_height: i32,
    ) -> ProjectResult<()> {
        self.edit(output_height, |candidate| {
            let Some(edge) = candidate.transitions.iter_mut().find(|edge| edge.id == *id) else {
                return fail("辺が存在しません");
            };
            edge.frames = frames;
            Ok(())
        })
    }

    /// 式を丸ごと置き換える。全ての部分式は無効になり、その状態に接する辺の対応は消える。
    pub fn replace_source(
        &mut self,
        id: &StateId,
        source: String,
        revision: String,
        output_height: i32,
    ) -> ProjectResult<()> {
        self.edit(output_height, |candidate| {
            let Some(index) = candidate.state_index(id) else {
                return fail("状態が存在しません");
            };
            let state = &mut candidate.states[index];
            state.equation.source = source;
            state.revision = revision;
            for part in &mut state.parts {
                part.binding.status = BindingStatus::Invalid;
            }
            for edge in &mut candidate.transitions {
                if edge.from == *id || edge.to == *id {
                    edge.correspondence.clear();
                }
            }
            Ok(())
        })
    }

    /// 部分式を削除する。参照していた action は missing になり、対応は消える。
    pub fn delete_part(
        &mut self,
        state_id: &StateId,
        id: &PartId,
        output_height: i32,
    ) -> ProjectResult<()> {
        self.edit(output_height, |candidate| {
            let Some(index) = candidate
                .state_index(state_id)
                .filter(|index| candidate.states[*index].part(id).is_some())
            else {
                return fail("部分式が存在しません");
            };
            candidate.states[index].parts.retain(|part| part.id != *id);
            for action in &mut candidate.actions {
                if action.state == *state_id && action.target == *id {
                    action.target_status = EquationTargetStatus::Missing;
                }
            }
            for edge in &mut candidate.transitions {
                edge.correspondence
                    .retain(|pair| pair.from != *id && pair.to != *id);
            }
            Ok(())
        })
    }

    /// 実在の部分式と、action が保持する欠落参照の ID（同じ名前空間で衝突させない）。
    fn part_id_reserved(&self, id: &PartId) -> bool {
        self.states.iter().any(|state| state.part(id).is_some())
            || self.actions.iter().any(|action| action.target == *id)
    }

    /// 状態を並べ替える。新しい順の隣接の組 (from, to) が旧い辺と同じ順の組なら、その辺（ID・尺・対応）を
    /// 保つ。それ以外の隣接は `new_edge_id` で発行した新しい辺（尺 `fresh_frames`、対応なし）にする。
    pub fn move_state(
        &mut self,
        id: &StateId,
        new_index: usize,
        new_edge_id: &mut dyn FnMut() -> String,
        fresh_frames: i64,
        output_height: i32,
    ) -> ProjectResult<()> {
        self.edit(output_height, |candidate| {
            let Some(from) = candidate
                .state_index(id)
                .filter(|_| new_index < candidate.states.len())
            else {
                return fail("移動する状態または移動先が不正です");
            };
            if from == new_index {
                return Ok(());
            }
            let moved = candidate.states.remove(from);
            candidate.states.insert(new_index, moved);
            let mut reserved: HashSet<String> = candidate
                .transitions
                .iter()
                .map(|edge| edge.id.0.clone())
                .collect();
            let mut edges = Vec::with_capacity(candidate.states.len() - 1);
            for pair in candidate.states.windows(2) {
                let (a, b) = (&pair[0].id, &pair[1].id);
                // 同じ順の組だけを保つ。逆向きの組や離れた組の対応は引き継がない。
                if let Some(kept) = candidate
                    .transitions
                    .iter()
                    .find(|edge| edge.from == *a && edge.to == *b)
                {
                    edges.push(kept.clone());
                    continue;
                }
                let Some(fresh) = (0..ID_ATTEMPTS)
                    .map(|_| new_edge_id())
                    .find(|id| !id.is_empty() && reserved.insert(id.clone()))
                else {
                    return fail("新しい辺の ID を発行できません");
                };
                edges.push(EquationStepTransition {
                    id: TransitionId(fresh),
                    from: a.clone(),
                    to: b.clone(),
                    frames: fresh_frames,
                    correspondence: Vec::new(),
                });
            }
            candidate.transitions = edges;
            Ok(())
        })
    }

    /// 新しい部分式を Bound で追加する。ID は sequence 内で未使用（欠落参照の ID とも別）であること。
    pub fn add_part(
        &mut self,
        state_id: &StateId,
        added: SemanticPart,
        output_height: i32,
    ) -> ProjectResult<()> {
        self.edit(output_height, |candidate| {
            let Some(index) = candidate.state_index(state_id) else {
                return fail("部分式を追加する状態が存在しません");
            };
            if added.id.0.is_empty() || candidate.part_id_reserved(&added.id) {
                return fail("部分式の ID が空または使用中です");
            }
            if !equation_binding_matches_source(&candidate.states[index], &added.binding) {
                return fail("部分式の範囲・revision・証人が今の式と一致しません");
            }
            candidate.states[index].parts.push(added);
            Ok(())
        })
    }

    /// 削除された部分式（action が missing で参照する ID）を同じ PartId で作り直し、その ID を参照する
    /// action を present へ戻す。対応は作らない。binding は今の式の Bound であること。
    pub fn restore_missing_part(
        &mut self,
        state_id: &StateId,
        restored: SemanticPart,
        output_height: i32,
    ) -> ProjectResult<()> {
        self.edit(output_height, |candidate| {
            let Some(index) = candidate.state_index(state_id) else {
                return fail("修復する状態が存在しません");
            };
            let referenced = candidate.actions.iter().any(|action| {
                action.state == *state_id
                    && action.target == restored.id
                    && action.target_status == EquationTargetStatus::Missing
            });
            let exists = candidate
                .states
                .iter()
                .any(|state| state.part(&restored.id).is_some());
            if !referenced || exists {
                return fail("この状態の action が欠落として参照する PartId ではありません");
            }
            if !equation_binding_matches_source(&candidate.states[index], &restored.binding) {
                return fail("部分式の範囲・revision・証人が今の式と一致しません");
            }
            for action in &mut candidate.actions {
                if action.state == *state_id && action.target == restored.id {
                    action.target_status = EquationTargetStatus::Present;
                }
            }
            candidate.states[index].parts.push(restored);
            Ok(())
        })
    }

    pub fn rename_part(
        &mut self,
        state_id: &StateId,
        id: &PartId,
        label: String,
        output_height: i32,
    ) -> ProjectResult<()> {
        self.edit(output_height, |candidate| {
            let Some(index) = candidate.state_index(state_id) else {
                return fail("状態が存在しません");
            };
            let Some(part) = candidate.states[index]
                .parts
                .iter_mut()
                .find(|part| part.id == *id)
            else {
                return fail("部分式が存在しません");
            };
            part.label = label;
            Ok(())
        })
    }

    pub fn add_correspondence(
        &mut self,
        id: &TransitionId,
        pair: PartPair,
        output_height: i32,
    ) -> ProjectResult<()> {
        self.edit(output_height, |candidate| {
            let Some(edge) = candidate.transitions.iter_mut().find(|edge| edge.id == *id) else {
                return fail("辺が存在しません");
            };
            edge.correspondence.push(pair);
            Ok(())
        })
    }

    pub fn remove_correspondence(
        &mut self,
        id: &TransitionId,
        pair: &PartPair,
        output_height: i32,
    ) -> ProjectResult<()> {
        self.edit(output_height, |candidate| {
            let Some(edge) = candidate.transitions.iter_mut().find(|edge| edge.id == *id) else {
                return fail("辺が存在しません");
            };
            let before = edge.correspondence.len();
            edge.correspondence.retain(|existing| existing != pair);
            if edge.correspondence.len() == before {
                return fail("対応が存在しません");
            }
            Ok(())
        })
    }

    pub fn add_action(&mut self, action: EquationAction, output_height: i32) -> ProjectResult<()> {
        self.edit(output_height, |candidate| {
            candidate.actions.push(action);
            Ok(())
        })
    }

    /// action の ID と所有状態は変えない。対象・区間・operation だけを置き換える。
    pub fn update_action(
        &mut self,
        updated: &EquationAction,
        output_height: i32,
    ) -> ProjectResult<()> {
        self.edit(output_height, |candidate| {
            let Some(action) = candidate
                .actions
                .iter_mut()
                .find(|action| action.id == updated.id)
            else {
                return fail("action が存在しません");
            };
            if action.state != updated.state {
                return fail("action の所有状態は変更できません");
            }
            *action = updated.clone();
            Ok(())
        })
    }

    pub fn delete_action(&mut self, id: &ActionId, output_height: i32) -> ProjectResult<()> {
        self.edit(output_height, |candidate| {
            let before = candidate.actions.len();
            candidate.actions.retain(|action| action.id != *id);
            if candidate.actions.len() == before {
                return fail("action が存在しません");
            }
            Ok(())
        })
    }

    /// 入力欄からの差分で式を書き換える。範囲の前の部分式はそのまま、後ろの部分式は差分だけずらし、
    /// 範囲と交わる部分式は無効にする。挿入は begin では前、end では後に入る。
    pub fn edit_source_trusted(
        &mut self,
        id: &StateId,
        delta: &TrustedEquationEdit,
        revision: String,
        output_height: i32,
    ) -> ProjectResult<()> {
        self.validate(output_height)?;
        let mut candidate = self.clone();
        let mismatch =
            || ProjectError::new("信頼済み編集の境界・旧式・新式・revision が一致しません");
        let Some(index) = candidate.state_index(id) else {
            return Err(mismatch());
        };
        let begin = utf16_to_utf8_offset(&delta.old_source, delta.begin_utf16);
        let end = utf16_to_utf8_offset(&delta.old_source, delta.end_utf16);
        let (Some(begin), Some(end)) = (begin, end) else {
            return Err(mismatch());
        };
        let state = &mut candidate.states[index];
        let expected_new_source = (begin <= end).then(|| {
            format!(
                "{}{}{}",
                &delta.old_source[..begin],
                delta.replacement,
                &delta.old_source[end..]
            )
        });
        if state.equation.source != delta.old_source
            || expected_new_source.as_deref() != Some(delta.new_source.as_str())
            || revision.is_empty()
            || revision == state.revision
            || i64::try_from(delta.new_source.len()).is_err()
        {
            return Err(mismatch());
        }
        let shift = delta.replacement.len() as i64 - (end - begin) as i64;
        for part in &mut state.parts {
            let binding = &mut part.binding;
            if binding.status != BindingStatus::Bound {
                continue;
            }
            if end as i64 <= binding.begin {
                binding.begin += shift;
                binding.end += shift;
            } else if (begin as i64) < binding.end {
                binding.status = BindingStatus::Invalid;
                continue;
            }
            binding.revision = revision.clone();
        }
        state.equation.source = delta.new_source.clone();
        state.revision = revision;
        // 無効になった参照だけを落とし、対応を自動で作り直さない。
        let invalid: HashSet<PartId> = state
            .parts
            .iter()
            .filter(|part| part.binding.status == BindingStatus::Invalid)
            .map(|part| part.id.clone())
            .collect();
        for edge in &mut candidate.transitions {
            let (from_here, to_here) = (edge.from == *id, edge.to == *id);
            edge.correspondence.retain(|pair| {
                !((from_here && invalid.contains(&pair.from))
                    || (to_here && invalid.contains(&pair.to)))
            });
        }
        candidate.validate(output_height)?;
        *self = candidate;
        Ok(())
    }

    /// 無効になった部分式を今の式の範囲へ結び直す。
    pub fn rebind_part(
        &mut self,
        state_id: &StateId,
        part_id: &PartId,
        binding: SourceBinding,
        output_height: i32,
    ) -> ProjectResult<()> {
        self.validate(output_height)?;
        let mut candidate = self.clone();
        let Some(state) = candidate
            .states
            .iter_mut()
            .find(|state| state.id == *state_id)
        else {
            return fail("再 binding の状態または PartId が存在しません");
        };
        if state.part(part_id).is_none() {
            return fail("再 binding の状態または PartId が存在しません");
        }
        if !equation_binding_matches_source(state, &binding) {
            return fail("再 binding の revision・UTF-8 境界・証人が不正です");
        }
        if let Some(part) = state.parts.iter_mut().find(|part| part.id == *part_id) {
            part.binding = binding;
        }
        candidate.validate(output_height)?;
        *self = candidate;
        Ok(())
    }
}
