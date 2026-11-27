//! Which title memory the title wrote since a host copy took it (decision
//! 0007, "Copies of title memory"). The runtime reserves title GPU memory
//! with write watching (`victory-hle` xmem), so the kernel keeps a dirty bit
//! per 4 KiB page; asking for a range's written pages also clears them. Host
//! copies of the same memory can overlap (nested buffers, textures in
//! buffers), so what one asks is kept here for the others: a journal of
//! each page's last write, numbered by the asking.
//!
//! Physical-page views (xmem's sections) cannot be write-watched. There a
//! copy leaves each 64 KiB block it took read-only, and our vectored handler
//! takes the first write to it: it notes the block as written and gives the
//! write access back. The runtime tells us when it maps or unmaps views
//! ([`title_memory_remapped`]), which resets their protection. Blocks found
//! wholly uncommitted are skipped while they remain so; one query can
//! confirm a whole region, including ordinary reservations between views.
//!
//! Writes into physical pages through another view are seen too: the
//! runtime tells us the views of the pages it wrote itself ([`writing_held`],
//! xmem's `write_physical` and DirectStorage's page destinations, through
//! [`title_memory_written`]), and a write by the title through any view of
//! a guarded page faults like one at the guarded address (the guard makes
//! every view of the page read-only, [`open`] gives them all back; not a
//! page of more than [`MANY_VIEWS`] views; `VICTORY_VIEW_GUARD=0` guards
//! the address alone, as before).
//!
//! A system call writing into a guarded block, or into a block guarded for
//! another view's sake, fails; the runtime opens them first, and holds them
//! open until the write is done ([`writing_held`]): no other view of those
//! pages is guarded meanwhile. An overlapped `ReadFile` is held only until
//! it is started.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

/// The unit tracked and copied: 64 KiB, the console's page (the kernel's
/// 4 KiB pages fold into it). Finer units made the checks of the world's
/// big buffers cost more than the copies they saved.
pub(crate) const PAGE: usize = 64 << 10;
/// The kernel's page, the unit of its answers.
const KERNEL_PAGE: usize = 4096;

/// A title-memory range whose changed pages another subsystem needs to read.
pub(crate) struct Tracked {
    start: usize,
    end: usize,
    seen: Mutex<BTreeMap<usize, u64>>,
}

/// Tracks a runtime GPU-memory range, including physical-page views when
/// `views` is true. Other process memory is not tracked.
pub(crate) fn track(start: usize, len: usize, views: bool) -> Option<Tracked> {
    let end = start.checked_add(len)?;
    if len == 0 || writes(start, end, views).is_none() {
        return None;
    }
    Some(Tracked {
        start,
        end,
        seen: Mutex::default(),
    })
}

struct Journal {
    /// Numbers the askings; 0 is "never".
    asked: u64,
    /// Page number → the asking that last found it written.
    written: BTreeMap<usize, u64>,
    /// Room for the kernel's answer.
    found: Vec<usize>,
    /// Page ranges asked ahead for a submission's refreshes ([`ask_ahead`])
    /// and the asking that did: `writes` answers for a range inside one
    /// from the journal, without another kernel call.
    ahead: Vec<(usize, usize)>,
    ahead_asked: u64,
    /// The ahead asking stands for a whole submission ([`ask_once`]): no
    /// other ahead asking is made, and `ahead` stays, until
    /// [`ask_once_end`].
    kept: bool,
    /// Diagnostics ([`found_written`]): the frame now, and the asking that
    /// was the newest when it began.
    #[cfg(feature = "diagnostics")]
    since: (u64, u64),
}

static JOURNAL: Mutex<Journal> = Mutex::new(Journal {
    asked: 0,
    written: BTreeMap::new(),
    found: Vec::new(),
    ahead: Vec::new(),
    ahead_asked: 0,
    kept: false,
    #[cfg(feature = "diagnostics")]
    since: (0, 0),
});

/// Since the last frame line (diagnostics): 64 KiB pages the kernel or the
/// guards found written by the title, each counted once a frame however
/// many askings found it. What a tracker of pages by fault would take as
/// faults a frame if it made every page read-only again once a frame
/// (item 2's step R3, `DESENHO-RASTREIO-EVENTO-ITEM2.md`).
#[cfg(feature = "diagnostics")]
static FOUND_WRITTEN: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Counts into [`FOUND_WRITTEN`] a page found written whose journal entry
/// was `before` (the asking that last found it, if any).
#[cfg(feature = "diagnostics")]
fn found_written(j: &mut Journal, before: Option<u64>) {
    let frame = crate::frame::current();
    if j.since.0 != frame {
        j.since = (frame, j.asked);
    }
    if before.is_none_or(|w| w < j.since.1) {
        FOUND_WRITTEN.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
}

/// Ranges of one submission closer than this are asked about together:
/// walking the pages between them costs less than another kernel call (a
/// call that resets written pages costs 30-70 microseconds whatever the
/// range).
const AHEAD_GAP: usize = (8 << 20) / PAGE;
/// The most one asking covers (its answer needs 8 bytes per 4 KiB page).
const AHEAD_MAX: usize = (1 << 30) / PAGE;

/// Asks the kernel about many ranges `(start, end)` at once, before the
/// [`writes`] of each: ranges close together are one asking, and `writes`
/// then answers from the journal until [`ask_ahead_done`]. The world's
/// submissions refresh thousands of host copies (6,664 askings a frame,
/// 456 ms, in run 14). Ranges that are not write-watched, or lie alone, are
/// left to `writes`.
pub(crate) fn ask_ahead(ranges: &[(usize, usize)]) {
    let mut pieces: Vec<(usize, usize)> = ranges
        .iter()
        .filter(|r| r.0 < r.1)
        .map(|&(start, end)| (start / PAGE, end.div_ceil(PAGE)))
        .collect();
    if pieces.len() < 2 {
        return;
    }
    pieces.sort_unstable();
    let mut j = JOURNAL.lock().unwrap_or_else(|e| e.into_inner());
    let j = &mut *j;
    // The submission's one asking stands ([`ask_once`]): what it does not
    // hold is left to `writes`.
    if j.kept {
        return;
    }
    // View memory the guards hold is left to `writes`, which answers it
    // from the guards ([`known_view`]): the kernel's write watch can only
    // fail there. Asked here with the others, a run holding such a range
    // failed, was asked again by halves down to single ranges, failing
    // each time, and the range then went to `writes` all the same (after
    // triippywhale d0581a55). `VICTORY_VIEW_ASK=1` asks as before.
    #[cfg(feature = "diagnostics")]
    let all = pieces.len();
    pieces.retain(|&(first, _)| !known_view(first * PAGE));
    #[cfg(feature = "diagnostics")]
    AHEAD_VIEWS.fetch_add((all - pieces.len()) as u64, std::sync::atomic::Ordering::Relaxed);
    j.asked += 1;
    j.ahead_asked = j.asked;
    j.ahead.clear();
    let mut i = 0;
    while i < pieces.len() {
        let first = pieces[i].0;
        let mut last = pieces[i].1;
        let mut k = i + 1;
        while k < pieces.len()
            && pieces[k].0 <= last + AHEAD_GAP
            && last.max(pieces[k].1) - first <= AHEAD_MAX
        {
            last = last.max(pieces[k].1);
            k += 1;
        }
        ask_run(j, &pieces[i..k]);
        i = k;
    }
}

/// Since the last frame line: the kernel askings that took a millisecond
/// or more, their microseconds, and the longest asking's. An asking walks
/// page tables, some ten microseconds for most ranges; in run 75 the 2,644
/// askings of a frame took 219 ms in the lines where the pass over the
/// heap's views ran long, 25-50 ms in the others. These numbers say
/// whether a few askings wait for something or all of them are slow.
#[cfg(feature = "diagnostics")]
static SLOW: [std::sync::atomic::AtomicU64; 3] = [const { std::sync::atomic::AtomicU64::new(0) }; 3];

#[cfg(feature = "diagnostics")]
fn slow(us: u64) {
    use std::sync::atomic::Ordering::Relaxed;
    if us >= 1000 {
        SLOW[0].fetch_add(1, Relaxed);
        SLOW[1].fetch_add(us, Relaxed);
    }
    SLOW[2].fetch_max(us, Relaxed);
}

/// Since the last frame line: ranges of view memory the guards hold that a
/// submission's askings ahead ([`ask_ahead`], [`ask_once`]) left out. Each
/// was a kernel asking that failed, or several, before.
#[cfg(feature = "diagnostics")]
pub(crate) static AHEAD_VIEWS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// [`SLOW`], taken.
#[cfg(feature = "diagnostics")]
pub(crate) fn slow_asks() -> [u64; 3] {
    [0, 1, 2].map(|i| SLOW[i].swap(0, std::sync::atomic::Ordering::Relaxed))
}

/// Since the last frame line, the kernel's write-watch askings by who
/// asked: a submission's askings ahead of ranges close together
/// ([`ask_run`]), the ranges alone it asks once ([`ask_once`]), and
/// [`writes`] on its own. For each: askings made, of them failed,
/// microseconds in the kernel, KiB asked about, written 4 KiB pages the
/// kernel reported, and microseconds noting them in the journal.
#[cfg(feature = "diagnostics")]
static ASKS: [[std::sync::atomic::AtomicU64; 6]; 3] =
    [const { [const { std::sync::atomic::AtomicU64::new(0) }; 6] }; 3];

#[cfg(feature = "diagnostics")]
fn asked_by(who: usize, parts: [u64; 6]) {
    for (count, n) in ASKS[who].iter().zip(parts) {
        count.fetch_add(n, std::sync::atomic::Ordering::Relaxed);
    }
}

/// With the frame line: [`ASKS`] per frame over `frames`.
#[cfg(feature = "diagnostics")]
pub(crate) fn asks_line(frames: f64) {
    let a = ASKS.each_ref().map(|w| w.each_ref().map(|c| c.swap(0, std::sync::atomic::Ordering::Relaxed) as f64 / frames));
    if a.iter().all(|w| w[0] == 0.0) {
        return;
    }
    let part = |w: &[f64; 6]| {
        format!(
            "{:.0} askings ({:.1} failed), {:.2} ms in the kernel over {:.0} MB, {:.0} written pages noted in {:.2} ms",
            w[0],
            w[1],
            w[2] / 1e3,
            w[3] / 1024.0,
            w[4],
            w[5] / 1e3
        )
    };
    tracing::info!(
        "write-watch askings per frame by who asked: ahead of a submission's ranges close together {}; ranges alone, \
         asked once for a submission {}; on their own {}; 64 KiB pages found written, each once a frame: {:.0} (the \
         faults a tracker of pages by fault would take)",
        part(&a[0]),
        part(&a[1]),
        part(&a[2]),
        FOUND_WRITTEN.swap(0, std::sync::atomic::Ordering::Relaxed) as f64 / frames
    );
}

/// The submission's refreshes are done: `writes` asks the kernel again.
pub(crate) fn ask_ahead_done() {
    let mut j = JOURNAL.lock().unwrap_or_else(|e| e.into_inner());
    if !j.kept {
        j.ahead.clear();
    }
}

/// Whether a submission asks the kernel once, before its first refresh,
/// about every range its lists refresh (`VICTORY_ASK_ONCE=1`; off by
/// default).
///
/// A submission's refreshes are done in groups, between the title's own
/// buffer copies (each copy flushes the refreshes recorded before it), and
/// every group asked the kernel again: 2,644 askings a frame, 70 a
/// submission. With the switch the journal answers all of them from one
/// asking made when the submission starts.
///
/// What changes: memory the title's other threads write while the
/// submission is being prepared is seen by the next submission's asking,
/// not by this one's later groups. What this submission's lists read was
/// written before they were submitted, so before the asking. Our own
/// writes into title memory while preparing are told to the journal
/// ([`wrote`]) or end the asking ([`ask_once_end`]).
pub(crate) fn once() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    // On by default since run 81 (with card buffers: 375-450 ms a frame
    // against 573-647, the picture unchanged over four minutes);
    // `VICTORY_ASK_ONCE=0` asks per range, as before.
    *ON.get_or_init(|| std::env::var_os("VICTORY_ASK_ONCE").is_none_or(|v| v != "0"))
}

/// With [`once`]: asks about `ranges` (everything a submission's lists
/// refresh) as [`ask_ahead`] does, and lets that asking stand until
/// [`ask_once_end`].
pub(crate) fn ask_once(ranges: &[(usize, usize)]) {
    if !once() {
        return;
    }
    ask_once_end();
    ask_ahead(ranges);
    let mut j = JOURNAL.lock().unwrap_or_else(|e| e.into_inner());
    let j = &mut *j;
    if j.ahead.is_empty() {
        // `ask_ahead` asked nothing (fewer than two ranges).
        j.asked += 1;
        j.ahead_asked = j.asked;
    }
    // `ask_ahead` leaves a range that lies alone to `writes`, which asks
    // once per flush: with one asking for the submission that was every
    // such range asked again at each of its 70 flushes (run 76: 2,300
    // askings a frame with the switch on, as without it). They are asked
    // here, once; what is not write-watched stays with `writes`.
    for &(start, end) in ranges {
        if start >= end {
            continue;
        }
        // View memory the guards hold is not asked about: the asking can
        // only fail there, one failed kernel call for each such copy at
        // every submission, and `writes` answers it from the guards
        // (after triippywhale cc368c7c).
        if known_view(start) {
            #[cfg(feature = "diagnostics")]
            AHEAD_VIEWS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            continue;
        }
        let (first, last) = (start / PAGE, end.div_ceil(PAGE));
        if !j.ahead.iter().any(|&(a, b)| a <= first && last <= b) && ask_kernel(j, first, last, 1) {
            j.ahead.push((first, last));
        }
    }
    j.kept = !j.ahead.is_empty();
}

/// The submission's one asking stands no longer: its refreshes are done,
/// or title memory was written in a way the journal was not told.
pub(crate) fn ask_once_end() {
    let mut j = JOURNAL.lock().unwrap_or_else(|e| e.into_inner());
    if j.kept {
        j.kept = false;
        j.ahead.clear();
    }
}

/// We wrote title memory `[at, at + len)` while a submission's one asking
/// stands (the title's buffer copies, done at submission): its pages count
/// as written after that asking, so the refreshes that follow copy them.
pub(crate) fn wrote(at: usize, len: usize) {
    let mut j = JOURNAL.lock().unwrap_or_else(|e| e.into_inner());
    if !j.kept || len == 0 {
        return;
    }
    j.asked += 1;
    let now = j.asked;
    // The asking that stands moves with it: `asked` and `writes` give it as
    // "now", and a page noted written after "now" stays stale for every
    // copy that takes it (copied at `now`, written at `now + 1`): copied
    // again at every refresh, and a reason to settle flights
    // (fxceless/victory-fork#410).
    j.ahead_asked = now;
    for page in at / PAGE..at.saturating_add(len).div_ceil(PAGE) {
        j.written.insert(page, now);
    }
}

/// We wrote the 64 KiB page numbered `page` of title memory through a
/// view of our own (`crate::inplace::write_own`): no guard was opened and
/// no kernel write-watch bit set, so the journal is told here. The page,
/// and the same physical page at its other title addresses (`others`,
/// page numbers), count as written at a new asking, which every host copy
/// that holds them takes at its next refresh. Returns the asking that had
/// last found `page` written (0: never) and the new one, for the copy
/// that wrote to note itself current.
pub(crate) fn wrote_own(page: usize, others: &[usize]) -> (u64, u64) {
    let mut j = JOURNAL.lock().unwrap_or_else(|e| e.into_inner());
    let j = &mut *j;
    j.asked += 1;
    let now = j.asked;
    // As in [`wrote`]: an asking that stands for a submission, or for a
    // flush, moves with it, or a copy taking the page at the standing
    // asking would find it written "later" at every refresh.
    if j.kept || !j.ahead.is_empty() {
        j.ahead_asked = now;
    }
    let before = j.written.insert(page, now).unwrap_or(0);
    for &other in others {
        if other != page {
            j.written.insert(other, now);
        }
    }
    (before, now)
}

/// One kernel asking for `pieces` (page ranges in order); by halves if it
/// fails, as a run across two reservations or into memory that is not
/// write-watched does. A piece alone is left to [`writes`].
fn ask_run(j: &mut Journal, pieces: &[(usize, usize)]) {
    if pieces.len() < 2 {
        return;
    }
    let first = pieces[0].0;
    let last = pieces.iter().map(|p| p.1).max().unwrap_or(first);
    if ask_kernel(j, first, last, 0) {
        j.ahead.push((first, last));
        return;
    }
    let (a, b) = pieces.split_at(pieces.len() / 2);
    ask_run(j, a);
    ask_run(j, b);
}

/// Asks the kernel for the written pages of pages `[first, last)` and notes
/// them in the journal under the ahead asking; false (and nothing changed)
/// if the range is not write-watched memory of one reservation.
fn ask_kernel(
    j: &mut Journal,
    first: usize,
    last: usize,
    #[cfg_attr(not(feature = "diagnostics"), allow(unused_variables))] who: usize,
) -> bool {
    use windows_sys::Win32::System::Memory::GetWriteWatch;
    const WRITE_WATCH_FLAG_RESET: u32 = 1;
    let mut count = (last - first) * PAGE / KERNEL_PAGE;
    j.found.resize(count.max(j.found.len()), 0);
    let mut granularity = 0;
    #[cfg(feature = "diagnostics")]
    let started = std::time::Instant::now();
    // SAFETY: the answer buffer holds `count` entries.
    let failed = unsafe {
        GetWriteWatch(
            WRITE_WATCH_FLAG_RESET,
            (first * PAGE) as *const _,
            (last - first) * PAGE,
            j.found.as_mut_ptr().cast(),
            &mut count,
            &mut granularity,
        )
    } != 0;
    #[cfg(feature = "diagnostics")]
    let (kernel, kib) = (started.elapsed().as_micros() as u64, ((last - first) * PAGE / 1024) as u64);
    if failed {
        #[cfg(feature = "diagnostics")]
        asked_by(who, [1, 1, kernel, kib, 0, 0]);
        return false;
    }
    #[cfg(feature = "diagnostics")]
    {
        use crate::copystat::{C, add};
        add(C::Watches, 1);
        add(C::WatchUs, started.elapsed().as_micros() as u64);
        slow(started.elapsed().as_micros() as u64);
    }
    #[cfg(feature = "diagnostics")]
    let noting = std::time::Instant::now();
    let now = j.ahead_asked;
    for k in 0..count {
        let page = j.found[k] / PAGE;
        let _before = j.written.insert(page, now);
        #[cfg(feature = "diagnostics")]
        found_written(j, _before);
    }
    #[cfg(feature = "diagnostics")]
    asked_by(who, [1, 0, kernel, kib, count as u64, noting.elapsed().as_micros() as u64]);
    true
}

/// What [`writes`] would say of `[start, end)` from the journal alone, if
/// the range lies in one asked ahead ([`ask_ahead`], [`ask_once`]): the
/// pages written since tracking began and the asking that last found each
/// written, with the number of the asking that stands (as [`writes`] gives
/// it). `None` if the kernel would have to be asked.
pub(crate) fn asked(start: usize, end: usize) -> Option<(u64, Vec<(usize, u64)>)> {
    let (first, last) = (start / PAGE, end.div_ceil(PAGE));
    let j = JOURNAL.lock().unwrap_or_else(|e| e.into_inner());
    j.ahead
        .iter()
        .any(|&(a, b)| a <= first && last <= b)
        .then(|| (j.ahead_asked, j.written.range(first..last).map(|(&p, &w)| (p, w)).collect()))
}

/// The pages of `[start, end)` (whole pages) as last written, for pages
/// written since tracking began: `(page number, asking)` in page order, and
/// this asking's number, which is higher than any in the list but those the
/// kernel just reported. `None` if the memory is not write-watched (nor,
/// with `guarded`, view memory, which is then guarded).
pub(crate) fn writes(start: usize, end: usize, guarded: bool) -> Option<(u64, Vec<(usize, u64)>)> {
    writes_how(start, end, guarded).map(|(now, written, _)| (now, written))
}

