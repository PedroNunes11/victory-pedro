//! `amd_fidelityfx_loader_xbox.dll`: the upscale dispatch.
//!
//! The title's Series S mode renders below its output size and calls the
//! console's FidelityFX upscaler to make the output. The loader and the
//! upscaler are the title's own DLLs and run as they are, but the
//! upscaler's passes are compute pipelines in a container we do not
//! translate: its output stayed black. `ffxDispatch` alone is taken here:
//! an upscale dispatch becomes a plain stretch of the colour input
//! (`victory_gpu::list::upscale_stretch`); anything else, and an upscale we
//! could not stretch, goes to the title's DLL. With a DLSS plugin
//! (`victory_gpu::dlss`) DLSS is tried before the stretch, and
//! `ffxCreateContext` is taken too, only to see what the title makes its
//! context with: the call goes on to the title's DLL.
//!
//! The structures are the public FidelityFX API's (`ffx_api.h`,
//! `ffx_upscale.h`): a header of a type and a next pointer, and resources of
//! a pointer, a 32-byte description and a state.

use std::ffi::c_void;
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};


/// The DLL whose `ffxDispatch` is taken.
pub const LOADER: &str = "amd_fidelityfx_loader_xbox.dll";

static REAL: AtomicUsize = AtomicUsize::new(0);
static REAL_CREATE: AtomicUsize = AtomicUsize::new(0);

/// The title's own `ffxDispatch`, for what is not stretched here.
pub fn set_real_dispatch(address: usize) {
    REAL.store(address, Ordering::Relaxed);
}

/// The title's own `ffxCreateContext`.
pub fn set_real_create(address: usize) {
    REAL_CREATE.store(address, Ordering::Relaxed);
}

#[repr(C)]
struct Header {
    kind: u64,
    next: *const Header,
}

#[repr(C)]
struct Resource {
    resource: *const c_void,
    description: [u32; 8],
    state: u32,
}

#[repr(C)]
struct Upscale {
    header: Header,
    list: *const c_void,
    colour: Resource,
    depth: Resource,
    motion: Resource,
    exposure: Resource,
    reactive: Resource,
    composition: Resource,
    output: Resource,
    jitter: [f32; 2],
    motion_scale: [f32; 2],
    render: [u32; 2],
    upscale: [u32; 2],
    // (The public API's `bool`s are bytes here: the title's, not checked.)
    sharpening: u8,
    sharpness: f32,
    frame_time: f32,
    pre_exposure: f32,
    reset: u8,
    /// Near, far, vertical angle, metres to a unit.
    camera: [f32; 4],
    flags: u32,
}

/// `ffxCreateContextDescUpscale`, as far as it is read.
#[repr(C)]
struct CreateUpscale {
    header: Header,
    flags: u32,
    max_render: [u32; 2],
    max_upscale: [u32; 2],
}

const _: () = assert!(size_of::<Upscale>() == 432);
const _: () = assert!(size_of::<Resource>() == 48);
const _: () = assert!(std::mem::offset_of!(Upscale, output) == 0x138);

/// `FFX_API_RETURN_ERROR`: what a dispatch nothing could answer returns.
const ERROR: u32 = 1;

/// `FFX_API_DISPATCH_DESC_TYPE_UPSCALE`.
const UPSCALE: u64 = 0x0001_0001;

/// `FFX_API_CREATE_CONTEXT_DESC_TYPE_UPSCALE`.
const CREATE_UPSCALE: u64 = 0x0001_0000;

type Dispatch = extern "system" fn(*mut *mut c_void, *const Header) -> u32;
type Create = extern "system" fn(*mut *mut c_void, *const Header, *const c_void) -> u32;

/// The upscale description in the chain of descriptions `at` starts.
///
/// # Safety
/// Null, or a chain of the title's.
unsafe fn upscale_in<'a>(mut at: *const Header) -> Option<&'a CreateUpscale> {
    // A chain is a few long: a longer one is not one.
    for _ in 0..16 {
        // SAFETY: the caller's chain.
        let header = unsafe { at.as_ref() }?;
        if header.kind == CREATE_UPSCALE {
            // SAFETY: a description of this type.
            return Some(unsafe { &*at.cast::<CreateUpscale>() });
        }
        at = header.next;
    }
    None
}

