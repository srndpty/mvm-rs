//! timeline の検証と編集。全ての編集は候補全体を検証し、失敗時は Project を変えない。

mod clipboard;
mod common;
mod edit;
mod gestures;
mod mapping;
mod marks;
mod ripple;
mod sequence;
mod settings;
mod speed;
mod transition_edit;
mod validate;

pub use clipboard::{CopyPlacement, place_copied_clips};
pub use common::{ClipIdIndex, EditResult, LinkMode, TrimEdge, finalize_timeline_candidate};
pub use edit::{
    EdgeEditKind, active_clip_at, active_clips_at, append_timeline_clip, apply_default_clip_fades,
    clamp_edge_edit, clip_ids_spanning_frame, clip_ids_spanning_frame_among, clip_with_edge_at,
    default_transition_frames, delete_timeline_clip, delete_timeline_clips, edit_text_clip,
    move_clip, move_clips, place_audio_clip_at, place_linked_av_pair_at, place_media_at_drop,
    place_still_clip_at, place_text_clip, place_timeline_clip_at, place_video_media_at,
    split_timeline_clips, step_clip_volume, text_preset_position, timeline_track_end_frame,
    toggle_clips_enabled, top_video_clip_at, touching_clip_id, trim_timeline_clip,
    unlink_timeline_clip,
};
pub use gestures::{
    BodyAction, BodyGesture, BodyPress, ClipSpan, DragBounds, EdgeAction, GestureModifiers,
    PenGeometry, PenKey, PenPoint, PenPress, PenRelease, PenSample, SnapFrames, SnapOffset, TOOLS,
    TimelineTool, ToolSpec, body_press, body_release, drag_snap_frames, edge_release,
    group_drag_offset_x, group_drag_track_index, linked_for, marker_near_ruler_x, pen_line_points,
    pen_line_y_at, pen_nearest_key, pen_press, pen_release, pen_snap_value, pen_usable_height,
    pen_value_at, pen_visible_keys, pen_visible_line_points, pen_y, snap_drag_offset_x,
};
pub use mapping::{
    ClipProducerRange, clip_duration_at, clip_fade_source_frame_at, clip_interval,
    clip_producer_range, clip_source_boundary_to_timeline, clip_source_frame_at,
    clip_speed_in_range, clip_timebase, clip_timeline_boundary_to_source, clip_video_source,
    frame_hold_producer_position, math_intro_source_frame_at, math_intro_timeline_frames,
    source_boundary_to_timeline_boundary, source_rate_matches_timeline_rate,
    timeline_boundary_to_source_boundary, timeline_clip_duration,
};
pub use marks::{
    add_timeline_marker, copy_clip_keys, delete_clip_keys, delete_timeline_marker,
    effect_edit_frame, paste_clip_keys, set_clip_effect_at, set_clip_fades, set_clip_key_animation,
    set_clip_key_interpolation, set_clip_key_spline, set_mark_in, set_mark_out,
    set_subtitle_visible, toggle_clip_key,
};
pub use ripple::{
    SelectDirection, clamp_slide_edit, clip_ids_from_frame, edit_timeline_time,
    ripple_delete_clips, ripple_trim_timeline_clip, roll_timeline_edit, slide_timeline_clip,
    slip_timeline_clip,
};
pub use sequence::{
    EquationRenderability, GraphFrame, add_equation_sequence, add_graph, copy_internal_clip,
    edit_equation_sequence, edit_graph, edit_math_clip, equation_renderability,
    evaluate_equation_clip, evaluate_graph_clip, place_new_equation_sequence, place_new_graph,
    place_new_math, set_graph_source_duration,
};
pub use settings::{
    ClipKeyEditPreview, TimelineGap, add_track, append_manim_timeline_clip_at, delete_clip_key,
    edit_clip_key, gap_at, install_manim_asset, preview_clip_key_edit, remove_track,
    ripple_delete_gap, set_audio_track_mix, set_project_video_settings, set_timeline_frame_rate,
    set_track_muted, set_track_solo, set_tracks_muted, video_settings_for_media,
};
pub use speed::{
    ClipSpeedDurationEdit, ClipSpeedDurationPreview, RateStretchPreview, RateStretchPreviewClip,
    SpeedDurationError, SpeedDurationInput, clamp_rate_edit, insert_frame_hold,
    preview_clip_speed_duration, preview_rate_stretch, rate_stretch_timeline_clip,
    set_clip_speed_duration,
};
pub use transition_edit::{
    SpanFitMode, TransitionEdit, TransitionSpanFit, apply_default_edit_transition,
    apply_math_transform_transition, delete_timeline_transition, nearest_transition_span,
    set_timeline_transition_span, transition_span_limits,
};
pub use validate::{TimelineValidation, reconcile_timeline_transitions, validate_timeline};

/// timeline の終端（clip と字幕の終端の最大）。検証に失敗したら失敗する。
pub fn timeline_end_frame(project: &crate::Project) -> crate::ProjectResult<i64> {
    validate_timeline(project).map(|validation| validation.total_frames)
}
