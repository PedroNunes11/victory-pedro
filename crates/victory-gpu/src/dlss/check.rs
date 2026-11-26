//! `VICTORY_DLSS_CHECK=<n>`: what the title hands the upscaler, said in the
//! log, so that a run can show whether the inputs are what [`super::plan`]
//! takes them for. Off unless set; every `n`th upscale dispatch (at least
//! every 128th, and the first) is looked at.
//!
//! Two lines for each dispatch looked at:
//! - at once: the context's flags, the sizes and formats of the textures,
//!   the jitter and how it moved over the last dispatches, the motion scale,
//!   the pre-exposure and the resets so far;
//! - [`super::KEPT`] dispatches later, when the list has run: the depth and
//!   motion textures read back, depth as its range and how much of it is
//!   exactly 0 and exactly 1 (the far plane says which way depth runs), motion
//!   in pixels (value times the scale) as its range, its mean length and how
//!   much of the image is exactly still.
//!
//! What the lines can and cannot say. A jitter that never changes, a motion
//! texture that is all zero while the camera moves, motion longer than the
//! image, or the far plane at the other end than the flag says, are each
//! wrong inputs. The SIGN of motion and of jitter cannot be read from one
//! frame: compare a steady pan to the right (objects go left, so with motion
//! "from where a texel is to where it was" the x values are positive) with
//! what the line says.
//!
//! shortcut: the read-back is taken for run [`super::KEPT`] dispatches after
//! it was recorded, as the features' release is. A list that never ran reads
//! as zeros, and the line says when everything read is zero.

use std::collections::VecDeque;
use std::sync::{Mutex, OnceLock};

use windows::Win32::Graphics::Direct3D12::{
    D3D12_PLACED_SUBRESOURCE_FOOTPRINT, D3D12_RESOURCE_DIMENSION_TEXTURE2D, D3D12_TEXTURE_COPY_LOCATION,
    D3D12_TEXTURE_COPY_LOCATION_0, D3D12_TEXTURE_COPY_TYPE_PLACED_FOOTPRINT, D3D12_TEXTURE_COPY_TYPE_SUBRESOURCE_INDEX,
    ID3D12GraphicsCommandList, ID3D12Resource,
};

use super::{Dispatch, KEPT, fidelity};

/// How many jitters back the line looks.
const JITTERS: usize = 64;

/// 0: off. Else every how many dispatches one is looked at.
fn every() -> u64 {
    static N: OnceLock<u64> = OnceLock::new();
    *N.get_or_init(|| {
        let n = std::env::var("VICTORY_DLSS_CHECK").ok().and_then(|v| v.trim().parse::<u64>().ok()).unwrap_or(0);
        // One read-back under way at a time.
        if n == 0 { 0 } else { n.max(2 * KEPT) }
    })
}

/// One texture's first level in the read-back buffer.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct Plane {
    pub offset: usize,
    pub pitch: usize,
    pub width: u32,
    pub height: u32,
    /// The texture's own format (`DXGI_FORMAT`).
    pub format: i32,
}

struct Pending {
    buffer: ID3D12Resource,
    size: usize,
    depth: Option<Plane>,
    motion: Option<Plane>,
    at: u64,
    d: Dispatch,
    flags: Option<u32>,
}

// SAFETY: a read-back buffer of ours, used behind one lock.
unsafe impl Send for Pending {}

struct Check {
    count: u64,
    jitters: VecDeque<[f32; 2]>,
    resets: u64,
    pending: Option<Pending>,
}

static CHECK: Mutex<Check> = Mutex::new(Check { count: 0, jitters: VecDeque::new(), resets: 0, pending: None });

/// Counts a dispatch; reads back the one looked at before, once its list
/// has run. Whether this one is to be looked at ([`copy`]).
pub(crate) fn due(d: &Dispatch) -> bool {
    let every = every();
    if every == 0 {
        return false;
    }
    let mut c = CHECK.lock().unwrap_or_else(|e| e.into_inner());
    c.count += 1;
    if c.jitters.len() == JITTERS {
        c.jitters.pop_front();
    }
    c.jitters.push_back(d.jitter);
    c.resets += u64::from(d.reset);
    let count = c.count;
    if c.pending.as_ref().is_some_and(|p| count >= p.at + KEPT) {
        if let Some(p) = c.pending.take() {
            read(&p);
        }
    }
    c.pending.is_none() && count % every == 1
}

