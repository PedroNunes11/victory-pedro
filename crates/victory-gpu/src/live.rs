//! Live switches: runtime switches that can be flipped mid-run, at a frame
//! boundary with nothing in the air (the in-session A/B mode, [`crate::ab`]).
//!
//! Each switch is a [`Switch`] static at its use site. Its first read takes
//! the value from the environment (`VICTORY_<NAME>`) and registers an entry
//! in a global name -> atomic map; every later read is one relaxed atomic
//! load. [`set`] changes the value for every reader from then on, and
//! [`reset`] puts back what the environment gave. Only names in [`LIVE`]
//! may be set; [`BOOT_ONLY`] names (state built at boot from them) are
//! refused.
//!
//! Ported from ZigZagBoi1/ScarlettPort, `runtime/victory/crates/victory-core/
//! src/live.rs` (fxceless/victory-fork lineage, branch `fx-cand4` `efcc7b9`;
//! GPL-3.0-or-later, its authors), 2026-11-26. Changed here: the lists of
//! live and boot-only switches are this fork's.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};

/// Switches that are safe to flip at a frame boundary once flights are
/// settled (names without the `VICTORY_` prefix): each only changes how
/// work of ours is scheduled from then on.
pub(crate) const LIVE: &[&str] = &["TEXTURE_WIDE", "WIDE_REFRESH", "COPY_PLAIN_ALL", "CLAMP_NO_WAIT", "SKIP_GROUP_BARRIERS", "NO_CARD_METER", "CLAMP_BATCH", "INDIRECT_NO_COUNT", "BUFFER_PASS_QUIET", "DESCRIPTOR_DELTA", "SUBPAGE_PARTS", "GUARD_BITS", "GUARD_REST", "OPEN_ONE_VIEW", "SLOT_FLAGS", "PLACED_TEXTURES", "HEAP_PRESYNC", "SYNC_LIGHT", "GUARD_OUTSIDE", "REFRESH_PARTS", "CHEAP_TIMERS", "SYNC_KEYS", "REPEAT_CENSUS", "FILL_ONCE", "LIBRARY_DEFER", "TABLE_MEMO", "PIPELINE_WAIT_ONCE", "SLOT_REASONS", "ALIAS_PER_SOURCE", "PARTLY_LESS", "RETRY_TURNS", "WC_NO_BACK"];

/// Switches known and NOT settable live: state built from them at boot, or
/// threads and lines that exist or not for the whole run.
pub(crate) const BOOT_ONLY: &[&str] = &[
    "ASYNC_SUBMIT",
    "ASYNC_PRESENT",
    "RELAY",
    "LATE_WAIT",
    "CARD_SCATTER",
    "CARD_MARKS",
    "SKIP_WC_BACK",
    "GUARD_FAST",
    "GUARD_PARK",
    "HEAP_SYNC_WIDE",
    "HEAP_SYNC_BESIDE",
    "CBV_ALIGN_DOWN",
];

/// `VICTORY_TEXTURE_WIDE` -> `TEXTURE_WIDE`.
pub(crate) fn short(name: &str) -> &str {
    let n = name.trim();
    n.strip_prefix("VICTORY_").unwrap_or(n)
}

/// Whether `name` may be flipped live.
pub(crate) fn is_live(name: &str) -> bool {
    LIVE.contains(&short(name))
}

struct Entry {
    value: AtomicU64,
    boot: u64,
}

fn registry() -> &'static Mutex<HashMap<String, &'static Entry>> {
    static R: OnceLock<Mutex<HashMap<String, &'static Entry>>> = OnceLock::new();
    R.get_or_init(Default::default)
}

/// A value as a switch reads it: a number, or `on`/`true` (1) and
/// `off`/`false` (0); anything else is `None` (the default applies).
pub(crate) fn parse(v: &str) -> Option<u64> {
    let v = v.trim();
    match v {
        "on" | "true" | "yes" => Some(1),
        "off" | "false" | "no" => Some(0),
        _ => v.parse().ok(),
    }
}

fn entry(name: &str, default: u64) -> &'static Entry {
    let key = short(name).to_string();
    let mut r = registry().lock().unwrap_or_else(|e| e.into_inner());
    r.entry(key.clone()).or_insert_with(|| {
        let boot = std::env::var(format!("VICTORY_{key}")).ok().and_then(|v| parse(&v)).unwrap_or(default);
        let now = pending().as_mut().and_then(|m| m.remove(&key)).unwrap_or(boot);
        Box::leak(Box::new(Entry { value: AtomicU64::new(now), boot }))
    })
}

