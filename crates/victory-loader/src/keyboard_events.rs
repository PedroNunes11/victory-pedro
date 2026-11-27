//! Research (native keyboard): give the title's keyboard events a listener.
//!
//! Each RAGE input device broadcasts its events (`{source = code << 8 |
//! device, type 7 pressed / 11 released / 3 axis, 0, value}`) to the
//! subscribers on a list at device+8: nodes `{object, handler, next}`,
//! null-terminated. The title subscribes one handler (an event buffer at
//! title +0x41c94d0) to the first pad and none to the keyboard (registry
//! slot 0), so key events, made in full by the keyboard device, go nowhere.
//!
//! `VICTORY_KEYBOARD_EVENTS=1` subscribes that same buffer to the keyboard
//! through [`relay`], which logs the events and whether the buffer took them
//! (its count at +0x26b8 grew), then lets the title's handler have them;
//! `=log` only logs. Nothing changes without the switch.

use std::ffi::c_void;
use std::sync::atomic::{AtomicU64, Ordering};

/// Registry slots 0 (keyboard) and 1 (first pad), as offsets in the image.
const KEYBOARD_SLOT: usize = 0x9ab_14c8;
const PAD_SLOT: usize = 0x9ab_14d0;
/// The title's event buffer handler, expected on the pad's list.
const BUFFER_HANDLER: usize = 0x41c_94d0;
/// The buffer's event count (u16) in its object.
const BUFFER_COUNT: usize = 0x26b8;

type Handler = unsafe extern "system" fn(*mut c_void, *const Event);

#[repr(C)]
#[derive(Clone, Copy)]
pub struct Event {
    source: u32,
    kind: u32,
    extra: u32,
    value: f32,
}

/// What [`relay`] is given as its object.
#[repr(C)]
struct Relay {
    object: *mut c_void,
    handler: Handler,
    forward: bool,
}

static SEEN: AtomicU64 = AtomicU64::new(0);
static TAKEN: AtomicU64 = AtomicU64::new(0);

unsafe extern "system" fn relay(me: *mut c_void, event: *const Event) {
    // SAFETY: `me` is our leaked `Relay`; `event` the title's, for this call.
    let (me, e) = unsafe { (&*me.cast::<Relay>(), *event) };
    let n = SEEN.fetch_add(1, Ordering::Relaxed) + 1;
    if !me.forward {
        if n <= 64 || n.is_power_of_two() {
            tracing::info!(target: "keyboard_events", n, source = format_args!("{:#x}", e.source), key = format_args!("{:#x}", e.source >> 8), kind = e.kind, value = e.value, "keyboard event (not forwarded)");
        }
        return;
    }
    let count = (me.object as usize + BUFFER_COUNT) as *const u16;
    // SAFETY: the buffer object's count; read without its lock, for the log only.
    let before = unsafe { count.read_volatile() };
    // SAFETY: the title's handler with its own object and the event it was given.
    unsafe { (me.handler)(me.object, event) };
    // SAFETY: as above.
    let after = unsafe { count.read_volatile() };
    let took = after != before;
    let taken = if took { TAKEN.fetch_add(1, Ordering::Relaxed) + 1 } else { TAKEN.load(Ordering::Relaxed) };
    if n <= 64 || n.is_power_of_two() {
        tracing::info!(target: "keyboard_events", n, taken, took, source = format_args!("{:#x}", e.source), key = format_args!("{:#x}", e.source >> 8), kind = e.kind, value = e.value, "keyboard event to the title's buffer");
    }
}

/// Starts the subscriber thread when `VICTORY_KEYBOARD_EVENTS` is `1` or `log`.
pub fn start(base: usize) {
    let forward = match std::env::var("VICTORY_KEYBOARD_EVENTS").as_deref() {
        Ok("1") => true,
        Ok("log") => false,
        _ => return,
    };
    let spawned = std::thread::Builder::new().name("keyboard events".into()).spawn(move || {
        let read = |at: usize| -> usize {
            // SAFETY: pointer-sized words of the mapped image or of title objects it points at.
            unsafe { std::ptr::read_volatile(at as *const usize) }
        };
        // Wait for the title's subscriber on the first pad.
        let (keyboard, object, handler) = loop {
            let keyboard = read(base + KEYBOARD_SLOT);
            let pad = read(base + PAD_SLOT);
            if keyboard != 0 && pad != 0 {
                let node = read(pad + 8);
                if node != 0 {
                    break (keyboard, read(node), read(node + 8));
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(500));
        };
        if handler != base + BUFFER_HANDLER {
            tracing::warn!(target: "keyboard_events", handler = format_args!("+{:#x}", handler.wrapping_sub(base)), "the pad's subscriber is not the expected buffer; left alone");
            return;
        }
        if read(keyboard + 8) != 0 {
            tracing::warn!(target: "keyboard_events", "the keyboard already has a subscriber; left alone");
            return;
        }
        let me = Box::leak(Box::new(Relay {
            object: object as *mut c_void,
            // SAFETY: the title's handler, checked above.
            handler: unsafe { std::mem::transmute::<usize, Handler>(handler) },
            forward,
        }));
        // A node as the title makes them: object, handler, next (null ends the list), and room.
        let node: &'static mut [usize; 8] = Box::leak(Box::new([0usize; 8]));
        node[0] = std::ptr::from_mut(me) as usize;
        node[1] = relay as Handler as usize;
        // SAFETY: the keyboard device's list head, one pointer-sized store.
        unsafe { std::ptr::write_volatile((keyboard + 8) as *mut usize, node.as_ptr() as usize) };
        tracing::info!(
            target: "keyboard_events",
            keyboard = format_args!("{keyboard:#x}"),
            buffer = format_args!("{object:#x}"),
            forward,
            "the keyboard's events now reach the title's input buffer"
        );
    });
    if let Err(e) = spawned {
        tracing::warn!(target: "keyboard_events", error = %e, "keyboard events did not start");
    }
}