/// The names of a context's flags.
fn flag_names(flags: u32) -> String {
    let names = [
        (fidelity::HDR, "HDR"),
        (fidelity::DISPLAY_MOTION, "motion at the output's size"),
        (fidelity::JITTERED_MOTION, "jitter in the motion"),
        (fidelity::DEPTH_INVERTED, "depth inverted (far 0)"),
        (fidelity::AUTO_EXPOSURE, "automatic exposure"),
        (fidelity::DYNAMIC_RESOLUTION, "rendered size changes"),
    ];
    let on: Vec<&str> = names.iter().filter(|(f, _)| flags & f != 0).map(|&(_, n)| n).collect();
    if on.is_empty() { String::from("none") } else { on.join(", ") }
}

/// How the last jitters moved: how many different ones, and their ranges.
pub(super) fn jitter_spread(jitters: &VecDeque<[f32; 2]>) -> (usize, [f32; 2], [f32; 2]) {
    let mut seen: Vec<[u32; 2]> = jitters.iter().map(|j| [j[0].to_bits(), j[1].to_bits()]).collect();
    seen.sort_unstable();
    seen.dedup();
    let range = |i: usize| {
        jitters.iter().fold([f32::INFINITY, f32::NEG_INFINITY], |r, j| [r[0].min(j[i]), r[1].max(j[i])])
    };
    (seen.len(), range(0), range(1))
}

