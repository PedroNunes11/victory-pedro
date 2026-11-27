//! BC1 colour blocks (also the colour half of BC2 and BC3) and BC4 blocks
//! (also the alpha half of BC3 and both halves of BC5).

use crate::{least_squares, principal};

fn unpack565(c: u16) -> [u8; 3] {
    let (r, g, b) = ((c >> 11) & 31, (c >> 5) & 63, c & 31);
    [((r << 3) | (r >> 2)) as u8, ((g << 2) | (g >> 4)) as u8, ((b << 3) | (b >> 2)) as u8]
}

fn pack565(c: [f32; 3]) -> u16 {
    let q = |v: f32, max: f32| (v.clamp(0.0, 255.0) * max / 255.0).round() as u16;
    (q(c[0], 31.0) << 11) | (q(c[1], 63.0) << 5) | q(c[2], 31.0)
}

/// The four colours of a colour block (the fourth transparent black in the
/// three-colour mode, which BC2 and BC3 never use: `four_only`).
fn palette(c0: u16, c1: u16, four_only: bool) -> [[u8; 4]; 4] {
    let (a, b) = (unpack565(c0), unpack565(c1));
    let mix = |wa: u16, wb: u16, d: u16| -> [u8; 4] {
        let m = |i: usize| ((u16::from(a[i]) * wa + u16::from(b[i]) * wb + d / 2) / d) as u8;
        [m(0), m(1), m(2), 255]
    };
    if four_only || c0 > c1 {
        [[a[0], a[1], a[2], 255], [b[0], b[1], b[2], 255], mix(2, 1, 3), mix(1, 2, 3)]
    } else {
        [[a[0], a[1], a[2], 255], [b[0], b[1], b[2], 255], mix(1, 1, 2), [0, 0, 0, 0]]
    }
}

/// Decodes a colour block into `px` (all four channels).
pub(crate) fn decode_colour(block: &[u8], px: &mut [[u8; 4]; 16], four_only: bool) {
    let c0 = u16::from_le_bytes([block[0], block[1]]);
    let c1 = u16::from_le_bytes([block[2], block[3]]);
    let indices = u32::from_le_bytes([block[4], block[5], block[6], block[7]]);
    let pal = palette(c0, c1, four_only);
    for (i, p) in px.iter_mut().enumerate() {
        *p = pal[((indices >> (2 * i)) & 3) as usize];
    }
}

fn distance(a: [u8; 4], b: [f32; 3]) -> f32 {
    (0..3).map(|c| (f32::from(a[c]) - b[c]).powi(2)).sum()
}

/// Indices into `pal` for `points` (and the error); transparent texels
/// (`None`) take index 3.
fn choose(pal: &[[u8; 4]; 4], usable: usize, points: &[Option<[f32; 3]>; 16]) -> (u32, f32) {
    let mut indices = 0u32;
    let mut error = 0.0;
    for (i, p) in points.iter().enumerate() {
        let k = match p {
            None => 3,
            Some(p) => {
                let (k, e) = (0..usable)
                    .map(|k| (k, distance(pal[k], *p)))
                    .min_by(|a, b| a.1.total_cmp(&b.1))
                    .unwrap_or((0, 0.0));
                error += e;
                k
            }
        };
        indices |= (k as u32) << (2 * i);
    }
    (indices, error)
}

