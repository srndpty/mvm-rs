//! 数式 sequence の試験。C++ 版 tests/harness/test_equation_sequence.cpp の assertion を移植したもの。
//! schema 20 以前の読み込みに関する検査は、旧 schema を受理しない方針のため移植しない。

// 試験の失敗は大きく落ちてよい（helper 関数も試験の一部）。Project::default() から field を足す書き方は意図的。
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::field_reassign_with_default
)]

mod common;

use std::path::PathBuf;

use common::*;
use mvm_core::Rational;
use mvm_core::source_frame_mapping::{convert_frame_boundary, source_frame_at_output_position};
use mvm_project::equation_sequence::*;
use mvm_project::json::{parse_project_json, serialize_project_json};
use mvm_project::timeline::*;
use mvm_project::*;
use serde_json::{Value, json};

const HEIGHT: i32 = 1080;

fn state(id: &str, hold: i64) -> EquationState {
    let mut state = EquationState {
        id: StateId::new(id),
        hold_frames: hold,
        revision: format!("rev-{id}"),
        ..EquationState::default()
    };
    state.equation.source = "x+x".to_owned();
    state.parts = vec![SemanticPart {
        id: PartId::new(format!("part-{id}")),
        label: "項".to_owned(),
        binding: binding(&state.revision, 0, 1),
    }];
    state
}

fn binding(revision: &str, begin: i64, end: i64) -> SourceBinding {
    SourceBinding {
        revision: revision.to_owned(),
        begin,
        end,
        expected_text: "x".to_owned(),
        status: BindingStatus::Bound,
    }
}

fn edge(id: &str, from: &str, to: &str, frames: i64) -> EquationStepTransition {
    EquationStepTransition {
        id: TransitionId::new(id),
        from: StateId::new(from),
        to: StateId::new(to),
        frames,
        correspondence: Vec::new(),
    }
}

fn pair(from: &str, to: &str) -> PartPair {
    PartPair {
        from: PartId::new(from),
        to: PartId::new(to),
    }
}

fn action(
    id: &str,
    state: &str,
    target: &str,
    start: i64,
    duration: i64,
    operation: EquationOperation,
) -> EquationAction {
    EquationAction {
        id: ActionId::new(id),
        state: StateId::new(state),
        target: PartId::new(target),
        target_status: EquationTargetStatus::Present,
        start,
        duration,
        operation,
    }
}

/// hold 4・transition 2 の状態を count 個並べ、s0 の part に [1, 3) の Outline を付けた sequence。
fn sequence(count: usize) -> EquationSequenceClipData {
    let mut data = EquationSequenceClipData::default();
    for index in 0..count {
        let id = format!("s{index}");
        data.states.push(state(&id, 4));
        if index > 0 {
            data.transitions.push(edge(
                &format!("t{}", index - 1),
                &format!("s{}", index - 1),
                &id,
                2,
            ));
        }
    }
    data.actions.push(action(
        "a0",
        "s0",
        "part-s0",
        1,
        2,
        EquationOperation::Outline,
    ));
    data
}

fn project_with(data: EquationSequenceClipData, timeline_fps: Rational) -> Project {
    let mut project = Project::default();
    project.timeline_fps = timeline_fps;
    add_equation_sequence(
        &mut project,
        data,
        "clip".to_owned(),
        "数式".to_owned(),
        V1,
        0,
    )
    .unwrap();
    project
}

fn project() -> Project {
    project_with(sequence(3), fps(60, 1))
}

