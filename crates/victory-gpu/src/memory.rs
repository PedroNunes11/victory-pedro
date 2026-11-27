//! Title memory by address, for translated shaders that read and write
//! through raw 40-bit addresses (`victory_shader::emit::SLOT_MEMORY`: the
//! console's ray-tracing structure builder and the programs that walk its
//! structures; the structures themselves lie in plain title memory, in no
//! buffer object).
//!
//! The copies are the ones every other access uses ([`crate::shadow`]: a
//! title buffer's copy, or the 4 MiB window of memory outside buffers), so
//! a root view and a raw address reach the same host bytes. This module
//! keeps what a shader needs to find them:
//!
//! - the **directory**: per 64 KiB page of title address space the number
//!   of the copy that holds the page (0: none) and the page's byte offset
//!   in it; one buffer for all lists, entries only ever go from empty to
//!   filled;
//! - per view heap a run of slots with a raw unordered access view of each
//!   numbered copy (slot 0 of the run: a null view, for pages without a
//!   copy);
//! - the **faults**: the page numbers shaders found empty, read back here
//!   once a frame, so the copies exist from the next frame on. A pointer a
//!   shader takes out of memory cannot be known before it runs; the
//!   addresses a dispatch is given as root views are made ready before it.
//!
//! **A page that is not there when a shader touches it is a lost access**:
//! the read gives 0, the store is dropped, and the fault note only helps
//! the next frame. For the structure builder that is the same as a dropped
//! stage, so before a dispatch the host makes ready everything it can see
//! the dispatch name: the whole copy around each root view, around each
//! pair of root constants that is a title address and around each root
//! constant that is one in 256-byte units (the builder passes structures
//! all three ways), and the 4 MiB after each (a structure may cross a
//! window's end).
//! shortcut: what a stage reaches only through pointers it reads out of
//! memory (InitHeaders' table of structure addresses, LoadInstances'
//! instance records) is not made ready unless something else named it;
//! the fault count in the log says whether that happens. The whole answer
//! is the extent of the title's allocation around an address, which the
//! memory module of the layer knows and this one does not ask yet.
//!
//! With the structure builder asked for (`crate::pipeline::builder_allowed`)
//! a list brings up to date only the copies its dispatches name (with the
//! title addresses in the records of list upload memory its root SRVs point
//! into), those of upload memory and those just numbered, after victory-fork
//! pull request 994; and
//! the directory follows the title's buffers (after DexNom/VIctory): a
//! page whose buffer the title released or replaced is given the copy of
//! what is there now ([`ownership_changed`], [`Memory::reconcile`]) in a new
//! directory, while lists already recorded keep the one they were recorded
//! with; and a dispatch brings up to date, and declares written, only the
//! pages the directory gives each copy, not the whole copy. Without it a
//! builder stage could store through a page into the copy of a buffer the
//! title had released, and that copy's bytes were then copied back over
//! whatever the title had put at that address since.
//!
//! shortcut: a copy that is numbered is kept for the rest of the run, and
//! there are at most [`COPIES`] of them (4 MiB windows mostly: 4 GiB at
//! most). Every dispatch of such a shader brings all numbered copies up to
//! date and, if it can store, declares all of them written (the bytes are
//! compared on the CPU afterwards, as for any unordered access view): a
//! dirty-page mask written by the shader would bound both.

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use victory_shader::emit::{MEMORY_FAULTS, MEMORY_PAGES};
use windows::Win32::Graphics::Direct3D12::*;

use crate::descriptor::host::HostHeap;
use crate::host::{HostList, host};
use crate::shadow::Shadow;

/// Copies that can be numbered (the run of heap slots is one longer). A
/// listed frame of the opening cutscene names about 50 windows.
const COPIES: u32 = 1023;
/// With the builder asked for: its dispatches named about 1,125 windows in
/// their first frame and about 1,450 in six minutes (victory-fork pull
/// request 994, run rt-dry1), more than [`COPIES`].
const COPIES_EXACT: u32 = 2047;

/// How many copies can be numbered in this run.
fn copies_max() -> u32 {
    if exact() { COPIES_EXACT } else { COPIES }
}
/// A window of memory outside buffers (`shadow.rs`).
const CELL: u64 = 4 << 20;
/// A directory page.
const PAGE: u64 = 1 << 16;