/// [`writes`], and whether the guards answered (the memory is view
/// memory, which the write watch does not know).
pub(crate) fn writes_how(start: usize, end: usize, guarded: bool) -> Option<(u64, Vec<(usize, u64)>, bool)> {
    use windows_sys::Win32::System::Memory::GetWriteWatch;
    const WRITE_WATCH_FLAG_RESET: u32 = 1;
    let (first, last) = (start / PAGE, end.div_ceil(PAGE));
    let mut held = JOURNAL.lock().unwrap_or_else(|e| e.into_inner());
    let j = &mut *held;
    // Asked ahead with the submission's other ranges: the journal has it.
    if j.ahead.iter().any(|&(a, b)| a <= first && last <= b) {
        return Some((
            j.ahead_asked,
            j.written
                .range(first..last)
                .map(|(&p, &w)| (p, w))
                .collect(),
            false,
        ));
    }
    let mut count = (last - first) * PAGE / KERNEL_PAGE;
    j.found.resize(count.max(j.found.len()), 0);
    let mut granularity = 0;
    #[cfg(feature = "diagnostics")]
    let started = std::time::Instant::now();
    // View memory the guards already hold cannot be write-watched: the
    // kernel asking would only fail. Copies of it are flushed many times a
    // frame, each time paying that failed call before the guards answer.
    let known_view = guarded && known_view(first * PAGE);
    // SAFETY: the answer buffer holds `count` entries.
    let failed = known_view
        || unsafe {
            GetWriteWatch(
                WRITE_WATCH_FLAG_RESET,
                (first * PAGE) as *const _,
                (last - first) * PAGE,
                j.found.as_mut_ptr().cast(),
                &mut count,
                &mut granularity,
            )
        } != 0;
    #[cfg(feature = "diagnostics")]
    crate::copystat::add(
        if known_view {
            crate::copystat::C::WatchKnownView
        } else {
            crate::copystat::C::WatchFailed
        },
        failed as u64,
    );
    let by_guards = failed;
    #[cfg(feature = "diagnostics")]
    if failed && !known_view && guarded {
        failed_asking(first * PAGE, last * PAGE);
    }
    let guarded = if failed {
        count = 0;
        if !guarded {
            return None;
        }
        #[cfg(feature = "diagnostics")]
        let guarding = std::time::Instant::now();
        let written = guard_later(first * PAGE, last * PAGE, guard_outside());
        #[cfg(feature = "diagnostics")]
        crate::copystat::add(crate::copystat::C::GuardUs, guarding.elapsed().as_micros() as u64);
        let (written, later) = written?;
        arming = later;
        written
    } else {
        Vec::new()
    };
    #[cfg(feature = "diagnostics")]
    {
        use crate::copystat::{C, add};
        add(C::Watches, 1);
        add(C::WatchUs, started.elapsed().as_micros() as u64);
        slow(started.elapsed().as_micros() as u64);
    }
    j.asked += 1;
    let now = j.asked;
    #[cfg(feature = "diagnostics")]
    let noting = std::time::Instant::now();
    let pages = j.found[..count].iter().map(|&at| at / PAGE).chain(guarded);
    for page in pages.collect::<Vec<_>>() {
        let _before = j.written.insert(page, now);
        #[cfg(feature = "diagnostics")]
        found_written(j, _before);
    }
    #[cfg(feature = "diagnostics")]
    asked_by(2, [0, 0, 0, 0, 0, noting.elapsed().as_micros() as u64]);
    let answer = (
        now,
        j.written
            .range(first..last)
            .map(|(&p, &w)| (p, w))
            .collect(),
        by_guards,
    ))
}

/// [`writes`] with `guarded`, for memory a last asking found to be view
/// memory ([`writes_how`]): the write watch, which fails for views, is
/// not asked first. `None` if the memory is no view now; the caller then
/// asks [`writes`].
///
/// A copy or a texture in view memory asked the write watch at every
/// refresh, and a submission's one asking ([`ask_once`], [`ask_ahead`])
/// asked it again for each such range, halving runs that held one:
/// system calls that fail, thousands a frame. `VICTORY_GUARD_KNOWN=0`
/// asks the write watch first every time, as before.
///
/// Also whether every block of the range is clean after the asking
/// ([`guard_changes`] then tells whether it still is).
pub(crate) fn writes_guarded(start: usize, end: usize) -> Option<(u64, Vec<(usize, u64)>, bool)> {
    let (first, last) = (start / PAGE, end.div_ceil(PAGE));
    // Held across the guards' answer, as `writes` holds it: another asker
    // must not find the blocks clean before their writes are in the
    // journal.
    let mut j = JOURNAL.lock().unwrap_or_else(|e| e.into_inner());
    #[cfg(feature = "diagnostics")]
    let started = std::time::Instant::now();
    let (written, settled) = guard_settled(first * PAGE, last * PAGE)?;
    #[cfg(feature = "diagnostics")]
    {
        use crate::copystat::{C, add};
        let us = started.elapsed().as_micros() as u64;
        add(C::GuardUs, us);
        add(C::Watches, 1);
        add(C::WatchUs, us);
        slow(us);
    }
    j.asked += 1;
    let now = j.asked;
    for page in written {
        j.written.insert(page, now);
    }
    Some((
        now,
        j.written
            .range(first..last)
            .map(|(&p, &w)| (p, w))
            .collect(),
        settled,
    ))
}

/// Whether memory found to be view memory is asked of the guards alone
/// from then on ([`writes_guarded`]); `VICTORY_GUARD_KNOWN=0`: no.
pub(crate) fn guards_known() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("VICTORY_GUARD_KNOWN").is_none_or(|v| v != "0"))
}

/// Diagnostics: one in 2,048 of the askings the kernel's write watch
/// refused for memory the guards did not know (some 200 a frame in the
/// first rooms, each followed by a question to Windows about the memory):
/// what the memory is and what the guards hold of its first block.
#[cfg(feature = "diagnostics")]
fn failed_asking(start: usize, end: usize) {
    use windows_sys::Win32::System::Memory::{MEMORY_BASIC_INFORMATION, VirtualQuery};
    static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    if !N.fetch_add(1, std::sync::atomic::Ordering::Relaxed).is_multiple_of(2048) {
        return;
    }
    // SAFETY: plain data filled in by the query.
    let mut info: MEMORY_BASIC_INFORMATION = unsafe { std::mem::zeroed() };
    // SAFETY: a valid out pointer of the right size.
    let n = unsafe { VirtualQuery(start as *const _, &mut info, size_of_val(&info)) };
    let (protected, clean, absent, viewed, aliased) = {
        let g = guards();
        let b = start / BLOCK;
        (g.protected.contains(&b), g.clean.contains(b), g.absent.contains(b), g.viewed.contains(&b), g.aliased.contains(&b))
    };
    tracing::info!(
        "a failed write-watch asking (one in 2048): {start:#x}+{:#x}; Windows says (answered {n}): base {:#x} region {:#x} state {:#x} type {:#x} protect {:#x} allocation protect {:#x}; \
         the guards of its first block: protected {protected} clean {clean} absent {absent} viewed {viewed} aliased {aliased}; is a view: {}",
        end - start,
        info.BaseAddress as usize,
        info.RegionSize,
        info.State,
        info.Type,
        info.Protect,
        info.AllocationProtect,
        is_view(start, end)
    );
}

impl Tracked {
    /// Takes changed pages of `[from, to)`, cut to the tracked range. A
    /// partial page stays changed so a later take can read its other bytes.
    pub(crate) fn take(&self, from: usize, to: usize) -> Vec<(usize, usize)> {
        let (from, to) = (from.max(self.start), to.min(self.end));
        if from >= to {
            return Vec::new();
        }
        let Some((_, pages)) = writes(from, to, true) else {
            return Vec::new();
        };
        let mut seen = self.seen.lock().unwrap_or_else(|e| e.into_inner());
        let mut ranges: Vec<(usize, usize)> = Vec::new();
        for (page, write) in pages {
            if seen.get(&page).is_some_and(|&last| last >= write) {
                continue;
            }
            let (start, end) = ((page * PAGE).max(from), ((page + 1) * PAGE).min(to));
            if start >= end {
                continue;
            }
            // Keep partial pages dirty: the caller has not taken the rest.
            if (page * PAGE).max(self.start) >= from
                && ((page + 1) * PAGE).min(self.end) <= to
            {
                seen.insert(page, write);
            }
            match ranges.last_mut() {
                Some((_, last)) if *last == start => *last = end,
                _ => ranges.push((start, end)),
            }
        }
        ranges
    }
}

/// Guarded blocks of view memory.
const BLOCK: usize = PAGE;

/// Whether the block holding `start` is view memory the guards already
/// hold (read-only by our hand, or noted absent): asking the kernel's write
/// watch about it can only fail. `VICTORY_VIEW_ASK=1` (or
/// `VICTORY_GUARD_ASK=1`) asks the kernel anyway, as before.
fn known_view(start: usize) -> bool {
    static ASK: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    let ask = *ASK.get_or_init(|| {
        ["VICTORY_VIEW_ASK", "VICTORY_GUARD_ASK"]
            .iter()
            .any(|n| std::env::var_os(n).is_some_and(|v| v == "1"))
    });
    if ask {
        return false;
    }
    // A clean block is read-only by our hand ([`guard_fast`]); a parked one
    // is noted absent ([`guard_park`]).
    let quiet = if guard_park() { all_quiet(start / BLOCK, start / BLOCK + 1) } else { guard_fast() && all_clean(start / BLOCK, start / BLOCK + 1) };
    if quiet {
        GUARD_FAST[1].fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        return true;
    }
    let g = guards();
    let first = start / BLOCK;
    g.protected.contains(&first) || g.absent.contains(first) || (view_known() && g.viewed.contains(&first))
}

/// A set of blocks, a bit each, 64 to a word, the words by their number
/// (after victory-fork pull request #987). Whether all of a range is in it
/// is answered a word at a time: the guards ask that of the clean blocks
/// some 10,000 times a frame in the open world, over hundreds of thousands
/// of blocks, and counted one by one in a tree of blocks that was most of
/// the 30 ms a frame of "guards".
struct Blocks(BTreeMap<usize, u64>);

impl Blocks {
    const fn new() -> Self {
        Self(BTreeMap::new())
    }

    /// The bits of `range`'s blocks in word `word`.
    fn mask(word: usize, range: &std::ops::Range<usize>) -> u64 {
        let low = word * 64;
        let (from, to) = (range.start.max(low), range.end.min(low + 64));
        if from >= to {
            return 0;
        }
        (u64::MAX >> (64 - (to - from))) << (from - low)
    }

    fn insert(&mut self, block: usize) {
        *self.0.entry(block / 64).or_insert(0) |= 1 << (block % 64);
    }

    fn remove(&mut self, block: usize) {
        if let Some(bits) = self.0.get_mut(&(block / 64)) {
            *bits &= !(1 << (block % 64));
            if *bits == 0 {
                self.0.remove(&(block / 64));
            }
        }
    }

    fn contains(&self, block: usize) -> bool {
        self.0.get(&(block / 64)).is_some_and(|bits| bits >> (block % 64) & 1 != 0)
    }

    /// Takes `bits` of word `word` out of the set.
    fn clear(&mut self, word: usize, bits: u64) {
        if let Some(held) = self.0.get_mut(&word) {
            *held &= !bits;
            if *held == 0 {
                self.0.remove(&word);
            }
        }
    }

    fn extend(&mut self, range: std::ops::Range<usize>) {
        if range.is_empty() {
            return;
        }
        for word in range.start / 64..=(range.end - 1) / 64 {
            *self.0.entry(word).or_insert(0) |= Self::mask(word, &range);
        }
    }

    /// Whether every block of `range` is in the set.
    fn holds(&self, range: std::ops::Range<usize>) -> bool {
        if range.is_empty() {
            return true;
        }
        let (first, last) = (range.start / 64, (range.end - 1) / 64);
        let mut words = self.0.range(first..=last);
        (first..=last).all(|word| {
            let mask = Self::mask(word, &range);
            words.next().is_some_and(|(&at, &bits)| at == word && bits & mask == mask)
        })
    }

    /// The set's blocks in `range`, in order.
    fn range(&self, range: std::ops::Range<usize>) -> impl Iterator<Item = usize> + '_ {
        let first = range.start / 64;
        let last = (range.end.max(range.start + 1) - 1) / 64;
        let words = if range.is_empty() { self.0.range(1..1) } else { self.0.range(first..=last) };
        words.flat_map(move |(&word, &bits)| {
            let mut left = bits & Self::mask(word, &range);
            std::iter::from_fn(move || {
                (left != 0).then(|| {
                    let bit = left.trailing_zeros() as usize;
                    left &= left - 1;
                    word * 64 + bit
                })
            })
        })
    }
}

struct Guards {
    /// Blocks we made read-only.
    protected: BTreeSet<usize>,
    /// Of those, the ones not written since a copy took them.
    clean: Blocks,
    /// Blocks already copied as zeros. While they remain wholly uncommitted,
    /// the copy can skip hundreds of MB of failing kernel reads per world frame.
    /// With [`holes_level`] 2 those also in `clean` were found uncommitted
    /// and hold as holes; the others are due a look.
    absent: Blocks,
    /// Blocks whose physical page was guarded together with its other views
    /// (those views' blocks are in it too): a write to one opens them all.
    /// A write range can be huge (`fread`), so only these are looked up.
    aliased: BTreeSet<usize>,
    /// With [`view_known`]: blocks made read-only at least once since the
    /// runtime last mapped or unmapped them. They are view memory whether
    /// their guard stands now or a write has opened it.
    viewed: BTreeSet<usize>,
    /// Block ranges a kernel write is under way into ([`writing_held`]):
    /// `(first, end, id)`.
    held: Vec<(usize, usize, u64)>,
    /// `(guarded, alias)`: other views of a guarded block's page that were
    /// not guarded with it because a kernel write was under way into them;
    /// the guarded block is taken as written again when the write ends.
    deferred: Vec<(usize, usize)>,
    /// Clean blocks with another view of their page that is not guarded
    /// (a kernel write under way there, a protection refused, too many
    /// views): a write through it is not seen, so they are never armed
    /// (after upstream VIctory #2455).
    exposed: BTreeSet<usize>,
    /// Blocks a submission's shaders write through marked views
    /// (`crate::marks`), from the moment they were found clean until the
    /// copy-back: what is known of their bytes from before ([`Armed`]).
    armed: BTreeMap<usize, Armed>,
    /// Blocks an asynchronous kernel write may still land in
    /// ([`writing_async`]): from, to, and until when. Treated as held.
    pins: Vec<(usize, usize, std::time::Instant)>,
    /// Room for blocks' bytes, made before the submission: the fault
    /// handler that keeps a block's bytes takes it from here and allocates
    /// nothing.
    spare: Vec<Box<[u8]>>,
    /// With [`holes_level`] 1 and 2: runs of uncommitted blocks as Windows
    /// described them in frame `holes_frame`, first block to end.
    holes: BTreeMap<usize, usize>,
    holes_frame: u64,
    /// With [`holes_level`] 2: the frame whose share of the holes was last
    /// made due ([`holes_due`]).
    due_frame: u64,
}

/// What an armed block's bytes were before the submission
/// (`crate::marks`).
pub(crate) enum Armed {
    /// Nothing changed it since it was armed: they are title memory's now.
    Untouched,
    /// Its bytes as they were when something was about to write it first.
    Saved(Box<[u8]>),
    /// It changed with its bytes from before not kept (a remapping, a write
    /// told when done, no room): unknown.
    Lost,
}

/// Blocks' bytes kept ready for armed blocks the title writes first
/// ([`arm`]).
const SPARE: usize = 32;

/// Arms `[start, end)` for a submission's shaders that mark their writes
/// (`crate::marks`): true if every block of it is guarded and clean, with
/// every other view of its pages guarded too, so that any change to it
/// until the copy-back is seen ([`armed_take`]). False changes nothing:
/// the caller keeps a snapshot instead.
pub(crate) fn arm(start: usize, end: usize) -> bool {
    let (first, last) = (start / BLOCK, end.div_ceil(BLOCK));
    let mut g = guards();
    if first >= last
        || !view_guard()
        || !g.clean.holds(first..last)
        || g.exposed.range(first..last).next().is_some()
        // Holes held clean ([`holes_level`] 2) have no bytes to keep.
        || g.absent.range(first..last).next().is_some()
    {
        return false;
    }
    for block in first..last {
        g.armed.entry(block).or_insert(Armed::Untouched);
    }
    while g.spare.len() < SPARE {
        g.spare.push(vec![0u8; BLOCK].into_boxed_slice());
    }
    true
}

/// What is known of the bytes from before of the armed block holding
/// `at`; it is armed no more (our own write back into it is no title
/// write). None if it was not armed.
pub(crate) fn armed_take(at: usize) -> Option<Armed> {
    guards().armed.remove(&(at / BLOCK))
}

/// The submission's copy-back is done: nothing is armed any more.
pub(crate) fn disarm() {
    let mut g = guards();
    let armed = std::mem::take(&mut g.armed);
    for (_, state) in armed {
        if let Armed::Saved(bytes) = state
            && g.spare.len() < SPARE
        {
            g.spare.push(bytes);
        }
    }
}

/// `block` is about to change (`before`: its bytes are still those from
/// before) or has changed: if armed and untouched, its bytes are kept, or
/// it becomes unknown.
fn touched(g: &mut Guards, block: usize, before: bool) {
    let Some(state) = g.armed.get_mut(&block) else {
        return;
    };
    if !matches!(state, Armed::Untouched) {
        return;
    }
    *state = Armed::Lost;
    if !before {
        return;
    }
    let Some(mut bytes) = g.spare.pop() else {
        return;
    };
    // Through the kernel: this may run in the fault handler.
    if crate::host::read_into_kernel((block * BLOCK) as *const u8, &mut bytes) {
        g.armed.insert(block, Armed::Saved(bytes));
    } else {
        g.spare.push(bytes);
    }
}

/// [`touched`] for the armed blocks of `[first, last)`.
fn touched_range(g: &mut Guards, first: usize, last: usize, before: bool) {
    if g.armed.is_empty() {
        return;
    }
    let armed: Vec<usize> = g.armed.range(first..last).map(|(&block, _)| block).collect();
    for block in armed {
        touched(g, block, before);
    }
}

/// Whether a block once guarded stays known as view memory while its
/// guard is open (`VICTORY_VIEW_KNOWN=1`; off by default).
///
/// A range whose first block the title wrote since the last check has
/// that block neither read-only nor noted absent, so nothing says it is a
/// view: the kernel's write watch is asked and fails, and then Windows is
/// asked what the memory is (`is_view`), a walk of the page tables to the
/// end of the pages that share the first one's state. In runs 222 and 225
/// that was 338-450 failed askings a frame and 6.5-7.8 ms a frame of the
/// submitting thread in `VirtualQuery`. With the switch the block is
/// remembered from the time it was first made read-only until the runtime
/// maps or unmaps it ([`title_memory_remapped`]).
///
/// `VICTORY_VIEW_KNOWN=alt`: twenty seconds with, twenty without, in turn,
/// each change said in the log, for measuring it in one scene. (Blocks
/// noted in one phase are forgotten at a remapping in either.)
fn view_known() -> bool {
    /// 0 off, 1 on, 2 in turn.
    static MODE: std::sync::OnceLock<u8> = std::sync::OnceLock::new();
    match *MODE.get_or_init(|| match std::env::var("VICTORY_VIEW_KNOWN").as_deref() {
        Ok("1") => 1,
        Ok("alt") => 2,
        _ => 0,
    }) {
        0 => false,
        1 => true,
        _ => {
            static EPOCH: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
            let on = (EPOCH.get_or_init(std::time::Instant::now).elapsed().as_secs() / 20) % 2 == 1;
            static LAST: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(2);
            if LAST.swap(u8::from(on), std::sync::atomic::Ordering::Relaxed) != u8::from(on) {
                tracing::info!(
                    "view memory stays known while its guard is open from now (VICTORY_VIEW_KNOWN=alt): {}",
                    if on { "ON" } else { "off, as usual" }
                );
            }
            on
        }
    }
}

