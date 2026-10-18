//! Undo の memory 予算に使う、Project の大きさの見積もり。
//!
//! 確保済みの容量ではなく、入っている文字列の長さと struct の大きさだけを数える。同じ中身なら同じ値になる。

use crate::model::{Project, TimelineClip};

pub fn approximate_project_bytes(project: &Project) -> usize {
    let mut bytes = std::mem::size_of::<Project>();
    for tracks in [&project.video_tracks, &project.audio_tracks] {
        bytes = bytes.saturating_add(
            tracks
                .len()
                .saturating_mul(std::mem::size_of::<crate::model::Track>()),
        );
        for track in tracks {
            add_str(&mut bytes, &track.name);
            add_str(&mut bytes, &track.mixer_name);
        }
    }
    for clip in &project.timeline_clips {
        bytes = bytes.saturating_add(std::mem::size_of::<TimelineClip>());
        add_path(&mut bytes, &clip.media_path);
        add_str(&mut bytes, &clip.name);
        add_str(&mut bytes, &clip.id);
        add_str(&mut bytes, &clip.media_item_id);
        add_str(&mut bytes, &clip.link_group_id);
        add_str(&mut bytes, &clip.text.content);
        add_str(&mut bytes, &clip.text.font_family);
        for keys in [
            &clip.effects.opacity_keys,
            &clip.effects.volume_keys,
            &clip.effects.ducking_keys,
            &clip.effects.position_x_keys,
            &clip.effects.position_y_keys,
            &clip.effects.scale_x_keys,
            &clip.effects.scale_y_keys,
            &clip.effects.rotation_keys,
            &clip.effects.crop_left_keys,
            &clip.effects.crop_top_keys,
            &clip.effects.crop_right_keys,
            &clip.effects.crop_bottom_keys,
        ] {
            bytes = bytes.saturating_add(
                keys.len()
                    .saturating_mul(std::mem::size_of::<crate::clip_effects::ClipKeyframe>()),
            );
        }
    }
    for transition in &project.timeline_transitions {
        add_str(&mut bytes, &transition.id);
        add_str(&mut bytes, &transition.outgoing_clip_id);
        add_str(&mut bytes, &transition.incoming_clip_id);
    }
    bytes = bytes.saturating_add(
        project
            .timeline_markers
            .len()
            .saturating_mul(std::mem::size_of::<i64>()),
    );
    for item in &project.media_items {
        add_str(&mut bytes, &item.id);
        add_str(&mut bytes, &item.name);
        add_path(&mut bytes, &item.media_path);
        add_str(&mut bytes, &item.folder_id);
    }
    if let Some(subtitles) = &project.subtitles {
        add_str(&mut bytes, &subtitles.style.font_family);
        for cue in &subtitles.cues {
            add_str(&mut bytes, &cue.id);
            add_str(&mut bytes, &cue.content);
        }
    }
    bytes
}

fn add_str(bytes: &mut usize, text: &str) {
    *bytes = bytes.saturating_add(text.len());
}

fn add_path(bytes: &mut usize, path: &std::path::Path) {
    *bytes = bytes.saturating_add(path.as_os_str().len());
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
