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
//! when it activates (title +0x1d67c0), and the title rebuilds these arrays
//! from its data and frees them (e.g. around the pause menu): the records go
//! in from a hook on that activation, every time, in blocks of the title's own
//! allocator (title +0x4d9c140), on the title's thread.

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
    // SAFETY: on the title's thread, with the title's handler and object.
    unsafe { translate(me, &e) };
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

/// The input system's actions (count, array), as offsets in the image.
const ACTION_COUNT: usize = 0xab7_7bd4;
const ACTION_ARRAY: usize = 0xab7_7bd8;
/// The first pad's index in the device registry: the low byte of its events' source.
const PAD_INDEX: u32 = 1;

/// `VICTORY_KEY_ACTIONS=<key>=<action>/<action>...,...` (hex: Windows virtual key, action ids =
/// joaat of their `INPUT_*` names, in order of preference).
static KEY_ACTIONS: std::sync::OnceLock<Vec<(u32, Vec<u32>)>> = std::sync::OnceLock::new();
/// Keys down and the pad source each was sent as (axis or button), to release the same one.
static HELD: std::sync::Mutex<Vec<(u32, u32, bool)>> = std::sync::Mutex::new(Vec::new());

fn parse_key_actions(spec: &str) -> Vec<(u32, Vec<u32>)> {
    let hex = |x: &str| u32::from_str_radix(x.trim().trim_start_matches("0x"), 16).ok();
    spec.split(',')
        .filter_map(|b| {
            let (key, actions) = b.split_once('=')?;
            Some((hex(key)?, actions.split('/').filter_map(hex).collect()))
        })
        .collect()
}

/// A pad source that is an axis (sticks and triggers) rather than a button.
fn is_axis(source: u32) -> bool {
    matches!(source, 0x3009 | 0x300c | 0x3010..=0x3013)
}

/// A key event, as the action it stands for: of the key's actions, the first the title has active
/// right now (+0x158 bit 2), sent as that action's own pad source (its first binding record) to
/// the title's buffer, as the pad would send it. The title's contexts decide what the key does
/// (on foot, in a car, in a menu); its action table is not changed.
///
/// # Safety
/// On the title's thread, inside the keyboard's event broadcast.
unsafe fn translate(me: &Relay, e: &Event) {
    let Some(map) = KEY_ACTIONS.get() else { return };
    let key = e.source >> 8;
    let Some((_, actions)) = map.iter().find(|(k, _)| *k == key) else { return };
    let base = IMAGE.load(Ordering::Relaxed);
    // SAFETY (both): the title's action array and its objects, read only.
    let read32 = |at: usize| unsafe { std::ptr::read_volatile(at as *const u32) };
    let read = |at: usize| unsafe { std::ptr::read_volatile(at as *const usize) };
    let send = |source: u32, kind: u32, value: f32| {
        let ev = Event { source: source << 8 | PAD_INDEX, kind, extra: 0, value };
        // SAFETY: the title's handler with its own object and an event of the pad's form.
        unsafe { (me.handler)(me.object, &ev) };
    };
    let mut held = HELD.lock().unwrap_or_else(|p| p.into_inner());
    match e.kind {
        7 => {
            if held.iter().any(|h| h.0 == key) {
                return;
            }
            let count = read32(base + ACTION_COUNT) as usize;
            let array = read(base + ACTION_ARRAY);
            for &id in actions {
                let Some(action) = (0..count).map(|i| array + i * 0x160).find(|&a| read32(a + 0x128) == id) else {
                    continue;
                };
                if read32(action + 0x158) & 2 == 0 || read32(action + 4) == 0 || read(action + 8) == 0 {
                    continue;
                }
                let entry = read(read(action + 8));
                let desc = if entry == 0 { 0 } else { read(entry + 0x10) };
                if desc == 0 || read32(desc + 4) == 0 {
                    continue;
                }
                let records = read(desc + 8);
                let (source, flags) = (read32(records), read32(records + 8) & 0xff);
                if !(0x3000..=0x3013).contains(&source) {
                    continue;
                }
                let axis = is_axis(source);
                let value = if axis && flags == 1 { -1.0 } else { 1.0 };
                send(source, if axis { 3 } else { 7 }, value);
                held.push((key, source, axis));
                static SAID: AtomicU64 = AtomicU64::new(0);
                if SAID.fetch_add(1, Ordering::Relaxed) < 200 {
                    tracing::info!(target: "keyboard_events", key = format_args!("{key:#x}"), action = format_args!("{id:#010x}"), source = format_args!("{source:#x}"), value, "key sent as its active action's pad source");
                }
                return;
            }
        }
        11 => {
            held.retain(|&(k, source, axis)| {
                if k == key {
                    if axis { send(source, 3, 0.0) } else { send(source, 11, 0.0) }
                }
                k != key
            });
        }
        _ => {}
    }
}

fn parse_binds(spec: &str) -> Vec<(u32, u32, u32)> {
    spec.split(',')
        .filter_map(|b| {
            let mut p = b.trim().split(':').map(|x| u32::from_str_radix(x.trim().trim_start_matches("0x"), 16).ok());
            Some((p.next()??, p.next()??, p.next().flatten().unwrap_or(0)))
        })
        .collect()
}

