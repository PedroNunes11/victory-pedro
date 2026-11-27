//! A least amount of indirect light, while the title's ray-traced light
//! does not run.
//!
//! The title's indirect diffuse light comes from probes it fills by tracing
//! rays, which do not run here yet. Its compute program that resolves them
//! leaves garbage instead (bright magenta on leaves, cloth and paper,
//! blocks of colours indoors), and whatever the sun or a lamp does not reach
//! directly is black. Right after that program, its output is replaced by a
//! constant, which the title's own lighting then applies with the surfaces'
//! colours. The program is told by a hash of the name its container carries
//! (no name of the title's in this source).
//!
//! `VICTORY_AMBIENT=<level>` sets it (0.6 by default, 0 for none); `off`
//! keeps the title's own output. `VICTORY_AMBIENT_TINT=r,g,b` sets its
//! colour (warm white by default). Less of it reaches creases, corners and
//! the underside of things, worked out from the scene's depth alone
//! (`VICTORY_AMBIENT_SHADE=strength,radius[,near,tan]`, on by default; a
//! strength of 0 turns it off). In diagnostics builds
//! `VICTORY_AMBIENT_FILE=<file>` tunes all of it in a running game: the
//! file holds `level r g b [strength radius near tan]`.
//! shortcut: the same light everywhere, from everywhere; gone once rays run.
//!
//! After Belobug/VIctory-Belobug-AMD (`ambient.rs`, GPL-3.0-or-later).

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Mutex, OnceLock};
use windows::Win32::Graphics::Direct3D::Fxc::D3DCompile;
use windows::Win32::Graphics::Direct3D::ID3DBlob;
use windows::Win32::Graphics::Direct3D12::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::core::s;

use crate::host::{HostList, host};
use crate::object::This;

/// The program's name, as its container carries it: its length and its
/// 64-bit FNV-1a, and the 3 bytes it starts with (every compute program's).
const NAME: (usize, u64, &[u8; 3]) = (31, 0x29db_9b1e_7101_035a, b"CS_");

/// Whether `container` is the program that resolves the indirect light.
pub(crate) fn resolves_light(container: &[u8]) -> bool {
    let (len, hash, start) = NAME;
    container.windows(len).any(|w| w.starts_with(start) && fnv(w) == hash)
}

fn fnv(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |h, &b| (h ^ u64::from(b)).wrapping_mul(0x100_0000_01b3))
}

/// Where it writes the indirect diffuse light (R9G9B9E5 as 32-bit
/// integers): the table's user-data slot and the dword in it.
pub(crate) const LIGHT: (u8, u32) = (1, 176);

/// 0.6 looked balanced where this was made. `VICTORY_AMBIENT=off` leaves
/// what the title's program wrote; 0 is no indirect diffuse light at all.
const DEFAULT: f32 = 0.6;

/// The level; `None` if off.
pub(crate) fn level() -> Option<f32> {
    static LEVEL: OnceLock<f32> = OnceLock::new();
    let level = *LEVEL.get_or_init(|| {
        let set = std::env::var("VICTORY_AMBIENT").unwrap_or_default();
        if set.trim() == "off" {
            return -1.0;
        }
        set.trim().parse().ok().filter(|l: &f32| l.is_finite() && *l >= 0.0).unwrap_or(DEFAULT)
    });
    (level >= 0.0).then_some(level)
}

/// Warm white.
const TINT: [f32; 3] = [1.0, 0.98, 0.94];

/// `r,g,b`, or warm white if it is not three numbers.
fn parse_tint(set: &str) -> [f32; 3] {
    let set: Vec<f32> = set
        .split(',')
        .filter_map(|v| v.trim().parse().ok())
        .filter(|v: &f32| v.is_finite())
        .collect();
    match set[..] {
        [r, g, b] => [r, g, b],
        _ => TINT,
    }
}

/// The light's colour: warm white, or what `VICTORY_AMBIENT_TINT=r,g,b`
/// gives, or what `VICTORY_AMBIENT_FILE` last gave.
fn tint() -> [f32; 3] {
    if let Some((_, tint)) = *LIVE.lock().unwrap_or_else(|e| e.into_inner()) {
        return tint;
    }
    static SET: OnceLock<[f32; 3]> = OnceLock::new();
    *SET.get_or_init(|| parse_tint(&std::env::var("VICTORY_AMBIENT_TINT").unwrap_or_default()))
}

/// The level and colour last read from `VICTORY_AMBIENT_FILE`, if any.
static LIVE: Mutex<Option<(f32, [f32; 3])>> = Mutex::new(None);

