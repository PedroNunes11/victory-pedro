//! Descriptor heaps: zeroed memory, with handles that are plain addresses
//! (CPU and GPU alike, as on the console).
//!
//! Descriptor sizes: views 32 bytes and samplers 16, the size of RDNA 2's
//! image and sampler descriptors, so a title that copies descriptors itself
//! moves whole ones. Render target and depth views are ours alone; 128 bytes
//! leaves room for whatever stride a title assumes.
#![allow(clippy::not_unsafe_ptr_arg_deref)]

use crate::object::{self, DESCRIPTOR_HEAP, E_INVALIDARG, HRESULT, Iid, Object, Table, This, guid};
use crate::resource::{Memory, PAGEABLE_IIDS};

#[cfg(windows)]
pub(crate) mod host;

/// The source bytes needed for a desktop CBV upload. Allocation and
/// descriptor creation apply their own 256-byte rounding.
#[cfg(any(windows, test))]
pub(crate) fn constant_staging_bytes(size: u32) -> u32 {
    size.min(64 * 1024)
}

const HEAP_IIDS: [Iid; 4] = [
    guid(
        0x8efb471d,
        0x616c,
        0x4f49,
        [0x90, 0xf7, 0x12, 0x7b, 0xb7, 0x63, 0xfa, 0x51],
    ),
    PAGEABLE_IIDS[0],
    PAGEABLE_IIDS[1],
    PAGEABLE_IIDS[2],
];

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub(crate) struct HeapDesc {
    kind: u32,
    count: u32,
    flags: u32,
    nodes: u32,
}

/// Newer titles (the title) pass the description followed by
/// memory of their own for the descriptors: an address and its size in
/// bytes, or a null address to have the heap allocate (inferred from the
/// title's call sites; older titles pass the plain description).
#[repr(C)]
#[derive(Debug, Clone, Copy)]
struct Placement {
    address: u64,
    size: u64,
}

#[repr(C)]
pub(crate) struct Heap {
    object: Object,
    desc: HeapDesc,
    /// The first descriptor: in `memory`, or in the title's own memory.
    pub(crate) start: u64,
    memory: Option<Memory>,
    /// Shader-visible view and sampler heaps get a host heap with the same
    /// descriptor order when a draw first uses them (decision 0007, step
    /// 3); slots written before wait in `pending`.
    #[cfg(windows)]
    host: std::sync::OnceLock<Option<host::HostHeap>>,
    #[cfg(windows)]
    pending: std::sync::Mutex<Vec<u32>>,
    /// Changed pages of a placed shader-visible heap. Some titles copy
    /// descriptor records into it as bytes, without a descriptor API call.
    #[cfg(windows)]
    written: Option<crate::watch::Tracked>,
}

pub(crate) fn heap<'a>(this: This) -> &'a Heap {
    // SAFETY: called from descriptor heap methods only.
    unsafe { &*this.cast::<Heap>() }
}

impl Heap {
    /// Whether this is a sampler heap.
    pub(crate) fn samplers(&self) -> bool {
        self.desc.kind == SAMPLERS
    }

    /// The host heap, made on first use from what the title wrote so far.
    /// Made late because a host view heap the size of the title's
    /// costs over 100 MB of commit, which its boot needs for its page pool.
    #[cfg(windows)]
    pub(crate) fn host(&self) -> Option<&host::HostHeap> {
        if let Some(h) = self.host.get() {
            return h.as_ref();
        }
        if self.desc.flags & SHADER_VISIBLE == 0 || !matches!(self.desc.kind, VIEWS_KIND | SAMPLERS)
        {
            return None;
        }
        let mut pending = self.pending.lock().unwrap_or_else(|e| e.into_inner());
        // A recorder can take the fast path as soon as the OnceLock is
        // initialized. Publish only after replaying the queued writes.
        self.host
            .get_or_init(|| {
                let step = u64::from(increment(self.desc.kind)?);
                let h = host::HostHeap::new(self.samplers(), self.desc.count)?;
                for i in pending.drain(..) {
                    h.mirror(i, self.start + u64::from(i) * step, false);
                }
                Some(h)
            })
            .as_ref()
    }

    /// The handle of descriptor `index` of this heap.
    #[cfg(windows)]
    pub(crate) fn handle(&self, index: u32) -> Option<u64> {
        let step = u64::from(increment(self.desc.kind)?);
        (index < self.desc.count).then(|| self.start + u64::from(index) * step)
    }

    /// The descriptor index of `handle` in this heap, if it lies in it.
    pub(crate) fn index(&self, handle: u64) -> Option<u32> {
        let step = u64::from(increment(self.desc.kind)?);
        let at = handle.checked_sub(self.start)?;
        let index = at / step;
        (index < u64::from(self.desc.count)).then_some(index as u32)
    }
}

const VIEWS_KIND: u32 = 0;
const SAMPLERS: u32 = 1;
const SHADER_VISIBLE: u32 = 1;

/// Shader-visible heaps by address range, for mirroring descriptor writes:
/// start, end, heap.
///
/// Read by every lookup and by [`sync_heaps`], written only when a heap is
/// made or dropped. It was a mutex, and [`sync_heaps`] holds it for its
/// whole pass (15 ms a frame on the submitting thread): the profile of run
/// 225 has the title's six recording threads waiting 48.5 ms a frame in
/// [`shader_slot`] for it, each lookup itself a few nanoseconds. Readers no
/// longer wait for each other. Nothing a holder calls takes it again.
#[cfg(windows)]
static SHADER_HEAPS: std::sync::RwLock<Vec<(u64, u64, usize)>> = std::sync::RwLock::new(Vec::new());

/// One [`sync_heaps`] at a time, as when the list above was a mutex.
#[cfg(windows)]
static SYNCING: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// The shader-visible heap holding `handle`, and the descriptor's index.
#[cfg(windows)]
fn shader_slot<'a>(handle: u64) -> Option<(&'a Heap, u32)> {
    let heaps = SHADER_HEAPS.read().unwrap_or_else(|e| e.into_inner());
    let &(_, _, at) = heaps
        .iter()
        .find(|&&(start, end, _)| (start..end).contains(&handle))?;
    // SAFETY: registered heaps are live (removed when dropped).
    let heap = unsafe { &*(at as *const Heap) };
    Some((heap, heap.index(handle)?))
}

/// Makes the host descriptor for what the title wrote at `handle`, if it
/// is in a shader-visible heap.
#[cfg(windows)]
fn mirror(handle: u64) {
    let Some((heap, index)) = shader_slot(handle) else {
        return;
    };
    let mut pending = heap.pending.lock().unwrap_or_else(|e| e.into_inner());
    match heap.host.get() {
        None => pending.push(index),
        Some(host) => {
            drop(pending);
            if let Some(host) = host {
                host.mirror(index, handle, false);
            }
        }
    }
}

/// Mirrors records copied directly into placed shader-visible heaps. Calls
/// to `Create*View` are already mirrored by [`mirror`]; raw CPU copies leave
/// only write-watch pages to reveal which slots changed.
///
/// Upstream VIctory 86a5ed6 does the same and counts 1,300 to 14,400 such
/// slots a frame in the world of the title this fork works on, against a
/// few hundred views made by calls; without it those host slots stayed
/// null, or went on showing what they showed before.
/// `VICTORY_HEAP_SYNC=0`: not done, as before. [`SYNCED`] counts.
#[cfg(windows)]
pub(crate) fn sync_heaps() {
    use std::sync::atomic::Ordering::Relaxed;
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    if !*ON.get_or_init(|| std::env::var_os("VICTORY_HEAP_SYNC").is_none_or(|v| v != "0")) {
        return;
    }
    let started = std::time::Instant::now();
    // The slots this pass changes are counted apart (diagnostics).
    #[cfg(feature = "diagnostics")]
    struct InSync;
    #[cfg(feature = "diagnostics")]
    impl Drop for InSync {
        fn drop(&mut self) {
            host::IN_SYNC.set(false);
        }
    }
    #[cfg(feature = "diagnostics")]
    let _in_sync = {
        host::IN_SYNC.set(true);
        InSync
    };
    // With `VICTORY_SYNC_LIGHT`, the resources the pass's slots let go of are
    // retired together when it ends (`host::sync_light`).
    let _retire = host::sync_retire_batch();
    let (mut pages, mut looked, mut mirrored, mut reading, mut taking) = (0u64, 0u64, 0u64, 0u64, 0u64);
    let mut page_bytes: Vec<u8> = Vec::new();
    let _one = SYNCING.lock().unwrap_or_else(|e| e.into_inner());
    let heaps = SHADER_HEAPS.read().unwrap_or_else(|e| e.into_inner());
    for &(start, end, at) in heaps.iter() {
        // SAFETY: registered heaps are live (removed when dropped).
        let heap = unsafe { &*(at as *const Heap) };
        let (Some(Some(host)), Some(written), Some(step)) =
            (heap.host.get(), &heap.written, increment(heap.desc.kind))
        else {
            continue;
        };
        let step = u64::from(step);
        // Capped host heaps have no counterparts for the title slots reserved
        // for per-draw tables; raw writes there must not be mirrored.
        let mirrored_count = heap.desc.count.min(host.count());
        let take_began = std::time::Instant::now();
        let ranges: Vec<(usize, usize)> = written.take(start as usize, end as usize).into_iter().collect();
        taking += take_began.elapsed().as_micros() as u64;
        if sync_wide() && page_at_once() {
            let [p, l, m, r] = sync_pages_wide(host, &ranges, (start, step), mirrored_count);
            (pages, looked, mirrored, reading) = (pages + p, looked + l, mirrored + m, reading + r);
            continue;
        }
        for (range_from, range_end) in ranges {
            let mut page = range_from & !(crate::watch::PAGE - 1);
            while page < range_end {
                let from = page.max(range_from);
                let to = (page + crate::watch::PAGE).min(range_end);
                // The page's records are read through the kernel, into a
                // copy: a page the title unmaps meanwhile fails the read
                // where 2,048 reads in place would fault.
                page_bytes.resize(to - from, 0);
                let read_began = std::time::Instant::now();
                let read = crate::host::read_into(from as *const u8, &mut page_bytes);
                reading += read_began.elapsed().as_micros() as u64;
                if read {
                    pages += 1;
                    let slots = changed_slots(from, to, start as usize, step, mirrored_count);
                    // The page's records in one go, under one lock
                    // (`HostHeap::mirror_copied_page`); one by one below
                    // with `VICTORY_HEAP_SYNC_PAGE=0`.
                    let slots = if page_at_once() {
                        let (seen, made) = host.mirror_copied_page(slots, (start, step), from, &page_bytes);
                        looked += seen;
                        mirrored += made;
                        0..0
                    } else {
                        slots
                    };
                    for index in slots {
                        let handle = start + index * step;
                        // A record that straddles the range's ends is left
                        // to the range that holds its first byte.
                        let Some(at) = (handle as usize).checked_sub(from) else {
                            continue;
                        };
                        let Some(record) = page_bytes.get(at..at + step as usize) else {
                            continue;
                        };
                        looked += 1;
                        // Only a record that is not the one last seen there
                        // (`HostHeap::mirror_copied`).
                        if host.mirror_copied(index as u32, handle, record) {
                            mirrored += 1;
                        }
                    }
                }
                page += crate::watch::PAGE;
            }
        }
    }
    drop(heaps);
    for (count, n) in SYNCED.iter().zip([pages, looked, mirrored, started.elapsed().as_micros() as u64, reading, taking]) {
        count.fetch_add(n, Relaxed);
    }
}

