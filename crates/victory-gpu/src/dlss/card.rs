//! DLSS on a real card, with images made here: does it run, does it take the
//! textures the title has, and are the signs of what it is given right. A
//! wrong sign makes the result measurably worse than the right one.
//!
//! Run by name, one at a time, with the plugin built (`plugins/dlss`):
//! `cargo test -p victory-gpu --release --lib -- --ignored dlss::card --test-threads=1 --nocapture`

use super::*;
use crate::host::{Host, host, transition};
use windows::Win32::Graphics::Direct3D12::*;
use windows::Win32::Graphics::Dxgi::Common::*;

const RENDER: [u32; 2] = [1280, 720];
const OUT: [u32; 2] = [1920, 1080];
const READ: D3D12_RESOURCE_STATES = D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE;
const INTO: D3D12_RESOURCE_STATES = D3D12_RESOURCE_STATE_COPY_DEST;
const UAV: D3D12_RESOURCE_STATES = D3D12_RESOURCE_STATE_UNORDERED_ACCESS;

fn texture(host: &Host, [w, h]: [u32; 2], format: DXGI_FORMAT, flags: D3D12_RESOURCE_FLAGS, state: D3D12_RESOURCE_STATES) -> ID3D12Resource {
    let mut out: Option<ID3D12Resource> = None;
    // SAFETY: plain texture creation.
    unsafe {
        host.device
            .CreateCommittedResource(
                &D3D12_HEAP_PROPERTIES { Type: D3D12_HEAP_TYPE_DEFAULT, ..Default::default() },
                D3D12_HEAP_FLAG_NONE,
                &D3D12_RESOURCE_DESC {
                    Dimension: D3D12_RESOURCE_DIMENSION_TEXTURE2D,
                    Width: u64::from(w),
                    Height: h,
                    DepthOrArraySize: 1,
                    MipLevels: 1,
                    Format: format,
                    SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                    Flags: flags,
                    ..Default::default()
                },
                state,
                None,
                &mut out,
            )
            .expect("a texture");
    }
    out.expect("a texture")
}

/// Waits until the plugin has started.
fn ready() {
    assert!(loaded(), "VICTORY_DLSS_PLUGIN names the plugin");
    begin();
    loop {
        match &*ENGINE.lock().unwrap_or_else(|e| e.into_inner()) {
            Some(started) => return assert!(started.is_some(), "the plugin started on this card"),
            None => std::thread::sleep(std::time::Duration::from_millis(10)),
        }
    }
}

fn whole(r: &ID3D12Resource) -> D3D12_TEXTURE_COPY_LOCATION {
    D3D12_TEXTURE_COPY_LOCATION {
        // SAFETY: the location borrows the resource.
        pResource: unsafe { std::mem::transmute_copy(r) },
        Type: D3D12_TEXTURE_COPY_TYPE_SUBRESOURCE_INDEX,
        Anonymous: D3D12_TEXTURE_COPY_LOCATION_0 { SubresourceIndex: 0 },
    }
}

fn rows(r: &ID3D12Resource, format: DXGI_FORMAT, [w, h]: [u32; 2], texel: u32) -> D3D12_TEXTURE_COPY_LOCATION {
    D3D12_TEXTURE_COPY_LOCATION {
        // SAFETY: the location borrows the resource.
        pResource: unsafe { std::mem::transmute_copy(r) },
        Type: D3D12_TEXTURE_COPY_TYPE_PLACED_FOOTPRINT,
        Anonymous: D3D12_TEXTURE_COPY_LOCATION_0 {
            PlacedFootprint: D3D12_PLACED_SUBRESOURCE_FOOTPRINT {
                Offset: 0,
                Footprint: D3D12_SUBRESOURCE_FOOTPRINT { Format: format, Width: w, Height: h, Depth: 1, RowPitch: pitch(w, texel) },
            },
        },
    }
}

fn pitch(width: u32, texel: u32) -> u32 {
    (width * texel).next_multiple_of(256)
}

