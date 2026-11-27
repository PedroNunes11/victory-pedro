//! Texel codecs for small textures: block-compressed BC1 to BC5 and BC7, and
//! 8-bit RGBA, BGRA and single-channel texels, decoded to RGBA8 and encoded
//! back from it. Written for the glyph texture dumps of `victory-gpu`
//! (`VICTORY_GLYPHS_DUMP`) and the glyphs plugin (`plugins/glyphs`), which
//! repaints parts of a title texture in that texture's own format.
//!
//! Decoding follows the public Direct3D block formats. Encoding aims at
//! "good and quick", not best: one principal-axis fit with one least-squares
//! pass; BC7 is written in modes 6 and 5, whichever comes closer for a block
//! (mode 6: one RGBA line with 4-bit indices; mode 5: separate RGB and alpha
//! lines with 2-bit indices). BC5 is decoded only.
//!
//! Single-channel formats (R8, A8, BC4) decode to grey `(v, v, v, 255)` and
//! encode from the red channel.

mod bc;
pub mod bc7;

/// A texel format this crate knows, by what its bytes are.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Format {
    Rgba8,
    Bgra8,
    /// B8G8R8X8: alpha is not stored (reads as 255).
    Bgrx8,
    R8,
    A8,
    Bc1,
    Bc2,
    Bc3,
    Bc4,
    Bc5,
    Bc7,
}

impl Format {
    /// The format of a `DXGI_FORMAT` value (typeless, UNORM and sRGB alike).
    pub fn from_dxgi(format: u32) -> Option<Format> {
        use Format::*;
        Some(match format {
            27..=29 => Rgba8,
            87 | 90 | 91 => Bgra8,
            88 | 92 | 93 => Bgrx8,
            60 | 61 => R8,
            65 => A8,
            70..=72 => Bc1,
            73..=75 => Bc2,
            76..=78 => Bc3,
            79..=81 => Bc4,
            82..=84 => Bc5,
            97..=99 => Bc7,
            _ => return None,
        })
    }

    /// A short name, as dump files are named.
    pub fn name(self) -> &'static str {
        use Format::*;
        match self {
            Rgba8 => "RGBA8",
            Bgra8 => "BGRA8",
            Bgrx8 => "BGRX8",
            R8 => "R8",
            A8 => "A8",
            Bc1 => "BC1",
            Bc2 => "BC2",
            Bc3 => "BC3",
            Bc4 => "BC4",
            Bc5 => "BC5",
            Bc7 => "BC7",
        }
    }

    /// Texels along one side of an element: 4 for blocks, else 1.
    pub fn block(self) -> u32 {
        if self.compressed() { 4 } else { 1 }
    }

    pub fn compressed(self) -> bool {
        use Format::*;
        matches!(self, Bc1 | Bc2 | Bc3 | Bc4 | Bc5 | Bc7)
    }

    /// Bytes of one element (a texel, or a 4x4 block).
    pub fn element_bytes(self) -> usize {
        use Format::*;
        match self {
            Rgba8 | Bgra8 | Bgrx8 => 4,
            R8 | A8 => 1,
            Bc1 | Bc4 => 8,
            Bc2 | Bc3 | Bc5 | Bc7 => 16,
        }
    }

    /// Whether [`encode_rect`] can write it.
    pub fn encodable(self) -> bool {
        self != Format::Bc5
    }

    /// One channel only (R8, A8, BC4).
    pub fn single_channel(self) -> bool {
        matches!(self, Format::R8 | Format::A8 | Format::Bc4)
    }
}

/// Bytes of one row of elements, and rows of elements, of a `width * height`
/// image (tightly packed).
pub fn level_shape(format: Format, width: u32, height: u32) -> (usize, usize) {
    let b = format.block();
    (width.max(1).div_ceil(b) as usize * format.element_bytes(), height.max(1).div_ceil(b) as usize)
}