/// Whether a thread of ours runs [`sync_heaps`] ahead of the submissions
/// (`VICTORY_HEAP_PRESYNC=1`, live; off by default): woken after each
/// submission has gone to the card, and every few milliseconds, it mirrors
/// the records the title copied in since, so that the pass each submission
/// still makes on its own thread finds only what came after. Run 212: the
/// pass took 14.9 ms a frame of the submitting thread, which the card waits
/// for (8-14 ms a frame); run 176 ran it beside the submission's own copies,
/// and the submission still waited 10.2 ms a frame for it. A record is
/// mirrored once the title has written it, as at the submission: only
/// earlier.
#[cfg(windows)]
fn presync() -> bool {
    static ON: crate::live::Switch = crate::live::Switch::new("HEAP_PRESYNC", 0);
    ON.on()
}

/// What wakes the thread of [`presync`]: set by [`presync_kick`].
#[cfg(windows)]
static PRESYNC_WAKE: (std::sync::Mutex<bool>, std::sync::Condvar) = (std::sync::Mutex::new(false), std::sync::Condvar::new());

/// Since the last frame line: passes of [`sync_heaps`] the thread of
/// [`presync`] made, and their microseconds.
#[cfg(windows)]
static PRESYNCED: [std::sync::atomic::AtomicU64; 2] = [const { std::sync::atomic::AtomicU64::new(0) }; 2];

/// With [`presync`] on: wakes its thread (started at the first call). After
/// a submission has gone to the card.
#[cfg(windows)]
pub(crate) fn presync_kick() {
    if !presync() {
        return;
    }
    static STARTED: std::sync::Once = std::sync::Once::new();
    STARTED.call_once(|| {
        if let Err(e) = std::thread::Builder::new().name("victory heap presync".into()).spawn(presync_thread) {
            tracing::warn!(error = %e, "no thread to sync descriptor heaps ahead of the submissions");
        }
    });
    let (woken, wake) = &PRESYNC_WAKE;
    *woken.lock().unwrap_or_else(|e| e.into_inner()) = true;
    wake.notify_one();
}

/// The thread of [`presync`]: a pass of [`sync_heaps`] when woken, or every
/// 4 ms, while the switch is on.
#[cfg(windows)]
fn presync_thread() {
    use std::sync::atomic::Ordering::Relaxed;
    crate::framelog::keep_on_fast_cores("victory-heap-presync");
    let (woken, wake) = &PRESYNC_WAKE;
    loop {
        {
            let guard = woken.lock().unwrap_or_else(|e| e.into_inner());
            let (mut guard, _) = wake
                .wait_timeout_while(guard, std::time::Duration::from_millis(4), |woken| !*woken)
                .unwrap_or_else(|e| e.into_inner());
            *guard = false;
        }
        if !presync() {
            continue;
        }
        let began = std::time::Instant::now();
        sync_heaps();
        PRESYNCED[0].fetch_add(1, Relaxed);
        PRESYNCED[1].fetch_add(began.elapsed().as_micros() as u64, Relaxed);
    }
}

/// Whether [`sync_heaps`] spreads a heap's changed pages over the threads
/// kept for copies (`VICTORY_HEAP_SYNC_WIDE=1`; off by default).
///
/// Run 118 (gameplay profile): the pass took 17.7 ms of the submitting
/// thread a frame, the title's render thread, which is the frame's limit
/// (the card stood ~20 ms a frame waiting for it, run 119), most of it
/// making ~10,000 host slots one after the other. The pages are
/// independent: each slot is made under the lock of its own stripe
/// (`host::Made`), the fingerprints under one lock a page, as before.
#[cfg(windows)]
fn sync_wide() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        let on = std::env::var_os("VICTORY_HEAP_SYNC_WIDE").is_some_and(|v| v == "1");
        if on {
            tracing::info!("the pass over records the title copied into its heaps runs on several threads (VICTORY_HEAP_SYNC_WIDE=1)");
        }
        on
    })
}

/// [`sync_heaps`] over `ranges` (changed bytes) of one heap with
/// [`sync_wide`]: its pages in some sixteen pieces, run by the threads kept
/// for copies and this one (`crate::shadow::wide_run`). Returns pages read,
/// records looked at, records mirrored and microseconds reading pages
/// (summed over the threads).
#[cfg(windows)]
fn sync_pages_wide(
    heap_host: &host::HostHeap,
    ranges: &[(usize, usize)],
    (start, step): (u64, u64),
    mirrored_count: u32,
) -> [u64; 4] {
    use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
    let mut pages: Vec<(usize, usize)> = Vec::new();
    for &(range_from, range_end) in ranges {
        let mut page = range_from & !(crate::watch::PAGE - 1);
        while page < range_end {
            pages.push((page.max(range_from), (page + crate::watch::PAGE).min(range_end)));
            page += crate::watch::PAGE;
        }
    }
    let counts = std::sync::Arc::new([const { AtomicU64::new(0) }; 4]);
    let per = pages.len().div_ceil(16).max(1);
    // The host heap outlives the jobs: `wide_run` returns when all of them
    // are done, and the caller holds the list of heaps, out of which a heap
    // is taken (under the other side of that lock) before it goes.
    let at = std::ptr::from_ref(heap_host) as usize;
    let jobs: Vec<Box<dyn FnOnce() + Send>> = pages
        .chunks(per)
        .map(|chunk| {
            let (chunk, counts) = (chunk.to_vec(), counts.clone());
            Box::new(move || {
                // SAFETY: live for the whole job (above).
                let heap_host = unsafe { &*(at as *const host::HostHeap) };
                let mut bytes: Vec<u8> = Vec::new();
                for (from, to) in chunk {
                    // As one by one: read through the kernel, into a copy.
                    bytes.resize(to - from, 0);
                    let began = std::time::Instant::now();
                    let read = crate::host::read_into(from as *const u8, &mut bytes);
                    counts[3].fetch_add(began.elapsed().as_micros() as u64, Relaxed);
                    if !read {
                        continue;
                    }
                    counts[0].fetch_add(1, Relaxed);
                    let slots = changed_slots(from, to, start as usize, step, mirrored_count);
                    let (seen, made) = heap_host.mirror_copied_page(slots, (start, step), from, &bytes);
                    counts[1].fetch_add(seen, Relaxed);
                    counts[2].fetch_add(made, Relaxed);
                }
            }) as Box<dyn FnOnce() + Send>
        })
        .collect();
    crate::shadow::wide_run(jobs);
    counts.each_ref().map(|c| c.load(Relaxed))
}

/// Since the last frame line, of [`sync_heaps`]: changed pages of placed
/// shader-visible heaps looked at, slots on them, slots whose record was
/// not the one last seen (mirrored), the microseconds it took, of them
/// those spent reading the pages, and those spent asking which pages
/// changed.
#[cfg(windows)]
pub(crate) static SYNCED: [std::sync::atomic::AtomicU64; 6] = [const { std::sync::atomic::AtomicU64::new(0) }; 6];

/// With the frame line: what [`sync_heaps`] did per frame over `frames`.
/// Nothing while no heap page changed.
#[cfg(windows)]
pub(crate) fn sync_line(frames: f64) {
    let [pages, looked, mirrored, micros, reading, taking] =
        [0, 1, 2, 3, 4, 5].map(|i| SYNCED[i].swap(0, std::sync::atomic::Ordering::Relaxed));
    let making = host::COPIED_MAKING.swap(0, std::sync::atomic::Ordering::Relaxed);
    if pages == 0 {
        return;
    }
    tracing::info!(
        "descriptor records the title copied into its heap as bytes (VICTORY_HEAP_SYNC=0: not looked for): per frame \
         {:.1} changed heap pages, {:.0} slots looked at, {:.1} whose record was new and was mirrored, {:.0} left to the \n         draws that read them through tables (VICTORY_HEAP_SYNC_TABLES=1 mirrors those too), {:.2} ms \
         (reading the pages {:.2} ms, making the mirrored slots {:.2} ms, the rest comparing records; asking which \
         pages changed {:.2} ms of that rest)",
        pages as f64 / frames,
        looked as f64 / frames,
        mirrored as f64 / frames,
        host::COPIED_TABLED.swap(0, std::sync::atomic::Ordering::Relaxed) as f64 / frames,
        micros as f64 / 1000.0 / frames,
        reading as f64 / 1000.0 / frames,
        making as f64 / 1000.0 / frames,
        taking as f64 / 1000.0 / frames
    );
    #[cfg(feature = "diagnostics")]
    {
        let s = host::SYNC_PARTS.each_ref().map(|p| p.swap(0, std::sync::atomic::Ordering::Relaxed) as f64 / frames);
        tracing::info!(
            "of the slots that pass changed per frame: noting {:.2} ms, making {:.2} ms (null views of those left for later \
             included), keeping {:.2} ms; {:.0} made ({:.0} of them texture views), {:.0} left for later, {:.0} kept showing \
             the placed texture's older view; walking the pages' records {:.2} ms, waiting for the slots' stripes {:.2} ms, \
             looking up the views named {:.2} ms",
            s[0] / 1e6,
            s[1] / 1e6,
            s[2] / 1e6,
            s[3],
            s[5],
            s[4],
            s[6],
            s[7] / 1e6,
            s[8] / 1e6,
            s[9] / 1e6
        );
    }
    let [passes, presync_us] = PRESYNCED.each_ref().map(|p| p.swap(0, std::sync::atomic::Ordering::Relaxed) as f64 / frames);
    if passes > 0.0 {
        tracing::info!(
            "of that, ahead of the submissions on a thread of ours (VICTORY_HEAP_PRESYNC=1) per frame: {passes:.1} passes in {:.2} ms",
            presync_us / 1000.0
        );
    }
    let parts = host::MIRROR_PARTS.each_ref().map(|p| p.swap(0, std::sync::atomic::Ordering::Relaxed) as f64 / frames);
    tracing::info!(
        "host slots changed per frame, all threads: {:.0} made ({:.0} of them constant buffer views), {:.0} left for later \
         ({:.0} constant buffer views); {:.2} ms noting buffers and stale views, {:.2} ms making descriptors, {:.2} ms keeping \
         their resources ({:.0} showed the same resource as before); made by draws and the heap pass from records noted at the          title's write without noting them again (VICTORY_DESCRIPTOR_DELTA=1) {:.0}, noted again {:.0}; retired in batches {:.0}; \
         locks of sets left untaken by the slots' bits (VICTORY_SLOT_FLAGS=1) {:.0}",
        parts[3] + parts[5],
        parts[5],
        parts[4] + parts[6],
        parts[6],
        parts[0] / 1e6,
        parts[1] / 1e6,
        parts[2] / 1e6,
        host::KEPT_SAME.swap(0, std::sync::atomic::Ordering::Relaxed) as f64 / frames,
        host::DELTA[0].swap(0, std::sync::atomic::Ordering::Relaxed) as f64 / frames,
        host::DELTA[1].swap(0, std::sync::atomic::Ordering::Relaxed) as f64 / frames,
        host::DELTA_RETIRED.swap(0, std::sync::atomic::Ordering::Relaxed) as f64 / frames,
        host::SLOT_FLAG_SKIPS.swap(0, std::sync::atomic::Ordering::Relaxed) as f64 / frames
    );
    #[cfg(feature = "diagnostics")]
    {
        let made = host::MAKE_PARTS.each_ref().map(|p| p.swap(0, std::sync::atomic::Ordering::Relaxed) as f64 / frames);
        tracing::info!(
            "descriptors made per frame, all threads: {:.0} buffer views (constant ones too), {:.2} ms before the host's view \
             call (finding the host copy, checking the view) and {:.2} ms in it; {:.0} texture views, {:.2} ms before the call \
             and {:.2} ms in it",
            made[2],
            made[0] / 1e6,
            made[1] / 1e6,
            made[5],
            made[3] / 1e6,
            made[4] / 1e6
        );
        let [all, all_ns, again, again_ns, again_sync] =
            host::MADE_AGAIN.each_ref().map(|p| p.swap(0, std::sync::atomic::Ordering::Relaxed) as f64 / frames);
        tracing::info!(
            "slots mirrored and made per frame, all threads: {all:.0} in {:.2} ms; of them {again:.0} from a record another \
             slot shows made already (a descriptor that could be copied from there) in {:.2} ms, {again_sync:.0} of those by \
             the pass over records copied in as bytes",
            all_ns / 1e6,
            again_ns / 1e6
        );
    }
}