/// Half floats, for values of these tests.
fn half(v: f32) -> u16 {
    let b = v.to_bits();
    let sign = (b >> 16 & 0x8000) as u16;
    let e = (b >> 23 & 0xff) as i32 - 112;
    match e {
        ..=0 => sign,
        31.. => sign | 0x7bff,
        _ => sign | (e as u16) << 10 | (b >> 13 & 0x3ff) as u16,
    }
}

fn float(h: u16) -> f32 {
    let e = i32::from(h >> 10 & 31);
    let m = f32::from(h & 0x3ff) / 1024.0;
    let v = if e == 0 { m * 2f32.powi(-14) } else { (1.0 + m) * 2f32.powi(e - 15) };
    if h & 0x8000 != 0 { -v } else { v }
}

/// The scene at a point in output pixels: rings ever closer together (finer
/// than the rendered size resolves towards the edges), thin slanted lines
/// and a gradient, in linear light up to about 1.
fn scene(x: f32, y: f32) -> f32 {
    let (dx, dy) = (x - 960.0, y - 540.0);
    let rings = 0.5 + 0.5 * ((dx * dx + dy * dy) * 0.0012).sin();
    let across = (x * 0.9 + y * 0.37).rem_euclid(17.0);
    let line = if across < 1.6 { 0.6 } else { 0.0 };
    0.05 + 0.45 * rings + line
}

/// The half size of the square in front in a scene of two layers, and where
/// its middle starts, in output pixels.
const FRONT: f32 = 180.0;
const FRONT_AT: [f32; 2] = [700.0, 560.0];

/// How far outside the square in front, moved by `moved`, a point is
/// (negative: inside).
fn outside(x: f32, y: f32, moved: [f32; 2]) -> f32 {
    ((x - moved[0] - FRONT_AT[0]).abs()).max((y - moved[1] - FRONT_AT[1]).abs()) - FRONT
}

/// The scene moved by `moved` (output pixels); in two layers, the square in
/// front moves over one that stands still.
fn scene_at(x: f32, y: f32, moved: [f32; 2], layers: bool) -> f32 {
    if !layers {
        return scene(x - moved[0], y - moved[1]);
    }
    if outside(x, y, moved) < 0.0 {
        return 1.1 - scene((x - moved[0]) * 0.7 + 400.0, (y - moved[1]) * 0.7);
    }
    scene(x, y)
}

fn halton(mut i: u32, base: u32) -> f32 {
    let (mut f, mut r) = (1.0, 0.0);
    while i > 0 {
        f /= base as f32;
        r += f * (i % base) as f32;
        i /= base;
    }
    r
}

/// What a sequence of frames is made with, and what DLSS is told of it.
#[derive(Clone, Copy)]
struct Case {
    frames: u32,
    /// The scene moves by this, in rendered pixels a frame.
    moves: [f32; 2],
    /// What the jitter and the motion are multiplied by when told: 1 is
    /// right, if the conventions are as the module says.
    jitter_sign: [f32; 2],
    motion_sign: [f32; 2],
    /// The motion is stored in parts of the image, with the scale to pixels.
    parts: bool,
    flags: u32,
    colour: DXGI_FORMAT,
    depth: DXGI_FORMAT,
    motion: DXGI_FORMAT,
    output: DXGI_FORMAT,
    exposure: bool,
    /// Two layers, with the depth of each stored near 1 and far 0 (true) or
    /// near 0 and far 1.
    layers: Option<bool>,
}

const RIGHT: Case = Case {
    frames: 40,
    moves: [0.0; 2],
    jitter_sign: [1.0; 2],
    motion_sign: [1.0; 2],
    parts: false,
    flags: fidelity::HDR | fidelity::DEPTH_INVERTED | fidelity::AUTO_EXPOSURE,
    colour: DXGI_FORMAT_R16G16B16A16_FLOAT,
    depth: DXGI_FORMAT_R32_TYPELESS,
    motion: DXGI_FORMAT_R16G16_FLOAT,
    output: DXGI_FORMAT_R16G16B16A16_FLOAT,
    exposure: false,
    layers: None,
};

struct Seen {
    /// Against the scene at the output's size, in dB, DLSS and the stretch.
    dlss: f32,
    stretch: f32,
    /// The card's time for one evaluation, the median, in ms.
    ms: f32,
    /// Card memory taken, in MiB.
    mib: f32,
}