/// Where the resolve reads the scene's depth (reversed, 32-bit float, with
/// a stencil plane): the table's user-data slot and the dword in it.
pub(crate) const DEPTH: (u8, u32) = (0, 464);

/// Strength (0 for none), the radius looked at in metres, the near plane's
/// distance and the tangent of half the vertical field of view (the last
/// two are guesses the picture is not sensitive to).
const SHADE: [f32; 4] = [1.0, 0.6, 0.1, 0.45];

/// [`SHADE`], with the values `s,r,n,t` gives in turn in place of its own.
fn parse_shade(set: &str) -> [f32; 4] {
    let mut shade = SHADE;
    for (to, v) in shade.iter_mut().zip(set.split(',')) {
        if let Ok(v) = v.trim().parse::<f32>()
            && v.is_finite()
        {
            *to = v;
        }
    }
    shade
}

/// The shading's values last read from `VICTORY_AMBIENT_FILE` (the four
/// after its first four), if any.
static LIVE_SHADE: Mutex<Option<[f32; 4]>> = Mutex::new(None);

/// How much less of the light reaches creases and corners, worked out from
/// the depth image alone (`VICTORY_AMBIENT_SHADE=s,r,n,t`, see [`SHADE`]).
/// shortcut: stands in for the occlusion the title's traced probes carry.
fn shade() -> [f32; 4] {
    if let Some(live) = *LIVE_SHADE.lock().unwrap_or_else(|e| e.into_inner()) {
        return live;
    }
    static SET: OnceLock<[f32; 4]> = OnceLock::new();
    *SET.get_or_init(|| parse_shade(&std::env::var("VICTORY_AMBIENT_SHADE").unwrap_or_default()))
}

/// The level from `VICTORY_AMBIENT_FILE`, if it gave one; starts the thread
/// that reads the file twice a second (diagnostics builds only), to tune
/// the light by eye in a running game.
fn live_level() -> Option<f32> {
    static STARTED: std::sync::Once = std::sync::Once::new();
    STARTED.call_once(|| {
        if !cfg!(feature = "diagnostics") {
            return;
        }
        let Some(file) = std::env::var_os("VICTORY_AMBIENT_FILE") else {
            return;
        };
        let _ = std::thread::Builder::new().name("victory-ambient".into()).spawn(move || {
            loop {
                let read: Vec<f32> = std::fs::read_to_string(&file)
                    .unwrap_or_default()
                    .split_whitespace()
                    .filter_map(|v| v.parse().ok())
                    .filter(|v: &f32| v.is_finite())
                    .collect();
                if let [level, r, g, b, ref rest @ ..] = read[..] {
                    if let [s, radius, n, t] = rest[..] {
                        *LIVE_SHADE.lock().unwrap_or_else(|e| e.into_inner()) =
                            Some([s, radius, n, t]);
                    }
                    let new = Some((level.max(0.0), [r, g, b]));
                    let mut live = LIVE.lock().unwrap_or_else(|e| e.into_inner());
                    if *live != new {
                        tracing::info!(level, r, g, b, "ambient light set from the file");
                        *live = new;
                    }
                }
                std::thread::sleep(std::time::Duration::from_millis(500));
            }
        });
    });
    LIVE.lock().unwrap_or_else(|e| e.into_inner()).map(|(level, _)| level)
}

/// The format the shading reads `depth`'s host copy through, if it is a
/// 32-bit float depth of the light's size.
fn depth_view(depth: &ID3D12Resource, width: u64, height: u32) -> Option<DXGI_FORMAT> {
    // SAFETY: a live resource.
    let d = unsafe { depth.GetDesc() };
    if d.Dimension != D3D12_RESOURCE_DIMENSION_TEXTURE2D || (d.Width, d.Height) != (width, height) {
        return None;
    }
    match d.Format {
        DXGI_FORMAT_R32G8X24_TYPELESS => Some(DXGI_FORMAT_R32_FLOAT_X8X24_TYPELESS),
        DXGI_FORMAT_R32_TYPELESS => Some(DXGI_FORMAT_R32_FLOAT),
        _ => None,
    }
}