/// The title's action activation (it links an action's sources to it), its first 19 bytes (all
/// position independent), and its allocator `(align, size) -> block`.
const ACTIVATE: usize = 0x1d_67c0;
const ACTIVATE_PROLOGUE: [u8; 19] =
    [0x41, 0x57, 0x41, 0x56, 0x41, 0x55, 0x41, 0x54, 0x56, 0x57, 0x55, 0x53, 0x48, 0x81, 0xec, 0xc8, 0, 0, 0];
const ALLOCATE: usize = 0x4d9_c140;

static BINDS: std::sync::OnceLock<Vec<(u32, u32, u32)>> = std::sync::OnceLock::new();
static TRAMPOLINE: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
static IMAGE: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
static ADDED: AtomicU64 = AtomicU64::new(0);

/// Before the title links an action's sources: the asked keyboard records go into its bindings,
/// in a block of the title's own allocator (the title frees and rebuilds these arrays, e.g. around
/// the pause menu, then links again: this runs each time).
unsafe extern "system" fn activate(action: usize) {
    // SAFETY: the title's action object, on the title's thread, before its activation reads it.
    unsafe { add_to(action) };
    let original = TRAMPOLINE.load(Ordering::Relaxed);
    // SAFETY: the trampoline runs the displaced prologue and goes on in the title's function.
    unsafe { std::mem::transmute::<usize, unsafe extern "system" fn(usize)>(original)(action) }
}

/// # Safety
/// `action` is one of the title's input actions, being activated on this thread.
unsafe fn add_to(action: usize) {
    let Some(binds) = BINDS.get() else { return };
    // SAFETY (both): fields of the title's action, its entry and binding descriptor.
    let read32 = |at: usize| unsafe { std::ptr::read_volatile(at as *const u32) };
    let read = |at: usize| unsafe { std::ptr::read_volatile(at as *const usize) };
    let id = read32(action + 0x128);
    let wanted: Vec<(u32, u32)> = binds.iter().filter(|b| b.0 == id).map(|b| (b.1, b.2)).collect();
    if wanted.is_empty() || read32(action + 4) == 0 || read(action + 8) == 0 {
        return;
    }
    let entry = read(read(action + 8));
    if entry == 0 || read(entry + 0x10) == 0 {
        return;
    }
    let desc = read(entry + 0x10);
    let n = read32(desc + 4) as usize;
    let old = read(desc + 8);
    let mut records: Vec<u32> = (0..n * 3).map(|i| read32(old + 4 * i)).collect();
    let base = IMAGE.load(Ordering::Relaxed);
    let (absent, missing): (Vec<(u32, u32)>, Vec<(u32, u32)>) = wanted
        .into_iter()
        .filter(|&(key, _)| !records.chunks(3).any(|r| r[0] == key))
        .partition(|&(key, _)| !has_record(base, key));
    if !absent.is_empty() {
        static SAID: AtomicU64 = AtomicU64::new(0);
        if SAID.fetch_add(1, Ordering::Relaxed) < 16 {
            tracing::info!(target: "keyboard_events", action = format_args!("{id:#010x}"), keys = ?absent, "keys left out: no source record yet (the next activation adds them)");
        }
    }
    if missing.is_empty() {
        return;
    }
    for &(key, flags) in &missing {
        records.extend([key, 0xf000, flags]);
    }
    // SAFETY: the title's allocator, called on the title's thread as its own code calls it.
    let block = unsafe {
        std::mem::transmute::<usize, unsafe extern "system" fn(usize, usize) -> *mut u32>(base + ALLOCATE)(
            4,
            records.len() * 4,
        )
    };
    if block.is_null() {
        return;
    }
    // SAFETY: a block of `records.len()` u32 just allocated; then the descriptor: records first,
    // then count and capacity. The old block is left to the title.
    unsafe {
        std::ptr::copy_nonoverlapping(records.as_ptr(), block, records.len());
        std::ptr::write_volatile((desc + 8) as *mut usize, block as usize);
        std::ptr::write_volatile((desc + 4) as *mut u32, (records.len() / 3) as u32);
        std::ptr::write_volatile(desc as *mut u32, (records.len() / 3) as u32);
    }
    let times = ADDED.fetch_add(1, Ordering::Relaxed) + 1;
    if times <= 64 || times.is_power_of_two() {
        tracing::info!(target: "keyboard_events", action = format_args!("{id:#010x}"), keys = ?missing, records = records.len() / 3, times, "keyboard bindings added at activation");
    }
}

/// The title's map of source state records (title 0x14ab77e80: `{u32 groups, u32 size, groups}`;
/// a group is 16 control bytes and 16 records of 0x78 bytes, the code first), looked up the way
/// title +0x5091ff0 does, without inserting.
const SOURCE_MAP: usize = 0xab7_7e80;

