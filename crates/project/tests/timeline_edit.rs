//! timeline 編集の試験。C++ 版 tests/harness/test_timeline_edit.cpp の assertion を移植したもの。
//! 期待値は手で計算した値であり、実装の式を呼んで作らない。

// 試験の失敗は大きく落ちてよい（helper 関数も試験の一部）。Project::default() から field を足す書き方は意図的。
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::field_reassign_with_default
)]
mod common;

use common::*;
use mvm_project::clip_effects::{ClipKeyframe, KeyInterpolation, evaluate_clip_keys};
use mvm_project::timeline::*;
use mvm_project::*;

fn key(frame: i64, value: f64) -> ClipKeyframe {
    ClipKeyframe::new(frame, value)
}

fn key_with(frame: i64, value: f64, interpolation: KeyInterpolation) -> ClipKeyframe {
    ClipKeyframe {
        interpolation,
        ..ClipKeyframe::new(frame, value)
    }
}

// 大きな timeline の検証が clip 数の 2 乗にならないこと。時間の閾値は環境で揺れるので、区間を比べた回数を見る。
#[test]
fn large_timeline_validation_scales() {
    const CLIPS_PER_TRACK: i64 = 10_000;
    let mut project = Project::default();
    // V1 と V2 に隙間なく並べる。clip の並びは track をまたいで交互にする
    // （track ごとに並べ直していなければ、隣どうしを見るだけでは重なりを見落とす）。
    for index in 0..CLIPS_PER_TRACK {
        for track in 0..2 {
            let mut value = clip_on(
                &format!("big-{track}-{index}"),
                TimelineClipKind::Video,
                TrackRef::video(track),
            );
            value.source_out_frame = 10;
            value.timeline_start_frame = (CLIPS_PER_TRACK - 1 - index) * 10;
            project.timeline_clips.push(value);
        }
    }
    let valid = validate_timeline(&project).unwrap();
    assert_eq!(valid.total_frames, CLIPS_PER_TRACK * 10);
    // track ごとに先頭以外の clip を 1 回ずつ比べる。
    assert_eq!(valid.overlap_comparisons, 2 * (CLIPS_PER_TRACK as u64 - 1));

    // 対照: 離れた位置の clip と重ねると検出する。
    let mut overlapping = project.clone();
    overlapping
        .timeline_clips
        .last_mut()
        .unwrap()
        .timeline_start_frame = 5;
    assert!(
        validate_timeline(&overlapping)
            .unwrap_err()
            .message()
            .contains("重複")
    );
    // 終端の最大を持つ長い clip が、間に短い clip を挟んだ後ろの clip と重なる形。
    let mut nested = Project::default();
    let long = clip("long");
    let mut short = clip("short");
    let mut late = clip("late");
    short.source_out_frame = 10;
    short.timeline_start_frame = 10;
    late.source_out_frame = 10;
    late.timeline_start_frame = 100;
    nested.timeline_clips = vec![late, short, long];
    let error = validate_timeline(&nested).unwrap_err();
    assert!(error.message().contains("long") && error.message().contains("short"));
}

// Undo 履歴は件数だけでなく大きさでも上限を決める。
#[test]
fn undo_history_budget() {
    let drops = |undo: &[usize], redo: &[usize], max_entries, max_bytes, undo_drop, redo_drop| {
        edit_history_entries_to_drop(undo, redo, max_entries, max_bytes)
            == EditHistoryDrop {
                undo: undo_drop,
                redo: redo_drop,
            }
    };
    assert!(drops(&[1; 101], &[], 100, 1000, 1, 0), "件数の上限");
    // 40 + 40 + 40 = 120 > 100 なので古い 1 件を捨てて 80。
    assert!(drops(&[40, 40, 40], &[], 100, 100, 1, 0), "byte の上限");
    assert!(
        drops(&[40, 60], &[], 100, 100, 0, 0),
        "上限ちょうどは捨てない"
    );
    // 最新の 1 件は予算を超えても残す。
    assert!(drops(&[10, 500], &[], 100, 100, 1, 0) && drops(&[500], &[], 100, 100, 0, 0));
    // 距離 2 は両側にあり、同じなら Undo 側から捨てる。
    assert!(drops(&[30, 30], &[30, 30], 100, 100, 1, 0));
    // Redo 側の方が遠ければ Redo 側から捨てる。
    assert!(drops(&[30], &[30, 30, 30], 100, 100, 0, 1));
    assert!(
        drops(&[1, 1], &[1, 1], 3, 1000, 1, 0),
        "件数の上限も合計で数える"
    );
    // 現在の状態に隣り合う Undo と Redo の 1 件ずつは残す。
    assert!(drops(&[10, 500], &[10, 500], 100, 100, 1, 1));
    assert!(drops(&[500], &[500], 100, 100, 0, 0));
}

#[test]
fn speed_duration_without_ripple() {
    let project = three_clips();
    let before = project.clone();
    let edit = speed_edit(1, 2);
    let mut rejected = project.clone();
    assert!(set_clip_speed_duration(&mut rejected, "id-A", &edit, LinkMode::Linked).is_err());
    assert_eq!(
        rejected, before,
        "リップル無効時の重なりを拒否し、Project を保つ"
    );
    let preview =
        preview_clip_speed_duration(&project, "id-A", &edit, LinkMode::Linked).unwrap_err();
    assert!(
        preview.overlaps_following,
        "後続 clip との重なりを上書き確認の対象として返す"
    );

    // 50% で A は 0-600 になり、300-600 の Manim を丸ごと覆う。B（600-900）は触らない。
    let overwrite = ClipSpeedDurationEdit {
        overwrite: true,
        ..edit
    };
    let mut overwritten = project.clone();
    set_clip_speed_duration(&mut overwritten, "id-A", &overwrite, LinkMode::Linked).unwrap();
    assert_eq!(overwritten.timeline_clips.len(), 2);
    assert_eq!(overwritten.timeline_clips[0].id, "id-A");
    assert_eq!(overwritten.timeline_clips[1].id, "id-B");
    assert_eq!(overwritten.timeline_clips[1].timeline_start_frame, 600);
    assert_eq!(overwritten.timeline_clips[1].source_in_frame, 0);

    // 40% で A は 0-750 になり、B の先頭 150 frame を削る（B の終端 900 は動かない）。
    let overwrite = ClipSpeedDurationEdit {
        speed: fps(2, 5),
        ..overwrite
    };
    let mut overwritten = project.clone();
    set_clip_speed_duration(&mut overwritten, "id-A", &overwrite, LinkMode::Linked).unwrap();
    assert_eq!(overwritten.timeline_clips.len(), 2);
    let b = &overwritten.timeline_clips[1];
    assert_eq!(
        (
            b.timeline_start_frame,
            b.source_in_frame,
            b.source_out_frame
        ),
        (750, 150, 300)
    );

    // 上書きの短縮は通常の左 trim と同じく effect を消さない。B（300 frame）を 150 frame へ縮める。
    let overwritten_fades = |fade_in: i64, fade_out: i64| {
        let mut faded = project.clone();
        faded.timeline_clips[2].effects.fade_in_frames = fade_in;
        faded.timeline_clips[2].effects.fade_out_frames = fade_out;
        set_clip_speed_duration(&mut faded, "id-A", &overwrite, LinkMode::Linked).unwrap();
        let b = find(&faded, "id-B").unwrap();
        (b.effects.fade_in_frames, b.effects.fade_out_frames)
    };
    assert_eq!(overwritten_fades(100, 0), (100, 0), "収まる fade in は残す");
    // fade in 200 + fade out 100 は収まらないので、末尾側の fade out を残して fade in を 50 に詰める。
    assert_eq!(overwritten_fades(200, 100), (50, 100));

    // 削除した clip のリンク相手は片方だけのリンクにならないよう未リンクにする。
    let mut linked = project.clone();
    linked.timeline_clips[1].kind = TimelineClipKind::Video;
    linked.timeline_clips[1].link_group_id = "covered".to_owned();
    let mut partner = audio_clip("partner");
    partner.timeline_start_frame = 300;
    partner.link_group_id = "covered".to_owned();
    linked.timeline_clips.push(partner);
    let overwrite = ClipSpeedDurationEdit {
        speed: fps(1, 2),
        ..overwrite
    };
    set_clip_speed_duration(&mut linked, "id-A", &overwrite, LinkMode::Linked).unwrap();
    assert_eq!(linked.timeline_clips.len(), 3);
    assert_eq!(linked.timeline_clips[2].id, "id-partner");
    assert!(linked.timeline_clips[2].link_group_id.is_empty());
}

#[test]
fn speed_duration_with_ripple_and_pitch() {
    let mut project = three_clips();
    let before = project.clone();
    let mut edit = ClipSpeedDurationEdit {
        ripple: true,
        ..speed_edit(1, 2)
    };
    set_clip_speed_duration(&mut project, "id-A", &edit, LinkMode::Linked).unwrap();
    assert_eq!(project.timeline_clips[0].timeline_start_frame, 0);
    assert_eq!(duration_at(&project, 0), 600);
    assert_eq!(project.timeline_clips[1].timeline_start_frame, 600);
    assert_eq!(project.timeline_clips[2].timeline_start_frame, 900);
    edit.speed = fps(1, 1);
    set_clip_speed_duration(&mut project, "id-A", &edit, LinkMode::Linked).unwrap();
    assert_eq!(project, before, "速度を元に戻して後続 clip を詰める");
    assert!(set_clip_speed_duration(&mut project, "id-A", &edit, LinkMode::Linked).is_err());
    assert_eq!(project, before, "変化のない速度指定を拒否する");

    // 等速のピッチ保持は preview だけが stretcher を通す食い違いになるので持たせない。
    let mut pitched = before.clone();
    pitched.timeline_clips[0].preserve_pitch = true;
    assert!(
        validate_timeline(&pitched).is_err(),
        "等速のピッチ保持を拒否する"
    );
    let mut pitched = before.clone();
    let mut pitch = ClipSpeedDurationEdit {
        preserve_pitch: true,
        ripple: true,
        ..speed_edit(1, 2)
    };
    set_clip_speed_duration(&mut pitched, "id-A", &pitch, LinkMode::Linked).unwrap();
    assert!(pitched.timeline_clips[0].preserve_pitch);
    pitch.speed = fps(1, 1);
    set_clip_speed_duration(&mut pitched, "id-A", &pitch, LinkMode::Linked).unwrap();
    assert!(
        !pitched.timeline_clips[0].preserve_pitch,
        "等速へ戻したときにピッチ保持を落とす"
    );
    validate_timeline(&pitched).unwrap();
}

fn held_project() -> Project {
    let mut held = Project::default();
    held.timeline_clips = vec![clip("hold-source")];
    let mut ids = prefixed_ids("new");
    insert_frame_hold(&mut held, "id-hold-source", 120, 120, &mut ids).unwrap();
    held
}

#[test]
fn frame_hold_splits_and_places() {
    let held = held_project();
    assert_eq!(held.timeline_clips.len(), 3);
    assert_eq!(held.timeline_clips[0].source_out_frame, 120);
    assert_eq!(held.timeline_clips[1].timeline_start_frame, 240);
    assert_eq!(held.timeline_clips[1].source_in_frame, 120);
    let hold = &held.timeline_clips[2];
    assert_eq!(hold.frame_hold.as_ref().unwrap().source_frame, 120);
    assert_eq!(hold.timeline_start_frame, 120);
    assert_eq!(clip_source_frame_at(hold, fps(60, 1), 119).unwrap(), 120);
}

// 保持中も動きは timeline の時間で進み、保持後の右側 clip もその続きから再開する。300 frame の clip の 120 へ
// 120 frame の保持を入れると、timeline の frame t の値は元の動きの min(t, 299) になる。
#[test]
fn frame_hold_keeps_motion_continuous() {
    let spline = ClipKeyframe {
        interpolation: KeyInterpolation::Spline,
        curve_start: 0.0,
        curve_end: 1.0,
        control1: 0.2,
        control2: 0.8,
        ..ClipKeyframe::new(0, 0.0)
    };
    for keys in [
        vec![key(0, 0.0), key(299, 299.0)],
        vec![key_with(0, 0.0, KeyInterpolation::EaseOut), key(299, 299.0)],
        vec![
            key_with(0, 0.0, KeyInterpolation::EaseInOut),
            key(150, 100.0),
            key(299, 299.0),
        ],
        vec![spline, key(299, 299.0)],
    ] {
        let mut motion = Project::default();
        motion.timeline_clips = vec![clip("motion-hold")];
        motion.timeline_clips[0].effects.position_x_keys = keys.clone();
        let mut ids = prefixed_ids("motion");
        insert_frame_hold(&mut motion, "id-motion-hold", 120, 120, &mut ids).unwrap();
        assert_eq!(motion.timeline_clips.len(), 3);
        let value_at = |frame: i64| {
            motion
                .timeline_clips
                .iter()
                .find(|piece| {
                    frame >= piece.timeline_start_frame && frame < clip_end(&motion, piece)
                })
                .map(|piece| {
                    evaluate_clip_keys(
                        &piece.effects.position_x_keys,
                        piece.effects.position_x_percent,
                        frame - piece.timeline_start_frame,
                    )
                })
                .unwrap_or(f64::NAN)
        };
        for frame in 0..420 {
            let expected = if keys.len() == 2 && keys[0].interpolation == KeyInterpolation::Linear {
                frame.min(299) as f64
            } else {
                evaluate_clip_keys(&keys, 0.0, frame.min(299))
            };
            assert!(
                (value_at(frame) - expected).abs() < 1e-8,
                "保持の前後・途中で動きが連続しません: frame {frame}"
            );
        }
        assert!((value_at(240) - value_at(239)).abs() < 2.0);
        validate_timeline(&motion).unwrap();
    }
}

// 保持は挿入位置の見た目（不透明度）で止める。fade-in 240 frame の中央（120）なら約 50%、
// key 0:100% → 200:0% の 120 なら 40%。automation は捨て、この値を基本値へ焼き込む。
#[test]
fn frame_hold_bakes_opacity() {
    let held_opacity = |setup: &dyn Fn(&mut TimelineClip)| {
        let mut faded = Project::default();
        faded.timeline_clips = vec![clip("fade-source")];
        setup(&mut faded.timeline_clips[0]);
        let mut ids = prefixed_ids("fade");
        insert_frame_hold(&mut faded, "id-fade-source", 120, 120, &mut ids).unwrap();
        let hold = faded
            .timeline_clips
            .iter()
            .find(|value| value.frame_hold.is_some())
            .unwrap();
        assert!(hold.effects.opacity_keys.is_empty());
        assert_eq!(
            (hold.effects.fade_in_frames, hold.effects.fade_out_frames),
            (0, 0)
        );
        hold.effects.opacity_percent
    };
    let fade_held = held_opacity(&|value| value.effects.fade_in_frames = 240);
    assert!((fade_held - 50.0).abs() <= 1.0, "{fade_held}");
    let key_held =
        held_opacity(&|value| value.effects.opacity_keys = vec![key(0, 100.0), key(200, 0.0)]);
    assert!((key_held - 40.0).abs() <= 1.0, "{key_held}");
}

#[test]
fn frame_hold_rejects_invalid_holds() {
    let mut held = held_project();
    let before = held.clone();
    let mut ids = prefixed_ids("bad");
    assert!(insert_frame_hold(&mut held, "id-hold-source", 0, 120, &mut ids).is_err());
    assert_eq!(held, before, "clip の端へのフレーム保持を拒否する");
    let hold_id = held.timeline_clips[2].id.clone();
    assert!(insert_frame_hold(&mut held, &hold_id, 180, 120, &mut ids).is_err());
    assert_eq!(held, before, "保持 clip からの再挿入を拒否する");

    let invalid = |change: &dyn Fn(&mut TimelineClip)| {
        let mut project = before.clone();
        change(&mut project.timeline_clips[2]);
        project
    };
    assert!(validate_timeline(&invalid(&|hold| hold.speed = fps(2, 1))).is_err());
    assert!(
        validate_timeline(&invalid(&|hold| hold
            .frame_hold
            .as_mut()
            .unwrap()
            .source_frame = 300))
        .is_err()
    );
    assert!(validate_timeline(&invalid(&|hold| hold.preserve_pitch = true)).is_err());
    assert!(validate_timeline(&invalid(&|hold| hold.link_group_id = "bad".to_owned())).is_err());
    let mut odd = invalid(&|hold| {
        let frame_hold = hold.frame_hold.as_mut().unwrap();
        frame_hold.source_frame = 1;
        frame_hold.source_fps = fps(120, 1);
    });
    assert!(frame_hold_producer_position(&odd.timeline_clips[2], fps(60, 1)).is_err());
    assert!(
        validate_timeline(&odd).is_err(),
        "出力位置へ換算できない素材 frame を拒否する"
    );
    // 同じ 120 fps の frame 1 でも、保持元が 50%（実効 60 fps）なら timeline の 1 frame 目に出る。
    odd.timeline_clips[2].frame_hold.as_mut().unwrap().speed = fps(1, 2);
    assert_eq!(
        frame_hold_producer_position(&odd.timeline_clips[2], fps(60, 1)),
        Ok(1)
    );
    validate_timeline(&odd).unwrap();
    odd.timeline_clips[2].frame_hold.as_mut().unwrap().speed = fps(1, 1000);
    assert!(
        validate_timeline(&odd).is_err(),
        "範囲外の保持元速度を拒否する"
    );
}