/// A colour block from 16 texels. With `bc1_alpha`, texels whose alpha is
/// below 128 are written transparent (the three-colour mode); otherwise
/// alpha is ignored and the four-colour mode is used.
pub(crate) fn encode_colour(px: &[[u8; 4]; 16], bc1_alpha: bool) -> [u8; 8] {
    let mut points: [Option<[f32; 3]>; 16] = [None; 16];
    let mut opaque = Vec::with_capacity(16);
    for (i, p) in px.iter().enumerate() {
        if !bc1_alpha || p[3] >= 128 {
            let v = [f32::from(p[0]), f32::from(p[1]), f32::from(p[2])];
            points[i] = Some(v);
            opaque.push(v);
        }
    }
    let three = opaque.len() < 16;
    if opaque.is_empty() {
        // All transparent: c0 == c1 is the three-colour mode, index 3 each.
        return [0, 0, 0, 0, 0xff, 0xff, 0xff, 0xff];
    }
    let (mean, axis) = principal(&opaque);
    let t: Vec<f32> = opaque.iter().map(|p| (0..3).map(|c| (p[c] - mean[c]) * axis[c]).sum()).collect();
    let (lo, hi) = t.iter().fold((f32::MAX, f32::MIN), |(l, h), &v| (l.min(v), h.max(v)));
    let at = |s: f32| -> [f32; 3] { [mean[0] + axis[0] * s, mean[1] + axis[1] * s, mean[2] + axis[2] * s] };
    let mut best = finish(pack565(at(hi)), pack565(at(lo)), three, &points);
    // One least-squares pass on the chosen indices (four-colour mode).
    if !three {
        let weight = [0.0, 1.0, 1.0 / 3.0, 2.0 / 3.0];
        let idx = u32::from_le_bytes([best.0[4], best.0[5], best.0[6], best.0[7]]);
        let c0 = u16::from_le_bytes([best.0[0], best.0[1]]);
        let c1 = u16::from_le_bytes([best.0[2], best.0[3]]);
        if c0 != c1 {
            let ts: Vec<f32> = (0..16).map(|i| weight[((idx >> (2 * i)) & 3) as usize]).collect();
            if let Some((a, b)) = least_squares(&opaque, &ts) {
                let refined = finish(pack565(a), pack565(b), false, &points);
                if refined.1 < best.1 {
                    best = refined;
                }
            }
        }
    }
    best.0
}

/// The block of endpoints `c0`, `c1` put in the order the mode needs, its
/// indices and its error.
fn finish(mut c0: u16, mut c1: u16, three: bool, points: &[Option<[f32; 3]>; 16]) -> ([u8; 8], f32) {
    // Four colours need c0 > c1; three (with transparency) c0 <= c1.
    if (!three && c0 < c1) || (three && c0 > c1) {
        std::mem::swap(&mut c0, &mut c1);
    }
    let pal = palette(c0, c1, false);
    let usable = if c0 > c1 { 4 } else { 3 };
    let (indices, error) = choose(&pal, usable, points);
    let mut out = [0u8; 8];
    out[..2].copy_from_slice(&c0.to_le_bytes());
    out[2..4].copy_from_slice(&c1.to_le_bytes());
    out[4..].copy_from_slice(&indices.to_le_bytes());
    (out, error)
}

fn bc4_palette(a0: u8, a1: u8) -> [u8; 8] {
    let (a, b) = (u32::from(a0), u32::from(a1));
    let mut p = [a0, a1, 0, 0, 0, 0, 0, 255];
    if a0 > a1 {
        for i in 2..8u32 {
            p[i as usize] = (((8 - i) * a + (i - 1) * b + 3) / 7) as u8;
        }
    } else {
        for i in 2..6u32 {
            p[i as usize] = (((6 - i) * a + (i - 1) * b + 2) / 5) as u8;
        }
        p[6] = 0;
    }
    p
}

/// A BC4 block's 16 values.
pub(crate) fn decode_bc4(block: &[u8]) -> [u8; 16] {
    let pal = bc4_palette(block[0], block[1]);
    let mut bits = 0u64;
    for (i, b) in block[2..8].iter().enumerate() {
        bits |= u64::from(*b) << (8 * i);
    }
    let mut out = [0u8; 16];
    for (i, o) in out.iter_mut().enumerate() {
        *o = pal[((bits >> (3 * i)) & 7) as usize];
    }
    out
}

/// A BC4 block from 16 values: the eight-value mode between their extremes.
pub(crate) fn encode_bc4(v: &[u8; 16]) -> [u8; 8] {
    let (lo, hi) = (*v.iter().min().unwrap_or(&0), *v.iter().max().unwrap_or(&0));
    let mut out = [hi, lo, 0, 0, 0, 0, 0, 0];
    if hi == lo {
        return out;
    }
    let pal = bc4_palette(hi, lo);
    let mut bits = 0u64;
    for (i, &x) in v.iter().enumerate() {
        let k = (0..8).min_by_key(|&k| (i32::from(pal[k]) - i32::from(x)).abs()).unwrap_or(0);
        bits |= (k as u64) << (3 * i);
    }
    for i in 0..6 {
        out[2 + i] = (bits >> (8 * i)) as u8;
    }
    out
}
