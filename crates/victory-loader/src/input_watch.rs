//! Research (the in-game controls work): which title code reads the pad's
//! state, to find RAGE's action mapper.
//!
//! The title's pad device objects sit in a registry of 32 at title
//! +0x9ab14c8; slot 1 is the pad our keyboard layer drives. Its state, as
//! read live: buttons (likely) at +0x30, left stick X/Y +0x38/+0x3c, right
//! stick +0x40/+0x44, triggers (likely) +0x58/+0x60.
//! `VICTORY_INPUT_WATCH=1` puts hardware data breakpoints (debug registers,
//! read or write) on those fields in every thread of the process, keeps
//! adding them to new threads, and counts each instruction that touches
//! them in a vectored handler (no locks there). Every 10 s the instructions
//! seen most are logged as `module+rva` (`input_watch` lines); after
//! `VICTORY_INPUT_WATCH_SECS` (default 300) the breakpoints are cleared.
//! A data breakpoint traps after the access, so a logged address is the
//! instruction AFTER the one that read or wrote. Nothing runs without the
//! switch.

use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
use windows_sys::Win32::System::Diagnostics::Debug::{
    AddVectoredExceptionHandler, CONTEXT, CONTEXT_DEBUG_REGISTERS_AMD64, EXCEPTION_POINTERS,
    GetThreadContext, SetThreadContext,
};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, TH32CS_SNAPTHREAD, THREADENTRY32, Thread32First, Thread32Next,
};
use windows_sys::Win32::System::Threading::{
    GetCurrentProcessId, GetCurrentThreadId, OpenThread, ResumeThread, SuspendThread,
    THREAD_GET_CONTEXT, THREAD_SET_CONTEXT, THREAD_SUSPEND_RESUME,
};

/// Registry slot 1 (the first pad), as an offset in the image.
const PAD_SLOT: usize = 0x9ab_14d0;

/// The fields watched: offset in the pad object, length (1, 2, 4 or 8) and
/// what they are thought to hold.
const FIELDS: [(usize, u64, &str); 4] = [
    (0x30, 8, "buttons"),
    (0x38, 8, "left stick"),
    (0x40, 8, "right stick"),
    (0x58, 8, "triggers"),
];

const STATUS_SINGLE_STEP: i32 = 0x8000_0004_u32 as i32;
const CONTINUE_EXECUTION: i32 = -1;
const CONTINUE_SEARCH: i32 = 0;

/// Seen instructions: open addressing on the address; the low 4 bits of the
/// count word hold which breakpoints (Dr6 bits) it hit.
const SLOTS: usize = 4096;
static SEEN_AT: [AtomicUsize; SLOTS] = [const { AtomicUsize::new(0) }; SLOTS];
static SEEN_COUNT: [AtomicU64; SLOTS] = [const { AtomicU64::new(0) }; SLOTS];
static ARMED: AtomicBool = AtomicBool::new(false);

fn note(at: usize, which: u64) {
    let mut i = (at.wrapping_mul(0x9e37_79b9_7f4a_7c15) >> 52) % SLOTS;
    for _ in 0..32 {
        let here = SEEN_AT[i].load(Ordering::Relaxed);
        if here == at
            || (here == 0
                && SEEN_AT[i].compare_exchange(0, at, Ordering::Relaxed, Ordering::Relaxed).map_or_else(|v| v == at, |_| true))
        {
            SEEN_COUNT[i].fetch_add(16 | which, Ordering::Relaxed);
            return;
        }
        i = (i + 1) % SLOTS;
    }
}

unsafe extern "system" fn trap(info: *mut EXCEPTION_POINTERS) -> i32 {
    // SAFETY: the system passes the exception being dispatched.
    let (record, context) = unsafe { (&*(*info).ExceptionRecord, &mut *(*info).ContextRecord) };
    if record.ExceptionCode != STATUS_SINGLE_STEP || !ARMED.load(Ordering::Relaxed) {
        return CONTINUE_SEARCH;
    }
    let which = context.Dr6 & 0xf;
    if which == 0 {
        return CONTINUE_SEARCH;
    }
    note(context.Rip as usize, which);
    context.Dr6 = 0;
    CONTINUE_EXECUTION
}

#[repr(C, align(16))]
struct Aligned(CONTEXT);

