//! What shaders wrote to copies in the card's memory of title memory the
//! title reserved write-combining is left on the card (`VICTORY_SKIP_WC_BACK=1`;
//! off by default).
//!
//! Ported from fxceless/victory-fork pull request #868 (head fef9592, by its
//! author; GPL-3.0-or-later), the part of it its author keeps apart from the
//! unsafe one: the reservations of `XMemVirtualAlloc` with the flags the title
//! asked for, and the test of whether a declared range lies wholly in one the
//! title asked write-combining. Left out: its census lines (logging only) and
//! `VICTORY_SKIP_WC_COMPARE`, which its author marks unsafe (a device loss was
//! reported with it), and which would put `Host::card_before`'s `declared` out
//! of step with its `by_marks` here.
//!
//! Why: a title does not read back memory it reserved write-combining (reads
//! of it are uncached, many times slower on the console); it writes it and
//! the GPU reads it. The pull request's census found "97 of 100 MB a frame
//! fetched back" from such memory, and with the skip, on an RTX 4070 /
//! Ryzen 5 5600, "184 → 148 ms (−20 %)", copy-back "76–100 → ~20 ms"; on an
//! RTX 5080 −11 to −13 %. The fetched pieces were also sent up again (their
//! merge made title memory dirty), so the uploads fall too.
//!
//! The risk, as its author states it: "Expect artifacts where the title's
//! CPU writes part of a 64 KiB piece the card wrote: the next upload sends the
//! stale bytes there", and anything that reads that title memory on the CPU
//! (a texture, another copy, a fill) sees it as it was before the shaders.

use std::collections::BTreeMap;
use std::sync::Mutex;

/// A reservation of `XMemVirtualAlloc`: its bytes and the console protection
/// the title asked for (before the runtime cleans it).
struct Reservation {
    len: usize,
    protect: u32,
}

/// The console's write-combining protection bit.
const WRITE_COMBINE: u32 = 0x400;

static RESERVATIONS: Mutex<BTreeMap<usize, Reservation>> = Mutex::new(BTreeMap::new());

/// Whether pieces of write-combining title memory are left on the card
/// (`VICTORY_SKIP_WC_BACK=1`).
pub(crate) fn skip_wc_back() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        let on = std::env::var_os("VICTORY_SKIP_WC_BACK").is_some_and(|v| v == "1");
        if on {
            tracing::info!("VICTORY_SKIP_WC_BACK=1: shader writes to write-combining title memory are not copied back");
        }
        on
    })
}

/// The runtime reserved `[at, at + len)` for the title (`XMemVirtualAlloc` at
/// an address of its choice) with the console `protect` the title asked for.
/// Older notes it overlaps are dropped: that memory was let go and reserved
/// again. (The title's `VirtualFree` reaches the host directly, so nothing
/// tells this module when a reservation goes; a range freed and then taken
/// by memory that is not reserved here keeps its old note.)
pub fn title_memory_reserved(at: usize, len: usize, protect: u32) {
    if !skip_wc_back() || at == 0 || len == 0 {
        return;
    }
    reserve_in(&mut RESERVATIONS.lock().unwrap_or_else(|e| e.into_inner()), at, len, protect);
}

/// [`title_memory_reserved`] into `reservations`.
fn reserve_in(reservations: &mut BTreeMap<usize, Reservation>, at: usize, len: usize, protect: u32) {
    let end = at.saturating_add(len);
    let stale: Vec<usize> = reservations
        .range(..end)
        .rev()
        .take_while(|&(&base, old)| base.saturating_add(old.len) > at)
        .map(|(&base, _)| base)
        .collect();
    for base in stale {
        reservations.remove(&base);
    }
    reservations.insert(at, Reservation { len, protect });
}

/// Whether every range of `ranges` lies wholly in one reservation the title
/// asked write-combining.
fn all_wc_in(reservations: &BTreeMap<usize, Reservation>, ranges: &[(usize, usize)]) -> bool {
    !ranges.is_empty()
        && ranges.iter().all(|&(from, to)| {
            // From the exclusive end: a reservation that begins inside the
            // range is found, and the range is not wholly in the one before.
            from < to
                && reservations.range(..to).next_back().is_some_and(|(&base, r)| {
                    base <= from && r.protect & WRITE_COMBINE != 0 && base.checked_add(r.len).is_some_and(|end| to <= end)
                })
        })
}

/// Whether all of `[from, to)` lies in one reservation the title asked
/// write-combining (recorded only with [`skip_wc_back`]; false otherwise).
pub(crate) fn wholly_write_combining(from: usize, to: usize) -> bool {
    skip_wc_back() && all_wc_in(&RESERVATIONS.lock().unwrap_or_else(|e| e.into_inner()), &[(from, to)])
}