#[test]
fn hold_and_transition_boundaries() {
    sequence(1).validate(HEIGHT).unwrap();
    for count in [1, 2, 3] {
        let data = sequence(count);
        let length = count as i64 * 4 + (count as i64 - 1) * 2;
        for frame in 0..length {
            let evaluation = data.evaluate(frame).unwrap();
            let (index, local) = (frame / 6, frame % 6);
            assert_eq!(
                evaluation.state,
                StateId::new(format!("s{index}")),
                "frame {frame}"
            );
            assert_eq!(evaluation.transition, local >= 4, "frame {frame}");
            assert_eq!(
                evaluation.local_frame,
                if local >= 4 { local - 4 } else { local }
            );
            if evaluation.transition {
                assert_eq!(
                    (
                        evaluation.progress_numerator,
                        evaluation.progress_denominator
                    ),
                    (local - 4, 2),
                    "i/N は 1 を含まない"
                );
            }
        }
        assert!(
            data.evaluate(-1).is_err() && data.evaluate(length).is_err(),
            "最初と最後の範囲外"
        );
    }
    let mut data = sequence(2);
    data.transitions[0].frames = 1;
    let evaluation = data.evaluate(4).unwrap();
    assert!(evaluation.transition);
    assert_eq!(
        (
            evaluation.progress_numerator,
            evaluation.progress_denominator
        ),
        (0, 1),
        "N=1 は source の標本"
    );
    let evaluation = data.evaluate(5).unwrap();
    assert!(
        !evaluation.transition && evaluation.local_frame == 0,
        "N=1 の次は target の hold 0"
    );
    for frame in 0..5 {
        assert_eq!(
            data.evaluate(frame).unwrap().active_action.is_some(),
            frame == 1 || frame == 2,
            "action の半開区間"
        );
    }
}

type Mutation = Box<dyn Fn(&mut EquationSequenceClipData)>;

#[test]
fn invalid_sequences_are_rejected() {
    let good = sequence(3);
    good.validate(HEIGHT).unwrap();
    let cases: Vec<(&str, Mutation)> = vec![
        ("空状態", Box::new(|d| d.states.clear())),
        ("hold 0", Box::new(|d| d.states[0].hold_frames = 0)),
        ("transition 0", Box::new(|d| d.transitions[0].frames = 0)),
        ("action 0", Box::new(|d| d.actions[0].duration = 0)),
        (
            "全長 overflow",
            Box::new(|d| d.states[0].hold_frames = i64::MAX),
        ),
        (
            "action 終端 overflow",
            Box::new(|d| d.actions[0].start = i64::MAX),
        ),
        (
            "重複 StateId",
            Box::new(|d| d.states[1].id = d.states[0].id.clone()),
        ),
        (
            "重複 TransitionId",
            Box::new(|d| d.transitions[1].id = d.transitions[0].id.clone()),
        ),
        (
            "重複 PartId",
            Box::new(|d| d.states[1].parts[0].id = d.states[0].parts[0].id.clone()),
        ),
        (
            "重複 ActionId",
            Box::new(|d| {
                let mut a = d.actions[0].clone();
                a.start = 3;
                a.duration = 1;
                d.actions.push(a);
            }),
        ),
        (
            "別状態の PartId",
            Box::new(|d| d.actions[0].target = PartId::new("part-s1")),
        ),
        (
            "欠落 StateId",
            Box::new(|d| d.actions[0].state = StateId::new("missing")),
        ),
        (
            "明示しない欠落 PartId",
            Box::new(|d| d.actions[0].target = PartId::new("missing")),
        ),
        ("hold を跨ぐ action", Box::new(|d| d.actions[0].start = 3)),
        (
            "同じ対象の重複 action",
            Box::new(|d| {
                let mut a = d.actions[0].clone();
                a.id = ActionId::new("a1");
                d.actions.push(a);
            }),
        ),
        (
            "異なる対象の同時 action",
            Box::new(|d| {
                let revision = d.states[0].revision.clone();
                d.states[0].parts.push(SemanticPart {
                    id: PartId::new("another"),
                    label: "項".to_owned(),
                    binding: binding(&revision, 2, 3),
                });
                let mut a = d.actions[0].clone();
                a.id = ActionId::new("a1");
                a.target = PartId::new("another");
                d.actions.push(a);
            }),
        ),
        (
            "vector 順と異なる辺",
            Box::new(|d| d.transitions[0].to = StateId::new("s2")),
        ),
        ("辺の数", Box::new(|d| drop(d.transitions.pop()))),
        (
            "一対一でない対応",
            Box::new(|d| {
                d.transitions[0].correspondence =
                    vec![pair("part-s0", "part-s1"), pair("part-s0", "part-s1")]
            }),
        ),
        (
            "隣接でない対応",
            Box::new(|d| d.transitions[0].correspondence = vec![pair("part-s0", "part-s2")]),
        ),
        (
            "不透明背景",
            Box::new(|d| d.states[0].equation.background_color = "#FF000000".to_owned()),
        ),
        (
            "半透明背景",
            Box::new(|d| d.states[0].equation.background_color = "#80000000".to_owned()),
        ),
        (
            "bound 範囲外",
            Box::new(|d| d.states[0].parts[0].binding.end = 9),
        ),
        (
            "UTF-8 codepoint の途中を bound 境界にしない",
            Box::new(|d| {
                d.states[0].equation.source = "あx".to_owned();
                d.states[0].parts[0].binding.begin = 1;
                d.states[0].parts[0].binding.end = 2;
            }),
        ),
        (
            "invalid でも旧範囲の証人長を検証",
            Box::new(|d| {
                let binding = &mut d.states[0].parts[0].binding;
                binding.status = BindingStatus::Invalid;
                binding.end = i64::MAX;
            }),
        ),
    ];
    for (name, mutate) in cases {
        let mut data = good.clone();
        mutate(&mut data);
        assert!(data.validate(HEIGHT).is_err(), "{name}");
    }

    let mut data = good.clone();
    data.states[0].equation.background_color = "#00aBcDef".to_owned();
    data.validate(HEIGHT)
        .expect("既存の色文法と alpha=0 の契約");
    let mut data = good.clone();
    data.actions[0].target = PartId::new("missing");
    data.actions[0].target_status = EquationTargetStatus::Missing;
    data.validate(HEIGHT).expect("明示 missing は修復可能");
    let mut data = good;
    data.states[0].equation.source = "あx".to_owned();
    data.states[0].parts[0].binding.end = 3;
    data.states[0].parts[0].binding.expected_text = "あ".to_owned();
    data.validate(HEIGHT).expect("UTF-8 byte 範囲の独立な対照");

    let mut project = project();
    project.timeline_clips[0].speed = fps(2, 1);
    assert!(
        validate_timeline(&project).is_err(),
        "数式 sequence は速度変更に対応しない"
    );
}

