//! `VICTORY_SETTLE_ROOM=1` lets twice as many lists fly and gives a submission
//! eight times its usual budget for merging changed runs under shader writes.
//! Unset or `0` keeps the usual limits. `alt` alternates every 20 seconds,
//! starting with the usual limits. Flights retain their slots and upload memory
//! until they land, and each extra merged run can cost a copy on the card.
//! Merging uses the existing [`super::late_exact`] rules, including its race
//! when the title and a shader write the same byte without synchronizing.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicU8, Ordering};
use std::time::Instant;

enum Mode {
    Off,
    On,
    Alternate(Instant),
}

impl Mode {
    fn parse(value: Option<&str>) -> Self {
        match value {
            Some("1") => Self::On,
            Some("alt") => Self::Alternate(Instant::now()),
            _ => Self::Off,
        }
    }
}

fn alternate(seconds: u64) -> bool {
    seconds / 20 % 2 == 1
}

pub(super) fn on() -> bool {
    static MODE: OnceLock<Mode> = OnceLock::new();
    match MODE.get_or_init(|| Mode::parse(std::env::var("VICTORY_SETTLE_ROOM").ok().as_deref())) {
        Mode::Off => false,
        Mode::On => true,
        Mode::Alternate(start) => {
            static LAST: AtomicU8 = AtomicU8::new(2);
            let extra = alternate(start.elapsed().as_secs());
            if LAST.swap(u8::from(extra), Ordering::Relaxed) != u8::from(extra) {
                tracing::info!(extra, "VICTORY_SETTLE_ROOM=alt: flight limits changed");
            }
            extra
        }
    }
}

pub(super) fn list_limit(base: usize, extra: bool) -> usize {
    if extra { base.saturating_mul(2) } else { base }
}

pub(super) fn merge_budget(card_steps: bool, extra: bool) -> usize {
    let base = if card_steps { 4096 } else { 512 };
    if extra { base * 8 } else { base }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extra_room_is_opt_in_and_alt_starts_off() {
        for value in [None, Some("0"), Some("other")] {
            assert!(matches!(Mode::parse(value), Mode::Off));
        }
        assert!(matches!(Mode::parse(Some("1")), Mode::On));
        assert!(matches!(Mode::parse(Some("alt")), Mode::Alternate(_)));
        assert_eq!(
            [0, 19, 20, 39, 40].map(alternate),
            [false, false, true, true, false]
        );
    }

    #[test]
    fn extra_room_scales_the_configured_limits() {
        assert_eq!(list_limit(37, false), 37);
        assert_eq!(list_limit(37, true), 74);
        assert_eq!(list_limit(usize::MAX, true), usize::MAX);
        assert_eq!(
            [false, true].map(|extra| merge_budget(false, extra)),
            [512, 4096]
        );
        assert_eq!(
            [false, true].map(|extra| merge_budget(true, extra)),
            [4096, 32768]
        );
    }
}