/// Spans of title memory (in pages) whose buffers changed, the last 256,
/// once a shader has used the directory with the builder asked for; and
/// how many changes there were in all.
static CHANGES: OnceLock<Mutex<VecDeque<(u64, u64)>>> = OnceLock::new();
static GENERATION: AtomicU64 = AtomicU64::new(0);

/// The title registered, released or replaced a buffer over
/// `[start, start + len)`: pages there may now belong to another copy.
pub(crate) fn ownership_changed(start: u64, len: u64) {
    if let Some(changes) = CHANGES.get() {
        let mut changes = changes.lock().unwrap_or_else(|e| e.into_inner());
        if changes.len() == 256 {
            changes.pop_front();
        }
        changes.push_back((start / PAGE, start.saturating_add(len).div_ceil(PAGE)));
        GENERATION.fetch_add(1, Ordering::Release);
    }
}

/// Whether the directory follows the title's buffers and dispatches work
/// on the directory's pages alone (see the module).
fn exact() -> bool {
    crate::pipeline::builder_allowed()
}

struct Memory {
    /// Buffer, its mapping, its GPU address.
    directory: (ID3D12Resource, *mut u32, u64),
    faults: (ID3D12Resource, *mut u32, u64),
    /// Copy `n` is `copies[n - 1]`.
    copies: Vec<Arc<Shadow>>,
    /// Per host view heap (by address): the first slot of its run, and how
    /// many copies have their view there.
    heaps: HashMap<usize, (u32, u32)>,
    /// The frame the faults were last read in.
    frame: u64,
    /// Pages asked for this frame that are not the title's memory.
    refused: std::collections::HashSet<u64>,
    /// Filled pages and the index (in `copies`) of the copy each has.
    pages: BTreeMap<u64, usize>,
    /// Per copy (by index) the runs of pages the directory gives it, as
    /// (title address, bytes), and the copies whose runs are to be made
    /// again.
    ranges: Vec<Vec<(u64, u64)>>,
    recalculate: BTreeSet<usize>,
    /// The changes ([`GENERATION`]) the directory has followed.
    generation: u64,
    /// Bumped whenever the directory is replaced: a list keeps every
    /// directory it was recorded with.
    revision: u64,
}

// SAFETY: the mappings are of buffers the struct owns; all access is under
// the mutex.
unsafe impl Send for Memory {}

static MEMORY: Mutex<Option<Memory>> = Mutex::new(None);

/// What a draw binds.
pub(crate) struct Ready {
    pub(crate) directory: u64,
    pub(crate) faults: u64,
    /// The view heap slot of the null view; copy `n`'s view is `n` further.
    pub(crate) base: u32,
    /// How many copies have their view in the run at this bind: shaders
    /// read no later number (`USER_MEMORY_BASE` dword 1).
    pub(crate) count: u32,
}

impl Memory {
    fn new() -> Option<Memory> {
        let host = host()?;
        let make = |dwords: u64, what: &str| {
            let (buffer, data) = host.shared_buffer(dwords * 4)?;
            crate::host::name(&buffer, || format!("title memory by address: {what}"));
            // SAFETY: a fresh mapping of at least that many bytes.
            unsafe { std::ptr::write_bytes(data, 0, (dwords * 4) as usize) };
            // SAFETY: a live buffer.
            let gpu = unsafe { buffer.GetGPUVirtualAddress() };
            Some((buffer, data.cast::<u32>(), gpu))
        };
        tracing::info!("title memory by address: a shader reads or writes through raw addresses");
        if exact() {
            CHANGES.get_or_init(Default::default);
        }
        Some(Memory {
            directory: make(u64::from(MEMORY_PAGES) * 2, "directory")?,
            faults: make(u64::from(MEMORY_FAULTS), "faults")?,
            copies: Vec::new(),
            heaps: HashMap::new(),
            frame: u64::MAX,
            refused: Default::default(),
            pages: BTreeMap::new(),
            ranges: Vec::new(),
            recalculate: BTreeSet::new(),
            generation: GENERATION.load(Ordering::Acquire),
            revision: 0,
        })
    }