/// source 範囲 [2, out) の clip に内部編集を掛け、外側の source 範囲を比べる。
fn range_case(
    name: &str,
    out: i64,
    expected: Option<(i64, i64)>,
    edit: impl FnOnce(&mut EquationSequenceClipData) -> ProjectResult<()>,
) {
    let mut project = project();
    project.timeline_clips[0].source_in_frame = 2;
    project.timeline_clips[0].source_out_frame = out;
    let before = project.clone();
    let result = edit_equation_sequence(&mut project, "clip", edit);
    match expected {
        Some((length, expected_out)) => {
            result.unwrap_or_else(|error| panic!("{name}: {error}"));
            let clip = &project.timeline_clips[0];
            assert_eq!(
                (
                    clip.source_frame_count,
                    clip.source_in_frame,
                    clip.source_out_frame
                ),
                (length, 2, expected_out),
                "{name}"
            );
        }
        None => {
            assert!(result.is_err(), "{name}");
            assert_eq!(project, before, "{name} は原子的に拒否");
        }
    }
}

#[test]
fn internal_edits_update_outer_source_range() {
    let hold = |frames| {
        move |d: &mut EquationSequenceClipData| d.change_hold(&StateId::new("s2"), frames, HEIGHT)
    };
    let transition = |frames| {
        move |d: &mut EquationSequenceClipData| {
            d.change_transition(&TransitionId::new("t0"), frames, HEIGHT)
        }
    };
    range_case("全体末尾 hold 延長", 16, Some((18, 18)), hold(6));
    range_case("全体末尾 hold 短縮", 16, Some((14, 14)), hold(2));
    range_case(
        "全体末尾 transition 延長",
        16,
        Some((18, 18)),
        transition(4),
    );
    range_case(
        "全体末尾 transition 短縮",
        16,
        Some((15, 15)),
        transition(1),
    );
    range_case("状態挿入", 16, Some((19, 19)), |d| {
        d.insert_state(
            1,
            state("insert", 3),
            &[edge("ea", "s0", "insert", 1), edge("eb", "insert", "s1", 1)],
            HEIGHT,
        )
    });
    range_case("状態削除", 16, Some((9, 9)), |d| {
        d.delete_state(
            &StateId::new("s1"),
            Some(edge("edge", "s0", "s2", 1)),
            HEIGHT,
        )
    });
    range_case("右 trim 延長", 12, Some((18, 12)), hold(6));
    range_case("右 trim 内で短縮", 12, Some((14, 12)), hold(2));
    range_case("可視末尾より短縮", 15, None, hold(1));
}

