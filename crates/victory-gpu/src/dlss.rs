//! DLSS Super Resolution for the title's FidelityFX upscale dispatch, by a
//! plugin its user builds against their own SDK (`plugins/dlss`).
//!
//! Off unless `VICTORY_DLSS_PLUGIN` names that DLL (a full path): nothing is
//! loaded then and the dispatch is stretched as before ([`crate::over`]).
//! Nothing of NVIDIA's is in this tree or linked here: the plugin has five C
//! functions of ours ([`Api`]), and it alone calls the SDK. Anything that
//! does not work out is told once in the log, and that dispatch is stretched.
//!
//! - `VICTORY_DLSS_SDK`: the SDK's folder, for the feature's DLL
//!   (`lib\Windows_x86_64\rel`). It can be beside the plugin instead.
//! - `VICTORY_DLSS_PRESET=J|K|L|M`: the model; the SDK's choice if not set.
//! - `VICTORY_DLSS_FLAGS=<hex>`: FidelityFX context flags ([`fidelity`]) to
//!   use in place of the ones the title made its context with.
//!
//! What differs between the two interfaces, and what [`plan`] makes of it
//! (the card tests in `dlss/card.rs` measure the first three):
//!
//! | | FidelityFX | DLSS | here |
//! |---|---|---|---|
//! | motion | `motion * scale` is in pixels of the motion texture's size, from where a texel is to where it was | the same | passed on, scale and sign as they are |
//! | jitter | pixels at the rendered size; the image is moved by it, y down | the same | passed on |
//! | depth | far 1, or 0 with `DEPTH_INVERTED` | the same, its own flag | flag |
//! | motion at the output's size | `DISPLAY_MOTION` | absence of "low resolution" | flag |
//! | jitter in the motion | `JITTERED_MOTION` | its own flag | flag |
//! | exposure | `AUTO_EXPOSURE`, or a 1x1 texture, or 1 if neither | a flag, or a 1x1 texture; must be one of them | texture if given and not automatic, else automatic |
//! | pre-exposure | what the colour was multiplied by; 0 is none | the same | passed on, 0 as 1 |
//! | reactive and composition masks | used | a mask its current models do not use | left out |
//! | sharpening | in the same dispatch | none | left out |

use std::ffi::{CStr, CString, c_char, c_void};
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

use windows::Win32::Graphics::Direct3D12::{
    D3D12_RESOURCE_FLAG_ALLOW_UNORDERED_ACCESS, ID3D12GraphicsCommandList, ID3D12Resource,
};
use windows::Win32::System::LibraryLoader::{
    GetProcAddress, LOAD_LIBRARY_SEARCH_DEFAULT_DIRS, LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR,
    LoadLibraryExW,
};
use windows::core::{HSTRING, Interface, PCSTR};

/// The plugin's interface this was written against.
const INTERFACE: u32 = 1;

/// FidelityFX upscale context flags (`ffx_upscale.h`, the public API).
pub mod fidelity {
    pub const HDR: u32 = 1 << 0;
    pub const DISPLAY_MOTION: u32 = 1 << 1;
    pub const JITTERED_MOTION: u32 = 1 << 2;
    pub const DEPTH_INVERTED: u32 = 1 << 3;
    pub const AUTO_EXPOSURE: u32 = 1 << 5;
    pub const DYNAMIC_RESOLUTION: u32 = 1 << 6;
}

/// The feature's flags, as its programming guide numbers them.
mod flag {
    pub const HDR: i32 = 1 << 0;
    pub const LOW_MOTION: i32 = 1 << 1;
    pub const JITTERED_MOTION: i32 = 1 << 2;
    pub const DEPTH_INVERTED: i32 = 1 << 3;
    pub const AUTO_EXPOSURE: i32 = 1 << 6;
}

/// What the title made an upscale context with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Context {
    pub flags: u32,
    pub max_render: [u32; 2],
    pub max_upscale: [u32; 2],
}

