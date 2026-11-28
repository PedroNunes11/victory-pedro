//! Keyboard key caps in place of the title's gamepad button pictures.
//!
//! The title draws every control prompt as a sprite of one Scaleform GFx
//! vector icon movie, picked by icon number. [`key_caps_with`] redraws that
//! movie using the player's live [`bindings::PromptBindings`] (from
//! `native-keys.txt` or `keys.txt`), supporting every PC key category
//! (letters, digits, F1–F12, modifiers, Enter/Esc/Tab/Backspace/Space,
//! arrows/navigation, numpad, punctuation, mouse buttons/wheel/motion, and
//! modifier combinations such as `Ctrl+E` or `Shift+Space`).
//!
//! [`hook`] hands the redrawn movie to the title and responds dynamically to
//! binding reloads and keyboard/controller device switching.

use std::collections::{HashMap, HashSet};

use victory_core::pad::{
    KeySpec, MOUSE_MOVES, MOUSE_WHEEL_DOWN, MOUSE_WHEEL_UP, Modifiers, PadControl, VK_ADD,
    VK_BACK, VK_CAPITAL, VK_CONTROL, VK_DECIMAL, VK_DELETE, VK_DIVIDE, VK_DOWN, VK_END,
    VK_ESCAPE, VK_F1, VK_F12, VK_HOME, VK_INSERT, VK_LBUTTON, VK_LCONTROL, VK_LEFT, VK_LMENU,
    VK_LSHIFT, VK_MBUTTON, VK_MENU, VK_MULTIPLY, VK_NEXT, VK_NUMPAD0, VK_NUMPAD9, VK_OEM_1,
    VK_OEM_2, VK_OEM_3, VK_OEM_4, VK_OEM_5, VK_OEM_6, VK_OEM_7, VK_OEM_COMMA, VK_OEM_MINUS,
    VK_OEM_PERIOD, VK_OEM_PLUS, VK_PRIOR, VK_RBUTTON, VK_RCONTROL, VK_RETURN, VK_RIGHT, VK_RMENU,
    VK_RSHIFT, VK_SHIFT, VK_SPACE, VK_SUBTRACT, VK_TAB, VK_UP, VK_XBUTTON1, VK_XBUTTON2,
};
use victory_formats::gfx::{
    self, Change, Contour, Depth, Edge, Movie, Placement, Point, ShapeBody, Sprite,
};

pub mod actions;
pub mod bindings;
#[cfg(windows)]
pub mod export;
#[cfg(windows)]
pub(crate) mod font;
#[cfg(windows)]
pub mod hook;

use bindings::PromptBindings;

/// An offset from an image's base to an 8-byte pointer.
#[derive(Debug, Clone, Copy)]
pub struct PointerOffset(pub u32);

/// An offset from an image's base to a function's first instruction.
#[derive(Debug, Clone, Copy)]
pub struct CodeOffset(pub u32);

/// Where a build keeps what the hook needs.
#[derive(Debug, Clone, Copy)]
pub struct HookOffsets {
    /// The pointer through which the UI runtime calls the function that
    /// builds a movie's address from a request.
    pub movie_address_builder: PointerOffset,
    /// The function that assigns C text to a UI runtime string.
    pub string_assign: CodeOffset,
}

/// The icon numbers and the gamepad controls each one pictures: the
/// numbers in the class names of the icon movie's sprites (Series 1.0.2.1),
/// matched to the pictures the title's own icons show.
pub const ICONS: &[(u32, &[PadControl])] = {
    use PadControl as C;
    const LEFT: &[PadControl] = &[
        C::LeftStickUp,
        C::LeftStickLeft,
        C::LeftStickDown,
        C::LeftStickRight,
    ];
    const RIGHT: &[PadControl] = &[
        C::RightStickUp,
        C::RightStickLeft,
        C::RightStickDown,
        C::RightStickRight,
    ];
    const PAD: &[PadControl] = &[C::Up, C::Left, C::Down, C::Right];
    &[
        (4, &[C::Up]),
        (5, &[C::Down]),
        (6, &[C::Left]),
        (7, &[C::Right]),
        (8, PAD),
        (9, PAD),
        (10, &[C::Up, C::Down]),
        (11, &[C::Left, C::Right]),
        (12, &[C::LeftStickUp]),
        (13, &[C::LeftStickDown]),
        (14, &[C::LeftStickLeft]),
        (15, &[C::LeftStickRight]),
        (16, &[C::LeftStick]),
        (17, LEFT),
        (18, &[C::LeftStickUp, C::LeftStickDown]),
        (19, &[C::LeftStickLeft, C::LeftStickRight]),
        (20, LEFT),
        (21, &[C::RightStickUp]),
        (22, &[C::RightStickDown]),
        (23, &[C::RightStickLeft]),
        (24, &[C::RightStickRight]),
        (25, &[C::RightStick]),
        (26, RIGHT),
        (27, &[C::RightStickUp, C::RightStickDown]),
        (28, &[C::RightStickLeft, C::RightStickRight]),
        (29, RIGHT),
        (30, &[C::A]),
        (31, &[C::B]),
        (32, &[C::X]),
        (33, &[C::Y]),
        (34, &[C::LeftShoulder]),
        (35, &[C::LeftTrigger]),
        (36, &[C::RightShoulder]),
        (37, &[C::RightTrigger]),
        (38, &[C::Menu]),
        (39, &[C::View]),
        (58, LEFT),
        (59, RIGHT),
    ]
};