static GUARDS: Mutex<Guards> = Mutex::new(Guards {
    protected: BTreeSet::new(),
    clean: Blocks::new(),
    absent: Blocks::new(),
    aliased: BTreeSet::new(),
    viewed: BTreeSet::new(),
    held: Vec::new(),
    deferred: Vec::new(),
    exposed: BTreeSet::new(),
    armed: BTreeMap::new(),
    pins: Vec::new(),
    spare: Vec::new(),
    holes: BTreeMap::new(),
    holes_frame: 0,
    due_frame: 0,
});

impl Guards {
    /// Whether a kernel write is under way into `block` (held, or pinned
    /// for an asynchronous one: [`writing_async`]); expired pins go.
    fn writing_into(&mut self, block: usize) -> bool {
        if self.held.iter().any(|&(from, to, _)| (from..to).contains(&block)) {
            return true;
        }
        if self.pins.is_empty() {
            return false;
        }
        let now = std::time::Instant::now();
        self.pins.retain(|&(_, _, until)| until > now);
        self.pins.iter().any(|&(from, to, _)| (from..to).contains(&block))
    }
}

fn guards() -> std::sync::MutexGuard<'static, Guards> {
    GUARDS.lock().unwrap_or_else(|e| e.into_inner())
}

/// Clean blocks made written, by region of addresses ([`region_shift`];
/// folded into [`REGIONS`] counters): while the count of a range's regions
/// stays as it was, no block of it has been taken out of the clean ones,
/// so a range all clean then is all clean still ([`guard_changes`]).
/// Counted under the guards' lock, after the blocks are taken out.
static REGION_CHANGES: [AtomicU64; REGIONS] = [const { AtomicU64::new(0) }; REGIONS];
const REGIONS: usize = 16384;

/// The bits of an address below its region: regions of
/// `VICTORY_CLEAN_REGION_KB` KiB (1,024 if unset; 65,536 as before), at
/// least a block. Of 64 MiB, the regions were too large to say anything:
/// the title writes some of nearly every 64 MiB of its view memory every
/// frame (its big GPU heap is 174 MB), so of some 9,800 askings a frame
/// only about 300 found their range's regions unchanged. Smaller regions
/// only make the answer finer: a range whose regions did not change has no
/// block taken out of the clean ones, whatever their size.
/// The size of [`region_shift`]'s regions in KiB (for the frame line).
#[cfg(feature = "diagnostics")]
pub(crate) fn clean_region_kb() -> usize {
    (1usize << region_shift()) >> 10
}

fn region_shift() -> u32 {
    static SHIFT: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
    *SHIFT.get_or_init(|| {
        let kb = std::env::var("VICTORY_CLEAN_REGION_KB")
            .ok()
            .and_then(|v| v.trim().parse::<usize>().ok())
            .filter(|&kb| kb > 0)
            .unwrap_or(1024);
        (kb.saturating_mul(1024)).max(BLOCK).next_power_of_two().trailing_zeros()
    })
}

/// Counts a change in the regions of blocks `[from, to)`.
fn changed(from: usize, to: usize) {
    if from >= to {
        return;
    }
    let (first, last) = (block_region(from), block_region(to - 1));
    if last - first >= REGIONS {
        for counter in &REGION_CHANGES {
            counter.fetch_add(1, Ordering::Release);
        }
        return;
    }
    for region in first..=last {
        REGION_CHANGES[region % REGIONS].fetch_add(1, Ordering::Release);
    }
}

fn block_region(block: usize) -> usize {
    block.saturating_mul(BLOCK) >> region_shift()
}

/// The changes counted so far in the regions of `[start, end)`: as long
/// as it is the same, no guarded block there was written, mapped or
/// unmapped since. It only grows.
pub(crate) fn guard_changes(start: usize, end: usize) -> u64 {
    if start >= end {
        return 0;
    }
    let shift = region_shift();
    let (first, last) = (start >> shift, (end - 1) >> shift);
    if last - first >= REGIONS {
        return REGION_CHANGES.iter().map(|c| c.load(Ordering::Acquire)).fold(0, u64::wrapping_add);
    }
    (first..=last)
        .map(|region| REGION_CHANGES[region % REGIONS].load(Ordering::Acquire))
        .fold(0, u64::wrapping_add)
}

/// Whether the block `page` is read-only by our hand and unwritten since a
/// copy took it (diagnostics of `VICTORY_BACK_OWN_CHECK`).
pub(crate) fn clean(page: usize) -> bool {
    guards().clean.contains(page)
}

/// For view memory: the pages of `[start, end)`'s blocks written since
/// they were last guarded (all of a block's pages), guarding them again
/// before the caller copies them. `None` if the memory is not a view.
#[cfg(test)]
fn guard(start: usize, end: usize) -> Option<Vec<usize>> {
    guard_settled(start, end).map(|(written, _)| written)
}

/// [`guard`], and whether every block of the range is clean after it:
/// none absent, held or left unguarded, which the next asking would take
/// as written again whatever happened.
fn guard_settled(start: usize, end: usize) -> Option<(Vec<usize>, bool)> {
    // A range whose first block the guards hold already (read-only by our
    // hand, or noted absent) is a view: no question to Windows. The
    // question walks the page tables to the end of the pages that share the
    // first one's state, and a guarded view is one long run of read-only
    // pages: 0.6 ms for the title's 174 MB heap, 16 times a frame, and most
    // of the 68 ms a frame of "guards" (run 58).
    // `VICTORY_GUARD_ASK=1` asks every time, as before.
    static ASK: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    let ask = *ASK.get_or_init(|| std::env::var_os("VICTORY_GUARD_ASK").is_some_and(|v| v == "1"));
    install();
    // One taking of the guards' lock when the range is known to be a view
    // (nearly every asking): the lock is the fault handler's too.
    let mut g = guards();
    let first = start / BLOCK;
    let known = !ask
        && (g.protected.contains(&first) || g.absent.contains(first) || (view_known() && g.viewed.contains(&first)));
    if !known {
        drop(g);
        if !is_view(start, end) {
            return None;
        }
        g = guards();
    }
    apply_changed(&mut g);
    holes_due(&mut g);
    let mut written = Vec::new();
    let mut run: Option<usize> = None;
    // The range's clean blocks, in order: read off the set in one pass,
    // not looked up one block at a time. A frame asks about some 10 GB of
    // view memory, 150,000 blocks, nearly all clean: the lookups were most
    // of the 60-90 ms a frame of "guards" (run 54). What the loop adds to
    // the set meanwhile is the block it stands at or ones before it.
    #[cfg(feature = "diagnostics")]
    crate::copystat::add(crate::copystat::C::GuardAsks, 1);
    if g.clean.holds(first..last) {
        #[cfg(feature = "diagnostics")]
        crate::copystat::add(crate::copystat::C::GuardAllClean, 1);
        // Nothing written, mapped or new since the range was last taken.
        return Some((written, true));
    }
    let clean: Vec<usize> = g.clean.range(first..last).collect();
    let mut next_clean = 0;
    let mut uncommitted_until = first;
    let mut previous: Option<usize> = None;
    let rest = guard_rest();
    for block in blocks {
        if bits {
            if let Some(p) = previous
                && block > p + 1
                && let Some(from) = run.take()
            {
                protect_or_defer(&mut g, from, p + 1, later, &mut arming);
            }
            previous = Some(block);
        }
        while next_clean < clean.len() && clean[next_clean] < block {
            next_clean += 1;
        }
        let is_clean = !bits && next_clean < clean.len() && clean[next_clean] == block;
        let mut open = block < last && !is_clean;
        if open && g.absent.contains(block) {
            // Ordinary commits need no runtime mapping notification. Reuse
            // this asking's query for the following blocks in the same region.
            if block >= uncommitted_until {
                uncommitted_until = hole_end(&mut g, block);
            }
            open = block >= uncommitted_until;
            if open {
                g.absent.remove(block);
                if holes_level() == 2 {
                    untold(block);
                }
            } else if holes_level() == 2 {
                // A hole still: it holds until it is due again, or told
                // mapped or committed.
                g.clean.insert(block);
                #[cfg(feature = "diagnostics")]
                crate::copystat::add(crate::copystat::C::HolesLooked, 1);
            }
        }
        if open {
            written.extend(block * BLOCK / PAGE..(block + 1) * BLOCK / PAGE);
            if g.writing_into(block) {
                // A kernel write is under way here: guarding the block
                // would fail it. Taken as written; the next copy after the
                // write guards it.
            } else if g.protected.contains(&block) {
                // Written through the runtime, or guarded for another
                // view's sake, still read-only.
                g.clean.insert(block);
            } else {
                run.get_or_insert(block);
                continue;
            }
        }
        if let Some(from) = run.take() {
            protect_or_defer(&mut g, from, block, later, &mut arming);
        }
    }
    let settled = g.clean.holds(first..last);
    Some((written, settled))
}

/// Whether `[start, end)` is view memory. The title's views lie in
/// placeholders, mapped and unmapped block by block as it streams, so a
/// range can begin in a block that is not mapped right now: the first
/// pages of the range that are there decide. A range with nothing there
/// counts as a view (its blocks are noted absent, and looked at again when
/// the runtime maps them).
///
/// Looking at the first address only took such a range for "not a view":
/// its copy was marked not watched for good and copied whole at every
/// refresh, around its unmapped pages (run 54: 300 such ranges, 570 MB
/// copied a frame for them, 255 copies a frame reading around holes).
/// `VICTORY_GUARD_HOLES=0` looks at the first address only, as before.
fn is_view(start: usize, end: usize) -> bool {
    use windows_sys::Win32::System::Memory::{
        MEM_COMMIT, MEM_MAPPED, MEMORY_BASIC_INFORMATION, VirtualQuery,
    };
    static HOLES: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    let holes = *HOLES.get_or_init(|| std::env::var_os("VICTORY_GUARD_HOLES").is_none_or(|v| v != "0"));
    let mut at = start;
    loop {
        // SAFETY: plain data filled in by the query.
        let mut info: MEMORY_BASIC_INFORMATION = unsafe { std::mem::zeroed() };
        // SAFETY: a valid out pointer of the right size.
        let n = unsafe { VirtualQuery(at as *const _, &mut info, size_of_val(&info)) };
        if n == 0 {
            return false;
        }
        if info.Type == MEM_MAPPED {
            return true;
        }
        // Committed memory that is no view (the title's heaps, ours), or
        // the answer as it was before.
        if info.State == MEM_COMMIT || !holes {
            return false;
        }
        // A hole (a placeholder, or free): on to what follows it.
        let next = (info.BaseAddress as usize).saturating_add(info.RegionSize);
        if next <= at || next >= end {
            return true;
        }
        at = next;
    }
}

/// Makes blocks `[from, to)` read-only and notes them guarded and clean;
/// block by block if they span views, leaving out any that fail, and
/// noting those not committed (unmapped) absent. The same physical pages
/// at every other view of them are made read-only too: a write through
/// one of those is this memory written, and only a fault can say so.
/// Another view a kernel write is under way into ([`writing_held`]) is
/// left writable, and the guarded block is taken as written again when
/// that write ends. `VICTORY_VIEW_GUARD=0` leaves the other views as they
/// are, as before.
fn protect(g: &mut Guards, from: usize, to: usize) {
    #[cfg(feature = "diagnostics")]
    let began = std::time::Instant::now();
    protect_here(g, from, to);
    #[cfg(feature = "diagnostics")]
    let aliasing = std::time::Instant::now();
    #[cfg(feature = "diagnostics")]
    {
        PROTECT_PARTS[0].fetch_add((aliasing - began).as_nanos() as u64, std::sync::atomic::Ordering::Relaxed);
        PROTECT_PARTS[1].fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
    if !view_guard() {
        return;
    }
    #[cfg(feature = "diagnostics")]
    struct Timed(std::time::Instant);
    #[cfg(feature = "diagnostics")]
    impl Drop for Timed {
        fn drop(&mut self) {
            PROTECT_PARTS[2].fetch_add(self.0.elapsed().as_nanos() as u64, std::sync::atomic::Ordering::Relaxed);
        }
    }
    #[cfg(feature = "diagnostics")]
    let _timed = Timed(aliasing);
    for (base, bytes) in crate::inplace::views_between(from.saturating_mul(BLOCK), to.saturating_mul(BLOCK)) {
        let first = (base / BLOCK).max(from);
        let last = (base.saturating_add(bytes).div_ceil(BLOCK)).min(to);
        for block in first..last {
            let shown = crate::inplace::pages_of(block * BLOCK);
            // One view (nothing else to guard), or the page the title shows
            // wherever it has nothing (thousands of views): each view of
            // that one is guarded by itself.
            if shown.len() < 2 {
                continue;
            }
            if shown.len() > MANY_VIEWS {
                // Not guarded together: never armed.
                g.exposed.insert(block);
                continue;
            }
            #[cfg(feature = "diagnostics")]
            ALIASES[0].fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            for alias in shown {
                let alias = alias / BLOCK;
                g.aliased.insert(alias);
                if alias == block {
                    continue;
                }
                if g.held.iter().any(|&(from, to, _)| (from..to).contains(&alias)) {
                    // The kernel is writing there now, and a protection
                    // change would fail that write.
                    if !g.deferred.contains(&(block, alias)) {
                        g.deferred.push((block, alias));
                    }
                    g.exposed.insert(block);
                    continue;
                }
                if g.writing_into(alias) {
                    // An asynchronous write may land there: left open, and
                    // what the guarded block shows is taken as written (the
                    // next check guards both once the pin is over).
                    g.clean.remove(block);
                    g.exposed.insert(block);
                    changed(block, block + 1);
                    continue;
                }
                if !protect_alias(g, alias) {
                    g.exposed.insert(block);
                }
            }
        }
    }
}

/// Views of one page past which its views are not guarded together.
pub(crate) const MANY_VIEWS: usize = 64;

/// Whether all the views of a guarded page are guarded together
/// (`VICTORY_VIEW_GUARD`).
fn view_guard() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("VICTORY_VIEW_GUARD").is_none_or(|v| v != "0"))
}

/// Diagnostics: blocks guarded together with their other views, blocks
/// opened because another view of theirs was written, and blocks opened
/// because a view of theirs was mapped; since the start.
#[cfg(feature = "diagnostics")]
pub(crate) static ALIASES: [std::sync::atomic::AtomicU64; 3] = [const { std::sync::atomic::AtomicU64::new(0) }; 3];

/// Makes `block` read-only: another view of pages a copy has just taken
/// through its own view, so that a write through this one faults too. It is
/// not noted clean. The copies of title memory at this view have not taken
/// the pages: noted clean, the block would not be reported written at their
/// next check and they would keep the bytes from before the write (`guard`
/// reports a block that is read-only and not clean as written, then notes it
/// clean). A block that is clean stays so: had the pages been written since
/// its copies took them, `open` would have opened it.
fn protect_alias(g: &mut Guards, block: usize) -> bool {
    use windows_sys::Win32::System::Memory::{PAGE_READONLY, VirtualProtect};
    if g.protected.contains(&block) {
        return true;
    }
    let mut old = 0;
    #[cfg(feature = "diagnostics")]
    PROTECT_PARTS[3].fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    // SAFETY: title view memory; only its protection changes.
    if unsafe { VirtualProtect((block * BLOCK) as *const _, BLOCK, PAGE_READONLY, &mut old) } != 0 {
        g.protected.insert(block);
        if view_known() {
            g.viewed.insert(block);
        }
        return true;
    }
    false
}

/// [`protect`] for one range, without the other views of its pages.
fn protect_here(g: &mut Guards, from: usize, to: usize) {
    use windows_sys::Win32::System::Memory::{PAGE_READONLY, VirtualProtect};
    let set = |from: usize, to: usize| {
        let mut old = 0;
        // SAFETY: title view memory; only its protection changes.
        unsafe {
            VirtualProtect(
                (from * BLOCK) as *const _,
                (to - from) * BLOCK,
                PAGE_READONLY,
                &mut old,
            ) != 0
        }
    };
    let note = |g: &mut Guards, from: usize, to: usize| {
        g.protected.extend(from..to);
        g.clean.extend(from..to);
        note_clean(from, to, true);
        if view_known() {
            g.viewed.extend(from..to);
        }
    };
    if set(from, to) {
        note(g, from, to);
        return;
    }
    // Block by block; a block that is not committed answers for the whole
    // uncommitted run it begins (one question, no protection asked for the
    // rest: 77 runs a frame were refused whole in the world, with 10,264
    // questions to Windows and 1,120 protections a frame, run int-i-1).
    let mut block = from;
    while block < to {
        if set(block, block + 1) {
            note(g, block, block + 1);
            block += 1;
            continue;
        }
        #[cfg(feature = "diagnostics")]
        let error = std::io::Error::last_os_error();
        let end = uncommitted_end(block).min(to);
        if end > block {
            g.absent.extend(block..end);
            if holes_level() == 2 {
                // Nothing to copy there until it is told mapped or
                // committed: the asking's first look takes it as clean.
                g.clean.extend(block..end);
            }
            block = end;
        } else {
            // Diagnostics: a committed block that will not be made
            // read-only is reported written at every asking and never
            // known to the guards (one in 1,024 said).
            #[cfg(feature = "diagnostics")]
            {
                static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
                if N.fetch_add(1, std::sync::atomic::Ordering::Relaxed).is_multiple_of(1024) {
                    tracing::info!("a block of title memory could not be made read-only (one in 1024): {:#x}: {error}", block * BLOCK);
                }
            }
            block += 1;
        }
    }
}

/// The end (exclusive) of the whole uncommitted blocks starting at `block`.
fn uncommitted_end(block: usize) -> usize {
    use windows_sys::Win32::System::Memory::{
        MEM_FREE, MEM_RESERVE, MEMORY_BASIC_INFORMATION, VirtualQuery,
    };
    // SAFETY: plain data filled in by the query.
    let mut info: MEMORY_BASIC_INFORMATION = unsafe { std::mem::zeroed() };
    // SAFETY: a valid out pointer of the right size.
    let n = unsafe { VirtualQuery((block * BLOCK) as *const _, &mut info, size_of_val(&info)) };
    if n != 0 && matches!(info.State, MEM_RESERVE | MEM_FREE) {
        (info.BaseAddress as usize).saturating_add(info.RegionSize) / BLOCK
    } else {
        block
    }
}

/// How long what Windows said of a run of uncommitted blocks holds
/// (`VICTORY_GUARD_HOLES_FRAME`): 0 (unset) every asking questions Windows
/// again; 1 for the rest of the frame; 2 until the runtime tells of a
/// mapping or a commit there (after victory-fork pull request #987).
///
/// The title's streamed buffers lie over placeholders with holes, and a
/// submission's askings walk them: 10,264 questions a frame in the world,
/// 7.5 ms of the submitting thread (run int-i-1), nearly all with the
/// answer of the asking before.
///
/// shortcut (1): a block the title commits by an ordinary commit (no word
/// from the runtime) after the frame's first question about it is copied
/// as zeros until the next frame begins.
///
/// shortcut (2): a hole found holds, with no question and no walk of its
/// blocks (they count as clean), until the runtime tells of a mapping
/// ([`title_memory_remapped`]) or of a commit ([`title_pages_changed`]: the
/// title's `VirtualAlloc` and `XMemVirtualAlloc`, `XMemAllocDefault`, and
/// our own allocations for it), or until it is due a look again
/// ([`holes_due`]). Memory committed in a way the runtime does not tell of
/// is copied as zeros until then, and is logged when the look finds it.
fn holes_level() -> u8 {
    static LEVEL: std::sync::OnceLock<u8> = std::sync::OnceLock::new();
    *LEVEL.get_or_init(|| {
        std::env::var("VICTORY_GUARD_HOLES_FRAME")
            .ok()
            .and_then(|v| v.trim().parse().ok())
            .filter(|&v| v <= 2)
            .unwrap_or(0)
    })
}