/// Whether [`sync_heaps`] hands a changed page's records to the host heap
/// in one go (`host::HostHeap::mirror_copied_page`). On by default: the
/// same records are mirrored either way. `VICTORY_HEAP_SYNC_PAGE=0`: one
/// by one, as before.
#[cfg(windows)]
fn page_at_once() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("VICTORY_HEAP_SYNC_PAGE").is_none_or(|v| v != "0"))
}

/// Title descriptor slots in a changed byte range that have host counterparts.
/// `range_end` is exclusive.
#[cfg(windows)]
fn changed_slots(
    range_from: usize,
    range_end: usize,
    heap_start: usize,
    step: u64,
    mirrored_count: u32,
) -> std::ops::Range<u64> {
    let from = (range_from - heap_start) as u64;
    let to = (range_end - heap_start) as u64;
    let first = from / step;
    let last = to.div_ceil(step).min(u64::from(mirrored_count));
    first.min(last)..last
}

/// Gives back dynamic slots of the host heap behind the title heap at
/// `heap`, if that heap still exists.
#[cfg(windows)]
pub(crate) fn release_dynamic(heap: usize, first: u32, n: u32) {
    let heaps = SHADER_HEAPS.read().unwrap_or_else(|e| e.into_inner());
    if heaps.iter().any(|&(_, _, h)| h == heap)
        // SAFETY: registered heaps are live (removed when dropped).
        && let Some(Some(host)) = unsafe { &*(heap as *const Heap) }.host.get()
    {
        host.release(first, n);
    }
}

#[cfg(windows)]
impl Drop for Heap {
    fn drop(&mut self) {
        let at = std::ptr::from_ref::<Heap>(self) as usize;
        let mut heaps = SHADER_HEAPS.write().unwrap_or_else(|e| e.into_inner());
        heaps.retain(|&(_, _, h)| h != at);
    }
}

/// Bytes per descriptor, by heap type (views, samplers, render targets,
/// depth).
fn increment(kind: u32) -> Option<u32> {
    [32, 16, 128, 128].get(kind as usize).copied()
}

/// Device method.
pub(crate) extern "system" fn increment_size(_: This, kind: u32) -> u32 {
    increment(kind).unwrap_or(0)
}

/// What our render target views hold: the resource and the view
/// description, turned into a host view when a command uses them.
#[repr(C)]
#[derive(Clone, Copy)]
pub(crate) struct RtvRecord {
    tag: u32,
    described: u32,
    pub(crate) resource: This,
    desc: [u32; 6],
}
const RTV_TAG: u32 = u32::from_le_bytes(*b"vRTV");
const _: () = assert!(size_of::<RtvRecord>() <= 112);

impl RtvRecord {
    /// The view description in the host's type (the same layout).
    #[cfg(windows)]
    pub(crate) fn desc(
        &self,
    ) -> Option<windows::Win32::Graphics::Direct3D12::D3D12_RENDER_TARGET_VIEW_DESC> {
        const TEXTURE_2D: u32 = 4;
        self.host_desc(TEXTURE_2D)
    }

    /// The depth view description in the host's type (the same layout).
    #[cfg(windows)]
    pub(crate) fn depth_desc(
        &self,
    ) -> Option<windows::Win32::Graphics::Direct3D12::D3D12_DEPTH_STENCIL_VIEW_DESC> {
        const TEXTURE_2D: u32 = 3;
        self.host_desc(TEXTURE_2D)
    }

    /// The description words as a host type, the format as the host has it.
    /// Without one, a typeless host copy gets a 2D view (`dimension`) in
    /// the title's format, which the host needs spelled out.
    #[cfg(windows)]
    fn host_desc<T>(&self, dimension: u32) -> Option<T> {
        assert!(size_of::<T>() <= size_of::<[u32; 6]>());
        let mut words = if self.described != 0 {
            self.desc
        } else {
            [
                crate::resource::view_format(self.resource)?,
                dimension,
                0,
                0,
                0,
                0,
            ]
        };
        // A target or depth view: of a target, in its stand-in format.
        words[0] = crate::resource::target_format(crate::resource::pc_format(words[0]));
        // SAFETY: both host view descriptions are 24 bytes in this layout.
        Some(unsafe { (&raw const words).cast::<T>().read_unaligned() })
    }
}

/// Device method: records the view at `handle` (descriptor memory).
pub(crate) extern "system" fn create_rtv(
    _: This,
    resource: This,
    desc: *const [u32; 6],
    handle: u64,
) {
    if handle == 0 {
        return;
    }
    // SAFETY: the title passes a description or null.
    let described = unsafe { desc.as_ref() };
    let record = RtvRecord {
        tag: RTV_TAG,
        described: u32::from(described.is_some()),
        resource,
        desc: described.copied().unwrap_or_default(),
    };
    // SAFETY: the handle is a descriptor slot, at least 112 bytes.
    unsafe { (handle as *mut RtvRecord).write_unaligned(record) };
}

/// Device method: a depth view, recorded as render target views are.
pub(crate) extern "system" fn create_dsv(
    this: This,
    resource: This,
    desc: *const [u32; 6],
    handle: u64,
) {
    create_rtv(this, resource, desc, handle);
    if handle != 0 {
        // SAFETY: just written by `create_rtv`.
        unsafe { (handle as *mut u32).write_unaligned(DSV_TAG) };
    }
}
const DSV_TAG: u32 = u32::from_le_bytes(*b"vDSV");

/// A shader view or sampler as the title described it (descriptions in the
/// host's layout, kept as words), for the host to build its own from.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum View {
    /// Constant buffer: GPU address and size.
    Constant(u64, u32),
    /// Shader resource: resource, description (none: the resource's own).
    Shader(usize, Option<[u32; 10]>),
    /// Unordered access: resource, counter resource, description.
    Unordered(usize, usize, Option<[u32; 10]>),
    Sampler([u32; 13]),
    /// The console's own sampler description: flags, filters per axis,
    /// addressing, LOD and anisotropy settings, border color, then
    /// console-only tuning words.
    ConsoleSampler([u32; 27]),
    /// A view of memory without a resource object: whether it is unordered
    /// access, the resource description, then the view's (the plain one
    /// followed by the memory address and console fields).
    Placed(bool, [u32; 14], [u32; 16]),
}

impl View {
    /// Whether the view reads buffer memory (constant buffers, buffer views
    /// of resources or of memory) or memory the title placed a texture in
    /// to sample: such views get host slots only for draws. Placed
    /// unordered texture views live on the host alone, as texture objects'
    /// unordered views do, so shader-visible heap writes mirror them.
    #[cfg(windows)]
    fn is_buffer(&self) -> bool {
        const BUFFER: u32 = 1;
        match self {
            View::Constant(..) | View::Placed(false, ..) => true,
            View::Placed(true, resource, _) => resource[0] == BUFFER,
            View::Shader(r, _) | View::Unordered(r, _, _) => {
                crate::resource::buffer_bytes(*r as This).is_some()
            }
            View::Sampler(_) | View::ConsoleSampler(_) => false,
        }
    }
}

/// Every distinct view in use; a descriptor holds a tag, an index into it
/// and the entry's generation, which survive titles copying descriptors as
/// bytes. A view of a resource lives as long as the resource: when the
/// title releases it, its entries are freed for reuse under a new
/// generation, so descriptors (and host slots made from them) naming the
/// old one go stale instead of naming whatever is made at the same address
/// next.
///
/// Views without a resource object have nothing that releases them, and a
/// title copies descriptors as bytes, so nothing says when the last
/// descriptor naming an entry is gone: such entries were kept for the run.
/// The title writes some 14,000 constant buffer views a frame,
/// each at the address its constants have that frame: 2,450,139 entries
/// after four minutes of play, 1,047 MB with the index, and 11,000 more
/// every second (run 65). So the two kinds that carry an address are not
/// entries any more: their records name them whole ([`constant_words`],
/// [`placed_words`]), and the table holds only what is bounded: views of
/// resource objects (freed with the resource), samplers (as many as there
/// are sampler states), and the shapes of placed views (their descriptions
/// without the address: as many as the title has kinds of textures and
/// buffers, however often it moves them).
/// `VICTORY_VIEW_INLINE=0`: every view is an entry, as before.
struct Views {
    /// Each entry's view and generation.
    list: Vec<(View, u32)>,
    index: std::collections::BTreeMap<View, u32>,
    /// Entries naming each resource.
    by_resource: std::collections::BTreeMap<usize, Vec<u32>>,
    free: Vec<u32>,
    /// The shapes of views of placed memory, and where each is.
    shapes: Vec<Shape>,
    shape_index: std::collections::BTreeMap<Shape, u32>,
}

