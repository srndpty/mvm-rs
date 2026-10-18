//! Graph clip の試験。C++ 版 tests/harness/test_graph_domain.cpp の assertion を移植したもの。
//! schema 21 からの移行の検査は、旧 schema を受理しない方針のため移植しない。

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
use mvm_project::graph_clip::*;
use mvm_project::json::{parse_project_json, serialize_project_json};
use mvm_project::timeline::*;
use mvm_project::*;
use serde_json::{Value, json};

const LENGTH: i64 = 300;

fn default_graph() -> GraphClipData {
    GraphClipData::with_default_function(GraphFunctionId("f1".to_owned()))
}

fn valid(data: &GraphClipData) -> bool {
    data.validate(LENGTH) == GraphValidationStatus::Valid
}

fn changed(change: impl FnOnce(&mut GraphClipData)) -> GraphClipData {
    let mut data = default_graph();
    change(&mut data);
    data
}

fn draw(frames: i64) -> GraphIntro {
    GraphIntro {
        kind: GraphIntroKind::Draw,
        frames,
    }
}

#[test]
fn graph_validation_limits() {
    assert!(valid(&default_graph()), "既定の Graph");
    assert!(!valid(&changed(|d| d.functions.clear())), "関数 0 個");
    for count in [1, 3, 4] {
        let data = changed(|d| {
            let function = d.functions[0].clone();
            d.functions = (0..count)
                .map(|index| GraphFunction {
                    id: GraphFunctionId(format!("f{index}")),
                    ..function.clone()
                })
                .collect();
        });
        assert_eq!(valid(&data), count <= 3, "関数の数 {count}");
    }
    for id in ["", "f!", "日本語", "f.1"] {
        assert!(
            !valid(&changed(
                |d| d.functions[0].id = GraphFunctionId(id.to_owned())
            )),
            "ID {id:?}"
        );
    }
    assert!(
        !valid(&changed(|d| d.functions.push(d.functions[0].clone()))),
        "ID の重複"
    );
    assert!(
        valid(&changed(
            |d| d.functions[0].id = GraphFunctionId("a".repeat(64))
        )),
        "ID 64 byte"
    );
    assert!(
        !valid(&changed(
            |d| d.functions[0].id = GraphFunctionId("a".repeat(65))
        )),
        "ID 65 byte"
    );
    assert!(
        valid(&changed(|d| d.functions[0].expression = "x".repeat(4096))),
        "式 4096 byte"
    );
    assert!(
        !valid(&changed(|d| d.functions[0].expression = "x".repeat(4097))),
        "式 4097 byte"
    );
    assert!(
        valid(&changed(|d| d.functions[0].label = "a".repeat(4096))),
        "label 4096 byte"
    );
    assert!(
        !valid(&changed(|d| d.functions[0].label = "a".repeat(4097))),
        "label 4097 byte"
    );
    assert!(
        !valid(&changed(|d| d.axes.x_label = "a".repeat(4097))),
        "軸 label の上限"
    );
    assert!(
        valid(&changed(|d| d.functions[0].stroke_width = 64.0)),
        "線幅 64"
    );
    for width in [0.0, -1.0, 64.01, f64::INFINITY] {
        assert!(
            !valid(&changed(|d| d.functions[0].stroke_width = width)),
            "線幅 {width}"
        );
    }
    assert!(
        !valid(&changed(|d| d.functions[0].domain_min = Some(f64::INFINITY))),
        "domain の非有限値"
    );
    assert!(
        !valid(&changed(|d| d.viewport.x_min = 5.0)),
        "viewport の順序"
    );
    assert!(
        !valid(&changed(|d| d.viewport.x_max = f64::NAN)),
        "viewport の非有限値"
    );
    assert!(
        !valid(&changed(|d| {
            d.functions[0].domain_min = Some(6.0);
            d.functions[0].domain_max = Some(7.0);
        })),
        "domain と viewport の交差が空"
    );
    // 式の解釈は backend の仕事。編集途中の不正な式は data として持てる。
    for expression in ["", "sin(", "未知", "__import__('os')"] {
        assert!(
            valid(&changed(
                |d| d.functions[0].expression = expression.to_owned()
            )),
            "{expression:?}"
        );
    }
    assert!(!valid(&changed(|d| d.intro.frames = 1)), "None の frames");
    for frames in [0, 1, 300, 301, 10001] {
        assert_eq!(
            valid(&changed(|d| d.intro = draw(frames))),
            (1..=300).contains(&frames),
            "Draw {frames}"
        );
    }
}