// 120 fps 素材を 50% にした clip の奇数 frame（素材 fps のままでは 60 fps timeline へ出せない）でも保持できる。
#[test]
fn frame_hold_of_slow_motion_high_fps_source() {
    let mut slow = Project::default();
    slow.timeline_clips = vec![clip("slow")];
    let source = &mut slow.timeline_clips[0];
    source.source_fps = fps(120, 1);
    source.source_frame_count = 600;
    source.source_out_frame = 600;
    source.speed = fps(1, 2);
    let mut ids = prefixed_ids("slow");
    insert_frame_hold(&mut slow, "id-slow", 61, 120, &mut ids).unwrap();
    let hold = slow
        .timeline_clips
        .iter()
        .find_map(|value| value.frame_hold.as_ref())
        .unwrap();
    assert_eq!(hold.source_frame, 61);
    assert_eq!(hold.speed, fps(1, 2));
    validate_timeline(&slow).unwrap();
}

#[test]
fn frame_hold_opens_gaps_on_all_tracks() {
    let mut multi = Project::default();
    add_track(&mut multi, TrackKind::Video).unwrap();
    multi.timeline_clips = vec![
        clip("main"),
        clip_on("overlay", TimelineClipKind::Video, V2),
        audio_clip("music"),
    ];
    let mut ids = prefixed_ids("multi");
    insert_frame_hold(&mut multi, "id-main", 120, 120, &mut ids).unwrap();
    assert_eq!(
        multi.timeline_clips.len(),
        7,
        "全 track を分割して保持区間を作る"
    );
    let shifted = |track: TrackRef| {
        multi
            .timeline_clips
            .iter()
            .filter(|piece| piece.id != "id-overlay" && piece.id != "id-music")
            .filter(|piece| piece.frame_hold.is_none())
            .filter(|piece| piece.timeline_start_frame == 240 && piece.track == track)
            .count()
    };
    assert_eq!((shifted(V2), shifted(A1)), (1, 1));
    validate_timeline(&multi).unwrap();
}

#[test]
fn frame_conversions_snap_to_latest_source_boundary() {
    for (num, den) in [(24, 1), (25, 1), (30000, 1001), (30, 1), (60, 1)] {
        for timeline_frame in 0..240 {
            let source =
                timeline_boundary_to_source_boundary(timeline_frame, fps(num, den), fps(60, 1))
                    .unwrap();
            let at =
                source_boundary_to_timeline_boundary(source, fps(num, den), fps(60, 1)).unwrap();
            let next = source_boundary_to_timeline_boundary(source + 1, fps(num, den), fps(60, 1))
                .unwrap();
            assert!(
                at <= timeline_frame,
                "逆変換が drag 位置を越えない source 境界へ snap する"
            );
            assert!(next > timeline_frame, "逆変換が最新の source 境界を選ぶ");
        }
    }
    assert!(source_boundary_to_timeline_boundary(i64::MAX, fps(1, 1), fps(i64::MAX, 1)).is_err());

    let project = Project::default();
    let normalized = clip("normalized");
    let mut equivalent = normalized.clone();
    equivalent.source_fps = fps(120, 2);
    assert!(source_rate_matches_timeline_rate(&project, &normalized));
    assert!(source_rate_matches_timeline_rate(&project, &equivalent));
}

// timeline fps を 60 以外へ設定できること。表の外は拒否すること。configurable と measured を混ぜないこと。
#[test]
fn timeline_frame_rates() {
    for (num, den) in [(30, 1), (30000, 1001), (24, 1), (120, 2)] {
        assert!(
            is_configurable_timeline_frame_rate(fps(num, den)),
            "{num}/{den}"
        );
    }
    for (num, den) in [(48, 1), (0, 1), (60, 0)] {
        assert!(
            !is_configurable_timeline_frame_rate(fps(num, den)),
            "{num}/{den}"
        );
    }
    assert!(is_measured_timeline_frame_rate(fps(60, 1)));
    for (num, den) in [(24, 1), (30000, 1001), (50, 1)] {
        assert!(
            !is_measured_timeline_frame_rate(fps(num, den)),
            "{num}/{den}"
        );
    }

    let mut project = Project::default();
    project.timeline_fps = fps(30, 1);
    let mut thirty = clip("thirty");
    thirty.source_fps = fps(30, 1);
    project.timeline_clips.push(thirty);
    assert_eq!(validate_timeline(&project).unwrap().total_frames, 300);
    project.timeline_fps = fps(48, 1);
    assert!(
        validate_timeline(&project).is_err(),
        "設定表に無い timeline fps を拒否する"
    );
    let mut reducible = Project::default();
    reducible.timeline_fps = fps(120, 2);
    assert!(
        validate_timeline(&reducible).is_err(),
        "約分されていない timeline fps を拒否する"
    );
}

// fps の変更では素材 domain を保ち、timeline 上の秒の位置だけを換算する。
#[test]
fn timeline_frame_rate_change() {
    let mut empty = Project::default();
    set_timeline_frame_rate(&mut empty, fps(24, 1)).unwrap();
    assert_eq!(empty.timeline_fps, fps(24, 1));
    set_timeline_frame_rate(&mut empty, fps(30000, 1001)).unwrap();
    assert_eq!(empty.timeline_fps, fps(30000, 1001));
    assert!(set_timeline_frame_rate(&mut empty, fps(48, 1)).is_err());
    assert!(set_timeline_frame_rate(&mut empty, fps(120, 2)).is_err());

    let mut with_clip = Project::default();
    let mut audio = audio_clip("voice");
    audio.source_in_frame = 60; // 左 trim 済み。素材 domain は取り込み時の fps
    append_timeline_clip(&mut with_clip, audio, A1).unwrap();
    with_clip.timeline_clips[0].timeline_start_frame = 120;
    let source_before = with_clip.timeline_clips[0].clone();
    set_project_video_settings(&mut with_clip, 1280, 720, fps(24, 1)).unwrap();
    assert_eq!(
        (with_clip.output_width, with_clip.output_height),
        (1280, 720)
    );
    assert_eq!(with_clip.timeline_fps, fps(24, 1));
    let after = &with_clip.timeline_clips[0];
    assert_eq!(
        after.timeline_start_frame, 48,
        "clip 開始位置の秒位置を保つ"
    );
    assert_eq!(after.source_fps, source_before.source_fps);
    assert_eq!(
        (after.source_in_frame, after.source_out_frame),
        (
            source_before.source_in_frame,
            source_before.source_out_frame
        )
    );
    let before_invalid = with_clip.clone();
    assert!(set_project_video_settings(&mut with_clip, 0, 720, fps(24, 1)).is_err());
    assert_eq!(with_clip, before_invalid);
    // 同じ rate への設定は no-op として成功にする。
    set_timeline_frame_rate(&mut with_clip, fps(24, 1)).unwrap();
}

#[test]
fn trim_and_lookup() {
    let mut project = Project::default();
    let mut fractional = clip("fractional");
    fractional.id = "fractional-id".to_owned();
    fractional.source_fps = fps(30000, 1001);
    project.timeline_clips.push(fractional);
    assert_eq!(
        validate_timeline(&project).unwrap().total_frames,
        601,
        "29.97fps clip の尺"
    );
    assert_eq!(active_clip_at(&project, V1, 0), Some(0));
    assert_eq!(active_clip_at(&project, V1, 600), Some(0));
    assert_eq!(active_clip_at(&project, V1, 601), None, "exclusive な終端");
    assert_eq!(active_clip_at(&project, V2, 0), None);

    trim_timeline_clip(
        &mut project,
        "fractional-id",
        TrimEdge::Left,
        3,
        LinkMode::Single,
    )
    .unwrap();
    assert_eq!(
        project.timeline_clips[0].source_in_frame, 1,
        "left trim を素材境界へ snap する"
    );
    let before_invalid = project.clone();
    assert!(
        trim_timeline_clip(
            &mut project,
            "fractional-id",
            TrimEdge::Right,
            -10000,
            LinkMode::Single
        )
        .is_err()
    );
    assert_eq!(project, before_invalid);

    let mut unrelated = clip_on("unrelated", TimelineClipKind::Video, V2);
    unrelated.timeline_start_frame = 77;
    unrelated.effects.opacity_percent = 42.0;
    project.timeline_clips.push(unrelated.clone());
    let right_start = project.timeline_clips[0].timeline_start_frame;
    trim_timeline_clip(
        &mut project,
        "fractional-id",
        TrimEdge::Right,
        -3,
        LinkMode::Single,
    )
    .unwrap();
    assert_eq!(project.timeline_clips[0].timeline_start_frame, right_start);
    assert_eq!(
        project.timeline_clips[1], unrelated,
        "trim で無関係 clip が変化しない"
    );

    let old_end = clip_end(&project, &project.timeline_clips[0]);
    trim_timeline_clip(
        &mut project,
        "fractional-id",
        TrimEdge::Left,
        4,
        LinkMode::Single,
    )
    .unwrap();
    assert_eq!(
        clip_end(&project, &project.timeline_clips[0]),
        old_end,
        "left trim が右端を保つ"
    );
    assert_eq!(project.timeline_clips[1], unrelated);
}

#[test]
fn placement_helpers() {
    let mut project = Project::default();
    let mut base = clip("base");
    base.timeline_start_frame = 120;
    project.timeline_clips.push(base);
    let mut added = clip("added");
    added.timeline_start_frame = 999;
    append_timeline_clip(&mut project, added, V1).unwrap();
    let last = project.timeline_clips.last().unwrap();
    assert_eq!(
        (last.track, last.timeline_start_frame),
        (V1, 420),
        "動画追加は V1 の最大 end へ"
    );

    let asset = ManimAsset {
        script_path: "scene.py".into(),
        scene_name: "Overlay".to_owned(),
        generated_video_path: "overlay.mp4".into(),
        generation_state: ManimGenerationState::Ready,
        source_fingerprint: String::new(),
    };
    append_manim_timeline_clip_at(
        &mut project,
        &asset,
        "overlay-id".to_owned(),
        fps(60, 1),
        120,
        180,
        V2,
    )
    .unwrap();
    let last = project.timeline_clips.last().unwrap();
    assert_eq!((last.track, last.timeline_start_frame), (V2, 180));
    assert!(
        append_manim_timeline_clip_at(
            &mut project,
            &asset,
            "overlay-id-2".to_owned(),
            fps(60, 1),
            120,
            400,
            V2
        )
        .is_err(),
        "同じ Manim asset の重複配置を拒否する"
    );

    let before = project.clone();
    assert!(move_clip(&mut project, "id-added", V1, 200).is_err());
    assert_eq!(project, before, "同じ track の重なりを拒否したら変えない");
    move_clip(&mut project, "id-added", V2, 420).unwrap();
    assert_eq!(
        (
            project.timeline_clips[1].track,
            project.timeline_clips[1].timeline_start_frame
        ),
        (V2, 420)
    );
    move_clip(&mut project, "id-added", V2, 500).unwrap();
    assert_eq!(project.timeline_clips[1].timeline_start_frame, 500);
    move_clip(&mut project, "id-added", V1, 420).unwrap();
    assert_eq!(project.timeline_clips[1].track, V1);
    assert!(
        move_clip(&mut project, "id-added", A1, 0).is_err(),
        "video clip を audio track へ移せない"
    );
}

#[test]
fn multiple_clip_move() {
    let starts = |project: &Project| -> Vec<(TrackRef, i64)> {
        project
            .timeline_clips
            .iter()
            .map(|clip| (clip.track, clip.timeline_start_frame))
            .collect()
    };
    {
        let mut project = Project::default();
        add_track(&mut project, TrackKind::Video).unwrap();
        let mut first = clip_on("multi-first", TimelineClipKind::Video, V1);
        let mut second = clip_on("multi-second", TimelineClipKind::Video, V2);
        first.timeline_start_frame = 100;
        second.timeline_start_frame = 500;
        let selection = vec![first.id.clone(), second.id.clone()];
        project.timeline_clips = vec![first.clone(), second];
        move_clips(
            &mut project,
            &selection,
            &first.id,
            V2,
            220,
            LinkMode::Linked,
            None,
        )
        .unwrap();
        assert_eq!(
            starts(&project),
            vec![(V2, 220), (V3, 620)],
            "時間・track 方向へ平行移動する"
        );

        let mut blocker = clip_on("multi-blocker", TimelineClipKind::Video, V2);
        blocker.timeline_start_frame = 600;
        project.timeline_clips.push(blocker);
        let before = project.clone();
        assert!(
            move_clips(
                &mut project,
                &selection,
                &first.id,
                V2,
                500,
                LinkMode::Linked,
                None
            )
            .is_err()
        );
        assert_eq!(project, before, "重なり拒否時に一部だけ変化しない");
        assert!(
            move_clips(
                &mut project,
                &selection[1..],
                &first.id,
                V1,
                300,
                LinkMode::Linked,
                None
            )
            .is_err(),
            "選択外の anchor を拒否する"
        );
        assert_eq!(project, before);
    }
    {
        let mut project = Project::default();
        let first = clip_on("track-limit-first", TimelineClipKind::Video, V1);
        let mut second = clip_on("track-limit-second", TimelineClipKind::Video, V2);
        second.timeline_start_frame = 400;
        let selection = vec![first.id.clone(), second.id.clone()];
        project.timeline_clips = vec![first.clone(), second];
        let before = project.clone();
        assert!(
            move_clips(
                &mut project,
                &selection,
                &first.id,
                V2,
                100,
                LinkMode::Linked,
                None
            )
            .is_err()
        );
        assert_eq!(project, before, "V3 が無い複数 track 移動は全体を拒否する");
    }
    {
        let mut project = Project::default();
        let mut video = clip_on("mixed-video", TimelineClipKind::Video, V1);
        let mut audio = audio_clip("mixed-audio");
        video.timeline_start_frame = 100;
        audio.timeline_start_frame = 500;
        let selection = vec![video.id.clone(), audio.id.clone()];
        project.timeline_clips = vec![video.clone(), audio];
        move_clips(
            &mut project,
            &selection,
            &video.id,
            V2,
            200,
            LinkMode::Linked,
            None,
        )
        .unwrap();
        assert_eq!(
            starts(&project),
            vec![(V2, 200), (A1, 600)],
            "audio は track を保って時間だけ動く"
        );
    }
    {
        let mut project = Project::default();
        let mut video = clip_on("linked-video", TimelineClipKind::Video, V1);
        let mut audio = audio_clip("linked-audio");
        video.timeline_start_frame = 100;
        audio.timeline_start_frame = 100;
        video.link_group_id = "move-link".to_owned();
        audio.link_group_id = "move-link".to_owned();
        project.timeline_clips = vec![video.clone(), audio.clone()];
        move_clips(
            &mut project,
            &[video.id.clone()],
            &video.id,
            V2,
            250,
            LinkMode::Linked,
            None,
        )
        .unwrap();
        assert_eq!(
            starts(&project),
            vec![(V2, 250), (A1, 250)],
            "リンク相手も同じ時間差で動く"
        );
        move_clips(
            &mut project,
            &[video.id.clone(), audio.id.clone()],
            &video.id,
            V1,
            300,
            LinkMode::Linked,
            None,
        )
        .unwrap();
        assert_eq!(
            starts(&project),
            vec![(V1, 300), (A1, 300)],
            "選択とリンク展開の重なりを二重に動かさない"
        );
    }
    {
        let mut project = Project::default();
        let mut anchor = clip_on("lower-bound-anchor", TimelineClipKind::Video, V1);
        let mut leftmost = clip_on("lower-bound-left", TimelineClipKind::Video, V2);
        anchor.timeline_start_frame = 100;
        leftmost.timeline_start_frame = 20;
        let selection = vec![anchor.id.clone(), leftmost.id.clone()];
        project.timeline_clips = vec![anchor.clone(), leftmost];
        move_clips(
            &mut project,
            &selection,
            &anchor.id,
            V1,
            50,
            LinkMode::Linked,
            None,
        )
        .unwrap();
        assert_eq!(
            starts(&project),
            vec![(V1, 80), (V2, 0)],
            "最左の clip を 0 秒へ snap する"
        );
    }
    {
        let mut project = Project::default();
        let mut video = clip_on("legacy-linked-video", TimelineClipKind::Video, V1);
        let mut audio = audio_clip("legacy-linked-audio");
        video.timeline_start_frame = 100;
        audio.timeline_start_frame = 100;
        video.link_group_id = "legacy-link".to_owned();
        audio.link_group_id = "legacy-link".to_owned();
        project.timeline_clips = vec![video.clone(), audio];
        move_clip(&mut project, &video.id, V2, 180).unwrap();
        assert_eq!(starts(&project), vec![(V2, 180), (A1, 180)]);
        move_clip(&mut project, &video.id, V1, 0).unwrap();
        assert_eq!(starts(&project), vec![(V1, 0), (A1, 0)]);
    }
}

