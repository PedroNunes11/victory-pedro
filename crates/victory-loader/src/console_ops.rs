//! Emulates the console CPU's instructions that the host CPU lacks.
//!
//! Titles are compiled for the console's Zen 2 CPU. On Intel CPUs (and
//! older AMD ones) a few of its instructions raise
//! `STATUS_ILLEGAL_INSTRUCTION`; the vectored handler in `crash` passes the
//! faulting thread's `CONTEXT` to [`emulate`], which does the instruction's
//! job, moves `Rip` past it and lets the thread go on. On a CPU that has
//! these instructions nothing faults, so none of this runs.
//!
//! | Instruction | Encoding | Emulated as |
//! |---|---|---|
//! | `clzero` | `0f 01 fc` | the 64-byte line holding rAX zeroed, if writable |
//! | `clwb` | `66 0f ae /6`, memory | nothing (a write-back hint) |
//! | `rdpru` | `0f 01 fd` | ECX 0 (MPERF) or 1 (APERF): the performance counter |
//! | `monitorx` | `0f 01 fa` | nothing |
//! | `mwaitx` | `0f 01 fb` | a yield of the thread's time slice |
//! | `rdpid` | `f3 0f c7 /7`, register | `GetCurrentProcessorNumber` |
//! | `extrq` | `66 0f 78 /0 ib ib`, `66 0f 79 /r` | the bit-field extract |
//! | `insertq` | `f2 0f 78 /r ib ib`, `f2 0f 79 /r` | the bit-field insert |
//!
//! Where the manual leaves `extrq`/`insertq` undefined (a field past bit
//! 63, the destination's upper quadword), the emulation does what a Zen 4
//! CPU was measured to do, by a test that compares the two for every length
//! and index on CPUs with SSE4a.
//!
//! Loops that `clzero` (or `clwb`) a whole buffer one line a pass, as
//! compilers emit them (`add ptr, 40h; cmp ptr, end; jb top`, see
//! [`decode::Loop`]), are finished in one exception: an exception costs
//! microseconds, a line a few nanoseconds.
//!
//! Written for Vertex from the manuals: AMD64
//! Architecture Programmer's Manual volume 3 (the instruction encoding
//! chapter; the reference pages of `clzero`, `clwb`, `rdpru`, `monitorx`,
//! `mwaitx`, `rdpid`), volume 4 (`extrq`, `insertq`), Intel's SDM volume 2
//! (instruction format, `clwb`, `rdpid`) and volume 3 (the priority among
//! exceptions: a code fetch fault comes before an invalid opcode, so the
//! bytes of an instruction that raised one are all readable), and the
//! windows-sys documentation of `CONTEXT`, `VirtualQuery`,
//! `QueryPerformanceCounter` and `GetCurrentProcessorNumber`.
//!
//! Cost: no allocation, no lock; the per-thread memory cache below means a
//! `clzero` usually needs no system call.

mod decode;

use decode::{Fields, Insn, Op};
use windows_sys::Win32::System::Diagnostics::Debug::{CONTEXT, M128A};

const EXCEPTION_CONTINUE_EXECUTION: i32 = -1;
const EXCEPTION_CONTINUE_SEARCH: i32 = 0;
/// `CONTEXT_FLOATING_POINT` without the architecture bit: the XMM
/// registers in the context are valid.
const CONTEXT_FLOATING_POINT_BIT: u32 = 0x8;
/// A cache line, as `clzero` zeroes it.
const LINE: u64 = 64;
const PAGE: usize = 0x1000;
/// Bytes read at `Rip`: enough for the longest instruction and for a body
/// plus the longest loop tail (6 + 7 + 3 + 6).
const WINDOW: usize = 32;
/// The status flags in `EFlags` that `cmp` sets: CF, PF, AF, ZF, SF, OF.
const STATUS_FLAGS: u32 = 0x8d5;

