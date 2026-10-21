//! 文字 block を straight alpha の RGBA にする。
//!
//! 重ね塗りは premultiplied で行い、最後に straight alpha へ戻す。compositor の RGBA layer は
//! 静止画と同じく straight alpha を受け取り、shader で alpha を掛けるため。
//! 行の高さは font size の 1.2 倍。輪郭は円に並べた字形で近似する。Qt の
//! stroke と同じ画素にはならない。preview と、将来の書き出しは同じ関数を使う。

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use cosmic_text::fontdb::Database;
use cosmic_text::{
    Attrs, Buffer, Color, Family, FontSystem, Metrics, Shaping, SwashCache, Weight, Wrap,
};

use crate::error::TextError;

/// 行内の揃え。字幕の画面上の位置もこれを使う。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Align {
    Left,
    Center,
    Right,
}

/// 画面へ置く 1 つの文字 block。色は 0xAARRGGBB。
#[derive(Clone, Debug, PartialEq)]
pub struct GlyphRun {
    pub content: String,
    pub font_family: String,
    pub font_size: i32,
    pub origin_x: i32,
    pub origin_y: i32,
    pub color: u32,
    pub bold: bool,
    pub align: Align,
    pub outline_color: u32,
    pub outline_width: i32,
    pub background: u32,
}

/// 画面の下へ置く字幕。折り返しと座標は [`place_subtitle`] が決める。
#[derive(Clone, Debug, PartialEq)]
pub struct SubtitlePaint {
    pub content: String,
    pub font_family: String,
    pub font_size: i32,
    pub bold: bool,
    pub color: u32,
    pub outline_color: u32,
    pub outline_width: i32,
    pub background: u32,
    pub align: Align,
    pub side_margin: f64,
    pub bottom_margin: f64,
}

const LINE_FACTOR: f64 = 1.2;

/// `run.font_family` の書体で `run` を `width`×`height` の straight alpha の RGBA に描く。
/// 複数行は block の幅（最も長い行）の中で `run.align` に揃える。
pub fn raster(run: &GlyphRun, width: u32, height: u32) -> Result<Vec<u8>, TextError> {
    raster_at(run, width, height, system_fonts()?)
}

/// この PC に入っている書体の名前（英語名）。重複を除いて並べる。
pub fn font_families() -> Result<Vec<String>, TextError> {
    let mut names: Vec<String> = system_fonts()?
        .faces()
        .filter_map(|face| face.families.first().map(|(name, _)| name.clone()))
        .collect();
    names.sort_by_key(|name| name.to_lowercase());
    names.dedup();
    Ok(names)
}

/// 書体の一覧に出す見本。`family` の書体で `text` を `font_size` px で描いた、block の大きさちょうどの
/// straight alpha の RGBA と幅・高さ。一覧を眺めるだけで字形を溜め込まないよう、読んだ書体は覚えない。
pub fn font_sample(
    family: &str,
    text: &str,
    font_size: i32,
    argb: u32,
) -> Result<(u32, u32, Vec<u8>), TextError> {
    let database = system_fonts()?;
    let (selected, canonical, _) = family_faces(database, family, false)?;
    let mut fonts = Fonts {
        system: FontSystem::new_with_locale_and_db("ja-JP".to_owned(), selected),
        family: canonical,
        synthetic_bold: 0,
    };
    let run = GlyphRun {
        content: text.to_owned(),
        font_family: family.to_owned(),
        font_size,
        origin_x: 0,
        origin_y: 0,
        color: argb,
        bold: false,
        align: Align::Left,
        outline_color: 0,
        outline_width: 0,
        background: 0,
    };
    let (width, height) = measure(&mut fonts, &run)?;
    let width = width.ceil().clamp(1.0, 4096.0) as u32;
    let height = height.ceil().clamp(1.0, 4096.0) as u32;
    let pixels = draw(&mut fonts, &run, width, height)?;
    Ok((width, height, pixels))
}

/// OS の書体の一覧。最初に 1 回だけ読む。1 つも無ければ失敗する。
fn system_fonts() -> Result<&'static Database, TextError> {
    static FONTS: OnceLock<Database> = OnceLock::new();
    let database = FONTS.get_or_init(|| {
        let mut database = Database::new();
        database.load_system_fonts();
        database
    });
    if database.is_empty() {
        return Err(TextError::new("OS の書体を読めません"));
    }
    Ok(database)
}