/// Says the dispatch's numbers and records copies of its depth and motion
/// textures (both in the copy-source state) into `list`, to be read back
/// later.
pub(crate) fn copy(
    list: &ID3D12GraphicsCommandList,
    d: &Dispatch,
    colour: &ID3D12Resource,
    depth: &ID3D12Resource,
    motion: &ID3D12Resource,
) {
    let Some(host) = crate::host::host() else { return };
    let flags = super::context(d.context, d).map(|c| c.flags);
    // SAFETY: plain queries of live resources.
    let desc = |r: &ID3D12Resource| unsafe { r.GetDesc() };
    let foot = |r: &ID3D12Resource, base: u64| -> Option<(D3D12_PLACED_SUBRESOURCE_FOOTPRINT, u64)> {
        let desc = desc(r);
        if desc.SampleDesc.Count != 1 || desc.Dimension != D3D12_RESOURCE_DIMENSION_TEXTURE2D {
            return None;
        }
        let mut placed = D3D12_PLACED_SUBRESOURCE_FOOTPRINT::default();
        let mut total = 0u64;
        // SAFETY: one subresource's layout into our own structures.
        unsafe { host.device.GetCopyableFootprints(&desc, 0, 1, base, Some(&mut placed), None, None, Some(&mut total)) };
        (total != 0 && total != u64::MAX).then_some((placed, total))
    };
    let depth_foot = foot(depth, 0);
    let motion_base = depth_foot.map_or(0, |(_, total)| total.next_multiple_of(512));
    let motion_foot = foot(motion, motion_base);
    let size = motion_foot.map_or(motion_base, |(_, total)| motion_base + total);
    let plane = |r: &ID3D12Resource, f: Option<(D3D12_PLACED_SUBRESOURCE_FOOTPRINT, u64)>| {
        f.map(|(placed, _)| Plane {
            offset: placed.Offset as usize,
            pitch: placed.Footprint.RowPitch as usize,
            width: placed.Footprint.Width,
            height: placed.Footprint.Height,
            format: desc(r).Format.0,
        })
    };
    let mut c = CHECK.lock().unwrap_or_else(|e| e.into_inner());
    let (different, x, y) = jitter_spread(&c.jitters);
    let size_of = |r: &ID3D12Resource| {
        let d = desc(r);
        (d.Width, d.Height, d.Format.0)
    };
    tracing::info!(
        "DLSS input check (VICTORY_DLSS_CHECK), dispatch {}: context flags {} ({}); rendered {:?} for an output of {:?}; \
         colour {:?}, depth {:?}, motion {:?} (width, height, format); jitter {:?}, over the last {} dispatches {} different \
         jitters, x {:.3} to {:.3}, y {:.3} to {:.3}{}; motion scale {:?}; pre-exposure {}; reset {} ({} resets so far)",
        c.count,
        flags.map_or(String::from("not seen"), |f| format!("{f:#x}")),
        flags.map_or(String::from("the title's context was not seen being made"), flag_names),
        d.render,
        d.upscale,
        size_of(colour),
        size_of(depth),
        size_of(motion),
        d.jitter,
        c.jitters.len(),
        different,
        x[0],
        x[1],
        y[0],
        y[1],
        if c.jitters.len() >= 8 && different == 1 {
            " (THE JITTER NEVER CHANGES: the image is not moved between frames, the upscaler gets no new samples)"
        } else {
            ""
        },
        d.motion_scale,
        d.pre_exposure,
        d.reset,
        c.resets
    );
    if size == 0 {
        tracing::info!("DLSS input check: neither the depth nor the motion texture can be copied out (multisampled or not 2D)");
        return;
    }
    let Some(buffer) = host.read_back_buffer(size) else { return };
    for (texture, placed) in [(depth, depth_foot), (motion, motion_foot)] {
        let Some((placed, _)) = placed else { continue };
        // SAFETY: an open list; locations naming a live texture in the
        // copy-source state and our buffer, which is kept until read.
        unsafe {
            let to = D3D12_TEXTURE_COPY_LOCATION {
                pResource: std::mem::transmute_copy(&buffer),
                Type: D3D12_TEXTURE_COPY_TYPE_PLACED_FOOTPRINT,
                Anonymous: D3D12_TEXTURE_COPY_LOCATION_0 { PlacedFootprint: placed },
            };
            let from = D3D12_TEXTURE_COPY_LOCATION {
                pResource: std::mem::transmute_copy(texture),
                Type: D3D12_TEXTURE_COPY_TYPE_SUBRESOURCE_INDEX,
                Anonymous: D3D12_TEXTURE_COPY_LOCATION_0 { SubresourceIndex: 0 },
            };
            list.CopyTextureRegion(&to, 0, 0, 0, &from, None);
        }
    }
    c.pending = Some(Pending {
        buffer,
        size: size as usize,
        depth: plane(depth, depth_foot),
        motion: plane(motion, motion_foot),
        at: c.count,
        d: *d,
        flags,
    });
}

/// A half-precision float.
pub(super) fn half(h: u16) -> f32 {
    let (sign, exponent, mantissa) = (u32::from(h >> 15) << 31, u32::from(h >> 10) & 0x1f, u32::from(h) & 0x3ff);
    f32::from_bits(match (exponent, mantissa) {
        (0, 0) => sign,
        (0, mut m) => {
            // Without the leading one: shifted up to it.
            let mut e: i32 = -14;
            while m & 0x400 == 0 {
                m <<= 1;
                e -= 1;
            }
            sign | ((e + 127) as u32) << 23 | (m & 0x3ff) << 13
        }
        (31, m) => sign | 0x7f80_0000 | m << 13,
        (e, m) => sign | (e + 127 - 15) << 23 | m << 13,
    })
}

/// Every how many texels and rows are looked at, so that about a million
/// are.
fn step(width: u32, height: u32) -> usize {
    ((u64::from(width) * u64::from(height)) as f64 / 1e6).sqrt().ceil().max(1.0) as usize
}

#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub(super) struct DepthStats {
    pub looked: u64,
    pub min: f32,
    pub max: f32,
    pub mean: f64,
    pub zeros: u64,
    pub ones: u64,
    pub bad: u64,
}

