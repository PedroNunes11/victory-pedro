//! The emulation on synthetic contexts: nothing here executes the
//! instructions (except the SSE4a comparison, on CPUs that have it), so it
//! runs on any CPU.

use super::*;
use windows_sys::Win32::System::Diagnostics::Debug::{CONTEXT_AMD64, CONTEXT_FULL_AMD64};
use windows_sys::Win32::System::Memory::{
    MEM_COMMIT, MEM_RELEASE, MEM_RESERVE, PAGE_NOACCESS, PAGE_PROTECTION_FLAGS, PAGE_READONLY,
    PAGE_READWRITE, VirtualAlloc, VirtualFree, VirtualProtect,
};

/// Pages of ours, released on drop.
struct Pages {
    base: *mut u8,
}

impl Pages {
    fn new(count: usize, protect: PAGE_PROTECTION_FLAGS) -> Self {
        // SAFETY: a fresh allocation of ours.
        let base = unsafe {
            VirtualAlloc(
                std::ptr::null(),
                count * PAGE,
                MEM_RESERVE | MEM_COMMIT,
                protect,
            )
        };
        assert!(!base.is_null());
        Pages { base: base.cast() }
    }

    fn reserved(count: usize) -> Self {
        // SAFETY: as above, nothing committed.
        let base =
            unsafe { VirtualAlloc(std::ptr::null(), count * PAGE, MEM_RESERVE, PAGE_NOACCESS) };
        assert!(!base.is_null());
        Pages { base: base.cast() }
    }

    fn at(&self, offset: usize) -> u64 {
        self.base as u64 + offset as u64
    }

    fn protect(&self, page: usize, protect: PAGE_PROTECTION_FLAGS) {
        let mut old = 0;
        // SAFETY: a page of ours.
        let ok =
            unsafe { VirtualProtect(self.base.add(page * PAGE).cast(), PAGE, protect, &mut old) };
        assert_ne!(ok, 0);
    }

    /// Bytes `range` of the first page(s); they must be readable.
    fn bytes(&self, range: std::ops::Range<usize>) -> &[u8] {
        // SAFETY: inside our allocation, which the caller keeps readable.
        unsafe { std::slice::from_raw_parts(self.base.add(range.start), range.len()) }
    }

    fn fill(&self, range: std::ops::Range<usize>, value: u8) {
        // SAFETY: inside our allocation, writable.
        unsafe { self.base.add(range.start).write_bytes(value, range.len()) };
    }

    fn put(&self, offset: usize, code: &[u8]) {
        // SAFETY: inside our allocation, writable.
        unsafe {
            self.base
                .add(offset)
                .copy_from_nonoverlapping(code.as_ptr(), code.len())
        };
    }
}

impl Drop for Pages {
    fn drop(&mut self) {
        // SAFETY: releases our allocation.
        unsafe { VirtualFree(self.base.cast(), 0, MEM_RELEASE) };
    }
}

fn blank_context() -> Box<CONTEXT> {
    // SAFETY: CONTEXT is plain data; all zeroes is a valid value.
    let mut context: Box<CONTEXT> = Box::new(unsafe { std::mem::zeroed() });
    context.ContextFlags = CONTEXT_FULL_AMD64;
    context.EFlags = 0x202;
    context
}

/// Runs the emulation on `code` (followed by `int3` padding, so a decoder
/// that reads too far sees nothing valid) with the registers `setup` sets.
/// Returns the result, the context after, and how far `Rip` moved.
fn run(code: &[u8], setup: impl FnOnce(&mut CONTEXT)) -> (i32, Box<CONTEXT>, u64) {
    let mut bytes = code.to_vec();
    bytes.extend_from_slice(&[0xcc; 32]);
    let mut context = blank_context();
    context.Rip = bytes.as_ptr() as u64;
    setup(&mut context);
    let r = emulate(&mut context);
    let moved = context.Rip.wrapping_sub(bytes.as_ptr() as u64);
    (r, context, moved)
}