#[test]
fn function_edits_keep_identity() {
    let mut data = default_graph();
    let mut function = data.functions[0].clone();
    function.id = GraphFunctionId("f2".to_owned());
    data.add_function(function.clone(), LENGTH).unwrap();
    data.move_function(&function.id, 0, LENGTH).unwrap();
    assert_eq!(data.functions[0].id, function.id, "並べ替えで ID を保つ");
    function.expression = "bad(".to_owned();
    data.update_function(&function, LENGTH).unwrap();
    assert_eq!(data.functions[0].id, function.id);
    data.delete_function(&GraphFunctionId("f1".to_owned()), LENGTH)
        .unwrap();
    assert!(
        data.delete_function(&function.id, LENGTH).is_err(),
        "最後の 1 つは消さない"
    );
}

fn graph_project() -> Project {
    let mut project = Project::default();
    add_graph(
        &mut project,
        "clip".to_owned(),
        GraphFunctionId("f1".to_owned()),
        "グラフ".to_owned(),
        V1,
        0,
    )
    .unwrap();
    project
}

#[test]
fn graph_edits_in_project() {
    let mut project = graph_project();
    assert_eq!(
        project.timeline_clips[0].source_frame_count,
        default_still_clip_frames(project.timeline_fps),
        "既定 5 秒"
    );
    let before = project.clone();
    assert!(
        edit_graph(&mut project, "clip", |g| {
            g.functions.clear();
            Ok(())
        })
        .is_err()
    );
    assert_eq!(project, before, "拒否した編集は変えない");
    edit_graph(&mut project, "clip", |g| {
        g.functions[0].expression = "sin(".to_owned();
        g.axes.x_label = "横軸".to_owned();
        Ok(())
    })
    .unwrap();
    assert_eq!(
        project.timeline_clips[0].graph.functions[0].id,
        GraphFunctionId("f1".to_owned()),
        "編集で ID を保つ"
    );
}

fn json_path() -> PathBuf {
    std::env::temp_dir()
        .join("mvm-rs-graph-test")
        .join("graph.mvm")
}

#[test]
fn graph_round_trips_through_json() {
    let project = graph_project();
    let text = serialize_project_json(&project, &json_path()).unwrap();
    assert_eq!(parse_project_json(&text, &json_path()).unwrap(), project);
    for fields in 0..4 {
        let mut optional = project.clone();
        let graph = &mut optional.timeline_clips[0].graph;
        if fields & 1 != 0 {
            graph.functions[0].domain_min = Some(-3.0);
        }
        if fields & 2 != 0 {
            graph.functions[0].domain_max = Some(3.0);
        }
        graph.axes.show_axes = false;
        graph.axes.show_grid = false;
        graph.axes.y_label = "縦軸".to_owned();
        graph.functions[0].label = "関数".to_owned();
        graph.functions[0].color = "#00FF6655".to_owned();
        let text = serialize_project_json(&optional, &json_path()).unwrap();
        assert_eq!(
            parse_project_json(&text, &json_path()).unwrap(),
            optional,
            "domain の省略・片側・両端と透明色 {fields}"
        );
    }
}

#[test]
fn graph_json_negative_cases() {
    let text = serialize_project_json(&graph_project(), &json_path()).unwrap();
    let base: Value = serde_json::from_str(&text).unwrap();
    let rejected = |value: &Value| {
        parse_project_json(&serde_json::to_string(value).unwrap(), &json_path()).is_err()
    };
    assert!(!rejected(&base), "対照: 保存したままの JSON は読める");
    let mutate = |change: &dyn Fn(&mut Value)| {
        let mut value = base.clone();
        change(&mut value);
        value
    };
    let graph = |value: &mut Value| -> Value { value["timeline_clips"][0]["graph"].take() };
    let put = |value: &mut Value, graph: Value| value["timeline_clips"][0]["graph"] = graph;
    let with_graph = |change: &dyn Fn(&mut Value)| {
        mutate(&|value| {
            let mut g = graph(value);
            change(&mut g);
            put(value, g);
        })
    };
    assert!(
        rejected(&with_graph(&|g| drop(
            g["viewport"]
                .as_object_mut()
                .unwrap()
                .insert("unknown".to_owned(), json!(1))
        ))),
        "未知の field"
    );
    assert!(
        rejected(&with_graph(&|g| drop(
            g["functions"][0]
                .as_object_mut()
                .unwrap()
                .insert("domain_min".to_owned(), Value::Null)
        ))),
        "domain の null"
    );
    assert!(
        rejected(&mutate(&|v| v["schema_version"] = json!(21))),
        "schema 21"
    );
    assert!(
        rejected(&mutate(&|v| v["schema_version"] = json!(23))),
        "schema 23"
    );
    assert!(
        rejected(&mutate(&|v| v["timeline_clips"][0]["kind"] = json!("math"))),
        "kind と data の不一致"
    );
    // 重複 key は Value を経由すると消えるので、文字列で作る。
    let marker = "\"show_grid\": ";
    assert!(text.contains(marker));
    let duplicated = text.replacen(marker, "\"show_grid\": false, \"show_grid\": ", 1);
    assert!(
        parse_project_json(&duplicated, &json_path()).is_err(),
        "重複 field"
    );
}