/// Ranges told by [`title_pages_changed`] and not applied yet: the next
/// asking takes them, so the title's threads never wait for the guards.
static CHANGED: Mutex<Vec<(usize, usize)>> = Mutex::new(Vec::new());
static CHANGED_ANY: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Whether the runtime should tell of ordinary commits ([`holes_level`]
/// 2).
pub fn title_pages_told() -> bool {
    holes_level() == 2
}

/// The title's memory `[at, at + len)` was committed by an ordinary call
/// (no mapping changed): with [`holes_level`] 2, the holes noted there are
/// looked at again. Nothing to do otherwise.
pub fn title_pages_changed(at: usize, len: usize) {
    if !title_pages_told() || len == 0 {
        return;
    }
    CHANGED.lock().unwrap_or_else(|e| e.into_inner()).push((at, len));
    CHANGED_ANY.store(true, std::sync::atomic::Ordering::Release);
    // The regions' counts move now (no lock): a refresh that passes over a
    // range noted all clean (`crate::shadow`'s clean spans) asks the guards
    // again, and that asking applies what was told.
    changed(at / BLOCK, at.saturating_add(len).div_ceil(BLOCK));
}

/// Applies what [`title_pages_changed`] was told since the last asking.
fn apply_changed(g: &mut Guards) {
    if !CHANGED_ANY.swap(false, std::sync::atomic::Ordering::AcqRel) {
        return;
    }
    let told = std::mem::take(&mut *CHANGED.lock().unwrap_or_else(|e| e.into_inner()));
    for (at, len) in told {
        let (first, last) = (at / BLOCK, at.saturating_add(len).div_ceil(BLOCK));
        let absent: Vec<usize> = g.absent.range(first..last).collect();
        for &block in &absent {
            g.absent.remove(block);
            g.clean.remove(block);
        }
        if let (Some(&from), Some(&to)) = (absent.first(), absent.last()) {
            g.holes.clear();
            changed(from, to + 1);
        }
        #[cfg(feature = "diagnostics")]
        crate::copystat::add(crate::copystat::C::HolesTold, absent.len() as u64);
    }
}

/// With [`holes_level`] 2: every how many frames a hole is looked at again
/// (`VICTORY_GUARD_HOLES_RECHECK`, 16 unset; 0 never).
fn holes_recheck() -> usize {
    static EVERY: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *EVERY.get_or_init(|| {
        std::env::var("VICTORY_GUARD_HOLES_RECHECK")
            .ok()
            .and_then(|v| v.trim().parse().ok())
            .unwrap_or(16)
    })
}

/// With [`holes_level`] 2, once a frame: a share of the holes (their words
/// of blocks by number, one share in [`holes_recheck`] each frame) is no
/// longer clean, so the askings of this frame and later question Windows
/// about them again, each once.
fn holes_due(g: &mut Guards) {
    if holes_level() != 2 {
        return;
    }
    let every = holes_recheck();
    let frame = crate::queue::presented();
    if every == 0 || g.due_frame == frame {
        return;
    }
    g.due_frame = frame;
    let turn = frame as usize % every;
    let Guards { absent, clean, .. } = g;
    let mut due = Vec::new();
    for (&word, &bits) in &absent.0 {
        if word % every == turn {
            clean.clear(word, bits);
            due.push(word);
        }
    }
    // The regions' counts move, or a range noted all clean
    // (`crate::shadow`'s clean spans) would pass over the hole for good.
    for word in due {
        changed(word * 64, word * 64 + 64);
    }
}

/// A hole that was due a look is committed or mapped, and the runtime had
/// not told ([`holes_level`] 2): counted, the first ones named with what
/// Windows says of them (which way of committing the runtime misses).
fn untold(block: usize) {
    use windows_sys::Win32::System::Memory::{MEMORY_BASIC_INFORMATION, VirtualQuery};
    static SEEN: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let seen = SEEN.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
    #[cfg(feature = "diagnostics")]
    crate::copystat::add(crate::copystat::C::HolesUntold, 1);
    if seen > 48 && !seen.is_power_of_two() {
        return;
    }
    // SAFETY: plain data filled in by the query.
    let mut info: MEMORY_BASIC_INFORMATION = unsafe { std::mem::zeroed() };
    // SAFETY: a valid out pointer of the right size.
    unsafe { VirtualQuery((block * BLOCK) as *const _, &mut info, size_of_val(&info)) };
    tracing::warn!(
        address = format_args!("{:#x}", block * BLOCK),
        state = format_args!("{:#x}", info.State),
        kind = format_args!("{:#x}", info.Type),
        allocation = format_args!("{:p}", info.AllocationBase),
        region = info.RegionSize,
        seen,
        "a hole was committed or mapped with no word from the runtime"
    );
}

/// [`uncommitted_end`] of an absent `block`, from this frame's answers
/// with [`holes_level`] 1 and 2.
fn hole_end(g: &mut Guards, block: usize) -> usize {
    if holes_level() == 0 {
        return uncommitted_end(block);
    }
    let frame = crate::queue::presented();
    if g.holes_frame != frame {
        g.holes.clear();
        g.holes_frame = frame;
    }
    if let Some((_, &end)) = g.holes.range(..=block).next_back()
        && block < end
    {
        return end;
    }
    let end = uncommitted_end(block);
    if end > block {
        g.holes.insert(block, end);
    }
    end
}

/// Gives write access back to the guarded blocks of `[start, end)` and
/// notes them written; true if any was guarded. The same physical pages at
/// every other view of them are opened too: the write that faulted (or is
/// ours, `crate::host::copy_mapped`) goes through one view, and every
/// guard over the pages must copy them again. `before`: the write is still
/// to come (a fault, a kernel write held open), so the bytes of armed
/// blocks are kept first.
fn open(g: &mut Guards, start: usize, end: usize, before: bool) -> bool {
    open_as(g, start, end, before, None)
}

/// [`open`]; `fault`: the instruction of a store that faulted there (the
/// bake probe tells a store from a write of the runtime by it).
fn open_as(g: &mut Guards, start: usize, end: usize, before: bool, fault: Option<usize>) -> bool {
    let blocks = open_here(g, start, end, before);
    if start < end && !g.aliased.is_empty() {
        let aliased: Vec<usize> = g.aliased.range(start / BLOCK..end.div_ceil(BLOCK)).copied().collect();
        for block in aliased {
            g.aliased.remove(&block);
            for alias in crate::inplace::pages_of(block * BLOCK) {
                if alias / BLOCK != block && !open_here(g, alias, alias.saturating_add(BLOCK), before).is_empty() {
                    #[cfg(feature = "diagnostics")]
                    ALIASES[1].fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                }
                g.aliased.remove(&(alias / BLOCK));
            }
        }
    }
    #[cfg(feature = "diagnostics")]
    let probed = {
        let taken = probe_take(start, end);
        for &block in &taken {
            if blocks.binary_search(&block).is_err() {
                make_writable(block);
            }
            changed(block, block + 1);
        }
        if !taken.is_empty() {
            let kind = match fault {
                Some(_) => crate::bake_probe::Writer::ProbeFault,
                None => crate::bake_probe::Writer::RuntimeIntoGuarded,
            };
            crate::bake_probe::note(kind, start, end - start, fault.unwrap_or(0) as u64);
        } else if let Some(pc) = fault
            && !blocks.is_empty()
        {
            crate::bake_probe::note(crate::bake_probe::Writer::GuardFault, start, end - start, pc as u64);
        }
        !taken.is_empty()
    };
    #[cfg(not(feature = "diagnostics"))]
    let probed = {
        let _ = fault;
        false
    };
    !blocks.is_empty() || probed
}

/// [`open`] for one range, without the other views of its pages: the
/// blocks it gave write access back to, in order.
fn open_here(g: &mut Guards, start: usize, end: usize, before: bool) -> Vec<usize> {
    // Before anything is made writable: no other thread can write them.
    touched_range(g, start / BLOCK, end.div_ceil(BLOCK), before);
    let blocks: Vec<usize> = g.protected.range(start / BLOCK..end.div_ceil(BLOCK)).copied().collect();
    for &block in &blocks {
        g.protected.remove(&block);
        g.clean.remove(block);
        g.exposed.remove(&block);
    }
    make_writable_runs(&blocks);
    if let (Some(&from), Some(&to)) = (blocks.first(), blocks.last()) {
        changed(from, to + 1);
    }
    blocks
}

// Blocks the bake probe made read-only, each with the bake whose sources
/// it guards ([`probe_guard`]).
#[cfg(feature = "diagnostics")]
static PROBED: Mutex<BTreeMap<usize, u64>> = Mutex::new(BTreeMap::new());

/// Takes the probe's blocks of `[start, end)` off its list (their
/// protection is the caller's to give back).
#[cfg(feature = "diagnostics")]
fn probe_take(start: usize, end: usize) -> Vec<usize> {
    let mut probed = PROBED.lock().unwrap_or_else(|e| e.into_inner());
    if probed.is_empty() {
        return Vec::new();
    }
    let taken: Vec<usize> = probed.range(start / BLOCK..end.div_ceil(BLOCK)).map(|(&b, _)| b).collect();
    for b in &taken {
        probed.remove(b);
    }
    taken
}

/// The bake probe: makes the blocks of `ranges` that are read-write view
/// memory read-only, for bake `bake`, so that the first store to each
/// faults once and is noted ([`open_as`]); the runtime's own writes there
/// open them as they open the copies' guards. Blocks a kernel write is
/// under way into, and blocks the copies guard already (their faults are
/// noted too), are left alone. Returns the blocks made read-only.
#[cfg(feature = "diagnostics")]
pub(crate) fn probe_guard(ranges: &[(usize, usize)], bake: u64) -> usize {
    use windows_sys::Win32::System::Memory::{PAGE_READONLY, PAGE_READWRITE, VirtualProtect};
    install();
    let mut g = guards();
    let mut probed = PROBED.lock().unwrap_or_else(|e| e.into_inner());
    let mut made = 0;
    for &(start, end) in ranges {
        for block in start / BLOCK..end.div_ceil(BLOCK) {
            if probed.contains_key(&block)
                || g.protected.contains(&block)
                || g.writing_into(block)
            {
                continue;
            }
            let mut old = 0;
            // SAFETY: title view memory; only its protection changes.
            if unsafe { VirtualProtect((block * BLOCK) as *const _, BLOCK, PAGE_READONLY, &mut old) } == 0 {
                continue;
            }
            if old != PAGE_READWRITE {
                // Not plain read-write memory: left as it was.
                let mut back = 0;
                // SAFETY: as above, back to the protection it had.
                unsafe { VirtualProtect((block * BLOCK) as *const _, BLOCK, old, &mut back) };
                continue;
            }
            probed.insert(block, bake);
            made += 1;
        }
    }
    made
}

/// The bake probe watched bake `bake`'s sources long enough: the blocks it
/// still guards for it get write access back (unless the copies guard
/// them now).
#[cfg(feature = "diagnostics")]
pub(crate) fn probe_release(bake: u64) {
    let g = guards();
    let mut probed = PROBED.lock().unwrap_or_else(|e| e.into_inner());
    let mine: Vec<usize> = probed.iter().filter(|&(_, &b)| b == bake).map(|(&block, _)| block).collect();
    for block in mine {
        probed.remove(&block);
        if !g.protected.contains(&block) {
            make_writable(block);
        }
    }
}

/// Gives write access back to `block`, one we made read-only; nothing
/// changes where it is not mapped.
fn make_writable(block: usize) {
    make_writable_span(block, block + 1);
}

/// Gives write access back to blocks `[from, to)` in one call; false if
/// Windows refused (the blocks are not all of one view, or not mapped).
fn make_writable_span(from: usize, to: usize) -> bool {
    use windows_sys::Win32::System::Memory::{PAGE_READWRITE, VirtualProtect};
    let mut old = 0;
    // SAFETY: title view memory; only its protection changes.
    unsafe {
        VirtualProtect(
            (from * BLOCK) as *const _,
            (to - from) * BLOCK,
            PAGE_READWRITE,
            &mut old,
        ) != 0
    }
}

/// [`make_writable`] for `blocks` (in order): a run of neighbours is one
/// call, block by block only where Windows refuses the run (it spans two
/// views; it never changes part of a range). A remapping or a write the
/// runtime announces opens hundreds of neighbouring blocks, each a call
/// into the kernel and a flush of the other cores' address caches.
/// `VICTORY_GUARD_RUNS=0` opens block by block, as before.
fn make_writable_runs(blocks: &[usize]) {
    static RUNS: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    let runs = *RUNS.get_or_init(|| std::env::var_os("VICTORY_GUARD_RUNS").is_none_or(|v| v != "0"));
    let mut i = 0;
    while i < blocks.len() {
        let mut k = i + 1;
        while runs && k < blocks.len() && blocks[k] == blocks[k - 1] + 1 {
            k += 1;
        }
        if k - i == 1 || !make_writable_span(blocks[i], blocks[k - 1] + 1) {
            for &block in &blocks[i..k] {
                make_writable(block);
            }
        }
        i = k;
    }
}

/// Our own write into title memory `[at, at + len)` that the handler does
/// not see: opens its guarded blocks, and the same pages at their other
/// views. For a write already done when it is told; a kernel write that
/// is still to come takes [`writing_held`] (the runtime's
/// `kernel_will_write`), as a guard taken before it ends would fail it.
#[cfg(test)]
pub(crate) fn writing(at: usize, len: usize) {
    open(&mut guards(), at, at.saturating_add(len), false);
    // A placed texture there changes with no descriptor written.
    crate::descriptor::host::remapped(at, len);
}

/// A kernel write into title memory that is under way ([`writing_held`]).
#[must_use = "the write is held only while this lives"]
pub struct Writing {
    id: u64,
}

/// `writing` for a write that runs on after the call, perhaps on another
/// thread: until what it returns is dropped, the blocks of `[at, at + len)`
/// are not guarded again, through a texture's other view either. The end of
/// the write takes the textures that show those pages as written.
pub fn writing_held(at: usize, len: usize) -> Writing {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let id = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    // The length may be the title's (`fread`'s size times count, saturated).
    let end = at.saturating_add(len);
    {
        let mut g = guards();
        // Held first: the opening below must not be linked again between.
        g.held.push((at / BLOCK, end.div_ceil(BLOCK), id));
        open(&mut g, at, end, true);
    }
    crate::descriptor::host::remapped(at, len);
    Writing { id }
}

/// How long blocks stay open for an asynchronous kernel write
/// ([`writing_async`]): `VICTORY_ASYNC_WRITE_MS` (2000; 0: only until the
/// call returns, as before).
fn async_pin() -> Option<std::time::Duration> {
    static MS: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
    let ms = *MS.get_or_init(|| {
        std::env::var("VICTORY_ASYNC_WRITE_MS").ok().and_then(|v| v.trim().parse().ok()).unwrap_or(2000)
    });
    (ms > 0).then(|| std::time::Duration::from_millis(ms))
}

/// [`writing_held`] for a kernel write that may land after the call that
/// asked for it returns (an overlapped read): the blocks of `[at, at +
/// len)` and of the other views of their pages stay open, and count as
/// written at every check, for a while ([`async_pin`]). Held only until
/// the call returned, a block guarded again meanwhile failed the read
/// when it landed.
pub fn writing_async(at: usize, len: usize) {
    let Some(pin) = async_pin() else {
        drop(writing_held(at, len));
        return;
    };
    if len == 0 {
        return;
    }
    let end = at.saturating_add(len);
    let until = std::time::Instant::now() + pin;
    {
        let mut g = guards();
        let (first, last) = (at / BLOCK, end.div_ceil(BLOCK));
        g.pins.push((first, last, until));
        let others: Vec<usize> = if view_guard() && g.aliased.range(first..last).next().is_some() {
            (first..last)
                .filter(|block| g.aliased.contains(block))
                .flat_map(|block| crate::inplace::pages_of(block * BLOCK))
                .map(|alias| alias / BLOCK)
                .filter(|alias| !(first..last).contains(alias))
                .collect()
        } else {
            Vec::new()
        };
        open(&mut g, at, end, true);
        // The same pages at their other views: the write lands there too.
        for block in others {
            g.pins.push((block, block + 1, until));
            open(&mut g, block * BLOCK, (block + 1) * BLOCK, true);
        }
    }
    crate::descriptor::host::remapped(at, len);
}

impl Drop for Writing {
    fn drop(&mut self) {
        let mut g = guards();
        g.held.retain(|&(_, _, id)| id != self.id);
        let Guards {
            held,
            deferred,
            clean,
            armed,
            ..
        } = &mut *g;
        deferred.retain(|&(guarded, alias)| {
            let still = held
                .iter()
                .any(|&(from, to, _)| (from..to).contains(&alias));
            if !still {
                // What the guarded block showed may predate the write.
                clean.remove(guarded);
                changed(guarded, guarded + 1);
                if let Some(state) = armed.get_mut(&guarded) {
                    *state = Armed::Lost;
                }
            }
            still
        });
    }
}

/// What `VICTORY_PAGE_WRITES` asks to be done with a write the guards do
/// not see once its bytes are there ([`title_memory_written`]), as bits:
/// 1, the guarded blocks under it are opened and count as written (host
/// copies of buffers and textures read through tables take them again);
/// 2, the placed textures picked by index that lie under it are due again;
/// 4, both also for reads to plain addresses when they have finished.
/// Unset or `guards`: bit 1 alone. `0`: nothing, counted only. `1`: bits
/// 1 and 2. `slots`: bit 2 alone. `all`: everything.
/// (Bit 1 by default: in every run of victory-fork 10,000 to 50,000
/// guarded blocks of host copies lay under such writes and kept the bytes
/// from before. Bit 2 makes texture slots due and flooded the queue in an
/// earlier form: still asked for by name.)
pub(crate) fn page_writes() -> u8 {
    static MODE: std::sync::OnceLock<u8> = std::sync::OnceLock::new();
    *MODE.get_or_init(|| match std::env::var("VICTORY_PAGE_WRITES").as_deref().map(str::trim) {
        Ok("0") => 0,
        Ok("1") => 3,
        Ok("slots") => 2,
        Ok("all") => 7,
        _ => 1,
    })
}

/// Since the start: writes into physical pages through the runtime's own
/// views that landed at addresses the title has mapped (one for each run
/// of addresses); their bytes; guarded blocks that lay under them (host
/// copies that would otherwise keep the bytes from before); reads to
/// plain addresses that finished.
pub(crate) static WRITTEN: [AtomicU64; 4] = [const { AtomicU64::new(0) }; 4];

/// The bytes of a write the guards do not see are there: title memory
/// `[at, at + len)`. `pages`: DirectStorage wrote them into physical pages
/// through a view of the runtime's own, and this is an address where the
/// title has those pages mapped (no title address faulted, nothing was
/// mapped or unmapped). Otherwise a read to a plain address has finished,
/// of which [`writing_held`] was told before it began.
///
/// Upstream's c75fdba does the same for its trackers (its `Writing` marks
/// the pages once more when dropped; page destinations are reported at
/// each address showing them). Here it is counted always and acted on as
/// [`page_writes`] says. Called without the runtime's page-table lock;
/// takes the guards' lock, then, alone, the lock of the ranges kept for
/// the texture pass.
pub fn title_memory_written(at: usize, len: usize, pages: bool) {
    use std::sync::atomic::Ordering::Relaxed;
    if len == 0 {
        return;
    }
    let mode = page_writes();
    let end = at.saturating_add(len);
    if pages {
        WRITTEN[0].fetch_add(1, Relaxed);
        WRITTEN[1].fetch_add(len as u64, Relaxed);
    } else {
        WRITTEN[3].fetch_add(1, Relaxed);
        if mode & 4 == 0 {
            return;
        }
    }
    {
        let mut g = guards();
        let guarded = g.protected.range(at / BLOCK..end.div_ceil(BLOCK)).count() as u64;
        if pages {
            WRITTEN[2].fetch_add(guarded, Relaxed);
        }
        if mode & 1 != 0 && guarded != 0 {
            // Told when done: the bytes from before are gone.
            open(&mut g, at, end, false);
        } else {
            touched_range(&mut g, at / BLOCK, end.div_ceil(BLOCK), false);
        }
    }
    // A placed texture there changed with no descriptor written.
    crate::descriptor::host::page_written(at, len);
}

/// The host GPU wrote title memory `[at, at + len)` itself, through a
/// window placed over it (`crate::unified`), and the submission is done: no
/// guard saw the write (over physical pages it went through a view of
/// ours), so the guarded blocks there and at the other views of the same
/// pages are opened and noted written (every other copy of them takes them
/// again), and the placed textures there are told.
pub(crate) fn gpu_wrote(at: usize, len: usize) {
    if len == 0 {
        return;
    }
    {
        let mut g = guards();
        open(&mut g, at, at.saturating_add(len));
    }
    crate::descriptor::host::page_written(at, len);
}

/// The runtime mapped a view of the physical bytes `[offset, offset + len)`
/// of `section` (`crate::inplace::title_memory_view`): the view is
/// writable, so the same bytes guarded at another view must be checked
/// again — and are, the pages noted written, which also has the next check
/// guard the new view (`protect`).
pub(crate) fn views_mapped(section: usize, offset: u64, len: usize) {
    if !view_guard() {
        return;
    }
    let ranges = crate::inplace::views_of_section(section, offset, len);
    if ranges.is_empty() {
        return;
    }
    let mut g = guards();
    for &(at, n) in &ranges {
        if !open_here(&mut g, at, at.saturating_add(n), true).is_empty() {
            #[cfg(feature = "diagnostics")]
            ALIASES[2].fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
    }
}

/// A direct read pins the virtual addresses it will read. A mapping change
/// announces the whole extent it can temporarily remove before waiting for
/// its readers, so new reads cannot starve it. Disjoint reads can proceed.
///
/// The runtime does not change a mapping in one step. To unmap one page of
/// a view it unmaps the whole view and maps what stays again
/// (`xmem.rs` `unmap`), and a view it maps is a reservation until the next
/// call commits it (`map`, then `commit`). For that moment pages the title
/// never gave up are "reserved, never committed", and a thread reading
/// them in place takes an access violation: run 71 in `Layout::detile`,
/// run 74 in the hash before it, both under `Staging::refresh_now`, which
/// had asked whether the texture was all mapped and been told yes. Asking
/// cannot help; not changing the mapping while such a read is under way
/// does.
// A missing range is global, including overflow and unknown extents.
type MappingRange = Option<(usize, usize)>;
fn mapping_range(at: usize, len: usize) -> MappingRange {
    if len == 0 { return None; }
    at.checked_add(len).map(|end| (at, end))
}
fn mapping_overlaps(a: MappingRange, b: MappingRange) -> bool {
    match (a, b) {
        (Some((a, b)), Some((c, d))) => a < d && c < b,
        _ => true,
    }
}
#[derive(Default)]
struct MappingState {
    next: u64,
    readers: Vec<(u64, MappingRange)>,
    // Pending and active writers share one ordered queue. New readers wait
    // behind an overlapping queued writer to prevent writer starvation.
    writers: Vec<(u64, MappingRange, bool)>,
}
static MAPPING: std::sync::Mutex<MappingState> = std::sync::Mutex::new(MappingState {
    next: 0, readers: Vec::new(), writers: Vec::new(),
});
static MAPPING_MOVED: std::sync::Condvar = std::sync::Condvar::new();
fn mapping_state() -> std::sync::MutexGuard<'static, MappingState> {
    MAPPING.lock().unwrap_or_else(|e| e.into_inner())
}
fn mapping_id(state: &mut MappingState) -> u64 {
    let id = state.next;
    state.next = id.checked_add(1).expect("mapping guard identifiers exhausted");
    id
}

/// Guard for a mapping change. Dropping it wakes overlapping waiters.
pub struct Remapping(u64, std::time::Instant);
impl Drop for Remapping {
    fn drop(&mut self) {
        let mut state = mapping_state();
        state.writers.retain(|writer| writer.0 != self.0);
        MAPPING_MOVED.notify_all();
        // From asking for the lock until it is let go (the frame log).
        crate::framelog::add(crate::framelog::T::RemapUs, self.1.elapsed().as_micros() as u64);
    }
}
fn mapping_write(range: MappingRange) -> Remapping {
    let asked = std::time::Instant::now();
    let mut state = mapping_state();
    let id = mapping_id(&mut state);
    state.writers.push((id, range, false));
    loop {
        let busy_readers = state.readers.iter().any(|&(_, other)| mapping_overlaps(range, other));
        let busy_writers = state.writers.iter().any(|&(other_id, other, active)| {
            other_id != id && (active || other_id < id) && mapping_overlaps(range, other)
        });
        if !busy_readers && !busy_writers { break; }
        state = MAPPING_MOVED.wait(state).unwrap_or_else(|e| e.into_inner());
    }
    state.writers.iter_mut().find(|writer| writer.0 == id).unwrap().2 = true;
    crate::framelog::add(crate::framelog::T::Remaps, 1);
    crate::framelog::add(crate::framelog::T::RemapWaitUs, asked.elapsed().as_micros() as u64);
    Remapping(id, asked)
}
/// Excludes all in-place readers during a mapping change of unknown extent.
pub fn title_memory_remapping() -> Remapping { mapping_write(None) }
/// Excludes readers overlapping the full range affected by a mapping change.
pub fn title_memory_remapping_range(at: usize, len: usize) -> Remapping {
    mapping_write(mapping_range(at, len))
}

/// Guard for an in-place title-memory read.
pub struct Reading(u64);
impl Drop for Reading {
    fn drop(&mut self) {
        let mut state = mapping_state();
        state.readers.retain(|reader| reader.0 != self.0);
        if !state.writers.is_empty() {
            MAPPING_MOVED.notify_all();
        }
    }
}
fn mapping_read(range: MappingRange, wait: bool) -> Option<Reading> {
    let mut state = if wait {
        mapping_state()
    } else {
        match MAPPING.try_lock() {
            Ok(state) => state,
            Err(std::sync::TryLockError::Poisoned(e)) => e.into_inner(),
            Err(std::sync::TryLockError::WouldBlock) => return None,
        }
    };
    while state.writers.iter().any(|&(_, other, _)| mapping_overlaps(range, other)) {
        if !wait { return None; }
        state = MAPPING_MOVED.wait(state).unwrap_or_else(|e| e.into_inner());
    }
    let id = mapping_id(&mut state);
    state.readers.push((id, range));
    Some(Reading(id))
}
/// Global read, for callers whose read extent is not known. Do not acquire
/// a second read or map title memory while a read guard is held.
pub fn title_memory_reading() -> Reading { mapping_read(None, true).unwrap() }
/// Nonblocking global read.
pub fn title_memory_reading_now() -> Option<Reading> { mapping_read(None, false) }
/// Read only the given range; overflow falls back to global exclusion.
pub fn title_memory_reading_range(at: usize, len: usize) -> Reading {
    mapping_read(mapping_range(at, len), true).unwrap()
}
/// Nonblocking version of the range read.
pub fn title_memory_reading_range_now(at: usize, len: usize) -> Option<Reading> {
    mapping_read(mapping_range(at, len), false)
}
#[cfg(test)]
fn mapping_writer_pending_for_test(at: usize, len: usize) -> bool {
    let range = mapping_range(at, len);
    mapping_state().writers.iter().any(|&(_, other, active)| !active && mapping_overlaps(range, other))
}
/// The runtime mapped or unmapped title memory `[at, at + len)`: what it
/// shows is new and read-write, so it is no longer guarded (nor known
/// unmapped).
pub fn title_memory_remapped(at: usize, len: usize) {
    // The bake probe: the mapping is new (read-write); its blocks are no
    // longer guarded by the probe.
    #[cfg(feature = "diagnostics")]
    {
        crate::bake_probe::note(crate::bake_probe::Writer::Remap, at, len, 0);
        probe_take(at, at.saturating_add(len));
    }
    // A placed texture there arrives, goes or changes with no descriptor
    // written.
    crate::descriptor::host::remapped(at, len);
    // Textures already uploaded from there must be uploaded again, or the
    // next list that samples them reads their old contents.
    crate::texture::remapped(at, len);
    // The views the runtime told us of there are no longer what it shows.
    crate::inplace::views_gone(at, len);
    // And nothing stays in place over it.
    crate::inplace::title_memory_freeing(at, len);
    let mut g = guards();
    g.holes.clear();
    let range = at / BLOCK..at.saturating_add(len).div_ceil(BLOCK);
    // What it showed is gone: armed blocks there are unknown.
    touched_range(&mut g, range.start, range.end, false);
    // A view mapped there now is another: no write pending into the old
    // one keeps it open.
    g.pins.retain(|&(from, to, _)| to <= range.start || from >= range.end);
    let exposed: Vec<usize> = g.exposed.range(range.clone()).copied().collect();
    for block in exposed {
        g.exposed.remove(&block);
    }
    let absent: Vec<usize> = g.absent.range(range.clone()).collect();
    for block in absent {
        g.absent.remove(block);
        g.clean.remove(block);
    }
    // A copy on another thread may have guarded a new view between its
    // mapping and this call: given write access back, or the title's next
    // write there would fault with nothing here to open (where nothing is
    // mapped any more the call changes nothing).
    let blocks: Vec<usize> = g.protected.range(range.clone()).copied().collect();
    for &block in &blocks {
        g.protected.remove(&block);
        g.clean.remove(block);
    }
    make_writable_runs(&blocks);
    // Only guarded blocks were clean: a range noted all clean is all
    // guarded, so a remapping elsewhere changes nothing it holds.
    if let (Some(&from), Some(&to)) = (blocks.first(), blocks.last()) {
        changed(from, to + 1);
    }
    let aliased: Vec<usize> = g.aliased.range(range.clone()).copied().collect();
    for block in aliased {
        g.aliased.remove(&block);
    }
    if view_known() {
        // The runtime calls this for two things only (`xmem`: `map` and
        // `unmap`): a range it has just mapped a section's pages into,
        // and a view it is about to make a placeholder again. Either way
        // what stands there is view memory or a hole in it, which is what
        // `is_view` would answer for a range that begins there; and the
        // write watch's asking can only fail there. The title remaps some
        // 83 times a frame, and each time the blocks were forgotten, so
        // that the next asking about each range went to the kernel twice
        // (run 235: 266 to 565 failed askings a frame with the blocks
        // remembered from their guards alone, no fewer than without).
        g.viewed.extend(at / BLOCK..at.saturating_add(len).div_ceil(BLOCK));
        return;
    }
    // What the range shows is new: no longer known to be a view.
    let viewed: Vec<usize> = g.viewed.range(range).copied().collect();
    for block in viewed {
        g.viewed.remove(&block);
    }
}

/// Handles a write access violation at `at` if the block belongs to our guards
/// or became writable concurrently.
pub(crate) fn handle_write_fault(at: usize, instruction: usize) -> bool {
    let mut g = guards();
    if open_as(&mut g, at, at + 1, true, Some(instruction)) {
        #[cfg(feature = "diagnostics")]
        crate::copystat::add(crate::copystat::C::GuardFaults, 1);
        crate::framelog::add(crate::framelog::T::GuardFaults, 1);
        return true;
    }
    if writable(at) {
        return true;
    }
    // Read-only, in a view of the title's pages we were told of, and no
    // guard of ours: only we make those views read-only (a protection
    // left behind by a race between a mapping and its notice), so the
    // block is given back rather than the title stopped.
    if unknown_open() && !matches!(crate::inplace::cover(at, 1), crate::inplace::Cover::None) && read_only(at) {
        let block = at / BLOCK;
        if make_writable_span(block, block + 1) {
            g.clean.remove(block);
            changed(block, block + 1);
            touched(&mut g, block, false);
            static LOGGED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            if LOGGED.fetch_add(1, Ordering::Relaxed) < 16 {
                tracing::warn!(at = format_args!("{at:#x}"), "a read-only view block no guard knew of was opened on a write to it");
            }
            return true;
        }
    }
    false
}

/// Whether a write fault in a read-only view block that no guard knows of
/// opens the block (`VICTORY_GUARD_UNKNOWN=0`: no, the fault goes on to the
/// crash handler, as before). After another fork's `1c7231d`.
fn unknown_open() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("VICTORY_GUARD_UNKNOWN").is_none_or(|v| v != "0"))
}