/// A movie with fewer icons than this is not the icon movie.
const FEWEST_ICONS: usize = 20;
/// Controls whose icons the icon movie has besides.
const FACE_BUTTONS: [PadControl; 4] = [PadControl::A, PadControl::B, PadControl::X, PadControl::Y];

/// One icon cell: 64 pixels of 20 twips, the size of the title's own icon
/// pictures (Series 1.0.2.1). Every single key cap fills the same square.
pub(crate) const CELL_TWIPS: i32 = 1280;
/// The cap's distance from the cell's edge and its corner radius.
const CAP_MARGIN_TWIPS: i32 = 50;
const CAP_RADIUS_TWIPS: i32 = 220;
/// A cap's side: every standard cap is this square.
const CAP_TWIPS: i32 = CELL_TWIPS - 2 * CAP_MARGIN_TWIPS;
/// Twips per em of a label that fits its cap: letters and digits alike.
const EM_TWIPS: f64 = 860.0;
/// Minimum horizontal room between a label and its cap's sides.
const SIDE_TWIPS: i32 = 100;
/// A lone symbol sign (arrow, Enter, Backspace) fills up to this height inside SIGN_ROOM_TWIPS.
const SIGN_TWIPS: i32 = 640;
const SIGN_ROOM_TWIPS: i32 = CELL_TWIPS - 2 * 250;
/// Gap between adjacent caps inside a cluster (`W A S D`, arrows, combos).
const GAP_TWIPS: i32 = 110;
/// A capital's height in ems (Segoe UI's).
const CAPITAL_EMS: f64 = 0.7;
/// Advance of a space in ems (Segoe UI's).
const SPACE_EMS: f64 = 0.274;

const WHITE: [u8; 4] = [0xFF; 4];
const BLACK: [u8; 4] = [0, 0, 0, 0xFF];

/// A position in ems, x right; y up from the baseline in a glyph.
pub(crate) type EmPoint = [f64; 2];

/// A character's outline, outer contours clockwise, and how far the pen moves after it.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Glyph {
    pub(crate) contours: Vec<Contour<EmPoint>>,
    pub(crate) advance_ems: f64,
}

/// What a single base key cap shows (without modifiers).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BaseKey {
    /// A key's name or sign, set in the host font.
    Text(&'static str),
    /// The mouse, with the active part filled in.
    Mouse(Press),
}

/// What one prompt key entry shows: either a single key/mouse or a modifier combination
/// (e.g. `Ctrl+E`, `Shift+F1`, `Ctrl+Shift+Space`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Key {
    Single(BaseKey),
    Combo {
        modifiers: Modifiers,
        base: BaseKey,
    },
}

/// The part of the mouse a mouse cap shows pressed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Press {
    /// None: the mouse is moved.
    Moves,
    Left,
    Right,
    /// The scroll wheel (press).
    Wheel,
    /// Scroll wheel up.
    WheelUp,
    /// Scroll wheel down.
    WheelDown,
}