/// Runs the emulation on code already placed at `rip`.
fn run_at(rip: u64, setup: impl FnOnce(&mut CONTEXT)) -> (i32, Box<CONTEXT>, u64) {
    let mut context = blank_context();
    context.Rip = rip;
    setup(&mut context);
    let r = emulate(&mut context);
    let moved = context.Rip.wrapping_sub(rip);
    (r, context, moved)
}

const CONTINUE: i32 = EXCEPTION_CONTINUE_EXECUTION;
const SEARCH: i32 = EXCEPTION_CONTINUE_SEARCH;
const CLZERO: [u8; 3] = [0x0f, 0x01, 0xfc];

/// `clzero; add rax, 40h; cmp rax, rcx; <jcc> top` with the jump's opcode.
fn clzero_loop(jcc: u8) -> [u8; 12] {
    [
        0x0f, 0x01, 0xfc, 0x48, 0x83, 0xc0, 0x40, 0x48, 0x39, 0xc8, jcc, 0xf4,
    ]
}
const JB: u8 = 0x72;
const JBE: u8 = 0x76;
const JNE: u8 = 0x75;

#[test]
fn clzero_zeroes_the_line_holding_rax() {
    let pages = Pages::new(1, PAGE_READWRITE);
    pages.fill(0..256, 0xff);
    let rax = pages.at(64 + 5);
    let (r, after, moved) = run(&CLZERO, |c| c.Rax = rax);
    assert_eq!((r, moved), (CONTINUE, 3));
    assert_eq!((after.Rax, after.EFlags), (rax, 0x202));
    assert!(pages.bytes(0..64).iter().all(|&b| b == 0xff));
    assert!(pages.bytes(64..128).iter().all(|&b| b == 0));
    assert!(pages.bytes(128..256).iter().all(|&b| b == 0xff));
    // The last byte of a line names that line; REX and DS are allowed.
    pages.fill(0..256, 0xff);
    let (r, _, moved) = run(&[0x3e, 0x48, 0x0f, 0x01, 0xfc], |c| c.Rax = pages.at(191));
    assert_eq!((r, moved), (CONTINUE, 5));
    assert!(pages.bytes(128..192).iter().all(|&b| b == 0));
    assert!(
        pages
            .bytes(64..128)
            .iter()
            .chain(pages.bytes(192..256))
            .all(|&b| b == 0xff)
    );
    // Many times on the same memory (the remembered region).
    for i in 0..1000 {
        pages.fill(0..64, 0xff);
        assert_eq!(run(&CLZERO, |c| c.Rax = pages.at(i % 64)).0, CONTINUE);
        assert!(pages.bytes(0..64).iter().all(|&b| b == 0));
    }
}

#[test]
fn clzero_on_memory_that_is_not_writable_is_not_hidden() {
    let read_only = Pages::new(1, PAGE_READONLY);
    let reserved = Pages::reserved(1);
    let no_access = Pages::new(1, PAGE_NOACCESS);
    for rax in [
        read_only.at(64),
        reserved.at(0),
        no_access.at(128),
        0,
        0xffff_8000_0000_0000,
    ] {
        let (r, after, moved) = run(&CLZERO, |c| c.Rax = rax);
        assert_eq!((r, moved, after.Rax), (SEARCH, 0, rax), "{rax:#x}");
    }
    // FS and GS overrides: their bases are not in the context.
    let pages = Pages::new(1, PAGE_READWRITE);
    pages.fill(0..64, 0xff);
    for seg in [0x64, 0x65] {
        assert_eq!(
            run(&[seg, 0x0f, 0x01, 0xfc], |c| c.Rax = pages.at(0)).0,
            SEARCH
        );
    }
    assert!(pages.bytes(0..64).iter().all(|&b| b == 0xff));
}