/// Read far more often than written: every slot a draw reads through a
/// table is looked up here, on several recording threads at once in the
/// world. Readers share it; only a view not seen before (a new table entry
/// or shape) and a released resource take it alone.
static VIEWS: std::sync::RwLock<Views> = std::sync::RwLock::new(Views {
    list: Vec::new(),
    index: std::collections::BTreeMap::new(),
    by_resource: std::collections::BTreeMap::new(),
    free: Vec::new(),
    shapes: Vec::new(),
    shape_index: std::collections::BTreeMap::new(),
});
const VIEW_TAG: u32 = u32::from_le_bytes(*b"vVIE");

/// A record is the tag and two words, `i` and `generation` (the host keeps
/// them as one 64-bit key per slot). The top two bits of `generation` say
/// what the other 62 bits are: 0, entry `i` of the table at that
/// generation; [`CONSTANT`], a constant buffer view, whole; [`PLACED`], a
/// view of placed memory: its address and the number of its shape.
const KIND_SHIFT: u32 = 30;
/// Table generations stay below the kind bits (and above 0).
const GENERATIONS: u32 = (1 << KIND_SHIFT) - 1;
const CONSTANT: u32 = 1;
const PLACED: u32 = 2;
/// Bits of a constant buffer view's address in its record: any address a
/// program can have. The 15 bits above them are its size.
const CONSTANT_ADDRESS_BITS: u32 = 47;
/// Bits of a placed view's address in its record: placed memory is title
/// memory, which lies below 1 TiB. The 22 bits above them number its shape.
const PLACED_ADDRESS_BITS: u32 = 40;
const SHAPES: usize = 1 << (62 - PLACED_ADDRESS_BITS);

/// A view of placed memory without its address: whether it is unordered
/// access, the resource description, and the view's words but the two that
/// hold the address (10 and 11).
type Shape = (bool, [u32; 14], [u32; 14]);

/// Whether constant buffer views and views of placed memory are named whole
/// in their records ([`Views`]). On by default; `VICTORY_VIEW_INLINE=0`
/// makes every view a table entry, as before.
fn inline() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("VICTORY_VIEW_INLINE").is_none_or(|v| v != "0"))
}

/// Views named whole in their records since the start: constant buffer
/// views, views of placed memory (for the memory line).
static NAMED_WHOLE: [std::sync::atomic::AtomicU64; 2] = [const { std::sync::atomic::AtomicU64::new(0) }; 2];

/// The two record words of the 62 bits `payload` of kind `kind`.
fn kind_words(kind: u32, payload: u64) -> [u32; 2] {
    [payload as u32, kind << KIND_SHIFT | (payload >> 32) as u32]
}

/// The 62 bits two record words carry beside their kind.
fn payload(i: u32, generation: u32) -> u64 {
    u64::from(generation & GENERATIONS) << 32 | u64::from(i)
}

/// The record words that name the constant buffer view of `size` bytes at
/// `address` whole; `None` if they cannot: an address past 47 bits, or a
/// size of 16 KiB or more that is no multiple of 256 or reaches 4 MiB (the
/// size has 15 bits: bytes below 16 KiB, else units of 256 bytes). Such a
/// view is a table entry.
fn constant_words(address: u64, size: u32) -> Option<[u32; 2]> {
    if address >> CONSTANT_ADDRESS_BITS != 0 {
        return None;
    }
    let code = if size < 1 << 14 {
        u64::from(size)
    } else if size.is_multiple_of(256) && size >> 8 < 1 << 14 {
        1 << 14 | u64::from(size >> 8)
    } else {
        return None;
    };
    Some(kind_words(CONSTANT, code << CONSTANT_ADDRESS_BITS | address))
}

/// The constant buffer view [`constant_words`] named.
fn constant_of(i: u32, generation: u32) -> View {
    let payload = payload(i, generation);
    let code = (payload >> CONSTANT_ADDRESS_BITS) as u32;
    let size = if code >> 14 == 0 { code } else { (code & 0x3fff) << 8 };
    View::Constant(payload & ((1 << CONSTANT_ADDRESS_BITS) - 1), size)
}

/// A placed view's shape and its address.
fn shape_of(unordered: bool, resource: &[u32; 14], desc: &[u32; 16]) -> (Shape, u64) {
    let mut resource = *resource;
    let mut rest = [0u32; 14];
    rest[..10].copy_from_slice(&desc[..10]);
    rest[10..].copy_from_slice(&desc[12..]);
    if shape_padding_out() {
        padding_out(unordered, &mut resource, &mut rest);
    }
    (
        (unordered, resource, rest),
        u64::from(desc[10]) | u64::from(desc[11]) << 32,
    )
}

/// Whether the words of a placed view that hold no field are left out of
/// its shape (`VICTORY_SHAPE_PADDING=1`; off by default).
///
/// Run 208: of the newest 20,000 shapes, besides the words that do differ
/// (the resource's width, the view's first element and element count),
/// words 1 and 13 of the resource's description and words 3 and 9 of the
/// view took 170 to 1,490 values each. In `D3D12_RESOURCE_DESC` words 1
/// and 13 are padding (after the dimension, and at the end); in
/// `D3D12_SHADER_RESOURCE_VIEW_DESC` word 3 is the padding before the
/// union; the union's words a view's dimension does not use hold whatever
/// the title's stack held. The title's bytes go there unchanged, so the
/// same kind of view became new shapes: 139,121 in run 208's seven
/// minutes, 365,806 in play3's nine (123 MB), and the table only grows.
/// The host reads none of those words (`srv_desc`, `uav_desc` and
/// `crate::resource` read the fields), so the view made from a shape is
/// the same.
fn shape_padding_out() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("VICTORY_SHAPE_PADDING").is_some_and(|v| v == "1"))
}

/// Zeroes the words of a placed view's resource description and view
/// description (`rest`: the view's words 0-9 and 12-15) that hold no
/// field ([`shape_padding_out`]).
fn padding_out(unordered: bool, resource: &mut [u32; 14], rest: &mut [u32; 14]) {
    // D3D12_RESOURCE_DESC: dimension, padding, alignment (2), width (2),
    // height, depth and levels, format, samples (2), layout, flags, padding.
    resource[1] = 0;
    resource[13] = 0;
    // The view's union: where it starts and how many of its words the
    // view's dimension uses (`None`: a dimension not known here, left as it is).
    let (first, used) = if unordered {
        // D3D12_UNORDERED_ACCESS_VIEW_DESC: format, dimension, union from
        // word 2 (buffer: first element (2), elements, stride, counter
        // offset (2), flags).
        let used = match rest[1] {
            1 => Some(7),
            2 | 6 => Some(1),
            3 | 8 => Some(3),
            4 | 7 => Some(2),
            5 => Some(4),
            _ => None,
        };
        (2, used)
    } else {
        // D3D12_SHADER_RESOURCE_VIEW_DESC: format, dimension, mapping,
        // padding, union from word 4 (buffer: first element (2), elements,
        // stride, flags).
        rest[3] = 0;
        let used = match rest[1] {
            1 | 3 | 10 => Some(5),
            2 | 8 | 9 => Some(3),
            4 => Some(4),
            5 => Some(6),
            6 => Some(1),
            7 | 11 => Some(2),
            _ => None,
        };
        (4, used)
    };
    if let Some(used) = used {
        for w in rest.iter_mut().take(10).skip(first + used) {
            *w = 0;
        }
    }
}

/// The record words that name the view of placed memory at `address` with
/// shape number `shape`.
fn placed_words(shape: u32, address: u64) -> [u32; 2] {
    kind_words(PLACED, u64::from(shape) << PLACED_ADDRESS_BITS | address)
}

impl Views {
    /// The number of `shape`, a new one if it is not there yet; `None` when
    /// the shapes are full (the view is then a table entry).
    fn shape(&mut self, shape: Shape) -> Option<u32> {
        if let Some(&at) = self.shape_index.get(&shape) {
            return Some(at);
        }
        if self.shapes.len() >= SHAPES {
            return None;
        }
        let at = self.shapes.len() as u32;
        self.shapes.push(shape);
        self.shape_index.insert(shape, at);
        Some(at)
    }

    /// The view of placed memory [`placed_words`] named.
    fn placed_of(&self, i: u32, generation: u32) -> Option<View> {
        let payload = payload(i, generation);
        let address = payload & ((1 << PLACED_ADDRESS_BITS) - 1);
        let &(unordered, resource, rest) = self.shapes.get((payload >> PLACED_ADDRESS_BITS) as usize)?;
        let mut desc = [0u32; 16];
        desc[..10].copy_from_slice(&rest[..10]);
        (desc[10], desc[11]) = (address as u32, (address >> 32) as u32);
        desc[12..].copy_from_slice(&rest[10..]);
        Some(View::Placed(unordered, resource, desc))
    }

    /// The record words of `view` as a table entry, if it has one.
    fn entry_of(&self, view: &View) -> Option<[u32; 2]> {
        let &i = self.index.get(view)?;
        Some([i, self.list[i as usize].1])
    }

    /// The record words of `view` as a table entry: its entry (a new or a
    /// freed one if the view is not there yet) and that entry's generation.
    fn entry(&mut self, view: View) -> [u32; 2] {
        let i = match self.index.get(&view) {
            Some(&i) => i,
            None => {
                let i = match self.free.pop() {
                    Some(i) => {
                        self.list[i as usize].0 = view.clone();
                        i
                    }
                    None => {
                        self.list.push((view.clone(), 1));
                        self.list.len() as u32 - 1
                    }
                };
                for r in view.resources().into_iter().filter(|&r| r != 0) {
                    self.by_resource.entry(r).or_default().push(i);
                }
                self.index.insert(view, i);
                i
            }
        };
        [i, self.list[i as usize].1]
    }
}

/// The two words that name `view` in a record: the view itself where its
/// kind allows ([`Views`]), else its table entry.
fn words_for(view: View) -> [u32; 2] {
    use std::sync::atomic::Ordering::Relaxed;
    if inline() {
        match &view {
            View::Constant(address, size) => {
                if let Some(words) = constant_words(*address, *size) {
                    NAMED_WHOLE[0].fetch_add(1, Relaxed);
                    return words;
                }
            }
            View::Placed(unordered, resource, desc) => {
                let (shape, address) = shape_of(*unordered, resource, desc);
                if address >> PLACED_ADDRESS_BITS == 0 {
                    // Nearly every placed view has a shape seen before.
                    let known = views_read().shape_index.get(&shape).copied();
                    let shape = known.or_else(|| views_write().shape(shape));
                    if let Some(shape) = shape {
                        NAMED_WHOLE[1].fetch_add(1, Relaxed);
                        return placed_words(shape, address);
                    }
                }
            }
            _ => {}
        }
    }
    // A view written before keeps its entry and generation.
    if let Some(words) = views_read().entry_of(&view) {
        return words;
    }
    views_write().entry(view)
}

fn views_read() -> std::sync::RwLockReadGuard<'static, Views> {
    VIEWS.read().unwrap_or_else(|e| e.into_inner())
}

fn views_write() -> std::sync::RwLockWriteGuard<'static, Views> {
    VIEWS.write().unwrap_or_else(|e| e.into_inner())
}