/// Maps any single Windows VK or synthetic mouse code to its visual [`BaseKey`].
pub(crate) fn base_key_label(virtual_key: u8) -> Option<BaseKey> {
    const LETTERS: &str = "ABCDEFGHIJKLMNOPQRSTUVWXYZ";
    const DIGITS: &str = "0123456789";
    const FKEYS: [&str; 12] = [
        "F1", "F2", "F3", "F4", "F5", "F6", "F7", "F8", "F9", "F10", "F11", "F12",
    ];
    const NUMPADS: [&str; 10] = ["N0", "N1", "N2", "N3", "N4", "N5", "N6", "N7", "N8", "N9"];

    if (b'A'..=b'Z').contains(&virtual_key) {
        let at = usize::from(virtual_key - b'A');
        return Some(BaseKey::Text(LETTERS.get(at..=at)?));
    }
    if (b'0'..=b'9').contains(&virtual_key) {
        let at = usize::from(virtual_key - b'0');
        return Some(BaseKey::Text(DIGITS.get(at..=at)?));
    }
    if (VK_F1..=VK_F12).contains(&virtual_key) {
        return Some(BaseKey::Text(FKEYS[usize::from(virtual_key - VK_F1)]));
    }
    if (VK_NUMPAD0..=VK_NUMPAD9).contains(&virtual_key) {
        return Some(BaseKey::Text(NUMPADS[usize::from(virtual_key - VK_NUMPAD0)]));
    }

    Some(match virtual_key {
        // Mouse
        VK_LBUTTON => BaseKey::Mouse(Press::Left),
        VK_RBUTTON => BaseKey::Mouse(Press::Right),
        VK_MBUTTON => BaseKey::Mouse(Press::Wheel),
        VK_XBUTTON1 => BaseKey::Text("MB4"),
        VK_XBUTTON2 => BaseKey::Text("MB5"),
        MOUSE_WHEEL_UP => BaseKey::Mouse(Press::WheelUp),
        MOUSE_WHEEL_DOWN => BaseKey::Mouse(Press::WheelDown),
        MOUSE_MOVES => BaseKey::Mouse(Press::Moves),
        // Core & modifiers
        VK_BACK => BaseKey::Text("\u{232B}"),
        VK_TAB => BaseKey::Text("Tab"),
        VK_RETURN => BaseKey::Text("\u{21B5}"),
        VK_ESCAPE => BaseKey::Text("Esc"),
        VK_SPACE => BaseKey::Text("Space"),
        VK_CAPITAL => BaseKey::Text("Caps"),
        VK_SHIFT | VK_LSHIFT => BaseKey::Text("Shift"),
        VK_RSHIFT => BaseKey::Text("RShf"),
        VK_CONTROL | VK_LCONTROL => BaseKey::Text("Ctrl"),
        VK_RCONTROL => BaseKey::Text("RCtrl"),
        VK_MENU | VK_LMENU => BaseKey::Text("Alt"),
        VK_RMENU => BaseKey::Text("RAlt"),
        // Arrows & navigation
        VK_LEFT => BaseKey::Text("\u{2190}"),
        VK_UP => BaseKey::Text("\u{2191}"),
        VK_RIGHT => BaseKey::Text("\u{2192}"),
        VK_DOWN => BaseKey::Text("\u{2193}"),
        VK_INSERT => BaseKey::Text("Ins"),
        VK_DELETE => BaseKey::Text("Del"),
        VK_HOME => BaseKey::Text("Home"),
        VK_END => BaseKey::Text("End"),
        VK_PRIOR => BaseKey::Text("PgUp"),
        VK_NEXT => BaseKey::Text("PgDn"),
        // Numpad operators
        VK_ADD => BaseKey::Text("N+"),
        VK_SUBTRACT => BaseKey::Text("N-"),
        VK_MULTIPLY => BaseKey::Text("N*"),
        VK_DIVIDE => BaseKey::Text("N/"),
        VK_DECIMAL => BaseKey::Text("N."),
        // Punctuation & special characters
        VK_OEM_COMMA => BaseKey::Text(","),
        VK_OEM_PERIOD => BaseKey::Text("."),
        VK_OEM_MINUS => BaseKey::Text("-"),
        VK_OEM_PLUS => BaseKey::Text("="),
        VK_OEM_4 => BaseKey::Text("["),
        VK_OEM_6 => BaseKey::Text("]"),
        VK_OEM_1 => BaseKey::Text(";"),
        VK_OEM_7 => BaseKey::Text("'"),
        VK_OEM_2 => BaseKey::Text("/"),
        VK_OEM_5 => BaseKey::Text("\\"),
        VK_OEM_3 => BaseKey::Text("`"),
        _ => return None,
    })
}

/// Converts a [`KeySpec`] (plain key or modifier combo) into a drawable [`Key`].
pub(crate) fn key_label(spec: KeySpec) -> Option<Key> {
    let base = base_key_label(spec.key)?;
    if spec.modifiers.is_empty() {
        Some(Key::Single(base))
    } else {
        Some(Key::Combo {
            modifiers: spec.modifiers,
            base,
        })
    }
}

/// Short display label for a [`Modifiers`] bitmask (e.g. `"Ctrl"`, `"C+S"` for Ctrl+Shift).
pub(crate) fn modifier_badge(mods: Modifiers) -> &'static str {
    let c = mods.contains(Modifiers::CTRL);
    let s = mods.contains(Modifiers::SHIFT);
    let a = mods.contains(Modifiers::ALT);
    match (c, s, a) {
        (true, false, false) => "Ctrl",
        (false, true, false) => "Shift",
        (false, false, true) => "Alt",
        (true, true, false) => "C+S",
        (true, false, true) => "C+A",
        (false, true, true) => "S+A",
        (true, true, true) => "C+S+A",
        (false, false, false) => "",
    }
}

/// The keys bound to `controls` under `bindings`; duplicates (e.g. 4 directions of mouse) collapse.
pub(crate) fn keys_of(controls: &[PadControl], bindings: &PromptBindings) -> Option<Vec<Key>> {
    let mut keys = controls
        .iter()
        .map(|&control| key_label(bindings.key_of(control)))
        .collect::<Option<Vec<_>>>()?;
    keys.dedup();
    Some(keys)
}

/// The characters `keys` need from the host font (not spaces).
pub(crate) fn characters(keys: &[Key]) -> Vec<char> {
    let mut out = Vec::new();
    for &key in keys {
        let base = match key {
            Key::Single(base) => base,
            Key::Combo { modifiers, base } => {
                out.extend(modifier_badge(modifiers).chars().filter(|&c| c != ' '));
                base
            }
        };
        if let BaseKey::Text(text) = base {
            out.extend(text.chars().filter(|&c| c != ' '));
        }
    }
    out
}

/// A key cap rectangle from `(left, top)` of size `(width, height)`, clockwise.
fn rounded_rect(left: i32, top: i32, width: i32, height: i32, radius: i32) -> Contour {
    let right = left + width;
    let bottom = top + height;
    let r = radius.min(width / 2).min(height / 2).max(0);
    let (near_x, far_x) = (left + r, right - r);
    let (near_y, far_y) = (top + r, bottom - r);
    let point = |x, y| Point { x, y };
    let corner = |cx, cy, x, y| Edge::Curve {
        control: point(cx, cy),
        to: point(x, y),
    };
    Contour {
        start: point(near_x, top),
        edges: vec![
            Edge::Line(point(far_x, top)),
            corner(right, top, right, near_y),
            Edge::Line(point(right, far_y)),
            corner(right, bottom, far_x, bottom),
            Edge::Line(point(near_x, bottom)),
            corner(left, bottom, left, far_y),
            Edge::Line(point(left, near_y)),
            corner(left, top, near_x, top),
        ],
    }
}