// track を任意に増減できること。clip の載った track を暗黙に消さないこと。
#[test]
fn track_editing() {
    let mut project = Project::default();
    assert_eq!(
        (project.video_tracks.len(), project.audio_tracks.len()),
        (2, 1)
    );
    add_track(&mut project, TrackKind::Video).unwrap();
    assert_eq!(project.video_tracks.last().unwrap().name, "V3");
    add_track(&mut project, TrackKind::Audio).unwrap();
    assert_eq!(project.audio_tracks.last().unwrap().name, "A2");
    set_track_muted(&mut project, V2, true).unwrap();
    assert!(project.video_tracks[1].muted);

    // 目玉のドラッグ塗りの確定。範囲外の index が 1 つでもあれば何も変えない。
    let before = project.video_tracks.clone();
    assert!(set_tracks_muted(&mut project, TrackKind::Video, &[0, 99], true).is_err());
    assert_eq!(project.video_tracks, before);
    assert!(set_tracks_muted(&mut project, TrackKind::Video, &[], true).is_err());
    set_tracks_muted(&mut project, TrackKind::Video, &[0, 2], true).unwrap();
    assert!(project.video_tracks.iter().all(|track| track.muted));
    set_tracks_muted(&mut project, TrackKind::Video, &[0, 1, 2], false).unwrap();
    assert!(project.video_tracks.iter().all(|track| !track.muted));

    set_track_solo(&mut project, A2, true).unwrap();
    assert!(project.audio_tracks[1].solo && !project.audio_tracks[0].solo);
    assert!(!project.is_track_output_enabled(A1) && project.is_track_output_enabled(A2));
    assert!(
        project.is_track_output_enabled(V1),
        "video の出力は audio の solo に影響されない"
    );
    let before_solo = project.video_tracks.clone();
    assert!(set_track_solo(&mut project, V1, true).is_err());
    assert_eq!(project.video_tracks, before_solo);
    set_track_solo(&mut project, A2, false).unwrap();
    assert!(project.is_track_output_enabled(A1));
    set_track_muted(&mut project, V2, true).unwrap();

    // V3 に clip を置くと V3 は消せない。
    project
        .timeline_clips
        .push(clip_on("onTop", TimelineClipKind::Video, V3));
    validate_timeline(&project).unwrap();
    let before_remove = project.clone();
    assert!(remove_track(&mut project, V3).is_err());
    assert_eq!(project, before_remove);
    // 空の V2 を消すと V3 の clip が V2 へ繰り上がる。
    remove_track(&mut project, V2).unwrap();
    assert_eq!(project.video_tracks.len(), 2);
    assert_eq!(project.video_tracks[1].name, "V2");
    assert_eq!(project.timeline_clips.last().unwrap().track.index, 1);
    // video track は最低 1 本。
    let mut single = Project::default();
    single.video_tracks.truncate(1);
    assert!(remove_track(&mut single, V1).is_err());
}

// audio clip は audio track にだけ載る。逆も拒否する。
#[test]
fn audio_clip_placement() {
    let mut project = Project::default();
    append_timeline_clip(&mut project, audio_clip("voice"), A1).unwrap();
    assert_eq!(project.timeline_clips[0].track, A1);
    validate_timeline(&project).unwrap();
    assert!(append_timeline_clip(&mut project, audio_clip("misplaced"), V1).is_err());
    let mut broken = project.clone();
    broken.timeline_clips[0].track = V1;
    assert!(validate_timeline(&broken).is_err());
    assert_eq!(
        active_clips_at(&project, TrackKind::Audio, 10),
        vec![Some(0)]
    );
    assert_eq!(
        active_clips_at(&project, TrackKind::Video, 10),
        vec![None, None]
    );
}

// 空白の ripple delete。詰める対象が無い場所は成功にしない。
#[test]
fn ripple_delete() {
    let mut project = Project::default();
    let first = clip("first");
    let mut second = clip("second");
    second.timeline_start_frame = 500; // 300..500 が空白
    let mut third = clip("third");
    third.timeline_start_frame = 900; // 800..900 が空白
    let mut other = clip_on("other", TimelineClipKind::Video, V2);
    other.timeline_start_frame = 400;
    project.timeline_clips = vec![first, second, third, other];
    validate_timeline(&project).unwrap();
    assert_eq!(
        gap_at(&project, V1, 400).unwrap(),
        TimelineGap {
            start: 300,
            end: 500
        }
    );
    assert!(gap_at(&project, V1, 100).is_err(), "clip 上は gap ではない");
    assert!(
        gap_at(&project, V1, 1300).is_err(),
        "後続 clip が無い終端の空白は gap ではない"
    );

    ripple_delete_gap(&mut project, V1, 400).unwrap();
    let starts: Vec<i64> = project
        .timeline_clips
        .iter()
        .map(|clip| clip.timeline_start_frame)
        .collect();
    assert_eq!(&starts[..3], &[0, 300, 700]);
    assert_eq!(
        starts[3], 300,
        "全 track の時間削除で他 track の区間を切り詰める"
    );
    let before = project.clone();
    assert!(ripple_delete_gap(&mut project, V1, 100).is_err());
    assert_eq!(project, before);
}

#[test]
fn ripple_delete_with_linked_clips() {
    let mut video = clip("linked-video");
    video.timeline_start_frame = 100;
    video.link_group_id = "ripple-link".to_owned();
    let mut audio = audio_clip("linked-audio");
    audio.timeline_start_frame = 100;
    audio.link_group_id = "ripple-link".to_owned();
    let mut project = Project::default();
    project.timeline_clips = vec![video.clone(), audio.clone()];
    ripple_delete_gap(&mut project, V1, 50).unwrap();
    assert_eq!(project.timeline_clips[0].timeline_start_frame, 0);
    assert_eq!(project.timeline_clips[1].timeline_start_frame, 0);

    // 他 track の削除区間にある素材も除去する。
    let mut blocker = audio_clip("audio-blocker");
    blocker.source_out_frame = 60;
    let mut blocked = Project::default();
    blocked.timeline_clips = vec![video.clone(), audio.clone(), blocker];
    validate_timeline(&blocked).unwrap();
    ripple_delete_gap(&mut blocked, V1, 50).unwrap();
    assert_eq!(blocked.timeline_clips.len(), 2);
    assert!(
        blocked
            .timeline_clips
            .iter()
            .all(|clip| clip.timeline_start_frame == 0)
    );

    // unlink 後も全 track を同じだけ詰める。
    let mut unlinked = Project::default();
    unlinked.timeline_clips = vec![video.clone(), audio];
    unlink_timeline_clip(&mut unlinked, &video.id).unwrap();
    ripple_delete_gap(&mut unlinked, V1, 50).unwrap();
    assert!(
        unlinked
            .timeline_clips
            .iter()
            .all(|clip| clip.timeline_start_frame == 0)
    );
}

// レート調整。期待値は手で計算した値である。
#[test]
fn rate_stretch() {
    // 60fps 素材 300 frame を 0 に置き、次の clip は 600 から。
    let mut project = Project::default();
    let target = clip("rate");
    let mut next = clip("rate-next");
    next.timeline_start_frame = 600;
    project.timeline_clips = vec![target.clone(), next];
    validate_timeline(&project).unwrap();

    // right 端を +300: 尺 600 = 50%。素材範囲と開始位置は変えない。
    rate_stretch_timeline_clip(
        &mut project,
        &target.id,
        TrimEdge::Right,
        300,
        LinkMode::Single,
    )
    .unwrap();
    let slowed = &project.timeline_clips[0];
    assert_eq!(slowed.speed, fps(1, 2));
    assert_eq!(duration_at(&project, 0), 600);
    assert_eq!(
        (
            slowed.timeline_start_frame,
            slowed.source_in_frame,
            slowed.source_out_frame
        ),
        (0, 0, 300)
    );

    // 次の clip に接したので、これ以上は伸ばせない。
    let before_blocked = project.clone();
    assert_eq!(
        clamp_rate_edit(
            &project,
            &target.id,
            TrimEdge::Right,
            10000,
            LinkMode::Single
        ),
        Ok(0)
    );
    assert!(
        rate_stretch_timeline_clip(
            &mut project,
            &target.id,
            TrimEdge::Right,
            10000,
            LinkMode::Single
        )
        .is_err()
    );
    assert_eq!(project, before_blocked);

    // 縮める方向は 1000%（尺 30）で止まる。
    assert_eq!(
        clamp_rate_edit(
            &project,
            &target.id,
            TrimEdge::Right,
            -1000,
            LinkMode::Single
        ),
        Ok(-570)
    );
    rate_stretch_timeline_clip(
        &mut project,
        &target.id,
        TrimEdge::Right,
        -1000,
        LinkMode::Single,
    )
    .unwrap();
    assert_eq!(project.timeline_clips[0].speed, fps(10, 1));
    assert_eq!(duration_at(&project, 0), 30);

    // 10% は尺 3000。隣を遠ざけて確かめる。
    project.timeline_clips[1].timeline_start_frame = 10000;
    assert_eq!(
        clamp_rate_edit(
            &project,
            &target.id,
            TrimEdge::Right,
            100000,
            LinkMode::Single
        ),
        Ok(2970)
    );

    // left 端は終端を保って開始位置を動かす。timeline 先頭で止まる。
    let mut left = Project::default();
    let mut left_target = clip("rate-left");
    left_target.timeline_start_frame = 100;
    left.timeline_clips = vec![left_target.clone()];
    assert_eq!(
        clamp_rate_edit(
            &left,
            &left_target.id,
            TrimEdge::Left,
            -1000,
            LinkMode::Single
        ),
        Ok(-100)
    );
    rate_stretch_timeline_clip(
        &mut left,
        &left_target.id,
        TrimEdge::Left,
        -1000,
        LinkMode::Single,
    )
    .unwrap();
    assert_eq!(left.timeline_clips[0].timeline_start_frame, 0);
    assert_eq!(duration_at(&left, 0), 400);
    assert_eq!(left.timeline_clips[0].speed, fps(3, 4));

    // 29.97fps 素材 100 frame（60fps で尺 ceil(200.2) = 201）を尺 250 にする。
    // 速度 = 100 * 60 * 1001 / (30000 * 250) = 1001 / 1250。尺はちょうど 250 になる。
    let mut ntsc_project = Project::default();
    let mut ntsc = clip("rate-ntsc");
    ntsc.source_fps = fps(30000, 1001);
    ntsc.source_out_frame = 100;
    ntsc_project.timeline_clips = vec![ntsc.clone()];
    assert_eq!(duration_at(&ntsc_project, 0), 201);
    rate_stretch_timeline_clip(
        &mut ntsc_project,
        &ntsc.id,
        TrimEdge::Right,
        49,
        LinkMode::Single,
    )
    .unwrap();
    assert_eq!(ntsc_project.timeline_clips[0].speed, fps(1001, 1250));
    assert_eq!(duration_at(&ntsc_project, 0), 250);

    // key は内容に付いて伸縮する。端は端へ写る。150 → round(150 * 599 / 299) = 301。
    let mut key_project = Project::default();
    let mut keyed = clip("rate-keys");
    keyed.effects.opacity_keys = vec![key(0, 0.0), key(150, 50.0), key(299, 100.0)];
    key_project.timeline_clips = vec![keyed.clone()];
    rate_stretch_timeline_clip(
        &mut key_project,
        &keyed.id,
        TrimEdge::Right,
        300,
        LinkMode::Single,
    )
    .unwrap();
    let frames: Vec<(i64, f64)> = key_project.timeline_clips[0]
        .effects
        .opacity_keys
        .iter()
        .map(|key| (key.frame, key.value))
        .collect();
    assert_eq!(frames, vec![(0, 0.0), (301, 50.0), (599, 100.0)]);

    // 50% の clip の trim: timeline 100 frame = 素材 50 frame。
    trim_timeline_clip(
        &mut key_project,
        &keyed.id,
        TrimEdge::Right,
        -100,
        LinkMode::Single,
    )
    .unwrap();
    assert_eq!(key_project.timeline_clips[0].source_out_frame, 250);
    assert_eq!(duration_at(&key_project, 0), 500);
    // 分割した左右とも速度を引き継ぎ、尺の合計は変わらない。
    split_timeline_clips(
        &mut key_project,
        &[keyed.id.clone()],
        200,
        &mut sequential_ids(),
        LinkMode::Linked,
    )
    .unwrap();
    assert_eq!(key_project.timeline_clips.len(), 2);
    assert_eq!(key_project.timeline_clips[1].speed, fps(1, 2));
    assert_eq!(
        duration_at(&key_project, 0) + duration_at(&key_project, 1),
        500
    );

    // リンク相手にも同じ速度を適用する。相手の track の隣の clip でも止まる。
    let mut linked = Project::default();
    let mut video = clip("rate-video");
    let mut audio = audio_clip("rate-audio");
    video.link_group_id = "rate-pair".to_owned();
    audio.link_group_id = "rate-pair".to_owned();
    let mut audio_next = audio_clip("rate-audio-next");
    audio_next.timeline_start_frame = 450;
    linked.timeline_clips = vec![video.clone(), audio, audio_next];
    assert_eq!(
        clamp_rate_edit(&linked, &video.id, TrimEdge::Right, 300, LinkMode::Linked),
        Ok(150)
    );
    assert_eq!(
        clamp_rate_edit(&linked, &video.id, TrimEdge::Right, 300, LinkMode::Single),
        Ok(300)
    );
    rate_stretch_timeline_clip(
        &mut linked,
        &video.id,
        TrimEdge::Right,
        150,
        LinkMode::Linked,
    )
    .unwrap();
    assert_eq!(linked.timeline_clips[0].speed, fps(2, 3));
    assert_eq!(linked.timeline_clips[1].speed, fps(2, 3));
    assert_eq!(
        (duration_at(&linked, 0), duration_at(&linked, 1)),
        (450, 450)
    );
    // Alt で片方だけ変えた後は、Linked のレート調整を拒否する。
    linked.timeline_clips[2].timeline_start_frame = 5000;
    rate_stretch_timeline_clip(
        &mut linked,
        &video.id,
        TrimEdge::Right,
        -150,
        LinkMode::Single,
    )
    .unwrap();
    let before_mismatch = linked.clone();
    assert!(
        rate_stretch_timeline_clip(
            &mut linked,
            &video.id,
            TrimEdge::Right,
            100,
            LinkMode::Linked
        )
        .is_err()
    );
    assert_eq!(linked, before_mismatch);
}

// 尺の違うリンク相手のレート調整。相手は同じ速度で自分の尺になる。drag 中の表示と確定の結果が一致する。
#[test]
fn rate_stretch_linked_different_durations() {
    // V: 300f、A: 240f（素材 in 0..240）、どちらも 100%、0 から。
    let mut project = Project::default();
    let mut video = clip("rate-v");
    let mut audio = audio_clip("rate-a");
    audio.source_out_frame = 240;
    video.link_group_id = "rate-unequal".to_owned();
    audio.link_group_id = "rate-unequal".to_owned();
    project.timeline_clips = vec![video.clone(), audio.clone()];
    validate_timeline(&project).unwrap();

    // V を +150（450f）。速度 = 300 / 450 = 2/3。A は 240 × 3/2 = 360f。
    let preview =
        preview_rate_stretch(&project, &video.id, TrimEdge::Right, 150, LinkMode::Linked).unwrap();
    assert_eq!(preview.applied_delta, 150);
    assert_eq!(preview.clips.len(), 2);
    let shown = |id: &str| {
        preview
            .clips
            .iter()
            .find(|shown| shown.clip_id == id)
            .unwrap()
    };
    let (shown_video, shown_audio) = (shown(&video.id), shown(&audio.id));
    assert_eq!(
        (
            shown_video.duration_frames,
            shown_video.end_delta,
            shown_video.start_delta,
            shown_video.speed
        ),
        (450, 150, 0, fps(2, 3))
    );
    assert_eq!(
        (
            shown_audio.duration_frames,
            shown_audio.end_delta,
            shown_audio.start_delta,
            shown_audio.speed
        ),
        (360, 120, 0, fps(2, 3))
    );
    rate_stretch_timeline_clip(
        &mut project,
        &video.id,
        TrimEdge::Right,
        150,
        LinkMode::Linked,
    )
    .unwrap();
    assert_eq!(
        (duration_at(&project, 0), duration_at(&project, 1)),
        (450, 360)
    );
    assert_eq!(project.timeline_clips[1].speed, fps(2, 3));

    // left 端。終端を 600 に揃えた V（300..600）と A（360..600）の V を -150 する。
    // V は 150 から 450f、A は終端を保って 360f になり 240 から始まる（開始のずれ -120）。
    let mut left = Project::default();
    let mut left_video = clip("rate-left-v");
    let mut left_audio = audio_clip("rate-left-a");
    left_video.timeline_start_frame = 300;
    left_audio.source_in_frame = 60;
    left_audio.timeline_start_frame = 360;
    left_video.link_group_id = "rate-left-unequal".to_owned();
    left_audio.link_group_id = "rate-left-unequal".to_owned();
    left.timeline_clips = vec![left_video.clone(), left_audio.clone()];
    let left_preview = preview_rate_stretch(
        &left,
        &left_video.id,
        TrimEdge::Left,
        -150,
        LinkMode::Linked,
    )
    .unwrap();
    assert_eq!(left_preview.clips.len(), 2);
    for shown in &left_preview.clips {
        let expected = if shown.clip_id == left_video.id {
            (-150, 0, 450)
        } else {
            (-120, 0, 360)
        };
        assert_eq!(
            (shown.start_delta, shown.end_delta, shown.duration_frames),
            expected
        );
    }
    rate_stretch_timeline_clip(
        &mut left,
        &left_video.id,
        TrimEdge::Left,
        -150,
        LinkMode::Linked,
    )
    .unwrap();
    assert_eq!(left.timeline_clips[0].timeline_start_frame, 150);
    assert_eq!(left.timeline_clips[1].timeline_start_frame, 240);

    // 伸縮できない量は preview でも動かさない。
    let blocked = preview_rate_stretch(
        &project,
        &video.id,
        TrimEdge::Right,
        -100000,
        LinkMode::Linked,
    )
    .unwrap();
    assert!(blocked.applied_delta < 0);
    assert!(
        blocked
            .clips
            .iter()
            .all(|shown| shown.start_delta == 0 && shown.end_delta <= 0)
    );
}