extern "system" fn create(
    context: *mut *mut c_void,
    desc: *const Header,
    memory: *const c_void,
) -> u32 {
    let real = REAL_CREATE.load(Ordering::Relaxed);
    // SAFETY: the title's own `ffxCreateContext`, with its own arguments.
    let code = if real == 0 { ERROR } else { unsafe { std::mem::transmute::<usize, Create>(real)(context, desc, memory) } };
    // The console's upscaler refused it (an output it has no tables for): with DLSS the context is ours.
    // SAFETY: the title's description chain.
    if code != 0
        && dlss()
        && let Some(made) = unsafe { upscale_in(desc) }
        && make_own(context, made)
    {
        tracing::info!(
            max_render = ?made.max_render,
            max_upscale = ?made.max_upscale,
            refused = code,
            "the console's upscaler refused an upscale context: DLSS answers it"
        );
        return 0;
    }
    // SAFETY: the title's descriptions, and the context it was given.
    if code == 0
        && let (Some(made), Some(&id)) = unsafe { (upscale_in(desc), context.as_ref()) }
    {
        #[cfg(windows)]
        victory_gpu::dlss::context_made(
            id as usize,
            victory_gpu::dlss::Context {
                flags: made.flags,
                max_render: made.max_render,
                max_upscale: made.max_upscale,
            },
        );
        #[cfg(not(windows))]
        let _ = (made, id);
    }
    code
}

/// One upscale dispatch as DLSS is asked for it.
#[cfg(windows)]
fn for_dlss(context: usize, d: &Upscale) -> (victory_gpu::dlss::Dispatch, victory_gpu::dlss::Textures) {
    (
        victory_gpu::dlss::Dispatch {
            context,
            jitter: d.jitter,
            motion_scale: d.motion_scale,
            render: d.render,
            upscale: d.upscale,
            pre_exposure: d.pre_exposure,
            reset: d.reset != 0,
        },
        victory_gpu::dlss::Textures {
            colour: d.colour.resource,
            depth: d.depth.resource,
            motion: d.motion.resource,
            exposure: d.exposure.resource,
            output: d.output.resource,
        },
    )
}

extern "system" fn dispatch(context: *mut *mut c_void, desc: *const Header) -> u32 {
    // SAFETY: the title passes a dispatch description, whose header says
    // what follows it.
    if let Some(header) = unsafe { desc.as_ref() } {
        static SEEN: AtomicU32 = AtomicU32::new(0);
        let first = SEEN.fetch_add(1, Ordering::Relaxed) < 4;
        if header.kind == UPSCALE {
            // SAFETY: an upscale description, by its type.
            let d = unsafe { &*desc.cast::<Upscale>() };
            if first {
                tracing::info!(
                    list = ?d.list,
                    colour = ?d.colour.resource,
                    colour_size = ?(d.colour.description[2], d.colour.description[3]),
                    output = ?d.output.resource,
                    output_size = ?(d.output.description[2], d.output.description[3]),
                    render = ?d.render,
                    upscale = ?d.upscale,
                    jitter = ?d.jitter,
                    motion_scale = ?d.motion_scale,
                    motion_size = ?(d.motion.description[2], d.motion.description[3]),
                    motion_format = d.motion.description[1],
                    pre_exposure = d.pre_exposure,
                    reset = d.reset,
                    "upscale dispatch"
                );
            }
            #[cfg(windows)]
            if victory_gpu::dlss::loaded() {
                // SAFETY: the title's context, if it passes one.
                let id = unsafe { context.as_ref() }.map_or(0, |&c| c as usize);
                let (dispatch, textures) = for_dlss(id, d);
                if victory_gpu::list::upscale_dlss(d.list, &dispatch, &textures) {
                    return 0;
                }
            }
            // The stretch stands in for DLSS when DLSS cannot take a dispatch, and for
            // the title's own upscaler only when there is none: without a DLSS plugin the
            // title's FidelityFX runs as it did before this hook.
            #[cfg(windows)]
            if (dlss() || REAL.load(Ordering::Relaxed) == 0)
                && victory_gpu::list::upscale_stretch(
                d.list,
                d.colour.resource,
                d.output.resource,
                d.render,
            ) {
                return 0;
            }
            if first {
                tracing::warn!("upscale dispatch not stretched: left to the title's upscaler");
            }
        } else if first {
            tracing::info!(
                kind = format_args!("{:#x}", header.kind),
                "another FidelityFX dispatch"
            );
        }
    }
    if own(id_of(context)) {
        // A context of ours means nothing to the console's DLL.
        return ERROR;
    }
    match REAL.load(Ordering::Relaxed) {
        // Neither stretched nor passed on: the output was not written, and
        // the title is told so instead of presenting it as it is.
        0 => {
            static TOLD: std::sync::Once = std::sync::Once::new();
            TOLD.call_once(|| {
                tracing::error!(
                    "a FidelityFX dispatch was not answered: no stretch was recorded and the title's own ffxDispatch was not found"
                );
            });
            ERROR
        }
        // SAFETY: the title's own `ffxDispatch`, with its own arguments.
        real => unsafe { std::mem::transmute::<usize, Dispatch>(real)(context, desc) },
    }
}