#[test]
fn clzero_with_an_address_size_prefix_uses_eax() {
    // A page below 4 GiB, if one of these is free.
    let low = (1..64u64).find_map(|i| {
        // SAFETY: asks for a fresh page at a fixed address.
        let p = unsafe {
            VirtualAlloc(
                (i << 24) as *const _,
                PAGE,
                MEM_RESERVE | MEM_COMMIT,
                PAGE_READWRITE,
            )
        };
        (!p.is_null()).then_some(p.cast::<u8>())
    });
    let Some(low) = low else { return };
    let pages = Pages { base: low };
    pages.fill(0..256, 0xff);
    let rax = 0xdead_beef_0000_0000 | pages.at(128 + 9);
    let (r, after, moved) = run(&[0x67, 0x0f, 0x01, 0xfc], |c| c.Rax = rax);
    assert_eq!((r, moved, after.Rax), (CONTINUE, 4, rax));
    assert!(pages.bytes(128..192).iter().all(|&b| b == 0));
    assert!(
        pages
            .bytes(0..128)
            .iter()
            .chain(pages.bytes(192..256))
            .all(|&b| b == 0xff)
    );
}

#[test]
fn clzero_loops_are_finished_in_one_go() {
    let pages = Pages::new(2, PAGE_READWRITE);
    let check = |zeroed: std::ops::Range<usize>| {
        let all = pages.bytes(0..2 * PAGE);
        for (i, &b) in all.iter().enumerate() {
            assert_eq!(b, if zeroed.contains(&i) { 0 } else { 0xff }, "byte {i}");
        }
    };
    // jb: stops when rax reaches rcx.
    pages.fill(0..2 * PAGE, 0xff);
    let (r, after, moved) = run(&clzero_loop(JB), |c| {
        c.Rax = pages.at(64);
        c.Rcx = pages.at(64 + 1024);
        c.EFlags = 0x202 | 0x1 | 0x80; // CF and SF set before
    });
    assert_eq!((r, moved), (CONTINUE, 12));
    assert_eq!((after.Rax, after.Rcx), (pages.at(1088), pages.at(1088)));
    assert_eq!(after.EFlags, 0x202 | 0x44); // cmp of equal values: ZF, PF
    check(64..1088);
    // jbe: one more line when the end is reached exactly.
    pages.fill(0..2 * PAGE, 0xff);
    let (r, after, moved) = run(&clzero_loop(JBE), |c| {
        c.Rax = pages.at(64);
        c.Rcx = pages.at(64 + 1024);
    });
    assert_eq!((r, moved, after.Rax), (CONTINUE, 12, pages.at(1152)));
    assert_eq!(
        after.EFlags & STATUS_FLAGS,
        compare_flags(pages.at(1152), pages.at(1088))
    );
    check(64..1152);
    // jne, and a pointer that is not line-aligned: whole lines still.
    pages.fill(0..2 * PAGE, 0xff);
    let (r, after, moved) = run(&clzero_loop(JNE), |c| {
        c.Rax = pages.at(8);
        c.Rcx = pages.at(8 + 256);
    });
    assert_eq!((r, moved, after.Rax), (CONTINUE, 12, pages.at(264)));
    check(0..256);
    // The whole allocation, to its last byte.
    pages.fill(0..2 * PAGE, 0xff);
    let (r, after, _) = run(&clzero_loop(JB), |c| {
        c.Rax = pages.at(0);
        c.Rcx = pages.at(2 * PAGE);
    });
    assert_eq!((r, after.Rax), (CONTINUE, pages.at(2 * PAGE)));
    check(0..2 * PAGE);
    // An end already behind the pointer: one pass, then the fall through.
    pages.fill(0..2 * PAGE, 0xff);
    let (r, after, moved) = run(&clzero_loop(JB), |c| {
        c.Rax = pages.at(512);
        c.Rcx = pages.at(0);
    });
    assert_eq!((r, moved, after.Rax), (CONTINUE, 12, pages.at(576)));
    assert_eq!(after.EFlags & STATUS_FLAGS, 0); // cmp gave 240h: no flag set
    check(512..576);
}