/// `run` を描いたときの block の幅と高さ（輪郭と余白を含む画素）。[`raster`] と同じ測り方。
pub fn block_size(run: &GlyphRun) -> Result<(f64, f64), TextError> {
    if run.content.is_empty() {
        return Err(TextError::new("文字画像の本文が空です"));
    }
    let mut fonts = load_fonts(system_fonts()?, &run.font_family, run.bold, run.font_size)?;
    measure(&mut fonts, run)
}

/// 字幕を折り返して下へ置き、[`raster`] と同じ画素にする。
pub fn raster_subtitle(
    paint: &SubtitlePaint,
    width: u32,
    height: u32,
) -> Result<Vec<u8>, TextError> {
    let run = place_subtitle(paint, width, height)?;
    raster(&run, width, height)
}

/// 字幕の定位置。フォントを読めないと折り返しの幅が決まらない。
pub fn place_subtitle(
    paint: &SubtitlePaint,
    width: u32,
    height: u32,
) -> Result<GlyphRun, TextError> {
    validate_canvas(width, height)?;
    if paint.content.is_empty() {
        return Err(TextError::new("文字画像の本文が空です"));
    }
    if paint.font_size <= 0 {
        return Err(TextError::new("文字の大きさが不正です"));
    }
    if !paint.side_margin.is_finite()
        || !paint.bottom_margin.is_finite()
        || paint.side_margin < 0.0
        || paint.bottom_margin < 0.0
        || paint.bottom_margin >= 1.0
    {
        return Err(TextError::new("字幕の余白が不正です"));
    }
    let padding = f64::from(paint.outline_width.max(0) + 2);
    let available = f64::from(width) * (1.0 - 2.0 * paint.side_margin) - 2.0 * padding;
    if available <= 0.0 {
        return Err(TextError::new("字幕の余白が描画幅を超えています"));
    }
    let line_box = f64::from(paint.font_size) * LINE_FACTOR + 2.0 * padding;
    let room = f64::from(height) * (1.0 - paint.bottom_margin);
    if line_box > room {
        return Err(TextError::new(
            "字幕が画面の高さを超えています。本文・サイズ・余白を調整してください",
        ));
    }
    let mut fonts = load_fonts(
        system_fonts()?,
        &paint.font_family,
        paint.bold,
        paint.font_size,
    )?;
    let wrapped = wrap_paragraphs(&mut fonts, paint, available as f32)?;
    let run = GlyphRun {
        content: wrapped,
        font_family: paint.font_family.clone(),
        font_size: paint.font_size,
        origin_x: 0,
        origin_y: 0,
        color: paint.color,
        bold: paint.bold,
        align: Align::Left,
        outline_color: paint.outline_color,
        outline_width: paint.outline_width,
        background: paint.background,
    };
    let (block_w, block_h) = measure(&mut fonts, &run)?;
    if block_h > room {
        return Err(TextError::new(
            "字幕が画面の高さを超えています。本文・サイズ・余白を調整してください",
        ));
    }
    let x = match paint.align {
        Align::Left => f64::from(width) * paint.side_margin,
        Align::Center => (f64::from(width) - block_w) / 2.0,
        Align::Right => f64::from(width) * (1.0 - paint.side_margin) - block_w,
    };
    let y = room - block_h;
    Ok(GlyphRun {
        origin_x: x.round().max(0.0) as i32,
        origin_y: y.round().max(0.0) as i32,
        align: paint.align,
        ..run
    })
}

fn raster_at(
    run: &GlyphRun,
    width: u32,
    height: u32,
    database: &Database,
) -> Result<Vec<u8>, TextError> {
    validate_canvas(width, height)?;
    if run.content.is_empty() {
        return Err(TextError::new("文字画像の本文が空です"));
    }
    if run.font_size <= 0 || run.outline_width < 0 {
        return Err(TextError::new("文字の大きさが不正です"));
    }
    let mut fonts = load_fonts(database, &run.font_family, run.bold, run.font_size)?;
    draw(&mut fonts, run, width, height)
}

