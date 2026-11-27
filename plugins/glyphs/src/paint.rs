//! Key caps painted over parts of one level of a title texture, in the
//! texture's own format.

use victory_bc::Format;

/// One cap to paint.
#[derive(Debug, Clone)]
pub struct Job {
    /// `[x, y, w, h]` in texels of the level the texture was hashed at
    /// (level 0 of what the engine hands over); `None`: all of it.
    pub rect: Option<[u32; 4]>,
    /// The cap's labels, longest first.
    pub labels: Vec<String>,
    pub premultiplied: bool,
}

/// Paints `jobs` into `bytes`: level `level` of a texture whose level 0 is
/// `base` (`[w, h]`), rows of elements `pitch` bytes apart. Only the
/// elements under a cap change. `false` if nothing was painted.
pub fn paint(format: Format, base: [u32; 2], level: u32, bytes: &mut [u8], pitch: usize, jobs: &[Job]) -> bool {
    if !format.encodable() || jobs.is_empty() {
        return false;
    }
    let lw = (base[0] >> level).max(1);
    let lh = (base[1] >> level).max(1);
    let Some(mut rgba) = victory_bc::decode(format, lw, lh, bytes, pitch) else {
        return false;
    };
    let (sx, sy) = (lw as f32 / base[0].max(1) as f32, lh as f32 / base[1].max(1) as f32);
    let style = if format.single_channel() { &crate::cap::MASK } else { &crate::cap::DARK };
    let mut painted = Vec::new();
    for job in jobs {
        let [x, y, w, h] = job.rect.unwrap_or([0, 0, base[0], base[1]]);
        let x0 = ((x as f32 * sx).floor() as u32).min(lw);
        let y0 = ((y as f32 * sy).floor() as u32).min(lh);
        let x1 = (((x + w) as f32 * sx).ceil() as u32).min(lw);
        let y1 = (((y + h) as f32 * sy).ceil() as u32).min(lh);
        if x1 <= x0 || y1 <= y0 {
            continue;
        }
        let (cw, ch) = (x1 - x0, y1 - y0);
        // One-channel textures take the mask premultiplied: its value is
        // the cap's coverage.
        let premultiplied = job.premultiplied || format.single_channel();
        let cap = crate::cap::render(cw, ch, &job.labels, style, premultiplied);
        for row in 0..ch {
            let src = &cap[(row * cw * 4) as usize..((row + 1) * cw * 4) as usize];
            let at = (((y0 + row) * lw + x0) * 4) as usize;
            rgba[at..at + src.len()].copy_from_slice(src);
        }
        painted.push([x0, y0, x1, y1]);
    }
    for rect in &painted {
        if !victory_bc::encode_rect(format, lw, lh, &rgba, bytes, pitch, *rect) {
            return false;
        }
    }
    !painted.is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn job(rect: Option<[u32; 4]>, label: &str) -> Job {
        Job { rect, labels: vec![label.to_string()], premultiplied: false }
    }

    #[test]
    fn a_whole_bc3_texture_becomes_a_cap() {
        let (w, h) = (64u32, 64u32);
        let (row, rows) = victory_bc::level_shape(Format::Bc3, w, h);
        let green = vec![[0u8, 200, 0, 255]; (w * h) as usize].concat();
        let mut bytes = victory_bc::encode(Format::Bc3, w, h, &green).unwrap();
        assert!(paint(Format::Bc3, [w, h], 0, &mut bytes, row, &[job(None, "E")]));
        assert_eq!(bytes.len(), row * rows);
        let back = victory_bc::decode(Format::Bc3, w, h, &bytes, row).unwrap();
        // No green left: corners are clear, the rim grey.
        assert!(back[3] < 16, "corner alpha {}", back[3]);
        let rim = &back[((2 * w + 32) * 4) as usize..][..4];
        assert!(rim[0] > 150 && (i32::from(rim[0]) - i32::from(rim[1])).abs() < 12, "rim {rim:?}");
    }

    #[test]
    fn an_atlas_cell_and_its_lower_level() {
        let (w, h) = (128u32, 64u32);
        for level in 0..3 {
            let (lw, lh) = (w >> level, h >> level);
            let (row, _) = victory_bc::level_shape(Format::Bc7, lw, lh);
            let level_fill = vec![[10u8, 20, 30, 255]; (lw * lh) as usize].concat();
            let mut bytes = victory_bc::encode(Format::Bc7, lw, lh, &level_fill).unwrap();
            let before = bytes.clone();
            assert!(paint(Format::Bc7, [w, h], level, &mut bytes, row, &[job(Some([64, 0, 64, 64]), "Q")]));
            // The left half (another glyph of the atlas) is untouched.
            let n = 16;
            for by in 0..(lh / 4) as usize {
                let left = (lw / 2 / 4) as usize;
                assert_eq!(&bytes[by * row..by * row + left * n], &before[by * row..by * row + left * n], "level {level}");
            }
            assert_ne!(bytes, before);
        }
    }

    #[test]
    fn rgba_bgra_and_masks() {
        for format in [Format::Rgba8, Format::Bgra8, Format::R8, Format::A8, Format::Bc1, Format::Bc4] {
            let (w, h) = (32u32, 32u32);
            let (row, rows) = victory_bc::level_shape(format, w, h);
            let mut bytes = vec![0u8; row * rows];
            assert!(paint(format, [w, h], 0, &mut bytes, row, &[job(None, "1")]), "{format:?}");
            let back = victory_bc::decode(format, w, h, &bytes, row).unwrap();
            // The rim, top middle, is light in every format.
            let rim = &back[((w + 16) * 4) as usize..][..4];
            assert!(rim[0] > 150, "{format:?} rim {rim:?}");
        }
        assert!(!paint(Format::Bc5, [8, 8], 0, &mut [0u8; 64], 32, &[job(None, "A")]));
    }

    #[test]
    fn rectangles_outside_the_texture_paint_nothing() {
        let mut bytes = vec![0u8; 16 * 16 * 4];
        assert!(!paint(Format::Rgba8, [16, 16], 0, &mut bytes, 64, &[job(Some([40, 40, 8, 8]), "A")]));
        assert!(bytes.iter().all(|&b| b == 0));
    }
}