#[test]
fn state_and_part_edits() {
    let mut data = sequence(3);
    data.transitions[0].correspondence = vec![pair("part-s0", "part-s1")];
    data.transitions[1].correspondence = vec![pair("part-s1", "part-s2")];
    data.actions.push(action(
        "a1",
        "s1",
        "part-s1",
        0,
        1,
        EquationOperation::Pulse,
    ));
    data.delete_state(
        &StateId::new("s1"),
        Some(edge("new-edge", "s0", "s2", 1)),
        HEIGHT,
    )
    .unwrap();
    assert_eq!(
        (
            data.states.len(),
            data.actions.len(),
            data.transitions.len()
        ),
        (2, 1, 1),
        "所有 action を消す"
    );
    assert!(
        data.transitions[0].correspondence.is_empty(),
        "新しい辺は対応を継承しない"
    );
    data.insert_state(
        1,
        state("insert", 3),
        &[
            edge("edge-a", "s0", "insert", 1),
            edge("edge-b", "insert", "s2", 1),
        ],
        HEIGHT,
    )
    .unwrap();
    data.change_hold(&StateId::new("insert"), 5, HEIGHT)
        .unwrap();
    data.change_transition(&TransitionId::new("edge-a"), 3, HEIGHT)
        .unwrap();
    let before = data.clone();
    assert!(data.change_hold(&StateId::new("s0"), 1, HEIGHT).is_err());
    assert_eq!(data, before, "action を切る編集は原子的に拒否");
    data.replace_source(
        &StateId::new("s0"),
        "x+x".to_owned(),
        "new-revision".to_owned(),
        HEIGHT,
    )
    .unwrap();
    assert_eq!(
        data.states[0].parts[0].binding.status,
        BindingStatus::Invalid,
        "同じ文字で binding を復活させない"
    );
    data.delete_part(&StateId::new("s0"), &PartId::new("part-s0"), HEIGHT)
        .unwrap();
    assert_eq!(
        data.actions[0].target_status,
        EquationTargetStatus::Missing,
        "部分式の削除は明示 missing を保つ"
    );

    let mut identical = sequence(3);
    let revision = identical.states[0].revision.clone();
    identical.states[0].parts.push(SemanticPart {
        id: PartId::new("same-text"),
        label: "同じ文字".to_owned(),
        binding: binding(&revision, 2, 3),
    });
    identical
        .delete_part(&StateId::new("s0"), &PartId::new("part-s0"), HEIGHT)
        .unwrap();
    assert_eq!(
        identical.actions[0].target,
        PartId::new("part-s0"),
        "同じ文字の別 PartId へ付け替えない"
    );
    assert_eq!(
        identical.actions[0].target_status,
        EquationTargetStatus::Missing
    );

    let mut project = project_with(data, fps(60, 1));
    project.timeline_clips[0].source_out_frame -= 1;
    let before = project.clone();
    assert!(
        edit_equation_sequence(&mut project, "clip", |d| d.change_hold(
            &StateId::new("s2"),
            1,
            HEIGHT
        ))
        .is_err()
    );
    assert_eq!(project, before, "内部編集で可視範囲を勝手に短縮しない");
}