/// Emulates the instruction at `context.Rip` if it is one of ours.
///
/// Returns `EXCEPTION_CONTINUE_EXECUTION` (-1) with the registers and `Rip`
/// as the instruction would leave them, or `EXCEPTION_CONTINUE_SEARCH` (0)
/// with `context` untouched when the instruction is not one we emulate
/// (`ud2` among them) or must fault anyway (`clzero` on memory that is not
/// writable).
///
/// `context` is that of the current thread, which faulted at `Rip`: the
/// instruction's bytes are readable.
pub(crate) fn emulate(context: &mut CONTEXT) -> i32 {
    let mut window = [0u8; WINDOW];
    // SAFETY: `Rip` is where the current thread faulted on an invalid
    // opcode, so the instruction's bytes, and the rest of their page, are
    // readable; `fetch` reads past the page only when the next is readable.
    let n = unsafe { fetch(context.Rip as usize, &mut window) };
    let code = &window[..n];
    let Ok(insn) = decode::decode(code) else {
        return EXCEPTION_CONTINUE_SEARCH;
    };
    let done = match insn.op {
        Op::Clzero => clzero(context, &insn, code),
        Op::Clwb { base } => {
            match base.filter(|_| !insn.addr32) {
                Some(ptr) if finish_loop(context, code, insn.len, ptr, |_, _| true) => {}
                _ => context.Rip += insn.len as u64,
            }
            true
        }
        Op::Rdpru => {
            // ECX alone selects; only EDX:EAX change (writing a 32-bit
            // register clears the upper half of the 64-bit one).
            let value = match context.Rcx as u32 {
                0 | 1 => performance_counter(),
                _ => 0,
            };
            context.Rax = value & 0xffff_ffff;
            context.Rdx = value >> 32;
            context.Rip += insn.len as u64;
            true
        }
        Op::Monitorx => {
            context.Rip += insn.len as u64;
            true
        }
        Op::Mwaitx => {
            // A wait for a store to the monitored line or a timer: either
            // may end it early, so giving up the time slice is a valid
            // (and short) wait. The timer in EBX is not honoured.
            std::thread::yield_now();
            context.Rip += insn.len as u64;
            true
        }
        Op::Rdpid { dst } => {
            // SAFETY: a plain query.
            let id = unsafe { windows_sys::Win32::System::Threading::GetCurrentProcessorNumber() };
            *gpr(context, dst) = u64::from(id);
            context.Rip += insn.len as u64;
            true
        }
        Op::Extrq { .. } | Op::Insertq { .. } => {
            let done = sse4a(context, insn.op);
            if done {
                context.Rip += insn.len as u64;
            }
            done
        }
    };
    if done {
        EXCEPTION_CONTINUE_EXECUTION
    } else {
        EXCEPTION_CONTINUE_SEARCH
    }
}

/// Copies the bytes at `at` into `window`: all of it when that stays in
/// `at`'s page or the next page is readable, else up to the page's end.
/// Returns how many were copied.
///
/// # Safety
/// `at`'s page is readable from `at` to its end.
unsafe fn fetch(at: usize, window: &mut [u8; WINDOW]) -> usize {
    let room = PAGE - (at & (PAGE - 1));
    let n = if room >= WINDOW || at.checked_add(room).is_some_and(memory::readable) {
        WINDOW
    } else {
        room
    };
    // SAFETY: the caller vouches for `at`'s page, `readable` for the next.
    unsafe { std::ptr::copy_nonoverlapping(at as *const u8, window.as_mut_ptr(), n) };
    n
}

