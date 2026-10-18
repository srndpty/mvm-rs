//! mvm 製品アプリの入口。

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

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
            .with_inner_size([1280.0, 720.0]),
        ..Default::default()
    };
    eframe::run_native(
        "mvm",
        options,
        Box::new(|creation| {
            install_ui_font(&creation.egui_ctx, Path::new(UI_FONT_PATH))?;
            creation.egui_ctx.set_theme(egui::Theme::Dark);
            Ok(Box::new(MvmApp))
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

struct MvmApp;

impl eframe::App for MvmApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        egui::CentralPanel::default().show(ui, |ui| {
            ui.heading("mvm");
            ui.label("Rust 移行中: P0 基盤");
            let timecode = mvm_core::timecode::format_timecode(0, 60, 1).unwrap_or_default();
            ui.monospace(timecode);
        });
    }
}