#[test]
fn split_clips() {
    let mut project = Project::default();
    let mut video = clip("split-video");
    video.effects.fade_in_frames = 30;
    video.effects.fade_out_frames = 40;
    video.link_group_id = "split-link".to_owned();
    let mut audio = audio_clip("split-audio");
    audio.link_group_id = "split-link".to_owned();
    project.timeline_clips = vec![video.clone(), audio.clone()];
    validate_timeline(&project).unwrap();

    let before = project.clone();
    for frame in [0, 300] {
        assert!(
            split_timeline_clips(
                &mut project,
                &[video.id.clone()],
                frame,
                &mut sequential_ids(),
                LinkMode::Linked
            )
            .is_err()
        );
    }
    assert_eq!(project, before, "clip の端での分割を拒否する");

    split_timeline_clips(
        &mut project,
        &[video.id.clone()],
        100,
        &mut sequential_ids(),
        LinkMode::Linked,
    )
    .unwrap();
    assert_eq!(
        project.timeline_clips.len(),
        4,
        "linked clip を相手ごと分割する"
    );
    let left_video = find(&project, &video.id).unwrap();
    let left_audio = find(&project, &audio.id).unwrap();
    let right = |kind: TimelineClipKind| {
        project
            .timeline_clips
            .iter()
            .find(|value| {
                value.id != video.id
                    && value.id != audio.id
                    && (value.kind == TimelineClipKind::Audio) == (kind == TimelineClipKind::Audio)
            })
            .unwrap()
    };
    let (right_video, right_audio) = (
        right(TimelineClipKind::Video),
        right(TimelineClipKind::Audio),
    );
    assert_eq!(left_video.source_out_frame, 100);
    assert_eq!(
        (
            right_video.source_in_frame,
            right_video.source_out_frame,
            right_video.timeline_start_frame
        ),
        (100, 300, 100)
    );
    assert_eq!(clip_end(&project, left_video), 100);
    assert_eq!(
        (
            left_video.effects.fade_in_frames,
            left_video.effects.fade_out_frames
        ),
        (30, 0)
    );
    assert_eq!(
        (
            right_video.effects.fade_in_frames,
            right_video.effects.fade_out_frames
        ),
        (0, 40)
    );
    assert_eq!(left_video.link_group_id, "split-link");
    assert_eq!(left_audio.link_group_id, "split-link");
    assert!(!right_video.link_group_id.is_empty() && right_video.link_group_id != "split-link");
    assert_eq!(
        right_video.link_group_id, right_audio.link_group_id,
        "右半分が新しい link group で結ばれる"
    );

    // リンク相手が分割位置を含まなければ相手は切らず、右半分は未リンクになる。
    let mut partial = Project::default();
    let mut short_audio = audio.clone();
    short_audio.source_out_frame = 50;
    partial.timeline_clips = vec![video.clone(), short_audio.clone()];
    split_timeline_clips(
        &mut partial,
        &[video.id.clone()],
        100,
        &mut sequential_ids(),
        LinkMode::Linked,
    )
    .unwrap();
    assert_eq!(partial.timeline_clips.len(), 3);
    assert!(
        partial
            .timeline_clips
            .last()
            .unwrap()
            .link_group_id
            .is_empty()
    );
    assert_eq!(
        find(&partial, &short_audio.id).unwrap().source_out_frame,
        50
    );

    // 29.97fps 素材を 60fps timeline で切っても左右が重ならない。
    let mut fractional = Project::default();
    let mut ntsc = clip("ntsc");
    ntsc.source_fps = fps(30000, 1001);
    fractional.timeline_clips = vec![ntsc.clone()];
    split_timeline_clips(
        &mut fractional,
        &[ntsc.id.clone()],
        101,
        &mut sequential_ids(),
        LinkMode::Linked,
    )
    .unwrap();
    assert_eq!(fractional.timeline_clips.len(), 2);
    assert_eq!(
        fractional.timeline_clips[0].source_out_frame,
        fractional.timeline_clips[1].source_in_frame
    );
    assert_eq!(
        clip_end(&fractional, &fractional.timeline_clips[0]),
        fractional.timeline_clips[1].timeline_start_frame
    );

    let mut spanning = three_clips();
    let mut upper = clip_on("upper", TimelineClipKind::Video, V2);
    upper.timeline_start_frame = 200;
    spanning.timeline_clips.push(upper);
    assert_eq!(
        clip_ids_spanning_frame(&spanning, 350),
        ids(&["id-Manim", "id-upper"])
    );
    assert_eq!(
        clip_ids_spanning_frame(&spanning, 300),
        ids(&["id-upper"]),
        "clip 境界上は含めない"
    );
    // 選択で絞る版: among の順序を保ち、境界ちょうどの clip と存在しない ID を含めない。
    assert_eq!(
        clip_ids_spanning_frame_among(&spanning, 350, &ids(&["id-upper", "missing", "id-Manim"])),
        ids(&["id-upper", "id-Manim"])
    );
    assert_eq!(
        clip_ids_spanning_frame_among(&spanning, 300, &ids(&["id-Manim", "id-upper"])),
        ids(&["id-upper"])
    );
    assert!(clip_ids_spanning_frame_among(&spanning, 350, &[]).is_empty());
}

// [ / ] の音量。映像を選ぶとリンク相手の audio を変え、映像の音量は 100 のまま。
#[test]
fn step_clip_volume_changes_audio_only() {
    let mut project = Project::default();
    let mut video = clip("video");
    video.link_group_id = "volume-link".to_owned();
    let mut audio = audio_clip("audio");
    audio.link_group_id = "volume-link".to_owned();
    let mut keyed = audio_clip("keyed");
    keyed.timeline_start_frame = 300;
    keyed.effects.volume_keys = vec![key(0, 0.0), key(10, 100.0), key(20, 199.0)];
    let lone = clip_on("lone", TimelineClipKind::Video, V2);
    project.timeline_clips = vec![video.clone(), audio.clone(), keyed.clone(), lone.clone()];
    validate_timeline(&project).unwrap();
    let volume_of = |project: &Project, id: &str| find(project, id).unwrap().effects.volume_percent;
    const PLUS_ONE_DB: f64 = 112.20184543019634;

    step_clip_volume(&mut project, &[video.id.clone()], 1.0).unwrap();
    assert!((volume_of(&project, &audio.id) - PLUS_ONE_DB).abs() < 1e-9);
    assert_eq!(volume_of(&project, &video.id), 100.0);

    // key は 0% を保ち、他は同じ規則（上限 200）で変える。base も変える。
    step_clip_volume(&mut project, &[keyed.id.clone()], 1.0).unwrap();
    let keys = &find(&project, &keyed.id).unwrap().effects.volume_keys;
    assert_eq!(keys.len(), 3);
    assert_eq!(keys[0].value, 0.0);
    assert!((keys[1].value - PLUS_ONE_DB).abs() < 1e-9);
    assert_eq!(keys[2].value, 200.0);
    assert!((volume_of(&project, &keyed.id) - PLUS_ONE_DB).abs() < 1e-9);

    // 対象外だけ・上限で変化なし・存在しない ID は失敗し、Project を変えない。
    let before = project.clone();
    assert!(step_clip_volume(&mut project, std::slice::from_ref(&lone.id), 1.0).is_err());
    assert_eq!(project, before);
    let mut at_maximum = project.clone();
    for value in &mut at_maximum.timeline_clips {
        if value.id == audio.id {
            value.effects.volume_percent = 200.0;
        }
    }
    let maximum_before = at_maximum.clone();
    assert!(step_clip_volume(&mut at_maximum, &[audio.id.clone()], 1.0).is_err());
    assert_eq!(at_maximum, maximum_before);
    assert!(step_clip_volume(&mut project, &ids(&["missing"]), 1.0).is_err());
    assert_eq!(project, before);

    step_clip_volume(&mut project, &[audio.id.clone(), keyed.id.clone()], -1.0).unwrap();
    assert!((volume_of(&project, &audio.id) - 100.0).abs() < 1e-9);
}

fn transition(
    id: &str,
    outgoing: &str,
    incoming: &str,
    before: i64,
    after: i64,
) -> TimelineTransition {
    TimelineTransition {
        id: id.to_owned(),
        outgoing_clip_id: outgoing.to_owned(),
        incoming_clip_id: incoming.to_owned(),
        frames_before_cut: before,
        frames_after_cut: after,
        kind: TransitionKind::Blend,
    }
}

/// A [0,300) と B [300,600) は同じ 600 frame の素材の前半と後半で、cut の前後に 300 frame ずつ余白がある。
fn transition_project() -> Project {
    let mut project = Project::default();
    let mut a = clip("A");
    a.source_frame_count = 600;
    a.source_out_frame = 300;
    let mut b = clip("B");
    b.source_frame_count = 600;
    b.source_in_frame = 300;
    b.source_out_frame = 600;
    b.timeline_start_frame = 300;
    project.timeline_clips = vec![a, b];
    project.timeline_transitions = vec![transition("t1", "id-A", "id-B", 30, 30)];
    project
}

#[track_caller]
fn assert_fails_with(project: &Project, fragment: &str) {
    match validate_timeline(project) {
        Ok(_) => panic!("期待: {fragment} / 実際: (成功)"),
        Err(error) => assert!(
            error.message().contains(fragment),
            "期待: {fragment} / 実際: {error}"
        ),
    }
}

// 検証の負例。いずれも対照群から 1 か所だけ変える。
#[test]
fn transition_validation() {
    let base = transition_project();
    validate_timeline(&base).unwrap();
    let variant = |change: &dyn Fn(&mut Project)| {
        let mut project = base.clone();
        change(&mut project);
        project
    };
    assert_fails_with(
        &variant(&|p| {
            p.timeline_transitions
                .push(p.timeline_transitions[0].clone())
        }),
        "ID が空または重複",
    );
    assert_fails_with(
        &variant(&|p| p.timeline_transitions[0].incoming_clip_id = "missing".to_owned()),
        "clip がありません",
    );
    assert_fails_with(
        &variant(&|p| p.timeline_clips[1].track = V2),
        "同じ track にありません",
    );
    assert_fails_with(
        &variant(&|p| p.timeline_clips[1].timeline_start_frame = 301),
        "接していません",
    );
    assert_fails_with(
        &variant(&|p| {
            p.timeline_transitions[0].frames_before_cut = 0;
            p.timeline_transitions[0].frames_after_cut = 0;
        }),
        "長さが不正",
    );
    assert_fails_with(
        &variant(&|p| p.timeline_transitions[0].frames_before_cut = -1),
        "長さが不正",
    );
    assert_fails_with(
        &variant(&|p| p.timeline_transitions[0].frames_before_cut = 301),
        "余白が足りません",
    );
    assert_fails_with(
        &variant(&|p| p.timeline_transitions[0].frames_after_cut = 301),
        "余白が足りません",
    );
    // A を [0,20) にする（尻の余白は 300 のまま）。
    assert_fails_with(
        &variant(&|p| {
            p.timeline_clips[0].source_in_frame = 280;
            p.timeline_clips[1].timeline_start_frame = 20;
        }),
        "尺を超えています",
    );
    assert_fails_with(
        &variant(&|p| p.timeline_clips[0].effects.fade_out_frames = 10),
        "フェードを設定できません",
    );
    assert_fails_with(
        &variant(&|p| p.timeline_clips[1].effects.fade_in_frames = 10),
        "フェードを設定できません",
    );

    // B を 60 frame にし、前後のトランジションが B の内側で 40 + 40 = 80 frame 重なる。
    let mut overlap = base.clone();
    overlap.timeline_clips[1].source_out_frame = 360;
    let mut c = clip("C");
    c.source_frame_count = 600;
    c.source_in_frame = 300;
    c.source_out_frame = 600;
    c.timeline_start_frame = 360;
    overlap.timeline_clips.push(c);
    overlap.timeline_transitions = vec![
        transition("t1", "id-A", "id-B", 20, 40),
        transition("t2", "id-B", "id-C", 40, 20),
    ];
    assert_fails_with(&overlap, "前後のトランジションが重なって");
    overlap.timeline_transitions[1].frames_before_cut = 20;
    validate_timeline(&overlap).unwrap();
    overlap
        .timeline_transitions
        .push(transition("t3", "id-A", "id-C", 1, 0));
    assert_fails_with(&overlap, "接していません");
    *overlap.timeline_transitions.last_mut().unwrap() = transition("t3", "id-A", "id-B", 1, 1);
    assert_fails_with(&overlap, "複数のトランジション");
}

// クロスディゾルブは画面全体を覆う不透明な映像どうしでだけ置ける。
#[test]
fn dissolve_requires_opaque_full_frame_video() {
    let base = transition_project();
    let variant = |change: &dyn Fn(&mut Project)| {
        let mut project = base.clone();
        change(&mut project);
        project
    };
    let scaled = variant(&|p| p.timeline_clips[1].effects.scale_x_percent = 80.0);
    assert_fails_with(&scaled, "画面全体を覆う不透明");
    assert_fails_with(
        &variant(&|p| p.timeline_clips[0].effects.position_x_percent = 10.0),
        "画面全体を覆う不透明",
    );
    assert_fails_with(
        &variant(&|p| p.timeline_clips[1].effects.crop_left_percent = 5.0),
        "画面全体を覆う不透明",
    );
    assert_fails_with(
        &variant(&|p| p.timeline_clips[1].effects.opacity_percent = 50.0),
        "画面全体を覆う不透明",
    );
    // 区間（outgoing の最後の 30 frame）に掛かる不透明度 key は拒否し、掛からなければ許す。
    assert_fails_with(
        &variant(&|p| {
            p.timeline_clips[0].effects.opacity_keys = vec![key(0, 100.0), key(299, 90.0)]
        }),
        "画面全体を覆う不透明",
    );
    validate_timeline(&variant(&|p| {
        p.timeline_clips[0].effects.opacity_keys = vec![key(0, 50.0), key(100, 100.0)]
    }))
    .unwrap();
    // 音声のクロスフェードは映像の見た目に関係しないので制限しない。
    let audio = variant(&|p| {
        for value in &mut p.timeline_clips {
            value.kind = TimelineClipKind::Audio;
            value.track = A1;
            value.effects.volume_percent = 50.0;
        }
    });
    validate_timeline(&audio).unwrap();
    // 作るときも同じ理由で断る（余白不足と取り違えない）。
    let mut refused = scaled.clone();
    refused.timeline_transitions.clear();
    let error = apply_default_edit_transition(
        &mut refused,
        "id-A",
        "id-B",
        60,
        LinkMode::Linked,
        &mut sequential_ids(),
    )
    .unwrap_err();
    assert!(error.message().contains("画面全体を覆う不透明"), "{error}");
}

// フレーム保持 clip の端には置けない。
#[test]
fn transition_rejects_frame_hold() {
    let mut held = transition_project();
    held.timeline_transitions.clear();
    insert_frame_hold(&mut held, "id-A", 100, 30, &mut sequential_ids()).unwrap();
    let hold_id = held
        .timeline_clips
        .iter()
        .find(|value| value.frame_hold.is_some())
        .unwrap()
        .id
        .clone();
    held.timeline_transitions = vec![transition("t-hold", "id-A", &hold_id, 0, 1)];
    assert_fails_with(&held, "フレーム保持");
}