/// For the memory line: the distinct views kept ([`Views`]), those of them
/// free for reuse, and the bytes the table and its index hold for them (an
/// estimate: an entry of the table, and one of the index with its share of
/// a tree node; the shapes likewise); then the entries that are constant
/// buffer views and views of placed memory (those that did not fit a
/// record, or all of them with `VICTORY_VIEW_INLINE=0`), the shapes of
/// placed views kept, and the views named whole in their records since the
/// start: constant buffer views, views of placed memory.
#[cfg(all(windows, feature = "diagnostics"))]
pub(crate) fn views_kept() -> [u64; 8] {
    use std::sync::atomic::Ordering::Relaxed;
    let views = views_read();
    let each = size_of::<(View, u32)>() as u64;
    let shape = size_of::<Shape>() as u64;
    let (mut constants, mut placed) = (0, 0);
    for (view, _) in &views.list {
        match view {
            View::Constant(..) => constants += 1,
            View::Placed(..) => placed += 1,
            _ => {}
        }
    }
    [
        views.list.len() as u64,
        views.free.len() as u64,
        views.list.capacity() as u64 * each
            + views.index.len() as u64 * each * 3 / 2
            + views.shapes.capacity() as u64 * shape
            + views.shape_index.len() as u64 * shape * 3 / 2,
        constants,
        placed,
        views.shapes.len() as u64,
        NAMED_WHOLE[0].load(Relaxed),
        NAMED_WHOLE[1].load(Relaxed),
    ]
}

/// For the memory line (item 2: the shapes of placed views grew from
/// 361,576 to 365,806 in play3's last minutes, and the table they are in
/// from 3 to 123 MB in nine): of the newest `last` shapes, the word
/// positions whose values differ most among them, as (position, distinct
/// values), most first: positions `r0`..`r13` of the resource's
/// description, `v0`..`v13` of the view's words without its address. A
/// shape is meant to be a kind of texture or buffer; a position that takes
/// as many values as there are shapes is a word that makes the same kind
/// a new shape (padding, or a union's words the view's kind does not use).
#[cfg(all(windows, feature = "diagnostics"))]
pub(crate) fn shape_word_census(last: usize) -> (usize, Vec<(String, usize)>) {
    let views = views_read();
    let from = views.shapes.len().saturating_sub(last);
    let newest = &views.shapes[from..];
    let mut out = Vec::new();
    for i in 0..28 {
        let distinct: std::collections::HashSet<u32> =
            newest.iter().map(|(_, resource, rest)| if i < 14 { resource[i] } else { rest[i - 14] }).collect();
        let name = if i < 14 { format!("r{i}") } else { format!("v{}", i - 14) };
        out.push((name, distinct.len()));
    }
    out.sort_by_key(|(_, n)| std::cmp::Reverse(*n));
    out.truncate(6);
    (newest.len(), out)
}

impl View {
    /// The resource objects the view names.
    fn resources(&self) -> [usize; 2] {
        match *self {
            View::Shader(r, _) => [r, 0],
            View::Unordered(r, counter, _) => [r, counter],
            _ => [0, 0],
        }
    }

    /// The texture object the view is of, if that texture has no host copy
    /// yet (`crate::resource::host_waits`): a slot written with such a
    /// view is left null until the texture is first used.
    #[cfg(windows)]
    fn waiting_texture(&self) -> Option<usize> {
        match *self {
            View::Shader(r, _) | View::Unordered(r, _, _)
                if crate::resource::host_waits(r as This) =>
            {
                Some(r)
            }
            _ => None,
        }
    }
}

/// Title slots (their handles) written with a view of a texture object
/// that had no host copy then, by texture. A title texture gets its host
/// copy on first use (`resource.rs` `Hosted`), and the title
/// writes each texture's views into its view heap when it creates the
/// texture: making those host slots would make every host copy at once.
/// They are made when the texture is: translated shaders index the heap
/// directly, so a slot is not always made again by the draw that reads it.
#[cfg(windows)]
static WAITING: std::sync::Mutex<std::collections::BTreeMap<usize, Vec<u64>>> =
    std::sync::Mutex::new(std::collections::BTreeMap::new());

/// Textures given their host copy since [`make_waiting`] last ran.
#[cfg(windows)]
static MADE_LATE: std::sync::Mutex<Vec<usize>> = std::sync::Mutex::new(Vec::new());

/// Title texture `resource` was given its host copy (called under that
/// texture's own lock: nothing else is locked here).
#[cfg(windows)]
pub(crate) fn texture_made(resource: usize) {
    MADE_LATE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .push(resource);
}

/// The slot at `handle` was written with a view of texture `resource`,
/// which has no host copy: it waits for one.
#[cfg(windows)]
fn wait_for(resource: usize, handle: u64) {
    /// A texture with more slots waiting than this is in use for all we
    /// know (its views are being copied about): its host copy is made.
    const MOST: usize = 64;
    let many = {
        let mut waiting = WAITING.lock().unwrap_or_else(|e| e.into_inner());
        let slots = waiting.entry(resource).or_default();
        if !slots.contains(&handle) {
            slots.push(handle);
        }
        slots.len() > MOST
    };
    if many {
        // Makes it, and notes it made.
        let _ = crate::resource::host_copy(resource as This);
    } else if !crate::resource::host_waits(resource as This) {
        // Made by another thread since the slot was looked at: that making
        // may have been taken by `make_waiting` before this slot was noted.
        texture_made(resource);
    }
}

/// Makes the host slots that waited for a texture's host copy, for the
/// textures made since the last call. Called before a submission runs, by
/// the thread that submits: a slot is read when a list runs.
#[cfg(windows)]
pub(crate) fn make_waiting() {
    let made = std::mem::take(&mut *MADE_LATE.lock().unwrap_or_else(|e| e.into_inner()));
    for resource in made {
        let handles = WAITING
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&resource);
        for handle in handles.into_iter().flatten() {
            // Only if the slot still holds a view of that texture: the
            // title may have written it since (and a view of another
            // texture without a host copy must go on waiting).
            // (Looked up first: the handle is read only while its heap is
            // there.)
            let Some((heap, index)) = shader_slot(handle) else {
                continue;
            };
            let same = match view(handle) {
                Some(View::Shader(r, _) | View::Unordered(r, _, _)) => r == resource,
                _ => false,
            };
            if same && let Some(Some(host)) = heap.host.get() {
                host.mirror(index, handle, true);
            }
        }
    }
}

fn record(handle: u64, view: View) {
    if handle == 0 {
        return;
    }
    let [i, generation] = words_for(view);
    // SAFETY: the handle is a descriptor slot, at least 16 bytes.
    unsafe { (handle as *mut [u32; 3]).write_unaligned([VIEW_TAG, i, generation]) };
    #[cfg(windows)]
    mirror(handle);
}

/// The title released `resource`: the views naming it go stale.
pub(crate) fn forget(resource: usize) {
    #[cfg(windows)]
    WAITING
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(&resource);
    let mut views = views_write();
    let Some(entries) = views.by_resource.remove(&resource) else {
        return;
    };
    for i in entries {
        let (view, generation) = views.list[i as usize].clone();
        // Entries freed through a view's other resource may hold another
        // view by now.
        if views.index.get(&view) != Some(&i) || !view.resources().contains(&resource) {
            continue;
        }
        views.index.remove(&view);
        // (Below the bits that say a record is no table entry.)
        views.list[i as usize].1 = ((generation + 1) & GENERATIONS).max(1);
        // The view's other resource (a counter) keeps a stale entry, which
        // `forget` of that resource skips.
        views.free.push(i);
    }
}

/// The view the record words `i` and `generation` name: the one they hold
/// whole, or the view of entry `i` if `generation` is still its own.
fn view_at(i: u32, generation: u32) -> Option<View> {
    match generation >> KIND_SHIFT {
        0 => {
            let views = views_read();
            let (view, current) = views.list.get(i as usize)?;
            (*current == generation).then(|| view.clone())
        }
        CONSTANT => Some(constant_of(i, generation)),
        PLACED => views_read().placed_of(i, generation),
        _ => None,
    }
}

/// The view recorded at `handle`, if one is (and its resource is alive).
pub(crate) fn view(handle: u64) -> Option<View> {
    if handle == 0 {
        return None;
    }
    // SAFETY: the handle is a descriptor slot the title passes back.
    let [tag, i, generation] = unsafe { (handle as *const [u32; 3]).read_unaligned() };
    if tag != VIEW_TAG {
        return None;
    }
    view_at(i, generation)
}

/// The view recorded at `handle`, and what says which record that is
/// (`host::HostHeap::publish`).
#[cfg(windows)]
pub(crate) fn view_and_key(handle: u64) -> Option<(View, u64)> {
    if handle == 0 {
        return None;
    }
    // SAFETY: as in `view`.
    let [tag, i, generation] = unsafe { (handle as *const [u32; 3]).read_unaligned() };
    if tag != VIEW_TAG {
        return None;
    }
    Some((view_at(i, generation)?, u64::from(generation) << 32 | u64::from(i)))
}

/// A description of `N` words, if the title passes one.
fn words<const N: usize>(desc: *const [u32; N]) -> Option<[u32; N]> {
    // SAFETY: the title passes a description or null.
    (!desc.is_null()).then(|| unsafe { desc.read_unaligned() })
}

/// Device method.
pub(crate) extern "system" fn create_cbv(_: This, desc: *const [u32; 4], handle: u64) {
    if let Some([lo, hi, size, _]) = words(desc) {
        let address = u64::from(lo) | u64::from(hi) << 32;
        record(handle, View::Constant(address, size));
    }
}

/// Device method.
pub(crate) extern "system" fn create_srv(
    _: This,
    resource: This,
    desc: *const [u32; 10],
    handle: u64,
) {
    record(handle, View::Shader(resource as usize, words(desc)));
}

/// Device method.
pub(crate) extern "system" fn create_uav(
    _: This,
    resource: This,
    counter: This,
    desc: *const [u32; 10],
    handle: u64,
) {
    let view = View::Unordered(resource as usize, counter as usize, words(desc));
    record(handle, view);
}

/// Device methods 0x230 and 0x238: shader resource and unordered access
/// views of memory the title placed itself, `(resource desc, view desc,
/// handle)`.
pub(crate) extern "system" fn create_placed_srv(
    _: This,
    resource: *const [u32; 14],
    desc: *const [u32; 16],
    handle: u64,
) {
    if let (Some(resource), Some(desc)) = (words(resource), words(desc)) {
        record(handle, View::Placed(false, resource, desc));
    }
}

pub(crate) extern "system" fn create_placed_uav(
    _: This,
    resource: *const [u32; 14],
    desc: *const [u32; 16],
    handle: u64,
) {
    if let (Some(resource), Some(desc)) = (words(resource), words(desc)) {
        record(handle, View::Placed(true, resource, desc));
    }
}

/// Device method.
pub(crate) extern "system" fn create_sampler(_: This, desc: *const [u32; 13], handle: u64) {
    if let Some(desc) = words(desc) {
        record(handle, View::Sampler(desc));
    }
}