/// The numbers of one upscale dispatch, as the title gave them.
#[derive(Debug, Clone, Copy)]
pub struct Dispatch {
    /// Which of the title's contexts.
    pub context: usize,
    pub jitter: [f32; 2],
    pub motion_scale: [f32; 2],
    pub render: [u32; 2],
    pub upscale: [u32; 2],
    pub pre_exposure: f32,
    pub reset: bool,
}

/// The title textures of one dispatch; `exposure` may be null.
#[derive(Debug, Clone, Copy)]
pub struct Textures {
    pub colour: *const c_void,
    pub depth: *const c_void,
    pub motion: *const c_void,
    pub exposure: *const c_void,
    pub output: *const c_void,
}

/// Their sizes, and what else the plan needs to know of them.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Sizes {
    pub colour: [u32; 2],
    pub depth: [u32; 2],
    pub motion: [u32; 2],
    pub motion_format: i32,
    pub output: [u32; 2],
    /// The output can be written by shaders.
    pub writable: bool,
    pub exposure: bool,
}

/// The plugin's `Create`.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Create {
    width: u32,
    height: u32,
    out_width: u32,
    out_height: u32,
    quality: i32,
    flags: i32,
    preset: u32,
}

/// The plugin's `Evaluate`.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub(crate) struct Evaluate {
    colour: *mut c_void,
    depth: *mut c_void,
    motion: *mut c_void,
    exposure: *mut c_void,
    output: *mut c_void,
    width: u32,
    height: u32,
    jitter: [f32; 2],
    motion_scale: [f32; 2],
    pre_exposure: f32,
    reset: u32,
}

/// The plugin's `Init`.
#[repr(C)]
struct Init {
    device: *mut c_void,
    data_path: *const u16,
    folders: *const *const u16,
    folder_count: u32,
    log: Option<extern "C" fn(*const c_char)>,
}

/// The plugin's functions.
#[derive(Clone, Copy)]
struct Api {
    init: unsafe extern "C" fn(*const Init) -> u32,
    create: unsafe extern "C" fn(*mut c_void, *const Create, *mut *mut c_void) -> u32,
    evaluate: unsafe extern "C" fn(*mut c_void, *mut c_void, *const Evaluate) -> u32,
    release: unsafe extern "C" fn(*mut c_void) -> u32,
}

/// Whether `code` is a failure, as the SDK tells them.
fn failed(code: u32) -> bool {
    code & 0xFFF0_0000 == 0xBAD0_0000
}

/// The quality mode by how much the image grows, at the middles between the
/// modes' own ratios (1, 1.5, 1.72, 2 and 3).
fn quality(render: [u32; 2], upscale: [u32; 2]) -> i32 {
    let grows = (upscale[0] as f32 / render[0] as f32).max(upscale[1] as f32 / render[1] as f32);
    match grows {
        g if g < 1.2 => 5,
        g if g < 1.61 => 2,
        g if g < 1.86 => 1,
        g if g < 2.5 => 0,
        _ => 3,
    }
}

/// `VICTORY_DLSS_PRESET` as the SDK numbers its presets (A is 1). Only the
/// ones its guide says to use.
fn preset(asked: Option<&str>) -> u32 {
    match asked.map(str::trim) {
        Some(p) if p.len() == 1 && "JKLM".contains(&p.to_ascii_uppercase()) => {
            u32::from(p.to_ascii_uppercase().as_bytes()[0] - b'A') + 1
        }
        _ => 0,
    }
}

/// `VICTORY_DLSS_FLAGS`.
fn forced_flags(asked: Option<&str>) -> Option<u32> {
    let v = asked?.trim();
    u32::from_str_radix(v.strip_prefix("0x").unwrap_or(v), 16).ok()
}