// ---------------------------------------------------------------------------------------------
// Contexts of our own, for DLSS. On a Series X led to FidelityFX (VICTORY_UPSCALER=7) the
// console's upscaler DLL refuses an upscale context for its 2560x1440 or 3840x2160 output (it has
// tables for 1920x1080 only) and the title stops. With DLSS loaded, a refused context is made here
// instead: its dispatches go to DLSS (or the stretch), and the calls the title makes on it
// (ffxQuery, ffxConfigure, ffxDestroyContext) are answered here by the public API's rules
// (AMD FidelityFX SDK 1.1.4 / 2.x `ffx_upscale.h`, MIT). Calls on the console's contexts and
// calls without a context go to the console's DLL as before.

static REAL_DESTROY: AtomicUsize = AtomicUsize::new(0);
static REAL_QUERY: AtomicUsize = AtomicUsize::new(0);
static REAL_CONFIGURE: AtomicUsize = AtomicUsize::new(0);
static OWN: std::sync::Mutex<Vec<usize>> = std::sync::Mutex::new(Vec::new());

/// The title's own `ffxDestroyContext`, `ffxQuery` and `ffxConfigure`.
pub fn set_real_others(destroy: Option<usize>, query: Option<usize>, configure: Option<usize>) {
    REAL_DESTROY.store(destroy.unwrap_or(0), Ordering::Relaxed);
    REAL_QUERY.store(query.unwrap_or(0), Ordering::Relaxed);
    REAL_CONFIGURE.store(configure.unwrap_or(0), Ordering::Relaxed);
}

fn own(context: usize) -> bool {
    context != 0 && OWN.lock().unwrap_or_else(|e| e.into_inner()).contains(&context)
}

/// What a context slot the title passes holds (0 for none).
fn id_of(context: *mut *mut c_void) -> usize {
    // SAFETY: null, or the title's context slot.
    unsafe { context.as_ref() }.map_or(0, |&c| c as usize)
}

const RETURN_UNKNOWN_DESCTYPE: u32 = 2;
const QUERY_UPSCALE_RATIO: u64 = 0x0001_0002;
const QUERY_RENDER_SIZE: u64 = 0x0001_0003;
const QUERY_JITTER_PHASES: u64 = 0x0001_0004;
const QUERY_JITTER_OFFSET: u64 = 0x0001_0005;
const QUERY_MEMORY: u64 = 0x0001_0008;

#[repr(C)]
struct QueryRatio {
    header: Header,
    quality: u32,
    out_ratio: *mut f32,
}
#[repr(C)]
struct QueryRenderSize {
    header: Header,
    display: [u32; 2],
    quality: u32,
    out_width: *mut u32,
    out_height: *mut u32,
}
#[repr(C)]
struct QueryPhases {
    header: Header,
    render_width: u32,
    display_width: u32,
    out_count: *mut i32,
}
#[repr(C)]
struct QueryOffset {
    header: Header,
    index: i32,
    phases: i32,
    out_x: *mut f32,
    out_y: *mut f32,
}
#[repr(C)]
struct QueryMemory {
    header: Header,
    out: *mut [u64; 2],
}

