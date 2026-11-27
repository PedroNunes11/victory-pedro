//! BC7 blocks: every mode decoded (the public Direct3D 11 format); modes 6
//! and 5 encoded, whichever comes closer for a block (mode 6: one line
//! through RGBA, 4-bit indices; mode 5: a colour line and an alpha line of
//! their own, 2-bit indices each, which suits cut-out edges).

use crate::{least_squares, principal};

/// Subsets, partition bits, rotation bits, index selection bits, colour
/// bits, alpha bits, p-bits per endpoint, p-bits per subset, index bits of
/// the first and the second index set.
#[derive(Clone, Copy)]
struct Mode {
    subsets: usize,
    partition_bits: u32,
    rotation_bits: u32,
    selection_bits: u32,
    colour_bits: u32,
    alpha_bits: u32,
    endpoint_p: bool,
    subset_p: bool,
    index_bits: u32,
    index2_bits: u32,
}

#[allow(clippy::too_many_arguments)]
const fn m(s: usize, pb: u32, rb: u32, isb: u32, cb: u32, ab: u32, ep: bool, sp: bool, ib: u32, ib2: u32) -> Mode {
    Mode {
        subsets: s,
        partition_bits: pb,
        rotation_bits: rb,
        selection_bits: isb,
        colour_bits: cb,
        alpha_bits: ab,
        endpoint_p: ep,
        subset_p: sp,
        index_bits: ib,
        index2_bits: ib2,
    }
}

const MODES: [Mode; 8] = [
    m(3, 4, 0, 0, 4, 0, true, false, 3, 0),
    m(2, 6, 0, 0, 6, 0, false, true, 3, 0),
    m(3, 6, 0, 0, 5, 0, false, false, 2, 0),
    m(2, 6, 0, 0, 7, 0, true, false, 2, 0),
    m(1, 0, 2, 1, 5, 6, false, false, 2, 3),
    m(1, 0, 2, 0, 7, 8, false, false, 2, 2),
    m(1, 0, 0, 0, 7, 7, true, false, 4, 0),
    m(2, 6, 0, 0, 5, 5, true, false, 2, 0),
];

fn weights(bits: u32) -> &'static [u32] {
    match bits {
        2 => &[0, 21, 43, 64],
        3 => &[0, 9, 18, 27, 37, 46, 55, 64],
        _ => &[0, 4, 9, 13, 17, 21, 26, 30, 34, 38, 43, 47, 51, 55, 60, 64],
    }
}

/// The subset of each texel (row by row), per partition: two subsets.
pub(crate) const PARTITIONS_2: [&str; 64] = [
    "0011001100110011", "0001000100010001", "0111011101110111", "0001001100110111",
    "0000000100010011", "0011011101111111", "0001001101111111", "0000000100110111",
    "0000000000010011", "0011011111111111", "0000000101111111", "0000000000010111",
    "0001011111111111", "0000000011111111", "0000111111111111", "0000000000001111",
    "0000100011101111", "0111000100000000", "0000000010001110", "0111001100010000",
    "0011000100000000", "0000100011001110", "0000000010001100", "0111001100110001",
    "0011000100010000", "0000100010001100", "0110011001100110", "0011011001101100",
    "0001011111101000", "0000111111110000", "0111000110001110", "0011100110011100",
    "0101010101010101", "0000111100001111", "0101101001011010", "0011001111001100",
    "0011110000111100", "0101010110101010", "0110100101101001", "0101101010100101",
    "0111001111001110", "0001001111001000", "0011001001001100", "0011101111011100",
    "0110100110010110", "0011110011000011", "0110011010011001", "0000011001100000",
    "0100111001000000", "0010011100100000", "0000001001110010", "0000010011100100",
    "0110110010010011", "0011011011001001", "0110001110011100", "0011100111000110",
    "0110110011001001", "0110001100111001", "0111111010000001", "0001100011100111",
    "0000111100110011", "0011001111110000", "0010001011101110", "0100010001110111",
];