/// `clzero`: the whole loop around it when there is one and its range is
/// writable, else the one line at rAX when that is writable.
fn clzero(context: &mut CONTEXT, insn: &Insn, code: &[u8]) -> bool {
    // FS and GS have bases we cannot see here; the others are flat.
    if matches!(insn.seg, Some(0x64 | 0x65)) {
        return false;
    }
    let address = if insn.addr32 {
        context.Rax & 0xffff_ffff
    } else {
        context.Rax
    };
    let line = address & !(LINE - 1);
    if !insn.addr32 {
        // The line of each pass is `line` plus 64 a pass: the pointer's
        // offset in its line does not change.
        let zero_range = |start: u64, passes: u64| {
            let len = (passes * LINE) as usize;
            let first = (start & !(LINE - 1)) as usize;
            if memory::writable_now(first, len) {
                // SAFETY: just found committed and writable.
                unsafe { std::ptr::write_bytes(first as *mut u8, 0, len) };
                true
            } else {
                false
            }
        };
        if finish_loop(context, code, insn.len, 0, zero_range) {
            return true;
        }
    }
    if !memory::writable(line as usize, LINE as usize) {
        return false;
    }
    // SAFETY: the line is committed and writable (a stale cache entry for
    // memory freed or protected since would fault here, as the real
    // instruction would).
    unsafe { std::ptr::write_bytes(line as *mut u8, 0, LINE as usize) };
    context.Rip += insn.len as u64;
    true
}

/// When the body of `body_len` bytes at the start of `code` (whose pointer
/// is register `ptr`) is followed by a loop [`decode::find_loop`] knows,
/// runs `body(pointer, passes)` for all of its passes at once and, if that
/// succeeds, leaves the registers, flags and `Rip` as the loop's fall
/// through would.
fn finish_loop(
    context: &mut CONTEXT,
    code: &[u8],
    body_len: usize,
    ptr: u8,
    body: impl FnOnce(u64, u64) -> bool,
) -> bool {
    let Some(found) = decode::find_loop(code, body_len, ptr) else {
        return false;
    };
    let start = *gpr(context, ptr);
    let end = *gpr(context, found.end);
    let Some((n, after)) = decode::passes(start, end, found.again) else {
        return false;
    };
    if !body(start, n) {
        return false;
    }
    *gpr(context, ptr) = after;
    let (a, b) = if found.ptr_first {
        (after, end)
    } else {
        (end, after)
    };
    context.EFlags = context.EFlags & !STATUS_FLAGS | compare_flags(a, b);
    context.Rip += found.len as u64;
    true
}

/// The status flags `cmp a, b` leaves, as `EFlags` bits.
fn compare_flags(a: u64, b: u64) -> u32 {
    let r = a.wrapping_sub(b);
    let carry = a < b;
    let parity = (r as u8).count_ones().is_multiple_of(2);
    let adjust = (a ^ b ^ r) & 0x10 != 0;
    let zero = r == 0;
    let sign = r >> 63 != 0;
    let overflow = ((a ^ b) & (a ^ r)) >> 63 != 0;
    u32::from(carry)
        | u32::from(parity) << 2
        | u32::from(adjust) << 4
        | u32::from(zero) << 6
        | u32::from(sign) << 7
        | u32::from(overflow) << 11
}

/// The 64-bit general register numbered `n` in instruction encodings.
fn gpr(context: &mut CONTEXT, n: u8) -> &mut u64 {
    match n & 15 {
        0 => &mut context.Rax,
        1 => &mut context.Rcx,
        2 => &mut context.Rdx,
        3 => &mut context.Rbx,
        4 => &mut context.Rsp,
        5 => &mut context.Rbp,
        6 => &mut context.Rsi,
        7 => &mut context.Rdi,
        8 => &mut context.R8,
        9 => &mut context.R9,
        10 => &mut context.R10,
        11 => &mut context.R11,
        12 => &mut context.R12,
        13 => &mut context.R13,
        14 => &mut context.R14,
        _ => &mut context.R15,
    }
}

/// What `rdpru` reads for MPERF and APERF: both count up at a fixed rate,
/// so their ratio (what software uses them for) is 1.
fn performance_counter() -> u64 {
    let mut count = 0i64;
    // SAFETY: writes `count`; cannot fail on Windows XP and later.
    unsafe { windows_sys::Win32::System::Performance::QueryPerformanceCounter(&mut count) };
    count as u64
}