// 編集後の整合（reconcile）。
#[test]
fn transitions_follow_edits() {
    let base = transition_project();
    let edited = |edit: &dyn Fn(&mut Project)| {
        let mut project = base.clone();
        edit(&mut project);
        project.timeline_transitions
    };
    assert!(
        edited(&|p| {
            delete_timeline_clip(p, 1).unwrap();
        })
        .is_empty(),
        "clip の削除で消す"
    );
    assert!(
        edited(&|p| {
            move_clip(p, "id-B", V1, 400).unwrap();
        })
        .is_empty(),
        "離れたら消す"
    );
    assert_eq!(
        edited(&|p| {
            move_clips(
                p,
                &ids(&["id-A", "id-B"]),
                "id-A",
                V2,
                0,
                LinkMode::Linked,
                None,
            )
            .unwrap();
        }),
        base.timeline_transitions,
        "両 clip を一緒に動かしたら変わらない"
    );
    // roll で cut を +290 動かすと、A の尻の余白が 10 frame になる。
    let rolled = edited(&|p| {
        roll_timeline_edit(p, "id-A", TrimEdge::Right, 290, LinkMode::Linked).unwrap();
    });
    assert_eq!(rolled.len(), 1);
    assert_eq!(
        (rolled[0].frames_before_cut, rolled[0].frames_after_cut),
        (30, 10)
    );
    // A を分割すると、トランジションの outgoing は右半分になる。
    let split = edited(&|p| {
        split_timeline_clips(
            p,
            &ids(&["id-A"]),
            100,
            &mut sequential_ids(),
            LinkMode::Linked,
        )
        .unwrap();
    });
    assert_eq!(split.len(), 1);
    assert_eq!(
        (
            split[0].outgoing_clip_id.as_str(),
            split[0].incoming_clip_id.as_str()
        ),
        ("new-1", "id-B")
    );
    assert_eq!(split[0].frames_before_cut, 30);
    // トランジションの区間の中（cut + 10）で B を切ると、B の左半分は 10 frame になる。
    let inside = edited(&|p| {
        split_timeline_clips(
            p,
            &ids(&["id-B"]),
            310,
            &mut sequential_ids(),
            LinkMode::Linked,
        )
        .unwrap();
    });
    assert_eq!(inside.len(), 1);
    assert_eq!(inside[0].frames_after_cut, 10);
    // 60fps → 30fps で長さも秒位置で換算する（30 frame = 0.5 秒 → 15 frame）。
    let thirty = edited(&|p| {
        set_timeline_frame_rate(p, fps(30, 1)).unwrap();
    });
    assert_eq!(
        (thirty[0].frames_before_cut, thirty[0].frames_after_cut),
        (15, 15)
    );
    // reconcile は変える必要の無いトランジションを変えない。
    let mut untouched = base.clone();
    reconcile_timeline_transitions(&mut untouched);
    assert_eq!(untouched, base);
}

// Shift+E。1 つでも有効なら全部を無効に、全部が無効なら全部を有効にする。リンク相手も揃える。
#[test]
fn toggle_clips_enabled_aligns_selection() {
    let mut project = Project::default();
    let mut video = clip("video");
    video.link_group_id = "enabled-link".to_owned();
    let mut audio = audio_clip("audio");
    audio.link_group_id = "enabled-link".to_owned();
    let mut other = clip_on("other", TimelineClipKind::Video, V2);
    other.enabled = false;
    project.timeline_clips = vec![video.clone(), audio.clone(), other.clone()];
    let enabled = |project: &Project| -> Vec<bool> {
        project
            .timeline_clips
            .iter()
            .map(|clip| clip.enabled)
            .collect()
    };
    let selection = vec![video.id.clone(), other.id.clone()];
    toggle_clips_enabled(&mut project, &selection).unwrap();
    assert_eq!(enabled(&project), vec![false, false, false]);
    toggle_clips_enabled(&mut project, &selection).unwrap();
    assert_eq!(enabled(&project), vec![true, true, true]);
    let before = project.clone();
    assert!(toggle_clips_enabled(&mut project, &[]).is_err());
    assert!(toggle_clips_enabled(&mut project, &[video.id.clone(), "missing".to_owned()]).is_err());
    assert_eq!(project, before);
}

// Shift+D（clip 選択）。
#[test]
fn default_clip_fades() {
    assert_eq!(default_transition_frames(fps(60, 1)), 60);
    assert_eq!(default_transition_frames(fps(30000, 1001)), 30);
    assert_eq!(default_transition_frames(fps(24000, 1001)), 24);

    let mut project = Project::default();
    let mut video = clip("video"); // 60fps 素材、300 frame、等速
    video.link_group_id = "fade-link".to_owned();
    let mut audio = audio_clip("audio");
    audio.link_group_id = "fade-link".to_owned();
    let mut fast = clip_on("fast", TimelineClipKind::Video, V2); // 2 倍速: timeline 150 frame
    fast.speed = fps(2, 1);
    let mut short = clip_on("short", TimelineClipKind::Video, V2); // 50 frame
    short.source_out_frame = 50;
    short.timeline_start_frame = 200;
    project.timeline_clips = vec![video.clone(), audio.clone(), fast.clone(), short.clone()];
    validate_timeline(&project).unwrap();
    let fades = |project: &Project, id: &str| {
        let effects = &find(project, id).unwrap().effects;
        (effects.fade_in_frames, effects.fade_out_frames)
    };
    apply_default_clip_fades(&mut project, &[video.id.clone()], 60).unwrap();
    assert_eq!(
        (fades(&project, &video.id), fades(&project, &audio.id)),
        ((60, 60), (60, 60))
    );
    apply_default_clip_fades(&mut project, &[fast.id.clone()], 60).unwrap();
    assert_eq!(
        fades(&project, &fast.id),
        (120, 120),
        "2 倍速は素材 120 frame"
    );
    apply_default_clip_fades(&mut project, &[short.id.clone()], 60).unwrap();
    assert_eq!(
        fades(&project, &short.id),
        (25, 25),
        "尺が足りなければ半分ずつ"
    );
    let before = project.clone();
    assert!(apply_default_clip_fades(&mut project, &[video.id.clone()], 60).is_err());
    assert_eq!(project, before);

    // 1 frame の clip は先頭だけが取る。
    let mut single = Project::default();
    let mut one = clip("one");
    one.source_out_frame = 1;
    single.timeline_clips = vec![one.clone()];
    apply_default_clip_fades(&mut single, &[one.id.clone()], 60).unwrap();
    assert_eq!(fades(&single, &one.id), (1, 0));

    // トランジションのある端は変えない。
    let mut with_transition = transition_project();
    apply_default_clip_fades(&mut with_transition, &ids(&["id-A", "id-B"]), 60).unwrap();
    assert_eq!(fades(&with_transition, "id-A"), (60, 0));
    assert_eq!(fades(&with_transition, "id-B"), (0, 60));
    assert_eq!(with_transition.timeline_transitions.len(), 1);
}

// clip の端を指定位置へ動かす。30fps 素材は 60fps timeline で 2 frame 単位にしか動けない。
#[test]
fn clip_with_edge_at_requires_exact_boundary() {
    let project = Project::default();
    let mut half = clip("half");
    half.source_fps = fps(30, 1);
    half.source_frame_count = 100;
    half.source_out_frame = 50; // timeline [0, 100)
    let aligned = clip_with_edge_at(&project, &half, TrimEdge::Right, 102).unwrap();
    assert_eq!((aligned.source_in_frame, aligned.source_out_frame), (0, 51));
    assert!(
        clip_with_edge_at(&project, &half, TrimEdge::Right, 101).is_err(),
        "素材 frame へ一意に換算できない"
    );
    assert!(
        clip_with_edge_at(&project, &half, TrimEdge::Right, 202).is_err(),
        "素材の範囲を超える"
    );
}

fn half_rate_transition_project() -> Project {
    let mut project = transition_project();
    for value in &mut project.timeline_clips {
        value.source_fps = fps(30, 1);
        value.source_frame_count = 300;
    }
    project.timeline_clips[0].source_out_frame = 150;
    project.timeline_clips[1].source_in_frame = 150;
    project.timeline_clips[1].source_out_frame = 300;
    validate_timeline(&project).unwrap();
    project
}

/// V1 の A / B と、それぞれにリンクした A1 の音声。
fn linked_transition_project() -> Project {
    let mut linked = transition_project();
    linked.timeline_transitions.clear();
    let mut audio_a = linked.timeline_clips[0].clone();
    audio_a.id = "id-audio-A".to_owned();
    audio_a.kind = TimelineClipKind::Audio;
    audio_a.track = A1;
    let mut audio_b = linked.timeline_clips[1].clone();
    audio_b.id = "id-audio-B".to_owned();
    audio_b.kind = TimelineClipKind::Audio;
    audio_b.track = A1;
    linked.timeline_clips[0].link_group_id = "link-A".to_owned();
    audio_a.link_group_id = "link-A".to_owned();
    linked.timeline_clips[1].link_group_id = "link-B".to_owned();
    audio_b.link_group_id = "link-B".to_owned();
    linked.timeline_clips.extend([audio_a, audio_b]);
    validate_timeline(&linked).unwrap();
    linked
}

fn spans(project: &Project) -> Vec<(i64, i64)> {
    project
        .timeline_transitions
        .iter()
        .map(|transition| (transition.frames_before_cut, transition.frames_after_cut))
        .collect()
}

// Shift+D（編集点）。transition_project の A / B は cut の前後に 300 frame ずつ余白がある。
#[test]
fn default_edit_transition() {
    let mut base = transition_project();
    base.timeline_transitions.clear();
    assert_eq!(
        touching_clip_id(&base, "id-A", TrimEdge::Right).as_deref(),
        Some("id-B")
    );
    assert_eq!(
        touching_clip_id(&base, "id-B", TrimEdge::Left).as_deref(),
        Some("id-A")
    );
    assert_eq!(touching_clip_id(&base, "id-A", TrimEdge::Left), None);
    let place = |project: &mut Project, frames: i64| {
        apply_default_edit_transition(
            project,
            "id-A",
            "id-B",
            frames,
            LinkMode::Linked,
            &mut sequential_ids(),
        )
    };

    let mut centered = base.clone();
    centered.timeline_clips[0].effects.fade_out_frames = 10;
    let placed = place(&mut centered, 60).unwrap();
    assert_eq!((placed.frames, placed.transition_count), (60, 1));
    assert_eq!(centered.timeline_transitions[0].id, placed.transition_id);
    assert_eq!(spans(&centered), vec![(30, 30)]);
    assert_eq!(
        centered.timeline_clips[0].effects.fade_out_frames, 0,
        "端のフェードを消す"
    );
    place(&mut centered, 20).unwrap();
    assert_eq!(
        spans(&centered),
        vec![(10, 10)],
        "同じ編集点の既存のトランジションを置き換える"
    );

    // outgoing の尻の余白が 10 frame しか無ければ、残りを cut の前へ寄せる。
    let mut short_tail = base.clone();
    short_tail.timeline_clips[0].source_frame_count = 310;
    assert_eq!(place(&mut short_tail, 60).unwrap().frames, 60);
    assert_eq!(spans(&short_tail), vec![(50, 10)]);

    // 両側とも余白が無ければ作らない。
    let mut no_handles = base.clone();
    no_handles.timeline_clips[0].source_frame_count = 300;
    no_handles.timeline_clips[1].source_in_frame = 0;
    no_handles.timeline_clips[1].source_out_frame = 300;
    let before = no_handles.clone();
    assert!(
        place(&mut no_handles, 60)
            .unwrap_err()
            .message()
            .contains("余白が足りない")
    );
    assert_eq!(no_handles, before);
    let mut missing = base.clone();
    assert!(
        apply_default_edit_transition(
            &mut missing,
            "id-A",
            "missing",
            60,
            LinkMode::Linked,
            &mut sequential_ids()
        )
        .is_err()
    );

    // 30fps 素材は 60fps timeline で 2 frame 単位にしか延ばせない。61 frame は 60 frame に縮める。
    let mut half_rate = half_rate_transition_project();
    half_rate.timeline_transitions.clear();
    assert_eq!(place(&mut half_rate, 61).unwrap().frames, 60);
    assert_eq!(spans(&half_rate), vec![(30, 30)]);

    // 置く区間（30 / 30）だけが不透明なら置ける。outgoing は最後の 30 frame だけが不透明。
    let mut opaque_tail = base.clone();
    opaque_tail.timeline_clips[0].effects.opacity_keys = vec![key(269, 99.0), key(270, 100.0)];
    assert_eq!(place(&mut opaque_tail, 60).unwrap().frames, 60);
    assert_eq!(spans(&opaque_tail), vec![(30, 30)]);
    // 80 frame なら cut の前は不透明な 30 frame まで、残りを後ろへ寄せる。
    let mut opaque_longer = base.clone();
    opaque_longer.timeline_clips[0].effects.opacity_keys = vec![key(269, 99.0), key(270, 100.0)];
    assert_eq!(place(&mut opaque_longer, 80).unwrap().frames, 80);
    assert_eq!(spans(&opaque_longer), vec![(30, 50)]);

    // 素材 frame に乗る長さは cut の前後で別々に決まる。outgoing が 30fps（尻へは 2 frame 単位）、
    // incoming が 60fps（頭の余白 40）なら、61 frame は 31 / 30 で置ける。
    let mut mixed = base.clone();
    mixed.timeline_clips[0].source_fps = fps(30, 1);
    mixed.timeline_clips[0].source_frame_count = 300;
    mixed.timeline_clips[0].source_out_frame = 150;
    mixed.timeline_clips[1].source_frame_count = 600;
    mixed.timeline_clips[1].source_in_frame = 40;
    mixed.timeline_clips[1].source_out_frame = 340;
    validate_timeline(&mixed).unwrap();
    let mixed_placed = place(&mut mixed, 61).unwrap();
    assert_eq!(mixed_placed.frames, 61);
    assert_eq!(spans(&mixed), vec![(31, 30)]);
    assert_eq!(
        mixed_placed.transition_id, "new-1",
        "長さの探索で ID を使い捨てない"
    );

    // リンク相手（音声）も同じ cut で接していれば一緒に置く。
    let mut linked = linked_transition_project();
    let both = place(&mut linked, 60).unwrap();
    assert_eq!(
        (both.transition_count, linked.timeline_transitions.len()),
        (2, 2)
    );
    // 音声の尻の余白が 20 frame しか無くても、映像と音声は同じ長さ・同じ cut の前後で置く。
    let mut short_audio = linked.clone();
    short_audio.timeline_transitions.clear();
    for value in &mut short_audio.timeline_clips {
        if value.id == "id-audio-A" {
            value.source_frame_count = 320;
        }
    }
    let matched = place(&mut short_audio, 60).unwrap();
    assert_eq!((matched.frames, matched.transition_count), (60, 2));
    assert_eq!(spans(&short_audio), vec![(40, 20), (40, 20)]);
    let mut single = linked.clone();
    single.timeline_transitions.clear();
    let one = apply_default_edit_transition(
        &mut single,
        "id-A",
        "id-B",
        60,
        LinkMode::Single,
        &mut sequential_ids(),
    )
    .unwrap();
    assert_eq!(
        (one.transition_count, single.timeline_transitions.len()),
        (1, 1)
    );

    delete_timeline_transition(&mut linked, &both.transition_id).unwrap();
    assert_eq!(linked.timeline_transitions.len(), 1);
    let before_missing = linked.clone();
    assert!(delete_timeline_transition(&mut linked, "missing").is_err());
    assert_eq!(linked, before_missing);
}

#[track_caller]
fn assert_span_refused(project: &Project, id: &str, before: i64, after: i64, fragment: &str) {
    let mut changed = project.clone();
    match set_timeline_transition_span(&mut changed, id, before, after, LinkMode::Linked) {
        Ok(_) => panic!("期待: {fragment} / 実際: (成功)"),
        Err(error) => assert!(
            error.message().contains(fragment),
            "期待: {fragment} / 実際: {error}"
        ),
    }
    assert_eq!(changed, *project, "断ったら Project を変えない");
}