/// Three subsets.
pub(crate) const PARTITIONS_3: [&str; 64] = [
    "0011001102212222", "0001001122112221", "0000200122112211", "0222002200110111",
    "0000000011221122", "0011001100220022", "0022002211111111", "0011001122112211",
    "0000000011112222", "0000111111112222", "0000111122222222", "0012001200120012",
    "0112011201120112", "0122012201220122", "0011011211221222", "0011200122002220",
    "0001001101121122", "0111001120012200", "0000112211221122", "0022002200221111",
    "0111011102220222", "0001000122212221", "0000001101220122", "0000110022102210",
    "0122012200110000", "0012001211222222", "0110122112210110", "0000011012211221",
    "0022110211020022", "0110011020022222", "0011012201220011", "0000200022112221",
    "0000000211221222", "0222002200120011", "0011001200220222", "0120012001200120",
    "0000111122220000", "0120120120120120", "0120201212010120", "0011220011220011",
    "0011112222000011", "0101010122222222", "0000000021212121", "0022112200221122",
    "0022001100220011", "0220122102201221", "0101222222220101", "0000212121212121",
    "0101010101012222", "0222011102220111", "0002111200021112", "0000211221122112",
    "0222011101110222", "0002111211120002", "0110011001102222", "0000000021122112",
    "0110011022222222", "0022001100110022", "0022112211220022", "0000000000002112",
    "0002000100020001", "0222122202221222", "0101222222222222", "0111201122012220",
];

/// The texel whose index drops its top bit: second subset of two.
pub(crate) const ANCHOR_2: [u8; 64] = [
    15, 15, 15, 15, 15, 15, 15, 15, 15, 15, 15, 15, 15, 15, 15, 15, 15, 2, 8, 2, 2, 8, 8, 15, 2, 8, 2, 2, 8, 8, 2, 2,
    15, 15, 6, 8, 2, 8, 15, 15, 2, 8, 2, 2, 2, 15, 15, 6, 6, 2, 6, 8, 15, 15, 2, 2, 15, 15, 15, 15, 15, 2, 2, 15,
];
/// Second and third subsets of three.
pub(crate) const ANCHOR_3: [[u8; 64]; 2] = [
    [
        3, 3, 15, 15, 8, 3, 15, 15, 8, 8, 6, 6, 6, 5, 3, 3, 3, 3, 8, 15, 3, 3, 6, 10, 5, 8, 8, 6, 8, 5, 15, 15, 8, 15, 3,
        5, 6, 10, 8, 15, 15, 3, 15, 5, 15, 15, 15, 15, 3, 15, 5, 5, 5, 8, 5, 10, 5, 10, 8, 13, 15, 12, 3, 3,
    ],
    [
        15, 8, 8, 3, 15, 15, 3, 8, 15, 15, 15, 15, 15, 15, 15, 8, 15, 8, 15, 3, 15, 8, 15, 8, 3, 15, 6, 10, 15, 15, 10, 8,
        15, 3, 15, 10, 10, 8, 9, 10, 6, 15, 8, 15, 3, 6, 6, 8, 15, 3, 15, 15, 15, 15, 15, 15, 15, 15, 15, 15, 3, 15, 15,
        8,
    ],
];

fn subset_of(subsets: usize, partition: usize, texel: usize) -> usize {
    match subsets {
        2 => usize::from(PARTITIONS_2[partition].as_bytes()[texel] - b'0'),
        3 => usize::from(PARTITIONS_3[partition].as_bytes()[texel] - b'0'),
        _ => 0,
    }
}

fn anchors(subsets: usize, partition: usize) -> [usize; 3] {
    match subsets {
        2 => [0, usize::from(ANCHOR_2[partition]), 0],
        3 => [0, usize::from(ANCHOR_3[0][partition]), usize::from(ANCHOR_3[1][partition])],
        _ => [0; 3],
    }
}

struct Reader {
    bits: u128,
    at: u32,
}

impl Reader {
    fn take(&mut self, n: u32) -> u32 {
        if n == 0 {
            return 0;
        }
        let v = ((self.bits >> self.at) & ((1u128 << n) - 1)) as u32;
        self.at += n;
        v
    }
}