/// The per-dimension ratio of a FidelityFX quality mode (native AA, quality, balanced,
/// performance, ultra performance), as the public SDK has them.
fn ratio(quality: u32) -> Option<f32> {
    [1.0, 1.5, 1.7, 2.0, 3.0].get(quality as usize).copied()
}

/// The jitter phase count for a render width against an output width (the SDK's rule).
fn phase_count(render_width: u32, display_width: u32) -> i32 {
    if render_width == 0 {
        return 1;
    }
    let r = display_width as f32 / render_width as f32;
    ((8.0 * r * r) as i32).max(1)
}

/// The radical inverse of `index` in `base`.
fn halton(index: i32, base: i32) -> f32 {
    let (mut f, mut result, mut i) = (1.0f32, 0.0f32, index);
    while i > 0 {
        f /= base as f32;
        result += f * (i % base) as f32;
        i /= base;
    }
    result
}

/// The sub-pixel jitter at `index` of a sequence of `phases` (the SDK's rule).
fn jitter(index: i32, phases: i32) -> (f32, f32) {
    let n = index.rem_euclid(phases.max(1)) + 1;
    (halton(n, 2) - 0.5, halton(n, 3) - 0.5)
}

/// Answers a query chain on a context of ours.
///
/// # Safety
/// `desc` is null or a chain of the title's query descriptions.
unsafe fn answer_query(mut desc: *const Header) -> u32 {
    let mut answered = false;
    for _ in 0..16 {
        // SAFETY: the caller's chain.
        let Some(h) = (unsafe { desc.as_ref() }) else { break };
        static TOLD: std::sync::Mutex<Vec<u64>> = std::sync::Mutex::new(Vec::new());
        {
            let mut told = TOLD.lock().unwrap_or_else(|e| e.into_inner());
            if !told.contains(&h.kind) {
                told.push(h.kind);
                tracing::info!(kind = format_args!("{:#x}", h.kind), "FidelityFX query on a DLSS context");
            }
        }
        // SAFETY (each arm): a description of that type; its out pointers are the title's.
        unsafe {
            match h.kind {
                QUERY_UPSCALE_RATIO => {
                    let q = &*desc.cast::<QueryRatio>();
                    if let (Some(r), Some(out)) = (ratio(q.quality), q.out_ratio.as_mut()) {
                        *out = r;
                        answered = true;
                    }
                }
                QUERY_RENDER_SIZE => {
                    let q = &*desc.cast::<QueryRenderSize>();
                    if let Some(r) = ratio(q.quality) {
                        if let Some(w) = q.out_width.as_mut() {
                            *w = (q.display[0] as f32 / r) as u32;
                        }
                        if let Some(hh) = q.out_height.as_mut() {
                            *hh = (q.display[1] as f32 / r) as u32;
                        }
                        answered = true;
                    }
                }
                QUERY_JITTER_PHASES => {
                    let q = &*desc.cast::<QueryPhases>();
                    if let Some(out) = q.out_count.as_mut() {
                        *out = phase_count(q.render_width, q.display_width);
                        answered = true;
                    }
                }
                QUERY_JITTER_OFFSET => {
                    let q = &*desc.cast::<QueryOffset>();
                    let (x, y) = jitter(q.index, q.phases);
                    if let Some(out) = q.out_x.as_mut() {
                        *out = x;
                    }
                    if let Some(out) = q.out_y.as_mut() {
                        *out = y;
                    }
                    answered = true;
                }
                QUERY_MEMORY => {
                    let q = &*desc.cast::<QueryMemory>();
                    if let Some(out) = q.out.as_mut() {
                        // About what a DLSS feature took on the card in the tests (docs/dlss.md).
                        *out = [96 << 20, 0];
                        answered = true;
                    }
                }
                _ => {}
            }
        }
        desc = h.next;
    }
    if answered { 0 } else { RETURN_UNKNOWN_DESCTYPE }
}