/// Runs a case; `None` if DLSS refused it.
fn run(case: &Case) -> Option<Seen> {
    static CONTEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(100);
    let id = CONTEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let host = host().expect("a device");
    ready();
    context_made(id, Context { flags: case.flags, max_render: RENDER, max_upscale: OUT });
    let depth_flags = match case.depth {
        DXGI_FORMAT_R32_FLOAT => D3D12_RESOURCE_FLAG_NONE,
        _ => D3D12_RESOURCE_FLAG_ALLOW_DEPTH_STENCIL,
    };
    let colour = texture(host, RENDER, case.colour, D3D12_RESOURCE_FLAG_ALLOW_RENDER_TARGET, READ);
    let depth = texture(host, RENDER, case.depth, depth_flags, READ);
    let motion = texture(host, RENDER, case.motion, D3D12_RESOURCE_FLAG_ALLOW_RENDER_TARGET, READ);
    let exposure = texture(host, [1, 1], DXGI_FORMAT_R32_FLOAT, D3D12_RESOURCE_FLAG_NONE, READ);
    let writable = D3D12_RESOURCE_FLAG_ALLOW_UNORDERED_ACCESS | D3D12_RESOURCE_FLAG_ALLOW_RENDER_TARGET;
    let output = texture(host, OUT, case.output, writable, UAV);
    let stretched = texture(host, OUT, DXGI_FORMAT_R16G16B16A16_FLOAT, writable, UAV);

    let [w, h] = RENDER;
    let upload = |size: u32| host.upload_buffer(u64::from(size)).expect("upload memory");
    let (colour_up, colour_at) = upload(pitch(w, 8) * h);
    let (motion_up, motion_at) = upload(pitch(w, 8) * h);
    let (depth_up, depth_at) = upload(pitch(w, 4) * h);
    let (exposure_up, exposure_at) = upload(256);
    // SAFETY: the buffers were made with these sizes.
    let (colour_at, motion_at, depth_at, exposure_at) = unsafe {
        (
            std::slice::from_raw_parts_mut(colour_at.cast::<u16>(), (pitch(w, 8) * h / 2) as usize),
            std::slice::from_raw_parts_mut(motion_at, (pitch(w, 8) * h) as usize),
            std::slice::from_raw_parts_mut(depth_at.cast::<f32>(), (pitch(w, 4) * h / 4) as usize),
            std::slice::from_raw_parts_mut(exposure_at.cast::<f32>(), 1),
        )
    };
    exposure_at[0] = 1.0;
    let (motion_texel, motion_copy) = match case.motion {
        DXGI_FORMAT_R16G16B16A16_FLOAT | DXGI_FORMAT_R16G16B16A16_TYPELESS => (8, DXGI_FORMAT_R16G16B16A16_FLOAT),
        DXGI_FORMAT_R32G32_FLOAT | DXGI_FORMAT_R32G32_TYPELESS => (8, DXGI_FORMAT_R32G32_FLOAT),
        _ => (4, DXGI_FORMAT_R16G16_FLOAT),
    };
    // FidelityFX: from where a texel is to where it was.
    let told = [0, 1].map(|i| -case.moves[i] * case.motion_sign[i]);
    let stored = if case.parts { [told[0] / w as f32, told[1] / h as f32] } else { told };
    let scale = if case.parts { [w as f32, h as f32] } else { [1.0; 2] };
    for y in 0..h {
        for x in 0..w {
            let at = (y * pitch(w, motion_texel) + x * motion_texel) as usize;
            if motion_copy == DXGI_FORMAT_R32G32_FLOAT {
                motion_at[at..at + 4].copy_from_slice(&stored[0].to_le_bytes());
                motion_at[at + 4..at + 8].copy_from_slice(&stored[1].to_le_bytes());
            } else {
                motion_at[at..at + 2].copy_from_slice(&half(stored[0]).to_le_bytes());
                motion_at[at + 2..at + 4].copy_from_slice(&half(stored[1]).to_le_bytes());
            }
        }
    }
    let typed = |f: DXGI_FORMAT| if f == DXGI_FORMAT_R16G16B16A16_TYPELESS { DXGI_FORMAT_R16G16B16A16_FLOAT } else { f };
    let mut stamps: Option<ID3D12QueryHeap> = None;
    // SAFETY: plain object creation.
    unsafe {
        host.device
            .CreateQueryHeap(
                &D3D12_QUERY_HEAP_DESC { Type: D3D12_QUERY_HEAP_TYPE_TIMESTAMP, Count: 2, NodeMask: 0 },
                &mut stamps,
            )
            .expect("a query heap");
    }
    let stamps = stamps.expect("a query heap");
    let stamps_back = host.read_back_buffer(16).expect("a buffer");
    let mut times = Vec::new();
    let memory = |h: &Host| h.video_memory().map_or(0, |m| m[1]);
    let layers = case.layers.is_some();
    let before = memory(host);
    let grows = OUT[0] as f32 / w as f32;
    let commands = host.new_list().expect("a list");
    commands.close();
    for n in 0..case.frames {
        let phase = n % 18 + 1;
        let jitter = [halton(phase, 2) - 0.5, halton(phase, 3) - 0.5];
        let moved = [0, 1].map(|i| case.moves[i] * n as f32);
        // FidelityFX: the image is moved by the jitter, so a texel sees the
        // scene at its middle less the jitter.
        for y in 0..h {
            for x in 0..w {
                let (sx, sy) = ((x as f32 + 0.5 - jitter[0]) * grows, (y as f32 + 0.5 - jitter[1]) * grows);
                let shift = moved.map(|m| m * grows);
                let v = scene_at(sx, sy, shift, layers);
                if let Some(inverted) = case.layers {
                    let front = outside(sx, sy, shift) < 0.0;
                    let near = if front { 0.8 } else { 0.05 };
                    depth_at[(y * pitch(w, 4) / 4 + x) as usize] = if inverted { near } else { 1.0 - near };
                    let m = if front { stored } else { [0.0; 2] }.map(|v| half(v).to_le_bytes());
                    let at = (y * pitch(w, 4) + x * 4) as usize;
                    motion_at[at..at + 2].copy_from_slice(&m[0]);
                    motion_at[at + 2..at + 4].copy_from_slice(&m[1]);
                }
                let at = (y * pitch(w, 8) / 2 + x * 4) as usize;
                colour_at[at..at + 4].copy_from_slice(&[half(v), half(v), half(v), 0x3c00]);
            }
        }
        commands.reset();
        let list = &commands.list;
        let d = Dispatch {
            context: id,
            jitter: [0, 1].map(|i| jitter[i] * case.jitter_sign[i]),
            motion_scale: scale,
            render: RENDER,
            upscale: OUT,
            pre_exposure: 1.0,
            reset: n == 0,
        };
        let inputs = Inputs {
            colour: &colour,
            depth: &depth,
            motion: &motion,
            exposure: case.exposure.then_some(&exposure),
            output: &output,
        };
        // SAFETY: an open list and live resources, waited for below.
        let done = unsafe {
            let to = |r| transition(r, READ, INTO);
            list.ResourceBarrier(&[to(&colour), to(&depth), to(&motion), to(&exposure)]);
            list.CopyTextureRegion(&whole(&colour), 0, 0, 0, &rows(&colour_up, typed(case.colour), RENDER, 8), None);
            list.CopyTextureRegion(&whole(&motion), 0, 0, 0, &rows(&motion_up, motion_copy, RENDER, motion_texel), None);
            list.CopyTextureRegion(&whole(&exposure), 0, 0, 0, &rows(&exposure_up, DXGI_FORMAT_R32_FLOAT, [1, 1], 4), None);
            if n == 0 || layers {
                // A quarter, in the depth plane's own layout.
                let bits = depth_at.as_mut_ptr();
                match case.depth {
                    DXGI_FORMAT_R24G8_TYPELESS => {
                        std::slice::from_raw_parts_mut(bits.cast::<u32>(), depth_at.len()).fill(0x40_0000)
                    }
                    DXGI_FORMAT_R16_TYPELESS => {
                        std::slice::from_raw_parts_mut(bits.cast::<u16>(), depth_at.len() * 2).fill(0x4000)
                    }
                    _ if layers => {}
                    _ => depth_at.fill(0.25),
                }
                let mut plane = D3D12_PLACED_SUBRESOURCE_FOOTPRINT::default();
                let desc = depth.GetDesc();
                host.device.GetCopyableFootprints(&desc, 0, 1, 0, Some(&mut plane), None, None, None);
                let from = D3D12_TEXTURE_COPY_LOCATION {
                    Anonymous: D3D12_TEXTURE_COPY_LOCATION_0 { PlacedFootprint: plane },
                    ..rows(&depth_up, DXGI_FORMAT_R32_FLOAT, RENDER, 4)
                };
                list.CopyTextureRegion(&whole(&depth), 0, 0, 0, &from, None);
            }
            let to = |r| transition(r, INTO, READ);
            list.ResourceBarrier(&[to(&colour), to(&depth), to(&motion), to(&exposure)]);
            list.EndQuery(&stamps, D3D12_QUERY_TYPE_TIMESTAMP, 0);
            let done = record(list, &d, &inputs);
            list.EndQuery(&stamps, D3D12_QUERY_TYPE_TIMESTAMP, 1);
            list.ResolveQueryData(&stamps, D3D12_QUERY_TYPE_TIMESTAMP, 0, 2, &stamps_back, 0);
            if n + 1 == case.frames && case.colour == DXGI_FORMAT_R16G16B16A16_FLOAT {
                assert!(crate::over::record_stretch(&host.device, list, &stretched, &colour, RENDER));
            }
            done
        };
        commands.close();
        host.execute(&[&commands]);
        // SAFETY: a plain query.
        unsafe { host.device.GetDeviceRemovedReason() }.expect("the device is still there");
        if !done {
            return None;
        }
        if n > 4 {
            let mut at = std::ptr::null_mut();
            // SAFETY: two stamps, written and waited for.
            unsafe {
                stamps_back.Map(0, None, Some(&mut at)).expect("mapped");
                let t = at.cast::<[u64; 2]>().read();
                stamps_back.Unmap(0, None);
                let hz = host.queue().GetTimestampFrequency().expect("a frequency");
                times.push((t[1] - t[0]) as f32 * 1000.0 / hz as f32);
            }
        }
    }
    let mib = memory(host).saturating_sub(before) as f32 / (1 << 20) as f32;
    times.sort_by(f32::total_cmp);
    let moved = [0, 1].map(|i| case.moves[i] * (case.frames - 1) as f32 * grows);
    let dlss = quality_of(host, &commands, &output, case.output, moved, layers);
    let stretch = quality_of(host, &commands, &stretched, DXGI_FORMAT_R16G16B16A16_FLOAT, moved, layers);
    Some(Seen { dlss, stretch, ms: times.get(times.len() / 2).copied().unwrap_or(0.0), mib })
}