#[test]
fn copies_remap_owned_ids() {
    let mut project = project();
    let data = &mut project.timeline_clips[0].equation_sequence;
    data.delete_part(&StateId::new("s0"), &PartId::new("part-s0"), HEIGHT)
        .unwrap();
    let mut another = data.actions[0].clone();
    another.id = ActionId::new("another-action");
    another.start = 3;
    another.duration = 1;
    data.actions.push(another);
    data.states[1].parts[0].id = PartId::new("fresh-1");
    data.actions.push(EquationAction {
        target_status: EquationTargetStatus::Missing,
        ..action(
            "dangling-action",
            "s1",
            "fresh-2",
            0,
            1,
            EquationOperation::Pulse,
        )
    });
    data.transitions[1].correspondence = vec![pair("fresh-1", "part-s2")];
    let data = data.clone();
    let mut new_id = prefixed_ids("fresh");
    let copy = copy_internal_clip(&project.timeline_clips[0], &mut new_id).unwrap();
    let remapped = &copy.equation_sequence;
    assert_ne!(copy.id, project.timeline_clips[0].id);
    assert_ne!(remapped.states[0].id, data.states[0].id);
    assert_eq!(
        remapped.actions[0].target, remapped.actions[1].target,
        "欠落参照も同じ新 ID へ remap"
    );
    assert_eq!(
        remapped.actions[0].target_status,
        EquationTargetStatus::Missing
    );
    assert_eq!(
        remapped.states[0].equation, data.states[0].equation,
        "ID の変更で意味を変えない"
    );
    assert_eq!(remapped.actions[0].start, data.actions[0].start);
    remapped.validate(HEIGHT).unwrap();
    assert_eq!(
        remapped.transitions[1].from, remapped.states[1].id,
        "全辺の端点・対応・action の状態を一括 remap"
    );
    assert_eq!(remapped.transitions[1].to, remapped.states[2].id);
    assert_eq!(
        remapped.transitions[1].correspondence[0].from,
        remapped.states[1].parts[0].id
    );
    assert_eq!(
        remapped.transitions[1].correspondence[0].to,
        remapped.states[2].parts[0].id
    );
    assert_eq!(remapped.actions[2].state, remapped.states[1].id);
    for taken in ["fresh-1", "fresh-2"] {
        assert_ne!(
            remapped.states[0].id.0, taken,
            "実在 ID と欠落 ID の両方を避ける"
        );
        assert_ne!(remapped.actions[2].target.0, taken);
    }
    let duplicate = copy_internal_clip(&copy, &mut new_id).unwrap();
    assert_ne!(duplicate.id, copy.id);
    let mut colliding = copy.equation_sequence.clone();
    let before = colliding.clone();
    assert!(
        colliding.remap_ids(&mut || "s0".to_owned()).is_err(),
        "衝突する発行器は有限回で拒否"
    );
    assert_eq!(colliding, before);
}

