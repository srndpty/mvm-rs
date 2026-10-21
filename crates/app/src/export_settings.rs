//! 書き出し設定のダイアログに出す要約（C++ 版の exportSettingsSummary）。入力映像の仕様は、読み込んだときに
//! 調べて素材に覚えている寸法と fps を使う（書き出しの前に file を開き直さない）。
//!
//! 静止画は C++ 版と違い、寸法を持つ入力として数える。書き出しは静止画を素材の寸法で読んで拡大・縮小する
//! ので、出力より小さい静止画には拡大の注意を出す。fps は持たないので fps の比較には入れない。

use mvm_core::Rational;
use mvm_project::{MediaKind, Project, TimelineClipKind};

/// 入力映像・出力の仕様と、気を付けること（無ければ空）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExportSettingsSummary {
    pub input: String,
    pub output: String,
    pub warning: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Spec {
    width: i32,
    height: i32,
    /// 静止画は None。
    fps: Option<Rational>,
}

fn fps_text(fps: Rational) -> String {
    if fps.den == 1 {
        format!("{} fps", fps.num)
    } else if fps.is_positive() {
        format!("{:.2} fps", fps.num as f64 / fps.den as f64)
    } else {
        "? fps".to_owned()
    }
}

fn spec_text(spec: Spec) -> String {
    match spec.fps {
        Some(fps) => format!("{}×{} / {}", spec.width, spec.height, fps_text(fps)),
        None => format!("静止画 {}×{}", spec.width, spec.height),
    }
}

/// 書き出しに出る映像の clip（有効で、出力される track の上）から要約を作る。
pub fn export_settings_summary(project: &Project) -> ExportSettingsSummary {
    let mut specs: Vec<Spec> = Vec::new();
    let mut failed = 0usize;
    let mut video_clips = 0usize;
    let mut file_clips = 0usize;
    let mut counts = [0usize; 4];
    for clip in &project.timeline_clips {
        if clip.kind == TimelineClipKind::Audio
            || !clip.enabled
            || !project.is_track_output_enabled(clip.track)
        {
            continue;
        }
        video_clips += 1;
        // Project のデータから出力の仕様で描く clip。file を読まない。
        let slot = match clip.kind {
            TimelineClipKind::Text => Some(0),
            TimelineClipKind::Math => Some(1),
            TimelineClipKind::EquationSequence => Some(2),
            TimelineClipKind::Graph => Some(3),
            _ => None,
        };
        if let Some(slot) = slot {
            counts[slot] += 1;
            continue;
        }
        file_clips += 1;
        let still = clip.kind == TimelineClipKind::Image;
        let spec = project
            .media_items
            .iter()
            .find(|item| item.id == clip.media_item_id || item.media_path == clip.media_path)
            .filter(|item| item.width > 0 && item.height > 0)
            .and_then(|item| match (still, item.kind) {
                (true, MediaKind::Image) => Some(Spec {
                    width: item.width,
                    height: item.height,
                    fps: None,
                }),
                (false, MediaKind::Video) if item.fps.is_positive() => Some(Spec {
                    width: item.width,
                    height: item.height,
                    fps: Some(item.fps),
                }),
                _ => None,
            });
        match spec {
            Some(spec) => {
                if !specs.contains(&spec) {
                    specs.push(spec);
                }
            }
            None => failed += 1,
        }
    }
    let fps = project.timeline_fps;
    let output = format!(
        "出力: {}×{} / {}",
        project.output_width,
        project.output_height,
        fps_text(fps)
    );
    let input = if video_clips == 0 {
        "入力映像: なし".to_owned()
    } else if file_clips == 0 {
        let kinds: Vec<String> = ["文字", "数式", "数式sequence", "グラフ"]
            .iter()
            .zip(counts)
            .filter(|(_, count)| *count > 0)
            .map(|(name, count)| format!("{name} {count}件"))
            .collect();
        format!(
            "入力映像: なし（{}を出力の仕様で描きます）",
            kinds.join("・")
        )
    } else if specs.is_empty() {
        "入力映像: 仕様を取得できません".to_owned()
    } else if let [spec] = specs.as_slice() {
        format!("入力映像: {}", spec_text(*spec))
    } else {
        let mut shown: Vec<String> = specs.iter().take(3).map(|spec| spec_text(*spec)).collect();
        if specs.len() > 3 {
            shown.push(format!("ほか{}仕様", specs.len() - 3));
        }
        format!("入力映像（複数仕様）: {}", shown.join("、"))
    };
    let raster_up = specs
        .iter()
        .any(|spec| spec.width < project.output_width || spec.height < project.output_height);
    let rate_up = specs.iter().filter_map(|spec| spec.fps).any(|input| {
        i128::from(input.num) * i128::from(fps.den) < i128::from(fps.num) * i128::from(input.den)
    });
    let mut warnings = Vec::new();
    if specs.len() > 1 {
        warnings.push("入力映像の仕様が混在しています。".to_owned());
    }
    if raster_up {
        warnings.push("一部の入力映像を拡大して書き出します。".to_owned());
    }
    if rate_up {
        warnings
            .push("一部の入力映像より高いfpsで書き出しますが、動きの情報は増えません。".to_owned());
    }
    if raster_up || rate_up {
        warnings.push(
            "出力の画素数やフレーム数が増えるため、元ファイルより容量が大きくなる場合があります。"
                .to_owned(),
        );
    }
    if failed > 0 {
        warnings.push(format!(
            "{failed}件の入力ファイルは仕様を確認できませんでした。"
        ));
    }
    if video_clips == 0 {
        warnings.push("映像クリップがないため、映像は黒になります。".to_owned());
    }
    ExportSettingsSummary {
        input,
        output,
        warning: warnings.join("\n"),
    }
}