/// The feature and the frame for one dispatch, or why there is none. The
/// frame's pointers are left null.
pub(crate) fn plan(
    context: Option<Context>,
    d: &Dispatch,
    s: &Sizes,
    preset: u32,
) -> Result<(Create, Evaluate), &'static str> {
    use fidelity as ffx;
    let context = context.ok_or("the title's context was not seen being made")?;
    let has = |f: u32| context.flags & f != 0;
    let inside = |a: [u32; 2], b: [u32; 2]| a[0] <= b[0] && a[1] <= b[1];
    let render = d.render;
    // Not given: the context's.
    let upscale = if d.upscale.contains(&0) { context.max_upscale } else { d.upscale };
    if render.contains(&0) || upscale.contains(&0) {
        return Err("a size is zero");
    }
    if upscale[0] < 64 || upscale[1] < 32 {
        return Err("the output is smaller than DLSS makes");
    }
    if !inside(render, upscale) {
        return Err("the output is smaller than what was rendered");
    }
    if !inside(render, s.colour) || !inside(render, s.depth) {
        return Err("more was rendered than the colour or depth texture holds");
    }
    if !inside(if has(ffx::DISPLAY_MOTION) { upscale } else { render }, s.motion) {
        return Err("the motion texture is too small");
    }
    // The output is written from its top left, over the upscaled size.
    if !inside(upscale, s.output) || !s.writable {
        return Err("the output is too small or cannot be written by shaders");
    }
    // R32G32, R16G16 and R16G16B16A16, float or typeless.
    if ![9, 10, 15, 16, 33, 34].contains(&s.motion_format) {
        return Err("the motion texture's format is not two or four floats");
    }
    if d.motion_scale.iter().any(|v| !v.is_finite() || *v == 0.0) {
        return Err("the motion scale is not a number or zero");
    }
    if d.jitter.iter().any(|v| !v.is_finite() || v.abs() > 1.0) {
        return Err("the jitter is not a part of a pixel");
    }
    let automatic = has(ffx::AUTO_EXPOSURE) || !s.exposure;
    let mut flags = 0;
    for (on, flag) in [
        (has(ffx::HDR), flag::HDR),
        (!has(ffx::DISPLAY_MOTION), flag::LOW_MOTION),
        (has(ffx::JITTERED_MOTION), flag::JITTERED_MOTION),
        (has(ffx::DEPTH_INVERTED), flag::DEPTH_INVERTED),
        (automatic, flag::AUTO_EXPOSURE),
    ] {
        if on {
            flags |= flag;
        }
    }
    // With a rendered size that changes, a feature for the largest one,
    // which holds the smaller ones; else for the size itself.
    let largest = if has(ffx::DYNAMIC_RESOLUTION) && inside(render, context.max_render) && inside(context.max_render, upscale) {
        context.max_render
    } else {
        render
    };
    let null = std::ptr::null_mut();
    Ok((
        Create {
            width: largest[0],
            height: largest[1],
            out_width: upscale[0],
            out_height: upscale[1],
            quality: quality(largest, upscale),
            flags,
            preset,
        },
        Evaluate {
            colour: null,
            depth: null,
            motion: null,
            exposure: null,
            output: null,
            width: render[0],
            height: render[1],
            jitter: d.jitter,
            motion_scale: d.motion_scale,
            pre_exposure: if d.pre_exposure.is_finite() && d.pre_exposure > 0.0 {
                d.pre_exposure
            } else {
                1.0
            },
            reset: u32::from(d.reset),
        },
    ))
}

/// Dispatches a feature is kept for after its last one, so that no list that
/// evaluates it can still run.
///
/// shortcut: a fixed count, as the stretch's views are kept; the title keeps
/// far fewer frames in flight.
const KEPT: u64 = 64;

struct Feature {
    context: usize,
    create: Create,
    handle: *mut c_void,
    used: u64,
    /// Its next frame starts anew: frames between were not its own.
    anew: bool,
}

/// The plugin, started, and its features.
struct Engine {
    api: Api,
    features: Vec<Feature>,
    /// Features that could not be made: not tried again every frame.
    refused: Vec<Create>,
    count: u64,
}

// SAFETY: the handles are the plugin's, used behind one lock.
unsafe impl Send for Engine {}

impl Engine {
    fn new(api: Api) -> Self {
        Engine { api, features: Vec::new(), refused: Vec::new(), count: 0 }
    }