/// `extrq`/`insertq` on the context's XMM registers; `false` when the
/// context carries none.
fn sse4a(context: &mut CONTEXT, op: Op) -> bool {
    if context.ContextFlags & CONTEXT_FLOATING_POINT_BIT == 0 {
        return false;
    }
    // SAFETY: with CONTEXT_FLOATING_POINT set, the union holds the legacy
    // save area, XMM0 to XMM15 included.
    let xmm: &mut [M128A; 16] = unsafe { &mut context.Anonymous.FltSave.XmmRegisters };
    let fields = |f: u64| ((f & 63) as u8, ((f >> 8) & 63) as u8);
    match op {
        Op::Extrq {
            dst,
            src,
            fields: f,
        } => {
            let (len, idx) = match f {
                Fields::Imm { len, idx } => (len, idx),
                Fields::Source => fields(xmm[usize::from(src)].Low),
            };
            let d = usize::from(dst);
            xmm[d] = M128A {
                Low: extrq(xmm[d].Low, len, idx),
                High: 0,
            };
        }
        Op::Insertq {
            dst,
            src,
            fields: f,
        } => {
            let source = xmm[usize::from(src)];
            let (len, idx) = match f {
                Fields::Imm { len, idx } => (len, idx),
                Fields::Source => fields(source.High as u64),
            };
            let d = usize::from(dst);
            xmm[d] = M128A {
                Low: insertq(xmm[d].Low, source.Low, len, idx),
                High: 0,
            };
        }
        _ => return false,
    }
    true
}

/// The mask of a bit field of `len` bits (bits 5:0 of `len`; 0 means 64).
fn field_mask(len: u8) -> u64 {
    match len & 63 {
        0 => u64::MAX,
        n => (1u64 << n) - 1,
    }
}

/// `extrq`: the `len`-bit field at bit `idx` of `value`, moved to bit 0 and
/// zero-extended. Only bits 5:0 of `len` and `idx` count; a length of 0
/// means 64.
///
/// The manual leaves undefined a field that runs past bit 63 (index plus
/// length over 64, which includes length 0 with a nonzero index). Here the
/// field is cut at bit 63: the result holds the bits from `idx` to 63,
/// zero above. The manual also leaves bits 127:64 of the destination
/// undefined; [`sse4a`] zeroes them, as the Zen CPU we compared with does
/// (and the field cut at bit 63 is what it gives too).
fn extrq(value: u64, len: u8, idx: u8) -> u64 {
    (value >> (idx & 63)) & field_mask(len)
}

/// `insertq`: the low `len` bits of `source` put into `dest` at bit `idx`,
/// the other bits of `dest` kept. Field bits as in [`extrq`].
///
/// Undefined in the manual, here as on the Zen CPU we compared with: a
/// field running past bit 63 is cut there (the source bits that would land
/// above 63 are dropped); bits 127:64 of the destination are zeroed (in
/// [`sse4a`]).
fn insertq(dest: u64, source: u64, len: u8, idx: u8) -> u64 {
    let idx = idx & 63;
    let mask = field_mask(len) << idx;
    dest & !mask | (source << idx) & mask
}

/// What is known about memory: whether ranges are committed and writable.
///
/// `VirtualQuery` is a system call, and the title can `clzero` many times
/// a frame, so each thread remembers a few writable regions (as the query
/// reports them: base and size). An entry is used at most
/// [`RECHECK_AFTER`] times before it is queried again, and a query that
/// finds an address not writable forgets every entry holding it. A region
/// freed or protected by another thread between checks would make the
/// next `clzero` on it fault inside the handler: an access violation, as
/// the real instruction would raise, not a silent write.
mod memory {
    use std::cell::Cell;
    use windows_sys::Win32::System::Memory::{
        MEM_COMMIT, MEMORY_BASIC_INFORMATION, PAGE_EXECUTE_READ, PAGE_EXECUTE_READWRITE,
        PAGE_EXECUTE_WRITECOPY, PAGE_GUARD, PAGE_READONLY, PAGE_READWRITE, PAGE_WRITECOPY,
        VirtualQuery,
    };

    /// Uses of a remembered region before it is queried again.
    pub(super) const RECHECK_AFTER: u32 = 256;
    const SLOTS: usize = 8;

