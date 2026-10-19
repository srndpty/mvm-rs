//! Undo の memory 予算に使う、Project の大きさの見積もり。
//!
//! 確保済みの容量ではなく、入っている要素の数・文字列の長さと struct の大きさだけを数える。同じ中身なら同じ値になる。
//! 各 struct は全 field を分解して数えるので、field を足すと数え忘れがコンパイルで分かる。

use std::path::PathBuf;

use mvm_core::Rational;

use crate::clip_effects::{ClipEffects, ClipKeyframe};
use crate::equation_sequence::{
    ActionId, EquationAction, EquationSequenceClipData, EquationState, EquationStepTransition,
    PartId, PartPair, SemanticPart, SourceBinding, StateId, TransitionId,
};
use crate::graph_clip::{GraphAxes, GraphClipData, GraphFunction, GraphFunctionId};
use crate::math_clip::MathClipData;
use crate::model::{
    FrameHold, ManimAsset, MediaFolder, MediaItem, Project, SubtitleCue, SubtitleStyle,
    SubtitleTrack, TextClipData, TimelineClip, TimelineTransition, Track, TrackRef,
};

pub fn approximate_project_bytes(project: &Project) -> usize {
    std::mem::size_of::<Project>().saturating_add(project.heap_bytes())
}

/// 値が heap に持つ大きさ（自身の struct の大きさは含めない）。
trait HeapBytes {
    fn heap_bytes(&self) -> usize;
}

fn sum(parts: &[usize]) -> usize {
    parts
        .iter()
        .fold(0, |total, part| total.saturating_add(*part))
}

impl HeapBytes for String {
    fn heap_bytes(&self) -> usize {
        self.len()
    }
}

impl HeapBytes for PathBuf {
    fn heap_bytes(&self) -> usize {
        self.as_os_str().len()
    }
}

impl<T: HeapBytes> HeapBytes for Vec<T> {
    fn heap_bytes(&self) -> usize {
        self.iter().fold(
            self.len().saturating_mul(std::mem::size_of::<T>()),
            |total, item| total.saturating_add(item.heap_bytes()),
        )
    }
}

impl<T: HeapBytes> HeapBytes for Option<T> {
    fn heap_bytes(&self) -> usize {
        self.as_ref().map_or(0, HeapBytes::heap_bytes)
    }
}

/// heap を持たない値。
macro_rules! no_heap {
    ($($ty:ty),* $(,)?) => {
        $(impl HeapBytes for $ty {
            fn heap_bytes(&self) -> usize {
                0
            }
        })*
    };
}

no_heap!(i64, Rational, TrackRef, FrameHold, ClipKeyframe);

/// 中身が String 1 つの ID。
macro_rules! string_id {
    ($($ty:ty),* $(,)?) => {
        $(impl HeapBytes for $ty {
            fn heap_bytes(&self) -> usize {
                self.0.heap_bytes()
            }
        })*
    };
}

string_id!(StateId, TransitionId, ActionId, PartId, GraphFunctionId);

impl HeapBytes for Project {
    fn heap_bytes(&self) -> usize {
        let Self {
            timeline_fps: _,
            output_width: _,
            output_height: _,
            video_tracks,
            audio_tracks,
            manim_assets,
            timeline_clips,
            timeline_transitions,
            timeline_markers,
            in_frame: _,
            out_frame: _,
            media_folders,
            media_items,
            subtitles,
            last_audio_adjustment_settings,
        } = self;
        sum(&[
            video_tracks.heap_bytes(),
            audio_tracks.heap_bytes(),
            manim_assets.heap_bytes(),
            timeline_clips.heap_bytes(),
            timeline_transitions.heap_bytes(),
            timeline_markers.heap_bytes(),
            media_folders.heap_bytes(),
            media_items.heap_bytes(),
            subtitles.heap_bytes(),
            last_audio_adjustment_settings.heap_bytes(),
        ])
    }
}

