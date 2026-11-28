//! The title's own hang watchdog, turned off at load (`VICTORY_TITLE_WATCHDOG=1`
//! or `KLBF_TITLE_WATCHDOG=1` keeps it).
//!
//! Ported from KLBF Lab (`v26.11.6`): GTA VI starts a watchdog thread (`+0x2215a60`)
//! that terminates the process by writing to address `0` (`+0x2215c35` / `+0x23cf683`)
//! if the main loop stalls >30 s during cold shader compilation. Turning its initial
//! `jne` exit check into an unconditional `jmp` lets the thread exit cleanly at once.

use crate::frame_rate::Patched;
use crate::resolution::Site;

/// +0x2215ab8: `mov al, [+0x71b6340] ; test al, 1 ; jne +0x2215dce`.
pub const STOP_CHECK: Site = Site {
    at: 0x2215_ab8,
    what: "the title's hang watchdog",
    bytes: &[0x8a, 0x05, 0x82, 0x08, 0xfa, 0x04, 0xa8, 0x01, 0x0f, 0x85, 0x08, 0x03, 0x00, 0x00],
};

/// Same bytes with the `jne` (6 bytes) turned into a `jmp` (5 bytes) + `nop` to the same exit.
pub fn stop_now_code() -> Vec<u8> {
    let mut code = STOP_CHECK.bytes[..8].to_vec();
    let rel = i32::from_le_bytes(STOP_CHECK.bytes[10..14].try_into().unwrap()) + 1;
    code.push(0xe9);
    code.extend(rel.to_le_bytes());
    code.push(0x90);
    code
}

/// Returns true (disable watchdog) unless `VICTORY_TITLE_WATCHDOG=1` or `KLBF_TITLE_WATCHDOG=1`.
pub fn asked_off() -> bool {
    let keep = |name: &str| std::env::var(name).is_ok_and(|v| v.trim() == "1");
    !keep("VICTORY_TITLE_WATCHDOG") && !keep("KLBF_TITLE_WATCHDOG")
}

/// Turns the title's watchdog off unless explicitly kept.
///
/// # Safety
/// The image stays mapped and no title thread is running its code yet.
#[cfg(windows)]
pub unsafe fn apply_asked(base: usize, size: usize) {
    if !asked_off() {
        println!("watchdog       left on (VICTORY_TITLE_WATCHDOG=1)");
        return;
    }
    // SAFETY: caller's contract.
    let done = unsafe { crate::resolution::write(base, size, &STOP_CHECK, &stop_now_code()) };
    match done {
        Patched::Done => {
            tracing::info!("watchdog: title hang watchdog disabled (VICTORY_TITLE_WATCHDOG=1 keeps it)");
        }
        Patched::Differs => {
            tracing::warn!("watchdog: not patched (bytes differ, another build?)");
        }
        Patched::Failed(why) => {
            tracing::warn!(why, "watchdog: not patched");
        }
    }
    println!("watchdog       title+{:#x}: {done:?}", STOP_CHECK.at);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_jump_goes_where_the_branch_went() {
        let target = |end: usize, rel: &[u8]| {
            (end as i64 + i64::from(i32::from_le_bytes(rel.try_into().unwrap()))) as usize
        };
        let code = stop_now_code();
        assert_eq!(code.len(), STOP_CHECK.bytes.len());
        assert_eq!(code[..8], STOP_CHECK.bytes[..8]);
        assert_eq!(code[8], 0xe9);
        assert_eq!(code[13], 0x90);
        assert_eq!(target(STOP_CHECK.at + 14, &STOP_CHECK.bytes[10..14]), 0x2215_dce);
        assert_eq!(target(STOP_CHECK.at + 13, &code[9..13]), 0x2215_dce);
    }
}