/// Standard square key cap from `left`, `width` wide, full height.
fn cap(left: i32, width: i32) -> Contour {
    rounded_rect(left, CAP_MARGIN_TWIPS, width, CAP_TWIPS, CAP_RADIUS_TWIPS)
}

/// An icon's drawing: its caps (white) and what is on them (black).
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Drawing {
    pub(crate) caps: Vec<Contour>,
    pub(crate) marks: Vec<Contour>,
    pub(crate) width: i32,
}

/// Draws a single `Key` (either one full square cap or a vertically stacked modifier
/// pill + main key cap inside the same square footprint) at `(left, down)`.
#[expect(
    clippy::cast_possible_truncation,
    reason = "positions inside one icon cell"
)]
fn draw_one(
    key: Key,
    left: i32,
    down: i32,
    glyphs: &HashMap<char, Glyph>,
    caps: &mut Vec<Contour>,
    marks: &mut Vec<Contour>,
) -> Option<i32> {
    match key {
        Key::Single(base) => {
            let (width, drawn) = match base {
                BaseKey::Text(text) => text_marks_in(text, glyphs, CAP_TWIPS, CAP_TWIPS, CELL_TWIPS / 2)?,
                BaseKey::Mouse(press) => (CAP_TWIPS, mouse(press)),
            };
            let middle = left + width / 2;
            let place = |p: Point| Point { x: p.x, y: p.y + down };
            caps.push(cap(left, width).map(place));
            marks.extend(drawn.iter().map(|contour| {
                contour.map(|p| Point {
                    x: p.x + middle,
                    y: p.y + down,
                })
            }));
            Some(width)
        }
        Key::Combo { modifiers, base } => {
            // Two stacked rounded caps inside the 1180×1180 square:
            // Top modifier pill (430 twips tall), 70-twip gap, bottom key cap (680 twips tall).
            const GAP: i32 = 70;
            const TOP_H: i32 = 430;
            const BOT_H: i32 = CAP_TWIPS - TOP_H - GAP; // 680
            let top_y = CAP_MARGIN_TWIPS + down;
            let bot_y = top_y + TOP_H + GAP;
            let middle = left + CAP_TWIPS / 2;

            // Top modifier pill
            let mod_text = modifier_badge(modifiers);
            let (_, mod_marks) = text_marks_in(
                mod_text,
                glyphs,
                CAP_TWIPS,
                TOP_H,
                top_y + TOP_H / 2,
            )?;
            caps.push(rounded_rect(left, top_y, CAP_TWIPS, TOP_H, 130));
            marks.extend(mod_marks.iter().map(|c| c.map(|p| Point { x: p.x + middle, y: p.y })));

            // Bottom main key cap
            caps.push(rounded_rect(left, bot_y, CAP_TWIPS, BOT_H, 160));
            match base {
                BaseKey::Text(text) => {
                    let (_, base_marks) =
                        text_marks_in(text, glyphs, CAP_TWIPS, BOT_H, bot_y + BOT_H / 2)?;
                    marks.extend(
                        base_marks
                            .iter()
                            .map(|c| c.map(|p| Point { x: p.x + middle, y: p.y })),
                    );
                }
                BaseKey::Mouse(press) => {
                    let raw = mouse(press);
                    let scale = f64::from(BOT_H) / f64::from(CAP_TWIPS);
                    let src_mid = f64::from(CELL_TWIPS) / 2.0;
                    let dst_mid = f64::from(bot_y) + f64::from(BOT_H) / 2.0;
                    marks.extend(raw.iter().map(|c| {
                        c.map(|p| Point {
                            x: middle + (f64::from(p.x) * scale).round() as i32,
                            y: (dst_mid + (f64::from(p.y) - src_mid) * scale).round() as i32,
                        })
                    }));
                }
            }
            Some(CAP_TWIPS)
        }
    }
}

/// `keys` drawn as square caps inside one 1280×1280 twips icon cell: one row,
/// or two rows of two for four keys (e.g. `W A S D` / arrows). Scaled whole
/// and centred so nothing ever clips into adjacent text.
#[expect(
    clippy::cast_possible_truncation,
    reason = "positions inside one icon cell"
)]
pub(crate) fn draw(keys: &[Key], glyphs: &HashMap<char, Glyph>) -> Option<Drawing> {
    if keys.is_empty() {
        return None;
    }
    let rows: Vec<&[Key]> = if keys.len() == 4 {
        keys.chunks(2).collect()
    } else {
        vec![keys]
    };
    let (mut caps, mut marks) = (Vec::new(), Vec::new());
    let mut widest = 0;
    for (row, row_keys) in rows.iter().enumerate() {
        let down = i32::try_from(row).ok()? * (CAP_TWIPS + GAP_TWIPS);
        let mut left = CAP_MARGIN_TWIPS;
        for &key in *row_keys {
            let width = draw_one(key, left, down, glyphs, &mut caps, &mut marks)?;
            left += width + GAP_TWIPS;
        }
        widest = widest.max(left - GAP_TWIPS - CAP_MARGIN_TWIPS);
    }
    let rows = i32::try_from(rows.len()).ok()?;
    let tall = rows * CAP_TWIPS + (rows - 1) * GAP_TWIPS;
    let scale = (f64::from(CAP_TWIPS) / f64::from(widest))
        .min(f64::from(CAP_TWIPS) / f64::from(tall))
        .min(1.0);
    let margin = f64::from(CAP_MARGIN_TWIPS);
    let left = margin + (f64::from(CAP_TWIPS) - f64::from(widest) * scale) / 2.0;
    let top = margin + (f64::from(CAP_TWIPS) - f64::from(tall) * scale) / 2.0;
    let fit = |p: Point| Point {
        x: (left + (f64::from(p.x) - margin) * scale).round() as i32,
        y: (top + (f64::from(p.y) - margin) * scale).round() as i32,
    };
    let fitted = |contours: Vec<Contour>| contours.iter().map(|c| c.map(fit)).collect();
    Some(Drawing {
        caps: fitted(caps),
        marks: fitted(marks),
        width: CELL_TWIPS,
    })
}