/// Whether `at` is committed and read-only now.
fn read_only(at: usize) -> bool {
    use windows_sys::Win32::System::Memory::{MEM_COMMIT, MEMORY_BASIC_INFORMATION, PAGE_READONLY, VirtualQuery};
    // SAFETY: plain data filled in by the query.
    let mut info: MEMORY_BASIC_INFORMATION = unsafe { std::mem::zeroed() };
    // SAFETY: a valid out pointer of the right size.
    let n = unsafe { VirtualQuery(at as *const _, &mut info, size_of_val(&info)) };
    n != 0 && info.State == MEM_COMMIT && info.Protect == PAGE_READONLY
}

/// Registers the handler that takes writes to guarded blocks, once, ahead
/// of the crash reporter's.
fn install() {
    use windows_sys::Win32::Foundation::{EXCEPTION_ACCESS_VIOLATION, NTSTATUS};
    use windows_sys::Win32::System::Diagnostics::Debug::{
        AddVectoredExceptionHandler, EXCEPTION_CONTINUE_EXECUTION, EXCEPTION_CONTINUE_SEARCH,
        EXCEPTION_POINTERS,
    };
    unsafe extern "system" fn on_write(info: *mut EXCEPTION_POINTERS) -> i32 {
        // SAFETY: the system passes the exception being dispatched.
        let r = unsafe { &*(*info).ExceptionRecord };
        const WRITE: usize = 1;
        if r.ExceptionCode != EXCEPTION_ACCESS_VIOLATION as NTSTATUS
            || r.NumberParameters < 2
            || r.ExceptionInformation[0] != WRITE
        {
            return EXCEPTION_CONTINUE_SEARCH;
        }
        let at = r.ExceptionInformation[1];
        #[cfg(feature = "diagnostics")]
        let entered = crate::tick::Tick::now();
        // Held across both checks: another thread may have opened the block
        // since this one faulted, and a copy may have guarded it again since
        // (then it is ours again, not read-only for some other reason).
        if handle_write_fault(at, r.ExceptionAddress as usize) {
            EXCEPTION_CONTINUE_EXECUTION
        } else {
            EXCEPTION_CONTINUE_SEARCH
        }
    }
    static ONCE: std::sync::Once = std::sync::Once::new();
    // SAFETY: the handler only touches our guard state and protections.
    ONCE.call_once(|| unsafe {
        AddVectoredExceptionHandler(1, Some(on_write));
    });
}

/// Guard faults taken by each thread since the last [`faults_by_thread`]:
/// (thread id, faults, nanoseconds in the handler, waiting for the guard
/// lock included). The cost an event tracker by fault (R3) would put on the
/// title's threads lands where these do.
#[cfg(feature = "diagnostics")]
static FAULTS_BY_THREAD: Mutex<Vec<(u32, u64, u64)>> = Mutex::new(Vec::new());

#[cfg(feature = "diagnostics")]
fn note_fault(ns: u64) {
    // SAFETY: a plain query.
    let id = unsafe { windows_sys::Win32::System::Threading::GetCurrentThreadId() };
    let mut all = FAULTS_BY_THREAD.lock().unwrap_or_else(|e| e.into_inner());
    match all.iter_mut().find(|t| t.0 == id) {
        Some(t) => (t.1, t.2) = (t.1 + 1, t.2 + ns),
        None => all.push((id, 1, ns)),
    }
}

/// Prints the guard faults by thread over `frames` frames, most costly
/// first, and starts counting again. The thread ids are the ones of the
/// recording threads' line ("thread N .. ms in .. calls").
#[cfg(feature = "diagnostics")]
pub(crate) fn faults_by_thread(frames: f64) {
    let mut all = std::mem::take(&mut *FAULTS_BY_THREAD.lock().unwrap_or_else(|e| e.into_inner()));
    if all.is_empty() || frames <= 0.0 {
        return;
    }
    all.sort_by(|a, b| b.2.cmp(&a.2));
    let (faults, ns) = all.iter().fold((0, 0), |s, t| (s.0 + t.1, s.1 + t.2));
    let recorders = crate::copystat::recorder_ids();
    let rec: Vec<&(u32, u64, u64)> = all.iter().filter(|t| recorders.contains(&t.0)).collect();
    let (rec_faults, rec_ns) = rec.iter().fold((0, 0), |s, t| (s.0 + t.1, s.1 + t.2));
    let rec_max = rec.iter().map(|t| t.2).max().unwrap_or(0);
    tracing::info!(
        "guard faults per frame, recording threads against the rest: {} recording threads {:.0} faults {:.2} ms \
         (the most on one {:.2} ms); others {:.0} faults {:.2} ms",
        rec.len(),
        rec_faults as f64 / frames,
        rec_ns as f64 / 1e6 / frames,
        rec_max as f64 / 1e6 / frames,
        (faults - rec_faults) as f64 / frames,
        (ns - rec_ns) as f64 / 1e6 / frames
    );
    let line: Vec<String> = all
        .iter()
        .take(12)
        .map(|t| {
            format!(
                "thread {}{} {:.0} faults {:.2} ms ({:.1} us each)",
                t.0,
                if recorders.contains(&t.0) { " (records)" } else { "" },
                t.1 as f64 / frames,
                t.2 as f64 / 1e6 / frames,
                t.2 as f64 / 1e3 / t.1 as f64
            )
        })
        .collect();
    tracing::info!(
        "guard faults by thread over {frames:.0} frames, per frame: {:.0} faults {:.2} ms in {} threads; {}",
        faults as f64 / frames,
        ns as f64 / 1e6 / frames,
        all.len(),
        line.join("; ")
    );
}

/// Whether `at` is committed read-write memory now.
fn writable(at: usize) -> bool {
    use windows_sys::Win32::System::Memory::{
        MEM_COMMIT, MEMORY_BASIC_INFORMATION, PAGE_READWRITE, VirtualQuery,
    };
    // SAFETY: plain data filled in by the query.
    let mut info: MEMORY_BASIC_INFORMATION = unsafe { std::mem::zeroed() };
    // SAFETY: a valid out pointer of the right size.
    let n = unsafe { VirtualQuery(at as *const _, &mut info, size_of_val(&info)) };
    n != 0 && info.State == MEM_COMMIT && info.Protect == PAGE_READWRITE
}

#[cfg(test)]
mod tests {
    /// One test at a time where they share the process-wide guard table
    /// (crt4ever 7b516e4: only the lock, for the ported reopen test).
    pub(super) static ONE_AT_A_TIME: std::sync::Mutex<()> = std::sync::Mutex::new(());
    thread_local! {
        /// [`guard_park`] for the test running on this thread only: the
        /// others, running meanwhile, keep asking Windows.
        pub(super) static PARK: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    }
    use super::*;
    use windows_sys::Win32::System::Memory::{
        MEM_COMMIT, MEM_RELEASE, MEM_RESERVE, PAGE_READWRITE, VirtualAlloc, VirtualFree,
    };