    /// Records `frame` with the feature `create` of `context` (made into the
    /// same list if there is none) into `list`.
    ///
    /// # Safety
    /// An open list, and live resources in the states the plugin asks.
    unsafe fn record(
        &mut self,
        list: *mut c_void,
        context: usize,
        create: &Create,
        mut frame: Evaluate,
    ) -> Result<(), (&'static str, u32)> {
        if self.refused.contains(create) {
            return Err(("DLSS made no such feature", 0));
        }
        self.count += 1;
        let at = match self.features.iter().position(|f| f.context == context && f.create == *create) {
            Some(at) => at,
            None => {
                let mut handle = std::ptr::null_mut();
                let started = std::time::Instant::now();
                // SAFETY: the caller's list; structures of the plugin's layout.
                let code = unsafe { (self.api.create)(list, create, &mut handle) };
                let ms = started.elapsed().as_millis() as u64;
                if failed(code) || handle.is_null() {
                    self.refused.push(*create);
                    return Err(("DLSS made no such feature", code));
                }
                tracing::info!(?create, ms, "a DLSS feature was made");
                self.features.push(Feature {
                    context,
                    create: *create,
                    handle,
                    used: self.count,
                    anew: true,
                });
                self.features.len() - 1
            }
        };
        let f = &mut self.features[at];
        f.used = self.count;
        frame.reset |= u32::from(f.anew);
        // SAFETY: as above, with a feature of the plugin's.
        let code = unsafe { (self.api.evaluate)(list, f.handle, &frame) };
        f.anew = failed(code);
        if failed(code) {
            return Err(("DLSS did not upscale the frame", code));
        }
        let (api, count) = (self.api, self.count);
        self.features.retain(|f| {
            let keep = f.used + KEPT >= count;
            if !keep {
                // SAFETY: not used by any list that can still run ([`KEPT`]).
                unsafe { (api.release)(f.handle) };
            }
            keep
        });
        Ok(())
    }

    /// A dispatch went another way: what follows starts anew.
    fn interrupted(&mut self) {
        self.features.iter_mut().for_each(|f| f.anew = true);
    }
}

extern "C" fn log(line: *const c_char) {
    if line.is_null() {
        return;
    }
    // SAFETY: the plugin passes a string that lives for the call.
    let line = unsafe { CStr::from_ptr(line) }.to_string_lossy();
    tracing::debug!("{}", line.trim_end());
}

/// Tells `why` once for each reason.
fn told(why: &'static str, code: u32) {
    static TOLD: Mutex<Vec<(&'static str, u32)>> = Mutex::new(Vec::new());
    let mut told = TOLD.lock().unwrap_or_else(|e| e.into_inner());
    if !told.contains(&(why, code)) && told.len() < 32 {
        told.push((why, code));
        tracing::warn!(code = format_args!("{code:#x}"), "no DLSS for an upscale dispatch, which is stretched instead: {why}");
    }
}

/// The plugin, loaded once.
fn plugin() -> Option<&'static (Api, PathBuf)> {
    static PLUGIN: OnceLock<Option<(Api, PathBuf)>> = OnceLock::new();
    PLUGIN
        .get_or_init(|| {
            let file = PathBuf::from(std::env::var_os("VICTORY_DLSS_PLUGIN").filter(|v| !v.is_empty())?);
            match load(&file) {
                Ok(api) => {
                    tracing::info!(file = %file.display(), "the DLSS plugin is loaded");
                    Some((api, file))
                }
                Err(why) => {
                    tracing::warn!(file = %file.display(), "no DLSS plugin: {why}");
                    None
                }
            }
        })
        .as_ref()
}

/// Whether a DLSS plugin was asked for and loaded.
pub fn loaded() -> bool {
    plugin().is_some()
}