/// 読んだ書体で `run` を描く。
fn draw(fonts: &mut Fonts, run: &GlyphRun, width: u32, height: u32) -> Result<Vec<u8>, TextError> {
    let width_usize =
        usize::try_from(width).map_err(|_| TextError::new("文字画像の寸法が不正です"))?;
    let height_usize =
        usize::try_from(height).map_err(|_| TextError::new("文字画像の寸法が不正です"))?;
    let bytes = width_usize
        .checked_mul(height_usize)
        .and_then(|pixels| pixels.checked_mul(4))
        .ok_or_else(|| TextError::new("文字画像の画素数が overflow しました"))?;
    let mut pixels = vec![0u8; bytes];
    let mut cache = SwashCache::new();
    let (block_w, block_h) = measure(fonts, run)?;
    fill_background(&mut pixels, width, run, block_w, block_h);
    // 揃えの基準は最も長い行の幅。幅を渡さないと cosmic-text は行ごとの幅で揃え、揃えが効かない。
    let padding = run.outline_width + 2;
    let content_width = (block_w - 2.0 * f64::from(padding)) as f32;
    let mut buffer = shape_lines(fonts, run, Some(content_width), Wrap::None)?;
    let origin_x = run.origin_x + padding;
    let origin_y = run.origin_y + padding;
    if run.outline_width > 0 {
        let radius = run.outline_width;
        let mut y = -radius;
        while y <= radius {
            let mut x = -radius;
            while x <= radius {
                if x * x + y * y <= radius * radius {
                    paint(
                        fonts,
                        &mut cache,
                        &mut buffer,
                        &mut pixels,
                        width,
                        (origin_x + x, origin_y + y),
                        run.outline_color,
                    );
                }
                x += 1;
            }
            y += 1;
        }
    }
    paint(
        fonts,
        &mut cache,
        &mut buffer,
        &mut pixels,
        width,
        (origin_x, origin_y),
        run.color,
    );
    unpremultiply(&mut pixels);
    Ok(pixels)
}

fn validate_canvas(width: u32, height: u32) -> Result<(), TextError> {
    if width == 0 || height == 0 {
        Err(TextError::new("文字画像の寸法または本文が不正です"))
    } else {
        Ok(())
    }
}

/// 描く書体。太字の字形が無い書体で太字を頼まれたら、字形を横にずらして重ねる幅（px）を持つ（Qt と同じく
/// 太字を合成する。別の書体へは置き換えない）。
struct Fonts {
    system: FontSystem,
    family: String,
    synthetic_bold: i32,
}

/// `family` の書体だけを集めた database と、英語名・太字の字形があるか。
type FamilyFaces = (Database, String, bool);

/// `database` から `family` の字形を読む。読んだ字形は共有の Arc に置き、同じ書体を 2 回目からは読み直さない
/// （preview は frame ごとに文字を描く）。
fn family_faces(
    database: &Database,
    family: &str,
    remember: bool,
) -> Result<FamilyFaces, TextError> {
    static CACHE: OnceLock<Mutex<HashMap<(usize, String), FamilyFaces>>> = OnceLock::new();
    let key = (std::ptr::from_ref(database) as usize, family.to_owned());
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Some(found) = cache
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .get(&key)
    {
        return Ok(found.clone());
    }
    let mut selected = Database::new();
    let mut canonical = None;
    let mut heavy = false;
    for face in database.faces() {
        if !face.families.iter().any(|(name, _)| name == family) {
            continue;
        }
        if canonical.is_none() {
            canonical = face.families.first().map(|(name, _)| name.clone());
        }
        heavy |= face.weight.0 >= 600;
        let data = database
            .with_face_data(face.id, |data, _| data.to_vec())
            .ok_or_else(|| TextError::new(format!("書体を読めません: {family}")))?;
        selected.load_font_data(data);
    }
    let Some(canonical) = canonical else {
        return Err(TextError::new(format!(
            "指定フォントが見つかりません: {family}"
        )));
    };
    let faces = (selected, canonical, heavy);
    if !remember {
        return Ok(faces);
    }
    cache
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .insert(key, faces.clone());
    Ok(faces)
}

/// `database` から `family` の書体だけを読む。名前は英語名・各言語の名前のどれでもよい。無ければ失敗する。
fn load_fonts(
    database: &Database,
    family: &str,
    bold: bool,
    font_size: i32,
) -> Result<Fonts, TextError> {
    let (selected, canonical, heavy) = family_faces(database, family, true)?;
    let synthetic_bold = if bold && !heavy {
        (font_size / 32).max(1)
    } else {
        0
    };
    Ok(Fonts {
        system: FontSystem::new_with_locale_and_db("ja-JP".to_owned(), selected),
        family: canonical,
        synthetic_bold,
    })
}

