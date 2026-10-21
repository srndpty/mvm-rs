//! mvm 製品アプリの入口。

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod audio_adjust_ui;
mod clip_dialogs;
mod dialog;
mod export_ui;
mod icons;
mod inspector;
mod math_ui;
mod mixer_ui;
mod motion_ui;
mod preview_ui;
mod project_panel;
mod shell;
mod subtitle_lane;
mod subtitle_panel;
mod text_style_ui;
mod theme;
mod timeline_ui;
mod transcribe_ui;
mod widgets;

use std::path::PathBuf;

use eframe::egui;

fn main() -> eframe::Result {
    let project = match parse_arguments(std::env::args().skip(1)) {
        Ok(project) => project,
        Err(message) => {
            eprintln!("{message}\nusage: mvm [--project <path>]");
            std::process::exit(2);
        }
    };
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("mvm")
            .with_inner_size([1440.0, 900.0])
            .with_min_inner_size([980.0, 640.0]),
        ..Default::default()
    };
    eframe::run_native(
        "mvm",
        options,
        Box::new(|creation| {
            theme::install_fonts(&creation.egui_ctx)?;
            theme::apply(&creation.egui_ctx);
            let mut app = shell::MvmApp::new();
            if let Some(path) = project {
                app.open_at_start(&path);
            }
            Ok(Box::new(app))
        }),
    )
}

/// 起動引数。C++ 版と同じく `--project <path>` で開く Project を渡す。知らない引数は受け付けない。
fn parse_arguments(arguments: impl Iterator<Item = String>) -> Result<Option<PathBuf>, String> {
    let mut arguments = arguments;
    let mut project = None;
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--project" => {
                let path = arguments
                    .next()
                    .ok_or_else(|| "--project の後に Project のパスがありません".to_owned())?;
                project = Some(PathBuf::from(path));
            }
            other => return Err(format!("知らない引数です: {other}")),
        }
    }
    Ok(project)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(arguments: &[&str]) -> Result<Option<PathBuf>, String> {
        parse_arguments(arguments.iter().map(|argument| (*argument).to_owned()))
    }

    #[test]
    fn the_project_argument_names_the_file_to_open() {
        assert_eq!(parse(&[]), Ok(None));
        assert_eq!(
            parse(&["--project", r"C:\demo\project.mvm"]),
            Ok(Some(PathBuf::from(r"C:\demo\project.mvm")))
        );
        assert!(parse(&["--project"]).is_err());
        assert!(parse(&["project.mvm"]).is_err());
        assert!(parse(&["--manim-executable", "manim.exe"]).is_err());
    }
}