/// The 16 texels of a BC7 block, row by row. A block of no mode (its first
/// byte zero) is transparent black, as Direct3D decodes it.
pub fn decode(block: &[u8; 16]) -> [[u8; 4]; 16] {
    let bits = u128::from_le_bytes(*block);
    if bits & 0xff == 0 {
        return [[0; 4]; 16];
    }
    let mode = (bits & 0xff).trailing_zeros() as usize;
    let md = MODES[mode];
    let mut r = Reader { bits, at: mode as u32 + 1 };
    let partition = r.take(md.partition_bits) as usize;
    let rotation = r.take(md.rotation_bits) as usize;
    let selection = r.take(md.selection_bits);
    let count = md.subsets * 2;
    let mut ep = [[0u32; 4]; 6];
    for c in 0..4 {
        let width = if c == 3 { md.alpha_bits } else { md.colour_bits };
        for e in ep.iter_mut().take(count) {
            e[c] = r.take(width);
        }
    }
    let mut p = [0u32; 6];
    if md.endpoint_p {
        for v in p.iter_mut().take(count) {
            *v = r.take(1);
        }
    } else if md.subset_p {
        for s in 0..md.subsets {
            let v = r.take(1);
            p[2 * s] = v;
            p[2 * s + 1] = v;
        }
    }
    let has_p = md.endpoint_p || md.subset_p;
    for (e, &pb) in ep.iter_mut().zip(&p).take(count) {
        for c in 0..4 {
            let mut width = if c == 3 { md.alpha_bits } else { md.colour_bits };
            if width == 0 {
                e[c] = 255;
                continue;
            }
            let mut v = e[c];
            if has_p {
                v = (v << 1) | pb;
                width += 1;
            }
            e[c] = ((v << (8 - width)) | (v >> (2 * width - 8))) & 255;
        }
    }
    let anchor = anchors(md.subsets, partition);
    let is_anchor = |t: usize| anchor[..md.subsets].contains(&t);
    let mut first = [0u32; 16];
    for (t, v) in first.iter_mut().enumerate() {
        *v = r.take(md.index_bits - u32::from(is_anchor(t)));
    }
    let mut second = [0u32; 16];
    if md.index2_bits > 0 {
        for (t, v) in second.iter_mut().enumerate() {
            *v = r.take(md.index2_bits - u32::from(t == 0));
        }
    }
    let (ci, cw, ai, aw) = match (md.index2_bits, selection) {
        (0, _) => (&first, weights(md.index_bits), &first, weights(md.index_bits)),
        (b2, 0) => (&first, weights(md.index_bits), &second, weights(b2)),
        (b2, _) => (&second, weights(b2), &first, weights(md.index_bits)),
    };
    let mut out = [[0u8; 4]; 16];
    for t in 0..16 {
        let s = subset_of(md.subsets, partition, t);
        let (lo, hi) = (ep[2 * s], ep[2 * s + 1]);
        let mut px = [0u32; 4];
        for c in 0..4 {
            let w = if c == 3 { aw[ai[t] as usize] } else { cw[ci[t] as usize] };
            px[c] = ((64 - w) * lo[c] + w * hi[c] + 32) >> 6;
        }
        if rotation > 0 {
            px.swap(rotation - 1, 3);
        }
        out[t] = px.map(|v| v as u8);
    }
    out
}

/// An endpoint (8 bits a channel) as mode 6 stores it with p-bit `p`:
/// seven bits a channel, and what it reads back as.
fn quantize6(e: [f32; 4], p: u32) -> ([u32; 4], [u32; 4]) {
    let mut q = [0u32; 4];
    let mut back = [0u32; 4];
    for c in 0..4 {
        q[c] = ((e[c] - p as f32) / 2.0).round().clamp(0.0, 127.0) as u32;
        back[c] = (q[c] << 1) | p;
    }
    (q, back)
}

/// Indices (4 bits) for `px` between read-back endpoints `a` and `b`, and
/// the error.
fn indices6(px: &[[f32; 4]; 16], a: [u32; 4], b: [u32; 4]) -> ([u32; 16], f32) {
    let w = weights(4);
    let mut pal = [[0f32; 4]; 16];
    for k in 0..16 {
        for c in 0..4 {
            pal[k][c] = (((64 - w[k]) * a[c] + w[k] * b[c] + 32) >> 6) as f32;
        }
    }
    let mut idx = [0u32; 16];
    let mut error = 0.0;
    for t in 0..16 {
        let (k, e) = (0..16)
            .map(|k| (k, (0..4).map(|c| (pal[k][c] - px[t][c]).powi(2)).sum::<f32>()))
            .min_by(|x, y| x.1.total_cmp(&y.1))
            .unwrap_or((0, 0.0));
        idx[t] = k as u32;
        error += e;
    }
    (idx, error)
}