/// Decodes a `width * height` image whose rows of elements are `pitch` bytes
/// apart into RGBA8 (`width * height * 4` bytes, rows tightly packed).
/// `None` if `bytes` is too short.
pub fn decode(format: Format, width: u32, height: u32, bytes: &[u8], pitch: usize) -> Option<Vec<u8>> {
    let (row, rows) = level_shape(format, width, height);
    if pitch < row || bytes.len() < pitch * (rows - 1) + row {
        return None;
    }
    let (w, h) = (width.max(1) as usize, height.max(1) as usize);
    let mut out = vec![0u8; w * h * 4];
    let n = format.element_bytes();
    if !format.compressed() {
        for y in 0..h {
            let src = &bytes[y * pitch..y * pitch + row];
            let dst = &mut out[y * w * 4..(y + 1) * w * 4];
            for x in 0..w {
                let s = &src[x * n..x * n + n];
                let px: [u8; 4] = match format {
                    Format::Rgba8 => [s[0], s[1], s[2], s[3]],
                    Format::Bgra8 => [s[2], s[1], s[0], s[3]],
                    Format::Bgrx8 => [s[2], s[1], s[0], 255],
                    _ => [s[0], s[0], s[0], 255],
                };
                dst[x * 4..x * 4 + 4].copy_from_slice(&px);
            }
        }
        return Some(out);
    }
    for by in 0..rows {
        for bx in 0..row / n {
            let block = &bytes[by * pitch + bx * n..by * pitch + bx * n + n];
            let texels = decode_block(format, block);
            for j in 0..4 {
                let y = by * 4 + j;
                if y >= h {
                    break;
                }
                for i in 0..4 {
                    let x = bx * 4 + i;
                    if x < w {
                        out[(y * w + x) * 4..(y * w + x) * 4 + 4].copy_from_slice(&texels[j * 4 + i]);
                    }
                }
            }
        }
    }
    Some(out)
}

/// One block's 16 texels, row by row.
pub fn decode_block(format: Format, block: &[u8]) -> [[u8; 4]; 16] {
    let mut px = [[0u8; 4]; 16];
    match format {
        Format::Bc1 => bc::decode_colour(&block[..8], &mut px, false),
        Format::Bc2 => {
            bc::decode_colour(&block[8..16], &mut px, true);
            for (i, p) in px.iter_mut().enumerate() {
                let a = (block[i / 2] >> (4 * (i % 2))) & 15;
                p[3] = a * 17;
            }
        }
        Format::Bc3 => {
            bc::decode_colour(&block[8..16], &mut px, true);
            let a = bc::decode_bc4(&block[..8]);
            for (p, a) in px.iter_mut().zip(a) {
                p[3] = a;
            }
        }
        Format::Bc4 => {
            for (p, v) in px.iter_mut().zip(bc::decode_bc4(&block[..8])) {
                *p = [v, v, v, 255];
            }
        }
        Format::Bc5 => {
            let r = bc::decode_bc4(&block[..8]);
            let g = bc::decode_bc4(&block[8..16]);
            for i in 0..16 {
                px[i] = [r[i], g[i], 0, 255];
            }
        }
        Format::Bc7 => px = bc7::decode(block.try_into().expect("a BC7 block is 16 bytes")),
        _ => {}
    }
    px
}

/// One block of `format` from 16 texels, row by row.
pub fn encode_block(format: Format, px: &[[u8; 4]; 16]) -> Vec<u8> {
    match format {
        Format::Bc1 => bc::encode_colour(px, true).to_vec(),
        Format::Bc2 => {
            let mut out = vec![0u8; 16];
            for (i, p) in px.iter().enumerate() {
                let a = ((u32::from(p[3]) * 15 + 127) / 255) as u8;
                out[i / 2] |= a << (4 * (i % 2));
            }
            out[8..].copy_from_slice(&bc::encode_colour(px, false));
            out
        }
        Format::Bc3 => {
            let mut a = [0u8; 16];
            for i in 0..16 {
                a[i] = px[i][3];
            }
            let mut out = bc::encode_bc4(&a).to_vec();
            out.extend(bc::encode_colour(px, false));
            out
        }
        Format::Bc4 => {
            let mut r = [0u8; 16];
            for i in 0..16 {
                r[i] = px[i][0];
            }
            bc::encode_bc4(&r).to_vec()
        }
        Format::Bc7 => bc7::encode(px).to_vec(),
        _ => Vec::new(),
    }
}