extern "system" fn query(context: *mut *mut c_void, desc: *const Header) -> u32 {
    if own(id_of(context)) {
        // SAFETY: the title's query chain.
        return unsafe { answer_query(desc) };
    }
    match REAL_QUERY.load(Ordering::Relaxed) {
        0 => ERROR,
        // SAFETY: the title's own `ffxQuery`, with its own arguments.
        real => unsafe { std::mem::transmute::<usize, Dispatch>(real)(context, desc) },
    }
}

extern "system" fn configure(context: *mut *mut c_void, desc: *const Header) -> u32 {
    if own(id_of(context)) {
        // Key-value tuning of FidelityFX's own passes has no counterpart in DLSS.
        static TOLD: std::sync::Once = std::sync::Once::new();
        TOLD.call_once(|| tracing::info!("FidelityFX configure on a DLSS context: nothing to set"));
        return 0;
    }
    match REAL_CONFIGURE.load(Ordering::Relaxed) {
        0 => ERROR,
        // SAFETY: the title's own `ffxConfigure`, with its own arguments.
        real => unsafe { std::mem::transmute::<usize, Dispatch>(real)(context, desc) },
    }
}

type Destroy = extern "system" fn(*mut *mut c_void, *const c_void) -> u32;

extern "system" fn destroy(context: *mut *mut c_void, memory: *const c_void) -> u32 {
    let id = id_of(context);
    if own(id) {
        OWN.lock().unwrap_or_else(|e| e.into_inner()).retain(|&c| c != id);
        // SAFETY: made by `make_own` with Box::into_raw, and no longer listed.
        drop(unsafe { Box::from_raw(id as *mut u64) });
        // SAFETY: the title's slot.
        if let Some(slot) = unsafe { context.as_mut() } {
            *slot = std::ptr::null_mut();
        }
        return 0;
    }
    match REAL_DESTROY.load(Ordering::Relaxed) {
        0 => ERROR,
        // SAFETY: the title's own `ffxDestroyContext`, with its own arguments.
        real => unsafe { std::mem::transmute::<usize, Destroy>(real)(context, memory) },
    }
}

/// Makes a context of ours in the title's slot `context` for the upscale description `made`.
fn make_own(context: *mut *mut c_void, made: &CreateUpscale) -> bool {
    // SAFETY: the title's slot, if it gives one.
    let Some(slot) = (unsafe { context.as_mut() }) else { return false };
    let id = Box::into_raw(Box::new(0x0d15_5c07_u64)) as usize;
    OWN.lock().unwrap_or_else(|e| e.into_inner()).push(id);
    *slot = id as *mut c_void;
    #[cfg(windows)]
    victory_gpu::dlss::context_made(
        id,
        victory_gpu::dlss::Context { flags: made.flags, max_render: made.max_render, max_upscale: made.max_upscale },
    );
    #[cfg(not(windows))]
    let _ = made;
    true
}

/// What the imports of `ffxQuery`, `ffxConfigure` and `ffxDestroyContext` are bound to with a DLSS
/// plugin.
pub fn query_address() -> usize {
    query as *const () as usize
}
/// See [`query_address`].
pub fn configure_address() -> usize {
    configure as *const () as usize
}
/// See [`query_address`].
pub fn destroy_address() -> usize {
    destroy as *const () as usize
}

#[cfg(test)]
mod own_tests {
    use super::*;
    use std::mem::offset_of;

    /// The query descriptions sit as the public API lays them out.
    #[test]
    fn query_descriptions_are_laid_out_as_the_public_api() {
        assert_eq!(offset_of!(QueryRatio, out_ratio), 24);
        assert_eq!(offset_of!(QueryRenderSize, quality), 24);
        assert_eq!(offset_of!(QueryRenderSize, out_width), 32);
        assert_eq!(offset_of!(QueryRenderSize, out_height), 40);
        assert_eq!(offset_of!(QueryPhases, out_count), 24);
        assert_eq!(offset_of!(QueryOffset, phases), 20);
        assert_eq!(offset_of!(QueryOffset, out_y), 32);
    }