    #[derive(Clone, Copy)]
    struct Region {
        start: usize,
        end: usize,
        uses: u32,
    }

    const EMPTY: Region = Region {
        start: 0,
        end: 0,
        uses: 0,
    };

    #[derive(Clone, Copy)]
    struct Known {
        regions: [Region; SLOTS],
        next: usize,
    }

    thread_local! {
        static KNOWN: Cell<Known> = const { Cell::new(Known { regions: [EMPTY; SLOTS], next: 0 }) };
    }

    /// The region `at` lies in, from its page on, and its protection.
    fn query(at: usize) -> Option<(usize, usize, u32)> {
        let mut info = MEMORY_BASIC_INFORMATION::default();
        let size = size_of::<MEMORY_BASIC_INFORMATION>();
        // SAFETY: writes `info`, of the size given.
        if unsafe { VirtualQuery(at as *const _, &mut info, size) } == 0 {
            return None;
        }
        let start = info.BaseAddress as usize;
        let committed = info.State == MEM_COMMIT && info.Protect & PAGE_GUARD == 0;
        Some((
            start,
            start.saturating_add(info.RegionSize),
            if committed { info.Protect } else { 0 },
        ))
    }

    const WRITABLE: u32 =
        PAGE_READWRITE | PAGE_WRITECOPY | PAGE_EXECUTE_READWRITE | PAGE_EXECUTE_WRITECOPY;

    /// Whether the page holding `at` is committed and readable.
    pub(super) fn readable(at: usize) -> bool {
        query(at).is_some_and(|(_, _, p)| p & (WRITABLE | PAGE_READONLY | PAGE_EXECUTE_READ) != 0)
    }

    /// Whether `[at, at + len)` is committed and writable, from what this
    /// thread remembers when it can.
    pub(super) fn writable(at: usize, len: usize) -> bool {
        let Some(end) = at.checked_add(len) else {
            return false;
        };
        let remembered = KNOWN
            .try_with(|known| {
                let mut k = known.get();
                let Some(r) = k.regions.iter_mut().find(|r| r.start <= at && end <= r.end) else {
                    return false;
                };
                if r.uses >= RECHECK_AFTER {
                    return false;
                }
                r.uses += 1;
                known.set(k);
                true
            })
            .unwrap_or(false);
        remembered || writable_now(at, len)
    }

    /// Whether `[at, at + len)` is committed and writable, by asking the
    /// system (once per region it spans); remembers or forgets accordingly.
    pub(super) fn writable_now(at: usize, len: usize) -> bool {
        let Some(end) = at.checked_add(len) else {
            return false;
        };
        let mut p = at;
        while p < end {
            match query(p) {
                Some((start, region_end, protect)) if protect & WRITABLE != 0 && region_end > p => {
                    remember(start, region_end);
                    p = region_end;
                }
                _ => {
                    forget(p);
                    return false;
                }
            }
        }
        true
    }

    fn remember(start: usize, end: usize) {
        let _ = KNOWN.try_with(|known| {
            let mut k = known.get();
            let fresh = Region {
                start,
                end,
                uses: 0,
            };
            if let Some(r) = k.regions.iter_mut().find(|r| r.start == start) {
                *r = fresh;
            } else {
                let slot = k.next % SLOTS;
                k.regions[slot] = fresh;
                k.next = (slot + 1) % SLOTS;
            }
            known.set(k);
        });
    }

    fn forget(at: usize) {
        let _ = KNOWN.try_with(|known| {
            let mut k = known.get();
            for r in k.regions.iter_mut().filter(|r| r.start <= at && at < r.end) {
                *r = EMPTY;
            }
            known.set(k);
        });
    }

    /// Whether this thread remembers `at` as writable (for tests).
    #[cfg(test)]
    pub(super) fn remembered(at: usize) -> bool {
        KNOWN.with(|k| k.get().regions.iter().any(|r| r.start <= at && at < r.end))
    }
}

#[cfg(test)]
mod tests;