/// Lays out `text` centred horizontally at `x = 0` and vertically at `center_y`,
/// fitted cleanly inside a cap of `(cap_w, cap_h)` twips.
#[expect(
    clippy::cast_possible_truncation,
    reason = "positions inside a few icon cells"
)]
fn text_marks_in(
    text: &str,
    glyphs: &HashMap<char, Glyph>,
    cap_w: i32,
    cap_h: i32,
    center_y: i32,
) -> Option<(i32, Vec<Contour>)> {
    let mut contours: Vec<Contour<EmPoint>> = Vec::new();
    let mut pen = 0.0;
    for character in text.chars() {
        if character == ' ' {
            pen += SPACE_EMS;
            continue;
        }
        let glyph = glyphs.get(&character)?;
        contours.extend(
            glyph
                .contours
                .iter()
                .map(|contour| contour.map(|[x, y]| [x + pen, y])),
        );
        pen += glyph.advance_ems;
    }
    let extent = |axis: usize| {
        contours
            .iter()
            .flat_map(Contour::points)
            .fold((f64::MAX, f64::MIN), |(low, high), point| {
                (low.min(point[axis]), high.max(point[axis]))
            })
    };
    let ((left, right), (low, high)) = (extent(0), extent(1));
    if !(right > left && high > low) {
        return None;
    }
    let is_symbol_sign = matches!(
        text,
        "\u{21B5}" | "\u{232B}" | "\u{2190}" | "\u{2191}" | "\u{2192}" | "\u{2193}"
    );
    let h_ratio = f64::from(cap_h) / f64::from(CAP_TWIPS);
    let em = if !is_symbol_sign {
        let room_x = f64::from(cap_w - 2 * SIDE_TWIPS).max(100.0);
        let room_y = f64::from(cap_h) * 0.72;
        (EM_TWIPS * h_ratio)
            .min(room_x / (right - left))
            .min(room_y / (high - low))
    } else {
        let room_x = f64::from(SIGN_ROOM_TWIPS).min(f64::from(cap_w - 2 * SIDE_TWIPS)).max(100.0);
        let room_y = (f64::from(SIGN_TWIPS) * h_ratio).min(f64::from(cap_h) * 0.72);
        (room_x / (right - left)).min(room_y / (high - low))
    };
    let centre = if text.chars().any(char::is_alphanumeric) {
        CAPITAL_EMS / 2.0
    } else {
        f64::midpoint(low, high)
    };
    let middle = f64::from(center_y);
    let twips = |[x, y]: EmPoint| Point {
        x: ((x - f64::midpoint(left, right)) * em).round() as i32,
        y: (middle - (y - centre) * em).round() as i32,
    };
    Some((
        cap_w,
        contours.iter().map(|contour| contour.map(twips)).collect(),
    ))
}

/// A point in twips while a mouse is drawn: x from its cap's middle, y down.
type At = (f64, f64);

fn arc(points: &mut Vec<At>, centre: At, radius: f64, from: f64, to: f64) {
    #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "a few steps")]
    let steps = ((to - from).abs() / 7.5).ceil().max(1.0) as usize;
    for step in 0..=steps {
        let angle = (from + (to - from) * step as f64 / steps as f64).to_radians();
        points.push((
            centre.0 + radius * angle.cos(),
            centre.1 + radius * angle.sin(),
        ));
    }
}

fn capsule(centre: At, half_width: f64, half_height: f64) -> Vec<At> {
    let reach = half_height - half_width;
    let mut points = Vec::new();
    arc(&mut points, (centre.0, centre.1 - reach), half_width, 180.0, 360.0);
    arc(&mut points, (centre.0, centre.1 + reach), half_width, 0.0, 180.0);
    points
}

#[expect(clippy::cast_possible_truncation, reason = "positions inside one icon cell")]
fn polygon(points: &[At], hole: bool) -> Contour {
    let area: f64 = points
        .iter()
        .zip(points.iter().cycle().skip(1))
        .map(|(a, b)| a.0 * b.1 - b.0 * a.1)
        .sum();
    let mut at: Vec<Point> = points
        .iter()
        .map(|&(x, y)| Point {
            x: x.round() as i32,
            y: y.round() as i32,
        })
        .collect();
    at.dedup();
    if (area > 0.0) == hole {
        at.reverse();
    }
    let start = at[0];
    let mut edges: Vec<Edge> = at[1..].iter().map(|&point| Edge::Line(point)).collect();
    edges.push(Edge::Line(start));
    Contour { start, edges }
}