// 既存のトランジションの長さと配置を変える（エフェクトコントロール）。
#[test]
fn transition_span_editing() {
    let base = transition_project();
    assert_eq!(
        transition_span_limits(&base, "t1", LinkMode::Linked),
        Ok((300, 300))
    );
    assert!(transition_span_limits(&base, "missing", LinkMode::Linked).is_err());

    // cut で開始（前 0 / 後 40）。ID と数は変えない。
    let mut start_at_cut = base.clone();
    let started =
        set_timeline_transition_span(&mut start_at_cut, "t1", 0, 40, LinkMode::Linked).unwrap();
    assert_eq!(
        (
            started.frames,
            started.transition_id.as_str(),
            started.transition_count
        ),
        (40, "t1", 1)
    );
    assert_eq!(start_at_cut.timeline_transitions[0].id, "t1");
    assert_eq!(spans(&start_at_cut), vec![(0, 40)]);
    let mut at_limit = base.clone();
    set_timeline_transition_span(&mut at_limit, "t1", 300, 300, LinkMode::Linked).unwrap();
    assert_eq!(spans(&at_limit), vec![(300, 300)], "上限ちょうどは置ける");

    assert_span_refused(&base, "t1", 30, 30, "変わっていません");
    assert_span_refused(&base, "t1", 0, 0, "1 フレーム以上");
    assert_span_refused(&base, "t1", -1, 40, "1 フレーム以上");
    assert_span_refused(&base, "t1", 301, 0, "余白が足りません");
    assert_span_refused(&base, "t1", 0, 301, "余白が足りません");
    assert_span_refused(&base, "missing", 10, 10, "ありません");

    // B の尻に B→C のトランジションが B の内側 100 frame を使っていれば、t1 の後ろは 200 まで。
    let mut chained = base.clone();
    let mut c = chained.timeline_clips[1].clone();
    c.id = "id-C".to_owned();
    c.name = "C".to_owned();
    c.timeline_start_frame = 600;
    chained.timeline_clips.push(c);
    chained
        .timeline_transitions
        .push(transition("t2", "id-B", "id-C", 100, 0));
    validate_timeline(&chained).unwrap();
    assert_eq!(
        transition_span_limits(&chained, "t1", LinkMode::Linked),
        Ok((300, 200))
    );
    assert_span_refused(&chained, "t1", 0, 201, "余白が足りません");
    set_timeline_transition_span(&mut chained, "t1", 0, 200, LinkMode::Linked).unwrap();

    // outgoing は最後の 30 frame だけが不透明。前 31 は不透明度で断り、前 30 は置ける。
    let mut opaque_tail = base.clone();
    opaque_tail.timeline_clips[0].effects.opacity_keys = vec![key(269, 99.0), key(270, 100.0)];
    validate_timeline(&opaque_tail).unwrap();
    assert_span_refused(&opaque_tail, "t1", 31, 0, "区間で不透明度が下がっています");
    set_timeline_transition_span(&mut opaque_tail.clone(), "t1", 30, 0, LinkMode::Linked).unwrap();

    // 30fps 素材は 60fps timeline で 2 frame 単位。31 は黙って 30 へ丸めず断る。
    let half_rate = half_rate_transition_project();
    assert_span_refused(&half_rate, "t1", 31, 30, "フレームに合わない");
    set_timeline_transition_span(&mut half_rate.clone(), "t1", 32, 30, LinkMode::Linked).unwrap();

    // 数値欄・ドラッグの値は、置ける最も近い長さへ吸着させる（前後は独立）。同じ距離なら今の値から離れる側。
    let nearest = |project: &Project, before, after, mode| {
        nearest_transition_span(project, "t1", before, after, mode, LinkMode::Linked)
            .map(|fit| (fit.frames_before_cut, fit.frames_after_cut))
    };
    assert_eq!(
        nearest(&half_rate, 31, 30, SpanFitMode::EachSide),
        Ok((32, 30))
    );
    assert_eq!(
        nearest(&half_rate, 29, 33, SpanFitMode::EachSide),
        Ok((28, 34))
    );
    // 不透明な区間（前 30 まで）と余白（後 300 まで）の内側へ止める。
    assert_eq!(
        nearest(&opaque_tail, 40, 400, SpanFitMode::EachSide),
        Ok((30, 300))
    );
    assert!(nearest(&base, 0, 0, SpanFitMode::EachSide).is_err());
    assert!(
        nearest_transition_span(
            &base,
            "missing",
            10,
            10,
            SpanFitMode::KeepTotal,
            LinkMode::Linked
        )
        .is_err()
    );
    // 総尺を保つ。outgoing は最後の 30 frame だけが不透明なので、長さ 100 の中央（50 / 50）は 30 / 70 で置ける。
    assert_eq!(
        nearest(&opaque_tail, 50, 50, SpanFitMode::KeepTotal),
        Ok((30, 70))
    );
    assert_eq!(
        nearest(&opaque_tail, 50, 50, SpanFitMode::EachSide),
        Ok((30, 50))
    );
    // 30fps 素材で本体を右へ 3 ずらす（33 / 27）と、長さ 60 のまま 34 / 26。
    assert_eq!(
        nearest(&half_rate, 33, 27, SpanFitMode::KeepTotal),
        Ok((34, 26))
    );
    // 長さ 61 は前後とも偶数なので置けない。最も近い総尺のうち増やした向きの 62（32 / 30）。
    assert_eq!(
        nearest(&half_rate, 31, 30, SpanFitMode::KeepTotal),
        Ok((32, 30))
    );
}

#[test]
fn transitions_and_edge_trims() {
    let base = transition_project();
    // clip の端を trim して cut で接しなくなれば、トランジションは消える。
    let mut trimmed_tail = base.clone();
    trim_timeline_clip(
        &mut trimmed_tail,
        "id-A",
        TrimEdge::Right,
        -10,
        LinkMode::Linked,
    )
    .unwrap();
    assert!(trimmed_tail.timeline_transitions.is_empty());
    // 接している隣の clip の方へは延ばさない（止める）。
    let mut touching = base.clone();
    assert_eq!(
        clamp_edge_edit(
            &touching,
            "id-B",
            TrimEdge::Left,
            EdgeEditKind::Trim,
            -10,
            LinkMode::Linked
        ),
        Ok(0)
    );
    assert_eq!(
        clamp_edge_edit(
            &touching,
            "id-A",
            TrimEdge::Right,
            EdgeEditKind::Trim,
            10,
            LinkMode::Linked
        ),
        Ok(0)
    );
    assert!(
        trim_timeline_clip(&mut touching, "id-B", TrimEdge::Left, -10, LinkMode::Linked).is_err()
    );
    assert_eq!(touching, base);
    // 縮める向きと、リップル・ローリングは止めない。
    assert_eq!(
        clamp_edge_edit(
            &touching,
            "id-B",
            TrimEdge::Left,
            EdgeEditKind::Trim,
            10,
            LinkMode::Linked
        ),
        Ok(10)
    );
    assert_eq!(
        clamp_edge_edit(
            &touching,
            "id-A",
            TrimEdge::Right,
            EdgeEditKind::Roll,
            10,
            LinkMode::Linked
        ),
        Ok(10)
    );
    let mut trimmed_head = base.clone();
    trim_timeline_clip(
        &mut trimmed_head,
        "id-B",
        TrimEdge::Left,
        10,
        LinkMode::Linked,
    )
    .unwrap();
    assert!(trimmed_head.timeline_transitions.is_empty());
}

// リンク相手（音声）の既存のトランジションも同じ値にする。Single なら本体だけ。
#[test]
fn transition_span_follows_link() {
    let mut linked = linked_transition_project();
    apply_default_edit_transition(
        &mut linked,
        "id-A",
        "id-B",
        60,
        LinkMode::Linked,
        &mut sequential_ids(),
    )
    .unwrap();
    assert_eq!(linked.timeline_transitions.len(), 2);
    let video_id = linked.timeline_transitions[0].id.clone();
    let audio_id = linked.timeline_transitions[1].id.clone();
    let mut single = linked.clone();
    let both =
        set_timeline_transition_span(&mut linked, &video_id, 10, 50, LinkMode::Linked).unwrap();
    assert_eq!(both.transition_count, 2);
    assert_eq!(linked.timeline_transitions[0].id, video_id);
    assert_eq!(linked.timeline_transitions[1].id, audio_id);
    assert_eq!(spans(&linked), vec![(10, 50), (10, 50)]);
    let one =
        set_timeline_transition_span(&mut single, &video_id, 10, 50, LinkMode::Single).unwrap();
    assert_eq!(one.transition_count, 1);
    assert_eq!(spans(&single), vec![(10, 50), (30, 30)]);
    // 相手の編集点にトランジションが無ければ作らない。
    let mut video_only = linked.clone();
    video_only
        .timeline_transitions
        .retain(|transition| transition.id != audio_id);
    set_timeline_transition_span(&mut video_only, &video_id, 20, 20, LinkMode::Linked).unwrap();
    assert_eq!(video_only.timeline_transitions.len(), 1);
}

// 長尺素材（2 時間、60fps）の吸着。A は cut の 100 frame 手前の 1 frame だけ 99% なので、cut の前に置けるのは
// 最後の 99 frame まで。時間ではなく、不透明度と素材 frame を調べた回数を見る（候補ごとに区間を辿り直す実装なら
// 余白（216000 frame）の数倍になる）。
#[test]
fn transition_span_fit_on_long_media() {
    let mut project = Project::default();
    let mut a = clip("A");
    a.source_frame_count = 432_000;
    a.source_out_frame = 216_000;
    a.effects.opacity_keys = vec![key(215_899, 100.0), key(215_900, 99.0), key(215_901, 100.0)];
    let mut b = clip("B");
    b.source_frame_count = 432_000;
    b.source_in_frame = 216_000;
    b.source_out_frame = 432_000;
    b.timeline_start_frame = 216_000;
    project.timeline_clips = vec![a, b];
    project.timeline_transitions = vec![transition("t1", "id-A", "id-B", 30, 30)];
    validate_timeline(&project).unwrap();
    let fitted = nearest_transition_span(
        &project,
        "t1",
        216_000,
        30,
        SpanFitMode::EachSide,
        LinkMode::Linked,
    )
    .unwrap();
    // 総尺を保つ吸着は incoming の不透明な範囲（216000）も数える。216030 は 99 / 215931。
    let kept = nearest_transition_span(
        &project,
        "t1",
        216_000,
        30,
        SpanFitMode::KeepTotal,
        LinkMode::Linked,
    )
    .unwrap();
    assert_eq!(
        (fitted.frames_before_cut, fitted.frames_after_cut),
        (99, 30)
    );
    assert_eq!(
        (kept.frames_before_cut, kept.frames_after_cut),
        (99, 215_931)
    );
    // 不透明な長さは cut の前後で 1 回ずつ数える（前は 100 回、後は余白の上限まで 216000 回）。
    const OPACITY_PROBES: u64 = 100 + 216_000;
    const EDGE_PROBE_LIMIT: u64 = 2 * (216_000 + 1);
    assert_eq!(
        (fitted.opacity_probes, kept.opacity_probes),
        (OPACITY_PROBES, OPACITY_PROBES)
    );
    assert!(fitted.edge_probes <= EDGE_PROBE_LIMIT && kept.edge_probes <= EDGE_PROBE_LIMIT);
}

fn span_of(project: &Project, id: &str) -> Option<(i64, i64)> {
    find(project, id).map(|clip| (clip.timeline_start_frame, clip_end(project, clip)))
}

// 上書き移動。V1 の long [0, 300) の上へ V2 の mover（60 frame）を動かす。
#[test]
fn move_overwrite() {
    let base = || {
        let mut project = Project::default();
        let mut mover = clip_on("mover", TimelineClipKind::Video, V2);
        mover.source_out_frame = 60;
        project.timeline_clips = vec![clip("long"), mover];
        project
    };
    let overwrite = |project: &mut Project, start: i64| {
        move_clips(
            project,
            &ids(&["id-mover"]),
            "id-mover",
            V1,
            start,
            LinkMode::Linked,
            Some(&mut sequential_ids()),
        )
    };
    let mut inside = base();
    overwrite(&mut inside, 100).unwrap();
    assert_eq!(span_of(&inside, "id-long"), Some((0, 100)));
    assert_eq!(span_of(&inside, "id-mover"), Some((100, 160)));
    assert_eq!(
        span_of(&inside, "new-1"),
        Some((160, 300)),
        "clip の中へ置くと前後に分ける"
    );
    assert_eq!(find(&inside, "new-1").unwrap().source_in_frame, 160);

    let mut tail = base();
    overwrite(&mut tail, 250).unwrap();
    assert_eq!(span_of(&tail, "id-long"), Some((0, 250)));
    assert_eq!(tail.timeline_clips.len(), 2);

    let mut head = base();
    overwrite(&mut head, 0).unwrap();
    assert_eq!(span_of(&head, "id-long"), Some((60, 300)));
    assert_eq!(find(&head, "id-long").unwrap().source_in_frame, 60);

    // 丸ごと覆った clip は消し、リンク相手は未リンクにする。
    let mut covered = base();
    let mut tiny = clip("tiny");
    tiny.source_out_frame = 30;
    tiny.timeline_start_frame = 400;
    tiny.link_group_id = "tiny-link".to_owned();
    let mut tiny_audio = audio_clip("tiny-audio");
    tiny_audio.source_out_frame = 30;
    tiny_audio.timeline_start_frame = 400;
    tiny_audio.link_group_id = "tiny-link".to_owned();
    covered.timeline_clips.extend([tiny, tiny_audio]);
    validate_timeline(&covered).unwrap();
    overwrite(&mut covered, 390).unwrap();
    assert!(find(&covered, "id-tiny").is_none());
    assert!(
        find(&covered, "id-tiny-audio")
            .unwrap()
            .link_group_id
            .is_empty()
    );

    // 上書きを指定しなければ重なりを拒否する。
    let mut rejected = base();
    let before = rejected.clone();
    assert!(
        move_clips(
            &mut rejected,
            &ids(&["id-mover"]),
            "id-mover",
            V1,
            100,
            LinkMode::Linked,
            None
        )
        .is_err()
    );
    assert_eq!(rejected, before);
}

#[test]
fn ripple_trim() {
    let mut project = Project::default();
    let first = clip("first");
    let mut second = clip("second");
    second.timeline_start_frame = 300;
    second.link_group_id = "ripple-trim-link".to_owned();
    let mut second_audio = audio_clip("second-audio");
    second_audio.timeline_start_frame = 300;
    second_audio.link_group_id = "ripple-trim-link".to_owned();
    let mut third = clip("third");
    third.timeline_start_frame = 700;
    let mut other = clip_on("other", TimelineClipKind::Video, V2);
    other.timeline_start_frame = 350;
    project.timeline_clips = vec![
        first.clone(),
        second.clone(),
        second_audio.clone(),
        third.clone(),
        other.clone(),
    ];
    validate_timeline(&project).unwrap();
    let start = |project: &Project, id: &str| find(project, id).unwrap().timeline_start_frame;

    ripple_trim_timeline_clip(
        &mut project,
        &first.id,
        TrimEdge::Right,
        -100,
        LinkMode::Single,
    )
    .unwrap();
    assert_eq!(clip_end(&project, find(&project, &first.id).unwrap()), 200);
    assert_eq!(
        (
            start(&project, &second.id),
            start(&project, &second_audio.id),
            start(&project, &third.id),
            start(&project, &other.id)
        ),
        (200, 200, 600, 250),
        "ripple trim で全 track を詰める"
    );

    ripple_trim_timeline_clip(
        &mut project,
        &second.id,
        TrimEdge::Left,
        50,
        LinkMode::Single,
    )
    .unwrap();
    let trimmed = find(&project, &second.id).unwrap();
    assert_eq!(
        (trimmed.timeline_start_frame, trimmed.source_in_frame),
        (200, 50)
    );
    assert_eq!(start(&project, &third.id), 550);
    let audio = find(&project, &second_audio.id).unwrap();
    assert_eq!(
        (audio.timeline_start_frame, audio.source_in_frame),
        (200, 0),
        "Single はリンク相手を trim しない"
    );

    // 素材の範囲を越えるドラッグは失敗させず、素材の端で止める。
    ripple_trim_timeline_clip(
        &mut project,
        &first.id,
        TrimEdge::Right,
        200,
        LinkMode::Single,
    )
    .unwrap();
    assert_eq!(find(&project, &first.id).unwrap().source_out_frame, 300);
    assert_eq!(
        (start(&project, &second.id), start(&project, &third.id)),
        (300, 650)
    );
    let before = project.clone();
    assert!(
        ripple_trim_timeline_clip(
            &mut project,
            &first.id,
            TrimEdge::Right,
            1,
            LinkMode::Single
        )
        .is_err()
    );
    assert_eq!(project, before);
}

/// 素材 600 frame のうち [in, in + 300) を start へ置いた clip。
fn roomy_clip(
    name: &str,
    kind: TimelineClipKind,
    track: TrackRef,
    source_in: i64,
    start: i64,
) -> TimelineClip {
    let mut value = clip_on(name, kind, track);
    value.source_frame_count = 600;
    value.source_in_frame = source_in;
    value.source_out_frame = source_in + 300;
    value.timeline_start_frame = start;
    value
}

fn roomy(name: &str, source_in: i64, start: i64) -> TimelineClip {
    roomy_clip(name, TimelineClipKind::Video, V1, source_in, start)
}

#[test]
fn roll_edit() {
    let mut project = Project::default();
    let outgoing = roomy("outgoing", 0, 0);
    let incoming = roomy("incoming", 100, 300);
    project.timeline_clips = vec![outgoing.clone(), incoming.clone()];
    validate_timeline(&project).unwrap();

    roll_timeline_edit(
        &mut project,
        &outgoing.id,
        TrimEdge::Right,
        50,
        LinkMode::Single,
    )
    .unwrap();
    assert_eq!(clip_end(&project, &project.timeline_clips[0]), 350);
    assert_eq!(
        (
            project.timeline_clips[1].timeline_start_frame,
            project.timeline_clips[1].source_in_frame
        ),
        (350, 150)
    );
    assert_eq!(clip_end(&project, &project.timeline_clips[1]), 600);
    roll_timeline_edit(
        &mut project,
        &incoming.id,
        TrimEdge::Left,
        -20,
        LinkMode::Single,
    )
    .unwrap();
    assert_eq!(clip_end(&project, &project.timeline_clips[0]), 330);
    assert_eq!(project.timeline_clips[1].timeline_start_frame, 330);

    let before = project.clone();
    assert!(
        roll_timeline_edit(
            &mut project,
            &outgoing.id,
            TrimEdge::Left,
            10,
            LinkMode::Single
        )
        .is_err()
    );
    assert_eq!(project, before, "隣接 clip が無い rolling を拒否する");
    // incoming を消すほどのドラッグは、incoming を 1 frame 残すところで止める。
    roll_timeline_edit(
        &mut project,
        &outgoing.id,
        TrimEdge::Right,
        400,
        LinkMode::Single,
    )
    .unwrap();
    assert_eq!(clip_end(&project, &project.timeline_clips[0]), 599);
    assert_eq!(project.timeline_clips[1].timeline_start_frame, 599);
    assert_eq!(clip_end(&project, &project.timeline_clips[1]), 600);
    let before_minimum = project.clone();
    assert!(
        roll_timeline_edit(
            &mut project,
            &outgoing.id,
            TrimEdge::Right,
            1,
            LinkMode::Single
        )
        .is_err()
    );
    assert_eq!(project, before_minimum);

    // 29.97fps の outgoing は 60fps timeline の 602 frame 目に境界を置けない。隙間を作らず拒否する。
    let mut fractional = Project::default();
    let mut ntsc = roomy("ntsc-outgoing", 0, 0);
    ntsc.source_fps = fps(30000, 1001);
    fractional.timeline_clips = vec![ntsc.clone(), roomy("after-ntsc", 100, 601)];
    validate_timeline(&fractional).unwrap();
    assert_eq!(clip_end(&fractional, &fractional.timeline_clips[0]), 601);
    let before_fractional = fractional.clone();
    assert!(
        roll_timeline_edit(
            &mut fractional,
            &ntsc.id,
            TrimEdge::Right,
            1,
            LinkMode::Single
        )
        .is_err()
    );
    assert_eq!(fractional, before_fractional);
}