#[test]
fn clzero_loops_fall_back_to_one_line() {
    let pages = Pages::new(2, PAGE_READWRITE);
    let one_line = |setup: &dyn Fn(&mut CONTEXT), code: &[u8]| {
        pages.fill(0..2 * PAGE, 0xff);
        let rax = pages.at(0);
        let (r, after, moved) = run(code, |c| {
            c.Rax = rax;
            setup(c);
        });
        assert_eq!((r, moved, after.Rax), (CONTINUE, 3, rax));
        assert!(pages.bytes(0..64).iter().all(|&b| b == 0));
        assert!(pages.bytes(64..2 * PAGE).iter().all(|&b| b == 0xff));
    };
    // An end far past the allocation, or garbage.
    one_line(&|c| c.Rcx = pages.at(2 * PAGE + 64), &clzero_loop(JB));
    one_line(&|c| c.Rcx = pages.at(1 << 40), &clzero_loop(JB));
    one_line(&|c| c.Rcx = u64::MAX, &clzero_loop(JB));
    // jne with an end that the pointer never meets.
    one_line(&|c| c.Rcx = pages.at(1000), &clzero_loop(JNE));
    // A loop shape we do not take (signed jl).
    one_line(&|c| c.Rcx = pages.at(1024), &clzero_loop(0x7c));
    // Part of the range read-only.
    pages.protect(1, PAGE_READONLY);
    pages.fill(0..PAGE, 0xff);
    let (r, after, moved) = run(&clzero_loop(JB), |c| {
        c.Rax = pages.at(0);
        c.Rcx = pages.at(2 * PAGE);
    });
    assert_eq!((r, moved, after.Rax), (CONTINUE, 3, pages.at(0)));
    assert!(pages.bytes(0..64).iter().all(|&b| b == 0));
    assert!(pages.bytes(64..PAGE).iter().all(|&b| b == 0xff));
}

#[test]
fn clwb_is_skipped_whole() {
    let d32 = [0x78, 0x56, 0x34, 0x12];
    let cases: Vec<Vec<u8>> = vec![
        vec![0x66, 0x0f, 0xae, 0x31],
        vec![0x66, 0x41, 0x0f, 0xae, 0x34, 0x24],
        vec![0x66, 0x0f, 0xae, 0x74, 0x24, 0x10],
        [&[0x66, 0x0f, 0xae, 0x35][..], &d32].concat(),
        [&[0x66, 0x0f, 0xae, 0xb4, 0xc8][..], &d32].concat(),
        [&[0x2e, 0x67, 0x66, 0x42, 0x0f, 0xae, 0xb4, 0xc8][..], &d32].concat(),
    ];
    for code in cases {
        // The operand is never touched: an unmapped address is fine.
        let (r, after, moved) = run(&code, |c| {
            c.Rcx = 0x10;
            c.Rax = 0x20;
        });
        assert_eq!((r, moved), (CONTINUE, code.len() as u64), "{code:02x?}");
        assert_eq!((after.Rcx, after.Rax, after.EFlags), (0x10, 0x20, 0x202));
    }
}

#[test]
fn clwb_loops_are_finished_in_one_go() {
    // clwb [rcx]; add rcx, 40h; cmp rcx, rax; jbe top
    let code = [
        0x66, 0x0f, 0xae, 0x31, 0x48, 0x83, 0xc1, 0x40, 0x48, 0x39, 0xc1, 0x76, 0xf3,
    ];
    let (r, after, moved) = run(&code, |c| {
        c.Rcx = 0x1000;
        c.Rax = 0x1000 + 640;
    });
    assert_eq!((r, moved), (CONTINUE, 13));
    assert_eq!((after.Rcx, after.Rax), (0x1000 + 704, 0x1000 + 640));
    assert_eq!(
        after.EFlags & STATUS_FLAGS,
        compare_flags(0x1000 + 704, 0x1000 + 640)
    );
    // With an address-size prefix the loop is not taken: just the clwb.
    let mut short = vec![0x67];
    short.extend_from_slice(&code);
    let (r, after, moved) = run(&short, |c| c.Rcx = 0x1000);
    assert_eq!((r, moved, after.Rcx), (CONTINUE, 5, 0x1000));
}

