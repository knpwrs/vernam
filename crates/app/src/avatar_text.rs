//! Signal Desktop's text photos (`AvatarTextEditor`), drawn as its
//! `avatarDataToBytes` draws them (`ts/util/avatarDataToBytes.dom.ts`): the
//! color filling a 1024-pixel square, the text in Inter Regular in the
//! color's foreground, centered with its middle 30 pixels below the
//! square's, at the largest size whose ink fits in three quarters of the
//! square (`getFittedFontSize`, `ts/util/avatarTextSizeCalculator.std.ts`).
//!
//! Desktop's canvas falls back to the system's fonts for what Inter lacks;
//! so does this: emoji in macOS's Apple Color Emoji (its bitmaps, as
//! Desktop's canvas draws them), and other scripts (Chinese, Hebrew, Thai…)
//! in a system font that has them, loaded only when one is needed. Each
//! character goes in a run of the font that has it, rather than leaving
//! usvg to fall back, which drops a cluster the fallback font shapes into a
//! different number of glyphs (a skin-toned emoji, a flag, a Devanagari
//! syllable). Runs aren't spans of one line either: usvg shapes a line in
//! each span's font and splices the glyphs, which panics on such a cluster
//! ("A👍🏽"). Several runs are placed one after another here, each its own
//! line, from their fonts' advances. Without a font that has a character,
//! it's left out.

use std::sync::{Arc, OnceLock};

use resvg::{tiny_skia, usvg};

/// Desktop's canvas size (`CANVAS_SIZE`).
pub const SIZE: u32 = 1024;

/// Desktop's `MAX_LENGTH`: at most three characters (graphemes).
pub const MAX_LENGTH: usize = 3;

/// Where macOS keeps its color emoji.
const EMOJI_FONT: &str = "/System/Library/Fonts/Apple Color Emoji.ttc";

/// The emoji font's family, as the runs name it.
const EMOJI_FAMILY: &str = "Apple Color Emoji";

/// Inter, and the system's color emoji when there is one, loaded once.
fn fonts() -> Arc<usvg::fontdb::Database> {
    static FONTS: OnceLock<Arc<usvg::fontdb::Database>> = OnceLock::new();
    FONTS.get_or_init(|| Arc::new(base_fonts())).clone()
}

fn base_fonts() -> usvg::fontdb::Database {
    let mut db = usvg::fontdb::Database::new();
    db.load_font_data(crate::fonts::FILES[0].to_vec());
    // Mapped, not read: the file is 192 MB.
    if let Err(error) = db.load_font_file(EMOJI_FONT) {
        tracing::info!(%error, "no color emoji font for text photos");
    }
    db
}

/// [`fonts`] and every system font, for text Inter lacks: loaded once, the
/// first time one is needed (about half a second, off the UI thread, as
/// all drawing and measuring here is).
fn all_fonts() -> Arc<usvg::fontdb::Database> {
    static FONTS: OnceLock<Arc<usvg::fontdb::Database>> = OnceLock::new();
    FONTS
        .get_or_init(|| {
            let mut db = base_fonts();
            db.load_system_fonts();
            Arc::new(db)
        })
        .clone()
}

/// Whether the font `data` holds has a glyph for every character of
/// `grapheme` (joiners and variation selectors aside).
fn has_glyphs(data: &[u8], index: u32, grapheme: &str) -> bool {
    let Ok(face) = ttf_parser::Face::parse(data, index) else { return false };
    grapheme.chars().filter(|c| !matches!(c, '\u{200d}' | '\u{fe0e}' | '\u{fe0f}')).all(|c| face.glyph_index(c).is_some())
}

/// Whether Inter has `grapheme`.
fn inter_has(grapheme: &str) -> bool {
    grapheme.chars().all(char::is_whitespace) || has_glyphs(crate::fonts::FILES[0], 0, grapheme)
}

/// The family of a system font with `grapheme`, Apple's last-resort font
/// (which draws a placeholder box for everything) aside.
fn system_family(db: &usvg::fontdb::Database, grapheme: &str) -> Option<String> {
    db.faces().find_map(|face| {
        let family = face.families.first().map(|(name, _)| name.clone())?;
        if family.contains("LastResort") {
            return None;
        }
        db.with_face_data(face.id, |data, index| has_glyphs(data, index, grapheme)).filter(|has| *has).map(|_| family)
    })
}