/// A block from 16 texels, row by row: mode 6 or mode 5, whichever reads
/// back closer (colour error weighted by alpha, plus alpha error).
pub fn encode(texels: &[[u8; 4]; 16]) -> [u8; 16] {
    let six = encode6(texels);
    let five = encode5(texels);
    if error(texels, &decode(&five)) < error(texels, &decode(&six)) { five } else { six }
}

/// Colour error where it shows (weighted by the texel's alpha) plus alpha
/// error.
fn error(want: &[[u8; 4]; 16], got: &[[u8; 4]; 16]) -> f32 {
    want.iter()
        .zip(got)
        .map(|(w, g)| {
            let d = |c: usize| (f32::from(w[c]) - f32::from(g[c])).powi(2);
            (d(0) + d(1) + d(2)) * f32::from(w[3]) / 255.0 + d(3)
        })
        .sum()
}

/// Writes bits from bit `at` up.
struct Writer {
    bits: u128,
    at: u32,
}

impl Writer {
    fn put(&mut self, v: u32, n: u32) {
        self.bits |= u128::from(v) << self.at;
        self.at += n;
    }
}

/// A mode 5 block: RGB 7 bits and alpha 8 bits an endpoint, no rotation,
/// 2-bit colour indices and 2-bit alpha indices.
pub fn encode5(texels: &[[u8; 4]; 16]) -> [u8; 16] {
    let w = weights(2);
    // The colour line through the texels that show.
    let all: Vec<[f32; 3]> = texels.iter().map(|p| [p[0], p[1], p[2]].map(f32::from)).collect();
    let shown: Vec<[f32; 3]> = texels.iter().filter(|p| p[3] > 0).map(|p| [p[0], p[1], p[2]].map(f32::from)).collect();
    let points = if shown.is_empty() { all.clone() } else { shown };
    let (mean, axis) = principal(&points);
    let t: Vec<f32> = points.iter().map(|p| (0..3).map(|c| (p[c] - mean[c]) * axis[c]).sum()).collect();
    let (lo, hi) = t.iter().fold((f32::MAX, f32::MIN), |(l, h), &v| (l.min(v), h.max(v)));
    let at = |s: f32| -> [f32; 3] { std::array::from_fn(|c| (mean[c] + axis[c] * s).clamp(0.0, 255.0)) };
    let q7 = |e: [f32; 3]| -> ([u32; 3], [u32; 3]) {
        let q = e.map(|v| (v * 127.0 / 255.0).round().clamp(0.0, 127.0) as u32);
        (q, q.map(|v| (v << 1) | (v >> 6)))
    };
    let colour_indices = |a: [u32; 3], b: [u32; 3]| -> ([u32; 16], f32) {
        let pal: [[f32; 3]; 4] = std::array::from_fn(|k| std::array::from_fn(|c| (((64 - w[k]) * a[c] + w[k] * b[c] + 32) >> 6) as f32));
        let mut idx = [0u32; 16];
        let mut err = 0.0;
        for i in 0..16 {
            let (k, e) = (0..4)
                .map(|k| (k, (0..3).map(|c| (pal[k][c] - all[i][c]).powi(2)).sum::<f32>() * f32::from(texels[i][3])))
                .min_by(|x, y| x.1.total_cmp(&y.1))
                .unwrap_or((0, 0.0));
            idx[i] = k as u32;
            err += e;
        }
        (idx, err)
    };
    let (mut qa, ra) = q7(at(lo));
    let (mut qb, rb) = q7(at(hi));
    let (mut ci, err) = colour_indices(ra, rb);
    // One least-squares pass over the texels that show.
    let ts: Vec<f32> = (0..16).filter(|&i| shown_or_all(texels, i)).map(|i| w[ci[i] as usize] as f32 / 64.0).collect();
    let pts: Vec<[f32; 3]> = (0..16).filter(|&i| shown_or_all(texels, i)).map(|i| all[i]).collect();
    if let Some((a, b)) = least_squares(&pts, &ts) {
        let (na, nra) = q7(a);
        let (nb, nrb) = q7(b);
        let (nci, nerr) = colour_indices(nra, nrb);
        if nerr < err {
            (qa, qb, ci) = (na, nb, nci);
        }
    }
    // Alpha: between its extremes.
    let (amin, amax) = texels.iter().fold((255u32, 0u32), |(l, h), p| (l.min(u32::from(p[3])), h.max(u32::from(p[3]))));
    let (mut aa, mut ab) = (amin, amax);
    let mut ai = [0u32; 16];
    for i in 0..16 {
        ai[i] = (0..4u32)
            .min_by_key(|&k| ((((64 - w[k as usize]) * aa + w[k as usize] * ab + 32) >> 6) as i32 - i32::from(texels[i][3])).abs())
            .unwrap_or(0);
    }
    // Texel 0's indices have no top bit.
    if ci[0] & 2 != 0 {
        std::mem::swap(&mut qa, &mut qb);
        ci.iter_mut().for_each(|k| *k = 3 - *k);
    }
    if ai[0] & 2 != 0 {
        std::mem::swap(&mut aa, &mut ab);
        ai.iter_mut().for_each(|k| *k = 3 - *k);
    }
    let mut out = Writer { bits: 1 << 5, at: 6 };
    out.put(0, 2);
    for c in 0..3 {
        out.put(qa[c], 7);
        out.put(qb[c], 7);
    }
    out.put(aa, 8);
    out.put(ab, 8);
    for (i, &k) in ci.iter().enumerate() {
        out.put(k, if i == 0 { 1 } else { 2 });
    }
    for (i, &k) in ai.iter().enumerate() {
        out.put(k, if i == 0 { 1 } else { 2 });
    }
    out.bits.to_le_bytes()
}