/// How close `output` is to the scene moved by `moved` (output pixels), in
/// dB, away from the edges; of two layers, along the edge between them.
fn quality_of(host: &Host, commands: &crate::host::HostList, output: &ID3D12Resource, format: DXGI_FORMAT, moved: [f32; 2], layers: bool) -> f32 {
    let [w, h] = OUT;
    let texel = if format == DXGI_FORMAT_R11G11B10_FLOAT { 4 } else { 8 };
    let back = host.read_back_buffer(u64::from(pitch(w, texel) * h)).expect("a buffer");
    commands.reset();
    let copied = if format == DXGI_FORMAT_R16G16B16A16_TYPELESS { DXGI_FORMAT_R16G16B16A16_FLOAT } else { format };
    // SAFETY: an open list and live resources, waited for.
    unsafe {
        commands.list.ResourceBarrier(&[transition(output, UAV, D3D12_RESOURCE_STATE_COPY_SOURCE)]);
        commands.list.CopyTextureRegion(&rows(&back, copied, OUT, texel), 0, 0, 0, &whole(output), None);
    }
    commands.close();
    host.execute(&[commands]);
    let mut at = std::ptr::null_mut();
    // SAFETY: the buffer's size, written and waited for.
    let bytes = unsafe {
        back.Map(0, None, Some(&mut at)).expect("mapped");
        std::slice::from_raw_parts(at.cast::<u8>(), (pitch(w, texel) * h) as usize)
    };
    let (mut sum, mut count) = (0.0f64, 0u32);
    for y in 80..h - 80 {
        for x in 80..w - 80 {
            if layers && outside(x as f32, y as f32, moved).abs() > 8.0 {
                continue;
            }
            let at = (y * pitch(w, texel) + x * texel) as usize;
            let got = if texel == 4 {
                // Red: 11 bits, 6 of mantissa, as a half's 10.
                let word = u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]]);
                float(((word & 0x7ff) << 4) as u16)
            } else {
                float(u16::from_le_bytes([bytes[at], bytes[at + 1]]))
            };
            let mut want = 0.0;
            for s in 0..16 {
                let (sx, sy) = (s % 4, s / 4);
                want += scene_at(
                    x as f32 + (sx as f32 + 0.5) / 4.0,
                    y as f32 + (sy as f32 + 0.5) / 4.0,
                    moved,
                    layers,
                );
            }
            sum += f64::from(got - want / 16.0).powi(2);
            count += 1;
        }
    }
    // SAFETY: not read any more.
    unsafe { back.Unmap(0, None) };
    (10.0 * (f64::from(count) / sum.max(1e-9)).log10()) as f32
}