#[test]
fn rdpru_reads_mperf_and_aperf() {
    let read = |ecx: u64| {
        let (r, after, moved) = run(&[0x0f, 0x01, 0xfd], |c| {
            c.Rcx = ecx;
            c.Rax = u64::MAX;
            c.Rdx = u64::MAX;
            c.Rbx = 7;
        });
        assert_eq!((r, moved), (CONTINUE, 3));
        assert!(after.Rax <= 0xffff_ffff && after.Rdx <= 0xffff_ffff);
        assert_eq!((after.Rcx, after.Rbx, after.EFlags), (ecx, 7, 0x202));
        after.Rdx << 32 | after.Rax
    };
    let first = read(0);
    assert_ne!(first, 0);
    let second = read(1);
    assert!(second >= first);
    // Only ECX counts.
    assert!(read(0xffff_ffff_0000_0000) >= second);
    assert!(read(0x5_0000_0001) >= second);
    for other in [2, 3, 0xffff_ffff, 0x100] {
        assert_eq!(read(other), 0);
    }
}

#[test]
fn monitorx_and_mwaitx_do_nothing_but_move_on() {
    for third in [0xfa, 0xfb] {
        let (r, after, moved) = run(&[0x0f, 0x01, third], |c| {
            c.Rax = 0x1234;
            c.Rbx = 1000;
            c.Rcx = 2;
            c.Rdx = 0;
        });
        assert_eq!((r, moved), (CONTINUE, 3));
        assert_eq!(
            (after.Rax, after.Rbx, after.Rcx, after.Rdx),
            (0x1234, 1000, 2, 0)
        );
        assert_eq!(after.EFlags, 0x202);
    }
}

#[test]
fn rdpid_writes_the_processor_number() {
    let (r, after, moved) = run(&[0xf3, 0x41, 0x0f, 0xc7, 0xff], |c| c.R15 = u64::MAX);
    assert_eq!((r, moved), (CONTINUE, 5));
    assert!(after.R15 < 64, "{}", after.R15); // a number within the processor group
    let (r, after, moved) = run(&[0xf3, 0x0f, 0xc7, 0xf8], |c| c.Rax = u64::MAX);
    assert_eq!((r, moved), (CONTINUE, 4));
    assert!(after.Rax < 64);
}

fn xmm(context: &mut CONTEXT) -> &mut [M128A; 16] {
    // SAFETY: the test contexts carry the floating-point state.
    unsafe { &mut context.Anonymous.FltSave.XmmRegisters }
}

fn reg(low: u64, high: u64) -> M128A {
    M128A {
        Low: low,
        High: high as i64,
    }
}