fn metrics(font_size: i32) -> Result<Metrics, TextError> {
    if font_size <= 0 {
        return Err(TextError::new("文字の大きさが不正です"));
    }
    let size = font_size as f32;
    let line = size * LINE_FACTOR as f32;
    if !size.is_finite() || !line.is_finite() || size <= 0.0 {
        return Err(TextError::new("文字の大きさが不正です"));
    }
    Ok(Metrics::new(size, line))
}

fn cosmic_align(align: Align) -> cosmic_text::Align {
    match align {
        Align::Left => cosmic_text::Align::Left,
        Align::Center => cosmic_text::Align::Center,
        Align::Right => cosmic_text::Align::Right,
    }
}

fn attrs(family: &str, bold: bool) -> Attrs<'_> {
    Attrs::new().family(Family::Name(family)).weight(if bold {
        Weight::BOLD
    } else {
        Weight::NORMAL
    })
}

fn shape_lines(
    fonts: &mut Fonts,
    run: &GlyphRun,
    width: Option<f32>,
    wrap: Wrap,
) -> Result<Buffer, TextError> {
    let system = &mut fonts.system;
    let mut buffer = Buffer::new(system, metrics(run.font_size)?);
    buffer.set_size(system, width, None);
    buffer.set_wrap(system, wrap);
    buffer.set_text(
        system,
        &run.content,
        &attrs(&fonts.family, run.bold),
        Shaping::Advanced,
    );
    let align = Some(cosmic_align(run.align));
    for line in &mut buffer.lines {
        line.set_align(align);
    }
    buffer.shape_until_scroll(system, false);
    Ok(buffer)
}

fn wrap_paragraphs(
    fonts: &mut Fonts,
    paint: &SubtitlePaint,
    available: f32,
) -> Result<String, TextError> {
    let probe = GlyphRun {
        content: paint.content.clone(),
        font_family: paint.font_family.clone(),
        font_size: paint.font_size,
        origin_x: 0,
        origin_y: 0,
        color: paint.color,
        bold: paint.bold,
        align: Align::Left,
        outline_color: paint.outline_color,
        outline_width: paint.outline_width,
        background: paint.background,
    };
    let buffer = shape_lines(fonts, &probe, Some(available), Wrap::WordOrGlyph)?;
    let mut lines = Vec::new();
    for layout in buffer.layout_runs() {
        let text = layout.text.trim_end_matches(['\n', '\r']);
        if layout.glyphs.is_empty() {
            lines.push(String::new());
        } else {
            let start = layout.glyphs.first().map(|glyph| glyph.start).unwrap_or(0);
            let end = layout
                .glyphs
                .last()
                .map(|glyph| glyph.end)
                .unwrap_or(text.len());
            if start <= end && end <= text.len() {
                lines.push(text[start..end].to_owned());
            }
        }
    }
    if lines.is_empty() {
        return Err(TextError::new("文字画像の本文が空です"));
    }
    Ok(lines.join("\n"))
}

fn measure(fonts: &mut Fonts, run: &GlyphRun) -> Result<(f64, f64), TextError> {
    let buffer = shape_lines(fonts, run, None, Wrap::None)?;
    let mut max_width = 0f32;
    let mut lines = 0i32;
    for layout in buffer.layout_runs() {
        max_width = max_width.max(layout.line_w);
        lines += 1;
    }
    if lines == 0 {
        lines = 1;
    }
    let padding = f64::from(run.outline_width.max(0) + 2);
    let line = f64::from(run.font_size) * LINE_FACTOR;
    Ok((
        f64::from(max_width) + f64::from(fonts.synthetic_bold) + 2.0 * padding,
        f64::from(lines) * line + 2.0 * padding,
    ))
}

