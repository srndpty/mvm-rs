//! timeline の検証と編集。全ての編集は候補全体を検証し、失敗時は Project を変えない。

mod common;
mod edit;
mod mapping;
mod ripple;
mod settings;
mod speed;
mod transition_edit;
mod validate;

pub use common::{ClipIdIndex, EditResult, LinkMode, TrimEdge, finalize_timeline_candidate};
pub use edit::{
    EdgeEditKind, active_clip_at, active_clips_at, append_timeline_clip, apply_default_clip_fades,
    clamp_edge_edit, clip_ids_spanning_frame, clip_ids_spanning_frame_among, clip_with_edge_at,
    default_transition_frames, delete_timeline_clip, move_clip, move_clips, place_audio_clip_at,
    place_linked_av_pair_at, place_media_at_drop, place_still_clip_at, place_timeline_clip_at,
    split_timeline_clips, step_clip_volume, timeline_track_end_frame, toggle_clips_enabled,
    touching_clip_id, trim_timeline_clip, unlink_timeline_clip,
};
pub use mapping::{
    ClipProducerRange, clip_duration_at, clip_fade_source_frame_at, clip_interval,
    clip_producer_range, clip_source_boundary_to_timeline, clip_source_frame_at,
    clip_speed_in_range, clip_timebase, clip_timeline_boundary_to_source, clip_video_source,
    frame_hold_producer_position, math_intro_source_frame_at, math_intro_timeline_frames,
    source_boundary_to_timeline_boundary, source_rate_matches_timeline_rate,
    timeline_boundary_to_source_boundary, timeline_clip_duration,
};
pub use ripple::{
    SelectDirection, clamp_slide_edit, clip_ids_from_frame, edit_timeline_time,
    ripple_trim_timeline_clip, roll_timeline_edit, slide_timeline_clip, slip_timeline_clip,
};
pub use settings::{
    ClipKeyEditPreview, TimelineGap, add_track, append_manim_timeline_clip_at, delete_clip_key,
    edit_clip_key, gap_at, preview_clip_key_edit, remove_track, ripple_delete_gap,
    set_project_video_settings, set_timeline_frame_rate, set_track_muted, set_track_solo,
    set_tracks_muted,
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