    /// A fresh, empty table of `dwords`.
    fn table(dwords: u64, what: &str) -> Option<(ID3D12Resource, *mut u32, u64)> {
        let (buffer, data) = host()?.shared_buffer(dwords * 4)?;
        crate::host::name(&buffer, || format!("title memory by address: {what}"));
        // SAFETY: a fresh mapping of at least that many bytes.
        unsafe { std::ptr::write_bytes(data, 0, (dwords * 4) as usize) };
        // SAFETY: a live buffer.
        let gpu = unsafe { buffer.GetGPUVirtualAddress() };
        Some((buffer, data.cast::<u32>(), gpu))
    }

    /// The copy that holds the whole page at `start` and the page's byte in
    /// it, if the memory is the title's and a buffer with unordered access
    /// holds it: what a root view there would get.
    fn page_owner(start: u64) -> Option<(Arc<Shadow>, u32)> {
        if !crate::texture::committed(start as *const u8, PAGE as usize) {
            return None;
        }
        let whole = |s: &Arc<Shadow>| s.offset_in(start).is_some_and(|_| s.left(start) >= PAGE);
        let shadow = crate::shadow::find(start, 1)
            .filter(whole)
            .or_else(|| crate::shadow::find(start, PAGE).filter(whole))?;
        // SAFETY: a live buffer.
        let flags = unsafe { shadow.buffer.GetDesc() }.Flags;
        if flags.0 & D3D12_RESOURCE_FLAG_ALLOW_UNORDERED_ACCESS.0 == 0 {
            return None;
        }
        let offset = u32::try_from(shadow.offset_in(start)?).ok()?;
        Some((shadow, offset))
    }

    /// The index of `shadow` in `copies`, numbering it if it has none.
    fn number(&mut self, shadow: &Arc<Shadow>) -> Option<usize> {
        if let Some(n) = self.copies.iter().position(|c| Arc::ptr_eq(c, shadow)) {
            return Some(n);
        }
        if self.copies.len() == copies_max() as usize {
            static ONCE: std::sync::Once = std::sync::Once::new();
            ONCE.call_once(|| {
                tracing::warn!(copies = copies_max(), "title memory by address: no number left for another copy; its pages read zeros")
            });
            return None;
        }
        self.copies.push(shadow.clone());
        Some(self.copies.len() - 1)
    }

    /// Sets the page's directory entry: copy index and byte, or empty.
    fn set_page(&mut self, page: u64, mapping: Option<(usize, u32)>) {
        let (number, offset) = match mapping {
            Some((index, offset)) => {
                if let Some(old) = self.pages.insert(page, index) {
                    self.recalculate.insert(old);
                }
                self.recalculate.insert(index);
                (index as u32 + 1, offset)
            }
            None => {
                if let Some(old) = self.pages.remove(&page) {
                    self.recalculate.insert(old);
                }
                (0, 0)
            }
        };
        // SAFETY: inside the directory's mapping. The offset first: a
        // shader that sees the number sees it too.
        unsafe {
            let entry = self.directory.1.add(page as usize * 2);
            entry.add(1).write_volatile(offset);
            entry.write_volatile(number);
        }
    }

    /// Follows the buffers the title registered or released since the last
    /// look: a filled page whose copy is no longer what holds it gets the
    /// copy of what does (or none), in a new directory, so lists recorded
    /// before keep the old entries.
    fn reconcile(&mut self) -> Option<()> {
        let now = GENERATION.load(Ordering::Acquire);
        if self.generation == now {
            return Some(());
        }
        let mut check = BTreeSet::new();
        let now = {
            let changes = CHANGES.get()?.lock().unwrap_or_else(|e| e.into_inner());
            let now = GENERATION.load(Ordering::Acquire);
            let count = now - self.generation;
            if count > changes.len() as u64 {
                check.extend(self.pages.keys().copied());
            } else {
                for &(first, end) in changes.iter().rev().take(count as usize) {
                    check.extend(self.pages.range(first..end).map(|(&page, _)| page));
                }
            }
            now
        };
        let mut replaced = false;
        for page in check {
            let old = self.pages[&page];
            let owner = Self::page_owner(page * PAGE);
            if owner.as_ref().is_some_and(|(s, _)| Arc::ptr_eq(s, &self.copies[old])) {
                continue;
            }
            if !replaced {
                let next = Self::table(u64::from(MEMORY_PAGES) * 2, "directory")?;
                // SAFETY: two tables of the same size; the GPU only reads
                // directories, the CPU writes them under the mutex.
                unsafe { std::ptr::copy_nonoverlapping(self.directory.1, next.1, MEMORY_PAGES as usize * 2) };
                self.directory = next;
                self.revision += 1;
                replaced = true;
            }
            let mapping = owner.and_then(|(s, offset)| self.number(&s).map(|n| (n, offset)));
            self.set_page(page, mapping);
        }
        self.generation = now;
        self.refused.clear();
        Some(())
    }

