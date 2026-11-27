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
//!
//! `VICTORY_KEY_BINDS=<action>:<key>:<flags>,...` (hex: action id = joaat of its
//! `INPUT_*` name, Windows virtual key, record flags) adds a keyboard record to
//! each action's bindings as soon as the actions exist (input system
//! 0x14ab77b90: actions of 0x160 bytes, count at +0x44, array at +0x48; id at
//! +0x128; entry +0x10 -> `{capacity, count, records}`, records of 12 bytes
//! `{source, second source or 0xf000, flags}`). An action links its sources
//! when its context activates, so this runs early (start screen).

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

/// The input system's action array (count, first), as offsets in the image.
const ACTION_COUNT: usize = 0xab7_7bd4;
const ACTION_ARRAY: usize = 0xab7_7bd8;

fn parse_binds(spec: &str) -> Vec<(u32, u32, u32)> {
    spec.split(',')
        .filter_map(|b| {
            let mut p = b.trim().split(':').map(|x| u32::from_str_radix(x.trim().trim_start_matches("0x"), 16).ok());
            Some((p.next()??, p.next()??, p.next().flatten().unwrap_or(0)))
        })
        .collect()
}

/// Adds the asked keyboard records once the title has its actions.
fn add_binds(base: usize, binds: Vec<(u32, u32, u32)>) {
    let read32 = |at: usize| -> u32 {
        // SAFETY: words of the mapped image or of title objects it points at.
        unsafe { std::ptr::read_volatile(at as *const u32) }
    };
    let read = |at: usize| -> usize {
        // SAFETY: as above.
        unsafe { std::ptr::read_volatile(at as *const usize) }
    };
    let (count, array) = loop {
        let count = read32(base + ACTION_COUNT) as usize;
        let array = read(base + ACTION_ARRAY);
        if count > 0 && array != 0 {
            break (count, array);
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
    };
    let _ = (count, array);
    for (id, key, flags) in binds {
        // The actions are filled in after the array appears: look again for a minute.
        let mut found = None;
        for _ in 0..300 {
            let count = read32(base + ACTION_COUNT) as usize;
            let array = read(base + ACTION_ARRAY);
            if let Some(action) = (0..count).map(|i| array + i * 0x160).find(|&a| read32(a + 0x128) == id) {
                let first = read(action + 8);
                if read32(action + 4) != 0 && first != 0 && read(first) != 0 && read(read(first) + 0x10) != 0 {
                    found = Some(read(read(first) + 0x10));
                    break;
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(200));
        }
        let Some(desc) = found else {
            tracing::warn!(target: "keyboard_events", action = format_args!("{id:#010x}"), "no such action with bindings");
            continue;
        };
        let n = read32(desc + 4) as usize;
        let old = read(desc + 8);
        let mut records: Vec<u32> = (0..n * 3).map(|i| read32(old + 4 * i)).collect();
        records.extend([key, 0xf000, flags]);
        let new = Box::leak(records.into_boxed_slice());
        // SAFETY: the action's binding descriptor: records first, then count and capacity, so a reader
        // never sees a count past its records.
        unsafe {
            std::ptr::write_volatile((desc + 8) as *mut usize, new.as_ptr() as usize);
            std::ptr::write_volatile((desc + 4) as *mut u32, (n + 1) as u32);
            std::ptr::write_volatile(desc as *mut u32, (n + 1) as u32);
        }
        tracing::info!(target: "keyboard_events", action = format_args!("{id:#010x}"), key = format_args!("{key:#x}"), flags, records = n + 1, "keyboard binding added");
    }
}

/// Starts the subscriber thread when `VICTORY_KEYBOARD_EVENTS` is `1` or `log`.
pub fn start(base: usize) {
    if let Ok(spec) = std::env::var("VICTORY_KEY_BINDS") {
        let binds = parse_binds(&spec);
        if !binds.is_empty() {
            let _ = std::thread::Builder::new().name("key binds".into()).spawn(move || add_binds(base, binds));
        }
    }
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
