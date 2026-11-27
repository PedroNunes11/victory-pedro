//! Room for code the loader writes beside the title's: `jmp rel32` encodings and memory within
//! their reach. The title patches (`descriptor_sites`, `write_sites`) put their stubs there.
//! (Split out of the instruction emulation of the VIctory-7900XTX tree, where these lived.)

pub(crate) const STUBS_LEN: usize = 64 << 10;

/// `jmp rel32` at `at` to `to`, if it reaches.
pub(crate) fn jump(at: usize, to: usize) -> Option<[u8; 5]> {
    let rel = i32::try_from(to as i64 - (at as i64 + 5)).ok()?;
    let [a, b, c, d] = rel.to_le_bytes();
    Some([0xe9, a, b, c, d])
}

/// Memory for code within reach of a `jmp rel32` at `at`.
pub(crate) fn near(at: usize) -> Option<usize> {
    use windows_sys::Win32::System::Memory::{MEM_COMMIT, MEM_RESERVE, PAGE_EXECUTE_READWRITE, VirtualAlloc};
    let at = at & !0xffff;
    (0x10_0000..0x7000_0000).step_by(0x10_0000).flat_map(|d| [at.checked_sub(d), at.checked_add(d)]).flatten().find_map(|place| {
        // SAFETY: reserves free memory at that address or fails.
        let got = unsafe { VirtualAlloc(place as *const _, STUBS_LEN, MEM_COMMIT | MEM_RESERVE, PAGE_EXECUTE_READWRITE) } as usize;
        (got != 0).then_some(got)
    })
}