    /// The SDK's jitter: 8 x the area ratio phases, Halton (2, 3) offsets in [-0.5, 0.5).
    #[test]
    fn jitter_follows_the_sdk() {
        assert_eq!(phase_count(1280, 1920), 18);
        assert_eq!(phase_count(1707, 2560), 17);
        assert_eq!(phase_count(1920, 1920), 8);
        assert_eq!(jitter(0, 8), (0.0, 1.0 / 3.0 - 0.5));
        assert_eq!(jitter(1, 8), (0.25 - 0.5, 2.0 / 3.0 - 0.5));
        assert_eq!(jitter(8, 8), jitter(0, 8));
        assert!((0..64).all(|i| {
            let (x, y) = jitter(i, 17);
            (-0.5..0.5).contains(&x) && (-0.5..0.5).contains(&y)
        }));
    }

    /// Render sizes and ratios for the quality modes; an unknown mode answers nothing.
    #[test]
    fn quality_modes_give_the_sdks_sizes() {
        let (mut w, mut h, mut r) = (0u32, 0u32, 0f32);
        let size = QueryRenderSize {
            header: Header { kind: QUERY_RENDER_SIZE, next: std::ptr::null() },
            display: [3840, 2160],
            quality: 1,
            out_width: &mut w,
            out_height: &mut h,
        };
        // SAFETY: a chain of our own.
        assert_eq!(unsafe { answer_query((&raw const size).cast()) }, 0);
        assert_eq!((w, h), (2560, 1440));
        let q = QueryRatio {
            header: Header { kind: QUERY_UPSCALE_RATIO, next: std::ptr::null() },
            quality: 3,
            out_ratio: &mut r,
        };
        // SAFETY: as above.
        assert_eq!(unsafe { answer_query((&raw const q).cast()) }, 0);
        assert_eq!(r, 2.0);
        let bad = QueryRatio {
            header: Header { kind: QUERY_UPSCALE_RATIO, next: std::ptr::null() },
            quality: 9,
            out_ratio: &mut r,
        };
        // SAFETY: as above.
        assert_eq!(unsafe { answer_query((&raw const bad).cast()) }, RETURN_UNKNOWN_DESCTYPE);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::mem::offset_of;

    /// The upscale description as the public API lays it out: what the
    /// stretch reads sits where the title wrote it.
    #[test]
    fn the_upscale_description_is_laid_out_as_the_public_api() {
        assert_eq!(size_of::<Header>(), 16);
        assert_eq!(offset_of!(Upscale, list), 0x10);
        assert_eq!(offset_of!(Upscale, colour), 0x18);
        assert_eq!(offset_of!(Upscale, depth), 0x48);
        assert_eq!(offset_of!(Upscale, motion), 0x78);
        assert_eq!(offset_of!(Upscale, output), 0x138);
        assert_eq!(offset_of!(Upscale, jitter), 0x168);
        assert_eq!(offset_of!(Upscale, motion_scale), 0x170);
        assert_eq!(offset_of!(Upscale, render), 0x178);
        assert_eq!(offset_of!(Upscale, upscale), 0x180);
        assert_eq!(offset_of!(Upscale, sharpening), 0x188);
        assert_eq!(offset_of!(Upscale, pre_exposure), 0x194);
        assert_eq!(offset_of!(Upscale, reset), 0x198);
        assert_eq!(offset_of!(Upscale, camera), 0x19c);
        assert_eq!(offset_of!(Upscale, flags), 0x1ac);
        assert_eq!(offset_of!(CreateUpscale, flags), 0x10);
        assert_eq!(offset_of!(CreateUpscale, max_render), 0x14);
        assert_eq!(offset_of!(CreateUpscale, max_upscale), 0x1c);
        assert_eq!(offset_of!(Resource, description), 8);
        assert_eq!(offset_of!(Resource, state), 40);
    }

    /// The upscale description is found wherever it is in its chain, and
    /// not in another's.
    #[test]
    fn the_context_description_is_found_in_its_chain() {
        let upscale = CreateUpscale {
            header: Header { kind: CREATE_UPSCALE, next: std::ptr::null() },
            flags: 0x29,
            max_render: [1280, 720],
            max_upscale: [1920, 1080],
        };
        let backend = Header { kind: 0x0100_0002, next: (&raw const upscale).cast() };
        let version = Header { kind: 0x1, next: &raw const backend };
        // SAFETY: a chain of our own, alive for the calls.
        let found = unsafe { upscale_in(&raw const version) }.expect("the description");
        assert_eq!((found.flags, found.max_render, found.max_upscale), (0x29, [1280, 720], [1920, 1080]));
        let alone = Header { kind: 0x1, next: std::ptr::null() };
        // SAFETY: as above; null is an empty chain.
        unsafe {
            assert!(upscale_in(&raw const alone).is_none());
            assert!(upscale_in(std::ptr::null()).is_none());
        }
    }

    /// The title's `ffxCreateContext` is called with its own arguments and
    /// its answer given back, a failure too.
    #[test]
    fn creation_goes_on_to_the_title() {
        extern "system" fn made(context: *mut *mut c_void, _: *const Header, _: *const c_void) -> u32 {
            // SAFETY: the test's pointer.
            unsafe { context.write(0x40 as *mut c_void) };
            0
        }
        extern "system" fn not(_: *mut *mut c_void, _: *const Header, _: *const c_void) -> u32 {
            7
        }
        let upscale = CreateUpscale {
            header: Header { kind: CREATE_UPSCALE, next: std::ptr::null() },
            flags: 0,
            max_render: [1280, 720],
            max_upscale: [1920, 1080],
        };
        let before = REAL_CREATE.swap(0, Ordering::Relaxed);
        let mut context = std::ptr::null_mut();
        let mut call = || create(&mut context, (&raw const upscale).cast(), std::ptr::null());
        assert_eq!(call(), ERROR);
        set_real_create(not as *const () as usize);
        assert_eq!(call(), 7);
        set_real_create(made as *const () as usize);
        assert_eq!(call(), 0);
        assert_eq!(context as usize, 0x40);
        REAL_CREATE.store(before, Ordering::Relaxed);
    }

    /// What DLSS is asked for is the title's dispatch, unchanged.
    #[cfg(windows)]
    #[test]
    fn the_dispatch_is_passed_on_as_it_is() {
        // SAFETY: numbers and pointers only: all zero is a value.
        let mut d: Upscale = unsafe { std::mem::zeroed() };
        d.jitter = [0.25, -0.375];
        d.motion_scale = [1280.0, -720.0];
        (d.render, d.upscale) = ([1280, 720], [1920, 1080]);
        (d.pre_exposure, d.reset) = (2.0, 3);
        d.motion.resource = 0x50 as *const c_void;
        let (n, t) = for_dlss(9, &d);
        assert_eq!((n.context, n.jitter, n.motion_scale), (9, d.jitter, d.motion_scale));
        assert_eq!((n.render, n.upscale, n.pre_exposure, n.reset), (d.render, d.upscale, 2.0, true));
        assert_eq!(t.motion as usize, 0x50);
        assert!(t.exposure.is_null());
    }

    /// A dispatch that is not an upscale, with no DLL of the title's to
    /// pass it to, is an error and not a silent success.
    #[test]
    fn a_dispatch_nothing_answers_is_an_error() {
        let before = REAL.swap(0, Ordering::Relaxed);
        let other = Header {
            kind: 0x0002_0001,
            next: std::ptr::null(),
        };
        assert_eq!(dispatch(std::ptr::null_mut(), &other), ERROR);
        assert_eq!(dispatch(std::ptr::null_mut(), std::ptr::null()), ERROR);
        REAL.store(before, Ordering::Relaxed);
    }
}

/// What the title's import of `ffxDispatch` is bound to. The DLL is not an
/// HLE module: the title's own must load, for its other entry points and
/// for the dispatches passed on.
pub fn dispatch_address() -> usize {
    dispatch as *const () as usize
}

/// Whether a DLSS plugin was asked for and loaded.
pub fn dlss() -> bool {
    #[cfg(windows)]
    return victory_gpu::dlss::loaded();
    #[cfg(not(windows))]
    false
}

/// What the import of `ffxCreateContext` is bound to with a DLSS plugin.
pub fn create_address() -> usize {
    create as *const () as usize
}