#[cfg(test)]
mod tests {
    use mvm_project::{MediaItem, TimelineClip, TrackRef};

    use super::*;

    fn video(id: &str, width: i32, height: i32, fps: Rational) -> (MediaItem, TimelineClip) {
        let item = MediaItem {
            id: format!("item-{id}"),
            kind: MediaKind::Video,
            media_path: format!("{id}.mp4").into(),
            name: id.to_owned(),
            fps,
            frame_count: 60,
            width,
            height,
            ..MediaItem::default()
        };
        let clip = TimelineClip {
            kind: TimelineClipKind::Video,
            id: id.to_owned(),
            name: id.to_owned(),
            media_path: item.media_path.clone(),
            media_item_id: item.id.clone(),
            source_fps: fps,
            source_frame_count: 60,
            source_out_frame: 60,
            track: TrackRef::video(0),
            ..TimelineClip::default()
        };
        (item, clip)
    }

    #[test]
    fn an_empty_timeline_writes_black() {
        let summary = export_settings_summary(&Project::default());
        assert_eq!(summary.input, "入力映像: なし");
        assert_eq!(summary.output, "出力: 1920×1080 / 60 fps");
        assert_eq!(
            summary.warning,
            "映像クリップがないため、映像は黒になります。"
        );
    }

    #[test]
    fn a_smaller_slower_input_warns_about_upscaling() {
        let (item, clip) = video("a", 1280, 720, Rational::new(30, 1));
        let project = Project {
            media_items: vec![item],
            timeline_clips: vec![clip],
            ..Project::default()
        };
        let summary = export_settings_summary(&project);
        assert_eq!(summary.input, "入力映像: 1280×720 / 30 fps");
        assert_eq!(
            summary.warning,
            "一部の入力映像を拡大して書き出します。\n\
             一部の入力映像より高いfpsで書き出しますが、動きの情報は増えません。\n\
             出力の画素数やフレーム数が増えるため、元ファイルより容量が大きくなる場合があります。"
        );
    }

