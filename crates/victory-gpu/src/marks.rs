//! Write marks (`victory_shader::emit`, "Write marks"; after upstream
//! VIctory's `cd94d35`): translated shaders set a bit per 64 KiB title page
//! they write through an unordered buffer view of the heap, so that after a
//! submission only those pages of a host copy are compared with title
//! memory and copied back, instead of every range shaders were declared to
//! write being kept before the submission (a snapshot) and searched after.
//! In an open-world run that was 750 MB of snapshots a frame (92 ms) and a
//! search of 43 ms, for copies of 50 MB written in 1.7 MB.
//!
//! The pieces:
//! - every host copy gets a run of bits in the bitmap, one per title page
//!   it touches ([`take`]), for as long as it lives;
//! - each host view heap keeps a mark table: per slot, the first bit and
//!   page offset of the unordered buffer view there (`descriptor::host`),
//!   written whenever the slot is;
//! - a submission whose shaders write a copy only through views whose
//!   writes they mark ([`begin`]) takes no snapshot of it: its declared
//!   ranges are armed instead ([`crate::watch::arm`]), which needs every
//!   64 KiB block of them guarded and clean (view memory, as the title's
//!   page pool is). A title write to an armed block before the copy-back
//!   keeps the block's bytes first (the guard's fault, or the runtime's
//!   own write, before it lands); a change seen only afterwards (a
//!   remapping, a write the runtime tells of when it is done) makes the
//!   block unknown, and its marked pages are left as the title has them
//!   (counted: "lost");
//! - after the submission ([`copy_back`]), the bitmap is read and cleared,
//!   and in each marked page the bytes that differ from the page's bytes
//!   before the submission (title memory as it is where the title did not
//!   write, else the kept bytes) are the shaders' and go into title memory.
//!
//! Where it applies: the relay, late waits and kept snapshots off (one
//! submission at a time: one bitmap cannot tell flights apart, and clearing
//! it at one landing would lose a later flight's marks). Copies in the
//! card's memory and in place keep their own ways; a copy that a
//! submission writes through anything unmarked (a root view, memory by
//! address, a program translated without marks), and a range not all
//! clean when armed, keep the snapshot.
//!
//! `VICTORY_VIEW_MARKS=0`: no marks in translations and none here.
//! `VICTORY_VIEW_MARKS=check` (diagnostics builds): snapshots are kept as
//! before as well, and after the marked pages go back, any byte a shader
//! changed in a page no bit names is logged and counted ("missed"): a
//! write the marks did not catch. The snapshot path then copies it back.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};

use windows::Win32::Graphics::Direct3D12::ID3D12Resource;

use crate::shadow::Shadow;
use crate::watch::PAGE;

/// Whether submissions use the marks (module notes): the translations
/// carry them, and the relay, late waits and kept snapshots are off.
pub(crate) fn on() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| {
        let translated = victory_shader::emit::view_marks();
        let (late, relay, keeps) = (crate::host::late(), crate::shadow::relay(), crate::shadow::keeps());
        let on = translated && !late && !relay && !keeps;
        tracing::info!(
            on,
            translated,
            late,
            relay,
            keeps,
            check = check(),
            "write marks: what shaders write through views is copied back by the pages they mark \
             (VICTORY_VIEW_MARKS=0 turns them off; they need the relay, late waits and kept snapshots off)"
        );
        on
    })
}

/// `VICTORY_VIEW_MARKS=check`, diagnostics builds (module notes).
pub(crate) fn check() -> bool {
    static CHECK: OnceLock<bool> = OnceLock::new();
    cfg!(feature = "diagnostics")
        && *CHECK.get_or_init(|| std::env::var_os("VICTORY_VIEW_MARKS").is_some_and(|v| v == "check"))
}

/// Bits never handed out: a mark table read past its end gives first bit
/// 0, and its marks land here.
const RESERVED: u32 = 64;

/// The bitmap's bits: free runs below `end` (first bit → length), the end
/// of what was ever handed out, and the copy holding each run.
struct Runs {
    free: BTreeMap<u32, u32>,
    end: u32,
    owners: BTreeMap<u32, Weak<Shadow>>,
}

static RUNS: Mutex<Runs> = Mutex::new(Runs {
    free: BTreeMap::new(),
    end: RESERVED,
    owners: BTreeMap::new(),
});

