//! Signal Desktop's text photos (`AvatarTextEditor`), drawn as its
//! `avatarDataToBytes` draws them (`ts/util/avatarDataToBytes.dom.ts`): the
//! color filling a 1024-pixel square, the text in Inter Regular in the
//! color's foreground, centered with its middle 30 pixels below the
//! square's, at the largest size whose ink fits in three quarters of the
//! square (`getFittedFontSize`, `ts/util/avatarTextSizeCalculator.std.ts`).
//!
//! Desktop's canvas falls back to the system's emoji font for emoji; so does
//! this, with macOS's Apple Color Emoji (its bitmaps, as Desktop's canvas
//! draws them). Without it, emoji have no glyph and are left out.

use std::sync::{Arc, OnceLock};

use resvg::{tiny_skia, usvg};

/// Desktop's canvas size (`CANVAS_SIZE`).
pub const SIZE: u32 = 1024;

/// Desktop's `MAX_LENGTH`: at most three characters (graphemes).
pub const MAX_LENGTH: usize = 3;

/// Where macOS keeps its color emoji.
const EMOJI_FONT: &str = "/System/Library/Fonts/Apple Color Emoji.ttc";

/// Inter, and the system's color emoji when there is one, loaded once.
fn fonts() -> Arc<usvg::fontdb::Database> {
    static FONTS: OnceLock<Arc<usvg::fontdb::Database>> = OnceLock::new();
    FONTS
        .get_or_init(|| {
            let mut db = usvg::fontdb::Database::new();
            db.load_font_data(crate::fonts::FILES[0].to_vec());
            // Mapped, not read: the file is 192 MB.
            if let Err(error) = db.load_font_file(EMOJI_FONT) {
                tracing::info!(%error, "no color emoji font for text photos");
            }
            Arc::new(db)
        })
        .clone()
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
    // Desktop steps down to `smol`, and keeps it if nothing fits.
    let (mut low, mut high) = (sizes.smol as i64, start as i64 - 1);
    if !fits(low as f32) {
        return low as f32;
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

/// The text alone, at `font_size`, in `fg`: Desktop's canvas `fillText` with
/// `textAlign = 'center'`, `textBaseline = 'middle'` (the em box's middle,
/// SVG's `central`) at the square's middle plus 30.
fn text_svg(text: &str, font_size: f32, fg: u32) -> String {
    format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{SIZE}\" height=\"{SIZE}\"><text x=\"{x}\" y=\"{y}\" \
         font-family=\"Inter, Apple Color Emoji\" font-size=\"{font_size}\" text-anchor=\"middle\" \
         dominant-baseline=\"central\" fill=\"{fill}\">{text}</text></svg>",
        x = SIZE / 2,
        y = SIZE / 2 + 30,
        fill = hex(fg),
        text = escaped(text),
    )
}

fn parse(svg: &str) -> anyhow::Result<usvg::Tree> {
    let options = usvg::Options { fontdb: fonts(), font_family: crate::fonts::INTER.into(), ..Default::default() };
    Ok(usvg::Tree::from_str(svg, &options)?)
}

/// The ink's width and height at `font_size` (Desktop's `measureText`
/// `actualBoundingBox`), 0 when nothing draws.
fn measure(text: &str, font_size: f32) -> (f32, f32) {
    match parse(&text_svg(text, font_size, 0)) {
        Ok(tree) => {
            let bounds = tree.root().abs_bounding_box();
            (bounds.width(), bounds.height())
        }
        Err(_) => (0., 0.),
    }
}

/// The text photo: `text` (at most three characters) on `bg`, in `fg`, as
/// a 1024-pixel PNG.
pub fn render(text: &str, bg: u32, fg: u32) -> anyhow::Result<Vec<u8>> {
    let font_size = fitted_font_size(SIZE as f32, text, |candidate| measure(text, candidate));
    let tree = parse(&text_svg(text, font_size, fg))?;
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
        // Nothing fits: Desktop ends at 5%.
        assert_eq!(fitted_font_size(1024., "A", |_| (2000., 2000.)), 52.);
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

    #[test]
    fn markup_in_the_text_is_text() {
        assert_eq!(escaped("<a&'\">"), "&lt;a&amp;&apos;&quot;&gt;");
        assert!(render("<&>", 0, 0xffffff).is_ok());
    }
}
