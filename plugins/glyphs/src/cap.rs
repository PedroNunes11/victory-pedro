//! A key cap drawn from nothing: a rounded key with a thicker lower rim and
//! its label, anti-aliased by 4x4 samples a pixel.

use crate::font;

/// Colours of a cap (sRGB values as they are stored).
#[derive(Clone, Copy, Debug)]
pub struct Style {
    pub rim: [f32; 3],
    pub top: [f32; 3],
    pub bottom: [f32; 3],
    pub text: [f32; 3],
    /// Opacity of the key's face (0: only the rim and the label are drawn).
    pub face: f32,
}

/// A dark key with a light rim and a white label. Mostly grey, so it reads
/// the same if the title views the texture's channels in another order;
/// the flat colours have channels of one parity, which BC7 mode 6 stores
/// exactly.
pub const DARK: Style =
    Style { rim: [205.0, 205.0, 205.0], top: [65.0, 65.0, 65.0], bottom: [33.0, 33.0, 33.0], text: [255.0; 3], face: 1.0 };

/// For one-channel textures (masks the title tints): rim and label only.
pub const MASK: Style = Style { rim: [255.0; 3], top: [0.0; 3], bottom: [0.0; 3], text: [255.0; 3], face: 0.0 };

/// Samples along one side of a pixel.
const SAMPLES: usize = 4;

/// Whether `(x, y)` is inside the rectangle `r` (`[x0, y0, x1, y1]`) with
/// corners of radius `radius`.
fn inside(x: f32, y: f32, r: [f32; 4], radius: f32) -> bool {
    if x < r[0] || x > r[2] || y < r[1] || y > r[3] {
        return false;
    }
    let radius = radius.min((r[2] - r[0]) / 2.0).min((r[3] - r[1]) / 2.0).max(0.0);
    let cx = x.clamp(r[0] + radius, r[2] - radius);
    let cy = y.clamp(r[1] + radius, r[3] - radius);
    (x - cx).powi(2) + (y - cy).powi(2) <= radius * radius
}

/// Where the parts of a `w * h` cap lie.
struct Geometry {
    outer: [f32; 4],
    radius: f32,
    face: [f32; 4],
    face_radius: f32,
}

fn geometry(w: f32, h: f32) -> Geometry {
    let margin = (w.min(h) * 0.03).floor();
    let outer = [margin, margin, w - margin, h - margin];
    let side = (outer[2] - outer[0]).min(outer[3] - outer[1]);
    let radius = side * 0.2;
    let rim = (side * 0.07).max(1.0);
    let face = [outer[0] + rim, outer[1] + rim, outer[2] - rim, outer[3] - rim * 1.9];
    Geometry { outer, radius, face, face_radius: (radius - rim).max(0.0) }
}

/// The label's place: cells per pixel, its top left corner, and its grid.
struct Text {
    scale: f32,
    at: [f32; 2],
    width: usize,
    cells: Vec<bool>,
}

/// Picks the first of `labels` that is not drawn much smaller than one
/// character would be (else the one drawn largest), and places it in the
/// middle of `face`.
fn place(labels: &[String], face: [f32; 4]) -> Option<Text> {
    let (fw, fh) = (face[2] - face[0], face[3] - face[1]);
    let pad = fw.min(fh) * 0.08;
    let (aw, ah) = (fw - 2.0 * pad, fh - 2.0 * pad);
    if aw <= 0.0 || ah <= 0.0 {
        return None;
    }
    // A lone character takes 62 % of the face's height.
    let tall = ah * 0.62 / font::HEIGHT as f32;
    let fits = |label: &String| {
        let (width, cells) = font::rasterize(label);
        (width > 0).then(|| (tall.min(aw / width as f32), width, cells))
    };
    let options: Vec<_> = labels.iter().filter_map(fits).collect();
    let (mut scale, width, cells) = options
        .iter()
        .find(|o| o.0 >= tall * 0.6)
        .or_else(|| options.iter().max_by(|a, b| a.0.total_cmp(&b.0)))
        .cloned()?;
    // Whole pixels a cell when there are a few: crisp strokes.
    let snap = scale >= 2.5;
    if snap {
        scale = scale.floor();
    }
    let (tw, th) = (width as f32 * scale, font::HEIGHT as f32 * scale);
    let mut at = [face[0] + (fw - tw) / 2.0, face[1] + (fh - th) / 2.0];
    if snap {
        at = at.map(f32::round);
    }
    Some(Text { scale, at, width, cells })
}

impl Text {
    fn covers(&self, x: f32, y: f32) -> bool {
        let u = (x - self.at[0]) / self.scale;
        let v = (y - self.at[1]) / self.scale;
        if u < 0.0 || v < 0.0 {
            return false;
        }
        let (u, v) = (u as usize, v as usize);
        u < self.width && v < font::HEIGHT && self.cells[v * self.width + u]
    }
}

