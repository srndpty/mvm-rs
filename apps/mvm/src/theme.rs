//! 色と widget の見た目。C++ 版（Qt Quick）の配色を写し、panel ごとに色を書き散らさない。

use std::sync::Arc;

use eframe::egui::{self, Color32, CornerRadius, FontFamily, FontId, Stroke};

const fn hex(rgb: u32) -> Color32 {
    Color32::from_rgb((rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8)
}

/// 窓の地。
pub const WINDOW: Color32 = hex(0x15_17_1b);
/// 左パネルなど、枠で囲う領域の地と枠。
pub const PANEL: Color32 = hex(0x1b_1f_25);
pub const PANEL_BORDER: Color32 = hex(0x34_38_40);
/// timeline の枠。
pub const TIMELINE: Color32 = hex(0x20_24_2a);
pub const TIMELINE_BORDER: Color32 = hex(0x3c_42_4c);
/// ツール列・ルーラー・ヘッダの角の地。
pub const STRIP: Color32 = hex(0x19_1c_21);
pub const PREVIEW: Color32 = hex(0x08_09_0b);

pub const TEXT: Color32 = hex(0xe6_e8_ec);
pub const TEXT_ICON: Color32 = hex(0xc9_cc_d2);
pub const TEXT_SECONDARY: Color32 = hex(0x9a_a2_ad);
pub const TEXT_TAB: Color32 = hex(0x8a_91_9c);
pub const TEXT_DIM: Color32 = hex(0x6f_76_81);
pub const TEXT_DISABLED: Color32 = hex(0x4c_52_5b);
pub const WARNING: Color32 = hex(0xf2_c6_6d);
pub const ERROR: Color32 = hex(0xef_8b_8b);
pub const ACCENT: Color32 = hex(0x64_a8_e8);

/// 押せる部品の地（通常・hover）と枠。
pub const CONTROL: Color32 = hex(0x2b_30_38);
pub const CONTROL_HOVER: Color32 = hex(0x3a_41_4c);
pub const CONTROL_BORDER: Color32 = hex(0x4a_51_5c);
/// 選んでいるもの（ツール・clip）の地と枠。
pub const SELECTED: Color32 = hex(0x31_5f_86);
pub const SELECTED_BORDER: Color32 = hex(0x65_a8_dc);
pub const TOOL_HOVER: Color32 = hex(0x2f_35_3e);

pub const RULER_TICK: Color32 = hex(0x77_80_8c);
pub const RULER_TEXT: Color32 = hex(0xaa_b1_ba);
pub const MARKER: Color32 = hex(0xf3_bd_54);
pub const IN_OUT: Color32 = hex(0x56_a5_e8);
pub const PLAYHEAD: Color32 = hex(0xf1_5b_5b);

pub const VIDEO_ROW: Color32 = hex(0x25_2a_31);
pub const VIDEO_ROW_ALT: Color32 = hex(0x20_25_2b);
pub const AUDIO_ROW: Color32 = hex(0x1e_26_26);
pub const ROW_BORDER: Color32 = hex(0x34_3a_43);
/// video と audio の境界の太線。
pub const ROW_DIVIDER: Color32 = hex(0x59_63_6f);
pub const VIDEO_HEADER: Color32 = hex(0x25_2a_31);
pub const AUDIO_HEADER: Color32 = hex(0x23_2a_2a);
pub const HEADER_TEXT_MUTED: Color32 = hex(0x8b_8f_96);
pub const SUBTITLE_HEADER: Color32 = hex(0x30_28_3b);
pub const SUBTITLE_HEADER_BORDER: Color32 = hex(0x48_40_4f);
pub const SUBTITLE_HEADER_TEXT: Color32 = hex(0xde_d0_ef);

pub const CLIP_VIDEO: Color32 = hex(0x2b_30_38);
pub const CLIP_AUDIO: Color32 = hex(0x2b_3a_33);
pub const CLIP_DETAIL: Color32 = hex(0xb8_c1_cc);
pub const WAVEFORM: Color32 = hex(0x7f_d4_9a);
pub const WAVEFORM_SELECTED: Color32 = hex(0xa9_d6_ff);
pub const KEY_LINE: Color32 = hex(0xff_d8_4d);

/// track の M / S を点けたときの色と、その上の文字の色。
pub const MUTE_ON: Color32 = hex(0x5f_b8_78);
pub const SOLO_ON: Color32 = hex(0xe8_c1_5a);
pub const TOGGLE_ON_TEXT: Color32 = hex(0x15_18_1c);
/// 非表示にした video track の目玉。
pub const EYE_HIDDEN: Color32 = hex(0x4f_9c_f0);

/// 入力欄の地（通常・押下・無効）と、hover・入力中の枠。
pub const FIELD: Color32 = hex(0x23_28_30);
pub const FIELD_PRESSED: Color32 = hex(0x2f_39_45);
pub const FIELD_DISABLED: Color32 = hex(0x1c_20_26);
pub const FIELD_DISABLED_TEXT: Color32 = hex(0x6a_70_7a);
pub const FIELD_FOCUS: Color32 = hex(0x5b_9b_d5);
/// 太字などの書式を入れたときのボタンの地。
pub const STYLE_ON: Color32 = hex(0x2f_4a_66);
/// エフェクトコントロールのキーの列・グラフ。
pub const KEY_LANE: Color32 = hex(0x20_26_2f);
pub const KEY_GRAPH: Color32 = hex(0x19_1f_27);
pub const KEY_GRID: Color32 = hex(0x30_39_46);
pub const KEY_ACCENT: Color32 = hex(0x62_b6_ff);
pub const KEY_MARK: Color32 = hex(0xd1_d7_df);
pub const KEY_BUTTON: Color32 = hex(0x28_2d_35);
pub const KEY_BUTTON_ON: Color32 = hex(0x24_47_62);
pub const KEY_ICON: Color32 = hex(0xbd_c6_d2);

/// ミキサーの目盛り・数値。
pub const SCALE_TICK: Color32 = hex(0x6d_75_80);
pub const SCALE_TEXT: Color32 = hex(0x8a_92_9d);
pub const MIXER_VALUE: Color32 = hex(0x65_b8_f0);
pub const MIXER_VALUE_HOVER: Color32 = hex(0xa9_d9_ff);

/// egui の部品へ配色を当てる。dark theme だけを持つ。
pub fn apply(ctx: &egui::Context) {
    ctx.set_theme(egui::Theme::Dark);
    ctx.style_mut_of(egui::Theme::Dark, |style| {
        let visuals = &mut style.visuals;
        visuals.panel_fill = WINDOW;
        visuals.window_fill = PANEL;
        visuals.window_stroke = Stroke::new(1.0, PANEL_BORDER);
        visuals.extreme_bg_color = hex(0x11_13_17);
        visuals.faint_bg_color = hex(0x1f_23_29);
        visuals.code_bg_color = hex(0x11_13_17);
        visuals.hyperlink_color = ACCENT;
        visuals.warn_fg_color = WARNING;
        visuals.error_fg_color = ERROR;
        visuals.window_corner_radius = CornerRadius::same(6);
        visuals.menu_corner_radius = CornerRadius::same(4);
        visuals.selection.bg_fill = SELECTED;
        visuals.selection.stroke = Stroke::new(1.0, TEXT);

        let radius = CornerRadius::same(3);
        let widgets = &mut visuals.widgets;
        widgets.noninteractive.bg_fill = PANEL;
        widgets.noninteractive.weak_bg_fill = PANEL;
        widgets.noninteractive.bg_stroke = Stroke::new(1.0, PANEL_BORDER);
        widgets.noninteractive.fg_stroke = Stroke::new(1.0, TEXT_ICON);
        widgets.noninteractive.corner_radius = radius;
        for (state, fill, border) in [
            (&mut widgets.inactive, CONTROL, CONTROL_BORDER),
            (&mut widgets.hovered, CONTROL_HOVER, hex(0x5a_62_6e)),
            (&mut widgets.active, hex(0x43_4b_57), SELECTED_BORDER),
            (&mut widgets.open, CONTROL_HOVER, CONTROL_BORDER),
        ] {
            state.bg_fill = fill;
            state.weak_bg_fill = fill;
            state.bg_stroke = Stroke::new(1.0, border);
            state.corner_radius = radius;
        }
        widgets.inactive.fg_stroke = Stroke::new(1.0, TEXT_ICON);
        widgets.hovered.fg_stroke = Stroke::new(1.5, TEXT);
        widgets.active.fg_stroke = Stroke::new(1.5, Color32::WHITE);
        widgets.open.fg_stroke = Stroke::new(1.0, TEXT);
    });
}

/// 枠で囲う領域（左パネル・preview）。
pub fn panel_frame() -> egui::Frame {
    egui::Frame::new()
        .fill(PANEL)
        .stroke(Stroke::new(1.0, PANEL_BORDER))
        .corner_radius(CornerRadius::same(4))
        .inner_margin(egui::Margin::same(8))
}

/// 日本語を表示するための UI フォント（通常と太字）。無ければ起動を失敗させる（代替フォントは探さない）。
const UI_FONT: (&str, &str) = ("meiryo", r"C:\Windows\Fonts\meiryo.ttc");
const UI_BOLD_FONT: (&str, &str) = ("meiryo-bold", r"C:\Windows\Fonts\meiryob.ttc");
/// 太字の font family の名前。track 名・clip 名・選んだタブなど、C++ 版で太字の文字に使う。
const BOLD_FAMILY: &str = "bold";

pub fn install_fonts(ctx: &egui::Context) -> Result<(), String> {
    let mut fonts = egui::FontDefinitions::default();
    for (name, path) in [UI_FONT, UI_BOLD_FONT] {
        let bytes = std::fs::read(path)
            .map_err(|error| format!("UI フォントを読めません ({path}): {error}"))?;
        fonts
            .font_data
            .insert(name.to_owned(), Arc::new(egui::FontData::from_owned(bytes)));
    }
    for family in [FontFamily::Proportional, FontFamily::Monospace] {
        fonts
            .families
            .entry(family)
            .or_default()
            .insert(0, UI_FONT.0.to_owned());
    }
    fonts.families.insert(
        FontFamily::Name(BOLD_FAMILY.into()),
        vec![UI_BOLD_FONT.0.to_owned(), UI_FONT.0.to_owned()],
    );
    ctx.set_fonts(fonts);
    Ok(())
}

/// 試験の harness はフォントファイルを読まない。太字の family だけを egui の既定の字形で定義する。
#[cfg(test)]
pub fn install_test_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    let proportional = fonts
        .families
        .get(&FontFamily::Proportional)
        .cloned()
        .unwrap_or_default();
    fonts
        .families
        .insert(FontFamily::Name(BOLD_FAMILY.into()), proportional);
    ctx.set_fonts(fonts);
}

/// 太字の文字。
pub fn bold(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(BOLD_FAMILY.into()))
}