fn say(what: &str, seen: &Option<Seen>) {
    match seen {
        Some(s) => eprintln!(
            "{what:<46} DLSS {:5.2} dB, stretch {:5.2} dB, {:.2} ms on the card, {:+.0} MiB",
            s.dlss, s.stretch, s.ms, s.mib
        ),
        None => eprintln!("{what:<46} refused"),
    }
}

/// A still scene: DLSS with the jitter's signs as documented is better than
/// with any other, and than the stretch.
#[test]
#[ignore = "needs VICTORY_DLSS_PLUGIN and a card that runs DLSS"]
fn a_still_scene_gains_detail_with_the_jitter_as_it_is() {
    let right = run(&RIGHT);
    say("still, jitter as it is", &right);
    let right = right.expect("DLSS ran");
    for sign in [[-1.0, -1.0], [-1.0, 1.0], [1.0, -1.0]] {
        let wrong = run(&Case { jitter_sign: sign, ..RIGHT });
        say(&format!("still, jitter times {sign:?}"), &wrong);
        assert!(wrong.expect("DLSS ran").dlss + 0.5 < right.dlss, "jitter times {sign:?}");
    }
    assert!(right.dlss > right.stretch + 1.0);
}

/// A scene that moves: the same for the motion's signs, and motion in parts
/// of the image with its scale is the motion in pixels.
#[test]
#[ignore = "needs VICTORY_DLSS_PLUGIN and a card that runs DLSS"]
fn a_moving_scene_is_followed_with_the_motion_as_it_is() {
    let moving = Case { moves: [3.25, -1.75], ..RIGHT };
    let right = run(&moving);
    say("moving, motion as it is", &right);
    let right = right.expect("DLSS ran");
    let parts = run(&Case { parts: true, ..moving });
    say("moving, motion in parts of the image", &parts);
    assert!((parts.expect("DLSS ran").dlss - right.dlss).abs() < 0.5);
    for sign in [[-1.0, -1.0], [-1.0, 1.0], [1.0, -1.0], [0.0, 0.0]] {
        let wrong = run(&Case { motion_sign: sign, ..moving });
        say(&format!("moving, motion times {sign:?}"), &wrong);
        assert!(wrong.expect("DLSS ran").dlss + 0.5 < right.dlss, "motion times {sign:?}");
    }
    assert!(right.dlss > right.stretch);
}