#[test]
fn extrq_and_insertq_work_on_the_saved_registers() {
    let setup = |c: &mut CONTEXT| {
        let x = xmm(c);
        for (i, r) in x.iter_mut().enumerate() {
            *r = reg(0x100 + i as u64, 0x200 + i as u64);
        }
        x[0] = reg(0x1111_2222_3333_4444, 0x5555);
        x[1] = reg(0xabcd, 0x7777);
    };
    // insertq xmm0, xmm1, 16, 16
    let (r, mut after, moved) = run(&[0xf2, 0x0f, 0x78, 0xc1, 0x10, 0x10], setup);
    assert_eq!((r, moved), (CONTINUE, 6));
    let x = *xmm(&mut after);
    assert_eq!((x[0].Low, x[0].High), (0x1111_2222_abcd_4444, 0));
    assert_eq!((x[1].Low, x[1].High), (0xabcd, 0x7777));
    assert_eq!((x[2].Low, x[15].High), (0x102, 0x20f));
    // extrq xmm0, 8, 20
    let (r, mut after, moved) = run(&[0x66, 0x0f, 0x78, 0xc0, 0x08, 0x14], setup);
    assert_eq!((r, moved), (CONTINUE, 6));
    assert_eq!((xmm(&mut after)[0].Low, xmm(&mut after)[0].High), (0x33, 0));
    // extrq xmm0, xmm2: fields in xmm2[13:8] and [5:0]; other bits ignored.
    let (r, mut after, moved) = run(&[0x66, 0x0f, 0x79, 0xc2], |c| {
        setup(c);
        xmm(c)[2] = reg(0xffff_ffff_ffff_d4c8, 0x9999); // index 20, length 8
    });
    assert_eq!((r, moved), (CONTINUE, 4));
    assert_eq!(xmm(&mut after)[0].Low, 0x33);
    assert_eq!(xmm(&mut after)[2].Low, 0xffff_ffff_ffff_d4c8);
    // insertq xmm1, xmm2: fields in xmm2[77:72] and [69:64].
    let (r, mut after, moved) = run(&[0xf2, 0x0f, 0x79, 0xca], |c| {
        setup(c);
        xmm(c)[1] = reg(u64::MAX, 0x4444);
        xmm(c)[2] = reg(0xab, 0x0408); // index 4, length 8
    });
    assert_eq!((r, moved), (CONTINUE, 4));
    let x = *xmm(&mut after);
    assert_eq!((x[1].Low, x[1].High), (0xffff_ffff_ffff_fabf, 0));
    assert_eq!((x[2].Low, x[2].High), (0xab, 0x0408));
    // XMM8 to XMM15 through REX: extrq xmm15, 4, 0; insertq xmm8, xmm9.
    let (r, mut after, moved) = run(&[0x66, 0x41, 0x0f, 0x78, 0xc7, 0x04, 0x00], |c| {
        xmm(c)[15] = reg(0x1234, 1);
    });
    assert_eq!((r, moved, xmm(&mut after)[15].Low), (CONTINUE, 7, 0x4));
    let (r, mut after, moved) = run(&[0xf2, 0x45, 0x0f, 0x79, 0xc1], |c| {
        xmm(c)[8] = reg(0xffff_0000, 0);
        xmm(c)[9] = reg(0x5, 0x0004); // index 0, length 4
    });
    assert_eq!(
        (r, moved, xmm(&mut after)[8].Low),
        (CONTINUE, 5, 0xffff_0005)
    );
    // A context without the floating-point state is left alone.
    let (r, _, moved) = run(&[0xf2, 0x0f, 0x79, 0xca], |c| {
        c.ContextFlags = CONTEXT_AMD64 | 0x3
    });
    assert_eq!((r, moved), (SEARCH, 0));
}

#[test]
fn bit_fields_follow_the_manual() {
    let v = 0x1111_2222_3333_4444;
    assert_eq!(extrq(v, 8, 20), 0x33);
    assert_eq!(extrq(v, 16, 0), 0x4444);
    assert_eq!(extrq(v, 1, 63), 0);
    assert_eq!(extrq(1 << 63, 1, 63), 1);
    assert_eq!(extrq(v, 4, 60), 0x1);
    assert_eq!(extrq(v, 63, 1), v >> 1);
    // Length 0 means 64: with index 0 the whole quadword.
    assert_eq!(extrq(v, 0, 0), v);
    // Only bits 5:0 of each field count.
    assert_eq!(extrq(v, 0xc8, 0xd4), 0x33);
    assert_eq!(extrq(v, 0x40, 0x40), v);
    // Undefined in the manual (past bit 63), here cut at bit 63.
    assert_eq!(extrq(v, 0, 8), v >> 8);
    assert_eq!(extrq(v, 60, 8), v >> 8);
    assert_eq!(extrq(u64::MAX, 8, 60), 0xf);

    let d = u64::MAX;
    assert_eq!(insertq(v, 0xabcd, 16, 16), 0x1111_2222_abcd_4444);
    assert_eq!(insertq(d, 0, 16, 16), 0xffff_ffff_0000_ffff);
    assert_eq!(insertq(0, 0xffff, 4, 4), 0xf0); // only `len` source bits
    assert_eq!(insertq(0, 1, 1, 63), 1 << 63);
    assert_eq!(insertq(d, 0, 1, 0), d - 1);
    assert_eq!(insertq(0, v, 0, 0), v); // length 0 is 64
    assert_eq!(insertq(0, v, 0x48, 0xc4), (v & 0xff) << 4); // bits 5:0
    // Undefined (past bit 63), here cut at bit 63.
    assert_eq!(insertq(0, 0xff, 8, 60), 0xf << 60);
    assert_eq!(insertq(0xf, v, 0, 4), 0xf | v << 4);
}