const SOURCE: &str = r#"
#define RS "RootConstants(num32BitConstants=8, b0), DescriptorTable(UAV(u0), SRV(t0))"
cbuffer Floor : register(b0) {
    float level; float3 tint;
    float strength; float radius; float near; float tan_y;
};
RWTexture2D<uint> light : register(u0);
Texture2D<float> depth : register(t0);
// The point seen at a texel, in metres from the eye (x right, y up, z away).
float3 seen(int2 p, float2 size) {
    float d = depth.Load(int3(clamp(p, int2(0, 0), int2(size) - 1), 0));
    float z = near / max(d, 1e-7);
    float2 ndc = (float2(p) + 0.5) / size * 2.0 - 1.0;
    return float3(ndc.x * tan_y * size.x / size.y * z, -ndc.y * tan_y * z, z);
}
// How much of the light from around reaches the texel: 1 in the open.
float open_to_light(int2 p, float2 size) {
    float3 c = seen(p, size);
    if (c.z > 400.0) return 1.0;
    float3 l = seen(p + int2(-1, 0), size), r = seen(p + int2(1, 0), size);
    float3 u = seen(p + int2(0, -1), size), d = seen(p + int2(0, 1), size);
    float3 dx = abs(l.z - c.z) < abs(r.z - c.z) ? c - l : r - c;
    float3 dy = abs(u.z - c.z) < abs(d.z - c.z) ? c - u : d - c;
    float3 n = normalize(cross(dy, dx));
    if (dot(n, c) > 0.0) n = -n;
    float px = radius * size.y / (2.0 * tan_y * c.z);
    if (px < 1.5) return 1.0;
    px = min(px, 128.0);
    float noise = frac(52.9829189 * frac(dot(float2(p), float2(0.06711056, 0.00583715))));
    float sum = 0.0;
    const int N = 16;
    for (int i = 0; i < N; i++) {
        float a = i * 2.399963 + noise * 6.2831853;
        float2 o = float2(cos(a), sin(a)) * (sqrt((i + 0.5) / N) * px);
        float3 v = seen(p + int2(round(o)), size) - c;
        float vv = dot(v, v);
        float over = dot(v, n) - 0.004 * c.z - 0.005;
        sum += max(over, 0.0) / sqrt(vv + 1e-6) * saturate(1.0 - vv / (radius * radius));
    }
    return saturate(1.0 - strength * 2.0 * sum / N);
}
float3 unpack(uint v) {
    return float3(v & 511, (v >> 9) & 511, (v >> 18) & 511) * exp2(float(v >> 27) - 24.0);
}
uint pack(float3 c) {
    c = clamp(c, 0.0, 65408.0);
    float most = max(c.r, max(c.g, c.b));
    int e = clamp(int(floor(log2(max(most, 1e-9)))) + 16, 0, 31);
    float scale = exp2(float(24 - e));
    if (most * scale >= 511.5) { e = min(e + 1, 31); scale = exp2(float(24 - e)); }
    uint3 m = min(uint3(c * scale + 0.5), 511u);
    return m.r | (m.g << 9) | (m.b << 18) | (uint(e) << 27);
}
[RootSignature(RS)]
[numthreads(8, 8, 1)]
void main(uint3 at : SV_DispatchThreadID) {
    uint w, h;
    light.GetDimensions(w, h);
    if (at.x >= w || at.y >= h) return;
    // What the resolve left is not kept: without rays its inputs are never
    // written and it holds whatever their memory did (bright magenta on
    // translucent surfaces, blocks of colours indoors: run v6, 2026-11-25).
    float open = strength > 0.0 ? open_to_light(int2(at.xy), float2(w, h)) : 1.0;
    light[at.xy] = pack(tint * level * open);
}
"#;

/// Pairs of views (the light, then the depth), used in turn: more than the
/// passes of every list in flight.
const SLOTS: u32 = 1024;

struct Pass {
    root: ID3D12RootSignature,
    pso: ID3D12PipelineState,
    views: ID3D12DescriptorHeap,
    step: u32,
    next: AtomicU32,
}
// SAFETY: D3D12 objects are free-threaded.
unsafe impl Send for Pass {}
// SAFETY: as above.
unsafe impl Sync for Pass {}