impl HeapBytes for Track {
    fn heap_bytes(&self) -> usize {
        let Self {
            name,
            muted: _,
            solo: _,
            mixer_name,
            mixer_gain_db: _,
            mixer_pan: _,
        } = self;
        sum(&[name.heap_bytes(), mixer_name.heap_bytes()])
    }
}

impl HeapBytes for ManimAsset {
    fn heap_bytes(&self) -> usize {
        let Self {
            script_path,
            scene_name,
            generated_video_path,
            generation_state: _,
            source_fingerprint,
        } = self;
        sum(&[
            script_path.heap_bytes(),
            scene_name.heap_bytes(),
            generated_video_path.heap_bytes(),
            source_fingerprint.heap_bytes(),
        ])
    }
}

impl HeapBytes for TimelineClip {
    fn heap_bytes(&self) -> usize {
        let Self {
            kind: _,
            media_path,
            name,
            id,
            media_item_id,
            source_fps: _,
            source_frame_count: _,
            source_in_frame: _,
            source_out_frame: _,
            timeline_start_frame: _,
            effects,
            track: _,
            link_group_id,
            speed: _,
            preserve_pitch: _,
            frame_hold: _,
            text,
            math,
            math_animation: _,
            equation_sequence,
            graph,
            enabled: _,
        } = self;
        sum(&[
            media_path.heap_bytes(),
            name.heap_bytes(),
            id.heap_bytes(),
            media_item_id.heap_bytes(),
            effects.heap_bytes(),
            link_group_id.heap_bytes(),
            text.heap_bytes(),
            math.heap_bytes(),
            equation_sequence.heap_bytes(),
            graph.heap_bytes(),
        ])
    }
}

impl HeapBytes for ClipEffects {
    fn heap_bytes(&self) -> usize {
        let Self {
            position_x_percent: _,
            position_y_percent: _,
            scale_x_percent: _,
            scale_y_percent: _,
            rotation_degrees: _,
            opacity_percent: _,
            volume_percent: _,
            normalization_gain_db: _,
            ducking_db: _,
            ducking_keys,
            audio_adjustment_settings,
            audio_adjustment_fingerprint,
            opacity_keys,
            volume_keys,
            position_x_keys,
            position_y_keys,
            scale_x_keys,
            scale_y_keys,
            rotation_keys,
            crop_left_keys,
            crop_top_keys,
            crop_right_keys,
            crop_bottom_keys,
            crop_left_percent: _,
            crop_top_percent: _,
            crop_right_percent: _,
            crop_bottom_percent: _,
            fade_in_frames: _,
            fade_out_frames: _,
        } = self;
        sum(&[
            ducking_keys.heap_bytes(),
            audio_adjustment_settings.heap_bytes(),
            audio_adjustment_fingerprint.heap_bytes(),
            opacity_keys.heap_bytes(),
            volume_keys.heap_bytes(),
            position_x_keys.heap_bytes(),
            position_y_keys.heap_bytes(),
            scale_x_keys.heap_bytes(),
            scale_y_keys.heap_bytes(),
            rotation_keys.heap_bytes(),
            crop_left_keys.heap_bytes(),
            crop_top_keys.heap_bytes(),
            crop_right_keys.heap_bytes(),
            crop_bottom_keys.heap_bytes(),
        ])
    }
}

impl HeapBytes for TextClipData {
    fn heap_bytes(&self) -> usize {
        let Self {
            content,
            font_family,
            font_size: _,
            x: _,
            y: _,
            color,
            bold: _,
            alignment,
            outline_color,
            outline_width: _,
            background_color,
        } = self;
        sum(&[
            content.heap_bytes(),
            font_family.heap_bytes(),
            color.heap_bytes(),
            alignment.heap_bytes(),
            outline_color.heap_bytes(),
            background_color.heap_bytes(),
        ])
    }
}