fn fill_background(pixels: &mut [u8], width: u32, run: &GlyphRun, block_w: f64, block_h: f64) {
    let color = premultiply(run.background);
    if color[3] == 0 {
        return;
    }
    let x0 = run.origin_x;
    let y0 = run.origin_y;
    let x1 = x0 + block_w.round() as i32;
    let y1 = y0 + block_h.round() as i32;
    let height = pixels.len() / (width as usize * 4);
    for y in y0..y1 {
        if y < 0 || y as usize >= height {
            continue;
        }
        for x in x0..x1 {
            if x < 0 || x as u32 >= width {
                continue;
            }
            blend(pixels, width, x, y, color);
        }
    }
}

fn paint(
    fonts: &mut Fonts,
    cache: &mut SwashCache,
    buffer: &mut Buffer,
    pixels: &mut [u8],
    width: u32,
    origin: (i32, i32),
    argb: u32,
) {
    let color = Color::rgba(red(argb), green(argb), blue(argb), alpha(argb));
    for layout in buffer.layout_runs() {
        for glyph in layout.glyphs.iter() {
            let physical = glyph.physical((0.0, 0.0), 1.0);
            let base_y = origin.1 + layout.line_y as i32 + physical.y;
            for shift in 0..=fonts.synthetic_bold {
                let base_x = origin.0 + physical.x + shift;
                cache.with_pixels(
                    &mut fonts.system,
                    physical.cache_key,
                    color,
                    |x, y, pixel| {
                        blend(
                            pixels,
                            width,
                            base_x + x,
                            base_y + y,
                            premultiply(
                                u32::from(pixel.a()) << 24
                                    | u32::from(pixel.r()) << 16
                                    | u32::from(pixel.g()) << 8
                                    | u32::from(pixel.b()),
                            ),
                        );
                    },
                );
            }
        }
    }
}

fn red(argb: u32) -> u8 {
    (argb >> 16) as u8
}

fn green(argb: u32) -> u8 {
    (argb >> 8) as u8
}

fn blue(argb: u32) -> u8 {
    argb as u8
}

fn alpha(argb: u32) -> u8 {
    (argb >> 24) as u8
}

fn premultiply(argb: u32) -> [u8; 4] {
    let a = u32::from(alpha(argb));
    let scale = |channel: u8| ((u32::from(channel) * a + 127) / 255) as u8;
    [
        scale(red(argb)),
        scale(green(argb)),
        scale(blue(argb)),
        a as u8,
    ]
}

/// premultiplied の画素を straight alpha に戻す。alpha 0 は全て 0 にする。
fn unpremultiply(pixels: &mut [u8]) {
    for pixel in pixels.as_chunks_mut::<4>().0 {
        let a = u32::from(pixel[3]);
        if a == 0 {
            pixel[..3].fill(0);
            continue;
        }
        for channel in &mut pixel[..3] {
            *channel = ((u32::from(*channel) * 255 + a / 2) / a).min(255) as u8;
        }
    }
}