/// Device method 0x1e8.
pub(crate) extern "system" fn create_sampler_x(_: This, desc: *const [u32; 27], handle: u64) {
    if let Some(desc) = words(desc) {
        record(handle, View::ConsoleSampler(desc));
    }
}

/// Device method: `count` descriptors of `kind` from `src` to `dst`.
pub(crate) extern "system" fn copy_simple(_: This, count: u32, dst: u64, src: u64, kind: u32) {
    let step = u64::from(increment(kind).unwrap_or(0));
    let len = u64::from(count) * step;
    if dst != 0 && src != 0 {
        // SAFETY: both ranges are descriptor slots the title owns.
        unsafe { std::ptr::copy(src as *const u8, dst as *mut u8, len as usize) };
        #[cfg(windows)]
        for i in 0..u64::from(count) {
            mirror(dst + i * step);
        }
    }
}

/// Device method: ranges of descriptors (start and count each) gathered
/// from the sources and scattered to the destinations, in order.
#[allow(clippy::too_many_arguments)] // the method's own arguments
pub(crate) extern "system" fn copy(
    _: This,
    dst_ranges: u32,
    dst_starts: *const u64,
    dst_sizes: *const u32,
    src_ranges: u32,
    src_starts: *const u64,
    src_sizes: *const u32,
    kind: u32,
) {
    let Some(step) = increment(kind) else {
        return;
    };
    // Each range as (start, count); a null size array means one descriptor
    // per range.
    let ranges = |n: u32, starts: *const u64, sizes: *const u32| -> Vec<(u64, u32)> {
        (0..n as usize)
            .map(|i| {
                // SAFETY: the title passes `n` starts and sizes (or null sizes).
                unsafe {
                    (
                        *starts.add(i),
                        if sizes.is_null() { 1 } else { *sizes.add(i) },
                    )
                }
            })
            .collect()
    };
    if dst_starts.is_null() || src_starts.is_null() {
        return;
    }
    let src = ranges(src_ranges, src_starts, src_sizes);
    let mut dst = ranges(dst_ranges, dst_starts, dst_sizes).into_iter();
    let (mut to, mut room) = dst.next().unwrap_or((0, 0));
    for (mut from, mut left) in src {
        while left > 0 {
            while room == 0 {
                let Some(next) = dst.next() else { return };
                (to, room) = next;
            }
            let n = left.min(room);
            copy_simple(std::ptr::null(), n, to, from, kind);
            let bytes = u64::from(n) * u64::from(step);
            (to, from, room, left) = (to + bytes, from + bytes, room - n, left - n);
        }
    }
}

/// The view recorded at `handle`, if one is.
pub(crate) fn rtv_record(handle: u64) -> Option<RtvRecord> {
    tagged_record(handle, RTV_TAG)
}

/// The depth view recorded at `handle`, if one is.
pub(crate) fn dsv_record(handle: u64) -> Option<RtvRecord> {
    tagged_record(handle, DSV_TAG)
}

fn tagged_record(handle: u64, tag: u32) -> Option<RtvRecord> {
    if handle == 0 {
        return None;
    }
    // SAFETY: the handle is a descriptor slot the title passes back.
    let record = unsafe { (handle as *const RtvRecord).read_unaligned() };
    (record.tag == tag).then_some(record)
}

/// Device method.
pub(crate) extern "system" fn create_heap(
    _: This,
    desc_ptr: *const HeapDesc,
    iid: *const Iid,
    out: *mut This,
) -> HRESULT {
    // SAFETY: the title passes a description (null is tolerated).
    let Some(&desc) = (unsafe { desc_ptr.as_ref() }) else {
        return E_INVALIDARG;
    };
    let Some(size) = increment(desc.kind).map(|i| u64::from(i) * u64::from(desc.count.max(1)))
    else {
        return E_INVALIDARG;
    };
    // SAFETY: read only as a hint: a title that passes the plain
    // description has its own stack or data after it.
    let placement = unsafe { desc_ptr.add(1).cast::<Placement>().read_unaligned() };
    // shortcut: a placement is recognised by its size matching the count,
    // since the plain and extended descriptions share the slot and the
    // interface version; find the flag that tells them apart if a title
    // trips over this.
    let placed = placement.address != 0 && placement.size == size;
    tracing::debug!(?desc, placed, "descriptor heap");
    let (start, memory) = if placed {
        (placement.address, None)
    } else {
        match Memory::zeroed(size, 64) {
            Ok(memory) => (memory.address(), Some(memory)),
            Err(hr) => return hr,
        }
    };
    let visible = desc.flags & SHADER_VISIBLE != 0 && matches!(desc.kind, VIEWS_KIND | SAMPLERS);
    let heap = Box::new(Heap {
        object: Object::new(&FUNCTIONS),
        desc,
        start,
        memory,
        #[cfg(windows)]
        host: std::sync::OnceLock::new(),
        #[cfg(windows)]
        pending: Default::default(),
        #[cfg(windows)]
        written: (placed && visible)
            .then(|| crate::watch::track(start as usize, size as usize, true))
            .flatten(),
    });
    #[cfg(windows)]
    if visible {
        let at = std::ptr::from_ref::<Heap>(&heap) as usize;
        let mut heaps = SHADER_HEAPS.write().unwrap_or_else(|e| e.into_inner());
        heaps.push((start, start + size, at));
    }
    #[cfg(not(windows))]
    let _ = visible;
    object::hand_out(DESCRIPTOR_HEAP, heap, iid, &HEAP_IIDS, out)
}

extern "system" fn query(this: This, iid: *const Iid, out: *mut This) -> HRESULT {
    let found = object::is(iid, &HEAP_IIDS).then_some(this);
    object::answer(DESCRIPTOR_HEAP, iid, found, out)
}
extern "system" fn release(this: This) -> u32 {
    object::release_boxed::<Heap>(this)
}
extern "system" fn desc(this: This, out: *mut HeapDesc) {
    if !out.is_null() {
        // SAFETY: checked non-null; the title passes room for a description.
        unsafe { out.write(heap(this).desc) };
    }
}
/// The same address for the CPU and the GPU start.
extern "system" fn start(this: This) -> u64 {
    heap(this).start
}