/// Encodes the elements of `bytes` (a `width * height` image in `format`,
/// rows of elements `pitch` bytes apart) that overlap `rect`
/// (`[x0, y0, x1, y1)` in texels) from `rgba` (the whole image, RGBA8, rows
/// tightly packed). Elements outside `rect` keep their bytes. A block that
/// `rect` only partly covers takes the texels of `rgba` around it as they
/// are, so `rgba` should hold the image's own texels there. `false` if the
/// format cannot be encoded or a buffer is too short.
pub fn encode_rect(
    format: Format,
    width: u32,
    height: u32,
    rgba: &[u8],
    bytes: &mut [u8],
    pitch: usize,
    rect: [u32; 4],
) -> bool {
    let (row, rows) = level_shape(format, width, height);
    let (w, h) = (width.max(1) as usize, height.max(1) as usize);
    if !format.encodable() || pitch < row || bytes.len() < pitch * (rows - 1) + row || rgba.len() < w * h * 4 {
        return false;
    }
    let [x0, y0, x1, y1] = rect.map(|v| v as usize);
    let (x1, y1) = (x1.min(w), y1.min(h));
    if x0 >= x1 || y0 >= y1 {
        return true;
    }
    let n = format.element_bytes();
    let texel = |x: usize, y: usize| -> [u8; 4] {
        let at = (y.min(h - 1) * w + x.min(w - 1)) * 4;
        [rgba[at], rgba[at + 1], rgba[at + 2], rgba[at + 3]]
    };
    if !format.compressed() {
        for y in y0..y1 {
            for x in x0..x1 {
                let p = texel(x, y);
                let d = &mut bytes[y * pitch + x * n..y * pitch + x * n + n];
                match format {
                    Format::Rgba8 => d.copy_from_slice(&p),
                    Format::Bgra8 => d.copy_from_slice(&[p[2], p[1], p[0], p[3]]),
                    Format::Bgrx8 => d.copy_from_slice(&[p[2], p[1], p[0], 255]),
                    _ => d[0] = p[0],
                }
            }
        }
        return true;
    }
    for by in y0 / 4..y1.div_ceil(4) {
        for bx in x0 / 4..x1.div_ceil(4) {
            let mut px = [[0u8; 4]; 16];
            for j in 0..4 {
                for i in 0..4 {
                    px[j * 4 + i] = texel(bx * 4 + i, by * 4 + j);
                }
            }
            let block = encode_block(format, &px);
            bytes[by * pitch + bx * n..by * pitch + bx * n + n].copy_from_slice(&block);
        }
    }
    true
}

/// A whole `width * height` RGBA8 image in `format`, rows tightly packed.
pub fn encode(format: Format, width: u32, height: u32, rgba: &[u8]) -> Option<Vec<u8>> {
    let (row, rows) = level_shape(format, width, height);
    let mut out = vec![0u8; row * rows];
    encode_rect(format, width, height, rgba, &mut out, row, [0, 0, width, height]).then_some(out)
}

/// Mean and principal axis (unit length) of `points`; the axis is the
/// luminance-like diagonal when they hardly vary.
pub(crate) fn principal<const N: usize>(points: &[[f32; N]]) -> ([f32; N], [f32; N]) {
    let mut mean = [0f32; N];
    if points.is_empty() {
        return (mean, [1.0 / (N as f32).sqrt(); N]);
    }
    for p in points {
        for c in 0..N {
            mean[c] += p[c];
        }
    }
    mean.iter_mut().for_each(|m| *m /= points.len() as f32);
    let mut cov = [[0f32; N]; N];
    for p in points {
        for a in 0..N {
            for b in 0..N {
                cov[a][b] += (p[a] - mean[a]) * (p[b] - mean[b]);
            }
        }
    }
    // Start from the channel that varies most, then power iteration.
    let mut axis = [0f32; N];
    let start = (0..N).max_by(|&a, &b| cov[a][a].total_cmp(&cov[b][b])).unwrap_or(0);
    if cov[start][start] < 1e-3 {
        return (mean, [1.0 / (N as f32).sqrt(); N]);
    }
    axis.copy_from_slice(&cov[start]);
    for _ in 0..8 {
        let mut next = [0f32; N];
        for a in 0..N {
            for b in 0..N {
                next[a] += cov[a][b] * axis[b];
            }
        }
        let len = next.iter().map(|v| v * v).sum::<f32>().sqrt();
        if len < 1e-12 {
            break;
        }
        for c in 0..N {
            axis[c] = next[c] / len;
        }
    }
    let len = axis.iter().map(|v| v * v).sum::<f32>().sqrt();
    if len < 1e-6 {
        return (mean, [1.0 / (N as f32).sqrt(); N]);
    }
    axis.iter_mut().for_each(|v| *v /= len);
    (mean, axis)
}

/// Endpoints `a`, `b` minimising the squared error of `points[i]` as
/// `a * (1 - t[i]) + b * t[i]`; `None` when the weights do not decide them.
pub(crate) fn least_squares<const N: usize>(points: &[[f32; N]], t: &[f32]) -> Option<([f32; N], [f32; N])> {
    let (mut aa, mut ab, mut bb) = (0f32, 0f32, 0f32);
    let (mut ax, mut bx) = ([0f32; N], [0f32; N]);
    for (p, &t) in points.iter().zip(t) {
        let s = 1.0 - t;
        aa += s * s;
        ab += s * t;
        bb += t * t;
        for c in 0..N {
            ax[c] += s * p[c];
            bx[c] += t * p[c];
        }
    }
    let det = aa * bb - ab * ab;
    if det.abs() < 1e-6 {
        return None;
    }
    let (mut a, mut b) = ([0f32; N], [0f32; N]);
    for c in 0..N {
        a[c] = ((bb * ax[c] - ab * bx[c]) / det).clamp(0.0, 255.0);
        b[c] = ((aa * bx[c] - ab * ax[c]) / det).clamp(0.0, 255.0);
    }
    Some((a, b))
}

#[cfg(test)]
mod tests;