#[test]
fn compare_flags_are_those_of_cmp() {
    assert_eq!(compare_flags(5, 5), 0x44); // ZF, PF
    assert_eq!(compare_flags(1, 2), 0x95); // CF, PF, AF, SF
    assert_eq!(compare_flags(1 << 63, 1), 0x814); // OF, AF, PF
    assert_eq!(compare_flags(0x40, 0x3f), 0x10); // AF only (result 1: odd parity)
    assert_eq!(compare_flags(0x1240, 0x1000), 0); // 240h: odd parity, no carries
}

#[test]
fn other_instructions_are_not_ours() {
    for code in [
        &[0x0f, 0x0b][..],                // ud2
        &[0x90],                          // nop
        &[0x0f, 0x31],                    // rdtsc
        &[0x0f, 0x01, 0xf9],              // rdtscp
        &[0x66, 0x0f, 0x79, 0x00],        // extrq with memory: does not exist
        &[0x66, 0x0f, 0x78, 0xc8, 8, 20], // extrq with ModRM.reg 1
        &[0x66, 0x0f, 0xae, 0x39],        // clflushopt
        &[0xf0, 0x0f, 0x01, 0xfc],        // lock clzero
    ] {
        let (r, after, moved) = run(code, |c| c.Rax = 0x1000);
        assert_eq!((r, moved, after.Rax), (SEARCH, 0, 0x1000), "{code:02x?}");
    }
}

#[test]
fn reads_no_further_than_readable_memory() {
    let pages = Pages::new(2, PAGE_READWRITE);
    pages.protect(1, PAGE_NOACCESS);
    pages.fill(0..PAGE, 0xff);
    // clzero in the page's last bytes, zeroing the page's first line.
    pages.put(PAGE - 3, &CLZERO);
    let (r, _, moved) = run_at(pages.at(PAGE - 3), |c| c.Rax = pages.at(0));
    assert_eq!((r, moved), (CONTINUE, 3));
    assert!(pages.bytes(0..64).iter().all(|&b| b == 0));
    // mwaitx there.
    pages.put(PAGE - 3, &[0x0f, 0x01, 0xfb]);
    assert_eq!(run_at(pages.at(PAGE - 3), |_| {}).0, CONTINUE);
    // A loop cut by the page's end: just the one line.
    pages.fill(0..PAGE - 16, 0xff);
    pages.put(PAGE - 8, &clzero_loop(JB)[..8]);
    let (r, after, moved) = run_at(pages.at(PAGE - 8), |c| {
        c.Rax = pages.at(0);
        c.Rcx = pages.at(512);
    });
    assert_eq!((r, moved, after.Rax), (CONTINUE, 3, pages.at(0)));
    assert!(pages.bytes(64..512).iter().all(|&b| b == 0xff));
    // An instruction that would run past the page (not something the CPU
    // can fault on as invalid): refused without reading further.
    pages.put(PAGE - 6, &[0x66, 0x0f, 0xae, 0xb0, 0x01, 0x02]);
    assert_eq!(run_at(pages.at(PAGE - 6), |_| {}).0, SEARCH);
}

