//! Renders prompt key caps outside the game (`victory key-caps <folder>`):
//! dark-ground PNG (256 px), transparent PNG (512 px), SVG, and an HTML
//! preview gallery. Supports exporting the live/custom `native-keys.txt`
//! layout, the classic pad layout (`--pad`), or the complete PC glyph matrix
//! (`--all`, covering every key category and modifier combination).

use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use victory_core::pad::{KeySpec, PadControl};
use victory_formats::gfx::{Contour, Edge};

use super::bindings::PromptBindings;
use super::font::Fonts;
use super::{
    BaseKey, CELL_TWIPS, Drawing, Glyph, ICONS, Key, Press, characters, draw, key_label, keys_of,
    modifier_badge,
};

const GROUND_PIXELS: usize = 256;
const TRANSPARENT_PIXELS: usize = 512;
const GROUND: [u8; 3] = [0x2B, 0x2B, 0x33];
const SAMPLES: usize = 4;
const CURVE_LINES: usize = 16;

/// One rendered key-cap picture.
pub struct Picture {
    pub number: u32,
    pub label: String,
    pub(crate) keys: Vec<Key>,
    drawing: Drawing,
}

/// Renders every icon of the title's icon movie for `bindings`.
#[must_use]
pub fn pictures_for(bindings: &PromptBindings) -> Vec<Picture> {
    let fonts = Fonts::open();
    let mut glyphs: HashMap<char, Glyph> = HashMap::new();
    let mut pictures = Vec::new();
    for &(number, controls) in ICONS {
        let Some(keys) = keys_of(controls, bindings) else {
            continue;
        };
        for character in characters(&keys) {
            if !glyphs.contains_key(&character)
                && let Some(glyph) = fonts.glyph(character)
            {
                glyphs.insert(character, glyph);
            }
        }
        let Some(drawing) = draw(&keys, &glyphs) else {
            continue;
        };
        pictures.push(Picture {
            number,
            label: control_name(controls),
            keys,
            drawing,
        });
    }
    pictures
}

/// Renders the complete PC key-cap glyph matrix covering every supported
/// category (letters, digits, F1–F12, modifiers, core keys, arrows & nav,
/// numpad, punctuation, mouse, and modifier combinations).
#[must_use]
pub fn all_glyph_pictures() -> Vec<Picture> {
    let specs: &[(&str, &str)] = &[
        ("Letter A", "A"),
        ("Letter W", "W"),
        ("Letter E", "E"),
        ("Letter Q", "Q"),
        ("Letter R", "R"),
        ("Letter F", "F"),
        ("Digit 0", "0"),
        ("Digit 1", "1"),
        ("Digit 5", "5"),
        ("Digit 9", "9"),
        ("Function F1", "F1"),
        ("Function F5", "F5"),
        ("Function F10", "F10"),
        ("Function F12", "F12"),
        ("Modifier Shift", "Shift"),
        ("Modifier RightShift", "RightShift"),
        ("Modifier Ctrl", "Ctrl"),
        ("Modifier RightCtrl", "RightCtrl"),
        ("Modifier Alt", "Alt"),
        ("Modifier RightAlt", "RightAlt"),
        ("Core Enter", "Enter"),
        ("Core Escape", "Esc"),
        ("Core Tab", "Tab"),
        ("Core Backspace", "Backspace"),
        ("Core Space", "Space"),
        ("Core CapsLock", "CapsLock"),
        ("Arrow Up", "ArrowUp"),
        ("Arrow Down", "ArrowDown"),
        ("Arrow Left", "ArrowLeft"),
        ("Arrow Right", "ArrowRight"),
        ("Nav Insert", "Insert"),
        ("Nav Delete", "Delete"),
        ("Nav Home", "Home"),
        ("Nav End", "End"),
        ("Nav PageUp", "PageUp"),
        ("Nav PageDown", "PageDown"),
        ("Numpad 0", "Numpad0"),
        ("Numpad 4", "Numpad4"),
        ("Numpad 8", "Numpad8"),
        ("Numpad Add", "NumpadAdd"),
        ("Numpad Sub", "NumpadSub"),
        ("Numpad Mul", "NumpadMul"),
        ("Numpad Div", "NumpadDiv"),
        ("Numpad Decimal", "NumpadDecimal"),
        ("Punct Comma", "Comma"),
        ("Punct Period", "Period"),
        ("Punct Minus", "Minus"),
        ("Punct Equals", "Equals"),
        ("Punct LeftBracket", "LeftBracket"),
        ("Punct RightBracket", "RightBracket"),
        ("Punct Semicolon", "Semicolon"),
        ("Punct Quote", "Quote"),
        ("Punct Slash", "Slash"),
        ("Punct Backslash", "Backslash"),
        ("Punct Backquote", "Backquote"),
        ("Mouse Move", "Mouse"),
        ("Mouse Left", "LeftClick"),
        ("Mouse Right", "RightClick"),
        ("Mouse Middle", "MiddleClick"),
        ("Mouse 4", "Mouse4"),
        ("Mouse 5", "Mouse5"),
        ("Mouse WheelUp", "WheelUp"),
        ("Mouse WheelDown", "WheelDown"),
        ("Combo Ctrl+E", "Ctrl+E"),
        ("Combo Shift+Space", "Shift+Space"),
        ("Combo Alt+F4", "Alt+F4"),
        ("Combo Ctrl+Shift+R", "Ctrl+Shift+R"),
        ("Combo Ctrl+LeftClick", "Ctrl+LeftClick"),
    ];
    let fonts = Fonts::open();
    let mut glyphs: HashMap<char, Glyph> = HashMap::new();
    let mut out = Vec::new();
    for (idx, &(label, token)) in specs.iter().enumerate() {
        let Some(spec) = KeySpec::parse(token) else {
            continue;
        };
        let Some(key) = key_label(spec) else {
            continue;
        };
        let keys = vec![key];
        for c in characters(&keys) {
            if !glyphs.contains_key(&c)
                && let Some(g) = fonts.glyph(c)
            {
                glyphs.insert(c, g);
            }
        }
        let Some(drawing) = draw(&keys, &glyphs) else {
            continue;
        };
        out.push(Picture {
            number: (idx + 1) as u32,
            label: label.to_owned(),
            keys,
            drawing,
        });
    }
    out
}

