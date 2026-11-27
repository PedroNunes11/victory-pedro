//! Research (the in-game controls work): who asks the title's input devices
//! for a source's value, and for which sources.
//!
//! RAGE's input devices (keyboard, pads; a registry of 32 at title
//! +0x9ab14c8) answer "what is source `code` worth" through vtable slot 9
//! (+0x48): codes 0..=0xff are keys, 0x1000/0x2000 mouse buttons/axes,
//! 0x3000.. pad buttons and axes. `VICTORY_INPUT_SPY=1` points that slot of
//! the keyboard's and the pads' vtables at [`spy_keyboard`] / [`spy_pad`],
//! which note the caller (the mapper) and the code, then call the title's
//! own function. Each distinct (device, code, caller) is logged once and
//! again at every power-of-two count (`input_spy` lines). Nothing changes
//! without the switch.

use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};

/// A vtable slot to replace: its offset in the image and the title function
/// expected there (as an offset too).
struct Slot {
    what: &'static str,
    at: usize,
    expected: usize,
}

/// The keyboard device's vtable is at +0x65f2100, the pad device's at
/// +0x65f1cc0 (master.exe of the Cyberleek release; checked before writing).
const KEYBOARD: Slot = Slot { what: "keyboard", at: 0x65f_2148, expected: 0x523_99b0 };
const PAD: Slot = Slot { what: "pad", at: 0x65f_1d08, expected: 0x51a_4580 };

type Value = unsafe extern "system" fn(*mut c_void, u32, *mut u32) -> f32;

static KEYBOARD_REAL: AtomicUsize = AtomicUsize::new(0);
static PAD_REAL: AtomicUsize = AtomicUsize::new(0);

fn on() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("VICTORY_INPUT_SPY").is_some_and(|v| v == "1"))
}

fn note(device: &'static str, code: u32) {
    let mut frames = [std::ptr::null_mut(); 4];
    // SAFETY: fills at most `frames.len()` return addresses; skips this
    // function, so the first is the spy's caller.
    let n = unsafe {
        windows_sys::Win32::System::Diagnostics::Debug::RtlCaptureStackBackTrace(
            2,
            frames.len() as u32,
            frames.as_mut_ptr(),
            std::ptr::null_mut(),
        )
    };
    let first = frames.first().map_or(0, |f| *f as usize);
    static SEEN: OnceLock<Mutex<HashMap<(&'static str, u32, usize), u64>>> = OnceLock::new();
    let count = {
        let mut seen = SEEN.get_or_init(Default::default).lock().unwrap_or_else(|e| e.into_inner());
        if seen.len() >= 4096 && !seen.contains_key(&(device, code, first)) {
            return;
        }
        let c = seen.entry((device, code, first)).or_insert(0);
        *c += 1;
        *c
    };
    if count.is_power_of_two() {
        let chain: Vec<_> = frames[..usize::from(n)].iter().map(|&f| crate::locate(f as usize)).collect();
        tracing::info!(target: "input_spy", device, code = format_args!("{code:#x}"), count, "source asked at {}", chain.join(" < "));
    }
}

unsafe extern "system" fn spy_keyboard(this: *mut c_void, code: u32, out: *mut u32) -> f32 {
    note("keyboard", code);
    // SAFETY: the title's function, saved before the slot was replaced.
    unsafe { std::mem::transmute::<usize, Value>(KEYBOARD_REAL.load(Ordering::Relaxed))(this, code, out) }
}

unsafe extern "system" fn spy_pad(this: *mut c_void, code: u32, out: *mut u32) -> f32 {
    note("pad", code);
    // SAFETY: as in `spy_keyboard`.
    unsafe { std::mem::transmute::<usize, Value>(PAD_REAL.load(Ordering::Relaxed))(this, code, out) }
}

/// Replaces the two slots when `VICTORY_INPUT_SPY=1`.
///
/// # Safety
/// The image of `size` bytes at `base` stays mapped and none of its code has
/// run yet.
pub unsafe fn apply(base: usize, size: usize) {
    use windows_sys::Win32::System::Memory::{PAGE_READWRITE, VirtualProtect};
    if !on() {
        return;
    }
    for (slot, real, spy) in [
        (&KEYBOARD, &KEYBOARD_REAL, spy_keyboard as Value),
        (&PAD, &PAD_REAL, spy_pad as Value),
    ] {
        if slot.at + 8 > size {
            tracing::warn!(device = slot.what, "input spy: slot outside the image");
            continue;
        }
        let at = (base + slot.at) as *mut usize;
        // SAFETY: inside the mapped image (checked); nothing runs yet.
        let found = unsafe { at.read_unaligned() };
        if found != base + slot.expected {
            tracing::warn!(device = slot.what, found = format_args!("{:#x}", found.wrapping_sub(base)), "input spy: not the expected function there (another build?)");
            continue;
        }
        real.store(found, Ordering::Relaxed);
        let mut old = 0;
        // SAFETY: as above; made writable for the store and put back.
        unsafe {
            if VirtualProtect(at.cast(), 8, PAGE_READWRITE, &mut old) == 0 {
                tracing::warn!(device = slot.what, "input spy: Windows would not make the slot writable");
                continue;
            }
            at.write_unaligned(spy as usize);
            VirtualProtect(at.cast(), 8, old, &mut old);
        }
        tracing::info!(device = slot.what, slot = format_args!("{:#x}", slot.at), "input spy: on");
    }
}