#[test]
fn the_memory_cache_forgets_what_a_query_contradicts() {
    let pages = Pages::new(1, PAGE_READWRITE);
    let at = pages.at(128) as usize;
    assert!(memory::writable(at, 64));
    assert!(memory::remembered(at));
    pages.protect(0, PAGE_READONLY);
    // Asking the system again finds it read-only and forgets it.
    assert!(!memory::writable_now(at, 64));
    assert!(!memory::remembered(at));
    assert!(!memory::writable(at, 64));
    // A remembered region goes stale for at most RECHECK_AFTER uses.
    pages.protect(0, PAGE_READWRITE);
    assert!(memory::writable(at, 64));
    pages.protect(0, PAGE_READONLY);
    let answers: Vec<bool> = (0..=memory::RECHECK_AFTER)
        .map(|_| memory::writable(at, 64))
        .collect();
    assert_eq!(answers.last(), Some(&false));
    assert!(!memory::remembered(at));
    // Ranges over several regions: all must be writable.
    let two = Pages::new(2, PAGE_READWRITE);
    assert!(memory::writable_now(two.at(0) as usize, 2 * PAGE));
    assert!(!memory::writable_now(two.at(0) as usize, 2 * PAGE + 1));
    two.protect(1, PAGE_READONLY);
    assert!(!memory::writable_now(two.at(0) as usize, 2 * PAGE));
    assert!(memory::writable_now(two.at(0) as usize, PAGE));
    assert!(!memory::writable_now(usize::MAX - 10, 64));
}

/// On a CPU with SSE4a (the AMD development machine, a Zen 4), the
/// emulation is compared with the real instructions for every length and
/// index, with random data and random bits outside the fields.
#[test]
fn sse4a_matches_this_cpu_when_it_has_it() {
    if !std::arch::is_x86_feature_detected!("sse4a") {
        return;
    }
    let mut seed = 0x9e37_79b9_7f4a_7c15u64;
    let mut next = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    for len in 0..64u64 {
        for idx in 0..64u64 {
            let fields = |r: u64| r & !0x3f3f | len | idx << 8;
            let dest = [next(), next()];
            let src_e = [fields(next()), next()];
            let src_i = [next(), fields(next())];
            // SAFETY: the instructions exist on this CPU; they touch only
            // xmm0, xmm1 and our arrays.
            let (hw_e, hw_i) =
                unsafe { (hardware(0x66, dest, src_e), hardware(0xf2, dest, src_i)) };
            let em_e = extrq(dest[0], src_e[0] as u8, (src_e[0] >> 8) as u8);
            let em_i = insertq(dest[0], src_i[0], src_i[1] as u8, (src_i[1] >> 8) as u8);
            // Defined and undefined (past bit 63) fields alike; the upper
            // quadword is zeroed.
            assert_eq!(hw_e, [em_e, 0], "extrq len {len} idx {idx}");
            assert_eq!(hw_i, [em_i, 0], "insertq len {len} idx {idx}");
        }
    }
}

/// `extrq xmm0, xmm1` (`66`) or `insertq xmm0, xmm1` (`f2`) on real
/// registers.
///
/// # Safety
/// The CPU has SSE4a.
unsafe fn hardware(prefix: u8, dest: [u64; 2], src: [u64; 2]) -> [u64; 2] {
    let mut d = dest;
    // SAFETY: loads and stores our arrays; the instruction is available.
    unsafe {
        if prefix == 0x66 {
            std::arch::asm!(
                "movdqu xmm0, xmmword ptr [{d}]",
                "movdqu xmm1, xmmword ptr [{s}]",
                ".byte 0x66, 0x0f, 0x79, 0xc1",
                "movdqu xmmword ptr [{d}], xmm0",
                d = in(reg) d.as_mut_ptr(), s = in(reg) src.as_ptr(),
                out("xmm0") _, out("xmm1") _,
            );
        } else {
            std::arch::asm!(
                "movdqu xmm0, xmmword ptr [{d}]",
                "movdqu xmm1, xmmword ptr [{s}]",
                ".byte 0xf2, 0x0f, 0x79, 0xc1",
                "movdqu xmmword ptr [{d}], xmm0",
                d = in(reg) d.as_mut_ptr(), s = in(reg) src.as_ptr(),
                out("xmm0") _, out("xmm1") _,
            );
        }
    }
    d
}