fn runs() -> std::sync::MutexGuard<'static, Runs> {
    RUNS.lock().unwrap_or_else(|e| e.into_inner())
}

impl Runs {
    /// A run of `n` bits: its first. None when the bitmap is full (its
    /// last bit takes marks past it and is never handed out).
    fn take(&mut self, n: u32) -> Option<u32> {
        if let Some((&at, &len)) = self.free.iter().find(|&(_, &len)| len >= n) {
            self.free.remove(&at);
            if len > n {
                self.free.insert(at + n, len - n);
            }
            return Some(at);
        }
        if self.end + n < victory_shader::emit::VIEW_WRITE_BITS {
            self.end += n;
            return Some(self.end - n);
        }
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(|| tracing::warn!("write marks: no bits left; copies made now keep their snapshots"));
        None
    }

    /// The run of `n` bits at `at` is free again, joined to its neighbours.
    fn give_back(&mut self, mut at: u32, mut n: u32) {
        if let Some((&before, &len)) = self.free.range(..at).next_back()
            && before + len == at
        {
            self.free.remove(&before);
            (at, n) = (before, n + len);
        }
        if let Some(len) = self.free.remove(&(at + n)) {
            n += len;
        }
        self.free.insert(at, n);
    }
}

/// The 64 KiB title pages `[start, start + len)` touches.
pub(crate) fn pages(start: u64, len: u64) -> u32 {
    ((start + len.max(1) - 1) >> 16).saturating_sub(start >> 16) as u32 + 1
}

/// A run of bits for a copy of `[start, start + len)`, if marks are on.
pub(crate) fn take(start: u64, len: u64) -> Option<u32> {
    if !on() {
        return None;
    }
    runs().take(pages(start, len))
}

/// The copy holding the run from `base` on.
pub(crate) fn register(base: u32, copy: &Arc<Shadow>) {
    runs().owners.insert(base, Arc::downgrade(copy));
}

/// The copy of `[start, start + len)` holding the run from `base` is gone.
pub(crate) fn give_back(base: u32, start: u64, len: u64) {
    let mut r = runs();
    r.owners.remove(&base);
    r.give_back(base, pages(start, len));
}

/// The bitmap: a buffer shaders write and the CPU reads, its mapping and
/// host GPU address.
struct Bitmap {
    _buffer: ID3D12Resource,
    data: usize,
    gpu: u64,
}

// SAFETY: D3D12 resources are free-threaded; the mapping is read and
// cleared only between submissions, which run one at a time when the
// marks are on.
unsafe impl Send for Bitmap {}
// SAFETY: as above.
unsafe impl Sync for Bitmap {}

fn bitmap() -> Option<&'static Bitmap> {
    static BITMAP: OnceLock<Option<Bitmap>> = OnceLock::new();
    BITMAP
        .get_or_init(|| {
            let bytes = u64::from(victory_shader::emit::VIEW_WRITE_BITS / 8);
            let (buffer, data) = crate::host::host()?.shared_buffer(bytes)?;
            crate::host::name(&buffer, || "pages shaders wrote through views".into());
            // SAFETY: our mapped buffer of `bytes` bytes.
            unsafe { std::ptr::write_bytes(data, 0, bytes as usize) };
            // SAFETY: a live buffer.
            let gpu = unsafe { buffer.GetGPUVirtualAddress() };
            Some(Bitmap {
                _buffer: buffer,
                data: data as usize,
                gpu,
            })
        })
        .as_ref()
}

/// The host GPU address of the bitmap, for the root UAV of shaders that
/// mark their writes (made whether or not [`on`]: such shaders need it
/// bound).
pub(crate) fn bitmap_gpu() -> Option<u64> {
    bitmap().map(|b| b.gpu)
}

/// A mark table with no entry, for a shader that marks its writes while
/// no host view heap is bound: its reads past the end give first bit 0,
/// which is [`RESERVED`].
pub(crate) fn empty_table_gpu() -> Option<u64> {
    static TABLE: OnceLock<Option<(usize, u64)>> = OnceLock::new();
    TABLE
        .get_or_init(|| {
            let (buffer, data) = crate::host::host()?.upload_buffer(4096)?;
            // SAFETY: our mapped buffer of 4,096 bytes.
            unsafe { std::ptr::write_bytes(data, 0xFF, 4096) };
            // SAFETY: a live buffer.
            let gpu = unsafe { buffer.GetGPUVirtualAddress() };
            // Kept for good.
            std::mem::forget(buffer);
            Some((data as usize, gpu))
        })
        .map(|t| t.1)
}