/// IUnknown 3, object 4, device child 1, heap 3.
static FUNCTIONS: Table<24> = {
    let mut f = object::d3d12_object(object::missing_table::<DESCRIPTOR_HEAP, 24>());
    f[0] = query as *const ();
    f[2] = release as *const ();
    f[7] = object::parent_device as *const (); // 0x38
    f[8] = desc as *const (); // 0x40
    f[9] = start as *const (); // 0x48 CPU
    f[10] = start as *const (); // 0x50 GPU
    Table(f)
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::object::{S_OK, slot};

    #[test]
    fn constant_buffer_staging_uses_only_the_host_addressable_prefix() {
        for (size, expected) in [
            (0, 0),
            (255, 255),
            (65_535, 65_535),
            (65_536, 65_536),
            (65_537, 65_536),
            (2_785_536, 65_536),
            (u32::MAX, 65_536),
        ] {
            assert_eq!(constant_staging_bytes(size), expected, "guest CBV size {size}");
        }
    }

    /// What newer titles pass; tests use it throughout, so that reading the
    /// placement stays inside the value.
    #[repr(C)]
    struct Extended(HeapDesc, Placement);

    fn create(desc: HeapDesc, placement: Placement, out: &mut This) -> HRESULT {
        let extended = Extended(desc, placement);
        create_heap(std::ptr::null(), &extended.0, &HEAP_IIDS[0], out)
    }

    #[cfg(windows)]
    #[test]
    fn raw_heap_sync_stops_before_dynamic_host_slots() {
        let start = 0x1000usize;
        let step = 32;
        let capped = 5;
        assert_eq!(
            changed_slots(
                start + 3 * step + 4,
                start + 4 * step + 8,
                start,
                step as u64,
                capped
            ),
            3..5
        );
        assert_eq!(
            changed_slots(
                start + 6 * step,
                start + 9 * step,
                start,
                step as u64,
                capped
            ),
            5..5
        );
    }

    /// A table made afresh for a draw holds the descriptors its shader
    /// moves halves through (`HostShader::half_views`), also past the range
    /// starts its used-resource entries give: with one entry for a range
    /// from dword 0 and a 16-bit float buffer view at dword 8, the table
    /// takes two dynamic slots, so the next allocation does not land on the
    /// view's. (The case avt0m4t48 found in review on #927.)
    #[cfg(windows)]
    #[test]
    fn a_fresh_table_holds_the_half_views_past_its_range_starts() {
        use victory_shader::{ResourceType, RootType, UsedResource};
        let Some(device) = crate::host::host() else {
            return; // no host GPU
        };
        let list = device.new_list().expect("a host list");
        let desc = HeapDesc {
            kind: VIEWS_KIND,
            count: 2,
            flags: SHADER_VISIBLE,
            nodes: 1,
        };
        let none = Placement {
            address: 0,
            size: 0,
        };
        let mut h: This = std::ptr::null();
        assert_eq!(create(desc, none, &mut h), S_OK);
        let title = heap(h);
        let host_heap = title.host().expect("a host view heap");
        let (first, second) = (title.handle(0).unwrap(), title.handle(1).unwrap());
        // An R16G16B16A16_FLOAT buffer view of memory in the second slot.
        let memory = [0u64; 64];
        let address = memory.as_ptr() as u64;
        let resource = [1, 0, 0, 0, 512, 0, 1, 0x1_0001, 0, 1, 0, 1, 0, 0];
        let mut words = [0u32; 16];
        words[..10].copy_from_slice(&[10, 1, 5768, 0, 0, 0, 64, 0, 0, 0]);
        words[10] = address as u32;
        words[11] = (address >> 32) as u32;
        create_placed_srv(std::ptr::null(), &resource, &words, second);
        assert!(view(second).as_ref().and_then(host::half_view).is_some());
        let mut state = crate::draw::State::default();
        state.set_heaps(&[h]);
        state.set_table(true, 1, first);
        let range = UsedResource {
            kind: ResourceType::Srv,
            root: RootType::Table,
            table_offset_dwords: 0,
            lower_bound: 0,
            stride: 8,
            dwords: 4,
            user_data: 4,
            slot: 1,
        };
        let made = state.tables(&list, &[range], &[], &[(1, 8)], true);
        let [(1, offset)] = made[..] else {
            panic!("one fresh table for root parameter 1, not {made:?}");
        };
        assert_ne!(offset, u32::MAX, "the table found no dynamic slots");
        let next = host_heap.dynamic(1).expect("a dynamic slot");
        host_heap.release(next, 1);
        drop(state);
        slot::<extern "system" fn(This) -> u32>(h, 0x10)(h);
        assert_eq!(
            next,
            offset / 32 + 2,
            "the next allocation took the table's second slot"
        );
    }

    #[test]
    fn heaps_hold_count_descriptors_from_their_start() {
        let null = std::ptr::null();
        let desc = HeapDesc {
            kind: 0,
            count: 1000,
            flags: 1,
            nodes: 0,
        };
        let none = Placement {
            address: 0,
            size: 0,
        };
        let mut h = null;
        assert_eq!(create(desc, none, &mut h), S_OK);
        let cpu = slot::<extern "system" fn(This) -> u64>(h, 0x48)(h);
        assert_eq!(cpu % 64, 0);
        assert_eq!(slot::<extern "system" fn(This) -> u64>(h, 0x50)(h), cpu);
        assert_eq!(increment_size(null, 0), 32);
        // SAFETY: the heap holds 1000 descriptors of 32 bytes.
        unsafe { (cpu as *mut u8).add(999 * 32 + 31).write(1) };
        assert_eq!(increment_size(null, 9), 0);
        let bad = HeapDesc { kind: 9, ..desc };
        assert_eq!(create(bad, none, &mut h), E_INVALIDARG);
    }

    /// A descriptor copied into a placed heap as bytes is mirrored before
    /// the next submission, even though no descriptor method saw the copy.
    #[cfg(windows)]
    #[test]
    fn records_copied_as_bytes_are_made_at_the_next_sync() {
        use windows_sys::Win32::System::Memory::{
            MEM_COMMIT, MEM_RELEASE, MEM_RESERVE, PAGE_READWRITE, VirtualAlloc, VirtualFree,
        };
        const MEM_WRITE_WATCH: u32 = 0x20_0000;
        let memory = unsafe {
            VirtualAlloc(
                std::ptr::null(),
                crate::watch::PAGE,
                MEM_RESERVE | MEM_COMMIT | MEM_WRITE_WATCH,
                PAGE_READWRITE,
            )
        };
        assert!(!memory.is_null());
        let desc = HeapDesc {
            kind: SAMPLERS,
            count: 64,
            flags: SHADER_VISIBLE,
            nodes: 1,
        };
        let placement = Placement {
            address: memory as u64,
            size: 64 * 16,
        };
        let mut h: This = std::ptr::null();
        assert_eq!(create(desc, placement, &mut h), S_OK);
        if let Some(host) = heap(h).host() {
            let mut elsewhere = [0u64; 2];
            let source = elsewhere.as_mut_ptr() as u64;
            let sampler = [21, 3, 3, 3, 0, 1, 0, 0, 0, 0, 0, 0x7f7f_ffff, 0];
            create_sampler(std::ptr::null(), &sampler, source);
            // SAFETY: the heap slot and source both hold 16 descriptor bytes.
            unsafe {
                std::ptr::copy_nonoverlapping(
                    source as *const u8,
                    memory.cast::<u8>().add(5 * 16),
                    16,
                )
            };
            assert!(!host.made(5));
            sync_heaps();
            assert!(host.made(5));
        }
        slot::<extern "system" fn(This) -> u32>(h, 0x10)(h);
        // SAFETY: this allocation belongs to the test.
        unsafe { VirtualFree(memory, 0, MEM_RELEASE) };
    }

    /// With `VICTORY_HEAP_PRESYNC`, a record copied in as bytes is made by the
    /// thread that runs the pass ahead of the submissions, woken as a
    /// submission wakes it, with no pass of the caller's own.
    #[cfg(windows)]
    #[test]
    fn records_copied_as_bytes_are_made_by_the_pass_ahead() {
        use windows_sys::Win32::System::Memory::{
            MEM_COMMIT, MEM_RELEASE, MEM_RESERVE, PAGE_READWRITE, VirtualAlloc, VirtualFree,
        };
        const MEM_WRITE_WATCH: u32 = 0x20_0000;
        // SAFETY: a fresh allocation of ours, freed at the end.
        let memory = unsafe {
            VirtualAlloc(std::ptr::null(), crate::watch::PAGE, MEM_RESERVE | MEM_COMMIT | MEM_WRITE_WATCH, PAGE_READWRITE)
        };
        assert!(!memory.is_null());
        let desc = HeapDesc { kind: SAMPLERS, count: 64, flags: SHADER_VISIBLE, nodes: 1 };
        let placement = Placement { address: memory as u64, size: 64 * 16 };
        let mut h: This = std::ptr::null();
        assert_eq!(create(desc, placement, &mut h), S_OK);
        if let Some(host) = heap(h).host() {
            let mut elsewhere = [0u64; 2];
            let source = elsewhere.as_mut_ptr() as u64;
            let sampler = [21, 3, 3, 3, 0, 1, 0, 0, 0, 0, 0, 0x7f7f_ffff, 0];
            create_sampler(std::ptr::null(), &sampler, source);
            // SAFETY: the heap slot and source both hold 16 descriptor bytes.
            unsafe { std::ptr::copy_nonoverlapping(source as *const u8, memory.cast::<u8>().add(7 * 16), 16) };
            assert!(!host.made(7));
            crate::live::set("HEAP_PRESYNC", 1).unwrap();
            presync_kick();
            let began = std::time::Instant::now();
            while !host.made(7) && began.elapsed() < std::time::Duration::from_secs(2) {
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            crate::live::reset("HEAP_PRESYNC");
            assert!(host.made(7), "made by the pass ahead");
        }
        slot::<extern "system" fn(This) -> u32>(h, 0x10)(h);
        // SAFETY: this allocation belongs to the test.
        unsafe { VirtualFree(memory, 0, MEM_RELEASE) };
    }

    /// With `VICTORY_SYNC_KEYS`, the pass over records copied in as bytes
    /// mirrors a record when the view it names changes, and not when only
    /// its other bytes do or one record naming no view takes another's
    /// place; a record seen the other way before the switch flipped is no
    /// change after it, either way.
    #[cfg(windows)]
    #[test]
    fn records_told_by_their_keys_are_mirrored_when_their_view_changes() {
        use windows_sys::Win32::System::Memory::{MEM_COMMIT, MEM_RELEASE, MEM_RESERVE, PAGE_READWRITE, VirtualAlloc, VirtualFree};
        // SAFETY: a fresh allocation of ours, freed at the end.
        let memory = unsafe { VirtualAlloc(std::ptr::null(), crate::watch::PAGE, MEM_RESERVE | MEM_COMMIT, PAGE_READWRITE) };
        assert!(!memory.is_null());
        let desc = HeapDesc { kind: SAMPLERS, count: 64, flags: SHADER_VISIBLE, nodes: 1 };
        let placement = Placement { address: memory as u64, size: 64 * 16 };
        let mut h: This = std::ptr::null();
        assert_eq!(create(desc, placement, &mut h), S_OK);
        if let Some(host) = heap(h).host() {
            let mut sources = [[0u64; 2]; 2];
            for (source, filter) in sources.iter_mut().zip([21, 20]) {
                let sampler = [filter, 3, 3, 3, 0, 1, 0, 0, 0, 0, 0, 0x7f7f_ffff, 0];
                create_sampler(std::ptr::null(), &sampler, source.as_mut_ptr() as u64);
            }
            // SAFETY: each source holds the 16 bytes of a record.
            let [a, b]: [[u8; 16]; 2] = sources.map(|s| unsafe { std::mem::transmute::<[u64; 2], [u8; 16]>(s) });
            let mut a_other = a;
            a_other[12..].copy_from_slice(&[9; 4]);
            let write = |record: [u8; 16]| {
                // SAFETY: slot 7 of the heap's memory, 16 bytes.
                unsafe { std::ptr::copy_nonoverlapping(record.as_ptr(), memory.cast::<u8>().add(7 * 16), 16) }
            };
            let pass = || {
                // SAFETY: the heap's memory, alive to the end of the test.
                let page = unsafe { std::slice::from_raw_parts(memory as *const u8, 64 * 16) };
                host.mirror_copied_page(7..8, (memory as u64, 16), memory as usize, page).1
            };
            crate::live::set("SYNC_KEYS", 1).unwrap();
            write(a);
            assert_eq!(pass(), 1);
            assert!(host.made(7));
            assert_eq!(pass(), 0, "the same record");
            write(a_other);
            assert_eq!(pass(), 0, "its other bytes alone");
            assert!(host.made(7));
            write(b);
            assert_eq!(pass(), 1, "another view");
            write([7; 16]);
            assert_eq!(pass(), 1, "no view of ours");
            assert!(!host.made(7));
            write([8; 16]);
            assert_eq!(pass(), 0, "no view of ours in place of another such");
            write(a);
            assert_eq!(pass(), 1);
            assert!(host.made(7));
            crate::live::set("SYNC_KEYS", 0).unwrap();
            assert_eq!(pass(), 0, "seen with keys: no change by its print");
            write(a_other);
            assert_eq!(pass(), 1, "prints see its other bytes");
            crate::live::set("SYNC_KEYS", 1).unwrap();
            assert_eq!(pass(), 0, "seen by its print: no change by its key");
            crate::live::reset("SYNC_KEYS");
        }
        slot::<extern "system" fn(This) -> u32>(h, 0x10)(h);
        // SAFETY: this allocation belongs to the test.
        unsafe { VirtualFree(memory, 0, MEM_RELEASE) };
    }

    /// The pass spread over the threads kept for copies (`sync_wide`) makes
    /// the slots of the changed ranges it is given, each piece on a thread
    /// of its own, and no others.
    #[cfg(windows)]
    #[test]
    fn records_copied_as_bytes_are_made_by_the_wide_pass_too() {
        use windows_sys::Win32::System::Memory::{
            MEM_COMMIT, MEM_RELEASE, MEM_RESERVE, PAGE_READWRITE, VirtualAlloc, VirtualFree,
        };
        // SAFETY: a fresh allocation of ours, freed at the end.
        let memory = unsafe { VirtualAlloc(std::ptr::null(), crate::watch::PAGE, MEM_RESERVE | MEM_COMMIT, PAGE_READWRITE) };
        assert!(!memory.is_null());
        let desc = HeapDesc { kind: SAMPLERS, count: 64, flags: SHADER_VISIBLE, nodes: 1 };
        let placement = Placement { address: memory as u64, size: 64 * 16 };
        let mut h: This = std::ptr::null();
        assert_eq!(create(desc, placement, &mut h), S_OK);
        if let Some(host) = heap(h).host() {
            let mut elsewhere = [0u64; 2];
            let source = elsewhere.as_mut_ptr() as u64;
            let sampler = [21, 3, 3, 3, 0, 1, 0, 0, 0, 0, 0, 0x7f7f_ffff, 0];
            create_sampler(std::ptr::null(), &sampler, source);
            for slot in [5, 40, 50] {
                // SAFETY: the heap slot and source both hold 16 descriptor bytes.
                unsafe { std::ptr::copy_nonoverlapping(source as *const u8, memory.cast::<u8>().add(slot * 16), 16) };
            }
            let m = memory as usize;
            let [pages, looked, mirrored, _] =
                sync_pages_wide(host, &[(m + 5 * 16, m + 6 * 16), (m + 40 * 16, m + 41 * 16)], (memory as u64, 16), 64);
            assert_eq!([pages, looked, mirrored], [2, 2, 2]);
            assert!(host.made(5) && host.made(40));
            assert!(!host.made(50), "a slot outside the ranges given is left");
        }
        slot::<extern "system" fn(This) -> u32>(h, 0x10)(h);
        // SAFETY: this allocation belongs to the test.
        unsafe { VirtualFree(memory, 0, MEM_RELEASE) };
    }

    #[test]
    fn views_survive_copies() {
        let null = std::ptr::null();
        let mut heap = vec![0u64; 4 * 8]; // four 32-byte slots
        let at = |i: usize| heap.as_ptr() as u64 + 32 * i as u64;
        let (s0, s1, s2, s3) = (at(0), at(1), at(2), at(3));
        create_cbv(null, &[0x1000, 1, 256, 0], s0);
        assert_eq!(view(s0), Some(View::Constant(0x1_0000_1000, 256)));
        create_srv(null, null, std::ptr::null(), s1);
        assert_eq!(view(s1), Some(View::Shader(0, None)));
        // The same view twice is one entry.
        create_srv(null, null, std::ptr::null(), s2);
        // SAFETY: both slots are in `heap`.
        assert_eq!(unsafe { *(s1 as *const [u32; 3]) }, unsafe {
            *(s2 as *const [u32; 3])
        });
        // Two ranges of one into one range of two.
        let (dst, dst_n, src, src_n) = ([s2], [2u32], [s0, s1], [1u32, 1]);
        copy(
            null,
            1,
            dst.as_ptr(),
            dst_n.as_ptr(),
            2,
            src.as_ptr(),
            src_n.as_ptr(),
            0,
        );
        assert_eq!(view(s2), view(s0));
        assert_eq!(view(s3), view(s1));
        assert_eq!(view(at(0) + 8), None);
        heap[0] = 0;
    }

    /// Constant buffer views are named whole in their records: any address
    /// a program can have, any size below 16 KiB, multiples of 256 bytes
    /// up to 4 MiB; what does not fit says so.
    #[test]
    fn constant_views_fit_their_records() {
        let top = (1u64 << CONSTANT_ADDRESS_BITS) - 1;
        for address in [0u64, 0x100, 0x2602_0000, 0xd_6b60_0000, 0x7ffd_a34f_0000, top] {
            for size in [0u32, 1, 255, 256, 4000, (1 << 14) - 1, 1 << 14, 65536, (1 << 22) - 256] {
                let [i, generation] = constant_words(address, size).expect("fits");
                assert_eq!(generation >> KIND_SHIFT, CONSTANT);
                assert_eq!(view_at(i, generation), Some(View::Constant(address, size)), "{address:#x} {size}");
            }
        }
        assert_eq!(constant_words(top + 1, 256), None, "an address past 47 bits");
        assert_eq!(constant_words(0x1000, (1 << 14) + 1), None, "16 KiB or more, no multiple of 256");
        assert_eq!(constant_words(0x1000, 1 << 22), None, "4 MiB");
        // Different views never share their words.
        assert_ne!(constant_words(0x1000, 256), constant_words(0x1000, 512));
        assert_ne!(constant_words(0x1000, 256), constant_words(0x1100, 256));
        assert_ne!(constant_words(0x1000, 1 << 14), constant_words(0x1000, 64));
    }

    /// With `VICTORY_SHAPE_PADDING`, placed views that differ only in words
    /// that hold no field have one shape; a field that differs still makes
    /// another.
    #[test]
    fn padding_makes_no_new_shape() {
        // A texture's view (Texture2D: four of the union's six words used).
        let resource = [3, 0xdead, 0, 0, 64, 0, 64, 0x1_0001, 98, 1, 0, 0x105, 0, 0xbeef];
        let mut srv = [0u32; 16];
        (srv[0], srv[1], srv[2], srv[3]) = (98, 4, 5768, 0x1234);
        (srv[4], srv[5], srv[8], srv[9]) = (0, 7, 0xaaaa, 0xbbbb);
        let (mut r1, mut v1) = (resource, [0u32; 14]);
        v1[..10].copy_from_slice(&srv[..10]);
        padding_out(false, &mut r1, &mut v1);
        let mut other = srv;
        (other[3], other[8], other[9]) = (0x9999, 1, 2);
        let mut r2 = resource;
        (r2[1], r2[13]) = (7, 8);
        let mut v2 = [0u32; 14];
        v2[..10].copy_from_slice(&other[..10]);
        padding_out(false, &mut r2, &mut v2);
        assert_eq!((r1, v1), (r2, v2), "padding and unused union words");
        // The level count is a field.
        let mut v3 = [0u32; 14];
        let mut levels = srv;
        levels[5] = 3;
        v3[..10].copy_from_slice(&levels[..10]);
        let mut r3 = resource;
        padding_out(false, &mut r3, &mut v3);
        assert_ne!(v1, v3, "a field");
        // A buffer's view: its stride and flags are fields, word 9 is not.
        let mut buffer = [0u32; 16];
        (buffer[1], buffer[4], buffer[6], buffer[7], buffer[8], buffer[9]) = (1, 16, 4, 16, 0, 0x77);
        let mut vb = [0u32; 14];
        vb[..10].copy_from_slice(&buffer[..10]);
        let mut rb = [1u32; 14];
        padding_out(false, &mut rb, &mut vb);
        assert_eq!((vb[6], vb[7], vb[9]), (4, 16, 0));
        // An unordered buffer view: the counter offset (words 6-7) is a field.
        let mut uav = [0u32; 16];
        (uav[1], uav[2], uav[4], uav[6], uav[9]) = (1, 8, 4, 0x40, 0x55);
        let mut vu = [0u32; 14];
        vu[..10].copy_from_slice(&uav[..10]);
        let mut ru = [1u32; 14];
        padding_out(true, &mut ru, &mut vu);
        assert_eq!((vu[2], vu[4], vu[6], vu[9]), (8, 4, 0x40, 0));
        // A dimension not known here is left as it is.
        let mut odd = [0u32; 14];
        (odd[1], odd[9]) = (99, 5);
        let mut ro = [1u32; 14];
        padding_out(false, &mut ro, &mut odd);
        assert_eq!(odd[9], 5);
    }

    /// The leak of run 65: a title that writes constant buffer views and
    /// views of placed memory at ever new addresses adds nothing to the
    /// table; one shape stands for all the placed views alike.
    #[test]
    fn views_at_new_addresses_add_no_entries() {
        if !inline() {
            return;
        }
        let null = std::ptr::null();
        let slots = [0u64; 4];
        let at = slots.as_ptr() as u64;
        // A resource description and a view no other test uses.
        let resource: [u32; 14] = std::array::from_fn(|k| 0x5ee0_0000 + k as u32);
        let mut desc: [u32; 16] = std::array::from_fn(|k| 0x7a70_0000 + k as u32);
        let mine = |v: &View| match v {
            View::Constant(address, _) => (0x6_0000_0000..0x6_1000_0000).contains(address),
            View::Placed(_, r, _) => *r == resource,
            _ => false,
        };
        let entries = || VIEWS.read().unwrap().list.iter().filter(|(v, _)| mine(v)).count();
        let shapes = || VIEWS.read().unwrap().shapes.iter().filter(|s| s.1 == resource).count();
        for n in 0..20_000u64 {
            let address = 0x6_0000_0000 + n * 0x100;
            create_cbv(null, &[address as u32, (address >> 32) as u32, 512, 0], at);
            assert_eq!(view(at), Some(View::Constant(address, 512)));
            let placed = 0xd_4940_0000 + n * 0x1_0000;
            (desc[10], desc[11]) = (placed as u32, (placed >> 32) as u32);
            create_placed_srv(null, &resource, &desc, at);
            assert_eq!(view(at), Some(View::Placed(false, resource, desc)));
        }
        assert_eq!(entries(), 0, "no table entries");
        assert_eq!(shapes(), 1, "one shape for 20,000 placed views");
        // The same shape, unordered: another shape.
        create_placed_uav(null, &resource, &desc, at);
        assert_eq!(view(at), Some(View::Placed(true, resource, desc)));
        assert_eq!(shapes(), 2);
        // Placed memory past 1 TiB (not title memory) is a table entry.
        (desc[10], desc[11]) = (0, 0x100);
        create_placed_srv(null, &resource, &desc, at);
        assert_eq!(view(at), Some(View::Placed(false, resource, desc)));
        assert_eq!(entries(), 1);
        // A record's words are the same for the same view (the host knows
        // a slot's view by them), and differ with the address.
        // SAFETY: both handles are slots of 32 bytes above.
        let words = |handle: u64| unsafe { *(handle as *const [u32; 3]) };
        let other = [0u64; 4];
        create_cbv(null, &[0x40, 6, 512, 0], at);
        create_cbv(null, &[0x40, 6, 512, 0], other.as_ptr() as u64);
        assert_eq!(words(at), words(other.as_ptr() as u64));
        create_cbv(null, &[0x140, 6, 512, 0], other.as_ptr() as u64);
        assert_ne!(words(at), words(other.as_ptr() as u64));
    }

    /// A released resource's views go stale; the same view of a new
    /// resource at the same address is a new record.
    #[test]
    fn views_die_with_their_resource() {
        let null = std::ptr::null();
        let slots = [0u64; 2 * 4];
        let (a, b) = (slots.as_ptr() as u64, slots.as_ptr() as u64 + 32);
        let resource = 0x7777_0000usize as This;
        create_srv(null, resource, std::ptr::null(), a);
        assert_eq!(view(a), Some(View::Shader(0x7777_0000, None)));
        forget(0x7777_0000);
        assert_eq!(view(a), None, "stale after release");
        create_srv(null, resource, std::ptr::null(), b);
        assert_eq!(view(b), Some(View::Shader(0x7777_0000, None)));
        // SAFETY: both slots are in `slots`.
        let (old, new) = unsafe { (*(a as *const [u32; 3]), *(b as *const [u32; 3])) };
        assert_ne!(old, new, "a new generation");
    }

    /// The title places its shader-visible heap in its own memory
    /// and checks that the heap starts there.
    #[test]
    fn placed_heaps_start_at_the_title_memory() {
        let mut memory = vec![0u8; 64 * 32];
        let address = memory.as_mut_ptr() as u64;
        let desc = HeapDesc {
            kind: 0,
            count: 64,
            flags: 1,
            nodes: 1,
        };
        let mut h = std::ptr::null();
        assert_eq!(
            create(
                desc,
                Placement {
                    address,
                    size: 64 * 32
                },
                &mut h
            ),
            S_OK
        );
        assert_eq!(slot::<extern "system" fn(This) -> u64>(h, 0x48)(h), address);
        assert_eq!(slot::<extern "system" fn(This) -> u64>(h, 0x50)(h), address);
        // A size that does not match the count is not a placement.
        assert_eq!(create(desc, Placement { address, size: 7 }, &mut h), S_OK);
        assert_ne!(slot::<extern "system" fn(This) -> u64>(h, 0x48)(h), address);
    }
}