/// V1 / A1 に [前 | リンク対 | 後] を並べる。リンク対は素材 600 frame の [100, 400) を 300 へ置く。
fn linked_tool_fixture() -> Project {
    let mut video = roomy_clip("pair-video", TimelineClipKind::Video, V1, 100, 300);
    let mut audio = roomy_clip("pair-audio", TimelineClipKind::Audio, A1, 100, 300);
    video.link_group_id = "pair".to_owned();
    audio.link_group_id = "pair".to_owned();
    let mut project = Project::default();
    project.timeline_clips = vec![
        roomy_clip("before-video", TimelineClipKind::Video, V1, 0, 0),
        roomy_clip("before-audio", TimelineClipKind::Audio, A1, 0, 0),
        video,
        audio,
        roomy_clip("after-video", TimelineClipKind::Video, V1, 100, 600),
        roomy_clip("after-audio", TimelineClipKind::Audio, A1, 100, 600),
    ];
    project
}

// 既定（Linked）はリンク相手にも同じ編集を適用し、Single（Alt）は操作した clip だけを編集する。
#[test]
fn linked_tool_editing() {
    let base = linked_tool_fixture();
    validate_timeline(&base).unwrap();
    let at = |project: &Project, name: &str| find(project, &format!("id-{name}")).unwrap().clone();
    let edited = |edit: &dyn Fn(&mut Project)| {
        let mut project = base.clone();
        edit(&mut project);
        project
    };

    let p = edited(&|p| {
        trim_timeline_clip(p, "id-pair-video", TrimEdge::Right, -50, LinkMode::Linked).unwrap();
    });
    assert_eq!(at(&p, "pair-audio").source_out_frame, 350);
    let p = edited(&|p| {
        trim_timeline_clip(p, "id-pair-video", TrimEdge::Right, -50, LinkMode::Single).unwrap();
    });
    assert_eq!(
        (
            at(&p, "pair-video").source_out_frame,
            at(&p, "pair-audio").source_out_frame
        ),
        (350, 400)
    );

    let p = edited(&|p| {
        ripple_trim_timeline_clip(p, "id-pair-video", TrimEdge::Right, -50, LinkMode::Linked)
            .unwrap();
    });
    assert_eq!(at(&p, "pair-audio").source_out_frame, 350);
    assert_eq!(
        (
            at(&p, "after-video").timeline_start_frame,
            at(&p, "after-audio").timeline_start_frame
        ),
        (550, 550)
    );
    // Single でも全 track の時間を削除する。
    let p = edited(&|p| {
        ripple_trim_timeline_clip(p, "id-pair-video", TrimEdge::Right, -50, LinkMode::Single)
            .unwrap();
    });
    assert_eq!(at(&p, "pair-audio").source_out_frame, 350);
    assert_eq!(
        (
            at(&p, "after-video").timeline_start_frame,
            at(&p, "after-audio").timeline_start_frame
        ),
        (550, 550)
    );

    let p = edited(&|p| {
        roll_timeline_edit(p, "id-pair-video", TrimEdge::Right, 20, LinkMode::Linked).unwrap();
    });
    assert_eq!(at(&p, "pair-audio").source_out_frame, 420);
    assert_eq!(
        (
            at(&p, "after-audio").timeline_start_frame,
            at(&p, "after-video").timeline_start_frame
        ),
        (620, 620)
    );
    let p = edited(&|p| {
        roll_timeline_edit(p, "id-pair-video", TrimEdge::Right, 20, LinkMode::Single).unwrap();
    });
    assert_eq!(
        (
            at(&p, "pair-audio").source_out_frame,
            at(&p, "after-audio").timeline_start_frame
        ),
        (400, 600)
    );
    // L カット: リンク相手が編集点を持たなければ、相手はそのまま残す。
    let p = edited(&|p| {
        p.timeline_clips
            .retain(|value| value.id != "id-after-audio");
        roll_timeline_edit(p, "id-pair-video", TrimEdge::Right, 20, LinkMode::Linked).unwrap();
    });
    assert_eq!(
        (
            at(&p, "pair-video").source_out_frame,
            at(&p, "pair-audio").source_out_frame
        ),
        (420, 400)
    );

    let p = edited(&|p| {
        slip_timeline_clip(p, "id-pair-video", 30, LinkMode::Linked).unwrap();
    });
    assert_eq!(
        (
            at(&p, "pair-video").source_in_frame,
            at(&p, "pair-audio").source_in_frame
        ),
        (130, 130)
    );
    // 全員がずらせる範囲で止め、リンク相手と同期を崩さない。
    let mut p = edited(&|p| {
        for value in &mut p.timeline_clips {
            if value.id == "id-pair-audio" {
                value.source_frame_count = 420;
            }
        }
        slip_timeline_clip(p, "id-pair-video", 30, LinkMode::Linked).unwrap();
    });
    assert_eq!(
        (
            at(&p, "pair-video").source_in_frame,
            at(&p, "pair-audio").source_in_frame
        ),
        (120, 120)
    );
    slip_timeline_clip(&mut p, "id-pair-video", 30, LinkMode::Single).unwrap();
    assert_eq!(
        (
            at(&p, "pair-video").source_in_frame,
            at(&p, "pair-audio").source_in_frame
        ),
        (150, 120)
    );

    let p = edited(&|p| {
        slide_timeline_clip(p, "id-pair-video", 20, LinkMode::Linked).unwrap();
    });
    assert_eq!(at(&p, "pair-audio").timeline_start_frame, 320);
    assert_eq!(clip_end(&p, &at(&p, "before-audio")), 320);
    // リンク相手に接している clip が無い（L / J カット）場合、相手は空白の分だけしか動けない。
    let p = edited(&|p| {
        p.timeline_clips
            .retain(|value| value.id != "id-before-audio" && value.id != "id-after-audio");
        let mut blocker = audio_clip("audio-blocker");
        blocker.source_out_frame = 30;
        blocker.timeline_start_frame = 250; // pair-audio（300 開始）との間に 20 frame の空白
        p.timeline_clips.push(blocker);
    });
    validate_timeline(&p).unwrap();
    assert_eq!(
        clamp_slide_edit(&p, "id-pair-video", -1000, LinkMode::Linked),
        Ok(-20)
    );
    let p = edited(&|p| {
        slide_timeline_clip(p, "id-pair-video", 20, LinkMode::Single).unwrap();
    });
    assert_eq!(
        (
            at(&p, "pair-video").timeline_start_frame,
            at(&p, "pair-audio").timeline_start_frame
        ),
        (320, 300)
    );

    let p = edited(&|p| {
        split_timeline_clips(
            p,
            &ids(&["id-pair-video"]),
            400,
            &mut sequential_ids(),
            LinkMode::Single,
        )
        .unwrap();
    });
    assert_eq!(p.timeline_clips.len(), base.timeline_clips.len() + 1);
    assert!(p.timeline_clips.last().unwrap().link_group_id.is_empty());
    assert_eq!(at(&p, "pair-audio").source_out_frame, 400);

    let p = edited(&|p| {
        move_clips(
            p,
            &ids(&["id-pair-video"]),
            "id-pair-video",
            V2,
            900,
            LinkMode::Single,
            None,
        )
        .unwrap();
    });
    assert_eq!(
        (
            at(&p, "pair-video").timeline_start_frame,
            at(&p, "pair-audio").timeline_start_frame
        ),
        (900, 300)
    );
    let p = edited(&|p| {
        move_clips(
            p,
            &ids(&["id-pair-video"]),
            "id-pair-video",
            V2,
            900,
            LinkMode::Linked,
            None,
        )
        .unwrap();
    });
    assert_eq!(at(&p, "pair-audio").timeline_start_frame, 900);
}

// レーザーで分割し前半を削除した後、右の clip の left 端を大きく引き延ばすと、分割前の clip まで戻り、
// それを越える分は止まる。
#[test]
fn trim_restores_split_clip() {
    let mut project = Project::default();
    // 60fps timeline に 30fps 素材（音声付き）を置く。
    let mut video = clip("restore-video");
    let mut audio = audio_clip("restore-audio");
    for value in [&mut video, &mut audio] {
        value.source_fps = fps(30, 1);
        value.source_frame_count = 300;
        value.source_out_frame = 300;
        value.link_group_id = "restore".to_owned();
    }
    project.timeline_clips = vec![video.clone(), audio];
    validate_timeline(&project).unwrap();
    split_timeline_clips(
        &mut project,
        &[video.id.clone()],
        241,
        &mut sequential_ids(),
        LinkMode::Linked,
    )
    .unwrap();
    assert_eq!(project.timeline_clips.len(), 4);
    let left = project.clip_index(&video.id).unwrap();
    delete_timeline_clip(&mut project, left).unwrap();
    assert_eq!(project.timeline_clips.len(), 2);
    let right_id = project
        .timeline_clips
        .iter()
        .find(|value| value.kind != TimelineClipKind::Audio)
        .unwrap()
        .id
        .clone();
    trim_timeline_clip(
        &mut project,
        &right_id,
        TrimEdge::Left,
        -100_000,
        LinkMode::Linked,
    )
    .unwrap();
    for value in &project.timeline_clips {
        assert_eq!(
            (
                value.source_in_frame,
                value.source_out_frame,
                value.timeline_start_frame
            ),
            (0, 300, 0)
        );
        assert_eq!(clip_end(&project, value), 600);
    }

    // 通常の trim は timeline 先頭より前へ出さない。リップルは開始位置を保つので制約を受けない。
    let mut offset = Project::default();
    offset.timeline_clips = vec![roomy("offset", 200, 50)];
    assert_eq!(
        clamp_edge_edit(
            &offset,
            "id-offset",
            TrimEdge::Left,
            EdgeEditKind::Trim,
            -1000,
            LinkMode::Single
        ),
        Ok(-50)
    );
    assert_eq!(
        clamp_edge_edit(
            &offset,
            "id-offset",
            TrimEdge::Left,
            EdgeEditKind::Ripple,
            -1000,
            LinkMode::Single
        ),
        Ok(-200)
    );
    assert_eq!(
        clamp_edge_edit(
            &offset,
            "id-offset",
            TrimEdge::Right,
            EdgeEditKind::Trim,
            -1000,
            LinkMode::Single
        ),
        Ok(-299)
    );
}

#[test]
fn slip_clip() {
    let mut project = Project::default();
    project.timeline_clips = vec![roomy("slip", 100, 50)];
    let range = |project: &Project| {
        let clip = &project.timeline_clips[0];
        (
            clip.source_in_frame,
            clip.source_out_frame,
            clip.timeline_start_frame,
        )
    };
    slip_timeline_clip(&mut project, "id-slip", 30, LinkMode::Single).unwrap();
    assert_eq!(range(&project), (130, 430, 50));
    slip_timeline_clip(&mut project, "id-slip", 1000, LinkMode::Single).unwrap();
    assert_eq!(range(&project), (300, 600, 50), "素材末尾で止める");
    let before = project.clone();
    assert!(slip_timeline_clip(&mut project, "id-slip", 5, LinkMode::Single).is_err());
    assert_eq!(project, before);
    slip_timeline_clip(&mut project, "id-slip", -1000, LinkMode::Single).unwrap();
    assert_eq!(range(&project), (0, 300, 50), "素材先頭で止める");
}

#[test]
fn slide_clip() {
    let mut project = Project::default();
    project.timeline_clips = vec![
        roomy("before", 0, 0),
        roomy("slide", 100, 300),
        roomy("after", 100, 600),
    ];
    validate_timeline(&project).unwrap();
    slide_timeline_clip(&mut project, "id-slide", 50, LinkMode::Linked).unwrap();
    assert_eq!(
        (
            project.timeline_clips[1].timeline_start_frame,
            project.timeline_clips[1].source_in_frame
        ),
        (350, 100)
    );
    assert_eq!(clip_end(&project, &project.timeline_clips[0]), 350);
    assert_eq!(
        (
            project.timeline_clips[2].timeline_start_frame,
            project.timeline_clips[2].source_in_frame
        ),
        (650, 150)
    );
    assert_eq!(clip_end(&project, &project.timeline_clips[2]), 900);

    // +50 のスライド後は before（素材 [0, 350)）の out が [-349, +250]、after（素材 [150, 400)）の in が [-150, +249]。
    assert_eq!(
        clamp_slide_edit(&project, "id-slide", 1000, LinkMode::Linked),
        Ok(249)
    );
    assert_eq!(
        clamp_slide_edit(&project, "id-slide", -1000, LinkMode::Linked),
        Ok(-150)
    );

    // 範囲を越えるドラッグは失敗させず、後ろの clip の素材の先頭で止める。
    slide_timeline_clip(&mut project, "id-slide", -400, LinkMode::Linked).unwrap();
    assert_eq!(project.timeline_clips[1].timeline_start_frame, 200);
    assert_eq!(clip_end(&project, &project.timeline_clips[0]), 200);
    assert_eq!(
        (
            project.timeline_clips[2].timeline_start_frame,
            project.timeline_clips[2].source_in_frame
        ),
        (500, 0)
    );
    let before = project.clone();
    assert!(slide_timeline_clip(&mut project, "id-slide", -1, LinkMode::Linked).is_err());
    assert_eq!(project, before);

    // 前の clip が素材の末尾まで使い切っていれば、右へはスライドできない。
    let mut exhausted = Project::default();
    exhausted.timeline_clips = vec![
        roomy("before", 300, 0),
        roomy("slide", 100, 300),
        roomy("after", 100, 600),
    ];
    let before_exhausted = exhausted.clone();
    assert!(slide_timeline_clip(&mut exhausted, "id-slide", 50, LinkMode::Linked).is_err());
    assert_eq!(exhausted, before_exhausted);
    assert_eq!(
        clamp_slide_edit(&exhausted, "id-slide", 50, LinkMode::Linked),
        Ok(0)
    );
    slide_timeline_clip(&mut exhausted, "id-slide", -50, LinkMode::Linked).unwrap();

    // 前後どちらかに接している clip が無ければ、単なる移動にせず拒否する。
    for neighbor in [roomy("after", 100, 600), roomy("before", 0, 0)] {
        let mut one_sided = Project::default();
        one_sided.timeline_clips = vec![roomy("slide", 100, 300), neighbor];
        let before = one_sided.clone();
        assert!(slide_timeline_clip(&mut one_sided, "id-slide", 20, LinkMode::Linked).is_err());
        assert_eq!(one_sided, before);
    }
    let mut alone = Project::default();
    alone.timeline_clips = vec![roomy("slide", 100, 300)];
    assert!(slide_timeline_clip(&mut alone, "id-slide", 20, LinkMode::Linked).is_err());
}

#[test]
fn track_select_from_frame() {
    let mut project = three_clips();
    let mut upper = clip_on("upper", TimelineClipKind::Video, V2);
    upper.timeline_start_frame = 100;
    project.timeline_clips.push(upper);
    assert_eq!(
        clip_ids_from_frame(&project, 350, SelectDirection::Forward, None),
        ids(&["id-Manim", "id-B", "id-upper"])
    );
    assert_eq!(
        clip_ids_from_frame(&project, 350, SelectDirection::Backward, None),
        ids(&["id-A", "id-Manim", "id-upper"])
    );
    assert_eq!(
        clip_ids_from_frame(&project, 350, SelectDirection::Forward, Some(V1)),
        ids(&["id-Manim", "id-B"])
    );
    assert_eq!(
        clip_ids_from_frame(&project, 300, SelectDirection::Forward, Some(V1)),
        ids(&["id-Manim", "id-B"])
    );
}

#[test]
fn validation_failures() {
    let invalid = |change: &dyn Fn(&mut Project)| {
        let mut project = three_clips();
        change(&mut project);
        validate_timeline(&project)
    };
    assert!(
        invalid(&|p| p.timeline_clips[1].id = p.timeline_clips[0].id.clone()).is_err(),
        "重複 clip ID"
    );
    assert!(
        invalid(&|p| p.timeline_clips[0].source_fps = fps(0, 1)).is_err(),
        "不正な素材 fps"
    );
    assert!(
        invalid(&|p| p.timeline_clips[0].source_out_frame = p.timeline_clips[0].source_in_frame)
            .is_err()
    );
    assert!(
        invalid(&|p| p.timeline_clips[0].track = TrackRef::video(7)).is_err(),
        "存在しない track"
    );
    assert!(
        invalid(&|p| p.video_tracks.clear()).is_err(),
        "video track が 0 本"
    );
    let gap = invalid(&|p| {
        p.timeline_clips[1].timeline_start_frame = 400;
        p.timeline_clips[2].timeline_start_frame = 800;
    })
    .unwrap();
    assert_eq!(gap.total_frames, 1100, "timeline の空白は受理する");
}