/// `text` in runs of one font family each, and the fonts they need.
fn runs(text: &str) -> (Vec<(String, String)>, Arc<usvg::fontdb::Database>) {
    use unicode_segmentation::UnicodeSegmentation as _;
    let graphemes: Vec<&str> = text.graphemes(true).collect();
    let is_emoji = |g: &str| signal_emoji::jumbo_count(g) == Some(1);
    let needs_system = graphemes.iter().any(|g| !is_emoji(g) && !inter_has(g));
    let db = if needs_system { all_fonts() } else { fonts() };
    let mut runs: Vec<(String, String)> = Vec::new();
    for grapheme in graphemes {
        let family = if is_emoji(grapheme) {
            EMOJI_FAMILY.to_owned()
        } else if inter_has(grapheme) {
            crate::fonts::INTER.to_owned()
        } else {
            // None has it: Inter, which leaves it out.
            system_family(&db, grapheme).unwrap_or_else(|| crate::fonts::INTER.to_owned())
        };
        match runs.last_mut() {
            Some((last, run)) if *last == family => run.push_str(grapheme),
            _ => runs.push((family, grapheme.to_owned())),
        }
    }
    (runs, db)
}

/// Desktop's `getFontSizes` for a bubble of `size`.
struct FontSizes {
    diameter: f32,
    single_emoji: f32,
    smol: f32,
    text: f32,
}

fn font_sizes(size: f32) -> FontSizes {
    FontSizes { diameter: (size * 0.75).ceil(), single_emoji: (size * 0.6).ceil(), smol: (size * 0.05).ceil(), text: (size * 0.45).ceil() }
}

/// Whether `text` is one emoji and nothing else (Desktop's
/// `Emoji.isLoneEmoji`), which starts at a larger size.
fn is_lone_emoji(text: &str) -> bool {
    use unicode_segmentation::UnicodeSegmentation as _;
    let mut graphemes = text.graphemes(true);
    let (Some(one), None) = (graphemes.next(), graphemes.next()) else { return false };
    signal_emoji::jumbo_count(one) == Some(1)
}

/// Desktop's `getFittedFontSize`: from the starting size down by one until
/// `measure`'s width and height are both under three quarters of `size`.
/// (The sizes that fit are all those below the largest that does, so this
/// searches for it rather than stepping down.)
pub fn fitted_font_size(size: f32, text: &str, measure: impl Fn(f32) -> (f32, f32)) -> f32 {
    let sizes = font_sizes(size);
    let start = if is_lone_emoji(text) { sizes.single_emoji } else { sizes.text };
    let fits = |candidate: f32| {
        let (width, height) = measure(candidate);
        width < sizes.diameter && height < sizes.diameter
    };
    if fits(start) {
        return start;
    }
    // Desktop steps down to `smol`; when nothing fits its loop has gone
    // one past it, and that's the size.
    let (mut low, mut high) = (sizes.smol as i64, start as i64 - 1);
    if !fits(low as f32) {
        return (low - 1) as f32;
    }
    while low < high {
        let mid = (low + high + 1) / 2;
        if fits(mid as f32) {
            low = mid;
        } else {
            high = mid - 1;
        }
    }
    low as f32
}

/// `family` as a quoted CSS font-family name, escaped for an attribute.
/// Quoted, as some system families start with a dot (`.ThonburiUI`), which
/// an unquoted name can't (svgtypes refuses it and usvg falls back). In
/// whichever quote the name doesn't hold: svgtypes skips an escaped quote
/// but keeps its backslash in the name.
fn family_attr(family: &str) -> String {
    let quote = if family.contains('\'') { '"' } else { '\'' };
    escaped(&format!("{quote}{family}{quote}"))
}