/// Vector mouse icon with highlighted left/right/wheel/scroll indicator.
fn mouse(press: Press) -> Vec<Contour> {
    let middle = f64::from(CELL_TWIPS) / 2.0;
    let (outer_x, outer_y, line) = (300.0, 460.0, 75.0);
    let (inner_x, inner_y) = (outer_x - line, outer_y - line);
    let inner_top = (0.0, middle - inner_y + inner_x);
    let inner_bottom = (0.0, middle + inner_y - inner_x);
    let split = middle - outer_y + 0.42 * 2.0 * outer_y;
    let (split_top, split_bottom) = (split - line / 2.0, split + line / 2.0);
    let between = line / 2.0;
    let (wheel_y, wheel_x, wheel_half, rim) = (middle - 248.0, 78.0, 118.0, 45.0);
    let wheel_top = (0.0, wheel_y - wheel_half + wheel_x);
    let wheel_bottom = (0.0, wheel_y + wheel_half - wheel_x);
    let meets = |radius: f64| (between / radius).acos().to_degrees();

    let mut left = vec![(-between, split_top), (-inner_x, split_top)];
    arc(&mut left, inner_top, inner_x, 180.0, 180.0 + meets(inner_x));
    arc(&mut left, wheel_top, wheel_x, 180.0 + meets(wheel_x), 180.0);
    arc(&mut left, wheel_bottom, wheel_x, 180.0, 180.0 - meets(wheel_x));
    let right: Vec<At> = left.iter().map(|&(x, y)| (-x, y)).collect();
    let mut body = vec![(inner_x, split_bottom), (-inner_x, split_bottom)];
    arc(&mut body, inner_bottom, inner_x, 180.0, 0.0);
    let wheel = capsule((0.0, wheel_y), wheel_x - rim, wheel_half - rim);

    let mut contours = vec![polygon(&capsule((0.0, middle), outer_x, outer_y), false)];
    if press != Press::Left {
        contours.push(polygon(&left, true));
    }
    if press != Press::Right {
        contours.push(polygon(&right, true));
    }
    contours.push(polygon(&body, true));
    if !matches!(press, Press::Wheel | Press::WheelUp | Press::WheelDown) {
        contours.push(polygon(&wheel, true));
    }
    // Add scroll arrow inside the lower body for WheelUp / WheelDown.
    if press == Press::WheelUp {
        let cy = middle + 170.0;
        contours.push(polygon(
            &[(0.0, cy - 110.0), (110.0, cy + 70.0), (-110.0, cy + 70.0)],
            false,
        ));
    } else if press == Press::WheelDown {
        let cy = middle + 170.0;
        contours.push(polygon(
            &[(0.0, cy + 110.0), (-110.0, cy - 70.0), (110.0, cy - 70.0)],
            false,
        ));
    }
    contours
}

fn icon_number(name: &[u8]) -> Option<u32> {
    let digits = name.get(name.iter().rposition(|&byte| byte == b'_')? + 1..)?;
    if digits.is_empty() || !digits.iter().all(u8::is_ascii_digit) {
        return None;
    }
    std::str::from_utf8(digits).ok()?.parse().ok()
}

fn still_shapes(sprite: &Sprite) -> Option<Vec<u16>> {
    let mut placed: Vec<(Depth, u16)> = sprite
        .placements
        .iter()
        .map(|placement| match placement.change {
            Change::Place(character) if placement.frame == 0 => Some((placement.depth, character)),
            Change::Place(_)
            | Change::Mask(_)
            | Change::Replace(_)
            | Change::Modify
            | Change::Remove
            | Change::Unread => None,
        })
        .collect::<Option<_>>()?;
    placed.sort_unstable();
    if placed.windows(2).any(|pair| pair[0].0 == pair[1].0) {
        return None;
    }
    Some(placed.into_iter().map(|(_, shape)| shape).collect())
}

fn controls(number: u32) -> Option<&'static [PadControl]> {
    ICONS
        .iter()
        .find(|(known, _)| *known == number)
        .map(|(_, controls)| *controls)
}

fn uses(sprites: &[Sprite], timeline: &[Placement]) -> Option<HashMap<u16, u32>> {
    let mut uses = HashMap::new();
    let placements = sprites
        .iter()
        .flat_map(|sprite| &sprite.placements)
        .chain(timeline);
    for placement in placements {
        match placement.change {
            Change::Place(character) | Change::Mask(character) | Change::Replace(character) => {
                *uses.entry(character).or_insert(0) += 1;
            }
            Change::Modify | Change::Remove => {}
            Change::Unread => return None,
        }
    }
    Some(uses)
}

/// Why a movie was not redrawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Refusal {
    /// It is not the icon movie.
    NotIconMovie,
    /// It is the icon movie, but it places something this module cannot read.
    Unreadable,
    /// It is the icon movie, but no icon of it could be drawn.
    NothingDrawn,
}