/// Since the start, pieces left on the card by [`skip_back`]: count and bytes.
pub(crate) static SKIPPED: [std::sync::atomic::AtomicU64; 2] = [const { std::sync::atomic::AtomicU64::new(0) }; 2];

/// Whether a piece of `bytes` the compare flagged in a range declared as
/// `declared` (title addresses) stays on the card instead of being fetched
/// and merged into title memory. A piece is clipped to its declaration, so
/// the whole declaration must be write-combining memory.
pub(crate) fn skip_back(declared: (usize, usize), bytes: u64) -> bool {
    if !skip_wc_back() {
        return false;
    }
    let wc = all_wc_in(&RESERVATIONS.lock().unwrap_or_else(|e| e.into_inner()), &[declared]);
    if wc {
        use std::sync::atomic::Ordering::Relaxed;
        let n = SKIPPED[0].fetch_add(1, Relaxed) + 1;
        let b = SKIPPED[1].fetch_add(bytes, Relaxed) + bytes;
        if n.is_power_of_two() {
            tracing::info!("VICTORY_SKIP_WC_BACK: {n} pieces ({} MB) of write-combining memory left on the card so far", b >> 20);
        }
    }
    wc
}

#[cfg(test)]
mod tests {
    use super::{Reservation, WRITE_COMBINE, all_wc_in, reserve_in};
    use std::collections::BTreeMap;

    fn reservation(len: usize, protect: u32) -> Reservation {
        Reservation { len, protect }
    }

    /// From the pull request: a card window over a write-combining and an
    /// ordinary reservation side by side; only a range wholly in the first
    /// is skipped.
    #[test]
    fn a_write_in_the_ordinary_half_of_a_card_window_still_comes_back() {
        const MIB: usize = 1 << 20;
        let window = 8 * MIB;
        let middle = window + 2 * MIB;
        let map = BTreeMap::from([(window, reservation(2 * MIB, 0x404)), (middle, reservation(2 * MIB, 0x04))]);
        let wc = (middle - 65536, middle);
        let ordinary = (middle, middle + 65536);
        assert!(all_wc_in(&map, &[wc]));
        assert!(!all_wc_in(&map, &[ordinary]));
        assert!(!all_wc_in(&map, &[wc, ordinary]));
        assert!(!all_wc_in(&map, &[(wc.0, ordinary.1)]));
    }

    /// From the pull request: a declaration must fit one known reservation;
    /// empty, crossing, unknown and overflowing ranges are not skipped.
    #[test]
    fn a_declaration_must_fit_one_known_reservation() {
        let map = BTreeMap::from([
            (0x10000, reservation(0x10000, 0x404)),
            (0x20000, reservation(0x10000, 0x404)),
            (usize::MAX - 8, reservation(16, 0x404)),
        ]);
        assert!(all_wc_in(&map, &[(0x10000, 0x20000), (0x20000, 0x30000)]));
        assert!(!all_wc_in(&map, &[(0x1ffff, 0x20001)]));
        assert!(!all_wc_in(&map, &[(0x30000, 0x30001)]));
        assert!(!all_wc_in(&map, &[(0x10000, 0x10000)]));
        assert!(!all_wc_in(&map, &[(usize::MAX - 8, 1)]));
        assert!(!all_wc_in(&map, &[(usize::MAX - 8, usize::MAX)]));
        assert!(!all_wc_in(&map, &[]));
    }

    /// Ours: memory reserved again drops the older notes it overlaps, so a
    /// range that was write-combining and is now ordinary comes back; a
    /// reservation beside it is kept.
    #[test]
    fn memory_reserved_again_takes_the_new_protection() {
        let (a, b, c) = (0x10_0000usize, 0x20_0000usize, 0x40_0000usize);
        let mut map = BTreeMap::new();
        reserve_in(&mut map, a, 0x20_0000, 0x404);
        reserve_in(&mut map, c, 0x10_0000, 0x404);
        assert!(all_wc_in(&map, &[(b, b + 0x1000)]));
        // An ordinary reservation now over the second half of the first.
        reserve_in(&mut map, b, 0x10_0000, 0x04);
        assert!(!all_wc_in(&map, &[(b, b + 0x1000)]), "the new reservation is ordinary");
        assert!(!all_wc_in(&map, &[(a, a + 0x1000)]), "the old note is gone");
        assert_eq!(map.get(&b).map(|r| r.protect & WRITE_COMBINE), Some(0));
        assert!(all_wc_in(&map, &[(c, c + 0x1000)]), "the one beside it stays");
    }
}
