//! Scheduling of descriptor records discovered by heap sync.

use std::collections::VecDeque;
use std::sync::{Mutex, OnceLock};

use super::{CONSTANT, PLACED};

/// Frames since a table visit during which a copy is left to a draw.
pub(super) const COPIED_FRAMES: u64 = 8;
/// Frames during which a slot still counts as belonging to a table.
pub(super) const TABLED_FRAMES: u64 = 240;

fn grace(name: &str, fallback: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(fallback)
        .max(1)
}

/// Frames a copied record waits for a draw before heap sync mirrors it.
pub(super) fn copied_grace() -> u64 {
    static FRAMES: OnceLock<u64> = OnceLock::new();
    *FRAMES.get_or_init(|| grace("VICTORY_COPIED_GRACE", 4))
}

/// Additional queue grace for a slot recently held by a table. These are
/// frames begun by the title, which can begin several per present.
pub(super) fn tabled_grace() -> u64 {
    static FRAMES: OnceLock<u64> = OnceLock::new();
    *FRAMES.get_or_init(|| grace("VICTORY_TABLED_GRACE", 8))
}

#[derive(Clone, Copy, Default)]
pub(super) struct Options {
    pub at_once: bool,
    pub orphans: bool,
}

impl Options {
    pub fn current() -> Self {
        fn enabled(name: &str, cached: &OnceLock<bool>) -> bool {
            #[cfg(feature = "diagnostics")]
            if let Some(on) = crate::ab::pick(name) {
                return on;
            }
            *cached.get_or_init(|| crate::switch::on(name))
        }
        static AT_ONCE: OnceLock<bool> = OnceLock::new();
        static ORPHANS: OnceLock<bool> = OnceLock::new();
        Self {
            at_once: enabled("VICTORY_COPIED_AT_ONCE", &AT_ONCE),
            orphans: enabled("VICTORY_TABLED_ORPHANS", &ORPHANS),
        }
    }

    pub fn schedule(self, kind: u32, table_age: Option<u64>) -> Schedule {
        if table_age.is_some_and(|age| age <= COPIED_FRAMES) {
            Schedule::Table
        } else if kind == CONSTANT
            || (kind == PLACED && (!self.at_once || table_age.is_some_and(|age| age <= TABLED_FRAMES)))
        {
            Schedule::Wait
        } else {
            Schedule::Now
        }
    }
}

pub(super) enum Schedule {
    Now,
    Wait,
    Table,
}

/// The view identity as well as the slot. A replacement must get its own
/// turn, even if the old record's grace expires before the next page scan.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct Record {
    pub index: u32,
    pub key: u64,
}

#[derive(Default)]
pub(super) struct Deferred(Mutex<VecDeque<(u64, u64, Vec<Record>)>>);

impl Deferred {
    pub fn push(&self, frame: u64, found_ms: u64, records: Vec<Record>) {
        if !records.is_empty() {
            self.0
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push_back((frame, found_ms, records));
        }
    }

    pub fn ready(&self, frame: u64, grace: u64, keep: impl Fn(u32, u64) -> bool) -> Vec<(Record, u64)> {
        let mut batches = self.0.lock().unwrap_or_else(|e| e.into_inner());
        let mut ready = Vec::new();
        while batches
            .front()
            .is_some_and(|(written, _, _)| frame.saturating_sub(*written) >= grace)
        {
            let (written, found_ms, records) = batches.pop_front().unwrap();
            ready.extend(
                records
                    .into_iter()
                    .filter(|r| keep(r.index, written))
                    .map(|r| (r, found_ms)),
            );
        }
        drop(batches);
        ready.sort_unstable();
        ready.dedup_by_key(|(record, _)| *record);
        ready
    }
}