#[test]
fn split_gives_independent_owned_data() {
    let mut new_id = prefixed_ids("split");
    for operation in [EquationOperation::Outline, EquationOperation::Pulse] {
        for split in [2, 5, 8] {
            let mut original = project();
            original.timeline_clips[0].equation_sequence.actions[0].operation = operation;
            let mut divided = original.clone();
            split_timeline_clips(
                &mut divided,
                &ids(&["clip"]),
                split,
                &mut new_id,
                LinkMode::Single,
            )
            .unwrap();
            let (left, right) = (&divided.timeline_clips[0], &divided.timeline_clips[1]);
            assert_eq!(left.id, "clip", "左は ID を保つ");
            assert_eq!(
                left.equation_sequence,
                original.timeline_clips[0].equation_sequence
            );
            assert_eq!(
                (
                    left.source_out_frame,
                    right.source_in_frame,
                    right.source_out_frame
                ),
                (split, split, 16)
            );
            assert_eq!(right.equation_sequence.states.len(), 3);
            assert_ne!(
                right.equation_sequence.states[0].id, left.equation_sequence.states[0].id,
                "完全な data を独立に所有"
            );
            let old =
                evaluate_equation_clip(&original.timeline_clips[0], fps(60, 1), split).unwrap();
            let first = evaluate_equation_clip(right, fps(60, 1), 0).unwrap();
            assert_eq!(
                (
                    old.transition,
                    old.local_frame,
                    old.progress_numerator,
                    old.active_action.is_some()
                ),
                (
                    first.transition,
                    first.local_frame,
                    first.progress_numerator,
                    first.active_action.is_some()
                ),
                "右片の最初の seek は分割前と同じ局所時刻"
            );
        }
    }
}

#[test]
fn rational_source_mapping() {
    for (source, samples) in [
        (fps(60, 1), vec![0, 1, 2, 3, 4, 5]),
        (fps(24, 1), vec![0, 0, 0, 1, 1, 2, 2, 2]),
        (fps(30000, 1001), vec![0, 0, 0, 1, 1, 2, 2, 3]),
    ] {
        let mut clip = project().timeline_clips[0].clone();
        clip.source_fps = source;
        for (position, expected) in samples.into_iter().enumerate() {
            assert_eq!(
                clip_source_frame_at(&clip, fps(60, 1), position as i64).unwrap(),
                expected,
                "{source:?} frame {position}"
            );
        }
    }
    let clip = project().timeline_clips[0].clone();
    for (position, expected) in [0, 2, 5, 7, 10, 12].into_iter().enumerate() {
        assert_eq!(
            clip_source_frame_at(&clip, fps(24, 1), position as i64).unwrap(),
            expected,
            "60 → 24"
        );
    }
    // double では 2^53+1 の整数を保持できない境界。
    let large = 9_007_199_254_740_993;
    assert_eq!(
        source_frame_at_output_position(large, fps(60, 1), fps(60, 1)),
        Some(large)
    );
    assert_eq!(
        convert_frame_boundary(large, fps(60, 1), fps(60, 1), false),
        Some(large)
    );
    assert_eq!(
        convert_frame_boundary(i64::MAX, fps(1, 1), fps(60, 1), true),
        None,
        "結果の overflow"
    );
    assert_eq!(
        source_frame_at_output_position(i64::MAX, fps(i64::MAX, 1), fps(1, i64::MAX)),
        None,
        "中間積の overflow"
    );
    let reverse = project();
    for (position, expected) in [(0, 0), (1, 2), (2, 4), (3, 6), (4, 8), (5, 10)] {
        assert_eq!(
            clip_source_frame_at(&reverse.timeline_clips[0], fps(30000, 1001), position).unwrap(),
            expected,
            "60 → 30000/1001"
        );
    }
}