/// A `w * h` cap labelled with the first of `labels` that fits, as RGBA8
/// rows; transparent around the key. `premultiplied`: colour times alpha
/// (else clear texels carry the rim's colour).
pub fn render(w: u32, h: u32, labels: &[String], style: &Style, premultiplied: bool) -> Vec<u8> {
    let (wf, hf) = (w as f32, h as f32);
    let g = geometry(wf, hf);
    let text = place(labels, g.face);
    let mut out = vec![0u8; (w * h * 4) as usize];
    let step = 1.0 / SAMPLES as f32;
    for py in 0..h {
        for px in 0..w {
            let mut sum = [0f32; 3];
            let mut alpha = 0f32;
            for sy in 0..SAMPLES {
                for sx in 0..SAMPLES {
                    let x = px as f32 + (sx as f32 + 0.5) * step;
                    let y = py as f32 + (sy as f32 + 0.5) * step;
                    if !inside(x, y, g.outer, g.radius) {
                        continue;
                    }
                    let (colour, a) = if !inside(x, y, g.face, g.face_radius) {
                        (style.rim, 1.0)
                    } else if text.as_ref().is_some_and(|t| t.covers(x, y)) {
                        (style.text, 1.0)
                    } else {
                        let t = ((y - g.face[1]) / (g.face[3] - g.face[1]).max(1.0)).clamp(0.0, 1.0);
                        (std::array::from_fn(|c| style.top[c] + (style.bottom[c] - style.top[c]) * t), style.face)
                    };
                    for c in 0..3 {
                        sum[c] += colour[c] * a;
                    }
                    alpha += a;
                }
            }
            let n = (SAMPLES * SAMPLES) as f32;
            let at = ((py * w + px) * 4) as usize;
            let a = alpha / n;
            for c in 0..3 {
                // A clear texel keeps the rim's colour (straight alpha), so
                // filtering at the edge does not darken it.
                let v = if premultiplied {
                    sum[c] / n
                } else if alpha > 0.0 {
                    sum[c] / alpha
                } else {
                    style.rim[c]
                };
                out[at + c] = v.round().clamp(0.0, 255.0) as u8;
            }
            out[at + 3] = (a * 255.0).round() as u8;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn px(img: &[u8], w: u32, x: u32, y: u32) -> [u8; 4] {
        let at = ((y * w + x) * 4) as usize;
        [img[at], img[at + 1], img[at + 2], img[at + 3]]
    }

    #[test]
    fn a_cap_is_a_key_with_its_label() {
        let (w, h) = (64, 64);
        let img = render(w, h, &["A".to_string()], &DARK, false);
        assert_eq!(img.len(), 64 * 64 * 4);
        // Corners outside the rounded key are clear.
        assert_eq!(px(&img, w, 0, 0)[3], 0);
        assert_eq!(px(&img, w, 63, 63)[3], 0);
        // The rim is light, the face dark, the label white.
        let rim = px(&img, w, 32, 3);
        assert_eq!(rim[3], 255);
        assert!(rim[0] > 180, "rim {rim:?}");
        let face = px(&img, w, 12, 30);
        assert!(face[0] < 80 && face[3] == 255, "face {face:?}");
        // "A" has its crossbar on row 3 of 7: a white run across the middle.
        let white = (0..w).filter(|&x| px(&img, w, x, 30)[0] > 240 && px(&img, w, x, 30)[3] == 255).count();
        assert!(white >= 10, "{white} white texels across the label");
    }

    #[test]
    fn labels_fall_back_to_a_shorter_one_when_tight() {
        let face = geometry(32.0, 32.0).face;
        let long = place(&["SPACE".to_string(), "SPC".to_string()], face).unwrap();
        assert_eq!(long.width, 3 * 6 - 1, "SPC on a small square key");
        let wide = geometry(160.0, 40.0).face;
        let t = place(&["SPACE".to_string(), "SPC".to_string()], wide).unwrap();
        assert_eq!(t.width, 5 * 6 - 1, "SPACE fits a wide key");
    }

    #[test]
    fn label_stays_inside_the_face() {
        for (w, h) in [(16, 16), (24, 24), (64, 32), (100, 100), (7, 7)] {
            let g = geometry(w as f32, h as f32);
            if let Some(t) = place(&["SHIFT".to_string(), "SHF".to_string()], g.face) {
                let right = t.at[0] + t.width as f32 * t.scale;
                let bottom = t.at[1] + font::HEIGHT as f32 * t.scale;
                assert!(t.at[0] >= g.face[0] - 0.5 && right <= g.face[2] + 0.5, "{w}x{h}");
                assert!(t.at[1] >= g.face[1] - 0.5 && bottom <= g.face[3] + 0.5, "{w}x{h}");
            }
        }
    }

    #[test]
    fn mask_style_has_no_face_and_premultiplies() {
        let (w, h) = (48, 48);
        let img = render(w, h, &["E".to_string()], &MASK, true);
        // The face is see-through: dark and clear.
        let face = px(&img, w, 10, 24);
        assert_eq!(face, [0, 0, 0, 0]);
        // The rim is white and opaque.
        assert_eq!(px(&img, w, 24, 2), [255, 255, 255, 255]);
    }

    #[test]
    fn tiny_caps_do_not_panic() {
        for (w, h) in [(1, 1), (2, 3), (4, 4)] {
            let img = render(w, h, &["F12".to_string()], &DARK, false);
            assert_eq!(img.len(), (w * h * 4) as usize);
        }
        assert_eq!(render(8, 8, &[], &DARK, false).len(), 8 * 8 * 4);
    }
}