    /// The runs of pages each copy whose pages changed has now.
    fn update_ranges(&mut self) {
        self.ranges.resize(self.copies.len(), Vec::new());
        for index in std::mem::take(&mut self.recalculate) {
            let (start, len) = self.copies[index].span();
            let ranges = &mut self.ranges[index];
            ranges.clear();
            for (&page, &owner) in self.pages.range(start / PAGE..(start + len).div_ceil(PAGE)) {
                if owner != index {
                    continue;
                }
                let at = page * PAGE;
                match ranges.last_mut() {
                    Some((first, bytes)) if *first + *bytes == at => *bytes += PAGE,
                    _ => ranges.push((at, PAGE)),
                }
            }
        }
    }

    /// With the builder asked for: the page of `address` gets its copy if
    /// it has none ([`Memory::map_exact`]); the index in `copies` of the
    /// copy that holds it, if one does.
    fn map_index(&mut self, address: u64) -> Option<usize> {
        let page = address / PAGE;
        if page >= u64::from(MEMORY_PAGES) - 1 {
            return None;
        }
        if let Some(&n) = self.pages.get(&page) {
            return Some(n);
        }
        if self.refused.contains(&page) {
            return None;
        }
        self.map_exact(page);
        self.pages.get(&page).copied()
    }

    /// What a dispatch names, with the builder asked for: the copy around
    /// `address` and the one 4 MiB after it, by index.
    fn name_exact(&mut self, address: u64) -> [Option<usize>; 2] {
        [self.map_index(address), self.map_index((address & !(CELL - 1)) + CELL)]
    }

    /// [`Memory::map`] with the builder asked for: the copy of what holds
    /// the page now, and every other page of that copy given the copy of
    /// what holds it (a buffer inside the copy's that has a copy of its
    /// own keeps its own).
    fn map_exact(&mut self, page: u64) {
        let start = page * PAGE;
        let Some((shadow, offset)) = Self::page_owner(start) else {
            self.refused.insert(page);
            return;
        };
        let count = self.copies.len();
        let Some(n) = self.number(&shadow) else { return };
        self.set_page(page, Some((n, offset)));
        if n < count {
            return;
        }
        let (first, len) = shadow.span();
        let end = (first + len) / PAGE;
        for p in first.div_ceil(PAGE)..end.min(u64::from(MEMORY_PAGES) - 1) {
            if p == page || self.pages.contains_key(&p) {
                continue;
            }
            if let Some((owner, at)) = Self::page_owner(p * PAGE)
                && let Some(number) = self.number(&owner)
            {
                self.set_page(p, Some((number, at)));
            }
        }
    }