fn part(control: PadControl) -> (&'static str, &'static str) {
    use PadControl as C;
    match control {
        C::Menu => ("Menu", ""),
        C::View => ("View", ""),
        C::A => ("A", ""),
        C::B => ("B", ""),
        C::X => ("X", ""),
        C::Y => ("Y", ""),
        C::Up => ("D-pad", "Up"),
        C::Down => ("D-pad", "Down"),
        C::Left => ("D-pad", "Left"),
        C::Right => ("D-pad", "Right"),
        C::LeftShoulder => ("LB", ""),
        C::RightShoulder => ("RB", ""),
        C::LeftTrigger => ("LT", ""),
        C::RightTrigger => ("RT", ""),
        C::LeftStick => ("Left stick", "click"),
        C::RightStick => ("Right stick", "click"),
        C::LeftStickUp => ("Left stick", "Up"),
        C::LeftStickDown => ("Left stick", "Down"),
        C::LeftStickLeft => ("Left stick", "Left"),
        C::LeftStickRight => ("Left stick", "Right"),
        C::RightStickUp => ("Right stick", "Up"),
        C::RightStickDown => ("Right stick", "Down"),
        C::RightStickLeft => ("Right stick", "Left"),
        C::RightStickRight => ("Right stick", "Right"),
    }
}

#[must_use]
pub fn control_name(controls: &[PadControl]) -> String {
    let parts: Vec<(&str, &str)> = controls.iter().map(|&c| part(c)).collect();
    let Some(&(group, _)) = parts.first() else {
        return String::new();
    };
    if parts.iter().all(|&(g, _)| g == group) {
        let ways: Vec<&str> = parts
            .iter()
            .map(|&(_, way)| way)
            .filter(|way| !way.is_empty())
            .collect();
        if ways.is_empty() || ways.len() == 4 {
            return group.to_owned();
        }
        return format!("{group} {}", ways.join("/"));
    }
    parts
        .iter()
        .map(|&(g, way)| {
            if way.is_empty() {
                g.to_owned()
            } else {
                format!("{g} {way}")
            }
        })
        .collect::<Vec<_>>()
        .join(" + ")
}