fn make(device: &ID3D12Device) -> windows::core::Result<Pass> {
    let mut code: Option<ID3DBlob> = None;
    let mut errors: Option<ID3DBlob> = None;
    // SAFETY: the source and names live through the call; out pointers ours.
    let compiled = unsafe {
        D3DCompile(
            SOURCE.as_ptr().cast(),
            SOURCE.len(),
            s!("ambient"),
            None,
            None,
            s!("main"),
            s!("cs_5_1"),
            0,
            0,
            &mut code,
            Some(&mut errors),
        )
    };
    let text = |b: &ID3DBlob| {
        // SAFETY: the blob's own bytes.
        unsafe { std::slice::from_raw_parts(b.GetBufferPointer().cast::<u8>(), b.GetBufferSize()) }
            .to_vec()
    };
    if let Err(e) = compiled {
        if let Some(errors) = errors {
            tracing::warn!("ambient light: {}", String::from_utf8_lossy(&text(&errors)));
        }
        return Err(e);
    }
    let code = text(&code.ok_or_else(windows::core::Error::empty)?);
    // SAFETY: the root signature is inside the shader; plain creations.
    unsafe {
        let root: ID3D12RootSignature = device.CreateRootSignature(0, &code)?;
        let pso = device.CreateComputePipelineState(&D3D12_COMPUTE_PIPELINE_STATE_DESC {
            pRootSignature: std::mem::transmute_copy(&root),
            CS: D3D12_SHADER_BYTECODE {
                pShaderBytecode: code.as_ptr().cast(),
                BytecodeLength: code.len(),
            },
            ..Default::default()
        })?;
        let kind = D3D12_DESCRIPTOR_HEAP_TYPE_CBV_SRV_UAV;
        let views = device.CreateDescriptorHeap(&D3D12_DESCRIPTOR_HEAP_DESC {
            Type: kind,
            NumDescriptors: SLOTS * 2,
            Flags: D3D12_DESCRIPTOR_HEAP_FLAG_SHADER_VISIBLE,
            NodeMask: 0,
        })?;
        Ok(Pass {
            root,
            pso,
            views,
            step: device.GetDescriptorHandleIncrementSize(kind),
            next: AtomicU32::new(0),
        })
    }
}

fn uav_barrier(list: &ID3D12GraphicsCommandList) {
    let barrier = D3D12_RESOURCE_BARRIER {
        Type: D3D12_RESOURCE_BARRIER_TYPE_UAV,
        ..Default::default()
    };
    // SAFETY: an open list; the barrier names no resource.
    unsafe { list.ResourceBarrier(&[barrier]) };
}