    /// The bit set of blocks answers as a set of block numbers would.
    #[test]
    fn blocks_answer_as_a_set_of_blocks() {
        let mut blocks = Blocks::new();
        let mut set = BTreeSet::new();
        let mut seed = 0x9e37_79b9_7f4a_7c15u64;
        let mut next = |n: usize| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed % n as u64) as usize
        };
        for _ in 0..2000 {
            let (a, len) = (next(700), next(150));
            match next(4) {
                0 => {
                    blocks.extend(a..a + len);
                    set.extend(a..a + len);
                }
                1 => {
                    blocks.insert(a);
                    set.insert(a);
                }
                _ => {
                    blocks.remove(a);
                    set.remove(&a);
                }
            }
            let (from, to) = (next(800), next(800));
            let range = from.min(to)..from.max(to);
            assert_eq!(blocks.holds(range.clone()), set.range(range.clone()).count() == range.len());
            assert_eq!(blocks.range(range.clone()).collect::<Vec<_>>(), set.range(range.clone()).copied().collect::<Vec<_>>());
            assert_eq!(blocks.contains(a), set.contains(&a));
        }
        assert!(blocks.holds(5..5));
        assert_eq!(blocks.range(9..9).count(), 0);
    }

    /// Smaller regions say "unchanged" for a range away from a change that
    /// the same 64 MiB region would have held.
    #[test]
    fn a_change_moves_only_the_regions_it_is_in() {
        let shift = region_shift();
        let region = 1usize << shift;
        // Far from anything else the tests touch.
        let base = 0x7a_0000_0000usize;
        let near = guard_changes(base, base + region);
        let far = guard_changes(base + 2 * region, base + 3 * region);
        changed(base / BLOCK, base / BLOCK + 1);
        assert!(guard_changes(base, base + region) > near);
        if shift < 26 {
            assert_eq!(guard_changes(base + 2 * region, base + 3 * region), far);
        }
    }
    const MEM_WRITE_WATCH: u32 = 0x20_0000;

    #[test]
    fn written_pages_are_reported_to_every_asker() {
        // SAFETY: a fresh write-watched allocation, freed at the end.
        let p = unsafe {
            VirtualAlloc(
                std::ptr::null(),
                4 * PAGE,
                MEM_RESERVE | MEM_COMMIT | MEM_WRITE_WATCH,
                PAGE_READWRITE,
            )
        } as usize;
        assert_ne!(p, 0);
        let (start, end) = (p, p + 4 * PAGE);
        let (_, before) = writes(start, end, false).expect("watched");
        assert!(before.is_empty());
        // SAFETY: committed pages of ours.
        unsafe { ((p + PAGE + 8) as *mut u8).write(1) };
        let (now, after) = writes(start, end, false).expect("watched");
        assert_eq!(after, [(p / PAGE + 1, now)]);
        // Another asker after the kernel's bit was cleared still sees it.
        let (later, again) = writes(p + PAGE, p + 2 * PAGE, false).expect("watched");
        assert!(later > now);
        assert_eq!(again, [(p / PAGE + 1, now)]);
        let mut plain = vec![0u8; PAGE];
        assert!(
            writes(
                plain.as_mut_ptr() as usize,
                plain.as_ptr() as usize + PAGE,
                true
            )
            .is_none()
        );
        // SAFETY: our allocation.
        unsafe { VirtualFree(p as *mut _, 0, MEM_RELEASE) };
    }

    /// `VICTORY_GUARD_REST`: a block rests only while written less than the
    /// rest ago, never with a rest of 0, and the book of opened blocks keeps
    /// its size bounded.
    #[test]
    fn a_block_rests_only_just_after_a_write() {
        let mut g = Guards {
            protected: BTreeSet::new(),
            clean: BTreeSet::new(),
            absent: BTreeSet::new(),
            aliased: BTreeSet::new(),
            viewed: BTreeSet::new(),
            arming: BTreeSet::new(),
            disturbed: BTreeSet::new(),
            opened: std::collections::BTreeMap::new(),
        };
        let rest = std::time::Duration::from_millis(50);
        assert!(!g.rests(7, rest), "never written");
        g.note_opened(7);
        assert!(g.rests(7, rest));
        assert!(!g.rests(7, std::time::Duration::ZERO), "no rest");
        assert!(!g.rests(8, rest));
        std::thread::sleep(std::time::Duration::from_millis(60));
        assert!(!g.rests(7, rest), "the rest is over");
        for b in 0..(1usize << 13) + 5 {
            g.note_opened(1000 + b);
        }
        assert!(g.opened.len() <= 1 << 13);
    }

    /// `VICTORY_GUARD_BITS`: the blocks the clean bits do not mark, in
    /// order, across a word's and a chunk's edge (far from any block a run
    /// guards), and none where all are clean.
    #[test]
    fn the_blocks_not_clean_are_read_off_the_bits() {
        let _one = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
        // A chunk edge far above any memory a test maps (2^28 blocks).
        let edge = (1usize << 28) + CHUNK_BLOCKS;
        let (from, to) = (edge - 70, edge + 70);
        note_clean(from, to, true);
        assert!(not_clean(from, to).is_empty());
        for b in [edge - 64, edge - 1, edge, edge + 3, edge + 69] {
            note_clean(b, b + 1, false);
        }
        assert_eq!(not_clean(from, to), [edge - 64, edge - 1, edge, edge + 3, edge + 69]);
        assert_eq!(not_clean(edge, edge + 4), [edge, edge + 3]);
        note_clean(from, to, false);
        assert_eq!(not_clean(edge - 2, edge + 2), [edge - 2, edge - 1, edge, edge + 1]);
        // A chunk no bit was ever set in: none clean.
        let far = (1usize << 28) + 9 * CHUNK_BLOCKS + 11;
        assert_eq!(not_clean(far, far + 3), [far, far + 1, far + 2]);
    }

    /// The clean bits (`CLEAN_BITS`) across a word's and a chunk's edge, far
    /// above any memory a test maps: a range is all clean only when every
    /// one of its bits is set, a cleared bit in the middle is seen, and a
    /// range reaching into a chunk never made is not clean.
    #[test]
    fn clean_bits_follow_their_blocks_across_words_and_chunks() {
        let _one = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
        // A chunk edge, 64 KiB blocks: 2^30 blocks is 64 TiB of addresses.
        let edge = (1usize << 30) + CHUNK_BLOCKS;
        let (from, to) = (edge - 70, edge + 70);
        assert!(!all_clean(from, to), "nothing noted yet");
        note_clean(from, to, true);
        assert!(all_clean(from, to));
        assert!(all_clean(from + 1, to - 1));
        assert!(all_clean(edge - 1, edge + 1), "across the chunk's edge");
        assert!(!all_clean(from - 1, to), "a block before the range is not clean");
        assert!(!all_clean(from, to + 1), "nor one after it");
        assert!(!all_clean(from, from), "an empty range is never clean");
        note_clean(edge + 3, edge + 4, false);
        assert!(!all_clean(from, to), "one block written in the middle");
        assert!(all_clean(from, edge + 3) && all_clean(edge + 4, to), "its neighbours stay clean");
        // Not clean past the last chunk made: 2 chunks on, nothing was noted.
        assert!(!all_clean(edge + 2 * CHUNK_BLOCKS - 1, edge + 2 * CHUNK_BLOCKS + 1));
        note_clean(from, to, false);
        assert!(!all_clean(from, edge - 60) && !all_clean(edge + 60, to));
        // Past the address space: nothing made, never clean.
        assert!(!all_clean(CHUNKS * CHUNK_BLOCKS, CHUNKS * CHUNK_BLOCKS + 2));
    }

    /// A range is quiet when each of its blocks is clean or parked, read off
    /// both tables word by word, where either table may have no chunk.
    #[test]
    fn a_range_of_clean_and_parked_blocks_is_quiet() {
        let _one = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
        let edge = (1usize << 29) + CHUNK_BLOCKS;
        let (from, to) = (edge - 70, edge + 70);
        assert!(!all_quiet(from, to), "nothing noted yet");
        // Parked blocks before the chunk's edge, where nothing is clean.
        note_bits(&PARKED_BITS, from, edge, true);
        assert!(all_quiet(from, edge) && !all_quiet(from, to));
        assert!(parked(from) && parked(edge - 1) && !parked(edge));
        // Clean ones after it, where nothing is parked.
        note_clean(edge, to, true);
        assert!(all_quiet(from, to));
        assert!(!all_quiet(from - 1, to) && !all_quiet(from, to + 1), "the blocks around it are neither");
        assert!(!all_quiet(from, from), "an empty range is never quiet");
        // One block unparked in the middle of a word.
        note_bits(&PARKED_BITS, edge - 5, edge - 4, false);
        assert!(!all_quiet(from, to));
        assert!(all_quiet(from, edge - 5) && all_quiet(edge - 4, to), "its neighbours stay quiet");
        note_bits(&PARKED_BITS, from, edge, false);
        note_clean(edge, to, false);
        assert!(!all_quiet(from, edge - 60) && !all_quiet(edge + 60, to));
        assert!(!all_quiet(CHUNKS * CHUNK_BLOCKS, CHUNKS * CHUNK_BLOCKS + 2), "past the address space");
    }

    /// The clean bits follow the guards of a real view: set for every block
    /// a check guards, cleared for the block a write faults open (before the
    /// write lands), set again when the next check reports it and guards it
    /// again; the lock-free check answers exactly what the locked one does.
    #[test]
    fn clean_bits_follow_the_guards_of_a_view() {
        use windows_sys::Win32::System::Memory::{
            CreateFileMappingW, FILE_MAP_WRITE, MapViewOfFile, MEMORY_MAPPED_VIEW_ADDRESS, UnmapViewOfFile,
        };
        let _one = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
        // SAFETY: a pagefile section of four blocks, mapped read-write.
        let view = unsafe {
            let section = CreateFileMappingW(-1isize as _, std::ptr::null(), PAGE_READWRITE, 0, (4 * BLOCK) as u32, std::ptr::null());
            assert!(!section.is_null());
            MapViewOfFile(section, FILE_MAP_WRITE, 0, 0, 4 * BLOCK).Value as usize
        };
        assert_ne!(view, 0);
        let (first, last) = (view / BLOCK, view / BLOCK + 4);
        // As the locked check would say: every block of the range in `clean`.
        let locked = || {
            let g = guards();
            g.clean.range(first..last).count() == last - first
        };
        assert!(!all_clean(first, last), "not guarded yet");
        // The first check reports every block (never taken) and guards them.
        assert_eq!(guard(view, view + 4 * BLOCK), Some((view / PAGE..view / PAGE + 4).collect()));
        assert!(all_clean(first, last) && locked());
        assert_eq!(guard(view, view + 4 * BLOCK), Some(Vec::new()), "nothing written since");
        // A write into block 2 faults, opens it and lands.
        // SAFETY: our view; the fault is taken by the guards' handler.
        unsafe { ((view + 2 * BLOCK + 8) as *mut u8).write_volatile(7) };
        assert!(!all_clean(first, last) && !locked());
        assert!(all_clean(first, first + 2) && all_clean(first + 3, last), "the other blocks stay clean");
        assert_eq!(
            guard(view, view + 4 * BLOCK),
            Some((view / PAGE + 2..view / PAGE + 3).collect()),
            "the written block is reported once"
        );
        assert!(all_clean(first, last) && locked(), "and guarded again");
        // The runtime maps something else there: nothing stays clean.
        title_memory_remapped(view, 4 * BLOCK);
        assert!(!all_clean(first, first + 1) && !all_clean(first + 3, last));
        // SAFETY: our view (still read-only: only the notes were dropped).
        unsafe { UnmapViewOfFile(MEMORY_MAPPED_VIEW_ADDRESS { Value: view as *mut _ }) };
    }

    /// A kernel read into a guarded view block fails; opened first, it
    /// lands and the block counts as written.
    #[test]
    fn kernel_reads_into_guarded_blocks_after_opening() {
        use std::io::{Read, Seek, Write};
        use windows_sys::Win32::System::Memory::{
            CreateFileMappingW, FILE_MAP_WRITE, MapViewOfFile, UnmapViewOfFile,
        };
        // SAFETY: a pagefile section of two blocks, mapped read-write.
        let view = unsafe {
            let section = CreateFileMappingW(
                -1isize as _,
                std::ptr::null(),
                PAGE_READWRITE,
                0,
                (2 * BLOCK) as u32,
                std::ptr::null(),
            );
            assert!(!section.is_null());
            MapViewOfFile(section, FILE_MAP_WRITE, 0, 0, 2 * BLOCK).Value as usize
        };
        assert_ne!(view, 0);
        assert!(guard(view, view + 2 * BLOCK).is_some()); // now read-only
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(b"from the disk").unwrap();
        // `ReadFile` inside: the kernel writes the view.
        let read = |file: &mut std::fs::File| {
            file.rewind().unwrap();
            // SAFETY: 13 bytes of our view, nothing else refers to them.
            let into = unsafe { std::slice::from_raw_parts_mut((view + 100) as *mut u8, 13) };
            file.read(into).is_ok_and(|n| n == 13)
        };
        assert!(
            !read(&mut file),
            "the kernel does not fault into our handler"
        );
        writing(view + 100, 13);
        assert!(read(&mut file));
        // SAFETY: the bytes just read.
        let landed = unsafe { std::slice::from_raw_parts((view + 100) as *const u8, 13) };
        assert_eq!(landed, b"from the disk");
        assert_eq!(
            guard(view, view + 2 * BLOCK),
            Some((view / PAGE..view / PAGE + 1).collect())
        );
        // SAFETY: our view.
        unsafe {
            UnmapViewOfFile(
                windows_sys::Win32::System::Memory::MEMORY_MAPPED_VIEW_ADDRESS {
                    Value: view as *mut _,
                },
            )
        };
        title_memory_remapped(view, 2 * BLOCK);
    }

    /// A view guarded on another thread before the runtime tells of its
    /// mapping is read-write once told: the title's next write there finds
    /// nothing guarded to open.
    #[test]
    fn a_view_guarded_before_it_is_told_of_is_writable_after() {
        use windows_sys::Win32::System::Memory::{
            CreateFileMappingW, FILE_MAP_WRITE, MEMORY_BASIC_INFORMATION,
            MEMORY_MAPPED_VIEW_ADDRESS, MapViewOfFile, PAGE_READONLY, UnmapViewOfFile,
            VirtualQuery,
        };
        // SAFETY: a pagefile section of one block, mapped read-write.
        let (section, view) = unsafe {
            let section = CreateFileMappingW(
                -1isize as _,
                std::ptr::null(),
                PAGE_READWRITE,
                0,
                BLOCK as u32,
                std::ptr::null(),
            );
            assert!(!section.is_null());
            let view = MapViewOfFile(section, FILE_MAP_WRITE, 0, 0, BLOCK).Value as usize;
            (section, view)
        };
        assert_ne!(view, 0);
        let protection = || {
            // SAFETY: plain data filled in by the query.
            let mut info: MEMORY_BASIC_INFORMATION = unsafe { std::mem::zeroed() };
            // SAFETY: a valid out pointer of the right size.
            let n = unsafe { VirtualQuery(view as *const _, &mut info, size_of_val(&info)) };
            assert_ne!(n, 0);
            info.Protect
        };
        assert!(guard(view, view + BLOCK).is_some());
        assert_eq!(protection(), PAGE_READONLY);
        title_memory_remapped(view, BLOCK);
        assert_eq!(protection(), PAGE_READWRITE);
        // SAFETY: our view, read-write again.
        unsafe { (view as *mut u8).write_volatile(1) };
        // SAFETY: our view and section.
        unsafe {
            UnmapViewOfFile(MEMORY_MAPPED_VIEW_ADDRESS {
                Value: view as *mut _,
            });
            windows_sys::Win32::Foundation::CloseHandle(section);
        }
        title_memory_remapped(view, BLOCK);
    }

    /// Armed blocks (write marks) tell the copy-back what they held before:
    /// untouched, the bytes from before the title's first write (kept by
    /// the guard's fault, before the write lands), or unknown after a write
    /// told when it is done. A range not all clean is not armed.
    #[test]
    fn armed_blocks_keep_their_bytes_from_before_the_first_write() {
        use windows_sys::Win32::System::Memory::{
            CreateFileMappingW, FILE_MAP_WRITE, MEMORY_MAPPED_VIEW_ADDRESS, MapViewOfFile, UnmapViewOfFile,
        };
        // SAFETY: a pagefile section of three blocks, mapped read-write.
        let (section, view) = unsafe {
            let section = CreateFileMappingW(
                -1isize as _,
                std::ptr::null(),
                PAGE_READWRITE,
                0,
                (3 * BLOCK) as u32,
                std::ptr::null(),
            );
            assert!(!section.is_null());
            let view = MapViewOfFile(section, FILE_MAP_WRITE, 0, 0, 3 * BLOCK).Value as usize;
            (section, view)
        };
        assert_ne!(view, 0);
        // SAFETY: (writes below) bytes of our view.
        unsafe {
            for k in 0..3 {
                ((view + k * BLOCK + 7) as *mut u8).write_volatile(0x40 + k as u8);
            }
        }
        assert!(guard(view, view + 3 * BLOCK).is_some(), "guarded, clean");
        assert!(arm(view, view + 3 * BLOCK));
        // The title writes block 1 (a fault), and the runtime tells of a
        // write into block 2 once it is done.
        // SAFETY: our view; the handler opens the block.
        unsafe { ((view + BLOCK + 7) as *mut u8).write_volatile(0x99) };
        writing(view + 2 * BLOCK + 7, 1);
        assert!(matches!(armed_take(view), Some(Armed::Untouched)));
        match armed_take(view + BLOCK) {
            Some(Armed::Saved(bytes)) => assert_eq!(bytes[7], 0x41, "the byte from before"),
            _ => panic!("block 1 kept"),
        }
        assert!(matches!(armed_take(view + 2 * BLOCK), Some(Armed::Lost)));
        assert!(armed_take(view).is_none(), "taken once");
        assert!(!arm(view, view + 3 * BLOCK), "blocks 1 and 2 are not clean");
        disarm();
        // SAFETY: our view and section.
        unsafe {
            UnmapViewOfFile(MEMORY_MAPPED_VIEW_ADDRESS {
                Value: view as *mut _,
            });
            windows_sys::Win32::Foundation::CloseHandle(section);
        }
        title_memory_remapped(view, 3 * BLOCK);
    }

    /// An asynchronous kernel write's blocks stay open while it may land:
    /// a check meanwhile reports them written and does not guard them
    /// again, so a kernel write into them still succeeds.
    /// An asynchronous kernel write's blocks stay open while it may land:
    /// a check meanwhile reports them written and does not guard them
    /// again, so a kernel write into them still succeeds.
    #[test]
    fn blocks_of_an_asynchronous_write_stay_open() {
        use windows_sys::Win32::System::Diagnostics::Debug::WriteProcessMemory;
        use windows_sys::Win32::System::Memory::{
            CreateFileMappingW, FILE_MAP_WRITE, MEMORY_MAPPED_VIEW_ADDRESS, MapViewOfFile, UnmapViewOfFile,
        };
        use windows_sys::Win32::System::Threading::GetCurrentProcess;
        // SAFETY: a pagefile section of one block, mapped read-write.
        let (section, view) = unsafe {
            let section = CreateFileMappingW(-1isize as _, std::ptr::null(), PAGE_READWRITE, 0, BLOCK as u32, std::ptr::null());
            assert!(!section.is_null());
            (section, MapViewOfFile(section, FILE_MAP_WRITE, 0, 0, BLOCK).Value as usize)
        };
        assert_ne!(view, 0);
        assert!(guard(view, view + BLOCK).is_some(), "guarded");
        writing_async(view + 100, 13);
        let pages: Vec<usize> = (view / PAGE..view / PAGE + 1).collect();
        assert_eq!(guard(view, view + BLOCK), Some(pages), "reported written, left open");
        let bytes = *b"from the disk";
        let mut done = 0;
        // SAFETY: 13 bytes of our view; the kernel checks the destination.
        let wrote = unsafe {
            WriteProcessMemory(GetCurrentProcess(), (view + 100) as _, bytes.as_ptr().cast(), 13, &mut done) != 0
        };
        assert!(wrote, "the kernel's late write lands");
        // SAFETY: our view and section.
        unsafe {
            UnmapViewOfFile(MEMORY_MAPPED_VIEW_ADDRESS { Value: view as *mut _ });
            windows_sys::Win32::Foundation::CloseHandle(section);
        }
        title_memory_remapped(view, BLOCK);
    }

    /// A read-only block of a view the runtime told of, which no guard
    /// knows, is opened by a write to it instead of the write faulting on.
    #[test]
    fn a_read_only_view_block_no_guard_knows_opens_on_a_write() {
        use windows_sys::Win32::System::Memory::{
            CreateFileMappingW, FILE_MAP_WRITE, MEMORY_MAPPED_VIEW_ADDRESS, MapViewOfFile, PAGE_READONLY,
            UnmapViewOfFile, VirtualProtect,
        };
        if !unknown_open() {
            return;
        }
        install();
        // SAFETY: a pagefile section of one block, mapped read-write, then
        // made read-only behind the guards' back.
        let (section, view) = unsafe {
            let section = CreateFileMappingW(-1isize as _, std::ptr::null(), PAGE_READWRITE, 0, BLOCK as u32, std::ptr::null());
            assert!(!section.is_null());
            let view = MapViewOfFile(section, FILE_MAP_WRITE, 0, 0, BLOCK).Value as usize;
            let mut old = 0;
            assert_ne!(VirtualProtect(view as *const _, BLOCK, PAGE_READONLY, &mut old), 0);
            (section, view)
        };
        assert_ne!(view, 0);
        crate::inplace::title_memory_view(view, BLOCK, section as usize, 0);
        // SAFETY: our view; the handler opens it.
        unsafe { ((view + 8) as *mut u8).write_volatile(7) };
        // SAFETY: our view.
        assert_eq!(unsafe { ((view + 8) as *const u8).read_volatile() }, 7);
        title_memory_remapped(view, BLOCK);
        // SAFETY: our view and section.
        unsafe {
            UnmapViewOfFile(MEMORY_MAPPED_VIEW_ADDRESS { Value: view as *mut _ });
            windows_sys::Win32::Foundation::CloseHandle(section);
        }
    }

    #[test]
    fn writes_in_partly_committed_view_blocks_are_reported() {
        use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
        use windows_sys::Win32::System::Memory::{
            CreateFileMappingW, FILE_MAP_WRITE, MEMORY_MAPPED_VIEW_ADDRESS, MapViewOfFile,
            SEC_RESERVE, UnmapViewOfFile,
        };

        const SIZE: usize = 3 * BLOCK;
        struct View(MEMORY_MAPPED_VIEW_ADDRESS);
        impl Drop for View {
            fn drop(&mut self) {
                title_memory_remapped(self.0.Value as usize, SIZE);
                // SAFETY: our view is still mapped.
                unsafe { UnmapViewOfFile(self.0) };
            }
        }
        // SAFETY: a new reserved pagefile section. The view holds its own
        // reference after we close the handle, including on assertion failure.
        let view = unsafe {
            let section = CreateFileMappingW(
                INVALID_HANDLE_VALUE,
                std::ptr::null(),
                PAGE_READWRITE | SEC_RESERVE,
                0,
                SIZE as u32,
                std::ptr::null(),
            );
            assert!(!section.is_null());
            let view = MapViewOfFile(section, FILE_MAP_WRITE, 0, 0, SIZE);
            CloseHandle(section);
            assert!(!view.Value.is_null());
            View(view)
        };
        let start = view.0.Value as usize;
        let commit = |at: usize, len| {
            // SAFETY: these pages belong to our reserved view.
            let committed = unsafe { VirtualAlloc(at as _, len, MEM_COMMIT, PAGE_READWRITE) };
            assert_eq!(committed as usize, at);
        };
        let early = start + BLOCK + 3 * KERNEL_PAGE;
        let late = start + 2 * BLOCK + 3 * KERNEL_PAGE;
        commit(start, BLOCK);
        commit(early, KERNEL_PAGE);
        let refresh = || guard(start, start + SIZE).unwrap();
        assert_eq!(refresh(), [start / PAGE, early / PAGE, late / PAGE]);
        // The last block is still entirely reserved.
        assert!(!refresh().contains(&(late / PAGE)));

        commit(late, KERNEL_PAGE); // Ordinary commit, without a runtime mapping notification.
        for at in [early, late] {
            assert!(writable(at));
            // SAFETY: each address is inside its committed read-write page.
            unsafe { (at as *mut u8).write_volatile(1) };
        }
        assert_eq!(refresh(), [early / PAGE, late / PAGE]);
    }

    /// A range of view memory whose first block is not mapped is a view
    /// all the same: its blocks are reported once, then the ones written
    /// or mapped since.
    #[test]
    fn a_range_that_begins_in_an_unmapped_block_is_guarded() {
        use windows_sys::Win32::System::Memory::{
            CreateFileMappingW, MEM_PRESERVE_PLACEHOLDER, MEM_REPLACE_PLACEHOLDER,
            MEM_RESERVE_PLACEHOLDER, MapViewOfFile3, PAGE_NOACCESS, UnmapViewOfFile2,
            VirtualAlloc2,
        };
        use windows_sys::Win32::System::Threading::GetCurrentProcess;
        // SAFETY: a fresh placeholder of three blocks and a pagefile
        // section of three; the middle block is mapped first.
        let (p, section) = unsafe {
            let p = VirtualAlloc2(
                GetCurrentProcess(),
                std::ptr::null(),
                3 * BLOCK,
                MEM_RESERVE | MEM_RESERVE_PLACEHOLDER,
                PAGE_NOACCESS,
                std::ptr::null_mut(),
                0,
            ) as usize;
            assert_ne!(p, 0);
            let section = CreateFileMappingW(
                -1isize as _,
                std::ptr::null(),
                PAGE_READWRITE,
                0,
                (3 * BLOCK) as u32,
                std::ptr::null(),
            );
            assert!(!section.is_null());
            (p, section)
        };
        let map = |block: usize| {
            // SAFETY: splits the placeholder at the block and maps the
            // section's block there.
            let view = unsafe {
                VirtualFree(
                    (p + block * BLOCK) as *mut _,
                    BLOCK,
                    MEM_RELEASE | MEM_PRESERVE_PLACEHOLDER,
                );
                MapViewOfFile3(
                    section,
                    GetCurrentProcess(),
                    (p + block * BLOCK) as *const _,
                    (block * BLOCK) as u64,
                    BLOCK,
                    MEM_REPLACE_PLACEHOLDER,
                    PAGE_READWRITE,
                    std::ptr::null_mut(),
                    0,
                )
            };
            assert_eq!(view.Value as usize, p + block * BLOCK);
            view
        };
        let pages = |block: usize| (p + block * BLOCK) / PAGE..(p + (block + 1) * BLOCK) / PAGE;
        let all: Vec<usize> = (0..3).flat_map(pages).collect();
        // Nothing mapped at all: a view with every block absent.
        assert!(is_view(p, p + 3 * BLOCK));
        let middle = map(1);
        title_memory_remapped(p + BLOCK, BLOCK);
        // The range begins in the placeholder, its middle block is a view.
        assert!(is_view(p, p + 3 * BLOCK));
        assert_eq!(guard(p, p + 3 * BLOCK), Some(all));
        assert_eq!(guard(p, p + 3 * BLOCK), Some(Vec::new()), "nothing written since");
        // SAFETY: inside the mapped middle block.
        unsafe { ((p + BLOCK + 8) as *mut u8).write_volatile(1) };
        assert_eq!(guard(p, p + 3 * BLOCK), Some(pages(1).collect()));
        let first = map(0);
        title_memory_remapped(p, BLOCK);
        assert_eq!(guard(p, p + 3 * BLOCK), Some(pages(0).collect()));
        assert_eq!(guard(p, p + 3 * BLOCK), Some(Vec::new()));
        // Memory of ours that is no view stays none.
        let mut local = [0u8; 16];
        let at = local.as_mut_ptr() as usize;
        assert!(!is_view(at, at + local.len()));
        // SAFETY: our views and placeholder.
        unsafe {
            UnmapViewOfFile2(GetCurrentProcess(), first, MEM_PRESERVE_PLACEHOLDER);
            UnmapViewOfFile2(GetCurrentProcess(), middle, MEM_PRESERVE_PLACEHOLDER);
            for block in 0..3 {
                VirtualFree((p + block * BLOCK) as *mut _, 0, MEM_RELEASE);
            }
            windows_sys::Win32::Foundation::CloseHandle(section);
        }
        title_memory_remapped(p, 3 * BLOCK);
    }

    /// Guarded neighbours are opened in one call when they are one view,
    /// and block by block when they are neighbouring views (which Windows
    /// does not change in one call): every block is writable after, and
    /// counts as written.
    #[test]
    fn neighbouring_guarded_blocks_are_opened_in_views_and_across_them() {
        use windows_sys::Win32::System::Memory::{
            CreateFileMappingW, FILE_MAP_WRITE, MEM_PRESERVE_PLACEHOLDER, MEM_REPLACE_PLACEHOLDER,
            MEM_RESERVE_PLACEHOLDER, MEMORY_BASIC_INFORMATION, MEMORY_MAPPED_VIEW_ADDRESS,
            MapViewOfFile, MapViewOfFile3, PAGE_NOACCESS, UnmapViewOfFile, UnmapViewOfFile2,
            VirtualAlloc2, VirtualQuery,
        };
        use windows_sys::Win32::System::Threading::GetCurrentProcess;
        let writable = |at: usize| {
            // SAFETY: plain data filled in by the query.
            let mut info: MEMORY_BASIC_INFORMATION = unsafe { std::mem::zeroed() };
            // SAFETY: a valid out pointer of the right size.
            let n = unsafe { VirtualQuery(at as *const _, &mut info, size_of_val(&info)) };
            n != 0 && info.Protect == PAGE_READWRITE
        };
        let pages = |at: usize, blocks: usize| -> Vec<usize> { (at / PAGE..(at + blocks * BLOCK) / PAGE).collect() };
        // SAFETY: a pagefile section of three blocks.
        let section = unsafe {
            CreateFileMappingW(-1isize as _, std::ptr::null(), PAGE_READWRITE, 0, (3 * BLOCK) as u32, std::ptr::null())
        };
        assert!(!section.is_null());
        // One view of the three blocks.
        // SAFETY: our section, read-write.
        let one = unsafe { MapViewOfFile(section, FILE_MAP_WRITE, 0, 0, 3 * BLOCK) }.Value as usize;
        assert_ne!(one, 0);
        assert_eq!(guard(one, one + 3 * BLOCK), Some(pages(one, 3)));
        assert!((0..3).all(|b| !writable(one + b * BLOCK)));
        writing(one, 3 * BLOCK);
        assert!((0..3).all(|b| writable(one + b * BLOCK)), "one view");
        assert_eq!(guard(one, one + 3 * BLOCK), Some(pages(one, 3)));
        // Three neighbouring views of a block each, in a placeholder.
        // SAFETY: a fresh placeholder of three blocks.
        let p = unsafe {
            VirtualAlloc2(
                GetCurrentProcess(),
                std::ptr::null(),
                3 * BLOCK,
                MEM_RESERVE | MEM_RESERVE_PLACEHOLDER,
                PAGE_NOACCESS,
                std::ptr::null_mut(),
                0,
            )
        } as usize;
        assert_ne!(p, 0);
        let views: Vec<MEMORY_MAPPED_VIEW_ADDRESS> = (0..3)
            .map(|block| {
                // SAFETY: splits the placeholder at the block and maps the
                // section's block there.
                let view = unsafe {
                    if block < 2 {
                        VirtualFree((p + block * BLOCK) as *mut _, BLOCK, MEM_RELEASE | MEM_PRESERVE_PLACEHOLDER);
                    }
                    MapViewOfFile3(
                        section,
                        GetCurrentProcess(),
                        (p + block * BLOCK) as *const _,
                        (block * BLOCK) as u64,
                        BLOCK,
                        MEM_REPLACE_PLACEHOLDER,
                        PAGE_READWRITE,
                        std::ptr::null_mut(),
                        0,
                    )
                };
                assert_eq!(view.Value as usize, p + block * BLOCK);
                view
            })
            .collect();
        title_memory_remapped(p, 3 * BLOCK);
        assert_eq!(guard(p, p + 3 * BLOCK), Some(pages(p, 3)));
        assert!((0..3).all(|b| !writable(p + b * BLOCK)));
        writing(p, 3 * BLOCK);
        assert!((0..3).all(|b| writable(p + b * BLOCK)), "neighbouring views");
        assert_eq!(guard(p, p + 3 * BLOCK), Some(pages(p, 3)));
        // A remapping opens them too.
        title_memory_remapped(p, 3 * BLOCK);
        assert!((0..3).all(|b| writable(p + b * BLOCK)), "remapped");
        // SAFETY: our views, placeholder and section.
        unsafe {
            for view in views {
                UnmapViewOfFile2(GetCurrentProcess(), view, MEM_PRESERVE_PLACEHOLDER);
            }
            for block in 0..3 {
                VirtualFree((p + block * BLOCK) as *mut _, 0, MEM_RELEASE);
            }
            UnmapViewOfFile(MEMORY_MAPPED_VIEW_ADDRESS { Value: one as *mut _ });
            windows_sys::Win32::Foundation::CloseHandle(section);
        }
        title_memory_remapped(one, 3 * BLOCK);
        title_memory_remapped(p, 3 * BLOCK);
    }

    /// Blocks of a placeholder between views are reported once, then not
    /// again (they cannot be guarded) until the runtime maps them.
    #[test]
    fn unmapped_blocks_are_reported_once_until_mapped() {
        use windows_sys::Win32::System::Memory::{
            CreateFileMappingW, MEM_PRESERVE_PLACEHOLDER, MEM_REPLACE_PLACEHOLDER,
            MEM_RESERVE_PLACEHOLDER, MapViewOfFile3, PAGE_NOACCESS, UnmapViewOfFile2,
            VirtualAlloc2,
        };
        use windows_sys::Win32::System::Threading::GetCurrentProcess;
        // SAFETY: a fresh placeholder of three blocks and a pagefile
        // section of two; block 0 shows the section's first block, as the
        // runtime maps physical pages.
        let (p, section) = unsafe {
            let p = VirtualAlloc2(
                GetCurrentProcess(),
                std::ptr::null(),
                3 * BLOCK,
                MEM_RESERVE | MEM_RESERVE_PLACEHOLDER,
                PAGE_NOACCESS,
                std::ptr::null_mut(),
                0,
            ) as usize;
            assert_ne!(p, 0);
            let section = CreateFileMappingW(
                -1isize as _,
                std::ptr::null(),
                PAGE_READWRITE,
                0,
                (2 * BLOCK) as u32,
                std::ptr::null(),
            );
            assert!(!section.is_null());
            (p, section)
        };
        let map = |block: usize| {
            // SAFETY: splits the placeholder at the block and maps the
            // section's block there.
            let view = unsafe {
                VirtualFree(
                    (p + block * BLOCK) as *mut _,
                    BLOCK,
                    MEM_RELEASE | MEM_PRESERVE_PLACEHOLDER,
                );
                MapViewOfFile3(
                    section,
                    GetCurrentProcess(),
                    (p + block * BLOCK) as *const _,
                    (block * BLOCK) as u64,
                    BLOCK,
                    MEM_REPLACE_PLACEHOLDER,
                    PAGE_READWRITE,
                    std::ptr::null_mut(),
                    0,
                )
            };
            assert_eq!(view.Value as usize, p + block * BLOCK);
            view
        };
        let first = map(0);
        let pages = |block: usize| (p + block * BLOCK) / PAGE..(p + (block + 1) * BLOCK) / PAGE;
        let all: Vec<usize> = (0..3).flat_map(pages).collect();
        assert_eq!(guard(p, p + 3 * BLOCK), Some(all));
        assert_eq!(
            guard(p, p + 3 * BLOCK),
            Some(Vec::new()),
            "nothing written since"
        );
        let second = map(1);
        title_memory_remapped(p + BLOCK, BLOCK);
        assert_eq!(guard(p, p + 3 * BLOCK), Some(pages(1).collect()));
        assert_eq!(guard(p, p + 3 * BLOCK), Some(Vec::new()));
        // SAFETY: our views and placeholder.
        unsafe {
            UnmapViewOfFile2(GetCurrentProcess(), first, MEM_PRESERVE_PLACEHOLDER);
            UnmapViewOfFile2(GetCurrentProcess(), second, MEM_PRESERVE_PLACEHOLDER);
            for block in 0..3 {
                VirtualFree((p + block * BLOCK) as *mut _, 0, MEM_RELEASE);
            }
            windows_sys::Win32::Foundation::CloseHandle(section);
        }
        title_memory_remapped(p, 3 * BLOCK);
    }

    /// Two views of one pagefile section of two blocks, told to the census
    /// as the runtime tells its own; unmapped when dropped.
    struct Shown {
        section: windows_sys::Win32::Foundation::HANDLE,
        views: Vec<usize>,
    }

    impl Shown {
        fn new() -> Shown {
            use windows_sys::Win32::System::Memory::CreateFileMappingW;
            // SAFETY: a new pagefile section of two blocks.
            let section = unsafe {
                CreateFileMappingW(
                    -1isize as _,
                    std::ptr::null(),
                    PAGE_READWRITE,
                    0,
                    (2 * BLOCK) as u32,
                    std::ptr::null(),
                )
            };
            assert!(!section.is_null());
            Shown {
                section,
                views: Vec::new(),
            }
        }

        /// Maps the whole section, as the runtime tells (`told`) or as a
        /// view of its own (not told).
        fn map(&mut self, told: bool) -> usize {
            use windows_sys::Win32::System::Memory::{FILE_MAP_WRITE, MapViewOfFile};
            // SAFETY: our section, read-write.
            let at = unsafe { MapViewOfFile(self.section, FILE_MAP_WRITE, 0, 0, 2 * BLOCK) }.Value
                as usize;
            assert_ne!(at, 0);
            if told {
                title_memory_remapped(at, 2 * BLOCK);
                crate::inplace::title_memory_view(at, 2 * BLOCK, self.section as usize, 0);
            }
            self.views.push(at);
            at
        }
    }

    impl Drop for Shown {
        fn drop(&mut self) {
            use windows_sys::Win32::System::Memory::{MEMORY_MAPPED_VIEW_ADDRESS, UnmapViewOfFile};
            for &at in &self.views {
                title_memory_remapped(at, 2 * BLOCK);
                // SAFETY: our view.
                unsafe {
                    UnmapViewOfFile(MEMORY_MAPPED_VIEW_ADDRESS {
                        Value: at as *mut _,
                    })
                };
            }
            // SAFETY: our handle.
            unsafe { windows_sys::Win32::Foundation::CloseHandle(self.section) };
        }
    }

    /// A guarded texture whose pages the title fills through another view
    /// (its pool, mapped into the tiled heap as well) is seen written, each
    /// time; so is one the other view of which is mapped after the guard.
    #[test]
    fn writes_through_another_view_of_the_pages_are_seen() {
        let mut shown = Shown::new();
        let (texture, pool) = (shown.map(true), shown.map(true));
        let pages = |at: usize, block: usize| -> Vec<usize> {
            ((at + block * BLOCK) / PAGE..(at + (block + 1) * BLOCK) / PAGE).collect()
        };
        let all: Vec<usize> = (0..2).flat_map(|b| pages(texture, b)).collect();
        assert_eq!(guard(texture, texture + 2 * BLOCK), Some(all));
        assert_eq!(guard(texture, texture + 2 * BLOCK), Some(Vec::new()));
        for round in 0..2 {
            // SAFETY: inside the pool view's second block.
            unsafe { ((pool + BLOCK + 8) as *mut u8).write_volatile(round) };
            assert_eq!(
                guard(texture, texture + 2 * BLOCK),
                Some(pages(texture, 1)),
                "round {round}"
            );
            assert_eq!(guard(texture, texture + 2 * BLOCK), Some(Vec::new()));
        }
        // SAFETY: the byte just written, read through the texture's view.
        let byte = unsafe { ((texture + BLOCK + 8) as *const u8).read_volatile() };
        assert_eq!(byte, 1);
        // A view mapped after the texture was guarded: writable from then
        // on, so the texture's pages count as written at once.
        let late = shown.map(true);
        // SAFETY: inside the late view's first block.
        unsafe { ((late + 8) as *mut u8).write_volatile(7) };
        let all: Vec<usize> = (0..2).flat_map(|b| pages(texture, b)).collect();
        assert_eq!(guard(texture, texture + 2 * BLOCK), Some(all));
        // And guarded through it from then on.
        // SAFETY: inside the late view's second block.
        unsafe { ((late + BLOCK + 8) as *mut u8).write_volatile(8) };
        assert_eq!(guard(texture, texture + 2 * BLOCK), Some(pages(texture, 1)));
        // The kernel writes into the pool once the runtime opened it.
        writing(pool, 1);
        assert_eq!(guard(texture, texture + 2 * BLOCK), Some(pages(texture, 0)));
    }

    /// A range of view memory asked of the guards alone is said all clean
    /// once guarded, and the count of changes of its regions moves with
    /// every way a clean block stops being clean: a write of the title's,
    /// one the runtime tells of, one under way, and a remapping. (Only
    /// growth is checked: other tests may change the same regions.)
    #[test]
    fn guard_changes_move_whenever_a_clean_block_is_written() {
        let mut shown = Shown::new();
        let at = shown.map(false);
        let range = (at, at + 2 * BLOCK);
        let all: Vec<usize> = (at / PAGE..(at + 2 * BLOCK) / PAGE).collect();
        let (_, written, settled) = writes_guarded(range.0, range.1).expect("a view");
        assert_eq!(written.into_iter().map(|(page, _)| page).collect::<Vec<_>>(), all);
        assert!(settled, "all guarded");
        let (_, _, settled) = writes_guarded(range.0, range.1).expect("a view");
        assert!(settled);
        let count = || guard_changes(range.0, range.1);
        // The title writes.
        let before = count();
        // SAFETY: inside the view's second block.
        unsafe { ((at + BLOCK + 8) as *mut u8).write_volatile(1) };
        assert!(count() > before, "a write of the title's");
        assert!(writes_guarded(range.0, range.1).expect("a view").2);
        // The runtime tells of a write.
        let before = count();
        writing(at + 8, 1);
        assert!(count() > before, "a write told");
        assert!(writes_guarded(range.0, range.1).expect("a view").2);
        // A write under way: not clean while it lasts, changed when done.
        let before = count();
        let held = writing_held(at + 8, 1);
        assert!(count() > before, "a write under way");
        let (_, _, settled) = writes_guarded(range.0, range.1).expect("a view");
        assert!(!settled, "a block held open is not clean");
        drop(held);
        assert!(writes_guarded(range.0, range.1).expect("a view").2);
        // A remapping.
        let before = count();
        title_memory_remapped(at, 2 * BLOCK);
        assert!(count() > before, "a remapping");
        // Memory that is no view is not answered for.
        let local = [0u8; 16];
        let mine = local.as_ptr() as usize;
        assert!(writes_guarded(mine, mine + local.len()).is_none());
        // The journal forgets the view's pages before the address is given
        // back: a test that is handed it next starts from nothing written.
        let mut j = JOURNAL.lock().unwrap_or_else(|e| e.into_inner());
        let pages: Vec<usize> = j.written.range(at / PAGE..(at + 2 * BLOCK) / PAGE).map(|(&p, _)| p).collect();
        for page in pages {
            j.written.remove(&page);
        }
    }

    /// A kernel write into pages another view guards must not fail because
    /// the next look at the texture guards the pages again: they are linked
    /// when the write is done, and the texture counts as written then.
    #[test]
    fn a_write_under_way_is_not_guarded_again_through_another_view() {
        use std::io::{Read, Seek, Write};
        let pages_of = |at: usize, block: usize| -> Vec<usize> {
            ((at + block * BLOCK) / PAGE..(at + (block + 1) * BLOCK) / PAGE).collect()
        };
        let mut shown = Shown::new();
        let (texture, pool) = (shown.map(true), shown.map(true));
        let pages = |block: usize| -> Vec<usize> {
            ((texture + block * BLOCK) / PAGE..(texture + (block + 1) * BLOCK) / PAGE).collect()
        };
        assert!(guard(texture, texture + 2 * BLOCK).is_some_and(|w| !w.is_empty()));
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(b"from the disk").unwrap();
        file.rewind().unwrap();
        let held = writing_held(pool + 100, 13);
        // The texture is looked at while the kernel's write is under way.
        assert_eq!(guard(texture, texture + 2 * BLOCK), Some(pages(0)));
        // SAFETY: 13 bytes of the pool view, nothing else refers to them.
        let into = unsafe { std::slice::from_raw_parts_mut((pool + 100) as *mut u8, 13) };
        assert!(
            file.read(into).is_ok_and(|n| n == 13),
            "the pool stays open"
        );
        drop(held);
        assert_eq!(guard(texture, texture + 2 * BLOCK), Some(pages(0)));
        // The written view itself is not guarded again while the write is
        // under way, and is guarded by the first copy after it.
        let held = writing_held(pool + 100, 13);
        assert_eq!(guard(pool, pool + BLOCK), Some(pages_of(pool, 0)));
        assert_eq!(guard(pool, pool + BLOCK), Some(pages_of(pool, 0)));
        // SAFETY: 13 bytes of the pool view, nothing else refers to them.
        unsafe { std::ptr::write_bytes((pool + 100) as *mut u8, 9, 13) };
        drop(held);
        assert_eq!(guard(pool, pool + BLOCK), Some(pages_of(pool, 0)));
        assert_eq!(guard(pool, pool + BLOCK), Some(Vec::new()));
        // Linked again: a store through the pool is seen.
        // SAFETY: inside the pool view's first block.
        unsafe { ((pool + 8) as *mut u8).write_volatile(3) };
        assert_eq!(guard(texture, texture + 2 * BLOCK), Some(pages(0)));
    }

    /// The runtime's writes into physical pages through a view of its own
    /// (DirectStorage's page destinations) are seen where the title's views
    /// show them, once told at those addresses (xmem's `pages_written`).
    #[test]
    fn physical_pages_written_by_the_runtime_are_seen() {
        let mut shown = Shown::new();
        let texture = shown.map(true);
        let own = shown.map(false);
        assert!(guard(texture, texture + 2 * BLOCK).is_some_and(|w| !w.is_empty()));
        assert_eq!(guard(texture, texture + 2 * BLOCK), Some(Vec::new()));
        // SAFETY: inside our own view's second block.
        unsafe { ((own + BLOCK + 100) as *mut u8).write_volatile(5) };
        assert_eq!(
            guard(texture, texture + 2 * BLOCK),
            Some(Vec::new()),
            "unseen until told"
        );
        title_memory_written(texture + BLOCK + 100, 13, true);
        assert_eq!(
            guard(texture, texture + 2 * BLOCK),
            Some(((texture + BLOCK) / PAGE..(texture + 2 * BLOCK) / PAGE).collect())
        );
    }

    /// A pagefile section mapped twice: two views of the same physical
    /// pages, the second left for the test; the section handle, the two
    /// addresses.
    fn two_views(bytes: usize) -> (usize, usize, usize) {
        use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
        use windows_sys::Win32::System::Memory::{
            CreateFileMappingW, FILE_MAP_WRITE, MapViewOfFile,
        };
        // SAFETY: a fresh pagefile section, mapped twice; the views keep
        // the section alive and the handle is closed by the caller.
        unsafe {
            let section = CreateFileMappingW(
                INVALID_HANDLE_VALUE,
                std::ptr::null(),
                PAGE_READWRITE,
                0,
                bytes as u32,
                std::ptr::null(),
            );
            assert!(!section.is_null());
            let one = MapViewOfFile(section, FILE_MAP_WRITE, 0, 0, bytes).Value as usize;
            let two = MapViewOfFile(section, FILE_MAP_WRITE, 0, 0, bytes).Value as usize;
            assert_ne!(one, 0);
            assert_ne!(two, 0);
            crate::inplace::title_memory_view(one, bytes, section as usize, 0);
            crate::inplace::title_memory_view(two, bytes, section as usize, 0);
            (section as usize, one, two)
        }
    }

    /// The section of [`two_views`] is closed and its views unmapped; the
    /// guard is told they are gone first.
    fn drop_views(section: usize, views: &[usize], bytes: usize) {
        use windows_sys::Win32::Foundation::CloseHandle;
        use windows_sys::Win32::System::Memory::{MEMORY_MAPPED_VIEW_ADDRESS, UnmapViewOfFile};
        for &view in views {
            title_memory_remapped(view, bytes);
            // SAFETY: a view of the section mapped by `two_views`.
            unsafe {
                UnmapViewOfFile(MEMORY_MAPPED_VIEW_ADDRESS {
                    Value: view as *mut _,
                })
            };
        }
        // SAFETY: the section's last reference.
        unsafe { CloseHandle(section as *mut std::ffi::c_void) };
    }

    /// The title wrote the pages of a guarded texture through another view
    /// of them: the fault at that view is the guard's to see, and the
    /// guarded view's next check reports the pages written.
    #[test]
    fn a_write_through_another_view_is_reported_at_the_guarded_one() {
        const SIZE: usize = 2 * BLOCK;
        let (section, guarded, other) = two_views(SIZE);
        let pages = |block: usize| (guarded + block * BLOCK) / PAGE;
        // The first check takes it in (and every view of its pages).
        assert_eq!(guard(guarded, guarded + SIZE), Some(vec![pages(0), pages(1)]));
        assert_eq!(guard(guarded, guarded + SIZE), Some(Vec::new()), "nothing written yet");
        // Writing through the other view faults into the handler, which
        // opens that view and the guarded one: the next check sees it.
        // SAFETY: the second view, read-write once the handler has opened it.
        unsafe { ((other + BLOCK + 8) as *mut u8).write(1) };
        assert_eq!(guard(guarded, guarded + SIZE), Some(vec![pages(1)]));
        assert_eq!(guard(guarded, guarded + SIZE), Some(Vec::new()));
        drop_views(section, &[guarded, other], SIZE);
    }

    /// A view of guarded pages is mapped anew: it is writable, so the
    /// guarded view checks its pages again (and guards the new view).
    #[test]
    fn a_view_mapped_beside_a_guarded_one_makes_it_check_again() {
        const SIZE: usize = 2 * BLOCK;
        let (section, guarded, other) = two_views(SIZE);
        let pages = |block: usize| (guarded + block * BLOCK) / PAGE;
        assert_eq!(guard(guarded, guarded + SIZE), Some(vec![pages(0), pages(1)]));
        assert_eq!(guard(guarded, guarded + SIZE), Some(Vec::new()), "nothing written yet");
        title_memory_remapped(other, SIZE);
        crate::inplace::title_memory_view(other, SIZE, section, 0);
        assert_eq!(guard(guarded, guarded + SIZE), Some(vec![pages(0), pages(1)]));
        assert_eq!(guard(guarded, guarded + SIZE), Some(Vec::new()));
        drop_views(section, &[guarded, other], SIZE);
    }

    /// A write of the runtime's own, told at one view of the pages
    /// (`writing`): the other views' next checks report the pages written.
    #[test]
    fn a_write_told_at_one_view_is_reported_at_the_others() {
        const SIZE: usize = 2 * BLOCK;
        let (section, guarded, other) = two_views(SIZE);
        let pages = |block: usize| (guarded + block * BLOCK) / PAGE;
        assert_eq!(guard(guarded, guarded + SIZE), Some(vec![pages(0), pages(1)]));
        assert_eq!(guard(guarded, guarded + SIZE), Some(Vec::new()), "nothing written yet");
        writing(other + BLOCK, BLOCK);
        assert_eq!(guard(guarded, guarded + SIZE), Some(vec![pages(1)]));
        assert_eq!(guard(guarded, guarded + SIZE), Some(Vec::new()));
        // Half a page and the next: both blocks, not just the first's.
        writing(other + BLOCK / 2, BLOCK);
        assert_eq!(guard(guarded, guarded + SIZE), Some(vec![pages(0), pages(1)]));
        drop_views(section, &[guarded, other], SIZE);
    }

    /// Both views of the pages are checked, each for a copy of its own
    /// (the title shows one physical page at several addresses, and each
    /// address has its host copy): a write through either is reported at
    /// both, whichever is checked first. Guarding the pages for the first
    /// view's check must not note the second view clean, or its copy would
    /// keep the bytes from before the write.
    #[test]
    fn a_write_is_reported_at_every_checked_view() {
        const SIZE: usize = 2 * BLOCK;
        let (section, one, two) = two_views(SIZE);
        let pages = |view: usize, block: usize| (view + block * BLOCK) / PAGE;
        assert_eq!(guard(one, one + SIZE), Some(vec![pages(one, 0), pages(one, 1)]));
        assert_eq!(guard(two, two + SIZE), Some(vec![pages(two, 0), pages(two, 1)]), "the second view's first check");
        assert_eq!(guard(one, one + SIZE), Some(Vec::new()), "nothing written yet");
        assert_eq!(guard(two, two + SIZE), Some(Vec::new()), "nothing written yet");
        // SAFETY: the second view, read-write once the handler has opened it.
        unsafe { ((two + BLOCK + 8) as *mut u8).write(1) };
        assert_eq!(guard(one, one + SIZE), Some(vec![pages(one, 1)]));
        assert_eq!(guard(two, two + SIZE), Some(vec![pages(two, 1)]), "written through this view, checked second");
        // SAFETY: the first view, as above.
        unsafe { ((one + 8) as *mut u8).write(1) };
        assert_eq!(guard(two, two + SIZE), Some(vec![pages(two, 0)]));
        assert_eq!(guard(one, one + SIZE), Some(vec![pages(one, 0)]), "written through this view, checked second");
        assert_eq!(guard(one, one + SIZE), Some(Vec::new()));
        assert_eq!(guard(two, two + SIZE), Some(Vec::new()));
        drop_views(section, &[one, two], SIZE);
    }

    /// A page shown in more than `MANY_VIEWS` views is guarded at each by
    /// itself: the others stay writable, and nothing looks through them all.
    #[test]
    fn a_page_of_many_views_is_guarded_one_view_at_a_time() {
        let (section, one, two) = two_views(BLOCK);
        let filler = one + (1 << 45);
        for n in 0..MANY_VIEWS {
            crate::inplace::title_memory_view(filler + n * BLOCK, BLOCK, section, 0);
        }
        assert_eq!(guard(one, one + BLOCK), Some(vec![one / PAGE]));
        assert!(writable(two), "another view of it is left alone");
        // SAFETY: the guarded view, opened by the handler.
        unsafe { ((one + 8) as *mut u8).write(1) };
        assert_eq!(guard(one, one + BLOCK), Some(vec![one / PAGE]));
        title_memory_remapped(filler, MANY_VIEWS * BLOCK);
        drop_views(section, &[one, two], BLOCK);
    }

    /// The view tests with blocks made read-only outside the locks
    /// ([`guard_outside`]): pages shown at another view are still guarded
    /// together with it, under the lock; a page of many views is guarded by
    /// itself, outside it.
    #[test]
    fn view_guards_hold_with_blocks_made_read_only_outside_the_locks() {
        struct Back;
        impl Drop for Back {
            fn drop(&mut self) {
                GUARD_OUTSIDE_TEST.store(0, std::sync::atomic::Ordering::Relaxed);
            }
        }
        let _back = Back;
        GUARD_OUTSIDE_TEST.store(2, std::sync::atomic::Ordering::Relaxed);
        let before = OUTSIDE[0].load(std::sync::atomic::Ordering::Relaxed);
        a_write_through_another_view_is_reported_at_the_guarded_one();
        a_view_mapped_beside_a_guarded_one_makes_it_check_again();
        a_write_told_at_one_view_is_reported_at_the_others();
        a_write_is_reported_at_every_checked_view();
        neighbouring_views_open_for_a_host_write();
        a_page_of_many_views_is_guarded_one_view_at_a_time();
        assert!(OUTSIDE[0].load(std::sync::atomic::Ordering::Relaxed) > before, "the page of many views, outside");
    }

    /// A write or mapping that runs past the end of the address space (a
    /// title's `fread` with a huge count) covers up to the end; the sum used
    /// to overflow. Up there no test has guarded blocks.
    #[test]
    fn ranges_past_the_address_space_end_do_not_overflow() {
        let top = usize::MAX - 16;
        writing(top, PAGE);
        title_memory_remapped(top, usize::MAX);
    }
}