fn base_text(base: BaseKey) -> &'static str {
    match base {
        BaseKey::Text(t) => t,
        BaseKey::Mouse(Press::Moves) => "Mouse",
        BaseKey::Mouse(Press::Left) => "LMB",
        BaseKey::Mouse(Press::Right) => "RMB",
        BaseKey::Mouse(Press::Wheel) => "MMB",
        BaseKey::Mouse(Press::WheelUp) => "WheelUp",
        BaseKey::Mouse(Press::WheelDown) => "WheelDown",
    }
}

fn key_text(key: Key) -> String {
    match key {
        Key::Single(base) => base_text(base).to_owned(),
        Key::Combo { modifiers, base } => {
            format!("{}+{}", modifier_badge(modifiers), base_text(base))
        }
    }
}

fn sanitize_filename(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            '\u{21B5}' => return "Enter".to_owned(),
            '\u{232B}' => return "Backspace".to_owned(),
            '\u{2190}' => return "Left".to_owned(),
            '\u{2191}' => return "Up".to_owned(),
            '\u{2192}' => return "Right".to_owned(),
            '\u{2193}' => return "Down".to_owned(),
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => "_".to_owned(),
            other => other.to_string(),
        })
        .collect()
}

impl Picture {
    #[must_use]
    pub fn file_stem(&self) -> String {
        let keys: Vec<String> = self
            .keys
            .iter()
            .map(|&k| sanitize_filename(&key_text(k)))
            .collect();
        format!(
            "{:02} - {} - {}",
            self.number,
            sanitize_filename(&self.label.replace('/', "+")),
            keys.join(" ")
        )
    }
}

#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    reason = "pixels of a small picture"
)]
fn coverage(contours: &[Contour], width: usize, height: usize, from_x: i32) -> Vec<f32> {
    let (columns, rows) = (width * SAMPLES, height * SAMPLES);
    let scale = rows as f64 / f64::from(CELL_TWIPS);
    let at = |x: i32, y: i32| (f64::from(x - from_x) * scale, f64::from(y) * scale);
    let mut lines: Vec<[f64; 4]> = Vec::new();
    for contour in contours {
        let mut from = at(contour.start.x, contour.start.y);
        for edge in &contour.edges {
            match *edge {
                Edge::Line(to) => {
                    let to = at(to.x, to.y);
                    lines.push([from.0, from.1, to.0, to.1]);
                    from = to;
                }
                Edge::Curve { control, to } => {
                    let (c, end) = (at(control.x, control.y), at(to.x, to.y));
                    let start = from;
                    for step in 1..=CURVE_LINES {
                        let t = step as f64 / CURVE_LINES as f64;
                        let u = 1.0 - t;
                        let point = (
                            u * u * start.0 + 2.0 * u * t * c.0 + t * t * end.0,
                            u * u * start.1 + 2.0 * u * t * c.1 + t * t * end.1,
                        );
                        lines.push([from.0, from.1, point.0, point.1]);
                        from = point;
                    }
                }
            }
        }
    }
    let mut hits = vec![0u32; width * height];
    let mut crossings: Vec<f64> = Vec::new();
    for row in 0..rows {
        let y = row as f64 + 0.5;
        crossings.clear();
        for &[x0, y0, x1, y1] in &lines {
            if (y0 <= y) != (y1 <= y) {
                crossings.push(x0 + (y - y0) * (x1 - x0) / (y1 - y0));
            }
        }
        crossings.sort_by(f64::total_cmp);
        for span in crossings.chunks_exact(2) {
            let first = (span[0] - 0.5).ceil().max(0.0) as usize;
            let end = ((span[1] - 0.5).ceil().max(0.0) as usize).min(columns);
            for column in first..end {
                hits[(row / SAMPLES) * width + column / SAMPLES] += 1;
            }
        }
    }
    let all = (SAMPLES * SAMPLES) as f32;
    hits.into_iter().map(|hit| hit as f32 / all).collect()
}