    /// Gives the page of `address` its copy, if it has none and the memory
    /// is the title's.
    fn map(&mut self, address: u64) {
        let page = address / PAGE;
        // The last entry stays empty: addresses past the directory read it.
        if page >= u64::from(MEMORY_PAGES) - 1 {
            return;
        }
        // SAFETY: inside the directory's mapping.
        let entry = unsafe { self.directory.1.add(page as usize * 2) };
        // SAFETY: as above.
        if unsafe { entry.read() } != 0 {
            return;
        }
        let start = page * PAGE;
        // A shader's pointer, or a constant taken for an address, may be
        // anything: only memory the title has.
        if self.refused.contains(&page) {
            return;
        }
        if exact() {
            self.map_exact(page);
            return;
        }
        if !crate::texture::committed(start as *const u8, PAGE as usize) {
            self.refused.insert(page);
            return;
        }
        // The copy a root view at that address would get; a buffer that
        // ends inside the page is passed over for what holds all of it.
        let whole = |s: &Arc<Shadow>| s.offset_in(start).is_some_and(|_| s.left(start) >= PAGE);
        let Some(shadow) = crate::shadow::find(start, 1)
            .filter(whole)
            .or_else(|| crate::shadow::find(start, PAGE).filter(whole))
        else {
            return;
        };
        // SAFETY: a live buffer.
        let flags = unsafe { shadow.buffer.GetDesc() }.Flags;
        if flags.0 & D3D12_RESOURCE_FLAG_ALLOW_UNORDERED_ACCESS.0 == 0 {
            return;
        }
        let n = match self.copies.iter().position(|c| Arc::ptr_eq(c, &shadow)) {
            Some(n) => n,
            None if self.copies.len() < COPIES as usize => {
                self.copies.push(shadow.clone());
                let n = self.copies.len() - 1;
                // Every page the copy holds whole, not only the one asked
                // for: a structure is more than 64 KiB.
                // shortcut: a page of a title buffer inside this one that
                // has a copy of its own is given this copy too.
                let (first, len) = shadow.span();
                let mut at = first.next_multiple_of(PAGE);
                while at + PAGE <= first + len && at / PAGE < u64::from(MEMORY_PAGES) - 1 {
                    // SAFETY: inside the directory's mapping.
                    let other = unsafe { self.directory.1.add((at / PAGE) as usize * 2) };
                    // SAFETY: as above; the offset first, as below.
                    unsafe {
                        if other.read() == 0 {
                            other.add(1).write_volatile((at - first) as u32);
                            other.write_volatile(n as u32 + 1);
                        }
                    }
                    at += PAGE;
                }
                n
            }
            None => {
                static ONCE: std::sync::Once = std::sync::Once::new();
                ONCE.call_once(|| {
                    tracing::warn!(COPIES, "title memory by address: no number left for another copy; its pages read zeros")
                });
                return;
            }
        };
        let offset = shadow.offset_in(start).unwrap_or(0);
        // The offset first: a shader that sees the number sees it too.
        // SAFETY: inside the directory's mapping.
        unsafe {
            entry.add(1).write_volatile(offset as u32);
            entry.write_volatile(n as u32 + 1);
        }
    }

    /// The pages shaders found empty since the last frame.
    fn take_faults(&mut self) {
        let mut pages = Vec::new();
        for i in 1..MEMORY_FAULTS as usize {
            // SAFETY: inside the faults' mapping; the GPU may be writing,
            // a number missed now is written again next frame.
            let at = unsafe { self.faults.1.add(i) };
            // SAFETY: as above.
            let page = unsafe { at.read_volatile() };
            if page != 0 {
                pages.push(page);
                // SAFETY: as above.
                unsafe { at.write_volatile(0) };
            }
        }
        if !pages.is_empty() {
            // Each is an access that read 0 or was dropped.
            static SAID: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
            if SAID.fetch_add(1, std::sync::atomic::Ordering::Relaxed) < 50 {
                tracing::warn!(
                    pages = pages.len(),
                    first = format_args!("{:#x}", u64::from(pages[0]) * PAGE),
                    "title memory by address: shaders touched pages that had no copy (those accesses read 0 or were dropped); the copies are made now"
                );
            }
        }
        for page in pages {
            self.map(u64::from(page) * PAGE);
        }
    }

    /// What a dispatch names: the copy around `address` and the 4 MiB
    /// after it.
    fn name(&mut self, address: u64) {
        self.map(address);
        self.map((address & !(CELL - 1)) + CELL);
    }
}

/// Before a draw whose shader reads title memory by address: what its
/// root arguments name gets its copies (`addresses`: its root views;
/// `constants`: its root constants, see the module), every numbered copy
/// has its view in `heap`'s run and is brought up to date when `list` is
/// submitted (and, with `writes`, copied back after). None if the host
/// cannot hold it; the draw is then dropped.
/// A key a list is asked about ([`HostList::asked`]) once a shader of it
/// may store to title memory through an address.
pub(crate) const STORES: usize = usize::MAX - 0x6f;

/// Bytes of a block of list upload memory looked through for title
/// addresses, from a root SRV's address on: some 340 geometry descriptions.
const SCAN: u64 = 16 << 10;