fn blend(pixels: &mut [u8], width: u32, x: i32, y: i32, src: [u8; 4]) {
    if x < 0 || y < 0 {
        return;
    }
    let Ok(x) = usize::try_from(x) else {
        return;
    };
    let Ok(y) = usize::try_from(y) else {
        return;
    };
    let Ok(width) = usize::try_from(width) else {
        return;
    };
    let Some(row) = y.checked_mul(width) else {
        return;
    };
    let Some(index) = row.checked_add(x).and_then(|pixel| pixel.checked_mul(4)) else {
        return;
    };
    let Some(pixel) = pixels.get_mut(index..index + 4) else {
        return;
    };
    let inverse = 255u16 - u16::from(src[3]);
    for channel in 0..4 {
        let mixed = u16::from(src[channel]) + u16::from(pixel[channel]) * inverse / 255;
        pixel[channel] = mixed as u8;
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    fn paint(content: &str) -> SubtitlePaint {
        SubtitlePaint {
            content: content.to_owned(),
            font_family: "Meiryo".to_owned(),
            font_size: 64,
            bold: false,
            color: 0xFFFF_FFFF,
            outline_color: 0xFF00_0000,
            outline_width: 0,
            background: 0,
            align: Align::Center,
            side_margin: 0.05,
            bottom_margin: 0.15,
        }
    }

    #[test]
    fn rejects_empty_text_an_unknown_family_and_a_missing_font() {
        let error = raster(
            &GlyphRun {
                content: String::new(),
                font_family: "Meiryo".to_owned(),
                font_size: 32,
                origin_x: 0,
                origin_y: 0,
                color: 0xFFFF_FFFF,
                bold: false,
                align: Align::Left,
                outline_color: 0,
                outline_width: 0,
                background: 0,
            },
            32,
            32,
        )
        .unwrap_err();
        assert!(error.to_string().contains("空です"), "{error}");

        let error = raster_at(
            &GlyphRun {
                content: "あ".to_owned(),
                font_family: "NotAFont".to_owned(),
                font_size: 32,
                origin_x: 0,
                origin_y: 0,
                color: 0xFFFF_FFFF,
                bold: false,
                align: Align::Left,
                outline_color: 0,
                outline_width: 0,
                background: 0,
            },
            32,
            32,
            &Database::new(),
        )
        .unwrap_err();
        assert!(error.to_string().contains("NotAFont"), "{error}");

        let error = raster_at(
            &GlyphRun {
                content: "あ".to_owned(),
                font_family: "Meiryo".to_owned(),
                font_size: 32,
                origin_x: 0,
                origin_y: 0,
                color: 0xFFFF_FFFF,
                bold: false,
                align: Align::Left,
                outline_color: 0,
                outline_width: 0,
                background: 0,
            },
            32,
            32,
            &Database::new(),
        )
        .unwrap_err();
        // 書体の無い一覧からは引けない（別の書体へ置き換えない）。
        assert!(
            error
                .to_string()
                .contains("指定フォントが見つかりません: Meiryo"),
            "{error}"
        );
    }

    fn run(content: &str, align: Align, background: u32) -> GlyphRun {
        GlyphRun {
            content: content.to_owned(),
            font_family: "Meiryo".to_owned(),
            font_size: 64,
            origin_x: 16,
            origin_y: 16,
            color: 0xFFFF_FFFF,
            bold: false,
            align,
            outline_color: 0xFF00_0000,
            outline_width: 0,
            background,
        }
    }

    /// `top..bottom` の行で、alpha が 0 でない最も左の列。
    fn leftmost_ink(image: &[u8], width: usize, top: usize, bottom: usize) -> usize {
        (top..bottom)
            .filter_map(|y| (0..width).find(|x| image[(y * width + x) * 4 + 3] > 0))
            .min()
            .expect("行に字形がありません")
    }

    #[test]
    fn unpremultiply_restores_straight_color() {
        // 半透明の赤（premultiplied で R=64, A=128）は straight で R=128。
        let mut pixels = [64, 0, 0, 128, 255, 255, 255, 255, 9, 9, 9, 0];
        unpremultiply(&mut pixels);
        assert_eq!(pixels, [128, 0, 0, 128, 255, 255, 255, 255, 0, 0, 0, 0]);
    }

    #[test]
    #[ignore = "workstation"]
    fn translucent_background_is_returned_as_straight_alpha() {
        let image = raster(&run("A", Align::Left, 0x80FF_0000), 256, 256).expect("Meiryo");
        // block の左上の角は padding の中で、字形は無い。
        let corner = (16 * 256 + 16) * 4;
        assert_eq!(&image[corner..corner + 4], &[255, 0, 0, 128]);
    }

    #[test]
    #[ignore = "workstation"]
    fn wrapped_lines_follow_the_alignment_inside_the_block() {
        // 1 行目は 1 文字、2 行目は 3 文字。全角 1 文字は 1 em = 64 px。
        let width = 512;
        let line = 77; // 64 * 1.2 の切り上げ
        let top = 16 + 2;
        let left = raster(
            &run(
                "あ
あああ",
                Align::Left,
                0,
            ),
            512,
            256,
        )
        .expect("Meiryo");
        let center = raster(
            &run(
                "あ
あああ",
                Align::Center,
                0,
            ),
            512,
            256,
        )
        .expect("Meiryo");
        let right = raster(
            &run(
                "あ
あああ",
                Align::Right,
                0,
            ),
            512,
            256,
        )
        .expect("Meiryo");
        let shift = |image: &[u8]| {
            leftmost_ink(image, width, top, top + line) as i64
                - leftmost_ink(image, width, top + line, top + 2 * line) as i64
        };
        assert!(shift(&left).abs() < 8, "左揃え {}", shift(&left));
        assert!(
            (48..=80).contains(&shift(&center)),
            "中央揃え {}",
            shift(&center)
        );
        assert!(
            (112..=144).contains(&shift(&right)),
            "右揃え {}",
            shift(&right)
        );

        let mut subtitle = paint(
            "あ
あああ",
        );
        subtitle.align = Align::Right;
        let placed = place_subtitle(&subtitle, 1920, 1080).expect("字幕");
        assert_eq!(placed.align, Align::Right, "字幕の揃えが失われています");
    }

    #[test]
    fn rejects_subtitles_that_do_not_fit() {
        let mut paint = paint("");
        let error = place_subtitle(&paint, 1920, 1080).unwrap_err();
        assert!(error.to_string().contains("空です"), "{error}");

        paint.content = "字幕".to_owned();
        paint.side_margin = 0.6;
        let error = place_subtitle(&paint, 1920, 1080).unwrap_err();
        assert!(error.to_string().contains("余白"), "{error}");

        paint.side_margin = 0.05;
        paint.font_size = 4_000;
        let error = place_subtitle(&paint, 1920, 100).unwrap_err();
        assert!(error.to_string().contains("高さ"), "{error}");

        let error = place_subtitle(&paint, 0, 100).unwrap_err();
        assert!(error.to_string().contains("寸法"), "{error}");
    }

    fn ink(image: &[u8]) -> usize {
        image.chunks(4).filter(|pixel| pixel[3] > 128).count()
    }

    #[test]
    #[ignore = "workstation"]
    fn os_fonts_are_listed_and_drawn_by_name() {
        let families = font_families().expect("OS の書体");
        assert!(families.iter().any(|name| name == "Meiryo"), "{families:?}");
        assert!(
            families.iter().any(|name| name == "MS Gothic"),
            "{families:?}"
        );
        // 日本語の名前でも同じ書体を引ける。
        let mut gothic = run("あ", Align::Left, 0);
        gothic.font_family = "ＭＳ ゴシック".to_owned();
        let localized = raster(&gothic, 128, 128).expect("ＭＳ ゴシック");
        gothic.font_family = "MS Gothic".to_owned();
        assert_eq!(localized, raster(&gothic, 128, 128).expect("MS Gothic"));
    }

    #[test]
    #[ignore = "workstation"]
    fn a_font_sample_is_cut_to_its_block() {
        let (width, height, pixels) =
            font_sample("Meiryo", "文字もじモジ", 14, 0xFFFF_FFFF).expect("Meiryo");
        assert_eq!(pixels.len(), (width * height * 4) as usize);
        // 6 文字 × 14 px に余白 2 px ずつ。行の高さは 1.2 倍。
        assert!((84..=96).contains(&width), "{width}");
        assert_eq!(height, 21);
        assert!(ink(&pixels) > 0);
        assert!(font_sample("NotAFont", "あ", 14, 0xFFFF_FFFF).is_err());
    }

    #[test]
    #[ignore = "workstation"]
    fn bold_is_synthesized_for_a_family_without_a_bold_face() {
        // MS Gothic には太字の字形が無い。Qt と同じく字形を太らせて描く（Meiryo へ置き換えない）。
        let mut regular = run("あいう", Align::Left, 0);
        regular.font_family = "MS Gothic".to_owned();
        let mut bold = regular.clone();
        bold.bold = true;
        let thin = ink(&raster(&regular, 512, 128).expect("MS Gothic"));
        let thick = ink(&raster(&bold, 512, 128).expect("MS Gothic"));
        assert!(
            thick > thin + thin / 10,
            "太字が太くなりません: {thin} → {thick}"
        );
    }

    #[test]
    #[ignore = "workstation"]
    fn meiryo_draws_opaque_ink_and_leaves_the_far_corner_clear() {
        let image = raster(
            &GlyphRun {
                content: "A".to_owned(),
                font_family: "Meiryo".to_owned(),
                font_size: 64,
                origin_x: 16,
                origin_y: 16,
                color: 0xFFFF_FFFF,
                bold: false,
                align: Align::Left,
                outline_color: 0xFF00_0000,
                outline_width: 0,
                background: 0,
            },
            256,
            256,
        )
        .expect("Meiryo");
        assert_eq!(image.len(), 256 * 256 * 4);
        assert!(
            image.chunks(4).any(|pixel| pixel[3] > 0),
            "字形が 1 画素もありません"
        );
        assert_eq!(&image[image.len() - 4..], &[0, 0, 0, 0]);
    }
}