/// Since the last line: pages compared, bytes written back, pages the
/// title wrote first (their bytes kept), pages left as the title has them
/// (their bytes from before unknown), ranges armed, ranges that kept a
/// snapshot (not all clean), copies marked in submissions, and with
/// [`check`] pages and bytes a shader changed that no bit names.
static STATS: [AtomicU64; 9] = [const { AtomicU64::new(0) }; 9];
pub(crate) const PAGES: usize = 0;
pub(crate) const BYTES: usize = 1;
pub(crate) const SAVED: usize = 2;
pub(crate) const LOST: usize = 3;
pub(crate) const ARMED: usize = 4;
pub(crate) const FELL_BACK: usize = 5;
const COPIES: usize = 6;
pub(crate) const MISSED_PAGES: usize = 7;
pub(crate) const MISSED_BYTES: usize = 8;

pub(crate) fn count(what: usize, n: u64) {
    STATS[what].fetch_add(n, Ordering::Relaxed);
}

/// The frame-rate line's account of the marks, per frame.
pub(crate) fn line(frames: f64) {
    if !on() {
        return;
    }
    let s: Vec<f64> = STATS.iter().map(|s| s.swap(0, Ordering::Relaxed) as f64 / frames).collect();
    tracing::info!(
        "write marks per frame: {:.0} copies marked in submissions, {:.0} ranges armed (no snapshot), {:.0} kept their snapshot \
         (not all guarded and clean); {:.0} marked pages compared, {:.2} MB written back; {:.1} pages the title wrote first \
         (kept before its write), {:.1} left as the title has them (bytes from before unknown){}",
        s[COPIES],
        s[ARMED],
        s[FELL_BACK],
        s[PAGES],
        s[BYTES] / 1e6,
        s[SAVED],
        s[LOST],
        if check() {
            format!("; check: {:.1} pages, {:.0} bytes a shader changed that no bit names", s[MISSED_PAGES], s[MISSED_BYTES])
        } else {
            String::new()
        }
    );
}

/// The copies a submission writes through marked views only, which take
/// no snapshot ([`begin`] to [`end`]).
pub(crate) struct Marking {
    copies: Vec<Arc<Shadow>>,
}

impl Marking {
    pub(crate) fn has(&self, copy: &Arc<Shadow>) -> bool {
        self.copies.iter().any(|c| Arc::ptr_eq(c, copy))
    }
}

/// Before a submission's preparation: `writes` are the copies its shaders
/// write, each with whether every declared range of it is written through
/// marked views only. Those copies (with bits) take no snapshot.
pub(crate) fn begin(writes: Vec<(Arc<Shadow>, bool)>) -> Marking {
    let mut copies = Vec::new();
    if on() {
        for (copy, marked) in writes {
            if marked && copy.marks_now(true) {
                copies.push(copy);
            }
        }
    }
    count(COPIES, copies.len() as u64);
    Marking { copies }
}