fn extent(drawing: &Drawing) -> (i32, i32) {
    let xs = || drawing.caps.iter().flat_map(Contour::points).map(|p| p.x);
    let margin = CELL_TWIPS - drawing.width.min(CELL_TWIPS) + 50;
    (
        xs().min().map_or(0, |x| (x - margin).min(0)),
        xs().max()
            .map_or(drawing.width, |x| (x + margin).max(drawing.width)),
    )
}

#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "a small picture"
)]
fn width_for(drawing: &Drawing, height: usize) -> usize {
    let (from, to) = extent(drawing);
    (f64::from(to - from) * height as f64 / f64::from(CELL_TWIPS)).ceil() as usize
}

#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "0 to 255"
)]
fn rgba(drawing: &Drawing, height: usize, ground: Option<[u8; 3]>) -> (usize, Vec<u8>) {
    let width = width_for(drawing, height);
    let from = extent(drawing).0;
    let caps = coverage(&drawing.caps, width, height, from);
    let marks = coverage(&drawing.marks, width, height, from);
    let mut out = Vec::with_capacity(width * height * 4);
    for (&c, &m) in caps.iter().zip(&marks) {
        match ground {
            Some(ground) => {
                for channel in ground {
                    let value = (f32::from(channel) * (1.0 - c) + 255.0 * c) * (1.0 - m);
                    out.push(value.round() as u8);
                }
                out.push(0xFF);
            }
            None => {
                let alpha = m + c * (1.0 - m);
                let white = if alpha > 0.0 {
                    255.0 * c * (1.0 - m) / alpha
                } else {
                    0.0
                };
                let value = white.round() as u8;
                out.extend([value, value, value, (alpha * 255.0).round() as u8]);
            }
        }
    }
    (width, out)
}

#[expect(clippy::cast_possible_truncation, reason = "a small picture")]
fn png(width: usize, height: usize, rgba: &[u8]) -> Vec<u8> {
    let row = width * 4;
    let mut raw = Vec::with_capacity((row + 1) * height);
    for line in rgba.chunks_exact(row) {
        raw.push(0);
        raw.extend_from_slice(line);
    }
    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut header = Vec::new();
    header.extend((width as u32).to_be_bytes());
    header.extend((height as u32).to_be_bytes());
    header.extend([8, 6, 0, 0, 0]);
    chunk(&mut out, b"IHDR", &header);
    let mut compressed = vec![0u8; zlib_rs::compress_bound(raw.len())];
    let (data, _) = zlib_rs::compress_slice(&mut compressed, &raw, zlib_rs::DeflateConfig::new(9));
    chunk(&mut out, b"IDAT", data);
    chunk(&mut out, b"IEND", &[]);
    out
}

#[expect(clippy::cast_possible_truncation, reason = "a chunk of a small picture")]
fn chunk(png: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    png.extend((data.len() as u32).to_be_bytes());
    let start = png.len();
    png.extend(kind);
    png.extend(data);
    let crc = crc32(&png[start..]);
    png.extend(crc.to_be_bytes());
}

fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &byte in bytes {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xedb8_8320 & (crc & 1).wrapping_neg());
        }
    }
    !crc
}

fn path(contours: &[Contour]) -> String {
    let mut d = String::new();
    for contour in contours {
        let _ = write!(d, "M{} {}", contour.start.x, contour.start.y);
        for edge in &contour.edges {
            let _ = match *edge {
                Edge::Line(to) => write!(d, "L{} {}", to.x, to.y),
                Edge::Curve { control, to } => {
                    write!(d, "Q{} {} {} {}", control.x, control.y, to.x, to.y)
                }
            };
        }
        d.push('Z');
    }
    d
}