fn has_record(base: usize, code: u32) -> bool {
    // SAFETY (both): the title's map and its groups, read only.
    let read32 = |at: usize| unsafe { std::ptr::read_volatile(at as *const u32) };
    let read8 = |at: usize| unsafe { std::ptr::read_volatile(at as *const u8) };
    let map = base + SOURCE_MAP;
    let groups = read32(map) as usize;
    // SAFETY: as above.
    let table = unsafe { std::ptr::read_volatile((map + 8) as *const usize) };
    if groups == 0 || table == 0 || !groups.is_power_of_two() {
        return false;
    }
    let mut h = code ^ (code >> 16);
    h = h.wrapping_mul(0x85eb_ca6b);
    h ^= h >> 13;
    h = h.wrapping_mul(0xc2b2_ae35);
    h ^= h >> 16;
    let tag = (h & 0x7f) as u8;
    let mut g = (h >> 7) as usize & (groups - 1);
    for _ in 0..groups {
        let group = table + g * 0x790;
        let mut empty = false;
        for i in 0..16 {
            let c = read8(group + i);
            if c == tag && read32(group + 0x10 + i * 0x78) == code {
                return true;
            }
            // vpsignb + vpmovmskb in the title: only 0x80 (empty) ends the search.
            empty |= c == 0x80;
        }
        if empty {
            return false;
        }
        g = (g + 1) & (groups - 1);
    }
    false
}

/// Puts [`activate`] in front of the title's activation.
///
/// # Safety
/// The image at `base` is mapped and none of its code has run yet.
unsafe fn hook_activation(base: usize) {
    use windows_sys::Win32::System::Diagnostics::Debug::FlushInstructionCache;
    use windows_sys::Win32::System::Memory::{
        MEM_COMMIT, MEM_RESERVE, PAGE_EXECUTE_READWRITE, VirtualAlloc, VirtualProtect,
    };
    use windows_sys::Win32::System::Threading::GetCurrentProcess;
    let at = base + ACTIVATE;
    // SAFETY: inside the mapped image; nothing runs yet.
    let found = unsafe { std::slice::from_raw_parts(at as *const u8, ACTIVATE_PROLOGUE.len()) };
    if found != ACTIVATE_PROLOGUE {
        tracing::warn!(target: "keyboard_events", "the action activation is not the expected code (another build?); no bindings");
        return;
    }
    let jump = |to: usize| -> [u8; 14] {
        let mut j = [0xff, 0x25, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        j[6..].copy_from_slice(&(to as u64).to_le_bytes());
        j
    };
    // SAFETY: a fresh page of our own for the trampoline.
    let trampoline =
        unsafe { VirtualAlloc(std::ptr::null(), 64, MEM_COMMIT | MEM_RESERVE, PAGE_EXECUTE_READWRITE) }.cast::<u8>();
    if trampoline.is_null() {
        return;
    }
    // SAFETY: our page: the displaced prologue, then a jump back after it.
    unsafe {
        std::ptr::copy_nonoverlapping(ACTIVATE_PROLOGUE.as_ptr(), trampoline, ACTIVATE_PROLOGUE.len());
        let back = jump(at + ACTIVATE_PROLOGUE.len());
        std::ptr::copy_nonoverlapping(back.as_ptr(), trampoline.add(ACTIVATE_PROLOGUE.len()), back.len());
    }
    TRAMPOLINE.store(trampoline as usize, Ordering::Relaxed);
    IMAGE.store(base, Ordering::Relaxed);
    let mut patch = [0x90u8; 19];
    patch[..14].copy_from_slice(&jump(activate as unsafe extern "system" fn(usize) as usize));
    let mut old = 0;
    // SAFETY: the title's function head (checked above), made writable for the write and put back.
    unsafe {
        if VirtualProtect(at as *const _, patch.len(), PAGE_EXECUTE_READWRITE, &mut old) == 0 {
            return;
        }
        std::ptr::copy_nonoverlapping(patch.as_ptr(), at as *mut u8, patch.len());
        VirtualProtect(at as *const _, patch.len(), old, &mut old);
        FlushInstructionCache(GetCurrentProcess(), at as *const _, patch.len());
    }
    tracing::info!(target: "keyboard_events", binds = BINDS.get().map_or(0, Vec::len), "keyboard bindings go in at each action activation");
}

/// Starts the subscriber thread when `VICTORY_KEYBOARD_EVENTS` is `1` or `log`.
pub fn start(base: usize) {
    IMAGE.store(base, Ordering::Relaxed);
    if let Ok(spec) = std::env::var("VICTORY_KEY_ACTIONS") {
        let map = parse_key_actions(&spec);
        tracing::info!(target: "keyboard_events", keys = map.len(), "keys stand for the title's actions (sent as each active action's pad source)");
        let _ = KEY_ACTIONS.set(map);
    }
    if let Ok(spec) = std::env::var("VICTORY_KEY_BINDS") {
        let binds = parse_binds(&spec);
        if !binds.is_empty() && BINDS.set(binds).is_ok() {
            // SAFETY: called before the title runs (run.rs, with the other image patches).
            unsafe { hook_activation(base) };
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