/// The depth values of the top left `rect` of `plane` in `bytes`; `None`
/// for a format not read here.
pub(super) fn depth_stats(bytes: &[u8], plane: &Plane, rect: [u32; 2]) -> Option<DepthStats> {
    // Bytes a texel, and its value.
    let (size, value): (usize, fn(&[u8]) -> f32) = match plane.format {
        // D32, R32 (typeless, float), and the depth plane of D32S8.
        19..=21 | 39..=41 => (4, |b| f32::from_le_bytes([b[0], b[1], b[2], b[3]])),
        // D24S8: depth in the low 24 bits.
        44..=46 => (4, |b| (u32::from_le_bytes([b[0], b[1], b[2], 0])) as f32 / 16_777_215.0),
        // D16, R16 (typeless, unorm).
        53 | 55 | 56 => (2, |b| f32::from(u16::from_le_bytes([b[0], b[1]])) / 65_535.0),
        _ => return None,
    };
    let (width, height) = (rect[0].min(plane.width) as usize, rect[1].min(plane.height) as usize);
    let step = step(width as u32, height as u32);
    let mut s = DepthStats { min: f32::INFINITY, max: f32::NEG_INFINITY, ..Default::default() };
    for row in (0..height).step_by(step) {
        let start = plane.offset + row * plane.pitch;
        let line = bytes.get(start..start + width * size)?;
        for texel in line.chunks_exact(size).step_by(step) {
            let v = value(texel);
            s.looked += 1;
            if !v.is_finite() {
                s.bad += 1;
                continue;
            }
            (s.min, s.max) = (s.min.min(v), s.max.max(v));
            s.mean += f64::from(v);
            s.zeros += u64::from(v == 0.0);
            s.ones += u64::from(v == 1.0);
        }
    }
    s.mean /= (s.looked - s.bad).max(1) as f64;
    Some(s)
}

#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub(super) struct MotionStats {
    pub looked: u64,
    /// In pixels: the texel's value times the scale.
    pub x: [f32; 2],
    pub y: [f32; 2],
    pub mean_length: f64,
    pub still: u64,
    /// Longer than the image is wide.
    pub long: u64,
    pub bad: u64,
}

/// The motion of the top left `rect` of `plane` in `bytes`, in pixels;
/// `None` for a format not read here.
pub(super) fn motion_stats(bytes: &[u8], plane: &Plane, rect: [u32; 2], scale: [f32; 2]) -> Option<MotionStats> {
    let halves = |b: &[u8]| [half(u16::from_le_bytes([b[0], b[1]])), half(u16::from_le_bytes([b[2], b[3]]))];
    let floats =
        |b: &[u8]| [f32::from_le_bytes([b[0], b[1], b[2], b[3]]), f32::from_le_bytes([b[4], b[5], b[6], b[7]])];
    let (size, value): (usize, &dyn Fn(&[u8]) -> [f32; 2]) = match plane.format {
        // R16G16B16A16 (typeless, float).
        9 | 10 => (8, &halves),
        // R32G32.
        15 | 16 => (8, &floats),
        // R16G16.
        33 | 34 => (4, &halves),
        _ => return None,
    };
    let (width, height) = (rect[0].min(plane.width) as usize, rect[1].min(plane.height) as usize);
    let step = step(width as u32, height as u32);
    let mut s = MotionStats {
        x: [f32::INFINITY, f32::NEG_INFINITY],
        y: [f32::INFINITY, f32::NEG_INFINITY],
        ..Default::default()
    };
    for row in (0..height).step_by(step) {
        let start = plane.offset + row * plane.pitch;
        let line = bytes.get(start..start + width * size)?;
        for texel in line.chunks_exact(size).step_by(step) {
            let v = value(texel);
            s.looked += 1;
            if !v[0].is_finite() || !v[1].is_finite() {
                s.bad += 1;
                continue;
            }
            let (px, py) = (v[0] * scale[0], v[1] * scale[1]);
            s.x = [s.x[0].min(px), s.x[1].max(px)];
            s.y = [s.y[0].min(py), s.y[1].max(py)];
            let length = f64::from(px).hypot(f64::from(py));
            s.mean_length += length;
            s.still += u64::from(v[0] == 0.0 && v[1] == 0.0);
            s.long += u64::from(length > width as f64);
        }
    }
    s.mean_length /= (s.looked - s.bad).max(1) as f64;
    Some(s)
}