impl HeapBytes for MathClipData {
    fn heap_bytes(&self) -> usize {
        let Self {
            syntax,
            source,
            font_size: _,
            color,
            background_color,
        } = self;
        sum(&[
            syntax.heap_bytes(),
            source.heap_bytes(),
            color.heap_bytes(),
            background_color.heap_bytes(),
        ])
    }
}

impl HeapBytes for EquationSequenceClipData {
    fn heap_bytes(&self) -> usize {
        let Self {
            states,
            transitions,
            actions,
        } = self;
        sum(&[
            states.heap_bytes(),
            transitions.heap_bytes(),
            actions.heap_bytes(),
        ])
    }
}

impl HeapBytes for EquationState {
    fn heap_bytes(&self) -> usize {
        let Self {
            id,
            equation,
            revision,
            hold_frames: _,
            parts,
        } = self;
        sum(&[
            id.heap_bytes(),
            equation.heap_bytes(),
            revision.heap_bytes(),
            parts.heap_bytes(),
        ])
    }
}

impl HeapBytes for SemanticPart {
    fn heap_bytes(&self) -> usize {
        let Self { id, label, binding } = self;
        sum(&[id.heap_bytes(), label.heap_bytes(), binding.heap_bytes()])
    }
}

impl HeapBytes for SourceBinding {
    fn heap_bytes(&self) -> usize {
        let Self {
            revision,
            begin: _,
            end: _,
            expected_text,
            status: _,
        } = self;
        sum(&[revision.heap_bytes(), expected_text.heap_bytes()])
    }
}

impl HeapBytes for EquationStepTransition {
    fn heap_bytes(&self) -> usize {
        let Self {
            id,
            from,
            to,
            frames: _,
            correspondence,
        } = self;
        sum(&[
            id.heap_bytes(),
            from.heap_bytes(),
            to.heap_bytes(),
            correspondence.heap_bytes(),
        ])
    }
}

impl HeapBytes for PartPair {
    fn heap_bytes(&self) -> usize {
        let Self { from, to } = self;
        sum(&[from.heap_bytes(), to.heap_bytes()])
    }
}

impl HeapBytes for EquationAction {
    fn heap_bytes(&self) -> usize {
        let Self {
            id,
            state,
            target,
            target_status: _,
            start: _,
            duration: _,
            operation: _,
        } = self;
        sum(&[id.heap_bytes(), state.heap_bytes(), target.heap_bytes()])
    }
}

impl HeapBytes for GraphClipData {
    fn heap_bytes(&self) -> usize {
        let Self {
            viewport: _,
            axes,
            functions,
            intro: _,
        } = self;
        sum(&[axes.heap_bytes(), functions.heap_bytes()])
    }
}

impl HeapBytes for GraphAxes {
    fn heap_bytes(&self) -> usize {
        let Self {
            show_axes: _,
            show_grid: _,
            x_label,
            y_label,
        } = self;
        sum(&[x_label.heap_bytes(), y_label.heap_bytes()])
    }
}

impl HeapBytes for GraphFunction {
    fn heap_bytes(&self) -> usize {
        let Self {
            id,
            expression,
            domain_min: _,
            domain_max: _,
            label,
            color,
            stroke_width: _,
        } = self;
        sum(&[
            id.heap_bytes(),
            expression.heap_bytes(),
            label.heap_bytes(),
            color.heap_bytes(),
        ])
    }
}

impl HeapBytes for TimelineTransition {
    fn heap_bytes(&self) -> usize {
        let Self {
            id,
            outgoing_clip_id,
            incoming_clip_id,
            frames_before_cut: _,
            frames_after_cut: _,
            kind: _,
        } = self;
        sum(&[
            id.heap_bytes(),
            outgoing_clip_id.heap_bytes(),
            incoming_clip_id.heap_bytes(),
        ])
    }
}