fn load(file: &std::path::Path) -> Result<Api, String> {
    if !file.is_absolute() {
        return Err("VICTORY_DLSS_PLUGIN is not a full path".into());
    }
    // SAFETY: loads a named library; its own folder and the system's are
    // searched for what it needs, never the current folder.
    let module = unsafe {
        LoadLibraryExW(
            &HSTRING::from(file.as_os_str()),
            None,
            LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR | LOAD_LIBRARY_SEARCH_DEFAULT_DIRS,
        )
    }
    .map_err(|e| e.to_string())?;
    let get = |name: &str| {
        let c = CString::new(name).map_err(|e| e.to_string())?;
        // SAFETY: a name with its end, and a module that stays loaded.
        unsafe { GetProcAddress(module, PCSTR(c.as_ptr().cast())) }
            .ok_or_else(|| format!("it lacks {name}"))
    };
    type Any = unsafe extern "system" fn() -> isize;
    // SAFETY: the functions have the signatures of the interface whose
    // version is asked first.
    unsafe {
        let version =
            std::mem::transmute::<Any, unsafe extern "C" fn() -> u32>(get("victory_dlss_version")?)();
        if version != INTERFACE {
            return Err(format!("its interface is version {version}, not {INTERFACE}"));
        }
        Ok(Api {
            init: std::mem::transmute::<Any, unsafe extern "C" fn(*const Init) -> u32>(get("victory_dlss_init")?),
            create: std::mem::transmute::<
                Any,
                unsafe extern "C" fn(*mut c_void, *const Create, *mut *mut c_void) -> u32,
            >(get("victory_dlss_create")?),
            evaluate: std::mem::transmute::<
                Any,
                unsafe extern "C" fn(*mut c_void, *mut c_void, *const Evaluate) -> u32,
            >(get("victory_dlss_evaluate")?),
            release: std::mem::transmute::<Any, unsafe extern "C" fn(*mut c_void) -> u32>(get("victory_dlss_release")?),
        })
    }
}

/// Starts the plugin on the host's device.
fn start() -> Option<Engine> {
    let (api, file) = plugin()?;
    let host = crate::host::host()?;
    let own = file.parent()?.to_owned();
    let mut folders = vec![own.clone()];
    if let Some(sdk) = std::env::var_os("VICTORY_DLSS_SDK") {
        folders.push(PathBuf::from(sdk).join(r"lib\Windows_x86_64\rel"));
    }
    let wide: Vec<HSTRING> = folders.iter().map(|f| HSTRING::from(f.as_os_str())).collect();
    let pointers: Vec<*const u16> = wide.iter().map(|w| w.as_ptr()).collect();
    let init = Init {
        device: host.device.as_raw(),
        data_path: pointers[0],
        folders: pointers.as_ptr(),
        folder_count: pointers.len() as u32,
        log: Some(log),
    };
    let started = std::time::Instant::now();
    // SAFETY: a structure of the plugin's layout; what it names lives
    // through the call, the device as long as the process.
    let code = unsafe { (api.init)(&init) };
    if failed(code) {
        told("the plugin did not start (no card of NVIDIA's that can, an old driver, or nvngx_dlss.dll not found)", code);
        return None;
    }
    let ms = started.elapsed().as_millis() as u64;
    tracing::info!(ms, "DLSS is up: the title's upscale dispatches go to it");
    Some(Engine::new(*api))
}

/// Not started, or still starting; started or not to be had.
static ENGINE: Mutex<Option<Option<Engine>>> = Mutex::new(None);

/// Starts the plugin on a thread of its own, once: that takes seconds (2 on
/// an RTX 5070, up to 27 the first time), which a thread of the title's must
/// not lose.
fn begin() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let run = || {
            let engine = start();
            *ENGINE.lock().unwrap_or_else(|e| e.into_inner()) = Some(engine);
        };
        if let Err(e) = std::thread::Builder::new().name("dlss-start".into()).spawn(run) {
            tracing::warn!(error = %e, "no thread to start DLSS on");
        }
    });
}

static CONTEXTS: Mutex<Vec<(usize, Context)>> = Mutex::new(Vec::new());

/// The title made an upscale context.
pub fn context_made(context: usize, made: Context) {
    tracing::info!(
        flags = format_args!("{:#x}", made.flags),
        max_render = ?made.max_render,
        max_upscale = ?made.max_upscale,
        "the title made an upscale context"
    );
    if loaded() {
        begin();
    }
    let mut contexts = CONTEXTS.lock().unwrap_or_else(|e| e.into_inner());
    contexts.retain(|(c, _)| *c != context);
    contexts.push((context, made));
}