/// `text` escaped for an SVG text element.
fn escaped(text: &str) -> String {
    text.chars()
        .map(|c| match c {
            '&' => "&amp;".to_owned(),
            '<' => "&lt;".to_owned(),
            '>' => "&gt;".to_owned(),
            '"' => "&quot;".to_owned(),
            '\'' => "&apos;".to_owned(),
            c => c.to_string(),
        })
        .collect()
}

fn hex(rgb: u32) -> String {
    format!("#{:06x}", rgb & 0xff_ffff)
}

/// How far `text` in `family` advances at `font_size`, from the font's
/// glyph advances (no kerning: runs meet at a change of font). An emoji
/// run advances one emoji width per emoji, however many code points each
/// is (the emoji font shapes each into one glyph).
fn advance(fonts: &usvg::fontdb::Database, family: &str, text: &str, font_size: f32) -> f32 {
    use unicode_segmentation::UnicodeSegmentation as _;
    let families = [usvg::fontdb::Family::Name(family)];
    let Some(id) = fonts.query(&usvg::fontdb::Query { families: &families, ..Default::default() }) else { return 0. };
    fonts
        .with_face_data(id, |data, index| {
            let face = ttf_parser::Face::parse(data, index).ok()?;
            let scale = font_size / f32::from(face.units_per_em());
            let width = |c: char| face.glyph_index(c).and_then(|g| face.glyph_hor_advance(g)).map_or(0., f32::from);
            let units: f32 = if family == EMOJI_FAMILY {
                text.graphemes(true).map(|g| g.chars().next().map_or(0., width)).sum()
            } else {
                text.chars().map(width).sum()
            };
            Some(units * scale)
        })
        .flatten()
        .unwrap_or(0.)
}

/// The text alone, in its `runs`, at `font_size`, in `fg`: Desktop's canvas
/// `fillText` with `textAlign = 'center'`, `textBaseline = 'middle'` (the em
/// box's middle, SVG's `central`) at the square's middle plus 30. One run
/// is one centered line; several are placed one after another from their
/// advances, the whole centered (see the module's note on usvg).
fn text_svg(runs: &[(String, String)], fonts: &usvg::fontdb::Database, font_size: f32, fg: u32) -> String {
    let (middle, y) = (SIZE as f32 / 2., SIZE / 2 + 30);
    let lines: String = if let [(family, text)] = runs {
        format!("<tspan font-family=\"{}\">{}</tspan>", family_attr(family), escaped(text))
    } else {
        let widths: Vec<f32> = runs.iter().map(|(family, text)| advance(fonts, family, text, font_size)).collect();
        let mut x = middle - widths.iter().sum::<f32>() / 2.;
        runs.iter()
            .zip(&widths)
            .map(|((family, text), width)| {
                let line = format!("<tspan x=\"{x}\" y=\"{y}\" font-family=\"{}\">{}</tspan>", family_attr(family), escaped(text));
                x += width;
                line
            })
            .collect()
    };
    let anchor = if runs.len() == 1 { "middle" } else { "start" };
    format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{SIZE}\" height=\"{SIZE}\"><text x=\"{middle}\" y=\"{y}\" \
         font-size=\"{font_size}\" text-anchor=\"{anchor}\" dominant-baseline=\"central\" fill=\"{fill}\">{lines}</text></svg>",
        fill = hex(fg),
    )
}

fn parse(svg: &str, fonts: Arc<usvg::fontdb::Database>) -> anyhow::Result<usvg::Tree> {
    let options = usvg::Options { fontdb: fonts, font_family: crate::fonts::INTER.into(), ..Default::default() };
    Ok(usvg::Tree::from_str(svg, &options)?)
}

/// The ink's width and height at `font_size` (Desktop's `measureText`
/// `actualBoundingBox`), 0 when nothing draws.
fn measure(runs: &[(String, String)], fonts: &Arc<usvg::fontdb::Database>, font_size: f32) -> (f32, f32) {
    match parse(&text_svg(runs, fonts, font_size, 0), fonts.clone()) {
        Ok(tree) => {
            let bounds = tree.root().abs_bounding_box();
            (bounds.width(), bounds.height())
        }
        Err(_) => (0., 0.),
    }
}