/// Reads what [`copy`] recorded and says it.
fn read(p: &Pending) {
    let mut data = std::ptr::null_mut();
    // SAFETY: a read-back buffer of ours of `size` bytes.
    let mapped = unsafe { p.buffer.Map(0, None, Some(&mut data)) };
    if mapped.is_err() || data.is_null() {
        tracing::info!("DLSS input check, dispatch {}: the read-back could not be mapped", p.at);
        return;
    }
    // SAFETY: the mapping holds `size` bytes; nothing writes it now but a
    // list that had not run, whose bytes would be read as they come.
    let bytes = unsafe { std::slice::from_raw_parts(data.cast::<u8>().cast_const(), p.size) };
    let percent = |n: u64, of: u64| 100.0 * n as f64 / of.max(1) as f64;
    let inverted = p.flags.map(|f| f & fidelity::DEPTH_INVERTED != 0);
    let depth = match p.depth.and_then(|plane| depth_stats(bytes, &plane, p.d.render).map(|s| (plane, s))) {
        Some((_, s)) => {
            let says = match inverted {
                Some(true) => "the context says far is 0",
                Some(false) => "the context says far is 1",
                None => "the context was not seen",
            };
            // The far plane (sky) is the value most of the exact ones are.
            let doubt = match inverted {
                Some(true) if percent(s.ones, s.looked) > 2.0 && s.ones > 4 * s.zeros => {
                    "; MORE OF IT IS EXACTLY 1 THAN 0: depth looks NOT inverted, against the flag"
                }
                Some(false) if percent(s.zeros, s.looked) > 2.0 && s.zeros > 4 * s.ones => {
                    "; MORE OF IT IS EXACTLY 0 THAN 1: depth looks inverted, against the flag"
                }
                _ => "",
            };
            format!(
                "depth over the rendered {}x{}: {} to {}, mean {:.4}, {:.1} % exactly 0 and {:.1} % exactly 1 ({says}{doubt}), \
                 {} not numbers",
                p.d.render[0],
                p.d.render[1],
                s.min,
                s.max,
                s.mean,
                percent(s.zeros, s.looked),
                percent(s.ones, s.looked),
                s.bad
            )
        }
        None => format!("depth not read (format {:?})", p.depth.map(|d| d.format)),
    };
    let display = p.flags.is_some_and(|f| f & fidelity::DISPLAY_MOTION != 0);
    let rect = if display { p.d.upscale } else { p.d.render };
    let mut nothing = false;
    let motion = match p.motion.and_then(|plane| motion_stats(bytes, &plane, rect, p.d.motion_scale)) {
        Some(s) => {
            nothing = s.still == s.looked;
            format!(
                "motion in pixels (value times the scale {:?}) over {}x{}: x {:.2} to {:.2}, y {:.2} to {:.2}, mean length \
                 {:.3}, {:.1} % exactly still, {:.2} % longer than the image is wide{}, {} not numbers",
                p.d.motion_scale,
                rect[0],
                rect[1],
                s.x[0],
                s.x[1],
                s.y[0],
                s.y[1],
                s.mean_length,
                percent(s.still, s.looked),
                percent(s.long, s.looked),
                if percent(s.long, s.looked) > 1.0 { " (THE SCALE LOOKS WRONG)" } else { "" },
                s.bad
            )
        }
        None => format!("motion not read (format {:?})", p.motion.map(|m| m.format)),
    };
    tracing::info!(
        "DLSS input check, dispatch {} read back: {depth}; {motion}{}",
        p.at,
        if nothing {
            "; ALL MOTION IS ZERO: right for a still camera and scene only (or the list had not run)"
        } else {
            ""
        }
    );
    // SAFETY: mapped above.
    unsafe { p.buffer.Unmap(0, None) };
}