/// After the submission ran, before the copy-backs of declared ranges:
/// every page its shaders marked of the copies in `m` goes back
/// ([`Shadow::copy_back_marked`]); the bitmap is cleared.
pub(crate) fn copy_back(m: &Marking) {
    let Some(b) = bitmap() else {
        return;
    };
    let end = runs().end.min(victory_shader::emit::VIEW_WRITE_BITS) as usize;
    let words = end.div_ceil(32);
    // SAFETY: the mapped bitmap; the card is done with it (one submission
    // at a time).
    let bits = unsafe { std::slice::from_raw_parts_mut(b.data as *mut u32, words) };
    let mut marked: Vec<u32> = Vec::new();
    for (w, word) in bits.iter_mut().enumerate() {
        // Most words are zero: read only, not written back (after
        // upstream VIctory #2409).
        if *word == 0 {
            continue;
        }
        let mut x = std::mem::take(word);
        while x != 0 {
            marked.push((w * 32) as u32 + x.trailing_zeros());
            x &= x - 1;
        }
    }
    // (Marks past the runs land in the last bit.)
    // SAFETY: as above; the last dword of the bitmap.
    unsafe { *(b.data as *mut u32).add((victory_shader::emit::VIEW_WRITE_BITS / 32 - 1) as usize) = 0 };
    if m.copies.is_empty() {
        return;
    }
    // Per copy of `m`, the title pages marked; and per page, the copies.
    let mut pages: Vec<(Arc<Shadow>, Vec<usize>)> = Vec::new();
    let mut holders: std::collections::BTreeMap<usize, Vec<usize>> = std::collections::BTreeMap::new();
    {
        let r = runs();
        for &n in marked.iter().filter(|&&n| n >= RESERVED) {
            let Some((&base, copy)) = r.owners.range(..=n).next_back() else {
                continue;
            };
            let Some(copy) = copy.upgrade().filter(|c| m.has(c)) else {
                continue;
            };
            let k = n - base;
            let (start, len) = copy.span();
            if k >= self::pages(start, len) {
                continue;
            }
            let page = (((start >> 16) + u64::from(k)) << 16) as usize;
            let at = match pages.iter().position(|(c, _)| Arc::ptr_eq(c, &copy)) {
                Some(at) => at,
                None => {
                    pages.push((copy, Vec::new()));
                    pages.len() - 1
                }
            };
            pages[at].1.push(page);
            holders.entry(page).or_default().push(at);
        }
    }
    // Page by page, its bytes before the submission once however many
    // copies hold it: what the title's first write kept, else title memory
    // as it is now (none written since the copies took it), read into one
    // buffer reused for every page; nothing for a page whose bytes from
    // before are not known.
    let mut now = vec![0u8; PAGE];
    for (page, copies) in holders {
        // Pages of ranges that kept their snapshots are compared there.
        let copies: Vec<&Arc<Shadow>> =
            copies.iter().map(|&at| &pages[at].0).filter(|c| c.marks_cover(page)).collect();
        if copies.is_empty() {
            continue;
        }
        let saved;
        let bytes: &[u8] = match crate::watch::armed_take(page) {
            Some(crate::watch::Armed::Untouched) if crate::host::read_into(page as *const u8, &mut now) => &now,
            Some(crate::watch::Armed::Saved(bytes)) => {
                count(SAVED, 1);
                saved = bytes;
                &saved
            }
            _ => {
                count(LOST, copies.len() as u64);
                continue;
            }
        };
        for copy in copies {
            let written = copy.copy_back_marked(page, bytes);
            count(PAGES, 1);
            count(BYTES, written as u64);
        }
    }
    #[cfg(feature = "diagnostics")]
    if check() {
        for copy in &m.copies {
            let list = pages.iter().find(|(c, _)| Arc::ptr_eq(c, copy)).map_or(&[][..], |(_, l)| &l[..]);
            let (missed_pages, missed_bytes) = copy.check_unmarked(list);
            count(MISSED_PAGES, missed_pages);
            count(MISSED_BYTES, missed_bytes);
        }
    }
}

/// After every copy-back of the submission: the copies' marked ranges are
/// done with, and nothing stays armed.
pub(crate) fn end(m: Marking) {
    for copy in &m.copies {
        copy.marks_done();
        copy.marks_now(false);
    }
    if !m.copies.is_empty() {
        crate::watch::disarm();
    }
}

#[cfg(test)]
mod tests {
    use super::{Runs, pages};
    use std::collections::BTreeMap;

    /// Bits are handed out in runs past the reserved ones and come back
    /// joined.
    #[test]
    fn runs_are_reused_and_joined() {
        let mut r = Runs {
            free: BTreeMap::new(),
            end: 64,
            owners: BTreeMap::new(),
        };
        assert_eq!((r.take(3), r.take(2)), (Some(64), Some(67)));
        r.give_back(64, 3);
        assert_eq!(r.take(1), Some(64), "first fit");
        r.give_back(67, 2);
        assert_eq!(r.free.iter().collect::<Vec<_>>(), [(&65, &4)], "67-68 joins the free 65-66");
        r.give_back(64, 1);
        assert_eq!(r.free.iter().collect::<Vec<_>>(), [(&64, &5)], "and 64 joins them");
        assert_eq!(r.take(5), Some(64));
        assert_eq!(pages(0x1_fff0, 0x20), 2, "a range across a page boundary");
        assert_eq!(pages(0x2_0000, 0x1_0000), 1);
    }
}