/// The font size `text` gets in a bubble of `size` (Desktop's editor fits
/// its input the same way, at 120 pixels).
pub fn fitted_size(text: &str, size: f32) -> f32 {
    let (runs, fonts) = runs(text);
    fitted_font_size(size, text, |candidate| measure(&runs, &fonts, candidate))
}

/// The text photo: `text` (at most three characters) on `bg`, in `fg`, as
/// a 1024-pixel PNG.
pub fn render(text: &str, bg: u32, fg: u32) -> anyhow::Result<Vec<u8>> {
    let (runs, fonts) = runs(text);
    let font_size = fitted_font_size(SIZE as f32, text, |candidate| measure(&runs, &fonts, candidate));
    let tree = parse(&text_svg(&runs, &fonts, font_size, fg), fonts.clone())?;
    let mut pixmap = tiny_skia::Pixmap::new(SIZE, SIZE).ok_or_else(|| anyhow::anyhow!("no pixmap"))?;
    pixmap.fill(tiny_skia::Color::from_rgba8((bg >> 16) as u8, (bg >> 8) as u8, bg as u8, 255));
    resvg::render(&tree, tiny_skia::Transform::identity(), &mut pixmap.as_mut());
    Ok(pixmap.encode_png()?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pixels(png: &[u8]) -> image::RgbaImage {
        image::load_from_memory(png).unwrap().to_rgba8()
    }

    #[test]
    fn text_is_drawn_centered_in_the_foreground_on_the_background() {
        let png = render("AB", 0x00ff00, 0xff0000).unwrap();
        let image = pixels(&png);
        assert_eq!(image.dimensions(), (SIZE, SIZE));
        assert_eq!(image.get_pixel(0, 0).0, [0, 0xff, 0, 0xff], "the background");
        let red: Vec<(u32, u32)> = image.enumerate_pixels().filter(|(_, _, p)| p.0 == [0xff, 0, 0, 0xff]).map(|(x, y, _)| (x, y)).collect();
        assert!(!red.is_empty(), "the text");
        // Centered across, and its middle a little below the square's.
        let (min_x, max_x) = (red.iter().map(|p| p.0).min().unwrap(), red.iter().map(|p| p.0).max().unwrap());
        let middle = (min_x + max_x) / 2;
        assert!(middle.abs_diff(SIZE / 2) < 20, "centered: {middle}");
        // Within three quarters of the square.
        assert!(max_x - min_x < 768);
    }

    #[test]
    fn the_size_fits_as_desktops_does() {
        // Short text starts at 45% (461), and fits.
        assert_eq!(fitted_font_size(1024., "A", |s| (s * 0.6, s * 0.7)), 461.);
        // Wide text steps down to the largest size under 768.
        let size = fitted_font_size(1024., "WWW", |s| (s * 2.8, s * 0.7));
        assert!(size * 2.8 < 768. && (size + 1.) * 2.8 >= 768., "{size}");
        // Nothing fits: Desktop's loop ends one past 5% (52).
        assert_eq!(fitted_font_size(1024., "A", |_| (2000., 2000.)), 51.);
        // One emoji starts at 60%.
        assert_eq!(fitted_font_size(1024., "😀", |s| (s * 0.5, s * 0.5)), 615.);
    }

    #[test]
    fn an_emoji_is_drawn_in_color_where_the_system_has_the_font() {
        if !std::path::Path::new(EMOJI_FONT).exists() {
            return;
        }
        let png = render("😀", 0x000000, 0xffffff).unwrap();
        let image = pixels(&png);
        // The grinning face's yellow, not the white text color or black.
        let yellow = image.pixels().filter(|p| p.0[0] > 200 && p.0[1] > 150 && p.0[2] < 100).count();
        assert!(yellow > 1000, "{yellow} yellow pixels");
    }

    /// Pixels neither the background (black) nor the text color (white):
    /// a color emoji's.
    fn colored(image: &image::RgbaImage) -> usize {
        // Skin tones are muted: a channel apart by more than 25.
        image.pixels().filter(|p| p.0[0].abs_diff(p.0[1]) > 25 || p.0[1].abs_diff(p.0[2]) > 25).count()
    }

    fn inked(image: &image::RgbaImage) -> usize {
        image.pixels().filter(|p| p.0[0] > 128).count()
    }

    #[test]
    fn other_scripts_are_drawn_in_a_system_font_that_has_them() {
        // macOS has fonts for these; elsewhere there may be none.
        if !cfg!(target_os = "macos") {
            return;
        }
        for text in ["李", "ש", "รัก"] {
            let png = render(text, 0x000000, 0xffffff).unwrap();
            assert!(inked(&pixels(&png)) > 2000, "{text} drawn");
        }
        // Inter's own stay in Inter, without loading the system's fonts.
        let (runs, _) = runs("AB");
        assert_eq!(runs, [(crate::fonts::INTER.to_owned(), "AB".to_owned())]);
    }

    #[test]
    fn an_emoji_of_several_code_points_stays_beside_text() {
        if !std::path::Path::new(EMOJI_FONT).exists() {
            return;
        }
        for text in ["A👍🏽", "A🇺🇸"] {
            let (runs, _) = runs(text);
            assert_eq!(runs.len(), 2, "{text}: Inter, then the emoji font");
            assert_eq!(runs[1].0, EMOJI_FAMILY);
            let png = render(text, 0x000000, 0xffffff).unwrap();
            let image = pixels(&png);
            assert!(colored(&image) > 1000, "{text}: the emoji is drawn");
            // The two runs together, centered.
            let xs: Vec<u32> = image.enumerate_pixels().filter(|(_, _, p)| p.0[0] > 60 || p.0[1] > 60 || p.0[2] > 60).map(|(x, _, _)| x).collect();
            let middle = (xs.iter().min().unwrap() + xs.iter().max().unwrap()) / 2;
            assert!(middle.abs_diff(SIZE / 2) < 40, "{text} centered: {middle}");
        }
    }

    /// The family of the font each glyph of `runs` is laid out in.
    fn drawn_in(runs: &[(String, String)], db: &Arc<usvg::fontdb::Database>) -> Vec<String> {
        let tree = parse(&text_svg(runs, db, 200., 0), db.clone()).unwrap();
        let mut families = Vec::new();
        for node in tree.root().children() {
            if let usvg::Node::Text(text) = node {
                for span in text.layouted() {
                    for glyph in &span.positioned_glyphs {
                        let family = db.face(glyph.font).and_then(|f| f.families.first().map(|(n, _)| n.clone()));
                        families.extend(family);
                    }
                }
            }
        }
        families
    }

    #[test]
    fn a_family_named_with_a_dot_is_drawn_in_its_own_face() {
        // macOS's hidden families start with a dot (`.ThonburiUI`); an
        // unquoted name can't, and usvg fell back to Inter.
        let db = all_fonts();
        // A dot-named face and a letter it has, from a few scripts.
        let letters = ('A'..='Z').chain(['ก', 'ש', 'ب', 'क', '李']);
        let found = db.faces().find_map(|f| {
            let family = f.families.first().map(|(n, _)| n.clone()).filter(|n| n.starts_with('.'))?;
            let letter = letters.clone().find(|c| db.with_face_data(f.id, |data, index| has_glyphs(data, index, &c.to_string())).unwrap_or(false))?;
            Some((family, letter))
        });
        let Some((family, letter)) = found else {
            eprintln!("no font family named with a dot has a letter here; skipped");
            return;
        };
        for runs in [vec![(family.clone(), letter.to_string())], vec![(crate::fonts::INTER.to_owned(), "A".to_owned()), (family.clone(), letter.to_string())]] {
            let drawn = drawn_in(&runs, &db);
            assert_eq!(drawn.last(), Some(&family), "{runs:?} drawn in {drawn:?}");
        }
        assert_eq!(family_attr(".SF Hebrew"), "&apos;.SF Hebrew&apos;");
        assert_eq!(family_attr("O'Font"), "&quot;O&apos;Font&quot;");
    }

    #[test]
    fn markup_in_the_text_is_text() {
        assert_eq!(escaped("<a&'\">"), "&lt;a&amp;&apos;&quot;&gt;");
        assert!(render("<&>", 0, 0xffffff).is_ok());
    }
}