/// The 8-byte values of `bytes` (from an 8-byte boundary) that are title
/// addresses as the builder writes them: bits 32 to 39 set, nothing above,
/// and a low dword of 64 KiB or more (as for an address in two constants;
/// an address that is not the title's memory is refused when named).
fn title_addresses(bytes: &[u8]) -> impl Iterator<Item = u64> + '_ {
    bytes.chunks_exact(8).filter_map(|q| {
        let v = u64::from_le_bytes(q.try_into().ok()?);
        ((1..=0xFF).contains(&(v >> 32)) && v as u32 >= 0x1_0000).then_some(v)
    })
}

/// `records`: the root views of the shader's root SRVs; with the builder
/// asked for, the blocks of list upload memory they point into are looked
/// through for title addresses (after victory-fork pull request 994,
/// commit 273fc655: the builder's leaf rehydration loads its index and
/// vertex buffers through the addresses in its 48-byte geometry records
/// there, which no root argument names).
pub(crate) fn ready(
    list: &HostList,
    heap: &HostHeap,
    addresses: &[u64],
    constants: &[u32],
    records: &[u64],
    writes: bool,
) -> Option<Ready> {
    let mut guard = MEMORY.lock().unwrap_or_else(|e| e.into_inner());
    if guard.is_none() {
        *guard = Memory::new();
    }
    let m = guard.as_mut()?;
    if exact() {
        m.reconcile()?;
    }
    let frame = crate::frame::current();
    if m.frame != frame {
        m.frame = frame;
        m.refused.clear();
        m.take_faults();
    }
    let numbered = m.copies.len();
    if writes {
        list.first_use(STORES);
    }
    // With the builder asked for (after victory-fork pull request 994,
    // commit 2d95091e): the copies the dispatch names, and those of list
    // upload memory, by index.
    let (mut named, mut upload) = (Vec::new(), Vec::new());
    if exact() {
        for &address in addresses {
            named.extend(m.name_exact(address).into_iter().flatten());
        }
        // List upload memory (`crate::list::upload`): the builder reaches
        // it through pointers it reads out of its root views.
        if let Some((start, len)) = crate::list::upload::span() {
            for chunk in (start..start + len).step_by(crate::list::upload::CHUNK) {
                upload.extend(m.map_index(chunk));
            }
        }
        for &address in records {
            let Some(handed) = crate::list::upload::handed_end(address) else {
                continue;
            };
            let from = address & !7;
            let end = handed.min(address.saturating_add(SCAN));
            if end <= from {
                continue;
            }
            let mut bytes = vec![0u8; (end - from) as usize];
            if crate::host::read_into(from as *const u8, &mut bytes) {
                for pointer in title_addresses(&bytes) {
                    named.extend(m.name_exact(pointer).into_iter().flatten());
                }
            }
        }
        // An address in two constants, or else in one in 256-byte units:
        // the low dword of a pair is not taken in 256-byte units as well
        // (717 of the 802 windows the builder's first frame named that way
        // were such low dwords).
        for (i, &c) in constants.iter().enumerate() {
            if let Some(&high) = constants.get(i + 1)
                && (1..=0xFF).contains(&high)
                && c >= 0x1_0000
            {
                named.extend(m.name_exact(u64::from(c) | u64::from(high) << 32).into_iter().flatten());
            } else if c >= 0x0100_0000 {
                named.extend(m.name_exact(u64::from(c) << 8).into_iter().flatten());
            }
        }
    } else {
        for &address in addresses {
            m.name(address);
        }
        // A title address in two constants (low dword, then bits 32 to 39), or
        // in one in 256-byte units. A constant that only looks like one names
        // memory nothing reads; one that is not the title's memory is refused.
        for (i, &c) in constants.iter().enumerate() {
            if let Some(&high) = constants.get(i + 1)
                && (1..=0xFF).contains(&high)
                && c >= 0x1_0000
            {
                m.name(u64::from(c) | u64::from(high) << 32);
            }
            if c >= 0x0100_0000 {
                m.name(u64::from(c) << 8);
            }
        }
    }
    let key = std::ptr::from_ref(heap) as usize;
    let (base, made) = match m.heaps.get(&key) {
        Some(&run) => run,
        None => {
            // Held for the heap's life: not given back with a list's slots.
            let base = heap.dynamic(copies_max() + 1)?;
            heap.null_raw_uav_at(base);
            (base, 0)
        }
    };
    for (n, shadow) in m.copies.iter().enumerate().skip(made as usize) {
        let (_, len) = shadow.span();
        heap.raw_uav_at(base + 1 + n as u32, &shadow.buffer, 0, len);
    }
    m.heaps.insert(key, (base, m.copies.len() as u32));
    if exact() {
        // Copies whose pages changed since the last look (numbered or moved
        // by the faults, the names or the buffers' changes above): brought
        // up to date again even if this list did already.
        let changed = m.recalculate.clone();
        m.update_ranges();
        // A list keeps every directory it was recorded with.
        const DIRECTORY: usize = usize::MAX - (1 << 40);
        let fresh = list.first_use(DIRECTORY - (m.revision as usize & 0xff_ffff));
        // Brought up to date at the list's submission, each once per list
        // (victory-fork pull request 994: all numbered copies for every list
        // made a median of 975 ms a frame with the builder's dispatches
        // bound and not run): a copy just numbered, the copies the list's
        // dispatches name and those of upload memory, and those whose pages
        // moved; declared written, with `writes`, all but upload memory's.
        // Only the pages the directory gives each copy (after DexNom/VIctory).
        // shortcut: memory a stage reaches only through pointers, in a copy
        // no dispatch names, is brought up to date when numbered and when
        // its pages move; the title's writes to it in between are not seen,
        // and the stage's writes there are not copied back.
        const NAMED: usize = usize::MAX - (1 << 41);
        let wanted: BTreeSet<usize> = (numbered..m.copies.len())
            .chain(named.iter().copied())
            .chain(upload.iter().copied())
            .collect();
        for n in wanted.union(&changed).copied() {
            let Some(shadow) = m.copies.get(n) else { continue };
            let again = changed.contains(&n);
            let ranges = m.ranges.get(n).map_or(&[][..], Vec::as_slice);
            if list.first_use(NAMED + 2 * n) || again {
                for &(start, len) in ranges {
                    shadow.refresh(list, start, len);
                }
            }
            if writes && !upload.contains(&n) && (list.first_use(NAMED + 2 * n + 1) || again) {
                for &(start, len) in ranges {
                    shadow.gpu_writes(list, start, len);
                }
            }
        }
        if fresh {
            list.keep(&m.directory.0);
            list.keep(&m.faults.0);
        }
        return Some(Ready {
            directory: m.directory.2,
            faults: m.faults.2,
            base,
            count: m.copies.len() as u32,
        });
    }
    // Once per list, and for copies numbered since: the builder alone
    // dispatches hundreds of times a frame.
    // shortcut: a copy another list numbers meanwhile is brought up to date
    // by that list's submission, which may run after this one's.
    const READ: usize = usize::MAX - 0x6d;
    const WRITE: usize = usize::MAX - 0x6e;
    let first_read = list.first_use(READ);
    let first_write = writes && list.first_use(WRITE);
    for (n, shadow) in m.copies.iter().enumerate() {
        let (start, len) = shadow.span();
        if first_read || n >= numbered {
            shadow.refresh(list, start, len);
        }
        if writes && (first_write || n >= numbered) {
            // Written at addresses the shaders compute: not marked.
            shadow.gpu_writes(list, start, len, false);
        }
    }
    if first_read {
        list.keep(&m.directory.0);
        list.keep(&m.faults.0);
    }
    Some(Ready {
        directory: m.directory.2,
        faults: m.faults.2,
        base,
        count: m.copies.len() as u32,
    })
}

#[cfg(test)]
mod record_tests {
    #[test]
    fn title_addresses_are_found_in_geometry_records() {
        // A geometry description as the builder copies it into upload
        // memory (48 bytes): no transform, index and vertex formats,
        // counts, the index buffer, the vertex buffer and its stride.
        let mut record = Vec::new();
        for v in [0u64, 0x10_0000_0035, 0x508_0000_1263, 0xb_13f6_53a0, 0xb_13f0_8bc0, 8] {
            record.extend_from_slice(&v.to_le_bytes());
        }
        let found: Vec<u64> = super::title_addresses(&record).collect();
        assert_eq!(found, [0xb_13f6_53a0, 0xb_13f0_8bc0]);
    }
}