#[test]
fn trims_keep_internal_phase() {
    for start in [1, 2, 5, 8] {
        let mut project = project();
        let original = project.timeline_clips[0].clone();
        trim_timeline_clip(
            &mut project,
            "clip",
            TrimEdge::Left,
            start,
            LinkMode::Single,
        )
        .unwrap();
        assert_eq!(
            project.timeline_clips[0].equation_sequence, original.equation_sequence,
            "trim は data を変えない"
        );
        let before = evaluate_equation_clip(&original, fps(60, 1), start).unwrap();
        let after = evaluate_equation_clip(&project.timeline_clips[0], fps(60, 1), 0).unwrap();
        assert_eq!(
            (
                &before.state,
                before.local_frame,
                &before.active_action,
                before.progress_numerator
            ),
            (
                &after.state,
                after.local_frame,
                &after.active_action,
                after.progress_numerator
            )
        );
    }
    for source in [fps(24, 1), fps(60, 1), fps(30000, 1001)] {
        let mut mixed = project();
        mixed.timeline_clips[0].source_fps = source;
        // [in, out) の始点は ceil(in R)。trim 前の同じ output 位置から始まる。
        let original = mixed.timeline_clips[0].clone();
        mixed.timeline_clips[0].source_in_frame = 5;
        let origin = clip_source_boundary_to_timeline(&original, 5, fps(60, 1)).unwrap();
        let before = evaluate_equation_clip(&original, fps(60, 1), origin).unwrap();
        let after = evaluate_equation_clip(&mixed.timeline_clips[0], fps(60, 1), 0).unwrap();
        assert_eq!(
            (&before.state, before.local_frame, before.progress_numerator),
            (&after.state, after.local_frame, after.progress_numerator),
            "異なる fps の trim は素材原点の位相を保つ {source:?}"
        );
    }
    let mut outside = project();
    let unchanged = outside.clone();
    assert!(
        trim_timeline_clip(&mut outside, "clip", TrimEdge::Right, 1, LinkMode::Single).is_err(),
        "L の外へ右延長しない"
    );
    assert_eq!(outside, unchanged);
    outside.timeline_clips[0].timeline_start_frame = 10;
    let unchanged = outside.clone();
    assert!(
        trim_timeline_clip(&mut outside, "clip", TrimEdge::Left, -1, LinkMode::Single).is_err(),
        "素材 0 より前へ延ばさない"
    );
    assert!(
        trim_timeline_clip(
            &mut outside,
            "clip",
            TrimEdge::Left,
            i64::MIN,
            LinkMode::Single
        )
        .is_err()
    );
    assert_eq!(outside, unchanged);

    let mut mixed = project();
    mixed.timeline_clips[0].source_fps = fps(24, 1);
    let before = mixed.clone();
    let mut new_id = prefixed_ids("split");
    assert!(
        split_timeline_clips(
            &mut mixed,
            &ids(&["clip"]),
            1,
            &mut new_id,
            LinkMode::Single
        )
        .is_err()
    );
    assert_eq!(
        mixed, before,
        "整数の素材境界で表せない split は原子的に拒否"
    );
    split_timeline_clips(
        &mut mixed,
        &ids(&["clip"]),
        5,
        &mut new_id,
        LinkMode::Single,
    )
    .unwrap();
    assert_eq!(
        (
            mixed.timeline_clips[0].source_out_frame,
            mixed.timeline_clips[1].source_in_frame
        ),
        (2, 2),
        "24 → 60 の split 境界は source 2 / output 5"
    );
}

#[test]
fn frame_rate_and_renderability() {
    let mut project = project();
    project.timeline_clips[0].timeline_start_frame = 60;
    let clip = project.timeline_clips[0].clone();
    set_timeline_frame_rate(&mut project, fps(24, 1)).unwrap();
    let changed = &project.timeline_clips[0];
    assert_eq!(
        changed.equation_sequence, clip.equation_sequence,
        "Project の fps 変更は内部時間と ID を保つ"
    );
    assert_eq!(
        (
            changed.source_fps,
            changed.source_frame_count,
            changed.timeline_start_frame
        ),
        (fps(60, 1), 16, 24)
    );

    let mut data = sequence(3);
    data.actions[0].duration = 1;
    let low = project_with(data, fps(60, 1));
    validate_timeline(&low).unwrap();
    let rendering = equation_renderability(&low.timeline_clips[0], fps(24, 1), HEIGHT);
    assert!(!rendering.renderable, "構造は有効でも出力標本が無い");
    assert_eq!(rendering.unsampled_actions, vec![ActionId::new("a0")]);
    assert!(
        equation_renderability(&low.timeline_clips[0], fps(60, 1), HEIGHT).renderable,
        "同じ fps なら見える"
    );

    let fractional = project_with(sequence(3), fps(30000, 1001));
    assert_eq!(
        fractional.timeline_clips[0].source_fps,
        fps(30000, 1001),
        "作成時の Project fps を source fps にする"
    );

    let mut invalid_source = self::project();
    invalid_source.timeline_clips[0]
        .equation_sequence
        .replace_source(
            &StateId::new("s0"),
            "x+x".to_owned(),
            "changed".to_owned(),
            HEIGHT,
        )
        .unwrap();
    validate_timeline(&invalid_source).expect("修復可能な invalid binding は構造として有効");
    assert!(
        !equation_renderability(&invalid_source.timeline_clips[0], fps(60, 1), HEIGHT).renderable,
        "出力はできない"
    );
}

