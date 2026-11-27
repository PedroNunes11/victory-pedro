//! Drives the in-session A/B schedule (`VICTORY_AB`, [`crate::ab_core`])
//! from the present path: frame times per present and device removal; and
//! changes variant at a frame boundary with every flight settled.
//!
//! Ported from ZigZagBoi1/ScarlettPort, `runtime/victory/crates/victory-gpu/
//! src/ab.rs` (fxceless/victory-fork lineage, branch `fx-cand4` `efcc7b9`;
//! GPL-3.0-or-later, its authors), 2026-11-26. Changed here: no per-second
//! meter, no fast-boot world detector (draws only); before a change the
//! queues' lines run first (`VICTORY_ASYNC_SUBMIT`), then the flights settle.

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::ab_core::{Scheduler, Step, Timing, apply, parse_schedule};

static STATE: Mutex<Option<Option<Scheduler>>> = Mutex::new(None);
static REMOVED: AtomicBool = AtomicBool::new(false);

fn with<R>(f: impl FnOnce(&mut Scheduler) -> R) -> Option<R> {
    let mut g = STATE.lock().unwrap_or_else(|e| e.into_inner());
    let s = g.get_or_insert_with(|| {
        let text = std::env::var("VICTORY_AB").ok().filter(|t| !t.trim().is_empty())?;
        match parse_schedule(&text) {
            Ok(v) => {
                let t = Timing::from_env();
                tracing::warn!(
                    "ab: {} variants, windows of {:.0} s (first {:.1} s not measured), warm-up {:.0} s, cycles {} ({text})",
                    v.len(),
                    t.window as f64 / 1e9,
                    t.settle as f64 / 1e9,
                    t.warmup as f64 / 1e9,
                    if t.cycles == 0 { "until exit".to_string() } else { t.cycles.to_string() }
                );
                // SAFETY: registers a function with the CRT's exit table.
                unsafe { atexit(at_exit) };
                Some(Scheduler::new(v, t))
            }
            Err(e) => {
                tracing::error!("ab: VICTORY_AB refused, no A/B this run: {e}");
                None
            }
        }
    });
    s.as_mut().map(f)
}

fn now_ns() -> u64 {
    static START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
    START.get_or_init(std::time::Instant::now).elapsed().as_nanos() as u64
}

/// Runs what the queues hold, settles every flight, then applies variant `i`.
fn change(s: &Scheduler, i: usize) {
    crate::queue::before_sync();
    crate::host::settle_for_present();
    apply(&s.variants, &s.variants[i]);
    let sets: Vec<String> = s.variants[i].sets.iter().map(|(k, v)| format!("{k}={v}")).collect();
    tracing::warn!("ab: variant {} now ({})", s.variants[i].name, if sets.is_empty() { "boot values".into() } else { sets.join(",") });
}

/// [`WORLD_DRAWS`] draws or more a frame for [`WORLD_FRAMES`] frames in a
/// row (menus draw 10-100, the world 1,500+).
const WORLD_DRAWS: u64 = 1500;
const WORLD_FRAMES: u32 = 60;

fn world_by_draws() -> bool {
    use std::sync::atomic::{AtomicU32, AtomicU64};
    // `VICTORY_AB_WORLD_DRAWS=<n>`: the draws a frame that mean the world
    // ([`WORLD_DRAWS`] by default); 0 starts the windows at once, counted
    // by the clock from launch, for what happens while the world loads
    // (run 417c: the texture queue filled and emptied before 1,500 draws).
    static DRAWS: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
    let draws = *DRAWS.get_or_init(|| {
        std::env::var("VICTORY_AB_WORLD_DRAWS").ok().and_then(|v| v.trim().parse().ok()).unwrap_or(WORLD_DRAWS)
    });
    if draws == 0 {
        return true;
    }
    static LAST: AtomicU64 = AtomicU64::new(0);
    static RUN: AtomicU32 = AtomicU32::new(0);
    let now = crate::draw::drawn();
    let d = now.saturating_sub(LAST.swap(now, Ordering::Relaxed));
    let run = if d >= draws { RUN.load(Ordering::Relaxed) + 1 } else { 0 };
    RUN.store(run, Ordering::Relaxed);
    run >= WORLD_FRAMES
}

/// A frame was presented (the title's present call returned).
pub(crate) fn presented() {
    static OFF: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    if *OFF.get_or_init(|| std::env::var_os("VICTORY_AB").is_none_or(|v| v.is_empty())) {
        return;
    }
    if REMOVED.swap(false, Ordering::Relaxed) {
        with(Scheduler::device_removed);
    }
    let in_world = world_by_draws();
    with(|s| match s.frame(now_ns(), in_world) {
        Step::Nothing => {}
        Step::Apply(i) => change(s, i),
        Step::Closed { line, summary, next } => {
            tracing::warn!("{line}");
            if let Some(t) = summary {
                for l in t.lines() {
                    tracing::warn!("{l}");
                }
            }
            if s.done() {
                tracing::warn!("ab: schedule done");
            }
            if let Some(i) = next {
                change(s, i);
            }
        }
    });
}

/// The host device was removed (taken at the next present, if any).
#[allow(dead_code)]
pub(crate) fn device_removed() {
    REMOVED.store(true, Ordering::Relaxed);
    // No present may come: log what there is now.
    if let Ok(g) = STATE.try_lock()
        && let Some(Some(s)) = g.as_ref()
    {
        for l in s.summary().lines() {
            tracing::warn!("{l} (at device removal)");
        }
    }
}

unsafe extern "C" {
    fn atexit(f: extern "C" fn()) -> i32;
}

/// At process exit: the summary of every window measured.
extern "C" fn at_exit() {
    if let Ok(g) = STATE.try_lock()
        && let Some(Some(s)) = g.as_ref()
    {
        let t = s.summary();
        for l in t.lines() {
            tracing::warn!("{l} (at exit)");
        }
        eprint!("{t}");
    }
}