/// Returns `true` if `movie` structurally matches the title's Scaleform button-icon movie.
#[must_use]
pub(crate) fn is_icon_movie(movie: &[u8]) -> bool {
    let Some(movie) = Movie::parse(movie) else {
        return false;
    };
    let Some(classes) = movie.classes() else {
        return false;
    };
    let mut numbers = HashSet::new();
    for (_, name) in classes {
        if let Some(n) = icon_number(name)
            && controls(n).is_some()
        {
            numbers.insert(n);
        }
    }
    let has_icon = |button: PadControl| {
        ICONS
            .iter()
            .any(|&(number, ctrls)| ctrls == [button] && numbers.contains(&number))
    };
    numbers.len() >= FEWEST_ICONS && FACE_BUTTONS.into_iter().all(has_icon)
}

/// Redraws `movie` using `bindings` and `glyph` outlines.
pub(crate) fn key_caps_with(
    movie: &[u8],
    bindings: &PromptBindings,
    glyph: &dyn Fn(char) -> Option<Glyph>,
) -> Result<Vec<u8>, Refusal> {
    let movie = Movie::parse(movie).ok_or(Refusal::NotIconMovie)?;
    let mut numbers = HashSet::new();
    let mut wanted: HashMap<u16, &'static [PadControl]> = HashMap::new();
    for (sprite, name) in movie.classes().ok_or(Refusal::NotIconMovie)? {
        let Some((number, controls)) = icon_number(name).and_then(|n| Some((n, controls(n)?)))
        else {
            continue;
        };
        if numbers.insert(number) {
            wanted.entry(sprite).or_insert(controls);
        }
    }
    let has_icon = |button: PadControl| {
        ICONS
            .iter()
            .any(|&(number, controls)| controls == [button] && numbers.contains(&number))
    };
    if numbers.len() < FEWEST_ICONS || !FACE_BUTTONS.into_iter().all(has_icon) {
        return Err(Refusal::NotIconMovie);
    }
    if movie.defines_buttons() {
        return Err(Refusal::Unreadable);
    }
    let sprites = movie.sprites().ok_or(Refusal::Unreadable)?;
    let timeline = movie.timeline().ok_or(Refusal::Unreadable)?;
    let uses = uses(&sprites, &timeline).ok_or(Refusal::Unreadable)?;
    let icons: Vec<(&[PadControl], Vec<u16>)> = sprites
        .iter()
        .filter_map(|sprite| Some((wanted.remove(&sprite.id)?, still_shapes(sprite)?)))
        .collect();
    if icons.iter().filter(|(_, shapes)| shapes.len() >= 2).count() < FEWEST_ICONS {
        return Err(Refusal::NotIconMovie);
    }
    let definitions = movie.shape_counts();
    let mut asked = HashSet::new();
    let mut glyphs: HashMap<char, Glyph> = HashMap::new();
    for keys in icons.iter().filter_map(|(controls, _)| keys_of(controls, bindings)) {
        for character in characters(&keys) {
            if asked.insert(character)
                && let Some(found) = glyph(character)
            {
                glyphs.insert(character, found);
            }
        }
    }
    let mut bodies: Vec<ShapeBody> = Vec::new();
    for (controls, shapes) in &icons {
        let [back, front, rest @ ..] = shapes.as_slice() else {
            continue;
        };
        if shapes
            .iter()
            .any(|shape| uses.get(shape) != Some(&1) || definitions.get(shape) != Some(&1))
        {
            continue;
        }
        let Some(keys) = keys_of(controls, bindings) else {
            continue;
        };
        let Some(drawing) = draw(&keys, &glyphs) else {
            continue;
        };
        let Some(marks) = gfx::shape(*front, BLACK, &drawing.marks) else {
            continue;
        };
        bodies.push(gfx::shape(*back, WHITE, &drawing.caps).ok_or(Refusal::Unreadable)?);
        bodies.push(marks);
        for &shape in rest {
            bodies.push(gfx::shape(shape, BLACK, &[]).ok_or(Refusal::Unreadable)?);
        }
    }
    if bodies.is_empty() {
        return Err(Refusal::NothingDrawn);
    }
    movie.replace_shapes(&bodies).ok_or(Refusal::Unreadable)
}

/// Hand-built movies for unit tests.
#[cfg(test)]
pub(crate) mod fixtures {
    use super::ICONS;

    pub(crate) const END: u16 = 0;
    pub(crate) const PLACE_OBJECT_2: u16 = 26;
    pub(crate) const DEFINE_SHAPE_3: u16 = 32;
    pub(crate) const DEFINE_SPRITE: u16 = 39;
    pub(crate) const SYMBOL_CLASS: u16 = 76;
    pub(crate) const FIRST_SPRITE: u16 = 60_000;

    pub(crate) fn tag(code: u16, body: &[u8]) -> Vec<u8> {
        let mut bytes = ((code << 6) | 0x3F).to_le_bytes().to_vec();
        bytes.extend(u32::try_from(body.len()).unwrap().to_le_bytes());
        bytes.extend(body);
        bytes
    }

    pub(crate) fn place(depth: u16, shape: u16) -> Vec<u8> {
        let mut body = vec![0b110];
        body.extend(depth.to_le_bytes());
        body.extend(shape.to_le_bytes());
        tag(PLACE_OBJECT_2, &body)
    }

    pub(crate) fn picture(id: u16) -> Vec<u8> {
        [id.to_le_bytes(), [0xAB, 0xCD]].concat()
    }