fn json_path() -> PathBuf {
    std::env::temp_dir()
        .join("mvm-rs-equation-test")
        .join("equation-test.mvm")
}

fn saved_project() -> Project {
    let mut project = project();
    project.timeline_clips[0].equation_sequence.transitions[0].correspondence =
        vec![pair("part-s0", "part-s1")];
    project
}

#[test]
fn equation_sequence_round_trips_through_json() {
    let mut project = saved_project();
    let text = serialize_project_json(&project, &json_path()).unwrap();
    assert_eq!(
        parse_project_json(&text, &json_path()).unwrap(),
        project,
        "厳密な往復"
    );
    project.timeline_clips[0]
        .equation_sequence
        .delete_part(&StateId::new("s0"), &PartId::new("part-s0"), HEIGHT)
        .unwrap();
    let text = serialize_project_json(&project, &json_path()).unwrap();
    assert_eq!(
        parse_project_json(&text, &json_path()).unwrap(),
        project,
        "修復可能な missing の往復"
    );
}

#[test]
fn equation_sequence_json_rejects_unknown_fields() {
    let text = serialize_project_json(&saved_project(), &json_path()).unwrap();
    let base: Value = serde_json::from_str(&text).unwrap();
    let rejected = |value: &Value| {
        parse_project_json(&serde_json::to_string(value).unwrap(), &json_path()).is_err()
    };
    assert!(!rejected(&base), "対照: 保存したままの JSON は読める");
    fn sequence(value: &mut Value) -> &mut Value {
        &mut value["timeline_clips"][0]["equation_sequence"]
    }
    let mutate = |change: &dyn Fn(&mut Value)| {
        let mut value = base.clone();
        change(&mut value);
        value
    };
    for operation in ["set_color", "reveal", "conceal", "unknown"] {
        assert!(
            rejected(&mutate(
                &|v| sequence(v)["actions"][0]["operation"] = json!(operation)
            )),
            "未知の operation {operation}"
        );
    }
    let insert = |pointer: &'static str, key: &'static str, value: Value| {
        mutate(&move |v| {
            let target = sequence(v)
                .pointer_mut(pointer)
                .unwrap()
                .as_object_mut()
                .unwrap();
            target.insert(key.to_owned(), value.clone());
        })
    };
    for (pointer, key) in [
        ("/states/0", "backend"),
        ("", "cache_key"),
        ("/states/0/parts/0", "glyph_id"),
        ("/states/0/parts/0/binding", "segment_index"),
        ("/transitions/0", "manim"),
        ("/actions/0", "cache_key"),
        ("/transitions/0/correspondence/0", "renderer_handle"),
    ] {
        assert!(
            rejected(&insert(pointer, key, json!("x"))),
            "renderer 由来の field {pointer}/{key}"
        );
    }
    let unknown_clip_field = mutate(&|v| {
        v["timeline_clips"][0]
            .as_object_mut()
            .unwrap()
            .insert("backend".to_owned(), json!("manim"));
    });
    assert!(
        rejected(&unknown_clip_field),
        "数式 sequence clip の未知 field"
    );
    // 重複 key は Value を経由すると消えるので、文字列で作る。
    let marker = "\"operation\": \"outline\"";
    assert!(text.contains(marker));
    let duplicated = text.replacen(
        marker,
        "\"operation\": \"outline\", \"operation\": \"pulse\"",
        1,
    );
    assert!(
        parse_project_json(&duplicated, &json_path()).is_err(),
        "重複 field"
    );
}