fn context(id: usize, d: &Dispatch) -> Option<Context> {
    static FORCED: OnceLock<Option<u32>> = OnceLock::new();
    let forced = *FORCED.get_or_init(|| forced_flags(std::env::var("VICTORY_DLSS_FLAGS").ok().as_deref()));
    let contexts = CONTEXTS.lock().unwrap_or_else(|e| e.into_inner());
    let seen = contexts.iter().find(|(c, _)| *c == id).map(|&(_, c)| c);
    match forced {
        Some(flags) => Some(Context {
            flags,
            ..seen.unwrap_or(Context { flags, max_render: d.render, max_upscale: d.upscale })
        }),
        None => seen,
    }
}

/// The host textures of one dispatch.
pub(crate) struct Inputs<'a> {
    pub colour: &'a ID3D12Resource,
    pub depth: &'a ID3D12Resource,
    pub motion: &'a ID3D12Resource,
    pub exposure: Option<&'a ID3D12Resource>,
    pub output: &'a ID3D12Resource,
}

impl Inputs<'_> {
    fn sizes(&self) -> Sizes {
        // SAFETY: plain queries of live resources.
        let desc = |r: &ID3D12Resource| unsafe { r.GetDesc() };
        let size = |r: &ID3D12Resource| {
            let d = desc(r);
            [d.Width as u32, d.Height]
        };
        Sizes {
            colour: size(self.colour),
            depth: size(self.depth),
            motion: size(self.motion),
            motion_format: desc(self.motion).Format.0,
            output: size(self.output),
            writable: desc(self.output).Flags.0 & D3D12_RESOURCE_FLAG_ALLOW_UNORDERED_ACCESS.0 != 0,
            exposure: self.exposure.is_some(),
        }
    }
}

/// Records `d` by DLSS into `list`: the inputs in the non-pixel shader
/// resource state, the output in the unordered access state. False if it
/// was not (the reason is told once, but for a plugin still starting); the
/// list's state may be changed either way.
pub(crate) fn record(list: &ID3D12GraphicsCommandList, d: &Dispatch, inputs: &Inputs) -> bool {
    if !loaded() {
        return false;
    }
    static PRESET: OnceLock<u32> = OnceLock::new();
    let preset = *PRESET.get_or_init(|| preset(std::env::var("VICTORY_DLSS_PRESET").ok().as_deref()));
    let mut engine = ENGINE.lock().unwrap_or_else(|e| e.into_inner());
    let Some(started) = engine.as_mut() else {
        begin();
        return false;
    };
    let Some(engine) = started else { return false };
    let done = plan(context(d.context, d), d, &inputs.sizes(), preset)
        .map_err(|why| (why, 0))
        .and_then(|(create, frame)| {
            let automatic = create.flags & flag::AUTO_EXPOSURE != 0;
            let frame = Evaluate {
                colour: inputs.colour.as_raw(),
                depth: inputs.depth.as_raw(),
                motion: inputs.motion.as_raw(),
                exposure: match inputs.exposure {
                    Some(e) if !automatic => e.as_raw(),
                    _ => std::ptr::null_mut(),
                },
                output: inputs.output.as_raw(),
                ..frame
            };
            // SAFETY: the caller's open list and live resources.
            unsafe { engine.record(list.as_raw(), d.context, &create, frame) }
        });
    if let Err((why, code)) = done {
        engine.interrupted();
        told(why, code);
        return false;
    }
    true
}

/// A dispatch DLSS was not asked for (a texture without a host copy).
pub(crate) fn skipped(why: &'static str) {
    if let Some(Some(engine)) = ENGINE.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
        engine.interrupted();
    }
    told(why, 0);
}

mod check;
pub(crate) use check::{copy as check_copy, due as check_due};

#[cfg(test)]
mod card;
#[cfg(test)]
mod tests;
