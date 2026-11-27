//! RenderDoc, for a frame of the host's commands that its tools can open
//! (diagnostics). `VICTORY_RENDERDOC=<path of renderdoc.dll>` (or `1`, for
//! where its installer puts it) loads it before the host device exists, as
//! its in-application API asks. A capture is then taken of the next frame
//! when a file named `rdc` appears in `VICTORY_CAPTURE`'s folder, or when
//! its own capture key (F12) is pressed in the window. Captures are named by
//! `VICTORY_RENDERDOC_OUT` (a path and the start of a file name), by
//! default `renderdoc\victory` in the capture folder.

use std::ffi::{CString, c_char, c_void};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU32, Ordering};

/// RenderDoc's table of entry points, in its order (version 1.1.2 of its
/// API: every later one starts the same).
#[repr(C)]
struct Api {
    get_api_version: unsafe extern "C" fn(*mut i32, *mut i32, *mut i32),
    set_capture_option_u32: unsafe extern "C" fn(u32, u32) -> i32,
    set_capture_option_f32: unsafe extern "C" fn(u32, f32) -> i32,
    get_capture_option_u32: unsafe extern "C" fn(u32) -> u32,
    get_capture_option_f32: unsafe extern "C" fn(u32) -> f32,
    set_focus_toggle_keys: unsafe extern "C" fn(*const u32, i32),
    set_capture_keys: unsafe extern "C" fn(*const u32, i32),
    get_overlay_bits: unsafe extern "C" fn() -> u32,
    mask_overlay_bits: unsafe extern "C" fn(u32, u32),
    remove_hooks: unsafe extern "C" fn(),
    unload_crash_handler: unsafe extern "C" fn(),
    set_capture_file_path_template: unsafe extern "C" fn(*const c_char),
    get_capture_file_path_template: unsafe extern "C" fn() -> *const c_char,
    get_num_captures: unsafe extern "C" fn() -> u32,
    get_capture: unsafe extern "C" fn(u32, *mut c_char, *mut u32, *mut u64) -> u32,
    trigger_capture: unsafe extern "C" fn(),
    is_target_control_connected: unsafe extern "C" fn() -> u32,
    launch_replay_ui: unsafe extern "C" fn(u32, *const c_char) -> u32,
    set_active_window: unsafe extern "C" fn(*mut c_void, *mut c_void),
    start_frame_capture: unsafe extern "C" fn(*mut c_void, *mut c_void),
    is_frame_capturing: unsafe extern "C" fn() -> u32,
    end_frame_capture: unsafe extern "C" fn(*mut c_void, *mut c_void) -> u32,
}

/// The version asked of `RENDERDOC_GetAPI`: 1.1.2.
const VERSION: i32 = 1_01_02;

type GetApi = unsafe extern "C" fn(i32, *mut *const Api) -> i32;

static API: OnceLock<Option<usize>> = OnceLock::new();

fn api() -> Option<&'static Api> {
    // SAFETY: the table RenderDoc gave, which lives as long as its library
    // (never unloaded).
    (*API.get()?).map(|at| unsafe { &*(at as *const Api) })
}

/// Loads RenderDoc if asked for. Before the host device is made: it only
/// sees devices made after it is there.
pub(crate) fn load() {
    API.get_or_init(|| {
        let asked = std::env::var_os("VICTORY_RENDERDOC")?;
        let path = if asked == "1" {
            r"C:\Program Files\RenderDoc\renderdoc.dll".into()
        } else {
            asked
        };
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
        let wide: Vec<u16> = path.encode_wide().chain(Some(0)).collect();
        // SAFETY: a NUL-terminated path the user named.
        let module = unsafe { LoadLibraryW(wide.as_ptr()) };
        if module.is_null() {
            tracing::warn!(
                path = %path.to_string_lossy(),
                error = %std::io::Error::last_os_error(),
                "RenderDoc not loaded"
            );
            return None;
        }
        // SAFETY: a loaded module and a NUL-terminated name.
        let Some(get) = (unsafe { GetProcAddress(module, c"RENDERDOC_GetAPI".as_ptr().cast()) })
        else {
            tracing::warn!("the library named is not RenderDoc's: no RENDERDOC_GetAPI");
            return None;
        };
        // SAFETY: the export has this signature in every version.
        let get: GetApi = unsafe { std::mem::transmute(get) };
        let mut table: *const Api = std::ptr::null();
        // SAFETY: an out pointer for the table.
        if unsafe { get(VERSION, &mut table) } != 1 || table.is_null() {
            tracing::warn!("RenderDoc does not have version 1.1.2 of its API");
            return None;
        }
        // SAFETY: the table just given.
        let api = unsafe { &*table };
        let (mut major, mut minor, mut patch) = (0, 0, 0);
        // SAFETY: out pointers.
        unsafe { (api.get_api_version)(&mut major, &mut minor, &mut patch) };
        let out = std::env::var_os("VICTORY_RENDERDOC_OUT")
            .map(std::path::PathBuf::from)
            .or_else(|| Some(crate::capture::folder()?.join("renderdoc").join("victory")));
        if let Some(out) = &out {
            if let Some(parent) = out.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            if let Ok(name) = CString::new(out.to_string_lossy().as_bytes()) {
                // SAFETY: a NUL-terminated string, copied by the call.
                unsafe { (api.set_capture_file_path_template)(name.as_ptr()) };
            }
        }
        // The loader has its own handling of the title's faults, and the
        // window shows the title's picture alone.
        // SAFETY: calls that take no pointers.
        unsafe {
            (api.unload_crash_handler)();
            (api.mask_overlay_bits)(0, 0);
        }
        tracing::info!(
            api = format_args!("{major}.{minor}.{patch}"),
            captures = %out.map(|o| o.to_string_lossy().into_owned()).unwrap_or_default(),
            "RenderDoc loaded"
        );
        Some(table as usize)
    });
}

/// Asks for a capture of the frame that starts at the next present.
pub(crate) fn trigger() {
    if let Some(api) = api() {
        // SAFETY: takes nothing.
        unsafe { (api.trigger_capture)() };
        tracing::warn!("RenderDoc will capture the frame after this present");
    }
}

/// Says in the log where each capture made since the last call is. Called
/// at every present.
pub(crate) fn saved() {
    static SEEN: AtomicU32 = AtomicU32::new(0);
    let Some(api) = api() else { return };
    // SAFETY: takes nothing.
    let made = unsafe { (api.get_num_captures)() };
    for i in SEEN.swap(made, Ordering::Relaxed)..made {
        let mut name = [0u8; 1024];
        let mut len = name.len() as u32;
        // SAFETY: a buffer of the length said; no timestamp wanted.
        let ok = unsafe { (api.get_capture)(i, name.as_mut_ptr().cast(), &mut len, std::ptr::null_mut()) };
        let end = name.iter().position(|&b| b == 0).unwrap_or(0);
        tracing::warn!(
            valid = ok == 1,
            file = %String::from_utf8_lossy(&name[..end]),
            "RenderDoc saved a capture"
        );
    }
}