    #[test]
    fn generated_clips_are_drawn_at_the_output_spec() {
        let text = TimelineClip {
            kind: TimelineClipKind::Text,
            id: "t".to_owned(),
            track: TrackRef::video(0),
            ..TimelineClip::default()
        };
        let mut hidden = text.clone();
        hidden.id = "u".to_owned();
        hidden.enabled = false;
        let project = Project {
            timeline_clips: vec![text, hidden],
            ..Project::default()
        };
        let summary = export_settings_summary(&project);
        assert_eq!(
            summary.input,
            "入力映像: なし（文字 1件を出力の仕様で描きます）"
        );
        assert_eq!(summary.warning, "");
    }

    fn image(id: &str, width: i32, height: i32) -> (MediaItem, TimelineClip) {
        let item = MediaItem {
            id: format!("item-{id}"),
            kind: MediaKind::Image,
            media_path: format!("{id}.png").into(),
            name: id.to_owned(),
            width,
            height,
            ..MediaItem::default()
        };
        let clip = TimelineClip {
            kind: TimelineClipKind::Image,
            id: id.to_owned(),
            name: id.to_owned(),
            media_path: item.media_path.clone(),
            media_item_id: item.id.clone(),
            source_out_frame: 120,
            track: TrackRef::video(0),
            ..TimelineClip::default()
        };
        (item, clip)
    }

    #[test]
    fn a_small_still_image_warns_about_upscaling_but_not_fps() {
        let (item, clip) = image("s", 640, 480);
        let project = Project {
            media_items: vec![item],
            timeline_clips: vec![clip],
            ..Project::default()
        };
        let summary = export_settings_summary(&project);
        assert_eq!(summary.input, "入力映像: 静止画 640×480");
        // 静止画は fps を持たないので、fps の注意は出さない。
        assert_eq!(
            summary.warning,
            "一部の入力映像を拡大して書き出します。\n\
             出力の画素数やフレーム数が増えるため、元ファイルより容量が大きくなる場合があります。"
        );
    }

    #[test]
    fn a_full_size_still_with_text_has_no_warning_and_an_unknown_still_is_reported() {
        let (item, clip) = image("s", 1920, 1080);
        let text = TimelineClip {
            kind: TimelineClipKind::Text,
            id: "t".to_owned(),
            track: TrackRef::video(1),
            ..TimelineClip::default()
        };
        let project = Project {
            media_items: vec![item.clone()],
            timeline_clips: vec![clip.clone(), text],
            ..Project::default()
        };
        let summary = export_settings_summary(&project);
        assert_eq!(summary.input, "入力映像: 静止画 1920×1080");
        assert_eq!(summary.warning, "");
        // 寸法を覚えていない静止画（素材が無い・動画として登録されている）は確認できなかったと出す。
        let mut wrong = item;
        wrong.kind = MediaKind::Video;
        for media_items in [Vec::new(), vec![wrong]] {
            let project = Project {
                media_items,
                timeline_clips: vec![clip.clone()],
                ..Project::default()
            };
            let summary = export_settings_summary(&project);
            assert_eq!(summary.input, "入力映像: 仕様を取得できません");
            assert_eq!(
                summary.warning,
                "1件の入力ファイルは仕様を確認できませんでした。"
            );
        }
    }

    #[test]
    fn mixed_inputs_and_unknown_files_are_reported() {
        let (a_item, a_clip) = video("a", 1920, 1080, Rational::new(60, 1));
        let (b_item, mut b_clip) = video("b", 1920, 1080, Rational::new(30000, 1001));
        b_clip.track = TrackRef::video(1);
        let (_, mut lost) = video("c", 1920, 1080, Rational::new(60, 1));
        lost.track = TrackRef::video(1);
        lost.timeline_start_frame = 600;
        let project = Project {
            media_items: vec![a_item, b_item],
            timeline_clips: vec![a_clip, b_clip, lost],
            ..Project::default()
        };
        let summary = export_settings_summary(&project);
        assert_eq!(
            summary.input,
            "入力映像（複数仕様）: 1920×1080 / 60 fps、1920×1080 / 29.97 fps"
        );
        assert!(
            summary
                .warning
                .starts_with("入力映像の仕様が混在しています。")
        );
        assert!(
            summary
                .warning
                .contains("1件の入力ファイルは仕様を確認できませんでした。")
        );
    }
}