/// A square in front that moves over what stands still: along its edge the
/// depth's direction tells which motion a pixel follows.
#[test]
#[ignore = "needs VICTORY_DLSS_PLUGIN and a card that runs DLSS"]
fn the_depths_direction_is_the_flags() {
    let plain = fidelity::HDR | fidelity::AUTO_EXPOSURE;
    let seen = |inverted: bool, told: bool| {
        let flags = plain | if told { fidelity::DEPTH_INVERTED } else { 0 };
        let seen = run(&Case { moves: [4.5, 2.25], layers: Some(inverted), flags, ..RIGHT });
        say(&format!("edge, inverted {inverted}, told inverted {told}"), &seen);
        seen.expect("DLSS ran").dlss
    };
    for inverted in [true, false] {
        let (right, wrong) = (seen(inverted, inverted), seen(inverted, !inverted));
        assert!(right > wrong, "inverted {inverted}: {right} dB told so, {wrong} dB told otherwise");
    }
}

/// The formats the title's host textures have: each gives what the usual
/// ones give.
#[test]
#[ignore = "needs VICTORY_DLSS_PLUGIN and a card that runs DLSS"]
fn the_titles_formats_are_taken() {
    let short = Case { frames: 12, ..RIGHT };
    let usual = run(&short);
    say("usual formats", &usual);
    let usual = usual.expect("DLSS ran").dlss;
    let half4 = DXGI_FORMAT_R16G16B16A16_TYPELESS;
    for (what, case) in [
        ("depth D32", Case { depth: DXGI_FORMAT_D32_FLOAT, ..short }),
        ("depth R32 colour", Case { depth: DXGI_FORMAT_R32_FLOAT, ..short }),
        ("depth R32G8X24 typeless", Case { depth: DXGI_FORMAT_R32G8X24_TYPELESS, ..short }),
        ("depth R24G8 typeless", Case { depth: DXGI_FORMAT_R24G8_TYPELESS, ..short }),
        ("depth R16 typeless", Case { depth: DXGI_FORMAT_R16_TYPELESS, ..short }),
        ("motion R16G16 typeless", Case { motion: DXGI_FORMAT_R16G16_TYPELESS, ..short }),
        ("motion R32G32", Case { motion: DXGI_FORMAT_R32G32_FLOAT, ..short }),
        ("motion R32G32 typeless", Case { motion: DXGI_FORMAT_R32G32_TYPELESS, ..short }),
        ("motion R16G16B16A16", Case { motion: DXGI_FORMAT_R16G16B16A16_FLOAT, ..short }),
        ("motion R16G16B16A16 typeless", Case { motion: half4, ..short }),
        ("colour R16G16B16A16 typeless", Case { colour: half4, ..short }),
        ("output R16G16B16A16 typeless", Case { output: half4, ..short }),
        ("output R11G11B10", Case { output: DXGI_FORMAT_R11G11B10_FLOAT, ..short }),
        ("exposure texture", Case { flags: fidelity::HDR | fidelity::DEPTH_INVERTED, exposure: true, ..short }),
        ("depth not inverted", Case { flags: fidelity::HDR | fidelity::AUTO_EXPOSURE, ..short }),
        ("dynamic size", Case { flags: short.flags | fidelity::DYNAMIC_RESOLUTION, ..short }),
    ] {
        let seen = run(&case);
        say(what, &seen);
        let seen = seen.expect("DLSS ran").dlss;
        assert!((seen - usual).abs() < 1.0, "{what}: {seen} dB, not {usual}");
    }
}