/// `VICTORY_GLYPHS_PREVIEW=<folder> cargo test -- --ignored preview`: raw
/// RGBA of a few caps through BC7, BC3 and BC1 and back,
/// `<format>_<label>_<w>x<h>.rgba`.
#[cfg(test)]
#[test]
#[ignore = "writes preview files"]
fn preview() {
    let Some(dir) = std::env::var_os("VICTORY_GLYPHS_PREVIEW") else { return };
    let dir = std::path::PathBuf::from(dir);
    for (labels, w, h) in [
        (vec!["SPACE", "SPC"], 64u32, 64u32),
        (vec!["E"], 64, 64),
        (vec!["LMB"], 48, 48),
        (vec!["SHIFT", "SHF"], 32, 32),
        (vec!["WASD"], 64, 64),
        (vec!["↑"], 24, 24),
        (vec!["SPACE", "SPC"], 128, 48),
        (vec!["F10"], 16, 16),
    ] {
        let labels: Vec<String> = labels.iter().map(|s| s.to_string()).collect();
        for format in [Format::Bc7, Format::Bc3, Format::Bc1] {
            let (row, rows) = victory_bc::level_shape(format, w, h);
            let mut bytes = vec![0u8; row * rows];
            let job = Job { rect: None, labels: labels.clone(), premultiplied: false };
            assert!(paint(format, [w, h], 0, &mut bytes, row, &[job]));
            let rgba = victory_bc::decode(format, w, h, &bytes, row).unwrap();
            let name: String = labels[0].chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '_' }).collect();
            std::fs::write(dir.join(format!("{}_{name}_{w}x{h}.rgba", format.name())), rgba).unwrap();
        }
    }
}