/// A live switch, declared as a static where it is read.
pub(crate) struct Switch {
    name: &'static str,
    default: u64,
    cell: OnceLock<&'static Entry>,
}

impl Switch {
    pub(crate) const fn new(name: &'static str, default: u64) -> Self {
        Switch { name, default, cell: OnceLock::new() }
    }

    /// The value now (one relaxed load after the first read).
    #[inline]
    pub(crate) fn get(&self) -> u64 {
        self.cell.get_or_init(|| entry(self.name, self.default)).value.load(Ordering::Relaxed)
    }

    /// `get() != 0`.
    #[inline]
    pub(crate) fn on(&self) -> bool {
        self.get() != 0
    }

    /// The value the environment gave at boot.
    #[cfg(test)]
    pub(crate) fn boot(&self) -> u64 {
        self.cell.get_or_init(|| entry(self.name, self.default)).boot
    }
}

/// Why [`set`] refused.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Refused {
    BootOnly(String),
    Unknown(String),
}

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Refused::BootOnly(n) => write!(f, "{n} is boot-only, not settable live"),
            Refused::Unknown(n) => write!(f, "{n} is not a known live switch"),
        }
    }
}

/// Whether `name` could be set; does nothing.
pub(crate) fn check(name: &str) -> Result<(), Refused> {
    let n = short(name);
    if is_live(n) {
        Ok(())
    } else if BOOT_ONLY.contains(&n) {
        Err(Refused::BootOnly(n.to_string()))
    } else {
        Err(Refused::Unknown(n.to_string()))
    }
}

/// Values set before their switch was first read: taken by its first read.
static PENDING: Mutex<Option<HashMap<String, u64>>> = Mutex::new(None);

fn pending() -> std::sync::MutexGuard<'static, Option<HashMap<String, u64>>> {
    PENDING.lock().unwrap_or_else(|e| e.into_inner())
}

/// Sets live switch `name` to `value` for every reader from now on. A
/// switch not read yet takes it at its first read (its boot value is still
/// the environment's or its default, for [`reset`]).
pub(crate) fn set(name: &str, value: u64) -> Result<(), Refused> {
    check(name)?;
    let key = short(name);
    let r = registry().lock().unwrap_or_else(|e| e.into_inner());
    if let Some(e) = r.get(key) {
        e.value.store(value, Ordering::Relaxed);
    } else {
        pending().get_or_insert_with(HashMap::new).insert(key.to_string(), value);
    }
    Ok(())
}

/// Puts `name` back to its boot value.
pub(crate) fn reset(name: &str) {
    let key = short(name);
    if let Some(m) = pending().as_mut() {
        m.remove(key);
    }
    let r = registry().lock().unwrap_or_else(|e| e.into_inner());
    if let Some(e) = r.get(key) {
        e.value.store(e.boot, Ordering::Relaxed);
    }
}

/// One test at a time where they set the process-wide switches.
#[cfg(test)]
pub(crate) fn tests_lock() -> std::sync::MutexGuard<'static, ()> {
    static ONE: Mutex<()> = Mutex::new(());
    ONE.lock().unwrap_or_else(|e| e.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// After the original: set changes every reader, reset puts the boot
    /// value back (here the default: the environment names none),
    /// boot-only and unknown names are refused.
    #[test]
    fn live_registry_set_reset_and_refuse() {
        static WIDE: Switch = Switch::new("WIDE_REFRESH", 0);
        let _one = tests_lock();
        reset("WIDE_REFRESH");
        assert!(!WIDE.on());
        assert_eq!(WIDE.boot(), 0);
        set("VICTORY_WIDE_REFRESH", 1).unwrap();
        assert!(WIDE.on());
        reset("WIDE_REFRESH");
        assert!(!WIDE.on());
        assert!(matches!(set("ASYNC_SUBMIT", 1), Err(Refused::BootOnly(_))));
        assert!(matches!(set("XYZ", 1), Err(Refused::Unknown(_))));
        assert_eq!(parse("on"), Some(1));
        assert_eq!(parse(" 250 "), Some(250));
    }
}
