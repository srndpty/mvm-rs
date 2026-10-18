//! mvm 製品アプリの入口。

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod preview_ui;
mod project_panel;
mod shell;
mod timeline_ui;

use std::path::Path;
use std::sync::Arc;

use eframe::egui;

/// 日本語を表示するための UI フォント。無ければ起動を失敗させる（代替フォントは探さない）。
const UI_FONT_PATH: &str = r"C:\Windows\Fonts\meiryo.ttc";
const UI_FONT_NAME: &str = "meiryo";

fn main() -> eframe::Result {
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
            install_ui_font(&creation.egui_ctx, Path::new(UI_FONT_PATH))?;
            creation.egui_ctx.set_theme(egui::Theme::Dark);
            Ok(Box::new(shell::MvmApp::new()))
        }),
    )
}

fn install_ui_font(ctx: &egui::Context, path: &Path) -> Result<(), String> {
    let bytes = std::fs::read(path)
        .map_err(|error| format!("UI フォントを読めません ({}): {error}", path.display()))?;
    let mut fonts = egui::FontDefinitions::default();
    fonts.font_data.insert(
        UI_FONT_NAME.to_owned(),
        Arc::new(egui::FontData::from_owned(bytes)),
    );
    for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
        fonts
            .families
            .entry(family)
            .or_default()
            .insert(0, UI_FONT_NAME.to_owned());
    }
    ctx.set_fonts(fonts);
    Ok(())
}