/// Whether texel `i` takes part in the colour fit: it shows, or none does.
fn shown_or_all(texels: &[[u8; 4]; 16], i: usize) -> bool {
    texels[i][3] > 0 || texels.iter().all(|p| p[3] == 0)
}

/// A mode 6 block from 16 texels, row by row.
pub fn encode6(texels: &[[u8; 4]; 16]) -> [u8; 16] {
    let px: [[f32; 4]; 16] = texels.map(|p| p.map(f32::from));
    let (mean, axis) = principal(&px);
    let t: Vec<f32> = px.iter().map(|p| (0..4).map(|c| (p[c] - mean[c]) * axis[c]).sum()).collect();
    let (lo, hi) = t.iter().fold((f32::MAX, f32::MIN), |(l, h), &v| (l.min(v), h.max(v)));
    let at = |s: f32| -> [f32; 4] { std::array::from_fn(|c| (mean[c] + axis[c] * s).clamp(0.0, 255.0)) };
    // Each pair of p-bits is tried: a flat colour whose channels differ in
    // parity is met (within one) only between two endpoints.
    let fit = |a: [f32; 4], b: [f32; 4]| {
        let mut best: Option<(([u32; 4], u32), ([u32; 4], u32), [u32; 16], f32)> = None;
        for (pa, pb) in [(0, 0), (1, 1), (0, 1), (1, 0)] {
            let (qa, ra) = quantize6(a, pa);
            let (qb, rb) = quantize6(b, pb);
            let (idx, err) = indices6(&px, ra, rb);
            if best.as_ref().is_none_or(|b| err < b.3) {
                best = Some(((qa, pa), (qb, pb), idx, err));
            }
        }
        best.expect("four tries")
    };
    let mut best = fit(at(lo), at(hi));
    for _ in 0..2 {
        let w = weights(4);
        let ts: Vec<f32> = best.2.iter().map(|&k| w[k as usize] as f32 / 64.0).collect();
        let Some((a, b)) = least_squares(&px, &ts) else { break };
        let next = fit(a, b);
        if next.3 < best.3 {
            best = next;
        } else {
            break;
        }
    }
    let (mut e0, mut e1, mut idx, _) = best;
    // Texel 0's index has no top bit: swap the endpoints if it needs one.
    if idx[0] & 8 != 0 {
        std::mem::swap(&mut e0, &mut e1);
        idx.iter_mut().for_each(|k| *k = 15 - *k);
    }
    let mut bits: u128 = 1 << 6;
    let mut at = 7u32;
    let mut put = |v: u32, n: u32| {
        bits |= u128::from(v) << at;
        at += n;
    };
    for c in 0..4 {
        put(e0.0[c], 7);
        put(e1.0[c], 7);
    }
    put(e0.1, 1);
    put(e1.1, 1);
    for (t, &k) in idx.iter().enumerate() {
        put(k, if t == 0 { 3 } else { 4 });
    }
    bits.to_le_bytes()
}