/// What the title does: its own list and textures, through the dispatch's
/// own way, and the title's state replayed after.
#[test]
#[ignore = "needs VICTORY_DLSS_PLUGIN and a card that runs DLSS"]
fn a_titles_list_and_textures_are_upscaled() {
    use crate::object::This;
    let at = std::time::Instant::now();
    let host = host().expect("a device");
    eprintln!("the host's device after {:?}", at.elapsed());
    ready();
    eprintln!("the plugin started after {:?}", at.elapsed());
    context_made(1, Context { flags: RIGHT.flags, max_render: RENDER, max_upscale: OUT });
    let made = |[w, h]: [u32; 2], format: DXGI_FORMAT, flags: i32, state: u32| {
        let heap = D3D12_HEAP_PROPERTIES { Type: D3D12_HEAP_TYPE_DEFAULT, ..Default::default() };
        let desc = D3D12_RESOURCE_DESC {
            Dimension: D3D12_RESOURCE_DIMENSION_TEXTURE2D,
            Width: u64::from(w),
            Height: h,
            DepthOrArraySize: 1,
            MipLevels: 1,
            Format: format,
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            Flags: D3D12_RESOURCE_FLAGS(flags),
            ..Default::default()
        };
        let mut out: This = std::ptr::null();
        let iid = crate::resource::test_iid();
        let hr = crate::resource::create_committed(
            std::ptr::null(),
            (&raw const heap).cast(),
            0,
            (&raw const desc).cast(),
            state,
            std::ptr::null(),
            iid,
            &mut out,
        );
        assert_eq!(hr, 0);
        out
    };
    // Targets and a depth target, as the title's are; the output is R9G9B9E5
    // (67), whose host copy is a stand-in.
    let colour = made(RENDER, DXGI_FORMAT(67), 1, 0x4);
    let depth = made(RENDER, DXGI_FORMAT_D32_FLOAT, 2, 0x10);
    let motion = made(RENDER, DXGI_FORMAT_R16G16_FLOAT, 1, 0x4);
    let output = made(OUT, DXGI_FORMAT(67), 5, 0x8);
    let red = |list: &crate::host::HostList, this: This, value: [f32; 4]| {
        crate::list::ensure_state(list, this, 0x4);
        let texture = crate::resource::host_resource(this).expect("a host copy");
        // SAFETY: an open list and a live target.
        unsafe { list.list.ClearRenderTargetView(host.rtv(Some(texture), None), &value, None) };
    };
    let (allocator, title) = crate::list::test_list();
    for frame in 0..4 {
        if frame > 0 {
            crate::list::test_reset(title, allocator);
        }
        let list = crate::list::host_list(title).expect("a host list");
        red(list, colour, [0.5, 0.25, 0.125, 1.0]);
        red(list, motion, [0.0; 4]);
        red(list, output, [0.0; 4]);
        let d = Dispatch {
            context: 1,
            jitter: [0.125, -0.25],
            motion_scale: [1.0, 1.0],
            render: RENDER,
            upscale: OUT,
            pre_exposure: 1.0,
            reset: false,
        };
        let textures = Textures {
            colour: colour.cast(),
            depth: depth.cast(),
            motion: motion.cast(),
            exposure: std::ptr::null(),
            output: output.cast(),
        };
        let called = std::time::Instant::now();
        assert!(crate::list::upscale_dlss(title.cast(), &d, &textures), "recorded on the title's list");
        eprintln!("dispatch {frame} recorded in {:?}, after {:?}", called.elapsed(), at.elapsed());
        list.close();
        host.execute(&[list]);
        // SAFETY: a plain query.
        unsafe { host.device.GetDeviceRemovedReason() }.expect("the device is still there");
    }
    eprintln!("four dispatches ran after {:?}", at.elapsed());
    let back = host.read_back_buffer(u64::from(pitch(OUT[0], 8) * OUT[1])).expect("a buffer");
    let ours = host.new_list().expect("a list");
    crate::list::ensure_state(&ours, output, 0x800);
    let texture = crate::resource::host_resource(output).expect("a host copy");
    // SAFETY: an open list and live resources, waited for; one texel in the
    // middle is read.
    unsafe {
        assert_eq!(texture.GetDesc().Format, DXGI_FORMAT_R16G16B16A16_FLOAT, "the stand-in");
        ours.list.CopyTextureRegion(&rows(&back, DXGI_FORMAT_R16G16B16A16_FLOAT, OUT, 8), 0, 0, 0, &whole(texture), None);
        ours.close();
        host.execute(&[&ours]);
        let mut at = std::ptr::null_mut();
        back.Map(0, None, Some(&mut at)).expect("mapped");
        let texel = at.cast::<u8>().add((540 * pitch(OUT[0], 8) + 960 * 8) as usize).cast::<[u16; 4]>().read_unaligned();
        back.Unmap(0, None);
        let got = texel.map(float);
        eprintln!("a title's dispatch: 0.5 0.25 0.125 became {got:?}");
        for (got, want) in got.iter().zip([0.5, 0.25, 0.125]) {
            assert!((got - want).abs() < 0.02, "{got} is not {want}");
        }
    }
}