fn svg(drawing: &Drawing) -> String {
    format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"{from} 0 {width} {cell}\" width=\"{px_w}\" height=\"{px}\">\
         <path fill=\"#fff\" fill-rule=\"evenodd\" d=\"{caps}\"/><path fill=\"#000\" fill-rule=\"evenodd\" d=\"{marks}\"/></svg>\n",
        from = extent(drawing).0,
        width = extent(drawing).1 - extent(drawing).0,
        cell = CELL_TWIPS,
        px_w = width_for(drawing, TRANSPARENT_PIXELS),
        px = TRANSPARENT_PIXELS,
        caps = path(&drawing.caps),
        marks = path(&drawing.marks),
    )
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn index(pictures: &[Picture], title: &str) -> String {
    let mut cards = String::new();
    for picture in pictures {
        let stem = picture.file_stem();
        let keys: Vec<String> = picture.keys.iter().map(|&k| key_text(k)).collect();
        let _ = write!(
            cards,
            "<figure><img src=\"{src}.png\" alt=\"\"><figcaption><b>{number}</b> &middot; {label}<br>\
             <span>{keys}</span></figcaption></figure>\n",
            src = escape(&stem).replace(' ', "%20"),
            number = picture.number,
            label = escape(&picture.label),
            keys = escape(&keys.join(" ")),
        );
    }
    format!(
        "<!doctype html>\n<html lang=\"en\"><head><meta charset=\"utf-8\"><title>{title}</title>\
<style>body{{background:#1d1d22;color:#e8e8ee;font:15px system-ui,sans-serif;margin:24px}}\
h1{{font-size:22px}}p{{max-width:72em;color:#b8b8c4}}main{{display:flex;flex-wrap:wrap;gap:14px}}\
figure{{margin:0;background:#2b2b33;border-radius:10px;padding:10px 14px;text-align:center;min-width:140px}}\
img{{height:96px}}figcaption{{font-size:13px;margin-top:6px}}span{{color:#9fd3ff}}</style></head><body>\n\
<h1>{title} ({count})</h1>\n\
<p>Every glyph is rendered using the exact same Scaleform vector cap layout, host font outlines, and geometric mouse icon injected into the game's icon movie. Files: dark-ground PNG here, 512px transparent PNG in <i>transparent/</i>, and vector SVG in <i>svg/</i>.</p>\n\
<main>\n{cards}</main></body></html>\n",
        title = escape(title),
        count = pictures.len(),
    )
}

/// Writes `pictures` to `folder` (dark PNG, `transparent/` PNG, `svg/`, `index.html`).
pub fn write_pictures(folder: &Path, pictures: &[Picture], title: &str) -> std::io::Result<Vec<PathBuf>> {
    let transparent = folder.join("transparent");
    let vectors = folder.join("svg");
    for dir in [folder, &transparent, &vectors] {
        std::fs::create_dir_all(dir)?;
        for entry in std::fs::read_dir(dir)? {
            let path = entry?.path();
            let old = path.extension().is_some_and(|ext| {
                ext.eq_ignore_ascii_case("png") || ext.eq_ignore_ascii_case("svg")
            });
            if old && path.is_file() {
                std::fs::remove_file(&path)?;
            }
        }
    }
    let mut written = Vec::new();
    let mut put = |path: PathBuf, bytes: &[u8]| -> std::io::Result<()> {
        std::fs::write(&path, bytes)?;
        written.push(path);
        Ok(())
    };
    for picture in pictures {
        let stem = picture.file_stem();
        let (width, pixels) = rgba(&picture.drawing, GROUND_PIXELS, Some(GROUND));
        put(
            folder.join(format!("{stem}.png")),
            &png(width, GROUND_PIXELS, &pixels),
        )?;
        let (width, pixels) = rgba(&picture.drawing, TRANSPARENT_PIXELS, None);
        put(
            transparent.join(format!("{stem}.png")),
            &png(width, TRANSPARENT_PIXELS, &pixels),
        )?;
        put(
            vectors.join(format!("{stem}.svg")),
            svg(&picture.drawing).as_bytes(),
        )?;
    }
    put(folder.join("index.html"), index(pictures, title).as_bytes())?;
    Ok(written)
}

/// Convenience wrapper exporting prompt icons for `bindings`.
pub fn write(folder: &Path, bindings: &PromptBindings) -> std::io::Result<Vec<PathBuf>> {
    let pictures = pictures_for(bindings);
    write_pictures(folder, &pictures, "Vertex Keyboard Prompt Glyphs")
}

/// Exports the full PC key-cap glyph matrix (`victory key-caps --all`).
pub fn write_all(folder: &Path) -> std::io::Result<Vec<PathBuf>> {
    let pictures = all_glyph_pictures();
    write_pictures(folder, &pictures, "Vertex Full PC Keyboard Glyph Matrix")
}