impl HeapBytes for MediaFolder {
    fn heap_bytes(&self) -> usize {
        let Self {
            id,
            name,
            parent_id,
        } = self;
        sum(&[id.heap_bytes(), name.heap_bytes(), parent_id.heap_bytes()])
    }
}

impl HeapBytes for MediaItem {
    fn heap_bytes(&self) -> usize {
        let Self {
            id,
            kind: _,
            media_path,
            name,
            folder_id,
            fps: _,
            frame_count: _,
            width: _,
            height: _,
            sample_rate: _,
            duration_samples: _,
        } = self;
        sum(&[
            id.heap_bytes(),
            media_path.heap_bytes(),
            name.heap_bytes(),
            folder_id.heap_bytes(),
        ])
    }
}

impl HeapBytes for SubtitleTrack {
    fn heap_bytes(&self) -> usize {
        let Self {
            visible: _,
            style,
            cues,
        } = self;
        sum(&[style.heap_bytes(), cues.heap_bytes()])
    }
}

impl HeapBytes for SubtitleStyle {
    fn heap_bytes(&self) -> usize {
        let Self {
            font_family,
            font_size: _,
            bold: _,
            color,
            outline_color,
            outline_width: _,
            background_color,
            alignment,
            side_margin: _,
            bottom_margin: _,
        } = self;
        sum(&[
            font_family.heap_bytes(),
            color.heap_bytes(),
            outline_color.heap_bytes(),
            background_color.heap_bytes(),
            alignment.heap_bytes(),
        ])
    }
}

impl HeapBytes for SubtitleCue {
    fn heap_bytes(&self) -> usize {
        let Self {
            id,
            start_frame: _,
            end_frame: _,
            content,
            link_clip_id,
        } = self;
        sum(&[
            id.heap_bytes(),
            content.heap_bytes(),
            link_clip_id.heap_bytes(),
        ])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::TimelineClipKind;

    #[test]
    fn a_longer_name_adds_exactly_the_extra_bytes() {
        let mut short = Project::default();
        short.video_tracks[0].name = "V".to_owned();
        let mut long = short.clone();
        long.video_tracks[0].name = "Vxxxx".to_owned();
        assert_eq!(
            approximate_project_bytes(&long) - approximate_project_bytes(&short),
            4
        );
        assert_eq!(
            approximate_project_bytes(&short),
            approximate_project_bytes(&short)
        );
        assert!(approximate_project_bytes(&Project::default()) > 0);
    }

    #[test]
    fn nested_equation_sequence_and_math_source_are_counted() {
        let mut project = Project::default();
        let mut clip = TimelineClip {
            kind: TimelineClipKind::EquationSequence,
            ..TimelineClip::default()
        };
        clip.math.source = String::new();
        clip.equation_sequence.states.clear();
        project.timeline_clips.push(clip.clone());
        let empty = approximate_project_bytes(&project);
        clip.math.source = "x".repeat(1000);
        let state = EquationState {
            id: StateId::new("s".repeat(10)),
            equation: MathClipData {
                source: "y".repeat(5000),
                ..MathClipData::default()
            },
            ..EquationState::default()
        };
        clip.equation_sequence.states = vec![state; 100];
        project.timeline_clips[0] = clip;
        let grown = approximate_project_bytes(&project) - empty;
        // 文字列だけで 1000 + 100 × (10 + 5000) bytes。state の struct 分はさらに上に乗る。
        assert!(grown >= 1000 + 100 * 5010, "grown = {grown}");
    }

    #[test]
    fn media_folders_and_audio_settings_are_counted() {
        let base = Project::default();
        let mut project = base.clone();
        project.last_audio_adjustment_settings = "a".repeat(300);
        project.media_folders.push(MediaFolder {
            id: "f".repeat(7),
            name: "n".repeat(11),
            parent_id: String::new(),
        });
        assert_eq!(
            approximate_project_bytes(&project) - approximate_project_bytes(&base),
            300 + 7 + 11 + std::mem::size_of::<MediaFolder>()
        );
    }
}