/// Replaces what `light` holds, in `list`, after the program that wrote it
/// as an image: less of the light where `depth` (what that program read the
/// scene's depth from, in the state it read it in) shows creases. Whether
/// it recorded anything (the list's bindings are then ours).
pub(crate) fn raise(list: &HostList, light: This, depth: Option<This>) -> bool {
    let (Some(level), Some(host)) = (level(), host()) else {
        return false;
    };
    // While the ray-tracing structure builder runs whole the resolve's
    // light is the traced one: left as it is, unless
    // `VICTORY_AMBIENT_WITH_RAYS=1` (to compare).
    static WITH_RAYS: OnceLock<bool> = OnceLock::new();
    if crate::pipeline::structures_built()
        && !*WITH_RAYS.get_or_init(|| std::env::var_os("VICTORY_AMBIENT_WITH_RAYS").is_some_and(|v| v == "1"))
    {
        return false;
    }
    let Some(light) = crate::resource::host_resource(light) else {
        return false;
    };
    static PASS: OnceLock<Option<Pass>> = OnceLock::new();
    let Some(pass) = PASS
        .get_or_init(|| {
            make(&host.device)
                .inspect_err(|e| tracing::warn!(error = %e, "no ambient light"))
                .ok()
        })
        .as_ref()
    else {
        return false;
    };
    // SAFETY: a live resource.
    let d = unsafe { light.GetDesc() };
    static ONCE: std::sync::Once = std::sync::Once::new();
    if d.Format != DXGI_FORMAT_R32_UINT {
        ONCE.call_once(|| tracing::warn!(format = d.Format.0, "no ambient light: another format"));
        return false;
    }
    let [r, g, b] = tint();
    let [strength, radius, near, tan_y] = shade();
    ONCE.call_once(|| {
        tracing::info!(
            level,
            r,
            g,
            b,
            strength,
            radius,
            near,
            tan_y,
            width = d.Width,
            height = d.Height,
            "ambient light"
        );
    });
    // The depth the resolve just read: of the light's size and a 32-bit
    // float depth, or the shading is left out.
    let depth = depth.and_then(crate::resource::host_resource);
    let seen = depth.and_then(|t| depth_view(t, d.Width, d.Height));
    static SHADED: std::sync::Once = std::sync::Once::new();
    if strength > 0.0 {
        SHADED.call_once(|| match (depth, seen) {
            (_, Some(format)) => {
                tracing::info!(format = format.0, "ambient light: shaded from the scene's depth");
            }
            (Some(t), None) => {
                // SAFETY: a live resource.
                let t = unsafe { t.GetDesc() };
                tracing::warn!(
                    format = t.Format.0,
                    width = t.Width,
                    height = t.Height,
                    "ambient light: not shaded, the depth the resolve reads is not one it takes"
                );
            }
            (None, _) => {
                tracing::warn!("ambient light: not shaded, no depth where the resolve read it");
            }
        });
    }
    let depth = depth.filter(|_| seen.is_some());
    let slot = (pass.next.fetch_add(1, Ordering::Relaxed) % SLOTS) * 2;
    list.keep(light);
    if let Some(depth) = depth {
        list.keep(depth);
    }
    let l = &list.list;
    uav_barrier(l);
    // SAFETY: an open list; a live texture the list keeps, in the
    // unordered-access state as the program before left it; a view in a slot
    // no list in flight reads.
    unsafe {
        let cpu = pass.views.GetCPUDescriptorHandleForHeapStart();
        let gpu = pass.views.GetGPUDescriptorHandleForHeapStart();
        let at = D3D12_CPU_DESCRIPTOR_HANDLE {
            ptr: cpu.ptr + (slot * pass.step) as usize,
        };
        let view = D3D12_UNORDERED_ACCESS_VIEW_DESC {
            Format: DXGI_FORMAT_R32_UINT,
            ViewDimension: D3D12_UAV_DIMENSION_TEXTURE2D,
            ..Default::default()
        };
        host.device
            .CreateUnorderedAccessView(light, None::<&ID3D12Resource>, Some(&view), at);
        // Without a depth, a null view: the table is read whole.
        let read = D3D12_SHADER_RESOURCE_VIEW_DESC {
            Format: seen.unwrap_or(DXGI_FORMAT_R32_FLOAT),
            ViewDimension: D3D12_SRV_DIMENSION_TEXTURE2D,
            Shader4ComponentMapping: D3D12_DEFAULT_SHADER_4_COMPONENT_MAPPING,
            Anonymous: D3D12_SHADER_RESOURCE_VIEW_DESC_0 {
                Texture2D: D3D12_TEX2D_SRV {
                    MipLevels: 1,
                    ..Default::default()
                },
            },
        };
        let beside = D3D12_CPU_DESCRIPTOR_HANDLE {
            ptr: at.ptr + pass.step as usize,
        };
        host.device.CreateShaderResourceView(depth, Some(&read), beside);
        l.SetDescriptorHeaps(&[Some(pass.views.clone())]);
        l.SetComputeRootSignature(&pass.root);
        l.SetPipelineState(&pass.pso);
        let level = live_level().unwrap_or(level);
        let strength = if depth.is_some() { strength } else { 0.0 };
        let constants = [level, r, g, b, strength, radius, near, tan_y];
        l.SetComputeRoot32BitConstants(0, 8, constants.as_ptr().cast(), 0);
        l.SetComputeRootDescriptorTable(
            1,
            D3D12_GPU_DESCRIPTOR_HANDLE {
                ptr: gpu.ptr + u64::from(slot * pass.step),
            },
        );
        l.Dispatch((d.Width as u32).div_ceil(8), d.Height.div_ceil(8), 1);
    }
    uav_barrier(l);
    true
}

#[cfg(test)]
mod tests {
    /// The pass compiles with the system's compiler and the host takes it.
    #[test]
    fn ambient_pass_builds() {
        let Some(host) = crate::host::host() else {
            return;
        };
        super::make(&host.device).expect("the ambient pass");
    }

    /// The program is told by the hash only where the whole name matches.
    #[test]
    fn the_name_is_told_by_its_hash() {
        assert!(!super::resolves_light(b"CS_ a container with another program in it"));
        assert!(!super::resolves_light(&[0u8; 64]));
        assert_eq!(super::fnv(b""), 0xcbf2_9ce4_8422_2325);
    }

    /// A tint is three numbers, or warm white; the shading's values replace
    /// the defaults in turn, as many as are given.
    #[test]
    fn tint_and_shade_are_read() {
        assert_eq!(super::parse_tint("0.5, 0.6,0.7"), [0.5, 0.6, 0.7]);
        assert_eq!(super::parse_tint(""), super::TINT);
        assert_eq!(super::parse_tint("1,2"), super::TINT);
        assert_eq!(super::parse_shade(""), super::SHADE);
        assert_eq!(super::parse_shade("0"), [0.0, 0.6, 0.1, 0.45]);
        assert_eq!(super::parse_shade("0.5,1.2"), [0.5, 1.2, 0.1, 0.45]);
        assert_eq!(super::parse_shade("x,2,,0.3"), [1.0, 2.0, 0.1, 0.3]);
    }
}