/// Debug registers for `addresses` (each with its length) in every thread
/// of the process but this one that `done` does not hold yet; `clear` takes
/// them off every thread instead.
fn arm_threads(addresses: &[(usize, u64)], done: &mut Vec<u32>, clear: bool) -> usize {
    // SAFETY: plain queries and a snapshot of this process's threads.
    let (me, process, snapshot) =
        unsafe { (GetCurrentThreadId(), GetCurrentProcessId(), CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0)) };
    if snapshot == INVALID_HANDLE_VALUE {
        return 0;
    }
    let mut dr7 = 0u64;
    for (n, &(_, len)) in addresses.iter().enumerate() {
        let len_bits = match len {
            1 => 0b00,
            2 => 0b01,
            8 => 0b10,
            _ => 0b11,
        };
        // local enable, read or write (0b11), length
        dr7 |= 1 << (2 * n) | 0b11 << (16 + 4 * n) | len_bits << (18 + 4 * n);
    }
    let mut armed = 0;
    // SAFETY: zeroed is a valid THREADENTRY32; its size is set before use.
    let mut entry: THREADENTRY32 = unsafe { std::mem::zeroed() };
    entry.dwSize = size_of::<THREADENTRY32>() as u32;
    // SAFETY: `snapshot` is open; `entry` is sized.
    let mut more = unsafe { Thread32First(snapshot, &mut entry) } != 0;
    while more {
        let id = entry.th32ThreadID;
        if entry.th32OwnerProcessID == process && id != me && (clear || !done.contains(&id)) {
            // SAFETY: a thread of our own process; suspended only while its
            // debug registers are read and written.
            unsafe {
                let thread = OpenThread(THREAD_GET_CONTEXT | THREAD_SET_CONTEXT | THREAD_SUSPEND_RESUME, 0, id);
                if !thread.is_null() {
                    // Nothing allocates while the thread is suspended (it may
                    // hold the heap lock): `set` is noted and pushed after.
                    let mut set = false;
                    if SuspendThread(thread) != u32::MAX {
                        let mut c: Aligned = std::mem::zeroed();
                        c.0.ContextFlags = CONTEXT_DEBUG_REGISTERS_AMD64;
                        if GetThreadContext(thread, &mut c.0) != 0 {
                            let a = |n: usize| addresses.get(n).map_or(0, |&(at, _)| at as u64);
                            (c.0.Dr0, c.0.Dr1, c.0.Dr2, c.0.Dr3) =
                                if clear { (0, 0, 0, 0) } else { (a(0), a(1), a(2), a(3)) };
                            c.0.Dr6 = 0;
                            c.0.Dr7 = if clear { 0 } else { dr7 };
                            set = SetThreadContext(thread, &c.0) != 0;
                        }
                        ResumeThread(thread);
                    }
                    CloseHandle(thread);
                    if set {
                        armed += 1;
                        if !clear {
                            done.push(id);
                        }
                    }
                }
            }
        }
        // SAFETY: as above.
        more = unsafe { Thread32Next(snapshot, &mut entry) } != 0;
    }
    // SAFETY: we opened it.
    unsafe { CloseHandle(snapshot) };
    armed
}

fn report(base: usize, names: &[&str]) {
    let mut seen: Vec<(u64, usize)> = (0..SLOTS)
        .filter_map(|i| {
            let at = SEEN_AT[i].load(Ordering::Relaxed);
            (at != 0).then(|| (SEEN_COUNT[i].load(Ordering::Relaxed), at))
        })
        .collect();
    seen.sort_by(|a, b| (b.0 >> 4).cmp(&(a.0 >> 4)));
    let total: u64 = seen.iter().map(|s| s.0 >> 4).sum();
    tracing::info!(target: "input_watch", instructions = seen.len(), hits = total, "pad fields touched so far");
    for &(count, at) in seen.iter().take(40) {
        let fields: Vec<&str> = (0..4).filter(|n| count >> n & 1 != 0).map(|n| names[n]).collect();
        let rva = at.wrapping_sub(base);
        tracing::info!(
            target: "input_watch",
            hits = count >> 4,
            fields = fields.join("+"),
            title = format_args!("+{rva:#x}"),
            "after {}",
            crate::locate(at)
        );
    }
}

/// Starts the watcher thread when `VICTORY_INPUT_WATCH=1`.
pub fn start(base: usize) {
    if std::env::var_os("VICTORY_INPUT_WATCH").is_none_or(|v| v != "1") {
        return;
    }
    let secs = std::env::var("VICTORY_INPUT_WATCH_SECS").ok().and_then(|v| v.parse::<u64>().ok()).unwrap_or(300);
    // SAFETY: registers a handler that claims only the single-step traps of
    // our own breakpoints while they are armed.
    unsafe { AddVectoredExceptionHandler(1, Some(trap)) };
    let spawned = std::thread::Builder::new().name("input watch".into()).spawn(move || {
        // Wait for the title to make its pads.
        let slot = (base + PAD_SLOT) as *const usize;
        let pad = loop {
            // SAFETY: a pointer-sized global of the mapped image.
            let pad = unsafe { std::ptr::read_volatile(slot) };
            if pad != 0 {
                break pad;
            }
            std::thread::sleep(std::time::Duration::from_millis(500));
        };
        let addresses: Vec<(usize, u64)> = FIELDS.iter().map(|&(off, len, _)| (pad + off, len)).collect();
        let names: Vec<&str> = FIELDS.iter().map(|f| f.2).collect();
        tracing::info!(target: "input_watch", pad = format_args!("{pad:#x}"), secs, "watching the first pad's fields");
        ARMED.store(true, Ordering::Relaxed);
        let mut done = Vec::new();
        let started = std::time::Instant::now();
        let mut last_report = started;
        while started.elapsed().as_secs() < secs {
            let n = arm_threads(&addresses, &mut done, false);
            if n > 0 {
                tracing::info!(target: "input_watch", threads = n, all = done.len(), "breakpoints set");
            }
            if last_report.elapsed().as_secs() >= 10 {
                report(base, &names);
                last_report = std::time::Instant::now();
            }
            std::thread::sleep(std::time::Duration::from_secs(1));
        }
        let cleared = arm_threads(&addresses, &mut done, true);
        ARMED.store(false, Ordering::Relaxed);
        report(base, &names);
        tracing::info!(target: "input_watch", threads = cleared, "breakpoints cleared");
    });
    if let Err(e) = spawned {
        tracing::warn!(target: "input_watch", error = %e, "input watch did not start");
    }
}