#[test]
fn draw_intro_mapping_split_and_duration() {
    let mut project = graph_project();
    let id = project.timeline_clips[0].graph.functions[0].id.clone();
    let mut fresh = prefixed_ids("new");
    let copied = copy_internal_clip(&project.timeline_clips[0], &mut fresh).unwrap();
    assert_ne!(
        copied.id, project.timeline_clips[0].id,
        "コピーは新しい clip ID"
    );
    assert_ne!(copied.graph.functions[0].id, id, "コピーは新しい関数 ID");

    project.timeline_clips[0].graph.intro = draw(3);
    let output = project.timeline_fps;
    for n in 0..4 {
        let frame = evaluate_graph_clip(&project.timeline_clips[0], output, n).unwrap();
        let expected = if n < 3 { (n, 3) } else { (1, 1) };
        assert_eq!(frame.source_frame, n);
        assert_eq!(
            (frame.progress_numerator, frame.progress_denominator),
            expected,
            "N=3 の frame {n}"
        );
    }
    split_timeline_clips(
        &mut project,
        &ids(&["clip"]),
        1,
        &mut fresh,
        LinkMode::Single,
    )
    .unwrap();
    assert_eq!(project.timeline_clips.len(), 2);
    assert_eq!(
        project.timeline_clips[0].graph.functions[0].id, id,
        "左は ID を保つ"
    );
    assert_ne!(
        project.timeline_clips[1].graph.functions[0].id, id,
        "右は新しい ID を所有"
    );
    let right = evaluate_graph_clip(&project.timeline_clips[1], output, 0).unwrap();
    assert_eq!(
        (
            right.source_frame,
            right.progress_numerator,
            right.progress_denominator
        ),
        (1, 1, 3),
        "右片は Draw を続ける"
    );
    let split = project.clone();
    let right_id = project.timeline_clips[1].id.clone();
    assert!(
        set_graph_source_duration(&mut project, &right_id, 2).is_err(),
        "Draw の尺より短くしない"
    );
    assert_eq!(project, split);

    let mut extension = Project::default();
    add_graph(
        &mut extension,
        "extended".to_owned(),
        GraphFunctionId("f".to_owned()),
        "延長".to_owned(),
        V1,
        0,
    )
    .unwrap();
    extension.timeline_clips[0].graph.intro = draw(3);
    set_graph_source_duration(&mut extension, "extended", 400).unwrap();
    assert_eq!(extension.timeline_clips[0].source_out_frame, 400);
    assert_eq!(
        extension.timeline_clips[0].graph.intro.frames, 3,
        "source の延長は intro を変えない"
    );
    set_graph_source_duration(&mut extension, "extended", 3).unwrap();
    assert!(set_graph_source_duration(&mut extension, "extended", 2).is_err());
    extension.timeline_clips[0].source_frame_count = 10;
    extension.timeline_clips[0].source_out_frame = 10;
    let full = extension.timeline_clips[0].graph.clone();
    trim_timeline_clip(
        &mut extension,
        "extended",
        TrimEdge::Left,
        1,
        LinkMode::Single,
    )
    .unwrap();
    assert_eq!(
        extension.timeline_clips[0].graph, full,
        "trim は data と ID を保つ"
    );
    assert_eq!(extension.timeline_clips[0].source_in_frame, 1);
    let trimmed = evaluate_graph_clip(&extension.timeline_clips[0], fps(60, 1), 0).unwrap();
    assert_eq!(
        (trimmed.progress_numerator, trimmed.progress_denominator),
        (1, 3),
        "trim は Draw をやり直さない"
    );

    let mut one = copied.clone();
    one.source_in_frame = 2;
    one.source_out_frame = 3;
    one.graph.intro = draw(3);
    let last = evaluate_graph_clip(&one, fps(60, 1), 0).unwrap();
    assert_eq!(
        (last.source_frame, last.progress_numerator),
        (2, 2),
        "1 frame の区間の最後の intro"
    );
    for (num, den) in [(24, 1), (24000, 1001), (30000, 1001), (60, 1)] {
        let mut clip = copied.clone();
        clip.source_fps = fps(num, den);
        clip.graph.intro = draw(3);
        let frame = evaluate_graph_clip(&clip, fps(60, 1), 2).unwrap();
        assert_eq!(
            frame.source_frame,
            2 * num / (60 * den),
            "{num}/{den} の始点"
        );
    }
}