#[test]
fn delete_selection() {
    let mut middle = three_clips();
    assert_eq!(
        delete_timeline_clip(&mut middle, 1),
        Ok(Some(1)),
        "中央の削除後は右隣を選ぶ"
    );
    assert_eq!(middle.timeline_clips.len(), 2);
    assert_eq!(middle.timeline_clips[1].name, "B");
    assert_eq!(delete_timeline_clip(&mut three_clips(), 0), Ok(Some(0)));
    assert_eq!(
        delete_timeline_clip(&mut three_clips(), 2),
        Ok(Some(1)),
        "末尾の削除後は左隣を選ぶ"
    );
    let mut only = Project::default();
    only.timeline_clips.push(clip("only"));
    assert_eq!(delete_timeline_clip(&mut only, 0), Ok(None));
    assert!(only.timeline_clips.is_empty());
    let before = middle.clone();
    assert!(delete_timeline_clip(&mut middle, 9).is_err());
    assert_eq!(middle, before);
}

#[test]
fn linked_clip_editing() {
    let mut project = Project::default();
    let mut video = clip("linked-video");
    let mut audio = audio_clip("linked-audio");
    video.link_group_id = "link-1".to_owned();
    audio.link_group_id = "link-1".to_owned();
    project.timeline_clips = vec![video.clone(), audio.clone()];
    move_clip(&mut project, &video.id, V2, 120).unwrap();
    assert_eq!(
        (
            project.timeline_clips[0].track,
            project.timeline_clips[0].timeline_start_frame
        ),
        (V2, 120)
    );
    assert_eq!(
        (
            project.timeline_clips[1].track,
            project.timeline_clips[1].timeline_start_frame
        ),
        (A1, 120)
    );

    let mut blocked = project.clone();
    let mut blocker = audio_clip("audio-blocker");
    blocker.timeline_start_frame = 500;
    blocked.timeline_clips.push(blocker);
    let before_blocked = blocked.clone();
    assert!(
        move_clip(&mut blocked, &video.id, V1, 300).is_err(),
        "リンク先が衝突する移動を拒否する"
    );
    assert_eq!(blocked, before_blocked);

    unlink_timeline_clip(&mut project, &video.id).unwrap();
    assert!(
        project
            .timeline_clips
            .iter()
            .all(|clip| clip.link_group_id.is_empty())
    );
    assert!(
        unlink_timeline_clip(&mut project, &video.id).is_err(),
        "未リンク clip のリンク解除は失敗"
    );

    video.link_group_id = "link-2".to_owned();
    audio.link_group_id = "link-2".to_owned();
    project.timeline_clips = vec![video.clone(), audio.clone()];
    delete_timeline_clip(&mut project, 0).unwrap();
    assert!(
        project.timeline_clips.is_empty(),
        "リンク clip は両方を削除する"
    );

    let mut malformed = Project::default();
    let mut first = clip("same-kind-1");
    let mut second = clip_on("same-kind-2", TimelineClipKind::Video, V2);
    first.link_group_id = "bad-link".to_owned();
    second.link_group_id = "bad-link".to_owned();
    malformed.timeline_clips = vec![first, second];
    assert!(
        validate_timeline(&malformed).is_err(),
        "同種 clip 同士のリンクを拒否する"
    );
    let mut orphan = Project::default();
    video.link_group_id = "orphan-link".to_owned();
    orphan.timeline_clips = vec![video.clone()];
    assert!(
        validate_timeline(&orphan).is_err(),
        "相手の無いリンクを拒否する"
    );

    let mut placed_pair = Project::default();
    video.link_group_id = "atomic-link".to_owned();
    audio.link_group_id = "atomic-link".to_owned();
    assert_eq!(
        place_linked_av_pair_at(&mut placed_pair, video.clone(), V1, audio.clone(), A1, 42),
        Ok(Some(0))
    );
    assert!(
        placed_pair
            .timeline_clips
            .iter()
            .all(|clip| clip.timeline_start_frame == 42)
    );
    let mut blocked_pair = placed_pair.clone();
    video.id = "blocked-video".to_owned();
    audio.id = "blocked-audio".to_owned();
    video.link_group_id = "blocked-link".to_owned();
    audio.link_group_id = "blocked-link".to_owned();
    assert!(place_linked_av_pair_at(&mut blocked_pair, video, V1, audio, A1, 42).is_err());
    assert_eq!(blocked_pair, placed_pair, "衝突したら片側だけを確定しない");
}

#[test]
fn manim_placement() {
    let mut project = Project::default();
    project.timeline_clips.push(clip("A"));
    project.manim_assets.push(ManimAsset {
        script_path: "scene.py".into(),
        scene_name: "Scene".to_owned(),
        generated_video_path: "scene.mp4".into(),
        generation_state: ManimGenerationState::Ready,
        source_fingerprint: String::new(),
    });
    let end = timeline_track_end_frame(&project, V1).unwrap();
    assert_eq!(end, 300);
    let asset = project.manim_assets[0].clone();
    assert_eq!(
        append_manim_timeline_clip_at(
            &mut project,
            &asset,
            "id-manim".to_owned(),
            fps(60, 1),
            300,
            end,
            V1
        ),
        Ok(Some(1))
    );
    assert_eq!(project.timeline_clips[1].kind, TimelineClipKind::Manim);
    assert!(
        append_manim_timeline_clip_at(
            &mut project,
            &asset,
            "id-manim-2".to_owned(),
            fps(60, 1),
            300,
            600,
            V1
        )
        .is_err()
    );
    delete_timeline_clip(&mut project, 1).unwrap();
    assert_eq!(
        project.manim_assets.len(),
        1,
        "Manim clip の削除で asset は消えない"
    );
}

#[test]
fn clip_key_editing() {
    use mvm_project::clip_effects::ClipKeyKind;
    let mut project = Project::default();
    let mut video = clip("key-video");
    video.source_frame_count = 120;
    video.source_out_frame = 100;
    project.timeline_clips.push(video.clone());
    let before = project.clone();
    assert!(
        preview_clip_key_edit(
            &project,
            &video.id,
            ClipKeyKind::Opacity,
            Some(22),
            40,
            50.0
        )
        .is_err()
    );
    let preview =
        preview_clip_key_edit(&project, &video.id, ClipKeyKind::Opacity, None, 20, 80.0).unwrap();
    assert_eq!(preview.frame, 20);
    assert_eq!(preview.effects.opacity_keys.len(), 1);
    assert_eq!(project, before, "候補の計算は Project を変えない");
    edit_clip_key(
        &mut project,
        &video.id,
        ClipKeyKind::Opacity,
        None,
        20,
        80.0,
    )
    .unwrap();
    assert_eq!(
        project.timeline_clips[0].effects, preview.effects,
        "候補と確定が同じ"
    );
    edit_clip_key(
        &mut project,
        &video.id,
        ClipKeyKind::Opacity,
        None,
        80,
        20.0,
    )
    .unwrap();
    let keys = project.timeline_clips[0].effects.opacity_keys.clone();
    assert!(
        (evaluate_clip_keys(&keys, 100.0, 50) - 50.0).abs() < 1e-9,
        "線形補間"
    );
    assert_eq!(
        (
            evaluate_clip_keys(&keys, 100.0, 0),
            evaluate_clip_keys(&keys, 100.0, 99)
        ),
        (80.0, 20.0),
        "端の保持"
    );
    let clamped = preview_clip_key_edit(
        &project,
        &video.id,
        ClipKeyKind::Opacity,
        Some(20),
        99,
        150.0,
    )
    .unwrap();
    assert_eq!(clamped.frame, 79, "隣の key の手前で止める");
    assert_eq!(clamped.effects.opacity_keys[0].value, 100.0, "値域で止める");

    let mut removed = project.clone();
    let unchanged = removed.clone();
    assert!(delete_clip_key(&mut removed, &video.id, ClipKeyKind::Opacity, 21).is_err());
    assert!(
        delete_clip_key(&mut removed, &video.id, ClipKeyKind::Volume, 20).is_err(),
        "種別違い"
    );
    assert_eq!(removed, unchanged);
    delete_clip_key(&mut removed, &video.id, ClipKeyKind::Opacity, 20).unwrap();
    let frames: Vec<i64> = removed.timeline_clips[0]
        .effects
        .opacity_keys
        .iter()
        .map(|key| key.frame)
        .collect();
    assert_eq!(frames, vec![80]);
    delete_clip_key(&mut removed, &video.id, ClipKeyKind::Opacity, 80).unwrap();
    assert!(removed.timeline_clips[0].effects.opacity_keys.is_empty());

    let original = project.clone();
    trim_timeline_clip(
        &mut project,
        &video.id,
        TrimEdge::Left,
        10,
        LinkMode::Single,
    )
    .unwrap();
    let keys = &project.timeline_clips[0].effects.opacity_keys;
    assert_eq!(keys[0].frame, 0);
    assert!((keys[0].value - 80.0).abs() < 1e-9);
    assert_eq!(
        (keys[2].frame, keys.last().unwrap().frame),
        (70, 89),
        "trim 後の見えるカーブと端を保つ"
    );

    let mut divided = original.clone();
    split_timeline_clips(
        &mut divided,
        &[video.id.clone()],
        50,
        &mut prefixed_ids("key-new"),
        LinkMode::Single,
    )
    .unwrap();
    assert_eq!(divided.timeline_clips.len(), 2);
    assert!(
        (evaluate_clip_keys(&divided.timeline_clips[0].effects.opacity_keys, 100.0, 49) - 51.0)
            .abs()
            < 1e-9
    );
    assert!(
        (evaluate_clip_keys(&divided.timeline_clips[1].effects.opacity_keys, 100.0, 0) - 50.0)
            .abs()
            < 1e-9
    );
    let right_keys = divided.timeline_clips[1].effects.opacity_keys.clone();
    let right_id = divided.timeline_clips[1].id.clone();
    slip_timeline_clip(&mut divided, &right_id, 10, LinkMode::Single).unwrap();
    assert_eq!(
        divided.timeline_clips[1].effects.opacity_keys, right_keys,
        "slip は key を動かさない"
    );

    let mut linked = original.clone();
    let mut audio = audio_clip("key-audio");
    audio.source_frame_count = 120;
    audio.source_out_frame = 100;
    audio.link_group_id = "key-pair".to_owned();
    linked.timeline_clips[0].link_group_id = "key-pair".to_owned();
    linked.timeline_clips.push(audio);
    edit_clip_key(&mut linked, &video.id, ClipKeyKind::Opacity, None, 50, 40.0).unwrap();
    assert!(
        linked.timeline_clips[1].effects.volume_keys.is_empty(),
        "リンク相手にペン操作を複製しない"
    );
    trim_timeline_clip(&mut linked, &video.id, TrimEdge::Left, 10, LinkMode::Linked).unwrap();
    assert_eq!(linked.timeline_clips[1].source_in_frame, 10);

    let invalid = |change: &dyn Fn(&mut TimelineClip)| {
        let mut project = original.clone();
        change(&mut project.timeline_clips[0]);
        validate_timeline(&project)
    };
    assert!(
        invalid(&|clip| clip.effects.opacity_keys.push(key(20, 30.0))).is_err(),
        "重複 key"
    );
    assert!(
        invalid(&|clip| clip.effects.opacity_keys[0].value = f64::NAN).is_err(),
        "非有限 key"
    );
    assert!(
        invalid(&|clip| clip.effects.opacity_keys[0].frame = 100).is_err(),
        "clip 外の key"
    );
    assert!(
        invalid(&|clip| clip.effects.volume_keys = vec![key(5, 50.0)]).is_err(),
        "映像 clip の音量 key"
    );
}

#[test]
fn timeline_marks() {
    let mut project = Project::default();
    project.timeline_markers = vec![5, 15, 40];
    project.in_frame = Some(5);
    project.out_frame = Some(40);
    validate_timeline(&project).unwrap();
    let invalid = |change: &dyn Fn(&mut Project)| {
        let mut changed = project.clone();
        change(&mut changed);
        validate_timeline(&changed)
    };
    assert!(
        invalid(&|p| p.timeline_markers = vec![5, 5]).is_err(),
        "重複マーカー"
    );
    assert!(
        invalid(&|p| p.timeline_markers = vec![15, 5]).is_err(),
        "昇順でないマーカー"
    );
    assert!(
        invalid(&|p| p.in_frame = Some(40)).is_err(),
        "空のイン・アウト"
    );
    set_project_video_settings(&mut project, 1920, 1080, fps(30, 1)).unwrap();
    assert_eq!(project.timeline_markers, vec![3, 8, 20]);
    assert_eq!((project.in_frame, project.out_frame), (Some(3), Some(20)));
    // clip が無くても保存位置は保ち、再生尺には加えない。
    assert_eq!(validate_timeline(&project).unwrap().total_frames, 0);
}

// ドロップ位置への配置。行の種別に合う側をその track へ置き、重なりは上書きせずに空いている track へ回す。
#[test]
fn place_media_at_drop_cases() {
    let placed = |project: &Project, id: &str| {
        let clip = find(project, id).unwrap();
        (clip.track, clip.timeline_start_frame)
    };
    let pair = || {
        let mut video = clip("drop");
        let mut audio = clip_on("drop-audio", TimelineClipKind::Audio, V1);
        video.link_group_id = "drop-link".to_owned();
        audio.link_group_id = "drop-link".to_owned();
        (video, audio)
    };
    // 映像行（V2）へのリンク対。音声は空いている A1 へ入る。
    let mut project = Project::default();
    let (video, audio) = pair();
    place_media_at_drop(&mut project, video, Some(audio), V2, 120).unwrap();
    assert_eq!(
        (
            placed(&project, "id-drop"),
            placed(&project, "id-drop-audio")
        ),
        ((V2, 120), (A1, 120))
    );
    // A1 が使用中なら、リンク音声は新しい A2 へ入る。
    let mut project = Project::default();
    project.timeline_clips = vec![audio_clip("busy")];
    let (video, audio) = pair();
    place_media_at_drop(&mut project, video, Some(audio), V1, 100).unwrap();
    assert_eq!(project.audio_tracks.len(), 2);
    assert_eq!(placed(&project, "id-drop-audio").0, A2);
    // 音声行へのリンク対。音声がその行へ、映像は空いている V1 へ入る。
    let mut project = Project::default();
    let (video, audio) = pair();
    place_media_at_drop(&mut project, video, Some(audio), A1, 30).unwrap();
    assert_eq!(
        (
            placed(&project, "id-drop-audio").0,
            placed(&project, "id-drop").0
        ),
        (A1, V1)
    );
    // 最後の行より外（index == track 数）は track を足して置く。負の位置は 0 へ寄せる。
    let mut project = Project::default();
    place_media_at_drop(
        &mut project,
        clip_on("image", TimelineClipKind::Image, V1),
        None,
        V3,
        -5,
    )
    .unwrap();
    assert_eq!(project.video_tracks.len(), 3);
    assert_eq!(placed(&project, "id-image"), (V3, 0));
    let mut project = Project::default();
    place_media_at_drop(&mut project, audio_clip("voice"), None, A1, 10).unwrap();
    assert_eq!(placed(&project, "id-voice").0, A1);

    // ドロップした行に置けないときは、上書きせずに置ける track へ回す（時刻はそのまま）。
    let mut base = Project::default();
    base.timeline_clips = vec![clip("existing")];
    let before = base.clone();
    let mut project = base.clone();
    place_media_at_drop(&mut project, audio_clip("voice"), None, V2, 40).unwrap();
    assert_eq!(
        placed(&project, "id-voice"),
        (A1, 40),
        "映像行へ落とした音声を音声 track へ回す"
    );
    let mut project = base.clone();
    place_media_at_drop(
        &mut project,
        clip_on("image", TimelineClipKind::Image, V1),
        None,
        A1,
        40,
    )
    .unwrap();
    assert_eq!(
        placed(&project, "id-image"),
        (V2, 40),
        "音声行へ落とした画像を映像 track へ回す"
    );
    // 最下段より下（新しい audio track の位置）へ画像を落としても、audio track は足さない。
    let mut project = base.clone();
    place_media_at_drop(
        &mut project,
        clip_on("image", TimelineClipKind::Image, V1),
        None,
        A2,
        40,
    )
    .unwrap();
    assert_eq!(project.audio_tracks.len(), 1);
    assert_eq!(placed(&project, "id-image").0, V2);
    // V1 は使用中。V2 が空いていればそこへ、同じ時刻で置く。
    let mut project = base.clone();
    place_media_at_drop(&mut project, clip("over"), None, V1, 100).unwrap();
    assert_eq!(placed(&project, "id-over"), (V2, 100));
    assert_eq!(find(&project, "id-existing"), base.timeline_clips.first());
    // 空いている映像 track が無ければ新しい track を足す。リンク音声は A1 へ。
    let mut project = base.clone();
    project
        .timeline_clips
        .push(clip_on("upper", TimelineClipKind::Video, V2));
    let (video, audio) = pair();
    place_media_at_drop(&mut project, video, Some(audio), V1, 100).unwrap();
    assert_eq!(project.video_tracks.len(), 3);
    assert_eq!(
        (
            placed(&project, "id-drop"),
            placed(&project, "id-drop-audio").0
        ),
        ((V3, 100), A1)
    );
    // 失敗は Project を変えない。
    assert!(place_media_at_drop(&mut base, clip("far"), None, TrackRef::video(3), 0).is_err());
    assert_eq!(base, before);
}