#[cfg(test)]
mod mapping_tests {
    use super::{
        title_memory_reading, title_memory_reading_range, title_memory_remapping,
        title_memory_remapping_range,
    };
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, mpsc};
    use std::time::Duration;
    static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn disjoint_reader_can_run_while_a_range_is_remapped() {
        let _serial = SERIAL.lock().unwrap();
        let _writer = super::title_memory_remapping_range(0x10000, 0x1000);
        assert!(super::title_memory_reading_range_now(0x20000, 0x1000).is_some());
        assert!(super::title_memory_reading_range_now(0x10800, 1).is_none());
        assert!(super::title_memory_reading_now().is_none());
        assert!(super::title_memory_reading_range_now(usize::MAX, 2).is_none());
    }

    #[test]
    fn nonblocking_read_does_not_wait_for_registry_mutex() {
        let _serial = SERIAL.lock().unwrap();
        let _registry = super::mapping_state();
        assert!(super::title_memory_reading_range_now(0x20000, 0x1000).is_none());
        assert!(super::title_memory_reading_now().is_none());
    }

    #[test]
    fn waiting_overlapping_writer_prevents_new_readers() {
        let _serial = SERIAL.lock().unwrap();
        let reader = super::title_memory_reading_range(0x30000, 0x1000);
        let (started, wait) = mpsc::channel();
        let thread = std::thread::spawn(move || {
            started.send(()).unwrap();
            let _writer = super::title_memory_remapping_range(0x30000, 0x1000);
        });
        wait.recv().unwrap();
        // Wait for the writer to register, rather than relying on scheduling.
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        while !super::mapping_writer_pending_for_test(0x30000, 0x1000) {
            assert!(std::time::Instant::now() < deadline, "writer failed to register");
            std::thread::yield_now();
        }
        assert!(super::title_memory_reading_range_now(0x30000, 1).is_none());
        assert!(super::title_memory_reading_range_now(0x40000, 1).is_some());
        drop(reader);
        thread.join().unwrap();
    }

    /// A change of mapping waits for a read in place that is under way, and
    /// a read in place waits for a change of mapping.
    #[test]
    fn reads_in_place_and_remappings_wait_for_each_other() {
        let _serial = SERIAL.lock().unwrap();
        // A reader holds; the change does not begin until it lets go.
        let changed = Arc::new(AtomicBool::new(false));
        let reading = title_memory_reading();
        let (began, wait) = mpsc::channel();
        let change = {
            let changed = changed.clone();
            std::thread::spawn(move || {
                began.send(()).unwrap();
                let _changing = title_memory_remapping_range(0x10000..0x20000);
                changed.store(true, Ordering::SeqCst);
            })
        };
        wait.recv().unwrap();
        std::thread::sleep(Duration::from_millis(100));
        assert!(!changed.load(Ordering::SeqCst), "the mapping changed under a read in place");
        drop(reading);
        change.join().unwrap();
        assert!(changed.load(Ordering::SeqCst));
        // A change holds; an overlapping read waits, while a disjoint read proceeds.
        let read = Arc::new(AtomicBool::new(false));
        let changing = title_memory_remapping_range(0x10000..0x20000);
        let _disjoint = title_memory_reading_range(0x30000..0x40000);
        drop(_disjoint);
        let (began, wait) = mpsc::channel();
        let reader = {
            let read = read.clone();
            std::thread::spawn(move || {
                began.send(()).unwrap();
                let _reading = title_memory_reading_range(0x10000..0x20000);
                read.store(true, Ordering::SeqCst);
            })
        };
        wait.recv().unwrap();
        std::thread::sleep(Duration::from_millis(100));
        assert!(!read.load(Ordering::SeqCst), "a read in place began during an overlapping change");
        drop(changing);
        reader.join().unwrap();
        assert!(read.load(Ordering::SeqCst));
        let _all = title_memory_remapping();
    }
}