    pub(crate) fn movie(
        shapes: &[u16],
        sprites: &[(u16, Vec<Vec<u8>>)],
        classes: &[(u16, String)],
    ) -> Vec<u8> {
        let mut bytes = b"GFX\x0b\0\0\0\0\0\0\x1e\x01\0".to_vec();
        for &shape in shapes {
            bytes.extend(tag(DEFINE_SHAPE_3, &picture(shape)));
        }
        for (id, own) in sprites {
            let mut body = [id.to_le_bytes(), [0, 0]].concat();
            body.extend(own.concat());
            body.extend(tag(END, &[]));
            bytes.extend(tag(DEFINE_SPRITE, &body));
        }
        let mut table = u16::try_from(classes.len()).unwrap().to_le_bytes().to_vec();
        for (id, name) in classes {
            table.extend(id.to_le_bytes());
            table.extend(name.bytes());
            table.push(0);
        }
        bytes.extend(tag(SYMBOL_CLASS, &table));
        bytes.extend(tag(END, &[]));
        let len = u32::try_from(bytes.len()).unwrap().to_le_bytes();
        bytes[4..8].copy_from_slice(&len);
        bytes
    }

    pub(crate) fn known_icons() -> Vec<(u32, Vec<u16>)> {
        ICONS
            .iter()
            .map(|&(number, _)| {
                let first = 100 * u16::try_from(number).unwrap();
                (number, vec![first, first + 1, first + 2])
            })
            .collect()
    }

    pub(crate) fn icon_movie(icons: &[(u32, Vec<u16>)]) -> Vec<u8> {
        let mut shapes: Vec<u16> = icons
            .iter()
            .flat_map(|(_, shapes)| shapes.clone())
            .collect();
        shapes.sort_unstable();
        shapes.dedup();
        let sprite = |index: usize| FIRST_SPRITE + u16::try_from(index).unwrap();
        let sprites: Vec<_> = icons
            .iter()
            .enumerate()
            .map(|(index, (_, shapes))| {
                let placed = (1..).zip(shapes).map(|(depth, &shape)| place(depth, shape));
                (sprite(index), placed.collect())
            })
            .collect();
        let classes: Vec<_> = icons
            .iter()
            .enumerate()
            .map(|(index, (number, _))| (sprite(index), format!("icon_{number}")))
            .collect();
        movie(&shapes, &sprites, &classes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fixtures::{icon_movie, known_icons};

    fn square(_: char) -> Option<Glyph> {
        Some(Glyph {
            contours: vec![Contour {
                start: [0.0, 0.0],
                edges: [[0.0, 0.5], [0.5, 0.5], [0.5, 0.0], [0.0, 0.0]]
                    .map(Edge::Line)
                    .to_vec(),
            }],
            advance_ems: 0.6,
        })
    }

    #[test]
    fn every_required_key_category_and_combo_renders_within_cell() {
        let specs = [
            "A", "Z", "0", "9", "F1", "F12", "Shift", "RightShift", "Ctrl", "RightCtrl", "Alt",
            "RightAlt", "Enter", "Esc", "Tab", "Backspace", "Space", "CapsLock", "ArrowUp",
            "ArrowDown", "ArrowLeft", "ArrowRight", "Insert", "Delete", "Home", "End", "PageUp",
            "PageDown", "Numpad0", "Numpad9", "NumpadAdd", "NumpadSub", "NumpadMul", "NumpadDiv",
            "NumpadDecimal", "Comma", "Period", "Minus", "Equals", "LeftBracket", "RightBracket",
            "Semicolon", "Quote", "Slash", "Backslash", "Backquote", "LeftClick", "RightClick",
            "MiddleClick", "Mouse4", "Mouse5", "WheelUp", "WheelDown", "Mouse", "Ctrl+E",
            "Shift+Space", "Alt+F4", "Ctrl+Shift+R", "Ctrl+LeftClick",
        ];
        for name in specs {
            let spec = KeySpec::parse(name).unwrap_or_else(|| panic!("parse {name}"));
            let key = key_label(spec).unwrap_or_else(|| panic!("label {name}"));
            let mut glyphs = HashMap::new();
            for c in characters(&[key]) {
                glyphs.insert(c, square(c).unwrap());
            }
            let drawing = draw(&[key], &glyphs).unwrap_or_else(|| panic!("draw {name}"));
            assert!(!drawing.caps.is_empty(), "{name}");
            assert!(!drawing.marks.is_empty(), "{name}");
            for contour in drawing.caps.iter().chain(&drawing.marks) {
                for p in contour.points() {
                    assert!(
                        (0..=CELL_TWIPS).contains(&p.x) && (0..=CELL_TWIPS).contains(&p.y),
                        "{name} point out of cell: {p:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn rebinding_changes_the_redrawn_gfx_movie_bytes() {
        let original = icon_movie(&known_icons());
        let b1 = PromptBindings::defaults(true);
        let b2 = b1.clone().with(PadControl::A, KeySpec::combo(Modifiers::CTRL, b'G'));
        let m1 = key_caps_with(&original, &b1, &square).unwrap();
        let m2 = key_caps_with(&original, &b2, &square).unwrap();
        assert_ne!(m1, original);
        assert_ne!(m1, m2, "rebound icon movie must differ from default");
    }
}
