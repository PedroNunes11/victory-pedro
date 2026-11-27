//! Draws on the host GPU (decision 0007, step 3). A list keeps the state
//! the title sets for its draws (pipeline, descriptor heaps, root
//! arguments, vertex and index buffers, targets); each draw then brings the
//! host list up to date: the host pipeline and our root signature, the
//! user data the translated shaders read, the host copies of the buffers
//! and descriptors they read, and the states of the textures involved.
//! State the host keeps across draws (viewports, scissors, blend factor,
//! stencil reference, topology, targets) is set on the host list as the
//! title sets it.

use std::sync::atomic::{AtomicU64, Ordering};

use victory_shader::resource::RootIndexedView;
use victory_shader::{ResourceType, RootType, UsedResource};

use crate::descriptor::View;
use windows::Win32::Foundation::RECT;
use windows::Win32::Graphics::Direct3D::D3D_PRIMITIVE_TOPOLOGY;
use windows::Win32::Graphics::Direct3D12::*;
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT;
use windows::core::Interface;

use crate::host::HostList;
use crate::object::This;
use crate::pipeline::host::HostPipeline;

/// A root argument as the title set it.
#[derive(Debug, Clone, Default)]
enum Root {
    #[default]
    None,
    /// A descriptor table's first descriptor (an address in a heap).
    Table(u64),
    Constants(Vec<u32>),
    /// A root view's GPU address.
    View(u64),
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct VertexView {
    address: u64,
    size: u32,
    stride: u32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct IndexView {
    address: u64,
    size: u32,
    format: u32,
}

/// Host states of a depth target, and of a texture shaders read.
const DEPTH_WRITE: u32 = 0x10;
const DEPTH_READ: u32 = 0x20;
const SHADER_RESOURCE: u32 = 0x40 | 0x80;

thread_local! {
    /// While a draw is prepared (`State::bind`, then its targets' states):
    /// the depth target the draw only tests (0: none, or one it writes),
    /// and whether a view its shaders read is of that same texture.
    static TESTED_DEPTH: std::cell::Cell<(usize, bool)> =
        const { std::cell::Cell::new((0, false)) };
    /// While a stage's tables are walked (`State::tables`): whether that
    /// stage's shader can store to buffers (`Translated::stores`).
    static STORES: std::cell::Cell<bool> = const { std::cell::Cell::new(true) };
    /// Likewise: whether that stage's shader marks the pages it writes
    /// through views (`crate::marks`).
    static MARKED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// While `State::early_textures` runs the texture pass: the
    /// milliseconds and the bytes that run may take, in place of the
    /// pass's own per frame.
    static PASS_BUDGET: std::cell::Cell<Option<(u64, u64)>> = const { std::cell::Cell::new(None) };
    /// For the run of the texture pass `State::early_textures` makes
    /// before a bake draw: the title slots that draw samples
    /// (`State::bake_sources`), which the pass serves first, whatever
    /// their turn, the card's room and the pass's budget; and whether it
    /// serves nothing else.
    static PASS_SOURCES: std::cell::RefCell<Option<(Vec<u32>, bool)>> = const { std::cell::RefCell::new(None) };
    /// What that run did with them: uploaded, their bytes, left because
    /// their memory is not mapped, and left because no more of their
    /// memory is mapped than at their last upload.
    static PASS_SOURCED: std::cell::Cell<[u64; 4]> = const { std::cell::Cell::new([0; 4]) };
    /// The tables a pixel shader reads as a decoration bake does
    /// ([`bake_tables`]), by the shader's hash: asked at every draw.
    static BAKE_TABLES: std::cell::RefCell<std::collections::HashMap<String, Option<BakeTables>>> =
        std::cell::RefCell::new(std::collections::HashMap::new());
}

/// The milliseconds of every frame the pass that uploads textures picked
/// by index takes before a shader that picks by index has asked for it
/// (`VICTORY_EARLY_TEXTURES_MS`, 100; 0: never, as before), and the
/// milliseconds it may take just before a draw of the title's decoration
/// bake (`VICTORY_BAKE_TEXTURES_MS`, 20000; 0: nothing special).
///
/// (They were 12 and 1500. On a Radeon RX 7800 XT the first bakes then ran
/// with 8,000 to 14,000 textures still due, and the character baked first
/// kept a black face and stained clothes for the run, while characters
/// baked later, with the queue empty, came out right. With 100 and 20000
/// the first ones have their skin; loading takes a little longer.)
///
/// The pass ran only from the first world frame on, where it met some
/// 24,000 slots at once and needed 60-80 frames for them. The title bakes
/// its characters' skin and decorations into textures of its own while it
/// loads, 159-1,165 frames before that (runs 156-174), with a pixel shader
/// that picks its sources by index: whatever was not on the card then was
/// baked as nothing, once, for the run. On the console a texture is there
/// when its bytes are.
fn early_budgets() -> (u64, u64) {
    static MS: std::sync::OnceLock<(u64, u64)> = std::sync::OnceLock::new();
    *MS.get_or_init(|| {
        let ms = |name: &str, default: u64| {
            std::env::var(name).ok().and_then(|v| v.trim().parse::<u64>().ok()).unwrap_or(default)
        };
        (ms("VICTORY_EARLY_TEXTURES_MS", 100), ms("VICTORY_BAKE_TEXTURES_MS", 20000))
    })
}

/// The pixel shaders of the title's decoration bake, by the first 16 hex
/// digits of their hash as the
/// frame listings print it. They draw into a 256x256 corner of 2048x2048
/// targets and name no texture in a table.
const BAKE_PIXEL_SHADERS: [&str; 4] =
    ["c0a7fe77b1862e10", "0d1575a396dfe513", "b7cfd8dda91ebb6b", "280967ad158cee0b"];

/// Where a pixel shader of the title's decoration bake finds what it
/// samples: the root parameter of its one table, and in that table the
/// dwords of its buffer of decoration records and of its constants.
///
/// The bake names no texture in a table. Its constants hold the view heap
/// indices of up to five textures (bytes 0x20, 0x30, 0x40, 0x50 and 0x60)
/// and the records it walks (first at 0x78, the one past the last at 0x7c,
/// of the count at 0); a record is 304 bytes and holds five more indices
/// (bytes 0, 0x10, 0x20, 0x30 and 0x40). Read from the programs' code,
/// which is the same in all 322 of them but for the two table offsets, and
/// checked at every draw: an index counts only if the slot it names holds
/// a texture view.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct BakeTables {
    slot: u8,
    records: u32,
    constants: u32,
}

/// The stride of the bake's records.
const BAKE_RECORD: u64 = 304;
/// Where its constants and each of its records hold view heap indices.
const BAKE_CONSTANT_INDICES: [usize; 5] = [0x20, 0x30, 0x40, 0x50, 0x60];
const BAKE_RECORD_INDICES: [usize; 5] = [0, 0x10, 0x20, 0x30, 0x40];

/// [`BakeTables`] of pipeline `p`, if its pixel shader reads one table
/// with a buffer of 304-byte records and constants and nothing else
/// through tables.
fn bake_tables(p: &HostPipeline) -> Option<BakeTables> {
    let stage = p.stages.iter().find(|s| s.shader.stage == "pixel")?;
    let read = || {
        let mut tabled = stage.translated().used.iter().filter(|u| u.root == RootType::Table);
        let (a, b) = (tabled.next()?, tabled.next()?);
        if tabled.next().is_some() {
            return None;
        }
        let (records, constants) = if a.kind == ResourceType::Srv { (a, b) } else { (b, a) };
        (records.kind == ResourceType::Srv
            && u64::from(records.stride) == BAKE_RECORD
            && constants.kind == ResourceType::Cbv
            && records.slot == constants.slot)
            .then_some(BakeTables {
                slot: records.slot,
                records: records.table_offset_dwords,
                constants: constants.table_offset_dwords,
            })
    };
    BAKE_TABLES.with_borrow_mut(|known| match known.get(stage.shader.hash.as_str()) {
        Some(&tables) => tables,
        None => {
            let tables = read();
            known.insert(stage.shader.hash.clone(), tables);
            tables
        }
    })
}

/// The view heap indices a bake draw names, in order and each once, and
/// the records it walks with how many of them were read: from its
/// constants, and from the records `first..end` they give and the one
/// after them, which the program looks at as well; no more records than
/// the buffer holds (`held`), and no more than 256. `record` reads the
/// start of one.
fn bake_named(
    constants: &[u8; 0x80],
    held: u32,
    mut record: impl FnMut(u32) -> Option<[u8; 0x44]>,
) -> (Vec<u32>, (u32, u32)) {
    let word = |bytes: &[u8], at: usize| u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]]);
    let mut named: Vec<u32> = BAKE_CONSTANT_INDICES.iter().map(|&at| word(constants, at)).collect();
    let (first, end) = (word(constants, 0x78), word(constants, 0x7c));
    let mut read = 0;
    // (No record is looked at where none is walked.)
    let last = if end > first { end.saturating_add(1) } else { first };
    for i in first..last.min(held).min(first.saturating_add(256)) {
        let Some(record) = record(i) else {
            break;
        };
        named.extend(BAKE_RECORD_INDICES.iter().map(|&at| word(&record, at)));
        read += 1;
    }
    named.sort_unstable();
    named.dedup();
    (named, (end.saturating_sub(first), read))
}

/// Whether the textures a bake draw samples are uploaded before it,
/// whatever the queue's order and the card's room
/// (`VICTORY_BAKE_SOURCES=0`: they are only counted), and the megabytes
/// one draw may have uploaded so (`VICTORY_BAKE_SOURCES_MB`, 1024).
fn bake_sources_forced() -> (bool, u64) {
    static ON: std::sync::OnceLock<(bool, u64)> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        (
            std::env::var_os("VICTORY_BAKE_SOURCES").is_none_or(|v| v != "0"),
            std::env::var("VICTORY_BAKE_SOURCES_MB").ok().and_then(|v| v.trim().parse().ok()).unwrap_or(1024),
        )
    })
}

/// What a bake draw samples ([`State::bake_sources`]).
#[derive(Default, Debug)]
struct BakeSources {
    /// The records the draw walks, and how many of them were read.
    records: (u32, u32),
    /// Slots named that hold a texture view, and their handles.
    textures: Vec<(u32, u64)>,
    /// Of them: with their host texture made from that view and nothing
    /// waiting.
    ready: u32,
    /// Waiting in the queue of textures picked by index.
    queued: Vec<u32>,
    /// Neither, and a texture the title placed in memory that is there:
    /// the host slot shows nothing, and nothing would make it (the title
    /// wrote a newer view of memory this one clashes with, which made
    /// this one stale to the layer; the bake reads it all the same).
    unmade: Vec<u32>,
    /// Neither, and something else: a texture the host itself writes.
    other: u32,
}

/// Since the start: bytes uploaded for bake draws as their own textures
/// ([`State::bake_sources`]).
static BAKE_SOURCED_BYTES: AtomicU64 = AtomicU64::new(0);

/// Since the start: early runs of the texture pass, and those of them
/// made for a bake draw.
pub(crate) static EARLY_TEXTURE_PASSES: [AtomicU64; 2] = [AtomicU64::new(0), AtomicU64::new(0)];

/// Whether a buffer range a shader names through an unordered access view
/// is given to shaders as written (snapshot before the submission, compare
/// and write back after) only if that shader's code can store to buffers.
/// The title binds buffers its shaders only read through such views, and
/// every range declared costs a snapshot and a compare of its bytes: 3.97
/// GB a frame in 31 submissions, 0.49 s of snapshots and 0.28 s of
/// comparing (run 28). On by default; `VICTORY_WRITE_SKIP=0` declares
/// every such range, as before.
fn write_skip() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("VICTORY_WRITE_SKIP").is_none_or(|v| v != "0"))
}

/// A run of indirect records clamped in one pass ([`windows`]): the host
/// copy they came from (its number), their kind and stride, the offset of
/// the first in that copy, how many, and the list's scratch buffer that
/// holds them clamped, record `i` at `i * step`.
#[derive(Clone)]
struct Window {
    copy: u64,
    kind: u32,
    stride: u32,
    /// How far apart the records are in the copy, and in the scratch
    /// buffer: what the title walks them by ([`windows`]).
    step: u32,
    base: u64,
    records: u32,
    scratch: ID3D12Resource,
}

/// The most records clamped in one pass ([`windows`]).
const WINDOW_MOST: u64 = 64;

/// Whether indirect draws that read record after record of one argument
/// buffer are clamped a window at a time (`VICTORY_INDIRECT_WINDOW=1`; off
/// by default).
///
/// The title issues its mesh draws one record per call, each from the next
/// slot of the same argument buffer: 3,460 a frame in gameplay, and before
/// each our clamp ran as a compute dispatch of its own between two waits,
/// so the card went from drawing to compute and back for every record. In
/// the timing run of the lit scene (run 79) a shadow draw of next to
/// nothing cost 14-16 microseconds, 4,126 records a frame.
///
/// With the switch, when a draw asks for a record a short step after the
/// one drawn before, the clamp takes that record and the ones that step
/// apart after it (16, then twice as many each time, [`WINDOW_MOST`] at
/// most, never past the buffer) into
/// the list's scratch buffer in one pass, and the draws that follow read
/// their record there with no pass of ours and no barrier (the scratch
/// buffer stays readable as arguments until the next clamp), until one of
/// these: a compute
/// dispatch, a direct draw, an indirect call with a count buffer or more
/// than one record, a draw whose shaders can store to buffers, a record
/// outside the window, another host list.
///
/// The step is the one the title is seen to take, not the stride its
/// command signature names: it draws one record a call, for which the
/// stride means nothing, and the title names 12 bytes (the three
/// words of a mesh dispatch) for records it keeps 32 bytes apart (2,173 of
/// a frame's 3,349 mesh records in run 87's listed frame; 283 are 16
/// apart, which ends a window). Walked by the named stride the window
/// never opened: run 87, with the switch, still clamped 5,068 records a
/// frame one at a time.
///
/// What it costs the guard against garbage records: nothing against a
/// hang (every record is bounded as before and charged to the list's
/// budget as before), but the budget is charged when a window is clamped,
/// so also for records of the window's tail that are never drawn. Those
/// hold garbage in this title (slots of all ones: run 93 spent lists'
/// budgets on them and emptied the draws after), so in a window a record
/// that asks for more than can run is left empty and charges nothing
/// (`crate::indirect::bound_ahead`); a tail record that looks real is
/// charged what it asks, a few thousand of 2^27. And arguments the title's CPU side changes are seen as
/// before, at the next submission.
///
/// Off for the rest of the run from the first error the card's driver
/// reports to the system (`crate::drvwatch`): every loss of the card in a
/// run with windows was announced that way seconds ahead.
fn windows() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("VICTORY_INDIRECT_WINDOW").is_some_and(|v| v == "1")) && !crate::drvwatch::tripped()
}

/// Where the clamps of a run of indirect draws go: the list `clamps`, open,
/// that runs before the host list at `into`, as long as that one has
/// `writes` dispatches and copies (a later one may write the arguments);
/// the waits that stand before it.
pub(crate) struct Hoist {
    pub(crate) clamps: HostList,
    pub(crate) waits: Vec<crate::list::ValueWait>,
    pub(crate) into: usize,
    pub(crate) writes: u64,
}

/// What the recording of a draw or dispatch spends, by part, in
/// nanoseconds since the last line ([`DRAW_PART_NAMES`]; diagnostics).
#[cfg(feature = "diagnostics")]
pub(crate) static DRAW_PARTS: [AtomicU64; 13] = [const { AtomicU64::new(0) }; 13];
#[cfg(feature = "diagnostics")]
pub(crate) const DRAW_PART_NAMES: [&str; 13] = [
    "indirect: finding the arguments' and count's copies",
    "indirect: clamping the arguments",
    "bind: textures picked by index",
    "bind: descriptor tables",
    "bind: title memory by address",
    "bind: user data and root views",
    "bind: heaps, root signature, pipeline, root UAVs",
    "prepare: vertex and index buffers, targets",
    "of the tables: which are made afresh",
    "of the tables: the title's slots (table_entry)",
    "of the tables: a fresh table's key",
    "of the tables: fresh tables found made in the list",
    "of the tables: fresh tables made (slots, constant copies, views)",
];

/// Laps of a draw's recording into [`DRAW_PARTS`]; nothing without
/// diagnostics.
struct Laps {
    #[cfg(feature = "diagnostics")]
    at: crate::tick::Tick,
}

impl Laps {
    fn start() -> Self {
        Self {
            #[cfg(feature = "diagnostics")]
            at: crate::tick::Tick::now(),
        }
    }
}

/// The records of a window opened at a remembered step ([`jump_windows`]):
/// what the record after a lone one opens.
const WINDOW_JUMP: u32 = 16;

/// What a record that would be clamped alone opens instead, if the step
/// its argument buffer is walked by is remembered: that step and how many
/// records (never past the buffer, of which `left` bytes are left from
/// the record on). None if nothing is remembered for this buffer, kind
/// and stride, or no second record fits.
fn jump_window(remembered: Option<(u64, u32, u32, u32)>, (copy, kind, stride): (u64, u32, u32), left: u64) -> Option<(u32, u32)> {
    let (c, k, s, step) = remembered?;
    if (c, k, s) != (copy, kind, stride) || step == 0 {
        return None;
    }
    let fit = (left / u64::from(step)).min(u64::from(WINDOW_JUMP)) as u32;
    (fit > 1).then_some((step, fit))
}

/// Diagnostics of [`windows`]: indirect commands since the last line, by
/// what became of their clamp. In order: read from a window (no pass of
/// ours); then clamped by a pass of ours because: it is a dispatch; it
/// asks for more than one record; it has a count buffer; its shaders can
/// store to buffers; it is the list's first; there was no window on its
/// argument buffer (another buffer, kind or stride, or the window was
/// ended); its record does not continue the window's walk; it is past the
/// window's end (the next window: as meant). And the records those passes
/// clamped in all; the passes that opened a window at a remembered step
/// ([`jump_windows`]), and those of them no draw read from.
#[cfg(feature = "diagnostics")]
pub(crate) static WINDOW_WHY: [std::sync::atomic::AtomicU64; 12] = [const { std::sync::atomic::AtomicU64::new(0) }; 12];

/// The line of [`WINDOW_WHY`], per frame over `frames` frames.
#[cfg(feature = "diagnostics")]
pub(crate) fn window_line(frames: f64) {
    let n: Vec<f64> = WINDOW_WHY.iter().map(|n| n.swap(0, std::sync::atomic::Ordering::Relaxed) as f64 / frames.max(1.0)).collect();
    tracing::info!(
        "indirect commands per frame by their clamp: {:.0} read from a window (no pass of ours); a pass of ours for {:.0}: {:.0} dispatches, {:.0} of more than one record, {:.0} with a count \
         buffer, {:.0} whose shaders can store to buffers, {:.0} first on their list, {:.0} with no window on their argument buffer, {:.0} off the window's walk, {:.0} past the window's end; \
         {:.0} records clamped by those passes; {:.0} of the passes opened a window at a remembered step (VICTORY_WINDOW_JUMP=1), {:.0} of those windows were read by no draw",
        n[0],
        n[1..9].iter().sum::<f64>(),
        n[1],
        n[2],
        n[3],
        n[4],
        n[5],
        n[6],
        n[7],
        n[8],
        n[9],
        n[10],
        n[11],
    );
}

/// What one title list has set, and what its host list was last given.
#[derive(Default)]
pub(crate) struct State {
    /// The list before the host list recorded into that holds the clamps of
    /// its indirect draws (`crate::indirect::hoisting`), still open.
    hoist: Option<Hoist>,
    pipeline: usize,
    /// View heap, sampler heap.
    heaps: [usize; 2],
    graphics: Vec<Root>,
    compute: Vec<Root>,
    vertex: [VertexView; 32],
    index: IndexView,
    /// Bound render target and depth resources, for their host states.
    targets: Vec<usize>,
    depth: usize,
    /// The host state the bound depth view needs ([`depth_state`]).
    depth_state: u32,
    /// The title's primitive topology (the host's may differ).
    topology: u32,
    /// What the title set that goes straight onto the host list, kept for
    /// the host list that continues this one after a cut ([`State::replay`]):
    /// whether a topology was set, the render target and depth handles, the
    /// viewports, the scissors, the blend factor and the stencil reference.
    topology_set: bool,
    target_handles: Vec<u64>,
    depth_handle: u64,
    targets_set: bool,
    viewports: Vec<D3D12_VIEWPORT>,
    scissors: Vec<RECT>,
    blend: Option<[f32; 4]>,
    stencil: Option<u32>,
    depth_bounds: Option<[f32; 2]>,
    /// The indirect records clamped last, if later draws may still read
    /// them ([`windows`]).
    window: Option<Window>,
    /// With [`jump_windows`]: the argument buffer (its copy's number), kind
    /// and stride of the last window of several records that a draw read
    /// from, and that window's step.
    walk_step: Option<(u64, u32, u32, u32)>,
    /// Whether [`State::window`] was opened at the remembered step, and
    /// whether a draw has read from it since.
    window_jumped: bool,
    window_read: bool,
    /// Diagnostics: the argument buffer and the offset of the indirect
    /// command before this one on the list.
    #[cfg(feature = "diagnostics")]
    last_indirect: (u64, u64),
    /// What the host list has: pipeline, root signature, heaps.
    host_pipeline: usize,
    /// Graphics, compute.
    host_roots: [usize; 2],
    host_heaps: [usize; 2],
    /// Tables made afresh in this list: (table, slot count, contents) → first
    /// dynamic slot.
    fresh_tables: std::collections::HashMap<(u64, u32, u64), u32>,
    /// `VICTORY_TABLE_MEMO`: table entries made in this list, by what they
    /// are ([`EntryKey`]), with the list's [`HostList::changes`] after and
    /// the host copy of the texture they show (0 for a buffer).
    entry_memo: std::cell::RefCell<std::collections::HashMap<EntryKey, (u64, usize)>>,
    /// Step 3: [`State::cbv_in_place`]'s answers in this list, by the
    /// descriptor's address and bytes, with the list's
    /// [`HostList::changes`] and `crate::shadow::COPIES_EPOCH` they were
    /// given at: asked again only when either has moved.
    in_place_memo: std::cell::RefCell<std::collections::HashMap<(u64, [u64; 4]), (u64, u64, bool)>>,
    /// The dynamic slots the list holds: title heap, first, count; given
    /// back when the list has run, is reset or released.
    dynamic: Vec<(usize, u32, u32)>,
    /// Root SRVs bound as one-view tables in this list: (title heap, title
    /// address; 0 for none) → dynamic slot, and the host copy the slot
    /// shows: later draws of the list bring that one up to date, whatever
    /// copy the address would find by then (`shadow.rs` `tails`).
    root_srv_slots: std::collections::HashMap<(usize, u64), RootSlot>,
    /// Root UAVs bound as one-view tables in this list, likewise.
    root_uav_slots: std::collections::HashMap<(usize, u64), RootSlot>,
    /// With the structure builder asked for: per entry of the two above
    /// (`true` for a UAV) the title's buffer changes it was last checked
    /// against ([`crate::shadow::buffer_generation`], [`State::forget_moved_root`]).
    root_generations: std::collections::HashMap<(bool, usize, u64), u64>,
    /// The list ran and its dynamic slots were given back.
    tables_gone: bool,
    /// The state the last draw or dispatch ran with, but its arguments
    /// ([`State::census`]).
    #[cfg(feature = "diagnostics")]
    census_last: u64,
}

impl Drop for State {
    fn drop(&mut self) {
        for &(heap, first, n) in &self.dynamic {
            crate::descriptor::release_dynamic(heap, first, n);
        }
    }
}

impl State {
    /// The list's submission has run (submissions are waited for): the
    /// dynamic slots its tables were made in are given back, not kept
    /// until the title resets the list. A frame of the opening cutscene
    /// asks for 12,500 tables; held until the resets they ran the 131,072
    /// slots out, and up to 4,900 draws a frame went without their tables.
    /// shortcut: a list the title runs a second time without a reset then
    /// reads slots others may have taken (logged once).
    pub(crate) fn tables_done(&mut self) {
        if self.tables_gone && self.dynamic.is_empty() {
            return;
        }
        if self.tables_gone {
            static ONCE: std::sync::Once = std::sync::Once::new();
            ONCE.call_once(|| tracing::warn!("a list ran again after its descriptor tables were given back"));
        }
        for (heap, first, n) in self.dynamic.drain(..) {
            crate::descriptor::release_dynamic(heap, first, n);
        }
        self.fresh_tables.clear();
        self.entry_memo.get_mut().clear();
        self.in_place_memo.get_mut().clear();
        self.root_srv_slots.clear();
        self.root_uav_slots.clear();
        self.root_generations.clear();
        self.tables_gone = true;
    }
}

/// An experiment on the title's scissor rectangles (`VICTORY_SCISSOR_TEST=full`):
/// they are all widened to the whole target. The title's room at
/// the start of the prologue lost its walls in five runs of seven, and in
/// those runs the player's body and the world outside were cut by one
/// rectangle around the window (run 85, frame 2046): one run with this on
/// says whether the walls are drawn and cut away by such a rectangle, or
/// not drawn at all. Off by default: a rectangle the title sets is part of
/// its picture.
fn scissor_test() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var("VICTORY_SCISSOR_TEST").as_deref() == Ok("full"))
}

/// An experiment on mesh draws (`VICTORY_MESH_TEST`): 1 for `skip` (none
/// is issued), 2 for `scissor` (each runs its shaders but rasterises one
/// pixel), 0 otherwise. It tells a card hung by a mesh shader from one hung
/// by what the shader's triangles cover.
fn mesh_test() -> u8 {
    static ON: std::sync::OnceLock<u8> = std::sync::OnceLock::new();
    *ON.get_or_init(|| match std::env::var("VICTORY_MESH_TEST").as_deref() {
        Ok("skip") => 1,
        Ok("scissor") => 2,
        _ => 0,
    })
}

/// `VICTORY_MESH_TEST=keep:<hex digits>[:<hex digits>]`: only the mesh
/// draws whose mesh shader's hash ends in one of the first digits (and
/// whose digit before it is one of the second, if given) are issued: a
/// search for the shaders that stop the card.
fn mesh_kept(pipeline: usize) -> bool {
    static KEEP: std::sync::OnceLock<Option<(String, String)>> = std::sync::OnceLock::new();
    let Some((last, before)) = KEEP.get_or_init(|| {
        let value = std::env::var("VICTORY_MESH_TEST").ok()?;
        let mut parts = value.strip_prefix("keep:")?.split(':');
        Some((parts.next()?.to_owned(), parts.next().unwrap_or("").to_owned()))
    }) else {
        return true;
    };
    let Some(p) = crate::pipeline::host_pipeline(pipeline as This) else {
        return true;
    };
    let Some(at) = p.name.find("mesh ") else {
        return true;
    };
    let hash = p.name[at + 5..].as_bytes();
    if hash.len() < 16 {
        return true;
    }
    last.as_bytes().contains(&hash[15]) && (before.is_empty() || before.as_bytes().contains(&hash[14]))
}

/// Draws recorded on the host GPU since the last frame line.
static DRAWN: AtomicU64 = AtomicU64::new(0);

/// Draws run on the host so far.
pub(crate) fn drawn() -> u64 {
    DRAWN.load(Ordering::Relaxed)
}

/// The draws traced since the last present (diagnostics builds, with
/// debug logging for this module): logged when the frame comes out black.
#[cfg(feature = "diagnostics")]
static FRAME: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

/// The draws traced since the last call.
#[cfg(feature = "diagnostics")]
pub(crate) fn take_frame() -> Vec<String> {
    std::mem::take(&mut *FRAME.lock().unwrap_or_else(|e| e.into_inner()))
}

/// Draws, mesh dispatches and dispatches recorded but not run on the host
/// (no host pipeline) since the last call.
pub(crate) fn take_dropped() -> u64 {
    DROPPED.swap(0, Ordering::Relaxed)
}
/// A root SRV's dynamic slot in a list, and the host copy it shows.
type RootSlot = (u32, Option<std::sync::Arc<crate::shadow::Shadow>>);

/// Buffer views of the heap whose slot showed another host buffer than
/// the copy the pass brought up to date for them, since the start
/// (diagnostics; for the memory line). It stays 0 while the pass and the
/// slots agree; with `VICTORY_VIEW_EXTENT=0` it counts the views that were
/// read stale.
#[cfg(feature = "diagnostics")]
pub(crate) static HEAP_VIEWS_ELSEWHERE: AtomicU64 = AtomicU64::new(0);
/// Passes over the view heap's buffer views for shaders that pick them by
/// index ([`State::heap_buffers`]), and the microseconds they took.
pub(crate) static HEAP_BUFFER_PASSES: AtomicU64 = AtomicU64::new(0);
pub(crate) static HEAP_BUFFER_MICROS: AtomicU64 = AtomicU64::new(0);
/// Where the pass over textures picked by index records its uploads: into
/// a list of the title's, or into lists of the thread that runs it ahead.
pub(crate) enum Into<'a> {
    List(&'a HostList),
    Ahead(&'a mut crate::heap_worker::Batch),
}

/// Of [`HEAP_BUFFER_MICROS`], the microseconds of the pass over textures
/// picked by index that runs inside it (`heap_textures`), and the buffer
/// slots the pass over buffer views looked at; since the last frame line.
pub(crate) static HEAP_PASS_PARTS: [AtomicU64; 5] = [const { AtomicU64::new(0) }; 5];

/// Since the last frame line (diagnostics), of the passes over textures
/// picked by index (`heap_textures`, every thread): microseconds making
/// slots (the host texture made there if it was not), and the slots;
/// microseconds making their textures readable (staging, detiling, the
/// upload recorded), and the textures.
#[cfg(feature = "diagnostics")]
pub(crate) static HEAP_TEXTURE_PARTS: [AtomicU64; 4] = [const { AtomicU64::new(0) }; 4];
/// Textures brought up to date for shaders that pick them by index
/// ([`State::heap_textures`]), the bytes of their memory, and the slots
/// still waiting.
pub(crate) static HEAP_TEXTURES: AtomicU64 = AtomicU64::new(0);
pub(crate) static HEAP_TEXTURE_BYTES: AtomicU64 = AtomicU64::new(0);
pub(crate) static HEAP_TEXTURES_WAITING: AtomicU64 = AtomicU64::new(0);
/// Passes that made no texture because the card had no room
/// (`crate::resource::room`), for the memory line.
pub(crate) static HEAP_TEXTURES_FULL: AtomicU64 = AtomicU64::new(0);
/// Since the start, for the memory line: slots not made because the card
/// had no room, and slots made while it had none, without their largest
/// mips (`crate::resource::ShortOfRoom`).
pub(crate) static HEAP_TEXTURES_REFUSED: AtomicU64 = AtomicU64::new(0);
pub(crate) static HEAP_TEXTURES_SHORT: AtomicU64 = AtomicU64::new(0);

/// Where the heap's pass ([`State::heap_buffers`]) spends its time, since
/// the last frame line (diagnostics builds; [`take_heap_pass_split`]):
/// indices are the constants of [`split`]. Only the passes the heap's pass
/// runs are counted here, not the early ones before a frame's first draws.
#[cfg(feature = "diagnostics")]
static HEAP_PASS_SPLIT: [AtomicU64; split::LEN] = [const { AtomicU64::new(0) }; split::LEN];

/// The parts of [`HEAP_PASS_SPLIT`].
pub(crate) mod split {
    /// Microseconds mirroring the buffer views' slots, and slots looked at.
    pub(crate) const SLOTS_US: usize = 0;
    pub(crate) const SLOTS: usize = 1;
    /// Microseconds bringing host copies up to date, the copies, and the
    /// bytes their spans cover.
    pub(crate) const REFRESH_US: usize = 2;
    pub(crate) const SPANS: usize = 3;
    pub(crate) const SPAN_BYTES: usize = 4;
    /// The texture pass before its loop: slots held back on a full card
    /// released and memory the runtime mapped, the sweep of views, the
    /// looks at uploaded textures, and the levels remade with the queue's
    /// due slots and the card's accounting.
    pub(crate) const REMAPPED_US: usize = 5;
    pub(crate) const SWEEP_US: usize = 6;
    pub(crate) const RELOOK_US: usize = 7;
    pub(crate) const DUE_US: usize = 8;
    /// The texture pass's loop: microseconds in slots that uploaded a
    /// texture, how many, the microseconds of those in making the host
    /// view (and with it the host texture), and microseconds in the slots
    /// that uploaded nothing.
    pub(crate) const MADE_US: usize = 9;
    pub(crate) const MADE: usize = 10;
    pub(crate) const MIRROR_US: usize = 11;
    pub(crate) const OTHER_US: usize = 12;
    /// Slots passed over unmapped, and slots refused for want of room.
    pub(crate) const AWAY: usize = 13;
    pub(crate) const REFUSED: usize = 14;
    /// Passes that had room, passes whose loop stopped on its time, and
    /// passes whose time was the full card's ([`super::full_card_span`]).
    pub(crate) const WITH_ROOM: usize = 15;
    pub(crate) const STOPPED_ON_TIME: usize = 16;
    pub(crate) const CAPPED: usize = 17;
    /// The longest pass, and the longest single slot of the loop
    /// (microseconds; maxima, not sums).
    pub(crate) const LONGEST_PASS_US: usize = 18;
    pub(crate) const LONGEST_SLOT_US: usize = 19;
    /// Of the buffer views' slots (diagnostics builds): microseconds making
    /// their host views (`HostHeap::mirror`) and finding the copy each
    /// shows (`descriptor::host::buffer_copy`), and of the slots, those of
    /// constant buffer views and their microseconds in both.
    #[cfg_attr(not(feature = "diagnostics"), allow(dead_code))]
    pub(crate) const SLOT_MIRROR_US: usize = 20;
    #[cfg_attr(not(feature = "diagnostics"), allow(dead_code))]
    pub(crate) const SLOT_COPY_US: usize = 21;
    #[cfg_attr(not(feature = "diagnostics"), allow(dead_code))]
    pub(crate) const CONSTANT_SLOTS: usize = 22;
    #[cfg_attr(not(feature = "diagnostics"), allow(dead_code))]
    pub(crate) const CONSTANT_US: usize = 23;
    #[cfg_attr(not(feature = "diagnostics"), allow(dead_code))]
    pub(crate) const LEN: usize = 24;
}

/// Adds `v` to part `i` of [`HEAP_PASS_SPLIT`] (nothing without
/// diagnostics).
#[inline]
fn split_add(i: usize, v: u64) {
    #[cfg(feature = "diagnostics")]
    HEAP_PASS_SPLIT[i].fetch_add(v, Ordering::Relaxed);
    #[cfg(not(feature = "diagnostics"))]
    let _ = (i, v);
}

/// Raises part `i` of [`HEAP_PASS_SPLIT`] to `v` (a maximum).
#[inline]
fn split_max(i: usize, v: u64) {
    #[cfg(feature = "diagnostics")]
    HEAP_PASS_SPLIT[i].fetch_max(v, Ordering::Relaxed);
    #[cfg(not(feature = "diagnostics"))]
    let _ = (i, v);
}

/// The microseconds since `at`.
fn micros(at: std::time::Instant) -> u64 {
    at.elapsed().as_micros() as u64
}

/// [`HEAP_PASS_SPLIT`] since the last call, and from zero again.
#[cfg(feature = "diagnostics")]
pub(crate) fn take_heap_pass_split() -> [u64; split::LEN] {
    HEAP_PASS_SPLIT.each_ref().map(|c| c.swap(0, Ordering::Relaxed))
}

/// When the pass over textures picked by index last found the card without
/// room for new ones (`crate::host::now_ms` plus one; 0: never).
static LAST_FULL_MS: AtomicU64 = AtomicU64::new(0);

/// How long the card counts as at its line after the pass last found it
/// without room.
const FULL_LATELY_MS: u64 = 10_000;

/// The time a pass over textures picked by index may take while the card
/// is at its line for new host textures (it found no room in the last ten
/// seconds), in place of the pass's own (`VICTORY_TEXTURE_STAGE_MS`, three
/// times that for a backlog): `VICTORY_TEXTURE_FULL_MS`, 50; 0: none, as
/// before. Bake passes keep theirs.
///
/// With the card at its line the queue only moves as fast as host
/// textures are let go: the open world held 9,000 to 15,000 slots due, so
/// the backlog's threefold time was always on, and when some tens of MB
/// were let go the next pass made textures for up to 750 ms of one frame
/// (some 1,500 small ones, about 2 ms each), where the pass otherwise took
/// 15 to 35 ms: the frames of 0.6 to 1 s among frames of 0.3 s. Spread over
/// frames, the same textures are made in the same order.
///
/// With the slots a full card turns down held back ([`full_card_pass`]),
/// the first pass that finds room makes all of them due at once, which is
/// when the burst comes: this time bounds that pass, and the rest wait
/// for the passes after it. The time also bounds a pass that holds slots
/// back: one that stops on it before its 1,025th refusal leaves the rest
/// due for the next pass, which goes on past the ones held.
pub(crate) fn full_card_span() -> Option<std::time::Duration> {
    static MS: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
    let ms = *MS.get_or_init(|| {
        std::env::var("VICTORY_TEXTURE_FULL_MS").ok().and_then(|v| v.trim().parse().ok()).unwrap_or(50)
    });
    let last = LAST_FULL_MS.load(Ordering::Relaxed);
    (ms > 0 && last > 0 && crate::host::now_ms().saturating_sub(last - 1) < FULL_LATELY_MS)
        .then(|| std::time::Duration::from_millis(ms))
}

/// The bytes and the time of the heap's pass over textures picked by index:
/// its own (`per_frame`, `span`), three times both for a `backlog`, and no
/// more time than the full card's (`full`, [`full_card_span`]) while the
/// card is at its line, backlog or not.
fn texture_pass_budget(
    per_frame: u64,
    span: std::time::Duration,
    backlog: bool,
    full: Option<std::time::Duration>,
) -> (u64, std::time::Duration) {
    match full {
        Some(full) => (per_frame, full.min(span)),
        None if backlog => (per_frame * 3, span * 3),
        None => (per_frame, span),
    }
}

/// Since the start, for textures picked by index that were uploaded whole
/// and are looked at again ([`relook_textures`]): looks made, looks that
/// found the memory changed (each puts the slot back for an upload), and
/// uploads made for such a find.
pub(crate) static HEAP_TEXTURES_RECHECKED: [AtomicU64; 3] = [const { AtomicU64::new(0) }; 3];

/// The most looks at uploaded textures a frame makes, and the longest it
/// spends on them: a fingerprint reads up to 48 KiB, and the slots uploaded
/// in one burst come due again together. What is left waits for the next
/// frame; the uploads the looks ask for come after every slot never
/// uploaded and within the pass's own budget.
const RELOOK_MOST: usize = 256;
const RELOOK_SPAN: std::time::Duration = std::time::Duration::from_millis(8);

/// Uploads of textures already on the card made while it had no room for
/// new ones, since the start.
pub(crate) static HEAP_TEXTURES_REUPLOADED_FULL: AtomicU64 = AtomicU64::new(0);

/// A bake draw's own sources go first in the pass before it, and may be
/// made while the card is past the line for new host textures, up to a
/// higher one (`crate::resource::room_for_bake`). With the card at its line
/// (a save in the open world: 13.6 GB in use of a budget of 14.8 GB, 1,592
/// passes that made nothing) the passes before the late bakes made nothing
/// from 9,000 to 13,000 due slots, the bakes sampled textures never made,
/// and the characters' faces came out black. `VICTORY_BAKE_SOURCES_FIRST=0`:
/// the bake's sources wait in the queue with the rest, as before.
fn bake_sources_first() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("VICTORY_BAKE_SOURCES_FIRST").is_none_or(|v| v != "0"))
}

thread_local! {
    /// The title slots the bake draw being bound names (its sources), for
    /// the pass that runs before it ([`State::heap_textures`]).
    static BAKE_SOURCES: std::cell::RefCell<Vec<u32>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// Since the start, for the passes before bake draws: the bakes' sources
/// found due, those of them uploaded (or brought up to date), and those
/// made while the card was past the line for new host textures.
pub(crate) static BAKE_SOURCES_SEEN: [AtomicU64; 3] = [const { AtomicU64::new(0) }; 3];
/// Whether a pass over the textures picked by index on a full card holds
/// back the slots that need a new host copy (`VICTORY_FULL_CARD_PASS=0`:
/// no, as before: they stay due, each pass looks at the same ones again
/// and stops at the 257th). Held back, such a slot is looked at again
/// after about four seconds or as soon as the card has room, and the pass
/// goes on to the slots behind it: the ones whose host copy exists and
/// only wants bringing up to date, and the ones whose memory went (letting
/// their host copies go is what makes room). What is uploaded while the
/// card has room is the same either way.
fn full_card_pass() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("VICTORY_FULL_CARD_PASS").is_none_or(|v| v != "0"))
}

/// Whether a texture that has its host copy is still brought up to date
/// while the card has no room for new ones (`VICTORY_REUPLOAD_WHEN_FULL=0`:
/// no).
fn reupload_when_full() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("VICTORY_REUPLOAD_WHEN_FULL").is_none_or(|v| v != "0"))
}

/// Whether textures picked by index are looked at again after their upload
/// (`VICTORY_HEAP_RECHECK=0`: no).
fn heap_recheck() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("VICTORY_HEAP_RECHECK").is_none_or(|v| v != "0"))
}
/// Since the last frame line, uploads of textures picked by index by the
/// frames their slot had been due (`TextureQueue::waited`), in powers of
/// two: bucket 0 for 0 frames, `b` for 2^(b-1) to 2^b - 1, the last for
/// 2^(N-2) and more; and the most frames one waited.
pub(crate) const WAITED_BUCKETS: usize = 13;
pub(crate) static HEAP_WAITED: [AtomicU64; WAITED_BUCKETS] = [const { AtomicU64::new(0) }; WAITED_BUCKETS];
pub(crate) static HEAP_WAITED_MOST: AtomicU64 = AtomicU64::new(0);

/// The bucket of [`HEAP_WAITED`] for `frames`.
pub(crate) fn waited_bucket(frames: u64) -> usize {
    ((64 - frames.leading_zeros()) as usize).min(WAITED_BUCKETS - 1)
}

/// From the buckets of [`HEAP_WAITED`]: the uploads, and the frames under
/// which half and 95 in 100 of them waited (the upper end of their
/// bucket; `u64::MAX` for the last).
pub(crate) fn waited_summary(buckets: &[u64; WAITED_BUCKETS]) -> (u64, u64, u64) {
    let n: u64 = buckets.iter().sum();
    let upper = |b: usize| if b + 1 == WAITED_BUCKETS { u64::MAX } else { (1u64 << b) - 1 };
    let at = |share: u64| {
        let need = (n * share).div_ceil(100).max(1);
        let mut seen = 0;
        for (b, &count) in buckets.iter().enumerate() {
            seen += count;
            if seen >= need {
                return upper(b);
            }
        }
        0
    };
    if n == 0 { (0, 0, 0) } else { (n, at(50), at(95)) }
}

/// Times a placed texture's slot was passed over because its memory is not
/// mapped.
pub(crate) static HEAP_TEXTURES_AWAY: AtomicU64 = AtomicU64::new(0);

/// Of [`HEAP_TEXTURES_AWAY`], the readings of nothing mapped that did NOT
/// let the slot go because no earlier look had found it away: a mapping
/// change of the runtime's, not a texture streamed out.
pub(crate) static HEAP_TEXTURES_AWAY_HELD: AtomicU64 = AtomicU64::new(0);

/// How many looks must already have found a slot's texture memory wholly
/// unmapped before the slot is let go (`VICTORY_TEXTURE_GONE_LOOKS`, 1;
/// 0 lets it go on the first reading, as before).
fn gone_looks() -> u32 {
    static N: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
    *N.get_or_init(|| std::env::var("VICTORY_TEXTURE_GONE_LOOKS").ok().and_then(|v| v.trim().parse().ok()).unwrap_or(1))
}
/// Slots the pass found up to date already: their texture is uploaded by
/// the draws that read it through a table (it has a staging buffer).
pub(crate) static HEAP_TEXTURES_TABLED: AtomicU64 = AtomicU64::new(0);
/// The queue of textures picked by index at the last pass
/// (`TextureQueue::census`): slots in it, due for an upload, waiting for
/// their memory, uploaded in part, the frames the oldest due one has
/// waited, slots never uploaded, slots uploaded whole that wait for a look
/// at their memory, slots a look found changed that wait for their upload,
/// and looks due.
pub(crate) static HEAP_TEXTURES_QUEUE: [AtomicU64; 9] = [const { AtomicU64::new(0) }; 9];
/// Since the start: looks at a slot uploaded in part that found no more of
/// its memory mapped; uploads of textures not all mapped; and of those,
/// the ones whose first page was not there (asked by the first page alone,
/// as before, they counted as not there at all).
pub(crate) static HEAP_TEXTURES_PARTLY: [AtomicU64; 3] = [const { AtomicU64::new(0) }; 3];
static DROPPED: AtomicU64 = AtomicU64::new(0);

/// Marks the last traced draw as not run on the host.
#[cfg(feature = "diagnostics")]
fn dropped(pipeline: usize) {
    crate::pipeline::note_drop(pipeline as This);
    if let Some(last) = FRAME.lock().unwrap_or_else(|e| e.into_inner()).last_mut() {
        last.push_str(" DROPPED");
    }
}

/// Dispatches run on the host so far.
static DISPATCHED: AtomicU64 = AtomicU64::new(0);

/// Dispatches run in parts since the last frame line: direct ones of more
/// groups than the host runs in one dimension, and indirect ones that went
/// through the split pass (of which the pass knows how many asked for more,
/// `indirect::bound_split`).
static IN_PARTS: [AtomicU64; 2] = [const { AtomicU64::new(0) }; 2];

pub(crate) fn take_in_parts() -> [u64; 2] {
    [0, 1].map(|i| IN_PARTS[i].swap(0, Ordering::Relaxed))
}

pub(crate) fn dispatched() -> u64 {
    DISPATCHED.load(Ordering::Relaxed)
}

/// The host address of a root view: its buffer's host copy, or for a
/// constant buffer not 256-byte aligned there (the console needs 16) a
/// copy in upload memory.
/// shortcut: a root view has no size; constant buffers are taken as 64 KiB
/// (cut at their buffer's end), other views as all that is left of their
/// buffer. (They were taken as 1 MiB: 1,452 of the 2,021 mesh vertex
/// streams of a cutscene frame reach further, and what lay past the first
/// megabyte was never brought up to date - meshes without bodies, exploded
/// triangles. `VICTORY_ROOT_VIEW_MB` sets a limit again.)
/// For a root constant buffer at title `address` whose host copy places it
/// off 256 bytes: the host address 256-aligned below it in that copy, and
/// the rows between (contract 3); `None` if it is on 256 already or the
/// address below lies before the copy (it then gets a copy of its own,
/// `root_view`, and the shader adds no rows).
fn root_cbv_rows(address: u64) -> Option<(u64, u32)> {
    if !crate::shader_cache::cbv_row_shift() {
        return None;
    }
    let shadow = crate::shadow::find(address, 1)?;
    // As tables' views: only copies in the card's memory
    // (`descriptor::host::in_place_upload`).
    if !shadow.in_place_ok() || crate::shadow::rewritten(address, 1 << 16) {
        return None;
    }
    let gpu = shadow.gpu(address);
    let below = gpu & !255;
    let (start, _) = shadow.span();
    (gpu % 256 != 0 && (gpu - below) % 16 == 0 && below >= shadow.gpu(start))
        .then_some((below, ((gpu - below) / 16) as u32))
}

/// [`root_cbv_rows`]' address, with the constants' range brought up to
/// date at submission as `root_view` does.
fn root_cbv_below(list: &HostList, address: u64) -> Option<(u64, u32)> {
    let below = root_cbv_rows(address)?;
    let shadow = crate::shadow::find(address, 1)?;
    shadow.refresh(list, address, shadow.left(address).min(1 << 16));
    Some(below)
}

fn root_view(list: &HostList, address: u64, srv: bool) -> Option<u64> {
    static LIMIT: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
    let limit = *LIMIT.get_or_init(|| {
        std::env::var("VICTORY_ROOT_VIEW_MB")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .map_or(u64::MAX, |mb| mb << 20)
    });
    let shadow = crate::shadow::find(address, 1)?;
    let len = shadow
        .left(address)
        .min(if srv { limit } else { 1 << 16 });
    let gpu = shadow.gpu(address);
    if srv || gpu % 256 == 0 {
        shadow.refresh(list, address, len);
        return Some(gpu);
    }
    list.fill_scratch(address as *const u8, len)
}

macro_rules! once {
    ($($arg:tt)*) => {{
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(|| tracing::warn!($($arg)*));
    }};
}

fn set(roots: &mut Vec<Root>, slot: u32, root: Root) {
    let slot = slot as usize;
    if roots.len() <= slot {
        roots.resize(slot + 1, Root::None);
    }
    roots[slot] = root;
}

impl State {
    /// Whether the clamps of indirect draws recorded in `list` now go to
    /// the list before it.
    pub(crate) fn hoists_into(&self, list: &HostList) -> bool {
        self.hoist
            .as_ref()
            .is_some_and(|h| h.into == std::ptr::from_ref(list) as usize && h.writes == list.writes())
    }

    /// From here on the clamps of `into`'s indirect draws go to a list
    /// before it. `waits`: those that stand before that list.
    pub(crate) fn hoist(&mut self, clamps: HostList, waits: Vec<crate::list::ValueWait>, into: &HostList) {
        debug_assert!(self.hoist.is_none());
        self.hoist = Some(Hoist {
            clamps,
            waits,
            into: std::ptr::from_ref(into) as usize,
            writes: into.writes(),
        });
    }

    /// Ends and closes the list of hoisted clamps, if any, and gives it
    /// back: it runs before the host list that draws.
    pub(crate) fn end_hoist(&mut self) -> Option<Hoist> {
        let h = self.hoist.take()?;
        h.clamps.clamp_end();
        h.clamps.close();
        Some(h)
    }

    pub(crate) fn reset(&mut self, pipeline: This) {
        *self = State::default();
        self.pipeline = pipeline as usize;
    }

    /// The title's topology; console-only ones become host lists (see
    /// [`crate::primitive`]).
    pub(crate) fn set_topology(&mut self, list: &HostList, topology: u32) {
        self.topology = topology;
        self.topology_set = true;
        let host = match topology {
            RECTANGLES => 4,
            TRIANGLE_FAN | LINE_LOOP | QUADS | QUAD_STRIP => {
                once!(topology, "console topology drawn as a triangle list");
                4
            }
            t => t,
        };
        // SAFETY: an open list.
        unsafe {
            list.list
                .IASetPrimitiveTopology(D3D_PRIMITIVE_TOPOLOGY(host as i32))
        };
    }

    pub(crate) fn set_pipeline(&mut self, pipeline: This) {
        self.pipeline = pipeline as usize;
    }

    pub(crate) fn set_heaps(&mut self, heaps: &[This]) {
        self.heaps = [0; 2];
        for &h in heaps {
            if !h.is_null() {
                let samplers = crate::descriptor::heap(h).samplers();
                self.heaps[usize::from(samplers)] = h as usize;
            }
        }
    }

    fn roots(&mut self, compute: bool) -> &mut Vec<Root> {
        if compute {
            &mut self.compute
        } else {
            &mut self.graphics
        }
    }

    pub(crate) fn set_table(&mut self, compute: bool, slot: u32, handle: u64) {
        set(self.roots(compute), slot, Root::Table(handle));
    }

    pub(crate) fn set_view(&mut self, compute: bool, slot: u32, address: u64) {
        set(self.roots(compute), slot, Root::View(address));
    }

    pub(crate) fn set_constants(&mut self, compute: bool, slot: u32, values: &[u32], offset: u32) {
        let roots = self.roots(compute);
        let s = slot as usize;
        if !matches!(roots.get(s), Some(Root::Constants(_))) {
            set(roots, slot, Root::Constants(Vec::new()));
        }
        if let Root::Constants(c) = &mut roots[s] {
            let end = offset as usize + values.len();
            if c.len() < end {
                c.resize(end, 0);
            }
            c[offset as usize..end].copy_from_slice(values);
        }
    }

    pub(crate) fn set_vertex_buffers(&mut self, start: u32, views: &[VertexView]) {
        for (i, v) in views.iter().enumerate() {
            if let Some(slot) = self.vertex.get_mut(start as usize + i) {
                *slot = *v;
            }
        }
    }

    pub(crate) fn clear_vertex_buffers(&mut self, start: u32, count: u32) {
        for slot in self.vertex.iter_mut().skip(start as usize).take(count as usize) {
            *slot = VertexView::default();
        }
    }

    pub(crate) fn set_index_buffer(&mut self, view: Option<&IndexView>) {
        self.index = view.copied().unwrap_or_default();
    }

    /// Render targets and depth, set on the host list at once.
    pub(crate) fn set_targets(&mut self, list: &HostList, handles: &[u64], depth: u64) {
        let Some(host) = crate::host::host() else {
            return;
        };
        self.target_handles.clear();
        self.target_handles.extend_from_slice(handles);
        self.depth_handle = depth;
        self.targets_set = true;
        self.targets.clear();
        let mut rtvs = Vec::new();
        for &h in handles {
            let Some(record) = crate::descriptor::rtv_record(h) else {
                once!(
                    handle = format_args!("{h:#x}"),
                    "render target without a recorded view"
                );
                continue;
            };
            let Some(texture) = crate::resource::host_resource(record.resource) else {
                // A typed null view (or a target without a host copy) still
                // takes its slot: left out, the next live target would move
                // into it and the pixel shader's outputs reach the wrong
                // targets.
                // shortcut: a view without a description or a format has no
                // null view to stand for it and is left out, as before.
                let null = record
                    .desc()
                    .filter(|d| d.Format.0 != 0 && d.ViewDimension.0 != 0);
                if let Some(desc) = null {
                    rtvs.push(host.rtv(None, Some(&desc)));
                }
                continue;
            };
            self.targets.push(record.resource as usize);
            #[cfg(feature = "diagnostics")]
            crate::capture::note_target(record.resource);
            rtvs.push(host.rtv(Some(texture), record.desc().as_ref()));
        }
        self.depth = 0;
        self.depth_state = DEPTH_WRITE;
        let dsv = crate::descriptor::dsv_record(depth).and_then(|record| {
            let texture = crate::resource::host_resource(record.resource)?;
            self.depth = record.resource as usize;
            self.depth_state = depth_state(texture, record.depth_desc().as_ref());
            if crate::resource::depth_and_unordered(record.resource) {
                crate::resource::DEPTH_UNORDERED[1].fetch_add(1, Ordering::Relaxed);
            }
            #[cfg(feature = "diagnostics")]
            crate::capture::note_target(record.resource);
            Some(host.dsv(texture, record.depth_desc().as_ref()))
        });
        // SAFETY: an open list; the views were just made.
        unsafe {
            list.list.OMSetRenderTargets(
                rtvs.len() as u32,
                (!rtvs.is_empty()).then_some(rtvs.as_ptr()),
                false,
                dsv.as_ref().map(std::ptr::from_ref),
            )
        };
    }

    /// Viewports, scissors, the blend factor and the stencil reference: set
    /// on the host list at once, and kept for [`State::replay`].
    pub(crate) fn set_viewports(&mut self, list: &HostList, viewports: &[D3D12_VIEWPORT]) {
        self.viewports.clear();
        self.viewports.extend_from_slice(viewports);
        crate::draw::viewports(list, viewports);
    }

    pub(crate) fn set_scissors(&mut self, list: &HostList, rects: &[RECT]) {
        self.scissors.clear();
        self.scissors.extend_from_slice(rects);
        if scissor_test() {
            // The experiment: every rectangle the title sets is widened to
            // the largest target there is, so nothing is cut by one. What
            // the listing shows (`op_text`) stays the title's own.
            let full = RECT {
                left: 0,
                top: 0,
                right: 16384,
                bottom: 16384,
            };
            crate::draw::scissors(list, &vec![full; rects.len()]);
            return;
        }
        crate::draw::scissors(list, rects);
    }

    pub(crate) fn set_blend_factor(&mut self, list: &HostList, factor: Option<&[f32; 4]>) {
        self.blend = factor.copied();
        crate::draw::blend_factor(list, factor);
    }

    pub(crate) fn set_stencil_ref(&mut self, list: &HostList, reference: u32) {
        self.stencil = Some(reference);
        crate::draw::stencil_ref(list, reference);
    }

    pub(crate) fn set_depth_bounds(&mut self, list: &HostList, min: f32, max: f32) {
        self.depth_bounds = Some([min, max]);
        crate::draw::depth_bounds(list, min, max);
    }

    /// Others recorded into the host list (`crate::external`): its heaps,
    /// root signatures and pipeline are set again before the next draw or
    /// dispatch.
    pub(crate) fn host_state_lost(&mut self) {
        // No title heaps (0) differ from what the host list has, too.
        (self.host_pipeline, self.host_roots, self.host_heaps) = (0, [0; 2], [usize::MAX; 2]);
    }

    /// `list` is a new host list that continues the title's list (it was
    /// cut at a wait for a memory value, `crate::list::cut`): a new host
    /// list has nothing set, so what the title set so far is set again.
    /// The pipeline, the root signatures and the heaps are set by the next
    /// draw or dispatch (`bind`, which is told here that the host list has
    /// none); root arguments, vertex and index buffers are set by every
    /// draw anyway. The dynamic descriptor slots and the tables made in
    /// them stay: the parts before the cut have not run yet, and they are
    /// given back when the title's list is reset.
    /// shortcut: the render target views are made again from the handles'
    /// descriptors as they are now, not as they were when the title set
    /// the targets (a descriptor rewritten in between names another view).
    pub(crate) fn replay(&mut self, list: &HostList) {
        self.host_pipeline = 0;
        self.host_roots = [0; 2];
        self.host_heaps = [0; 2];
        if self.topology_set {
            self.set_topology(list, self.topology);
        }
        if self.targets_set {
            let handles = std::mem::take(&mut self.target_handles);
            self.set_targets(list, &handles, self.depth_handle);
        }
        if !self.viewports.is_empty() {
            crate::draw::viewports(list, &self.viewports);
        }
        if !self.scissors.is_empty() {
            let rects = std::mem::take(&mut self.scissors);
            self.set_scissors(list, &rects);
        }
        if let Some(factor) = self.blend {
            crate::draw::blend_factor(list, Some(&factor));
        }
        if let Some(reference) = self.stencil {
            crate::draw::stencil_ref(list, reference);
        }
        if let Some([min, max]) = self.depth_bounds {
            crate::draw::depth_bounds(list, min, max);
        }
    }

    /// The user-data value of a root argument, as translated shaders read
    /// it: a table's byte offset in its heap, a constant's value.
    fn user_value(&self, compute: bool, slot: u8, root: RootType, dword: u32) -> u32 {
        match (self.roots_ref(compute).get(usize::from(slot)), root) {
            (Some(Root::Table(handle)), RootType::Table) => match self.heap_of(*handle) {
                Some(heap) => (handle - heap.start) as u32,
                None => {
                    once!(
                        handle = format_args!("{handle:#x}"),
                        "descriptor table outside the bound heaps"
                    );
                    0
                }
            },
            (Some(Root::Constants(c)), RootType::Constants) => {
                c.get(dword as usize).copied().unwrap_or(0)
            }
            // The host's own ([`Self::host_word`]): no root argument.
            (_, RootType::Other(_)) => 0,
            _ => {
                once!(
                    slot,
                    ?root,
                    "a shader reads a root argument the title did not set"
                );
                0
            }
        }
    }

    /// A user-data dword the host computes for a translated shader
    /// (`victory_shader::emit::USER_MEMORY_BASE`, `USER_VIEW_ADDRESS`).
    fn host_word(
        &self,
        compute: bool,
        kind: u32,
        slot: u8,
        dword: u32,
        memory: Option<&crate::memory::Ready>,
    ) -> u32 {
        use victory_shader::emit::{USER_CBV_SHIFT, USER_MEMORY_BASE, USER_VIEW_ADDRESS, USER_VIEW_BIAS};
        if kind == USER_MEMORY_BASE {
            return memory.map_or(0, |m| if dword == 0 { m.base } else { m.count });
        }
        if kind == USER_CBV_SHIFT {
            // The rows the root constant buffer was put below by, as `bind`
            // put it (`root_cbv_below`): found again, as the bias is.
            return match self.roots_ref(compute).get(usize::from(slot)) {
                Some(&Root::View(address)) => root_cbv_rows(address).map_or(0, |(_, rows)| rows),
                _ => 0,
            };
        }
        if kind == USER_VIEW_BIAS {
            // The view begins on 16 bytes of the copy the address finds
            // (`root_uav`, `root_srv_table`).
            // shortcut: found again here, not taken from the list's slot.
            return match self.roots_ref(compute).get(usize::from(slot)) {
                Some(&Root::View(address)) => crate::shadow::find(address, 1).map_or(0, |s| {
                    // SAFETY: a live buffer.
                    let base = unsafe { s.buffer.GetGPUVirtualAddress() };
                    ((s.gpu(address) - base) & 15) as u32
                }),
                _ => 0,
            };
        }
        if kind != USER_VIEW_ADDRESS && kind != USER_VIEW_ADDRESS + 1 {
            once!(kind, "a shader reads a host value this build does not know");
            return 0;
        }
        // The title address of the buffer view at that dword of the table.
        let address = match self.roots_ref(compute).get(usize::from(slot)) {
            Some(&Root::Table(table)) => crate::descriptor::view(table + u64::from(dword) * 4)
                .and_then(|view| crate::descriptor::host::buffer_range(&view))
                .map_or(0, |range| range.0),
            _ => 0,
        };
        if address == 0 {
            once!(slot, dword, "a shader takes the address of a buffer view that is not there: it reads 0");
        }
        if kind == USER_VIEW_ADDRESS {
            address as u32
        } else {
            (address >> 32) as u32
        }
    }

    /// For a shader that reads title memory by address
    /// ([`crate::memory`]): its directory, fault notes and view slots,
    /// with the pages of the root views the list has set made ready.
    fn memory_ready(&self, list: &HostList, compute: bool, writes: bool, srvs: &[u8]) -> Option<crate::memory::Ready> {
        let heap = match self.heaps[0] {
            0 => None,
            h => crate::descriptor::heap(h as This).host(),
        }?;
        let (mut addresses, mut constants, mut records) = (Vec::new(), Vec::new(), Vec::new());
        for (slot, root) in self.roots_ref(compute).iter().enumerate() {
            match root {
                Root::View(address) => {
                    addresses.push(*address);
                    // The title's root SRVs the shader reads: records it
                    // may take addresses from (`crate::memory::ready`).
                    if u8::try_from(slot).is_ok_and(|s| srvs.contains(&s)) {
                        records.push(*address);
                    }
                }
                Root::Constants(values) => constants.extend_from_slice(values),
                _ => {}
            }
        }
        crate::memory::ready(list, heap, &addresses, &constants, &records, writes)
    }

    /// The bound heap holding `handle`.
    fn heap_of(&self, handle: u64) -> Option<&'static crate::descriptor::Heap> {
        self.heaps
            .iter()
            .filter(|&&h| h != 0)
            .map(|&h| crate::descriptor::heap(h as This))
            .find(|h| h.index(handle).is_some())
    }

    /// Brings the host list up to date for a draw; false if the draw cannot
    /// run on the host.
    fn prepare(&mut self, list: &HostList, indexed: bool) -> bool {
        self.prepare_for(list, indexed, false)
    }

    /// As [`State::prepare`], for draws (`mesh` false) or mesh dispatches.
    fn prepare_for(&mut self, list: &HostList, indexed: bool, mesh: bool) -> bool {
        #[cfg(windows)]
        if crate::pipeline::is_skipped(self.pipeline as This) {
            return false;
        }
        let Some(p) = crate::pipeline::host_pipeline(self.pipeline as This) else {
            return false;
        };
        if p.compute || p.mesh != mesh {
            once!(mesh, "a draw or mesh dispatch with another kind of pipeline");
            return false;
        }
        if p.rects && self.topology != RECTANGLES {
            // Its geometry stage would turn each triangle into a rectangle.
            once!("a rectangle pipeline drawn with another topology");
        }
        if !self.bind(list, p, false) {
            return false;
        }
        let mut laps = Laps::start();
        let l = &list.list;
        // Vertex buffers the input layout reads.
        let mut views = [D3D12_VERTEX_BUFFER_VIEW::default(); 32];
        let mut count = 0;
        for (slot, (view, &v)) in views.iter_mut().zip(&self.vertex).enumerate() {
            if p.slots & 1 << slot == 0 {
                continue;
            }
            count = slot + 1;
            if v.address == 0 || v.size == 0 {
                continue;
            }
            let Some(shadow) = crate::shadow::find_any(v.address, u64::from(v.size)) else {
                continue;
            };
            shadow.refresh(list, v.address, u64::from(v.size));
            *view = D3D12_VERTEX_BUFFER_VIEW {
                BufferLocation: shadow.gpu(v.address),
                SizeInBytes: v.size,
                StrideInBytes: v.stride,
            };
        }
        // SAFETY: an open list; the views name live host buffers.
        unsafe {
            if count > 0 {
                l.IASetVertexBuffers(0, Some(&views[..count]));
            }
        }
        if indexed {
            let v = self.index;
            let Some(shadow) = crate::shadow::find_any(v.address, u64::from(v.size)) else {
                return false;
            };
            shadow.refresh(list, v.address, u64::from(v.size));
            let view = D3D12_INDEX_BUFFER_VIEW {
                BufferLocation: shadow.gpu(v.address),
                SizeInBytes: v.size,
                Format: DXGI_FORMAT(v.format as i32),
            };
            // SAFETY: an open list; the view names a live host buffer.
            unsafe { l.IASetIndexBuffer(Some(&view)) };
        }
        const RENDER_TARGET: u32 = 0x4;
        for &t in &self.targets {
            crate::list::ensure_state(list, t as This, RENDER_TARGET);
        }
        if self.depth != 0 {
            // A depth texture shaders also write: what they wrote, first.
            list.twin_use(self.depth as This, false, true);
            // A view that writes neither plane keeps the texture readable:
            // the depth read state, together with the shader read states
            // if this draw's shaders read the same texture (the write state
            // put it out of the state those views need, and a move to it
            // and back for every such draw).
            let state = if self.depth_state == DEPTH_READ {
                let (tested, read) = TESTED_DEPTH.get();
                if read && tested == self.depth {
                    DEPTH_READ | SHADER_RESOURCE
                } else {
                    DEPTH_READ
                }
            } else {
                DEPTH_WRITE
            };
            crate::list::ensure_state(list, self.depth as This, state);
        }
        laps.lap(7);
        true
    }

    /// A descriptor a shader reads through a table: its host slot made if
    /// it changed, constant data copied, textures made readable.
    /// What draws and dispatches share: heaps, our root signature, the
    /// pipeline, and per stage the user data, root views and the
    /// descriptors its tables hold.
    /// False if the draw or dispatch must not run: a root SRV it reads
    /// could not be given a view.
    pub(crate) fn bind(&mut self, list: &HostList, p: &HostPipeline, compute: bool) -> bool {
        #[cfg(feature = "diagnostics")]
        crate::bake_probe::draw_begin();
        let bound = self.bind_views(list, p, compute);
        #[cfg(feature = "diagnostics")]
        crate::bake_probe::draw_end(
            || p.stages.iter().map(|s| s.shader.hash.as_str()).collect::<Vec<_>>().join("+"),
            compute,
            &self.targets,
        );
        bound
    }

    /// [`State::bind`].
    fn bind_views(&mut self, list: &HostList, p: &HostPipeline, compute: bool) -> bool {
        let l = &list.list;
        let mut laps = Laps::start();
        // Textures picked by index, before anything asks for them.
        self.early_textures(list, p, compute);
        laps.lap(2);
        if crate::host::walls_trace() {
            self.walls_census(p, compute);
        }
        // Heaps first: a root signature with directly indexed heaps must be
        // set after them.
        if self.host_heaps != self.heaps {
            let heaps: Vec<Option<ID3D12DescriptorHeap>> = self
                .heaps
                .iter()
                .filter(|&&h| h != 0)
                .filter_map(|&h| crate::descriptor::heap(h as This).host())
                .map(|h| Some(h.heap.clone()))
                .collect();
            heaps.iter().flatten().for_each(|h| list.keep(h));
            // SAFETY: an open list and shader-visible heaps.
            unsafe { l.SetDescriptorHeaps(&heaps) };
            self.host_heaps = self.heaps;
            self.host_roots = [0; 2];
        }
        // The depth target a draw only tests, for the views of it that the
        // draw's shaders read (`readable`).
        let tested = if !compute && self.depth_state == DEPTH_READ {
            self.depth
        } else {
            0
        };
        TESTED_DEPTH.set((tested, false));
        let root = p.root.as_raw() as usize;
        if self.host_roots[usize::from(compute)] != root {
            // SAFETY: an open list and a live root signature.
            unsafe {
                if compute {
                    l.SetComputeRootSignature(&p.root);
                } else {
                    l.SetGraphicsRootSignature(&p.root);
                }
            }
            self.host_roots[usize::from(compute)] = root;
        }
        // By host object: a title pipeline released and another made at its
        // address is another host pipeline (the list keeps the old one).
        if self.host_pipeline != p.pso.as_raw() as usize {
            // SAFETY: an open list and a live pipeline.
            unsafe { l.SetPipelineState(&p.pso) };
            self.host_pipeline = p.pso.as_raw() as usize;
            list.used_pipeline(&p.name);
            // The title may release the pipeline before it submits the list.
            list.keep(&p.pso);
            list.keep(&p.root);
        }
        for stage in &p.stages {
            let t = stage.translated();
            laps.lap(6);
            STORES.set(t.stores);
            MARKED.set(victory_shader::emit::marks_writes(&t.host));
            let made = self.tables(list, &t.used, &t.host.root_indexed_views, compute);
            STORES.set(true);
            MARKED.set(false);
            if made.iter().any(|m| m.1 == u32::MAX) {
                once!("a draw whose descriptor table found no free slots is dropped");
                return false;
            }
            // What the shader stores through root views and raw addresses is
            // copied back into title memory afterwards.
            let writes = (t.stores || !write_skip())
                && !crate::pipeline::builder_quarantined(self.pipeline as This);
            // With precise ray writes (`crate::pipeline::precise_ray_writes`)
            // a shader that reads title memory by address declares the
            // copies its directory names written only if it stores through
            // them, and a root UAV written only if it stores through that
            // one: a walk of the structures writes its result to a texture
            // or a separate buffer.
            let precise = crate::pipeline::precise_ray_writes();
            let raw_writes = if precise {
                writes && (t.host.writes_guest_memory || !write_skip())
            } else {
                writes
            };
            // Title memory by address: what the shader finds it by.
            let memory = if t.host.root_srvs.contains(&victory_shader::emit::SLOT_MEMORY) {
                let Some(ready) = self.memory_ready(list, compute, raw_writes, &t.host.root_srvs) else {
                    once!("a shader that reads title memory by address without a view heap to hold its views: the draw is dropped");
                    return false;
                };
                Some(ready)
            } else {
                None
            };
            laps.lap(4);
            if let Some((param, dwords)) = stage.user_data {
                let mut values = vec![0u32; dwords as usize];
                for u in &t.host.user_data {
                    if let RootType::Other(kind) = u.root {
                        values[u.index as usize] =
                            self.host_word(compute, kind, u.slot, u.dword, memory.as_ref());
                        continue;
                    }
                    let fresh = made
                        .iter()
                        .find(|m| m.0 == u.slot && u.root == RootType::Table);
                    values[u.index as usize] = match fresh {
                        Some(&(_, offset)) => offset,
                        None => self.user_value(compute, u.slot, u.root, u.dword),
                    };
                }
                // SAFETY: an open list; `values` covers the parameter.
                unsafe {
                    if compute {
                        l.SetComputeRoot32BitConstants(param, dwords, values.as_ptr().cast(), 0);
                    } else {
                        l.SetGraphicsRoot32BitConstants(param, dwords, values.as_ptr().cast(), 0);
                    }
                }
            }
            for (views, srv) in [(&stage.root_cbvs, false), (&stage.root_srvs, true)] {
                for &(slot, param) in views {
                    if srv && slot == victory_shader::emit::SLOT_MEMORY {
                        // Ours: the directory of title memory by address.
                        if let Some(m) = &memory {
                            // SAFETY: an open list; the directory lives as
                            // long as the process and the list holds it.
                            unsafe {
                                if compute {
                                    l.SetComputeRootShaderResourceView(param, m.directory);
                                } else {
                                    l.SetGraphicsRootShaderResourceView(param, m.directory);
                                }
                            }
                        }
                        continue;
                    }
                    if srv && slot == victory_shader::emit::SLOT_VIEW_MARKS {
                        // Ours: the bound view heap's mark table.
                        let table = self
                            .heaps
                            .iter()
                            .filter(|&&h| h != 0)
                            .map(|&h| crate::descriptor::heap(h as This))
                            .find(|h| !h.samplers())
                            .and_then(|h| h.host())
                            .and_then(|host| host.marks().map(|(buffer, gpu)| (buffer.clone(), gpu)));
                        let gpu = match table {
                            Some((buffer, gpu)) => {
                                // The list may outlive the heap that owns it.
                                list.keep(&buffer);
                                Some(gpu)
                            }
                            None => crate::marks::empty_table_gpu(),
                        };
                        if let Some(gpu) = gpu {
                            // SAFETY: an open list; the table lives while the list holds it.
                            unsafe {
                                if compute {
                                    l.SetComputeRootShaderResourceView(param, gpu);
                                } else {
                                    l.SetGraphicsRootShaderResourceView(param, gpu);
                                }
                            }
                        }
                        continue;
                    }
                    if srv && slot == victory_shader::emit::SLOT_VIEW_ADDRESSES {
                        // Ours: the view heap's table of view addresses
                        // (only with the structure builder asked for: a draw
                        // without it is dropped, as the program was before).
                        let table = self
                            .heaps
                            .iter()
                            .filter(|&&h| h != 0)
                            .map(|&h| crate::descriptor::heap(h as This))
                            .find(|h| !h.samplers())
                            .and_then(|h| h.host())
                            .and_then(|h| h.addresses());
                        let Some((gpu, buffer)) = table else {
                            once!("a shader takes view addresses from the view heap, which has no table of them (VICTORY_BVH_BUILDER=1 makes it): the draw is dropped");
                            return false;
                        };
                        // The list may outlive the title's heap object,
                        // which owns the table.
                        list.keep(buffer);
                        // SAFETY: an open list; the list holds the table.
                        unsafe {
                            if compute {
                                l.SetComputeRootShaderResourceView(param, gpu);
                            } else {
                                l.SetGraphicsRootShaderResourceView(param, gpu);
                            }
                        }
                        continue;
                    }
                    if srv && slot == victory_shader::emit::SLOT_CBV_SHIFTS {
                        // Ours: the rows each constant buffer view of the
                        // bound heap begins before the title's constants
                        // (contract 3).
                        match self.heap_cbv_shifts(list) {
                            // SAFETY: an open list; the table lives as long
                            // as the heap the list holds.
                            Some(gpu) => unsafe {
                                if compute {
                                    l.SetComputeRootShaderResourceView(param, gpu);
                                } else {
                                    l.SetGraphicsRootShaderResourceView(param, gpu);
                                }
                            },
                            None => once!("a shader shifts constant buffer rows of a heap that is not set or has no table of them"),
                        }
                        continue;
                    }
                    if srv && slot == victory_shader::emit::SLOT_STRIDES {
                        // Ours, not the title's: the shader picks buffers
                        // from the view heap by an index it computes.
                        match self.heap_buffers(list) {
                            // SAFETY: an open list; the table lives as long
                            // as the heap the list holds.
                            Some(gpu) => unsafe {
                                if compute {
                                    l.SetComputeRootShaderResourceView(param, gpu);
                                } else {
                                    l.SetGraphicsRootShaderResourceView(param, gpu);
                                }
                            },
                            None => once!("a shader picks buffers from a view heap that is not set"),
                        }
                        continue;
                    }
                    let tabled = srv && crate::pipeline::host::bounded_root_srvs();
                    let Some(&Root::View(address)) = self.roots_ref(compute).get(usize::from(slot))
                    else {
                        once!(slot, "a shader reads a root view the title did not set");
                        // A table left unset is undefined: a null view.
                        if tabled && !self.root_srv_table(list, None, param, compute, false) {
                            return false;
                        }
                        continue;
                    };
                    // The shader adds the bytes a raw view begins early by.
                    let biased = srv
                        && t.host.user_data.iter().any(|u| {
                            u.slot == slot
                                && u.root == RootType::Other(victory_shader::emit::USER_VIEW_BIAS)
                        });
                    if tabled {
                        if !self.root_srv_table(list, Some(address), param, compute, biased) {
                            return false;
                        }
                        continue;
                    }
                    // Contract 3: a root constant buffer the shader shifts
                    // by `USER_CBV_SHIFT` goes on the 256 bytes below in
                    // its host copy instead of a copy of its own.
                    let shifted = !srv
                        && t.host.user_data.iter().any(|u| {
                            u.slot == slot && u.root == RootType::Other(victory_shader::emit::USER_CBV_SHIFT)
                        });
                    let below = shifted.then(|| root_cbv_below(list, address)).flatten();
                    let Some(mut gpu) = below.map(|(gpu, _)| gpu).or_else(|| root_view(list, address, srv)) else {
                        continue;
                    };
                    if biased {
                        gpu -= u64::from(self.host_word(
                            compute,
                            victory_shader::emit::USER_VIEW_BIAS,
                            slot,
                            0,
                            None,
                        ));
                    }
                    // SAFETY: an open list; the address is in live memory.
                    unsafe {
                        match (compute, srv) {
                            (false, false) => l.SetGraphicsRootConstantBufferView(param, gpu),
                            (false, true) => l.SetGraphicsRootShaderResourceView(param, gpu),
                            (true, false) => l.SetComputeRootConstantBufferView(param, gpu),
                            (true, true) => l.SetComputeRootShaderResourceView(param, gpu),
                        }
                    }
                }
            }
            laps.lap(5);
            for &(slot, param) in &stage.root_uavs {
                if slot == victory_shader::emit::SLOT_VIEW_WRITES {
                    // Ours: the bitmap of pages shaders write through views.
                    if let Some(gpu) = crate::marks::bitmap_gpu() {
                        // SAFETY: an open list; the bitmap lives for good.
                        unsafe {
                            if compute {
                                l.SetComputeRootUnorderedAccessView(param, gpu);
                            } else {
                                l.SetGraphicsRootUnorderedAccessView(param, gpu);
                            }
                        }
                    }
                    continue;
                }
                if slot == victory_shader::emit::SLOT_FAULTS {
                    // Ours: where shaders note the pages they found empty.
                    if let Some(m) = &memory {
                        // SAFETY: an open list; the buffer lives as long as
                        // the process and the list holds it.
                        unsafe {
                            if compute {
                                l.SetComputeRootUnorderedAccessView(param, m.faults);
                            } else {
                                l.SetGraphicsRootUnorderedAccessView(param, m.faults);
                            }
                        }
                    }
                    continue;
                }
                let address = match self.roots_ref(compute).get(usize::from(slot)) {
                    Some(&Root::View(address)) => Some(address),
                    _ => {
                        once!(slot, "a shader writes through a root view the title did not set");
                        None
                    }
                };
                let writes = if precise {
                    writes && (!write_skip() || t.host.written_root_uavs.contains(&slot))
                } else {
                    writes
                };
                if !self.root_uav(list, address, param, compute, writes) {
                    return false;
                }
            }
        }
        // Depth textures it writes through unordered access: their colour
        // twins hold what they lack from here on.
        list.twins_written();
        laps.lap(6);
        true
    }

    /// Binds a root UAV: a raw unordered access view of the title buffer's
    /// host copy from `address` on, as a one-view table (a null view
    /// without an address, without a host copy shaders can write, or at an
    /// address not on 16 bytes), or as a root descriptor with
    /// `VICTORY_ROOT_SRV_DIRECT=1` (the dispatch is dropped then if there
    /// is nothing to bind). With `writes`, what the shader stored is copied
    /// back into title memory when the list has run. False if the draw
    /// must be dropped.
    /// shortcut: a root view has no size: the view, the refresh and the
    /// copy back cover all that is left of the title's buffer
    /// (`VICTORY_ROOT_UAV_MB` sets a limit for all three).
    /// With the structure builder asked for (after DexNom/VIctory): a root
    /// view's slot in this list is made again when the title has replaced
    /// the buffer at its address since the slot was made (the builder's
    /// stages run between the title's buffer changes within one list).
    /// Before, the list's later dispatches kept the copy of the buffer
    /// that was there first, and their stores went into a copy of memory
    /// the title had given to another buffer. The old slot stays held
    /// until the list is done: draws recorded with it keep it.
    fn forget_moved_root(
        &mut self,
        uav: bool,
        key: (usize, u64),
        find: impl FnOnce() -> Option<std::sync::Arc<crate::shadow::Shadow>>,
    ) {
        if !crate::pipeline::builder_allowed() {
            return;
        }
        let generation = crate::shadow::buffer_generation();
        let seen = self.root_generations.entry((uav, key.0, key.1)).or_insert(generation);
        if *seen == generation {
            return;
        }
        *seen = generation;
        let slots = if uav { &mut self.root_uav_slots } else { &mut self.root_srv_slots };
        let Some((_, old)) = slots.get(&key) else {
            return;
        };
        let now = find();
        if old.as_ref().map(std::sync::Arc::as_ptr) != now.as_ref().map(std::sync::Arc::as_ptr) {
            slots.remove(&key);
        }
    }

    fn root_uav(
        &mut self,
        list: &HostList,
        address: Option<u64>,
        param: u32,
        compute: bool,
        writes: bool,
    ) -> bool {
        const UNORDERED: i32 = D3D12_RESOURCE_FLAG_ALLOW_UNORDERED_ACCESS.0;
        static LIMIT: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
        let limit = *LIMIT.get_or_init(|| {
            std::env::var("VICTORY_ROOT_UAV_MB")
                .ok()
                .and_then(|v| v.parse::<u64>().ok())
                .map_or(u64::MAX, |mb| mb << 20)
        });
        let find = |a: u64| {
            let s = crate::shadow::find(a, 1)?;
            // SAFETY: a live buffer.
            let flags = unsafe { s.buffer.GetDesc() }.Flags;
            if flags.0 & UNORDERED == 0 {
                once!("a root UAV of a buffer whose host copy shaders cannot write");
                return None;
            }
            Some(s)
        };
        let ready = |a: u64, s: &std::sync::Arc<crate::shadow::Shadow>| {
            let len = s.left(a).min(limit);
            s.refresh(list, a, len);
            if writes {
                // A host root descriptor: no heap view, so its writes are not
                // marked (`crate::shadow::Shadow::gpu_writes`).
                s.gpu_writes(list, a, len, false);
            }
        };
        if !crate::pipeline::host::bounded_root_srvs() {
            let Some((a, s)) = address.and_then(|a| Some((a, find(a)?))) else {
                once!("a root UAV with nothing to bind: the draw is dropped");
                return false;
            };
            ready(a, &s);
            // From the 16 bytes below: the shader adds the rest.
            // SAFETY: a live buffer.
            let base = unsafe { s.buffer.GetGPUVirtualAddress() };
            let gpu = base + ((s.gpu(a) - base) & !15);
            // SAFETY: an open list; the address is in a live buffer.
            unsafe {
                if compute {
                    list.list.SetComputeRootUnorderedAccessView(param, gpu);
                } else {
                    list.list.SetGraphicsRootUnorderedAccessView(param, gpu);
                }
            }
            return true;
        }
        let heap = match self.heaps[0] {
            0 => None,
            h => Some(crate::descriptor::heap(h as This)),
        };
        let Some((heap, host)) = heap.and_then(|h| Some((h, h.host()?))) else {
            once!("a root UAV without a bound view heap: the draw is dropped");
            return false;
        };
        let key = (std::ptr::from_ref(heap) as usize, address.unwrap_or(0));
        self.forget_moved_root(true, key, || address.and_then(find));
        let (index, shadow) = match self.root_uav_slots.get(&key) {
            Some((index, shadow)) => (*index, address.zip(shadow.clone())),
            None => {
                let shadow = address.and_then(|a| find(a).map(|s| (a, s)));
                let Some(index) = host.dynamic(1) else {
                    return false;
                };
                self.dynamic.push((key.0, index, 1));
                if let Some((a, s)) = &shadow {
                    // SAFETY: a live buffer.
                    let base = unsafe { s.buffer.GetGPUVirtualAddress() };
                    let offset = s.gpu(*a) - base;
                    // Raw views start on 16 bytes: from the 16 bytes
                    // below, and the shader adds the rest to its offsets
                    // (`victory_shader::emit::USER_VIEW_BIAS`). (An address
                    // not on 16 bytes got a null view at first: the
                    // builder's counters are at such addresses.)
                    let bias = offset & 15;
                    host.raw_uav_at(index, &s.buffer, offset - bias, s.left(*a).min(limit) + bias);
                }
                if shadow.is_none() {
                    host.null_raw_uav_at(index);
                }
                self.root_uav_slots
                    .insert(key, (index, shadow.as_ref().map(|(_, s)| s.clone())));
                (index, shadow)
            }
        };
        if let Some((a, s)) = &shadow {
            ready(*a, s);
        }
        let handle = host.gpu_handle(index);
        // SAFETY: an open list; the slot is in the bound view heap.
        unsafe {
            if compute {
                list.list.SetComputeRootDescriptorTable(param, handle);
            } else {
                list.list.SetGraphicsRootDescriptorTable(param, handle);
            }
        }
        true
    }

    /// Binds a root SRV as a one-view table
    /// ([`crate::pipeline::host::bounded_root_srvs`]): a raw view of the
    /// title buffer's host copy from `address` to the copy's end, or a null
    /// view (no address, no host copy, or an address not on 16 bytes). One
    /// dynamic slot per address per list, given back with the list's
    /// others. False if no slot could be had or no view heap is bound.
    fn root_srv_table(
        &mut self,
        list: &HostList,
        address: Option<u64>,
        param: u32,
        compute: bool,
        biased: bool,
    ) -> bool {
        let heap = match self.heaps[0] {
            0 => None,
            h => Some(crate::descriptor::heap(h as This)),
        };
        let Some((heap, host)) = heap.and_then(|h| Some((h, h.host()?))) else {
            once!("a root SRV without a bound view heap: the draw is dropped");
            return false;
        };
        // A view that begins early is another view than one that does not
        // (title addresses stay below bit 63).
        let key = (
            std::ptr::from_ref(heap) as usize,
            address.unwrap_or(0) | u64::from(biased) << 63,
        );
        self.forget_moved_root(false, key, || address.and_then(|a| crate::shadow::find(a, 1)));
        let (index, shadow) = match self.root_srv_slots.get(&key) {
            Some((index, shadow)) => (*index, address.zip(shadow.clone())),
            None => {
                let shadow = address.and_then(|a| crate::shadow::find(a, 1).map(|s| (a, s)));
                let Some(index) = host.dynamic(1) else {
                    return false;
                };
                self.dynamic.push((key.0, index, 1));
                self.root_srv_slots
                    .insert(key, (index, shadow.as_ref().map(|(_, s)| s.clone())));
                match &shadow {
                    Some((a, s)) => {
                        // SAFETY: a live buffer.
                        let base = unsafe { s.buffer.GetGPUVirtualAddress() };
                        let offset = s.gpu(*a) - base;
                        if biased {
                            // From the 16 bytes below; the shader adds the
                            // rest (`victory_shader::emit::USER_VIEW_BIAS`).
                            let bias = offset & 15;
                            host.raw_at(index, &s.buffer, offset - bias, s.left(*a) + bias);
                        } else if offset.is_multiple_of(16) {
                            host.raw_at(index, &s.buffer, offset, s.left(*a));
                        } else {
                            // Raw views start on 16 bytes; one from the
                            // aligned start would shift every read.
                            once!(
                                address = format_args!("{a:#x}"),
                                "a root SRV not on 16 bytes reads zeros"
                            );
                            host.null_raw_at(index);
                        }
                    }
                    None => host.null_raw_at(index),
                }
                (index, shadow)
            }
        };
        // As for root descriptors (`root_view`): all that is left of the
        // buffer is brought up to date. The first 1 MiB only left what lay
        // past it stale (most mesh vertex streams reach further).
        if let Some((a, s)) = &shadow {
            s.refresh(list, *a, s.left(*a));
        }
        let handle = host.gpu_handle(index);
        // SAFETY: an open list; the slot is in the bound view heap.
        unsafe {
            if compute {
                list.list.SetComputeRootDescriptorTable(param, handle);
            } else {
                list.list.SetGraphicsRootDescriptorTable(param, handle);
            }
        }
        true
    }

    /// For a shader that marks the pages it writes through views
    /// (`victory_shader::emit`, "Write marks"): the GPU address of the bound
    /// The bound view heap's table of constant buffer view rows (contract
    /// 3), held by `list`.
    fn heap_cbv_shifts(&self, list: &HostList) -> Option<u64> {
        let heap = self
            .heaps
            .iter()
            .filter(|&&h| h != 0)
            .map(|&h| crate::descriptor::heap(h as This))
            .find(|h| !h.samplers())?;
        let host = heap.host()?;
        let gpu = host.cbv_shifts_gpu()?;
        const ONCE: usize = usize::MAX - 0x60;
        if list.first_use(ONCE)
            && let Some(buffer) = host.cbv_shifts_buffer()
        {
            list.keep(buffer);
        }
        Some(gpu)
    }

    /// With the translator's `VICTORY_CBV_ROW_SHIFT`: whether table entry
    /// `u`, a constant buffer view, is read in place from the title's slot
    /// (made on the 256 bytes below, the shader adding the rows), so its
    /// table need not be made afresh for the draw.
    fn cbv_in_place(&self, u: &UsedResource, compute: bool) -> bool {
        if !crate::shader_cache::cbv_row_shift() {
            return false;
        }
        let why = self.cbv_in_place_why(u, compute);
        if slot_reasons() {
            IN_PLACE_WHY[why].fetch_add(1, Ordering::Relaxed);
        }
        why == IN_PLACE_YES
    }

    /// [`State::cbv_in_place`], remembered in the list while nothing it
    /// depends on has moved ([`State::in_place_memo`]): the list's changes
    /// (a texture state, twin or written texture) and the host copies (one
    /// made, let go or marked rewritten in flight). Run 424m1 spent 6 ms a
    /// frame, all threads, asking it per draw.
    fn cbv_in_place_memo(&self, list: &HostList, u: &UsedResource, compute: bool) -> bool {
        if !crate::shader_cache::cbv_row_shift() {
            return false;
        }
        let Some(&Root::Table(table)) = self.roots_ref(compute).get(usize::from(u.slot)) else {
            return false;
        };
        let handle = table + u64::from(u.table_offset_dwords) * 4;
        // SAFETY: a descriptor slot of the title's heap, as `mirror` reads it.
        let bytes = unsafe { (handle as *const [u64; 4]).read_unaligned() };
        let now = (list.changes(), crate::shadow::COPIES_EPOCH.load(Ordering::Relaxed));
        if let Some(&(changes, epoch, answer)) = self.in_place_memo.borrow().get(&(handle, bytes))
            && (changes, epoch) == now
        {
            return answer;
        }
        let answer = self.cbv_in_place(u, compute);
        let mut memo = self.in_place_memo.borrow_mut();
        if memo.len() >= 1 << 16 {
            memo.clear();
        }
        memo.insert((handle, bytes), (now.0, now.1, answer));
        answer
    }

    /// [`State::cbv_in_place`]'s answer, as an index into [`IN_PLACE_WHY_NAMES`].
    fn cbv_in_place_why(&self, u: &UsedResource, compute: bool) -> usize {
        let Some(&Root::Table(table)) = self.roots_ref(compute).get(usize::from(u.slot)) else {
            return 0;
        };
        let handle = table + u64::from(u.table_offset_dwords) * 4;
        let Some(heap) = self.heap_of(handle) else {
            return 8;
        };
        let Some(host) = heap.host() else {
            return 9;
        };
        let Some(index) = heap.index(handle) else {
            return 10;
        };
        if index >= host.count() {
            if slot_reasons() {
                let band = (((index - host.count()) >> 16) as usize).min(PAST_BANDS - 1);
                PAST_HEAP_BAND[band].fetch_add(1, Ordering::Relaxed);
            }
            return 11;
        }
        let Some(View::Constant(address, size)) = host.mirror(index, handle, true) else {
            return 1;
        };
        // Per draw: the copy may have changed kind since the slot was made,
        // and the slot must view the copy that is refreshed.
        if host.cbv_refused(index) {
            return 2;
        }
        let Some(shadow) = crate::shadow::find(address, u64::from(size)) else {
            return 3;
        };
        if !shadow.in_place_ok() {
            return 4;
        }
        if crate::shadow::rewritten(address, u64::from(size)) {
            return 5;
        }
        if !host.kept_is(index, &shadow.buffer) {
            KEPT_OTHER.fetch_add(1, Ordering::Relaxed);
            return 6;
        }
        IN_PLACE_YES
    }

    /// view heap's table of marks, held by `list`.
    fn heap_marks(&self, list: &HostList) -> Option<u64> {
        let heap = self
            .heaps
            .iter()
            .filter(|&&h| h != 0)
            .map(|&h| crate::descriptor::heap(h as This))
            .find(|h| !h.samplers())?;
        let host = heap.host()?;
        let gpu = host.marks_gpu()?;
        const ONCE: usize = usize::MAX - 0x5f;
        if list.first_use(ONCE)
            && let Some(buffer) = host.marks_buffer()
        {
            list.keep(buffer);
        }
        Some(gpu)
    }

    /// For a shader that picks buffers from the view heap by an index it
    /// computes (SM 6.6 dynamic resources): the GPU address of the heap's
    /// table of element strides, with every buffer view of the heap given
    /// its host slot and its memory brought up to date when `list` is
    /// submitted. Tables tell us which views a draw reads; nothing tells
    /// us which of the heap's a computed index names, so all of them are
    /// made ready, once per list.
    /// shortcut: the buffers are read, not written: what such a shader
    /// writes through a picked unordered access view stays in the host
    /// copy. A list refreshes them where it first runs such a shader;
    /// title writes recorded later in the same list are not seen.
    fn heap_buffers(&self, list: &HostList) -> Option<u64> {
        let heap = self
            .heaps
            .iter()
            .filter(|&&h| h != 0)
            .map(|&h| crate::descriptor::heap(h as This))
            .find(|h| !h.samplers())?;
        let host = heap.host()?;
        let gpu = host.strides_gpu()?;
        // One key per list, as textures have: no title resource is there.
        const ONCE: usize = usize::MAX - 0x5e;
        if !list.first_use(ONCE) {
            return Some(gpu);
        }
        // The list may outlive the title's heap object, which owns the table.
        if let Some(buffer) = host.strides_buffer() {
            list.keep(buffer);
        }
        // The pass itself once per frame: per list it ran up to 51 times a
        // frame over 81,000 views and 4,100 host copies, 18 s of every
        // frame (2026-11-24, run 11).
        // shortcut: what the title writes into such buffers between lists
        // of one frame is seen a frame late.
        static FRAME: AtomicU64 = AtomicU64::new(u64::MAX);
        let frame = crate::frame::current();
        if FRAME.swap(frame, Ordering::Relaxed) == frame {
            return Some(gpu);
        }
        let started = std::time::Instant::now();
        // Per host heap: the host copy and range each slot's buffer view
        // reads, kept from pass to pass. Only the slots written since the
        // last pass are looked at again (83,000 slots a pass otherwise, 0.1 s
        // at best); all of them every 16 passes, because a view of a
        // released resource goes stale without a write, and a slot written
        // before its memory had a host copy is only made then.
        let (slots, mut kept) = host.buffers.begin_pass();
        let ranges = &mut kept.by_slot;
        // Whether the device was there before the pass: a view that removes
        // it is named below.
        static GONE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
        let mut alive = !GONE.load(Ordering::Relaxed) && crate::descriptor::host::device_removed().is_none();
        HEAP_PASS_PARTS[1].fetch_add(slots.len() as u64, Ordering::Relaxed);
        split_add(split::SLOTS, slots.len() as u64);
        // The slots made here are not marked for the next pass again
        // (`descriptor::host`, `VICTORY_PASS_REVISITS`).
        let in_pass = crate::descriptor::host::HeapPass::enter();
        for (n, &index) in slots.iter().enumerate() {
            ranges.remove(&index);
            let Some(handle) = heap.handle(index) else {
                continue;
            };
            #[cfg(feature = "diagnostics")]
            let mirroring = std::time::Instant::now();
            let mirrored = host.mirror(index, handle, true);
            #[cfg(feature = "diagnostics")]
            let mirror_us = micros(mirroring);
            #[cfg(feature = "diagnostics")]
            split_add(split::SLOT_MIRROR_US, mirror_us);
            let Some(view) = mirrored else {
                continue;
            };
            // Every 64 views while the device lives (a cheap query).
            if alive
                && n % 64 == 63
                && let Some(why) = crate::descriptor::host::device_removed()
            {
                alive = false;
                GONE.store(true, Ordering::Relaxed);
                let last: Vec<String> = slots[n - 63..=n]
                    .iter()
                    .filter_map(|&i| crate::descriptor::view(heap.handle(i)?).map(|v| format!("{i}: {v:?}")))
                    .map(|s| s.chars().take(200).collect())
                    .collect();
                tracing::error!(why, slot = index, "the host device went while buffer views of the heap were made; one of: {last:#?}");
                crate::host::log_removal_context();
            }
            // The copy the slot shows (`buffer_copy`), not the one the
            // view's range alone would find.
            #[cfg(feature = "diagnostics")]
            let copying = std::time::Instant::now();
            let copy = crate::descriptor::host::buffer_copy(&view);
            #[cfg(feature = "diagnostics")]
            {
                let copy_us = micros(copying);
                split_add(split::SLOT_COPY_US, copy_us);
                if matches!(view, View::Constant(..)) {
                    split_add(split::CONSTANT_SLOTS, 1);
                    split_add(split::CONSTANT_US, mirror_us + copy_us);
                }
            }
            let Some((shadow, address, len)) = copy else {
                continue;
            };
            #[cfg(feature = "diagnostics")]
            if host.shows(index, &shadow.buffer) == Some(false) {
                HEAP_VIEWS_ELSEWHERE.fetch_add(1, Ordering::Relaxed);
            }
            ranges.insert(index, (shadow, address, address + len));
        }
        drop(in_pass);
        let views = ranges.len() as u32;
        split_add(split::SLOTS_US, micros(started));
        let refreshing = std::time::Instant::now();
        // Per host copy, the span its picked views cover.
        let mut spans: std::collections::HashMap<usize, (std::sync::Arc<crate::shadow::Shadow>, u64, u64)> =
            std::collections::HashMap::new();
        for (shadow, from, to) in ranges.values() {
            let key = std::sync::Arc::as_ptr(shadow) as usize;
            let span = spans.entry(key).or_insert_with(|| (shadow.clone(), *from, *to));
            span.1 = span.1.min(*from);
            span.2 = span.2.max(*to);
        }
        drop(kept);
        let bytes: u64 = spans.values().map(|s| s.2 - s.1).sum();
        // Memory that is not write-watched is copied whole by every refresh.
        // It was once refreshed only every 120 frames (unmapped pages read
        // one by one cost 14 GB a frame in run 11, before they were
        // remembered): streamed geometry then drew from stale buffers, and
        // the garbage ended runs (the card stopped answering 14-90 frames
        // into the opening cutscene; with every frame it ran on). The pass
        // costs some 30 ms a frame this way.
        static WHOLE: std::sync::Mutex<Option<std::collections::HashMap<usize, u64>>> = std::sync::Mutex::new(None);
        let mut whole = WHOLE.lock().unwrap_or_else(|e| e.into_inner());
        let whole = whole.get_or_insert_with(Default::default);
        for (shadow, from, to) in spans.values() {
            if shadow.unwatched() {
                // By the copy's number: a copy made where one let go was
                // (`shadow::let_go_idle`) has never been refreshed.
                let key = shadow.id() as usize;
                // `VICTORY_UNWATCHED_FRAMES` sets the interval (1: every frame).
                static EVERY: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
                let every = *EVERY.get_or_init(|| {
                    std::env::var("VICTORY_UNWATCHED_FRAMES")
                        .ok()
                        .and_then(|v| v.parse().ok())
                        .unwrap_or(1)
                });
                match whole.get(&key) {
                    Some(&last) if frame < last + every => continue,
                    _ => {
                        whole.insert(key, frame);
                    }
                }
            }
            shadow.refresh(list, *from, to - from);
            HEAP_PASS_PARTS[4].fetch_add(to - from, Ordering::Relaxed);
        }
        split_add(split::REFRESH_US, micros(refreshing));
        split_add(split::SPANS, spans.len() as u64);
        split_add(split::SPAN_BYTES, bytes);
        let textures_began = std::time::Instant::now();
        if !crate::heap_worker::kick(heap) {
            Self::heap_textures(Into::List(list), heap, host);
        }
        HEAP_PASS_PARTS[0].fetch_add(textures_began.elapsed().as_micros() as u64, Ordering::Relaxed);
        HEAP_BUFFER_PASSES.fetch_add(1, Ordering::Relaxed);
        HEAP_BUFFER_MICROS.fetch_add(started.elapsed().as_micros() as u64, Ordering::Relaxed);
        split_max(split::LONGEST_PASS_US, micros(started));
        static LOGGED: AtomicU64 = AtomicU64::new(0);
        // The first pass and every growth of the set by half.
        let seen = LOGGED.load(Ordering::Relaxed);
        if u64::from(views) > seen + seen / 2 {
            LOGGED.store(u64::from(views), Ordering::Relaxed);
            tracing::info!(
                slots = slots.len(),
                views,
                copies = spans.len(),
                mb = bytes >> 20,
                ms = started.elapsed().as_millis(),
                "buffer views of the heap made ready for shaders that pick them by index"
            );
        }
        Some(gpu)
    }

    /// Runs the pass that uploads textures picked by index
    /// ([`State::heap_textures`]) where nothing has asked for it yet
    /// ([`early_budgets`]): once a frame for a few milliseconds, at the
    /// frame's first draw with a view heap set, and with a larger budget
    /// of its own before every draw whose pixel shader is one of the
    /// title's bake programs. The uploads are recorded into `list` ahead
    /// of the draw being bound, as the world's pass records them.
    /// Claims the early or bake texture-upload budget for `frame` once a view
    /// heap is known to be bound. Returns `Some(((ms, bytes), is_bake))`.
    fn claim_early_budget(
        frame: u64,
        bake: bool,
        early_ms: u64,
        bake_ms: u64,
        last_frame: &AtomicU64,
        baked_ms: &AtomicU64,
    ) -> Option<((u64, u64), bool)> {
        let first = last_frame.swap(frame, Ordering::Relaxed) != frame;
        if first {
            baked_ms.store(0, Ordering::Relaxed);
        }
        let left = bake_ms.saturating_sub(baked_ms.load(Ordering::Relaxed));
        if bake && left > 0 {
            // What a bake may read must be there now: up to 2 GiB.
            Some(((left, 2 << 30), true))
        } else if first && early_ms > 0 {
            Some(((early_ms, 64 << 20), false))
        } else {
            None
        }
    }

    fn early_textures(&self, list: &HostList, p: &HostPipeline, compute: bool) {
        let (early_ms, bake_ms) = early_budgets();
        let is_bake = !compute
            && p.stages.iter().any(|s| BAKE_PIXEL_SHADERS.contains(&s.shader.hash.as_str()));
        if !compute && (early_ms > 0 || bake_ms > 0) {
            let bake = bake_ms > 0 && is_bake;
            static FRAME: AtomicU64 = AtomicU64::new(u64::MAX);
            // The milliseconds this frame's bake draws have taken so far: a
            // frame has several, and together they get `bake_ms`.
            static BAKED_MS: AtomicU64 = AtomicU64::new(0);
            let sources = if bake && bake_sources_first() { self.bake_source_slots(p) } else { Vec::new() };
            BAKE_SOURCES.set(sources);
            let _ = self.early_textures_with(
                list,
                bake,
                early_ms,
                bake_ms,
                crate::frame::current(),
                &FRAME,
                &BAKED_MS,
            );
            BAKE_SOURCES.with_borrow_mut(Vec::clear);
        }
        // After the pass (with the passes off it did not run): what the
        // bake's data names, as it stands.
        #[cfg(feature = "diagnostics")]
        if is_bake {
            self.probe_bake(p);
        }
    }

    /// `VICTORY_BAKE_PROBE=1`: hands the bake probe the memory of the
    /// buffers this bake draw's tables name, its root constants and its
    /// root views, with the targets it draws into (`crate::bake_probe`).
    #[cfg(feature = "diagnostics")]
    fn probe_bake(&self, p: &HostPipeline) {
        if !crate::bake_probe::on() {
            return;
        }
        let Some(heap) = self
            .heaps
            .iter()
            .filter(|&&h| h != 0)
            .map(|&h| crate::descriptor::heap(h as This))
            .find(|h| !h.samplers())
        else {
            return;
        };
        let Some(host) = heap.host() else {
            return;
        };
        let pixel = p
            .stages
            .iter()
            .map(|s| s.shader.hash.as_str())
            .find(|h| BAKE_PIXEL_SHADERS.contains(h))
            .unwrap_or("");
        let buffers = self.bake_data(p);
        crate::bake_probe::bake(&crate::bake_probe::BakeDraw {
            frame: crate::frame::current(),
            pixel,
            buffers,
            targets: &self.targets,
            heap,
            host,
            queue: HEAP_TEXTURES_QUEUE.each_ref().map(|c| c.load(Ordering::Relaxed)),
        });
    }

    /// The view heap bound for shaders (not the samplers'), if any.
    fn view_heap(&self) -> Option<&'static crate::descriptor::Heap> {
        self.heaps
            .iter()
            .filter(|&&h| h != 0)
            .map(|&h| crate::descriptor::heap(h as This))
            .find(|h| !h.samplers())
    }

    /// The title slots a bake draw's data names that hold a texture view:
    /// the textures its pixel shader picks by index. A dword that only
    /// looks like such a slot is one more texture made ready, no more.
    fn bake_source_slots(&self, p: &HostPipeline) -> Vec<u32> {
        let Some(heap) = self.view_heap() else {
            return Vec::new();
        };
        let mut slots: Vec<u32> = Vec::new();
        for (_, bytes) in self.bake_data(p) {
            for c in bytes.chunks_exact(4) {
                let v = u32::from_le_bytes([c[0], c[1], c[2], c[3]]);
                // Slots below 16 are the title's own fixed ones; the rest
                // of small values are mostly counts and flags.
                if v < 16 || slots.contains(&v) {
                    continue;
                }
                if heap
                    .handle(v)
                    .and_then(crate::descriptor::view)
                    .is_some_and(|view| crate::descriptor::host::is_texture_view(&view))
                {
                    slots.push(v);
                    if slots.len() >= 512 {
                        return slots;
                    }
                }
            }
        }
        slots
    }

    /// The memory of the buffers a bake draw's tables name, of its root
    /// views, and its root constants (each at a made-up address): where the
    /// slots of the textures it picks are.
    fn bake_data(&self, p: &HostPipeline) -> Vec<(u64, Vec<u8>)> {
        let mut buffers: Vec<(u64, Vec<u8>)> = Vec::new();
        let mut take = |address: u64, len: u64| {
            if address == 0 || len == 0 || buffers.iter().any(|(a, _)| *a == address) {
                return;
            }
            let len = (len.min(16 << 10) as usize) & !3;
            let mut bytes = vec![0u8; len];
            crate::host::copy_mapped(bytes.as_mut_ptr(), address as *const u8, len, false);
            buffers.push((address, bytes));
        };
        let roots = self.roots_ref(false);
        for stage in &p.stages {
            for u in &stage.translated().used {
                if let (RootType::Table, Some(Root::Table(table))) = (u.root, roots.get(usize::from(u.slot))) {
                    let handle = table + u64::from(u.table_offset_dwords) * 4;
                    match crate::descriptor::view(handle) {
                        Some(View::Constant(address, size)) => take(address, u64::from(size)),
                        Some(view) => {
                            if let Some((address, len)) = crate::descriptor::host::buffer_range(&view) {
                                take(address, len);
                            }
                        }
                        None => {}
                    }
                }
            }
        }
        for root in roots {
            if let Root::View(address) = root {
                take(*address, 4096);
            }
        }
        // The root constants have no address: a made-up one each.
        for (i, root) in roots.iter().enumerate() {
            if let Root::Constants(values) = root {
                let bytes: Vec<u8> = values.iter().flat_map(|v| v.to_le_bytes()).collect();
                buffers.push((u64::MAX - i as u64, bytes));
            }
        }
        buffers
    }

    #[allow(clippy::too_many_arguments)]
    fn early_textures_with(
        &self,
        list: &HostList,
        bake: bool,
        early_ms: u64,
        bake_ms: u64,
        frame: u64,
        last_frame: &AtomicU64,
        baked_ms: &AtomicU64,
    ) -> bool {
        // Check for a bound view heap before claiming the frame's early pass,
        // so draws without a view heap do not burn the frame's one early run.
        let Some(heap) = self
            .heaps
            .iter()
            .filter(|&&h| h != 0)
            .map(|&h| crate::descriptor::heap(h as This))
            .find(|h| !h.samplers())
        else {
            return false;
        };
        let Some(host) = heap.host() else {
            return false;
        };
        let Some((budget, bake)) =
            Self::claim_early_budget(frame, bake, early_ms, bake_ms, last_frame, baked_ms)
        else {
            return false;
        };
        // The frame's early pass, not a bake's: no longer than the full
        // card's time while the card is at its line ([`full_card_span`]).
        let budget = match full_card_span() {
            Some(full) if !bake => (budget.0.min(full.as_millis() as u64), budget.1),
            _ => budget,
        };
        let n = EARLY_TEXTURE_PASSES[usize::from(bake)].fetch_add(1, Ordering::Relaxed) + 1;
        if bake && n.is_power_of_two() {
            let [slots, due, ..] = HEAP_TEXTURES_QUEUE.each_ref().map(|c| c.load(Ordering::Relaxed));
            tracing::info!(
                frame,
                bakes = n,
                queued_at_the_last_pass = slots,
                due_at_the_last_pass = due,
                uploaded_so_far = HEAP_TEXTURES.load(Ordering::Relaxed),
                "a draw of the title's decoration bake: textures picked by index are uploaded first (VICTORY_BAKE_TEXTURES_MS=0: not)"
            );
        }
        let began = std::time::Instant::now();
        let uploaded_before = HEAP_TEXTURES.load(Ordering::Relaxed);
        PASS_BUDGET.set(Some(budget));
        self.heap_textures(list, heap, host);
        // A pass takes at most 4,096 slots, and a bake reads whatever its
        // shader picks, once: before a bake the pass runs again for as long
        // as slots were due and it uploaded some, within the bake's time.
        // (With one pass the first bakes of a run met 4,400 to 6,800 slots
        // due on a Radeon RX 7800 XT, and the clothes of the characters
        // baked then stayed stained for the run.)
        let mut passes = 1u32;
        while bake && passes < 64 {
            let due = HEAP_TEXTURES_QUEUE[1].load(Ordering::Relaxed);
            let spent = began.elapsed().as_millis() as u64;
            if due == 0 || spent >= budget.0 {
                break;
            }
            let before = HEAP_TEXTURES.load(Ordering::Relaxed);
            PASS_BUDGET.set(Some((budget.0 - spent, budget.1)));
            self.heap_textures(list, heap, host);
            passes += 1;
            if HEAP_TEXTURES.load(Ordering::Relaxed) == before {
                break;
            }
        }
        PASS_BUDGET.set(None);
        PASS_SOURCES.set(None);
        if let Some(sources) = &sources {
            let done = PASS_SOURCED.get();
            BAKE_SOURCED_BYTES.fetch_add(done[1], Ordering::Relaxed);
            Self::log_bake_sources(p, sources, done, Self::bake_missing(sources, host), force);
        }
        if bake {
            baked_ms.fetch_add(began.elapsed().as_millis() as u64, Ordering::Relaxed);
            if n.is_power_of_two() {
                tracing::info!(
                    frame,
                    bakes = n,
                    passes,
                    uploaded = HEAP_TEXTURES.load(Ordering::Relaxed) - uploaded_before,
                    due_at_the_last_pass = HEAP_TEXTURES_QUEUE[1].load(Ordering::Relaxed),
                    waiting_for_their_memory = HEAP_TEXTURES_QUEUE[2].load(Ordering::Relaxed),
                    ms = began.elapsed().as_millis() as u64,
                    card_has_room = crate::resource::room(),
                    bake_sources_named = BAKE_SOURCES.with_borrow(Vec::len),
                    bake_sources_due_so_far = BAKE_SOURCES_SEEN[0].load(Ordering::Relaxed),
                    bake_sources_uploaded_so_far = BAKE_SOURCES_SEEN[1].load(Ordering::Relaxed),
                    bake_sources_made_past_the_line_so_far = BAKE_SOURCES_SEEN[2].load(Ordering::Relaxed),
                    "textures picked by index uploaded before a draw of the decoration bake (its own sources first: VICTORY_BAKE_SOURCES_FIRST=0 not)"
                );
            }
        }
        true
    }

    /// The textures a draw of the title's decoration bake samples
    /// ([`BakeTables`]), by what the queue of textures picked by index
    /// holds of them. `None`: the draw's table does not hold what a bake's
    /// does.
    fn bake_sources(
        &self,
        tables: BakeTables,
        heap: &crate::descriptor::Heap,
        host: &crate::descriptor::host::HostHeap,
    ) -> Option<BakeSources> {
        let Some(&Root::Table(table)) = self.roots_ref(false).get(usize::from(tables.slot)) else {
            return None;
        };
        let View::Constant(constants, size) = crate::descriptor::view(table + u64::from(tables.constants) * 4)?
        else {
            return None;
        };
        let mut words = [0u8; 0x80];
        if (size as usize) < words.len() || !crate::host::read_into(constants as *const u8, &mut words) {
            return None;
        }
        let (address, len) = crate::descriptor::view(table + u64::from(tables.records) * 4)
            .and_then(|view| crate::descriptor::host::buffer_range(&view))
            .unwrap_or((0, 0));
        let held = u32::try_from(len / BAKE_RECORD).unwrap_or(u32::MAX);
        let (named, records) = bake_named(&words, held, |i| {
            let mut record = [0u8; 0x44];
            crate::host::read_into((address + u64::from(i) * BAKE_RECORD) as *const u8, &mut record).then_some(record)
        });
        let mut sources = BakeSources { records, ..Default::default() };
        let mut textures = Vec::new();
        for index in named {
            let Some(handle) = heap.handle(index) else {
                continue;
            };
            if crate::descriptor::view(handle).is_some_and(|view| crate::descriptor::host::is_texture_view(&view)) {
                textures.push((index, handle));
            }
        }
        let indices: Vec<u32> = textures.iter().map(|t| t.0).collect();
        sources.queued = host.texture_slots_queued(&indices).into_iter().map(|q| q.0).collect();
        sources.textures = textures;
        for &(index, handle) in &sources.textures {
            if sources.queued.contains(&index) {
                continue;
            }
            if host.texture_slot_made_from(index, handle) {
                sources.ready += 1;
            } else if let Some(View::Placed(false, resource, desc)) = crate::descriptor::view(handle)
                && crate::texture::mapped_bytes(
                    (u64::from(desc[10]) | u64::from(desc[11]) << 32) as *const u8,
                    crate::resource::placed_bytes(&resource).unwrap_or(1) as usize,
                ) > 0
            {
                sources.unmade.push(index);
            } else {
                sources.other += 1;
            }
        }
        Some(sources)
    }

    /// Of a bake draw's textures, the ones the draw samples wrong: placed
    /// by the title in memory that is there, with a host slot that shows
    /// nothing of them. Counted when the passes before the draw are done.
    fn bake_missing(sources: &BakeSources, host: &crate::descriptor::host::HostHeap) -> u64 {
        let missing = |&&(index, handle): &&(u32, u64)| {
            !host.texture_slot_made_from(index, handle)
                && matches!(
                    crate::descriptor::view(handle),
                    Some(View::Placed(false, resource, desc)) if crate::texture::mapped_bytes(
                        (u64::from(desc[10]) | u64::from(desc[11]) << 32) as *const u8,
                        crate::resource::placed_bytes(&resource).unwrap_or(1) as usize,
                    ) > 0
                )
        };
        sources.textures.iter().filter(missing).count() as u64
    }

    /// A line for a bake draw's sources: for the first draws, then at
    /// counts that double, and for up to 256 draws that sample some of
    /// theirs wrong (`missing`, [`State::bake_missing`]).
    fn log_bake_sources(p: &HostPipeline, sources: &BakeSources, done: [u64; 4], missing: u64, forced: bool) {
        static DRAWS: AtomicU64 = AtomicU64::new(0);
        static SHORT: AtomicU64 = AtomicU64::new(0);
        static TOTALS: [AtomicU64; 7] = [const { AtomicU64::new(0) }; 7];
        let n = DRAWS.fetch_add(1, Ordering::Relaxed) + 1;
        let waiting = sources.queued.len() as u64;
        let unmade = sources.unmade.len() as u64;
        let [uploaded, bytes, unmapped, as_before] = done;
        for (total, now) in
            TOTALS.iter().zip([waiting, uploaded, unmapped, u64::from(sources.other), unmade, as_before, missing])
        {
            total.fetch_add(now, Ordering::Relaxed);
        }
        if !(n.is_power_of_two() || (missing > 0 && SHORT.fetch_add(1, Ordering::Relaxed) < 256)) {
            return;
        }
        let pixel = p.stages.iter().find(|s| s.shader.stage == "pixel").map_or("", |s| s.shader.hash.as_str());
        let [waited, uploads, unmapped_so_far, others, unmade_so_far, as_before_so_far, missing_so_far] =
            TOTALS.each_ref().map(|t| t.load(Ordering::Relaxed));
        tracing::info!(
            frame = crate::frame::current(),
            draws = n,
            pixel,
            records = sources.records.0,
            records_read = sources.records.1,
            textures = sources.textures.len(),
            on_the_card = sources.ready,
            waiting,
            made_stale = unmade,
            the_host_s_own = sources.other,
            forced,
            uploaded,
            kilobytes = bytes >> 10,
            as_far_as_mapped_already = as_before,
            not_mapped = unmapped,
            sampled_wrong = missing,
            sampled_wrong_so_far = missing_so_far,
            megabytes_so_far = BAKE_SOURCED_BYTES.load(Ordering::Relaxed) >> 20,
            waited_so_far = waited,
            uploaded_so_far = uploads,
            as_far_as_mapped_already_so_far = as_before_so_far,
            not_mapped_so_far = unmapped_so_far,
            made_stale_so_far = unmade_so_far,
            the_host_s_own_so_far = others,
            "a decoration bake draw's own textures, by its constants and records: the ones still waiting are uploaded before it (VICTORY_BAKE_SOURCES=0: only counted)"
        );
    }

    /// The queue's side of the card accounting line
    /// (`crate::resource::card_accounting`, which says what the line
    /// holds): the slots that are due, by what their texture has on the
    /// card. Off unless `VICTORY_CARD_ACCOUNTING=1`; then at the first pass
    /// that finds the card without room, and every five seconds after it.
    /// Nothing is made, uploaded or counted as used here. For 18,000 due
    /// slots: 18,000 looks at a title slot and at the texture it views, a
    /// lock each, some milliseconds of the thread that records the frame.
    fn card_accounting(
        heap: &crate::descriptor::Heap,
        host: &crate::descriptor::host::HostHeap,
        frame: u64,
        room: bool,
    ) {
        static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        if !*ON.get_or_init(|| std::env::var_os("VICTORY_CARD_ACCOUNTING").is_some_and(|v| v == "1")) {
            return;
        }
        // When the next line is due (0: none yet, the card has had room).
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let now = crate::host::now_ms().max(1);
        let next = NEXT.load(Ordering::Relaxed);
        // (With levels on demand the card may never be without room: the
        // lines then start with the first texture made that way.)
        let by_level = crate::tiles::STATS[4].load(Ordering::Relaxed) > 0;
        let due_now = if next == 0 { !room || by_level } else { now >= next };
        if !due_now || NEXT.compare_exchange(next, now + 5000, Ordering::Relaxed, Ordering::Relaxed).is_err() {
            return;
        }
        // Due slots; of them uploaded before; of them viewing nothing the
        // title fills; and the textures (each once) whose host copy is
        // alive, with their bytes on the card.
        let mut due = [0u64; 5];
        // Textures (each once) that never had a host copy, and ones whose
        // host copy was let go: where their memory is, and its bytes.
        let (mut never, mut gone) = (Vec::new(), Vec::new());
        let mut seen = std::collections::BTreeSet::new();
        for (index, uploaded) in host.texture_slots_all_due(frame) {
            due[0] += 1;
            due[1] += u64::from(uploaded);
            let state = match heap.handle(index).and_then(crate::descriptor::view) {
                Some(View::Placed(false, resource, desc)) if resource[0] != 1 => {
                    let address = u64::from(desc[10]) | u64::from(desc[11]) << 32;
                    match crate::resource::placed_known(address, &resource) {
                        Some(texture) => crate::resource::copy_state(texture),
                        // No title texture was made for it: the pass never
                        // got as far as making one.
                        None => Some((0, crate::resource::placed_bytes(&resource).unwrap_or(0), address)),
                    }
                }
                Some(View::Shader(object, _)) if crate::resource::title_fills(object as This) => {
                    crate::resource::copy_state(object as This)
                }
                _ => None,
            };
            let Some((state, bytes, memory)) = state else {
                due[2] += 1;
                continue;
            };
            if !seen.insert(memory) {
                continue;
            }
            match state {
                2 => {
                    due[3] += 1;
                    due[4] += bytes;
                }
                1 => gone.push((memory, bytes)),
                _ => never.push((memory, bytes)),
            }
        }
        crate::resource::card_accounting(room, due, never, gone);
    }

    /// The texture side of [`State::heap_buffers`]: textures the title fills
    /// itself (streamed pages, layout-library copies) whose views sit in the
    /// view heap are created (placed textures have no host texture until
    /// something reads them) and uploaded, as a table read would do, for
    /// shaders that pick them by a computed index. Within a budget of bytes
    /// per frame and in all; the rest waits for later lists and frames.
    /// In the order `TextureQueue::due` gives: slots looked at before and
    /// first looks in turns; first looks newest and oldest in turns;
    /// slots looked at before newest first (`VICTORY_RETRY_TURNS=1`: in
    /// turns too). Slots draws read through tables are not in the queue.
    /// shortcut: uploaded once, and again only when the title rewrites the
    /// slot. A placed texture not all mapped yet is uploaded with what is
    /// there and looked at again (1, 2, 4, ... frames later, at most about
    /// a second): uploaded again when more of it is mapped. Pages mapped
    /// in place of others, and contents changed with no descriptor write,
    /// are missed (hook the page-mapping events for those).
    /// `VICTORY_HEAP_TEXTURES=0` turns it off.
    /// On a thread of its own the pass records into lists of its own, and
    /// the slots show what they upload from then on
    /// (`crate::heap_worker`).
    pub(crate) fn heap_textures(
        mut into: Into<'_>,
        heap: &crate::descriptor::Heap,
        host: &crate::descriptor::host::HostHeap,
    ) {
        static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        if !*ON.get_or_init(|| std::env::var_os("VICTORY_HEAP_TEXTURES").is_none_or(|v| v != "0")) {
            return;
        }
        /// Bytes of texture memory staged per frame.
        // `VICTORY_TEXTURE_STAGE_MB` raises it: a backlog of tens of thousands of
        // textures (a scene's first minutes) then drains in fewer frames.
        static PER_FRAME_MB: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
        let per_frame: u64 = *PER_FRAME_MB.get_or_init(|| {
            std::env::var("VICTORY_TEXTURE_STAGE_MB").ok().and_then(|v| v.trim().parse().ok()).unwrap_or(192)
        }) << 20;
        static FRAME: AtomicU64 = AtomicU64::new(u64::MAX);
        static SPENT: AtomicU64 = AtomicU64::new(0);
        let frame = crate::frame::current();
        if FRAME.swap(frame, Ordering::Relaxed) != frame {
            SPENT.store(0, Ordering::Relaxed);
        }
        // No more of them are made while the card has no room
        // (`resource::room`): by what is alive now, not by what was ever
        // staged (4 GB in all, after which later textures stayed empty for
        // the run). The ones nothing names any more are let go, which
        // makes room again: so the slots are still looked at (the pass
        // returned here, and a slot whose texture had been streamed out
        // kept its host copy for as long as the card was full).
        let mut room = crate::resource::room();
        // A bake draw's own textures (`State::early_textures`), and whether
        // this run is for them alone: then nothing of the queue's upkeep is
        // done, which a pass of this frame did or will do.
        let sources = PASS_SOURCES.take();
        let only = sources.as_ref().is_some_and(|s| s.1);
        // Bytes uploaded by this pass since the card was last asked
        // whether it has room: asked again every 32 MiB (a pass may upload
        // 192 MB and more, and one asking a pass let the queue run some
        // 400 MB past its line while the world loaded: run 193).
        let mut since_asked = 0u64;
        if !room && !only {
            HEAP_TEXTURES_FULL.fetch_add(1, Ordering::Relaxed);
            LAST_FULL_MS.store(crate::host::now_ms() + 1, Ordering::Relaxed);
            once!("textures picked by index: the card has no room for more; they wait");
        }
        // The heap's own pass (not an early one): its parts are counted.
        let counted = PASS_BUDGET.get().is_none();
        let add = |i: usize, v: u64| {
            if counted {
                split_add(i, v);
            }
        };
        add(split::WITH_ROOM, u64::from(room));
        let mut refused = 0;
        let part = std::time::Instant::now();
        // Room again: the slots held back while the card was full are due
        // now (`full_card_pass`).
        if room && !only {
            host.texture_slots_room(frame);
        }
        // Slots whose texture's memory the runtime mapped, unmapped or
        // wrote since the last pass are due again.
        host.texture_slots_remapped(frame);
        add(split::REMAPPED_US, micros(part));
        let part = std::time::Instant::now();
        host.texture_sweep(heap, frame);
        add(split::SWEEP_US, micros(part));
        let part = std::time::Instant::now();
        if heap_recheck() {
            relook_textures(host, frame);
        }
        add(split::RELOOK_US, micros(part));
        let part = std::time::Instant::now();
        // Levels on demand: the slots of textures whose first level with
        // memory changed since the last pass are made again from it.
        host.texture_views_remade();
        // And with room, the textures whose upload left a held level
        // without the card's memory are uploaded again.
        if room && !only {
            host.texture_slots_waiting(frame);
        }
        let (mut due, census) = host.texture_slots_due(frame, 4096);
        // Before a bake draw: its own sources first ([`bake_sources_first`]).
        let first = BAKE_SOURCES.with_borrow(|sources| {
            if sources.is_empty() { Vec::new() } else { host.texture_slots_due_of(frame, sources) }
        });
        let firsts = first.len();
        if firsts > 0 {
            BAKE_SOURCES_SEEN[0].fetch_add(firsts as u64, Ordering::Relaxed);
            due.retain(|d| !first.iter().any(|f| f.0 == d.0));
            due.splice(0..0, first);
        }
        // Waiting for an upload: not the slots uploaded whole that wait for
        // their next look at the memory.
        HEAP_TEXTURES_WAITING.store(census[0] - census[6], Ordering::Relaxed);
        for (count, now) in HEAP_TEXTURES_QUEUE.iter().zip(census) {
            count.store(now, Ordering::Relaxed);
        }
        // The bake draw's own come first, due or not, and once.
        let sourced = match &sources {
            Some((slots, _)) => {
                let first = host.texture_slots_queued(slots);
                due.retain(|d| !first.iter().any(|f| f.0 == d.0));
                let n = first.len();
                due.splice(0..0, first);
                n
            }
            None => 0,
        };
        let sourced_at_most = bake_sources_forced().1 << 20;
        let mut sourced_bytes = 0u64;
        // Where the card's memory is, and what the due slots would need
        // (`VICTORY_CARD_ACCOUNTING=1`; a line when the card first has no
        // room, and every five seconds after it).
        Self::card_accounting(heap, host, frame, room);
        add(split::DUE_US, micros(part));
        // The frames in about four seconds, between 16 and 256: how long a
        // slot with nothing to upload waits at most for its next look. In
        // frames alone it was 120: four minutes at the opening cutscene's
        // half a frame per second, while the title streams by the clock.
        // (About a second before the runtime told of the memory it maps:
        // the looks are now only for what that does not tell.)
        static PASSED: AtomicU64 = AtomicU64::new(0);
        let now = crate::host::now_ms();
        let took = if only { 100 } else { now.saturating_sub(PASSED.swap(now, Ordering::Relaxed)).max(1) };
        // The worker may run two passes in a frame: no shorter than that.
        let took = if matches!(into, Into::Ahead(_)) { took.max(100) } else { took };
        let most = (4000 / took).clamp(16, 256);
        // What a table entry of a texture a shader samples says.
        let sampled = UsedResource {
            kind: ResourceType::Srv,
            root: RootType::Table,
            table_offset_dwords: 0,
            lower_bound: 0,
            stride: 0,
            dwords: 8,
            user_data: 0,
            slot: 0,
        };
        // Also within a time per pass: the pass runs on a thread that
        // records the frame, and making and staging 192 MB of textures took
        // 1.6-2.2 s of each of the world's first twenty frames (run 20).
        // The rest waits for the next frames, as it does past the bytes.
        static SPAN_MS: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
        let span = std::time::Duration::from_millis(*SPAN_MS.get_or_init(|| {
            std::env::var("VICTORY_TEXTURE_STAGE_MS").ok().and_then(|v| v.trim().parse().ok()).unwrap_or(250)
        }));
        // A backlog (a scene's first minutes: 22,000 slots due, 2-3 minutes of black
        // surfaces at 192 MB a frame) gets three times the bytes and the time until it
        // drains; `VICTORY_TEXTURE_CATCHUP=0` keeps the plain budget.
        static CATCHUP: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        let catchup = *CATCHUP.get_or_init(|| std::env::var_os("VICTORY_TEXTURE_CATCHUP").is_none_or(|v| v != "0"));
        // Not while the card is at its line ([`full_card_span`]): the
        // backlog then waits for room, not for time, and the pass gets the
        // full card's time.
        let full = if counted { full_card_span() } else { None };
        let (per_frame, span) = texture_pass_budget(per_frame, span, catchup && due.len() >= 1024, full);
        add(split::CAPPED, u64::from(full.is_some()));
        // A run made early (`State::early_textures`) has a budget of its
        // own: its bytes on top of what the frame has spent so far.
        // (Not `most`: that is the longest wait between looks, used below.)
        let (span, bytes_at_most) = match PASS_BUDGET.get() {
            Some((ms, bytes)) => {
                (std::time::Duration::from_millis(ms), SPENT.load(Ordering::Relaxed).saturating_add(bytes))
            }
            None => (span, per_frame),
        };
        let began = std::time::Instant::now();
        // Per slot of the loop: when it began, and whether it uploaded.
        let mut slot: Option<std::time::Instant> = None;
        let mut slot_made = false;
        // This pass's slots: uploaded, passed over unmapped, and the
        // microseconds of the uploads in making their host views.
        let (mut made, mut away, mut mirror_us) = (0u64, 0u64, 0u64);
        let spent_before = SPENT.load(Ordering::Relaxed);
        let end_slot = |slot: &mut Option<std::time::Instant>, made: bool| {
            if let Some(at) = slot.take() {
                let us = micros(at);
                add(if made { split::MADE_US } else { split::OTHER_US }, us);
                if counted {
                    split_max(split::LONGEST_SLOT_US, us);
                }
            }
        };
        for (at, (index, write)) in due.into_iter().enumerate() {
            end_slot(&mut slot, slot_made);
            slot_made = false;
            if SPENT.load(Ordering::Relaxed) >= bytes_at_most || began.elapsed() > span {
                add(split::STOPPED_ON_TIME, u64::from(began.elapsed() > span));
                break;
            }
            slot = Some(std::time::Instant::now());
            let bake_source = at < firsts;
            // A placed texture whose memory is not mapped (not streamed in
            // yet, or streamed out again: the heap keeps tens of thousands
            // of such views) gets no host texture; looked at again soon (a
            // title writes a texture's view before its pages are mapped,
            // and the pass runs early in a frame). One of which some is
            // mapped (a texture streaming in: its smaller mips lie at the
            // start of its memory and arrive first) is uploaded with what
            // is there, and again when more is. (It was uploaded again
            // every 120 frames, changed or not; and asked by its first
            // page alone, one whose first page is the one missing counted
            // as not there.)
            let mut partly = None;
            if let Some((View::Placed(false, resource, desc), _)) = &record {
                let address = u64::from(desc[10]) | u64::from(desc[11]) << 32;
                // Of the mips the view shows: what lies behind them is
                // another texture's.
                let len = crate::resource::placed_bytes_from(&resource, crate::resource::first_mip(&desc)).unwrap_or(1);
                let mapped = crate::texture::mapped_bytes(address as *const u8, len as usize);
                if mapped == 0 {
                    if source {
                        let [uploaded, bytes, unmapped, as_before] = PASS_SOURCED.get();
                        PASS_SOURCED.set([uploaded, bytes, unmapped + 1, as_before]);
                    }
                    HEAP_TEXTURES_AWAY.fetch_add(1, Ordering::Relaxed);
                    add(split::AWAY, 1);
                    away += 1;
                    // Streamed out: what the slot showed is let go.
                    host.texture_slot_gone(index, write);
                    host.texture_slot_later(index, write, frame, most);
                    continue;
                }
                if mapped < len {
                    // (One of a bake draw's own is left so only if the
                    // host slot shows what was uploaded then.)
                    if mapped <= host.texture_slot_mapped(index)
                        && !(source
                            && !heap.handle(index).is_some_and(|handle| host.texture_slot_made_from(index, handle)))
                    {
                        if source {
                            let [uploaded, bytes, unmapped, as_before] = PASS_SOURCED.get();
                            PASS_SOURCED.set([uploaded, bytes, unmapped, as_before + 1]);
                        }
                        HEAP_TEXTURES_PARTLY[0].fetch_add(1, Ordering::Relaxed);
                        if partly_less() {
                            host.texture_slot_partly_less(index, write, frame, most, mapped);
                        } else {
                            host.texture_slot_later(index, write, frame, most);
                        }
                        continue;
                    }
                    if !crate::texture::committed(address as *const u8, 1) {
                        HEAP_TEXTURES_PARTLY[2].fetch_add(1, Ordering::Relaxed);
                    }
                    partly = Some(mapped);
                }
            }
            // A texture object the host renders to or writes holds the
            // host's own contents: nothing to bring up to date, and this
            // pass is no use of it (its host copy is made when something
            // uses it, and its slots then: `descriptor::make_waiting`).
            if let Some((View::Shader(object, _), _)) = &record
                && crate::resource::buffer_bytes(*object as This).is_none()
                && !crate::resource::title_fills(*object as This)
            {
                host.texture_slot_done(index, write, None);
                continue;
            }
            // From here on a host texture is made: not without room. The
            // slot stays due.
            // Not so a slot already made from this very view: its host
            // texture exists, and uploading it again takes none of the
            // card's memory. It was refused all the same, so a texture the
            // title filled after its first upload kept its empty host copy
            // from the moment the card reached the line, which is the first
            // world frame in eight launches of nine: the same slots were
            // found changed again at every pass and never uploaded (run
            // 179: 33,000 such findings), and the main character's face
            // had its colour only in the launches that entered the world
            // under the line. `VICTORY_REUPLOAD_WHEN_FULL=0`: refused, as
            // before.
            let (mut short, mut reupload) = (false, false);
            if !room && bake_source && crate::resource::room_for_bake() {
                // A bake's own source: made past the line, up to the
                // bake's ([`bake_sources_first`]).
                BAKE_SOURCES_SEEN[2].fetch_add(1, Ordering::Relaxed);
            } else if !room {
                let has_its_texture = reupload_when_full()
                    && heap.handle(index).is_some_and(|handle| host.texture_slot_made_from(index, handle));
                // Not whole, then: without its largest mips, up to a higher
                // line ([`crate::resource::ShortOfRoom`]).
                short = !has_its_texture && crate::resource::room_for_short();
                if !has_its_texture && !short {
                    refused += 1;
                    add(split::REFUSED, 1);
                    HEAP_TEXTURES_REFUSED.fetch_add(1, Ordering::Relaxed);
                    if full_card_pass() {
                        // Held back for the frames of about four seconds
                        // (`most`), or until the card has room; the pass
                        // goes on, within a count that bounds one pass.
                        host.texture_slot_held(index, write, frame + most);
                        if refused > 1024 {
                            break;
                        }
                    } else if refused > 256 {
                        break;
                    }
                    continue;
                }
                if short {
                    HEAP_TEXTURES_SHORT.fetch_add(1, Ordering::Relaxed);
                }
                reupload = has_its_texture;
            }
            // For the rest of this slot's turn: a host copy made here is
            // made without its largest mips.
            let _short = short.then(crate::resource::ShortOfRoom::begin);
            if reupload {
                let n = HEAP_TEXTURES_REUPLOADED_FULL.fetch_add(1, Ordering::Relaxed) + 1;
                if n == 1 || n.is_multiple_of(500) {
                    tracing::info!(
                        so_far = n,
                        slot = index,
                        "a texture already on the card is brought up to date although the card has no room for new ones"
                    );
                }
            }
            let mirroring = std::time::Instant::now();
            let view = heap.handle(index).and_then(|handle| host.mirror(index, handle, true));
            let mirrored = micros(mirroring);
            let placed = view.as_ref().and_then(crate::descriptor::host::placed_texture);
            let resource = match placed.as_ref().or(view.as_ref()) {
                Some(&View::Shader(resource, _)) => resource as This,
                _ => {
                    host.texture_slot_done(index, write, None);
                    continue;
                }
            };
            let title_fills = crate::resource::host_resource(resource).is_some_and(crate::texture::title_written);
            if !title_fills {
                // a render target, a depth or unordered access texture: the host's own
                host.texture_slot_done(index, write, None);
                continue;
            }
            // A texture that has a staging buffer now is one draws read
            // through a table: they upload it, and check it at every
            // submission that samples it.
            let tabled = crate::texture::is_staged(resource);
            if tabled {
                crate::texture::upload_again(resource);
            }
            if let Some(view) = &view {
                match &mut into {
                    Into::List(list) => readable(list, sampled.kind, view),
                    Into::Ahead(batch) => {
                        let Some(list) = batch.list() else {
                            return;
                        };
                        readable(list, sampled.kind, view);
                        if let (Some(handle), Some((_, key))) = (heap.handle(index), &record) {
                            batch.publish(index, handle, *key);
                        }
                    }
                }
            }
            #[cfg(feature = "diagnostics")]
            {
                HEAP_TEXTURE_PARTS[2].fetch_add(staging.elapsed().as_micros() as u64, Ordering::Relaxed);
                HEAP_TEXTURE_PARTS[3].fetch_add(1, Ordering::Relaxed);
            }
            // A texture view the host refuses removes the device: name it.
            static GONE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
            if !GONE.load(Ordering::Relaxed)
                && let Some(why) = crate::descriptor::host::device_removed()
            {
                GONE.store(true, Ordering::Relaxed);
                let text: String = format!("{view:?}").chars().take(300).collect();
                tracing::error!(why, slot = index, "the host device went while textures picked by index were made ready; last: {text}");
                crate::host::log_removal_context();
                return;
            }
            let (bytes, whole) = crate::texture::staged(resource).unwrap_or((0, true));
            // Not all of it there: looked at again next frame, and from
            // then on as a slot with nothing new (a placed texture: until
            // more of it is mapped; a texture object at an address the
            // title mapped: again in 120 frames, as before).
            let again = match partly {
                Some(mapped) => Some((frame + 1, mapped)),
                None => (!whole).then_some((frame + 120, 0)),
            };
            if again.is_some() {
                HEAP_TEXTURES_PARTLY[1].fetch_add(1, Ordering::Relaxed);
            }
            if source {
                let [uploaded, was, unmapped, as_before] = PASS_SOURCED.get();
                PASS_SOURCED.set([uploaded + 1, was + if tabled { 0 } else { bytes }, unmapped, as_before]);
                sourced_bytes += bytes;
            }
            if tabled {
                // Nothing was staged here, so nothing is counted or spent,
                // and the staging buffer stays: taking it away made the
                // next draw that reads the texture through a table stage,
                // detile and upload all of it once more.
                HEAP_TEXTURES_TABLED.fetch_add(1, Ordering::Relaxed);
                if bake_source {
                    BAKE_SOURCES_SEEN[1].fetch_add(1, Ordering::Relaxed);
                }
                #[cfg(feature = "diagnostics")]
                crate::bake_probe::uploaded(heap, index, frame, partly.is_some(), true);
                host.texture_slot_done(index, write, again);
                continue;
            }
            // Uploaded: the list holds the staging buffer until it has run;
            // a table read, or the next look at a texture not all mapped
            // yet, makes it anew. (Kept for those until now: every partly
            // streamed texture of the heap held a staging buffer of its full
            // size for good.)
            let memory = if again.is_none() && heap_recheck() {
                crate::texture::staged_memory(resource)
            } else {
                None
            };
            crate::texture::unstage(resource);
            SPENT.fetch_add(bytes, Ordering::Relaxed);
            if let Into::Ahead(batch) = &mut into {
                batch.uploaded(bytes);
            }
            since_asked += bytes;
            if room && since_asked >= 32 << 20 {
                since_asked = 0;
                room = crate::resource::room();
                if !room {
                    LAST_FULL_MS.store(crate::host::now_ms() + 1, Ordering::Relaxed);
                }
            }
            HEAP_TEXTURE_BYTES.fetch_add(bytes, Ordering::Relaxed);
            HEAP_TEXTURES.fetch_add(1, Ordering::Relaxed);
            if let Some(waited) = host.texture_slot_waited(index, write, frame) {
                HEAP_WAITED[waited_bucket(waited)].fetch_add(1, Ordering::Relaxed);
                HEAP_WAITED_MOST.fetch_max(waited, Ordering::Relaxed);
            }
            match memory {
                // Whole and uploaded: looked at again ([`relook_textures`]).
                Some((at, len)) => {
                    let print = crate::texture::fingerprint(at, len);
                    if host.texture_slot_uploaded_whole(index, write, frame, (at, len), print) {
                        HEAP_TEXTURES_RECHECKED[2].fetch_add(1, Ordering::Relaxed);
                    }
                }
                None => host.texture_slot_done(index, write, again),
            }
        }
        end_slot(&mut slot, slot_made);
        // A long pass of the heap's own, named (diagnostics: the first 20,
        // then every 50th).
        #[cfg(feature = "diagnostics")]
        if counted && began.elapsed() >= std::time::Duration::from_millis(100) {
            static LONG: AtomicU64 = AtomicU64::new(0);
            let n = LONG.fetch_add(1, Ordering::Relaxed);
            if n < 20 || n.is_multiple_of(50) {
                tracing::info!(
                    frame,
                    loop_ms = began.elapsed().as_millis() as u64,
                    span_ms = span.as_millis() as u64,
                    room,
                    full_card_time = full.is_some(),
                    due_at_start = census[1],
                    made,
                    made_mb = SPENT.load(Ordering::Relaxed).saturating_sub(spent_before) >> 20,
                    mirror_ms = mirror_us / 1000,
                    away,
                    refused,
                    long_passes = n + 1,
                    "a long pass over textures picked by index"
                );
            }
        }
        #[cfg(not(feature = "diagnostics"))]
        let _ = (made, away, mirror_us, spent_before, census);
    }

    /// For the frame listing, after a dropped dispatch's line: its group
    /// counts and the compute root arguments it was recorded with (root
    /// constants as hex dwords, root views with the title buffer they
    /// point into). A program that does not translate has no bindings to
    /// list; these say which memory it would have read and written (the
    /// ray-tracing structure builder passes its buffers this way).
    #[cfg(feature = "diagnostics")]
    fn list_dropped_roots(&self, groups: [u32; 3]) {
        crate::capture::op(0, || {
            use std::fmt::Write;
            let [x, y, z] = groups;
            let mut text = format!("  its roots ({x}x{y}x{z}):");
            for (slot, root) in self.compute.iter().enumerate() {
                match root {
                    Root::None => {}
                    Root::Table(a) => {
                        let _ = write!(text, " {slot}=table {a:#x}");
                    }
                    Root::Constants(v) => {
                        let _ = write!(text, " {slot}=constants {:x?}", &v[..v.len().min(48)]);
                    }
                    Root::View(a) => {
                        let _ = write!(text, " {slot}=view {a:#x}");
                        match crate::shadow::extent(*a) {
                            Some((start, len)) => {
                                let _ = write!(text, " in buffer {start:#x}+{len:#x}");
                            }
                            None => text.push_str(" in no buffer"),
                        }
                    }
                }
            }
            text
        });
    }

    /// Runs a dispatch on the host.
    pub(crate) fn dispatch(&mut self, list: &HostList, x: u32, y: u32, z: u32) {
        self.dispatch_inner(list, x, y, z, 0);
    }

    /// Runs a console `DispatchX(x, y, z, flags)` on the host.
    ///
    /// Bit 5 (`0x20`) of `COMPUTE_DISPATCH_INITIATOR` is
    /// `USE_THREAD_DIMENSIONS`: `(x, y, z)` are raw thread counts rather than
    /// thread-group counts, and trailing lanes of partial tail groups are
    /// masked off in initial hardware `EXEC`.
    pub(crate) fn dispatch_x(&mut self, list: &HostList, x: u32, y: u32, z: u32, flags: u32) {
        self.dispatch_inner(list, x, y, z, flags);
    }

    fn dispatch_inner(&mut self, list: &HostList, x: u32, y: u32, z: u32, flags: u32) {
        // It may write the arguments a window holds clamped.
        self.window = None;
        #[cfg(windows)]
        if crate::pipeline::is_skipped(self.pipeline as This) {
            DROPPED.fetch_add(1, Ordering::Relaxed);
            #[cfg(feature = "diagnostics")]
            crate::pipeline::note_drop(self.pipeline as This);
            return;
        }
        // A program that walks ray-tracing structures nothing has built.
        if crate::pipeline::walk_held(self.pipeline as This) {
            #[cfg(feature = "diagnostics")]
            crate::pipeline::note_why("a walk of ray-tracing structures that are not built (VICTORY_BVH_HOLD=1)".to_owned());
            return;
        }
        // The ray-tracing structure builder's stages run all or none.
        if crate::pipeline::is_builder(self.pipeline as This) && !crate::pipeline::builder_runs() {
            DROPPED.fetch_add(1, Ordering::Relaxed);
            #[cfg(feature = "diagnostics")]
            {
                crate::pipeline::note_why("a stage of the ray-tracing structure builder, which is off".to_owned());
                self.list_dropped_roots([x, y, z]);
            }
            return;
        }
        // Past the clamp's bounds (`VICTORY_DIRECT_BOUNDS`): trimmed or
        // dropped, and a stage of the builder dropped stops the others.
        let asked = [x, y, z];
        let Some([x, y, z]) = crate::indirect::direct_bounded(1, asked, crate::indirect::dispatch_bounded(asked)) else {
            DROPPED.fetch_add(1, Ordering::Relaxed);
            if crate::pipeline::is_builder(self.pipeline as This) {
                crate::pipeline::builder_off("one of its dispatches was past the direct bounds");
            }
            return;
        };
        let Some(p) = crate::pipeline::host_pipeline(self.pipeline as This) else {
            once!(
                pipeline = format_args!("{:#x}", self.pipeline),
                "a dispatch without a host pipeline dropped"
            );
            DROPPED.fetch_add(1, Ordering::Relaxed);
            #[cfg(feature = "diagnostics")]
            {
                crate::pipeline::note_drop(self.pipeline as This);
                self.list_dropped_roots([x, y, z]);
            }
            return;
        };
        if !p.compute {
            once!("a dispatch with a graphics pipeline");
            return;
        }
        let t = p.stages[0].translated();
        let flags = thread_dimension_flags(crate::pipeline::is_builder(self.pipeline as This), flags);
        let Some(([gx, gy, gz], partial)) = dispatch_dimensions([x, y, z], t.threads, flags) else {
            return;
        };
        if partial != [0; 3]
            && !t.host.user_data.iter().any(|u| {
                u.root == RootType::Other(victory_shader::emit::USER_DISPATCH_THREADS)
            })
        {
            once!(
                pipeline = %p.name,
                threads = ?[x, y, z],
                group = ?t.threads,
                "partial DispatchX(USE_THREAD_DIMENSIONS) on a compute shader without thread-count masking"
            );
        }
        self.dispatch_threads = partial;
        let bound = self.bind(list, p, true);
        self.dispatch_threads = [0; 3];
        if !bound {
            DROPPED.fetch_add(1, Ordering::Relaxed);
            // A stage of the structure builder that cannot be bound is a
            // dropped stage: the others stop too.
            if crate::pipeline::is_builder(self.pipeline as This) {
                crate::pipeline::builder_off("one of its dispatches could not be bound");
            }
            return;
        }
        #[cfg(feature = "diagnostics")]
        list.note_op(|| self.op_text(format!("dispatch {gx}x{gy}x{gz}"), true));
        let (x, y, z) = (gx, gy, gz);
        match (x > crate::indirect::PART).then(|| group_base(p)).flatten() {
            Some((param, dword)) => {
                // More groups in x than the host runs in one dimension: in
                // parts, each told the number of its first group.
                IN_PARTS[0].fetch_add(1, Ordering::Relaxed);
                static SAID: std::sync::Once = std::sync::Once::new();
                SAID.call_once(|| {
                    tracing::info!(
                        x,
                        parts = x.div_ceil(crate::indirect::PART),
                        "a dispatch of more groups in x than the host runs in one dimension is run in parts"
                    )
                });
                // SAFETY: an open list with everything set; the parameter
                // and dword are the pipeline's.
                unsafe {
                    for (base, groups) in crate::indirect::dispatch_parts(x) {
                        list.list.SetComputeRoot32BitConstant(param, base, dword);
                        list.list.Dispatch(groups, y, z);
                    }
                    list.list.SetComputeRoot32BitConstant(param, 0, dword);
                }
            }
            _ => {
                if x > crate::indirect::PART {
                    once!(
                        x,
                        pipeline = format_args!("{:#x}", self.pipeline),
                        "a dispatch of more groups in x than the host runs in one dimension, by a program that cannot be run in parts: the host gets it as it is"
                    );
                }
                // SAFETY: an open list with everything set.
                unsafe { list.list.Dispatch(x, y, z) }
            }
        }
        DISPATCHED.fetch_add(1, Ordering::Relaxed);
        if self.ambient(list, p) {
            // Our pass set its own heap, root signature and pipeline: the
            // next draw or dispatch sets the title's again.
            self.host_pipeline = 0;
            self.host_roots = [0; 2];
            self.host_heaps = [0; 2];
        }
    }

    /// After a dispatch of `p`: the least indirect light, if it is the
    /// program that resolves it ([`crate::ambient`]).
    fn ambient(&self, list: &HostList, p: &crate::pipeline::host::HostPipeline) -> bool {
        let Some(stage) = p.stages.first().filter(|s| s.shader.resolves_light) else {
            return false;
        };
        // The view the program has in a table, at a user-data slot and a
        // dword in it.
        let view = |kind: ResourceType, (slot, offset): (u8, u32)| {
            let u = stage.translated().used.iter().find(|u| {
                u.kind == kind
                    && u.root == RootType::Table
                    && (u.slot, u.table_offset_dwords) == (slot, offset)
            })?;
            let Some(Root::Table(table)) = self.roots_ref(true).get(usize::from(u.slot)) else {
                return None;
            };
            crate::descriptor::view(table + u64::from(u.table_offset_dwords) * 4)
        };
        // Once, what the program reads and writes through its tables: where
        // to look if the light or the depth moved.
        #[cfg(feature = "diagnostics")]
        {
            static LISTED: std::sync::Once = std::sync::Once::new();
            LISTED.call_once(|| {
                for u in stage.translated().used.iter().filter(|u| u.root == RootType::Table) {
                    let r = match view(u.kind, (u.slot, u.table_offset_dwords)) {
                        Some(crate::descriptor::View::Shader(r, _))
                        | Some(crate::descriptor::View::Unordered(r, ..)) => r,
                        _ => continue,
                    };
                    let Some(t) = crate::resource::host_resource(r as This) else {
                        continue;
                    };
                    // SAFETY: a live resource.
                    let d = unsafe { t.GetDesc() };
                    tracing::info!(
                        kind = ?u.kind,
                        slot = u.slot,
                        offset = u.table_offset_dwords,
                        format = d.Format.0,
                        width = d.Width,
                        height = d.Height,
                        depth = d.DepthOrArraySize,
                        flags = d.Flags.0,
                        "the light resolve uses"
                    );
                }
            });
        }
        let Some(crate::descriptor::View::Unordered(light, ..)) =
            view(ResourceType::Uav, crate::ambient::LIGHT)
        else {
            once!("no ambient light: the program's light is not where it was");
            return false;
        };
        // The scene's depth, as the program read it: the dispatch left it in
        // a state shaders read.
        let depth = match view(ResourceType::Srv, crate::ambient::DEPTH) {
            Some(crate::descriptor::View::Shader(depth, _)) => Some(depth as This),
            _ => None,
        };
        crate::ambient::raise(list, light as This, depth)
    }

    /// What a draw or dispatch runs, for the device-removed report
    /// (diagnostics): the pipeline, then per stage the user data it gets
    /// (with the view at that heap index, if one is), the host addresses
    /// of its root views and the views its tables name.
    #[cfg(feature = "diagnostics")]
    fn op_text(&self, kind: String, compute: bool) -> String {
        use std::fmt::Write;
        let Some(p) = crate::pipeline::host_pipeline(self.pipeline as This) else {
            return kind;
        };
        let mut text = format!("{kind} {}", p.name);
        // What it draws into (a dispatch writes through its views below).
        if !compute {
            let described = |t: usize| {
                crate::resource::host_made(t as This).map(|r| {
                    // SAFETY: a live resource.
                    let d = unsafe { r.GetDesc() };
                    format!("{t:#x} {}x{} f{}", d.Width, d.Height, d.Format.0)
                })
            };
            let targets: Vec<String> = self.targets.iter().filter_map(|&t| described(t)).collect();
            let _ = write!(text, " targets {targets:?} depth {:?} state [{}]", described(self.depth), p.state);
            // The title's scissor and viewport at this draw (the first of
            // each): the title draws what it sees through a
            // portal (a window, from inside a room) under the portal's
            // rectangle, and the listing did not say which draws those are.
            if let Some(r) = self.scissors.first() {
                let _ = write!(text, " scissor [{} {} {} {}]", r.left, r.top, r.right, r.bottom);
            }
            if let Some(v) = self.viewports.first() {
                let _ = write!(
                    text,
                    " viewport [{} {} {} {} depth {}..{}]",
                    v.TopLeftX, v.TopLeftY, v.Width, v.Height, v.MinDepth, v.MaxDepth
                );
            }
        }
        let views = (self.heaps[0] != 0)
            .then(|| crate::descriptor::heap(self.heaps[0] as This).host())
            .flatten();
        for stage in &p.stages {
            let t = stage.translated();
            let _ = write!(text, " | {}:", stage.shader.hash);
            for u in &t.host.user_data {
                let v = match u.root {
                    RootType::Other(kind) => self.host_word(compute, kind, u.slot, u.dword, None),
                    _ => self.user_value(compute, u.slot, u.root, u.dword),
                };
                let _ = write!(text, " u{}={v:#x}", u.index);
                let index = if u.root == RootType::Table { v / 32 } else { v };
                if let Some(slot) = views.and_then(|h| h.describe_slot(index)) {
                    let _ = write!(text, " [heap {index}: {slot}]");
                }
            }
            for &(slot, _) in stage.root_cbvs.iter().chain(&stage.root_srvs).chain(&stage.root_uavs) {
                if let Some(Root::View(address)) = self.roots_ref(compute).get(usize::from(slot)) {
                    let host = crate::shadow::find(*address, 1)
                        .map(|s| (s.gpu(*address), s.left(*address)));
                    let _ = write!(text, " root{slot}={address:#x} host {host:#x?}");
                }
            }
        }
        let _ = write!(text, " tables {:?}", self.reads(compute));
        if !compute && !list_slots().is_empty() {
            text.push_str(&self.slots_text());
        }
        text
    }

    /// For the frame listing, after a draw whose shader `VICTORY_LIST_SLOTS`
    /// names: the heap slots it will pick textures from by index, and what
    /// the title and the host hold there (one `~slot` line each), with the
    /// words they were read from (`~raw` lines, to decode offline what is
    /// not decoded here).
    ///
    /// Where the slots are, for the title's character shaders
    /// (read from their native code): in the small
    /// buffers and constants of the draw's own table (every word of those
    /// that names a texture view gets a line); in the material record, at
    /// the byte offset the draw's constants (table 1, dword 56) hold at
    /// 0x1c, of the buffer at table 0, dword 160; and in the record at the
    /// byte offset the second root constant holds of the buffer at table 0,
    /// dword 688. A word that is a small number by chance and names a
    /// texture gets a line too.
    #[cfg(feature = "diagnostics")]
    fn slots_text(&self) -> String {
        use std::fmt::Write;
        const MATERIALS: u32 = 160;
        const GLOBAL: u32 = 688;
        const DRAW_CONSTANTS: u32 = 56;
        let wanted = list_slots();
        let Some(p) = crate::pipeline::host_pipeline(self.pipeline as This) else {
            return String::new();
        };
        let Some(stage) = p.stages.iter().find(|s| wanted.iter().any(|w| s.shader.hash.starts_with(w.as_str())))
        else {
            return String::new();
        };
        let Some(heap) = self
            .heaps
            .iter()
            .filter(|&&h| h != 0)
            .map(|&h| crate::descriptor::heap(h as This))
            .find(|h| !h.samplers())
        else {
            return String::new();
        };
        let t = stage.translated();
        let mut text = String::new();
        let entry = |slot: u8, dwords: u32| -> Option<View> {
            let Some(Root::Table(table)) = self.roots_ref(false).get(usize::from(slot)) else {
                return None;
            };
            crate::descriptor::view(table + u64::from(dwords) * 4)
        };
        let mut material = None;
        for u in t.used.iter().filter(|u| u.root == RootType::Table && u.kind != ResourceType::Sampler) {
            let label = format!("s{}+{}", u.slot, u.table_offset_dwords);
            let Some(view) = entry(u.slot, u.table_offset_dwords) else {
                let _ = write!(text, "\n    ~table {label} no view");
                continue;
            };
            let range = match &view {
                View::Constant(address, size) => Some((*address, u64::from(*size))),
                other => crate::descriptor::host::buffer_range(other),
            };
            match range {
                // A small buffer or constants: its words.
                Some((address, len)) if len <= 2048 => {
                    Self::slot_words(&mut text, heap, &label, address, len as usize);
                    if u.slot == 1 && u.table_offset_dwords == DRAW_CONSTANTS && len >= 0x20 {
                        let mut word = [0u8; 4];
                        if crate::host::read_into((address + 0x1c) as *const u8, &mut word) {
                            material = Some(u32::from_le_bytes(word));
                        }
                    }
                }
                Some(_) => {}
                // A texture read through the table: the title slot it sits in.
                None => {
                    let Some(Root::Table(table)) = self.roots_ref(false).get(usize::from(u.slot)) else {
                        continue;
                    };
                    match heap.index(table + u64::from(u.table_offset_dwords) * 4) {
                        Some(index) => text.push_str(&Self::slot_line(heap, &format!("table:{label}"), 0, index)),
                        None => {
                            let _ = write!(text, "\n    ~table {label} title=[{}]", view_state(Some(&view)));
                        }
                    }
                }
            }
        }
        let uses = |slot: u8, dwords: u32| {
            t.used.iter().any(|u| u.root == RootType::Table && u.slot == slot && u.table_offset_dwords == dwords)
        };
        if let Some(at) = material
            && uses(0, MATERIALS)
            && let Some((base, len)) = entry(0, MATERIALS).as_ref().and_then(crate::descriptor::host::buffer_range)
        {
            let at = u64::from(at);
            let _ = write!(text, "\n    ~material record at {at:#x} of the buffer at {base:#x} bytes {len}");
            if at + 64 <= len {
                Self::slot_words(&mut text, heap, "M", base + at, 64);
            }
            if at + 1936 + 512 <= len {
                Self::slot_words(&mut text, heap, "M+1936", base + at + 1936, 512);
            }
        }
        if uses(0, GLOBAL)
            && let Some(slot) = t.used.iter().find(|u| u.root == RootType::Constants).map(|u| u.slot)
            && let Some(Root::Constants(words)) = self.roots_ref(false).get(usize::from(slot))
            && let Some(&at) = words.get(1)
            && let Some((base, len)) = entry(0, GLOBAL).as_ref().and_then(crate::descriptor::host::buffer_range)
        {
            let at = u64::from(at);
            let _ = write!(text, "\n    ~root constants {words:08x?}: record at {at:#x} of the buffer at {base:#x} bytes {len}");
            if at + 64 <= len {
                Self::slot_words(&mut text, heap, "G", base + at, 64);
            }
        }
        text
    }

    /// Up to 512 bytes of title memory at `address` as hex words (a `~raw`
    /// line), and a `~slot` line for every word of them that names a
    /// texture view in `heap`.
    #[cfg(feature = "diagnostics")]
    fn slot_words(text: &mut String, heap: &crate::descriptor::Heap, label: &str, address: u64, len: usize) {
        use std::fmt::Write;
        let mut bytes = vec![0u8; len.min(512) & !3];
        if bytes.is_empty() || !crate::host::read_into(address as *const u8, &mut bytes) {
            let _ = write!(text, "\n    ~raw {label} at {address:#x} bytes {len}: unreadable");
            return;
        }
        let words: Vec<u32> =
            bytes.chunks_exact(4).map(|w| u32::from_le_bytes([w[0], w[1], w[2], w[3]])).collect();
        let _ = write!(text, "\n    ~raw {label} at {address:#x} bytes {len}:");
        for w in &words {
            let _ = write!(text, " {w:08x}");
        }
        for (i, &index) in words.iter().enumerate() {
            let names = index != 0
                && heap
                    .handle(index)
                    .and_then(crate::descriptor::view)
                    .as_ref()
                    .is_some_and(crate::descriptor::host::is_texture_view);
            if names {
                text.push_str(&Self::slot_line(heap, label, i * 4, index));
            }
        }
    }

    /// One `~slot` line: where the slot number was read (`from`, byte
    /// `at`), what the title's heap holds in slot `index` with the state
    /// of that texture ([`view_state`]), what the host slot was made from,
    /// and what the queue of textures picked by index knows of it.
    #[cfg(feature = "diagnostics")]
    fn slot_line(heap: &crate::descriptor::Heap, from: &str, at: usize, index: u32) -> String {
        let title = heap.handle(index).and_then(crate::descriptor::view);
        let host: String = heap
            .host()
            .and_then(|h| h.describe_slot(index))
            .unwrap_or_else(|| "none".into())
            .chars()
            .take(90)
            .collect();
        let queue = match heap.host() {
            Some(h) => h.texture_slot_state(index, crate::frame::current()),
            None => "no host heap".into(),
        };
        format!(
            "\n    ~slot from={from}+{at:#x} slot={index} title=[{}] host=[{host}] queue=[{queue}]",
            view_state(title.as_ref())
        )
    }

    /// A mesh draw under `VICTORY_MESH_TEST=scissor` rasterises one pixel:
    /// the scissors before it (`on`), the title's after it.
    fn mesh_scissor(&self, list: &HostList, on: bool) {
        if mesh_test() != 2 {
            return;
        }
        if on {
            let one = RECT { left: 0, top: 0, right: 1, bottom: 1 };
            crate::draw::scissors(list, &vec![one; self.scissors.len().max(1)]);
        } else if !self.scissors.is_empty() {
            crate::draw::scissors(list, &self.scissors);
        }
    }

    /// Runs a mesh dispatch of `x` groups on the host (the title's mesh
    /// programs are one-dimensional).
    pub(crate) fn dispatch_mesh(&mut self, list: &HostList, x: u32) {
        self.draw_ends_window();
        #[cfg(feature = "diagnostics")]
        self.trace("mesh", x, 0);
        let bounded = crate::indirect::direct_bounded(2, x, Some(crate::indirect::mesh_bounded(x)));
        let x = bounded.unwrap_or(0);
        let ran = bounded.is_some()
            && mesh_test() != 1
            && mesh_kept(self.pipeline)
            && self.prepare_for(list, false, true)
            && match list.list.cast::<ID3D12GraphicsCommandList6>() {
                Ok(l6) => {
                    #[cfg(feature = "diagnostics")]
                    list.note_op(|| self.op_text(format!("mesh {x}"), false));
                    self.mesh_scissor(list, true);
                    // SAFETY: an open list with everything set.
                    unsafe { l6.DispatchMesh(x, 1, 1) };
                    self.mesh_scissor(list, false);
                    DRAWN.fetch_add(1, Ordering::Relaxed);
                    true
                }
                Err(_) => false,
            };
        if !ran {
            DROPPED.fetch_add(1, Ordering::Relaxed);
            #[cfg(feature = "diagnostics")]
            dropped(self.pipeline);
        }
    }

    /// The walls trace (`crate::host::walls_trace`): one log line per
    /// frame with the draws of its main pass (four or more colour targets)
    /// by scissor rectangle, and how often each of the title's culling
    /// programs was dispatched. The title clips what it draws
    /// through a window or door to that opening's rectangle while it holds
    /// the camera to be in a room (run 182, frame 1529: 1,386 draws under
    /// `531 0 1789 1337`); in a launch without the room's walls the same
    /// shot has no such rectangle (run 187, frame 1414). The line shows
    /// the frame in which that changes.
    fn walls_census(&self, p: &HostPipeline, compute: bool) {
        const CULL: [(&str, &str); 9] = [
            ("55679a0547649d0b", "tree"),
            ("89d08e561897d4e5", "tree_imposter"),
            ("9209777e8face9dd", "grass"),
            ("17af15c4015cb4b6", "prop"),
            ("fdd56d97a806bcca", "prop_nocull"),
            ("42f717f1b4d18e94", "simple"),
            ("1c4b3c0547e3e6fa", "slod"),
            ("9146291b38ea9ce1", "meshlet_args"),
            ("42683eebec081520", "depth_pyramid_cpu_occlusion"),
        ];
        type Census = (u64, std::collections::BTreeMap<[i32; 4], u32>, [u32; 9]);
        static CENSUS: std::sync::Mutex<Census> =
            std::sync::Mutex::new((u64::MAX, std::collections::BTreeMap::new(), [0; 9]));
        let frame = crate::frame::current();
        let mut census = CENSUS.lock().unwrap_or_else(|e| e.into_inner());
        if census.0 != frame {
            let draws: u32 = census.1.values().sum();
            if draws > 0 {
                let mut rects: Vec<([i32; 4], u32)> = census.1.iter().map(|(r, n)| (*r, *n)).collect();
                rects.sort_unstable_by_key(|r| std::cmp::Reverse(r.1));
                let scissors: Vec<String> = rects
                    .iter()
                    .take(5)
                    .map(|(r, n)| format!("{} {} {} {} x{n}", r[0], r[1], r[2], r[3]))
                    .collect();
                let counts = census.2;
                let culling: Vec<String> = CULL
                    .iter()
                    .zip(counts)
                    .filter(|(_, n)| *n > 0)
                    .map(|(c, n)| format!("{}={n}", c.1))
                    .collect();
                tracing::info!(
                    frame = census.0,
                    main_pass_draws = draws,
                    rectangles = rects.len(),
                    scissors = ?scissors,
                    culling = ?culling,
                    "walls trace: the frame's main pass by scissor rectangle, and its culling dispatches"
                );
            }
            census.0 = frame;
            census.1.clear();
            census.2 = [0; 9];
        }
        if compute {
            for (i, c) in CULL.iter().enumerate() {
                if p.stages.iter().any(|s| s.shader.hash.as_str() == c.0) {
                    census.2[i] += 1;
                }
            }
        } else if self.targets.iter().filter(|&&t| t != 0).count() >= 4 {
            let r = self.scissors.first().map_or([0; 4], |r| [r.left, r.top, r.right, r.bottom]);
            *census.1.entry(r).or_insert(0) += 1;
        }
    }

    fn roots_ref(&self, compute: bool) -> &Vec<Root> {
        if compute {
            &self.compute
        } else {
            &self.graphics
        }
    }

    fn table_entry(&self, list: &HostList, u: &UsedResource, compute: bool) {
        let Some(Root::Table(table)) = self.roots_ref(compute).get(usize::from(u.slot)) else {
            return;
        };
        let handle = table + u64::from(u.table_offset_dwords) * 4;
        let Some(heap) = self.heap_of(handle) else {
            return;
        };
        let (Some(host), Some(index)) = (heap.host(), heap.index(handle)) else {
            return;
        };
        // Step 3 reads constant buffer views in place through the title's
        // slots, so tables it no longer makes afresh come here, every entry,
        // every draw (run 424m1: 15 -> 45 ms a frame, all threads): with it
        // on, entries are remembered as with `VICTORY_TABLE_MEMO=1`.
        let memo = match table_memo() {
            0 if crate::shader_cache::cbv_row_shift() => 1,
            memo => memo,
        };
        let key = (memo != 0).then(|| {
            // SAFETY: a descriptor slot of the title's heap, as `mirror` reads it.
            let bytes = unsafe { (handle as *const [u64; 4]).read_unaligned() };
            (handle, bytes, u.kind, STORES.get(), TESTED_DEPTH.get().0)
        });
        let known = key.and_then(|key| self.entry_memo.borrow().get(&key).copied());
        let hit = known.is_some_and(|(changes, copy)| {
            changes == list.changes()
                && crate::descriptor::view(handle).is_some_and(|view| memo_copy(&view) == Some(copy))
        });
        memo_count(hit);
        if hit && memo == 1 {
            return;
        }
        let before = list.changes();
        let view = host.mirror(index, handle, true);
        if let Some(view) = &view {
            readable(list, u.kind, view);
            // Up to date by this draw: not the texture pass's to make.
            if crate::descriptor::host::is_texture_view(view) {
                host.texture_slot_tabled(index, true);
            }
        }
        if hit && list.changes() != before {
            // Verify: the entry skipped would have changed the list.
            memo_wrong(handle, u.kind);
        }
        if let Some(key) = key {
            let mut memo = self.entry_memo.borrow_mut();
            // Not the depth target the draw tests: reading it marks
            // `TESTED_DEPTH`, which a skip would not.
            let tested = TESTED_DEPTH.get().0;
            let remember = |view: &&View| {
                u.kind != ResourceType::Uav && !matches!(view, View::Shader(r, _) if tested != 0 && *r as usize == tested)
            };
            match view.as_ref().filter(remember).and_then(memo_copy) {
                Some(copy) => {
                    if memo.len() >= 1 << 16 {
                        memo.clear();
                    }
                    memo.insert(key, (list.changes(), copy));
                }
                None => {
                    memo.remove(&key);
                }
            }
        }
    }

    /// The descriptors a shader reads through tables. Tables holding a
    /// constant buffer are made afresh for the draw in dynamic slots, with
    /// the constant data copied to upload memory when the list is
    /// submitted: the host wants constant buffers 256-byte aligned, the
    /// console only 16. Returns those tables' new byte offsets by root
    /// parameter; the other tables' slots are made from the title's.
    ///
    /// Within a list, the same table holding the same descriptors is made
    /// once: its constant data is copied at submission either way, and the
    /// big tables the world binds would otherwise take hundreds of slots per
    /// draw, wrapping the dynamic slots onto lists not yet submitted.
    ///
    /// `half`: the table descriptors the shader moves sixteen bits a
    /// channel as unsigned integers (`HostShader::half_views`). A table
    /// with a 16-bit float or signed buffer view at one of them is made
    /// afresh as well, with that view in the unsigned format of the same
    /// layout (`descriptor::host::half_view`): the title's own slot keeps
    /// the title's format for the shaders that read it as floats.
    pub(crate) fn tables(
        &mut self,
        list: &HostList,
        used: &[UsedResource],
        indexed: &[RootIndexedView],
        half: &[(u8, u32)],
        compute: bool,
    ) -> Vec<(u8, u32)> {
        let mut laps = Laps::start();
        let tables = |u: &&UsedResource| u.root == RootType::Table;
        let mut fresh: Vec<u8> = used
            .iter()
            .filter(tables)
            .filter(|u| u.kind == ResourceType::Cbv && !self.cbv_in_place_memo(list, u, compute))
            .map(|u| u.slot)
            .collect();
        // The view shown in place of the title's at dword `at` of the table
        // in root parameter `slot`, if the shader moves halves there and
        // the title's view is a 16-bit float or signed buffer view.
        let halved = |state: &Self, slot: u8, at: u32| -> Option<View> {
            if !half.contains(&(slot, at)) {
                return None;
            }
            let Some(&Root::Table(table)) = state.roots_ref(compute).get(usize::from(slot)) else {
                return None;
            };
            crate::descriptor::host::half_view(&crate::descriptor::view(table + u64::from(at) * 4)?)
        };
        for &(slot, at) in half {
            let Some(&Root::Table(table)) = self.roots_ref(compute).get(usize::from(slot)) else {
                continue;
            };
            let Some(view) = crate::descriptor::view(table + u64::from(at) * 4) else {
                continue;
            };
            if crate::descriptor::host::half_view(&view).is_some() {
                if !fresh.contains(&slot) {
                    fresh.push(slot);
                }
            } else if let Some(format) = crate::descriptor::host::half_inexact(&view) {
                once!(
                    format,
                    "a shader moves a buffer's 16-bit channels as integers through a view whose values the console converts: they are read and written unconverted"
                );
            }
        }
        // Tables reaching past a host heap smaller than the title's (the
        // host's limit, 1,000,000 views on NVIDIA) are made afresh too: the
        // title's slots there have no host slot of their own.
        for u in used.iter().filter(tables) {
            if fresh.contains(&u.slot) {
                continue;
            }
            let Some(&Root::Table(table)) = self.roots_ref(compute).get(usize::from(u.slot)) else {
                continue;
            };
            let end = table + u64::from(u.table_offset_dwords) * 4;
            let Some(heap) = self.heap_of(table) else {
                continue;
            };
            let (Some(host), Some(index)) = (heap.host(), heap.index(end)) else {
                continue;
            };
            if index >= host.count() {
                fresh.push(u.slot);
            }
        }
        fresh.sort_unstable();
        fresh.dedup();
        laps.lap(8);
        for u in used.iter().filter(tables) {
            if !fresh.contains(&u.slot) {
                self.table_entry(list, u, compute);
            }
        }
        laps.lap(9);
        let mut made = Vec::new();
        for slot in fresh {
            let Some(&Root::Table(table)) = self.roots_ref(compute).get(usize::from(slot)) else {
                continue;
            };
            let entries: Vec<&UsedResource> = used
                .iter()
                .filter(tables)
                .filter(|u| u.slot == slot)
                .collect();
            let Some(heap) = self.heap_of(table) else {
                continue;
            };
            if heap.samplers() || (table - heap.start) % 32 != 0 {
                continue;
            }
            let Some(host) = heap.host() else {
                continue;
            };
            let mut selected: Vec<u32> = indexed
                .iter()
                .filter(|index| index.table_slot == slot)
                .map(|index| {
                    let constant = self.user_value(
                        compute,
                        index.constant_slot,
                        RootType::Constants,
                        index.constant_dword,
                    );
                    // The shader adds i32 byte offsets and shifts by 5.
                    index.byte_offset.wrapping_add(constant << index.shift) / 32
                })
                .collect();
            // The descriptors the shader moves halves through are made too,
            // and the table reaches them: they may lie past the range starts
            // the entries give (a range of several views names only its
            // first; review on #927).
            selected.extend(
                half.iter()
                    .filter(|&&(s, _)| s == slot)
                    .map(|&(_, at)| at / 8),
            );
            selected.sort_unstable();
            selected.dedup();
            let n = entries
                .iter()
                .map(|u| u.table_offset_dwords / 8)
                .chain(selected.last().copied())
                .max()
                .unwrap_or(0)
                + 1;
            let last = table.checked_add(u64::from(n - 1) * 32);
            if n > crate::descriptor::host::dynamic_slots()
                || last.and_then(|handle| heap.index(handle)).is_none()
            {
                once!(slot, n, "fresh descriptor table exceeds the available span");
                continue;
            }
            // The table and what its entries hold (record words).
            let key = {
                use std::hash::{Hash, Hasher};
                let mut h = std::collections::hash_map::DefaultHasher::new();
                for k in 0..n {
                    let at = table + u64::from(k) * 32;
                    // SAFETY: a descriptor slot in title memory.
                    unsafe { (at as *const [u32; 3]).read_unaligned() }.hash(&mut h);
                    // (Another table than the same slots made for a shader
                    // that reads them in the title's formats.)
                    half.contains(&(slot, k * 8)).hash(&mut h);
                }
                (table, n, h.finish())
            };
            laps.lap(10);
            if let Some(&first) = self.fresh_tables.get(&key) {
                for u in entries.iter().filter(|u| u.kind != ResourceType::Cbv) {
                    let handle = table + u64::from(u.table_offset_dwords) * 4;
                    if let Some(view) = crate::descriptor::view(handle) {
                        readable(list, u.kind, &view);
                    }
                }
                for k in selected {
                    if entries.iter().any(|u| u.table_offset_dwords / 8 == k) {
                        continue;
                    }
                    let handle = table + u64::from(k) * 32;
                    if let Some(view) = crate::descriptor::view(handle)
                        && let Some(kind) = view_kind(&view)
                    {
                        readable(list, kind, &view);
                    }
                }
                made.push((slot, first * 32));
                laps.lap(11);
                continue;
            }
            let taken = host.dynamic(n);
            if slot_reasons() {
                // Why the table is made afresh, as `fresh` was built: a
                // constant buffer view, else a 16-bit view, else past the
                // host heap.
                let why = if entries
                    .iter()
                    .any(|u| u.kind == ResourceType::Cbv && !self.cbv_in_place(u, compute))
                {
                    0
                } else if half.iter().any(|&(s, _)| s == slot) {
                    1
                } else {
                    2
                };
                note_slot_reason(why, n, taken.is_some());
                if why == 0 {
                    let cbvs = entries.iter().filter(|u| u.kind == ResourceType::Cbv).filter_map(|u| {
                        crate::descriptor::view(table + u64::from(u.table_offset_dwords) * 4)
                    });
                    note_cbv_reason(cbvs.map(|v| crate::descriptor::host::step3_reason(&v)).max().unwrap_or(0), n);
                }
            }
            let Some(first) = taken else {
                // No room: the draw must not run with the title's own
                // offsets (slots never made for it); `bind` drops it.
                made.push((slot, u32::MAX));
                continue;
            };
            self.fresh_tables.insert(key, first);
            self.dynamic
                .push((std::ptr::from_ref(heap) as usize, first, n));
            // The title's own slots of this table: what the draw reads of
            // them it brings up to date here, in the dynamic slots.
            let title_first = heap.index(table).filter(|first| first + n <= host.count());
            // The whole range: a shader may index past the entries its
            // used-resource list names (descriptor arrays).
            for k in 0..n {
                let index = first + k;
                let used = entries.iter().find(|u| u.table_offset_dwords / 8 == k);
                match crate::descriptor::view(table + u64::from(k) * 32) {
                    Some(View::Constant(address, size)) => {
                        let Some(gpu) = list.fill_scratch(address as *const u8, u64::from(size)) else {
                            host.make_null(index);
                            continue;
                        };
                        host.constant_at(index, gpu, size);
                    }
                    Some(view) => {
                        match halved(self, slot, k * 8) {
                            Some(shown) => host.make_at(index, &shown),
                            None => host.make_at(index, &view),
                        }
                        let kind = used.map(|u| u.kind).or_else(|| {
                            selected.binary_search(&k).ok().and_then(|_| view_kind(&view))
                        });
                        if let Some(kind) = kind {
                            readable(list, kind, &view);
                        }
                        // The title's slot is a table's: out of the queue
                        // of textures picked by index if the draw read it
                        // (its texture is up to date now), and made in
                        // place as the texture pass would have made it,
                        // for a shader that picks it by index all the same.
                        if let Some(title) = title_first
                            && crate::descriptor::host::is_texture_view(&view)
                        {
                            host.texture_slot_tabled(title + k, kind.is_some());
                            if kind.is_some() {
                                host.mirror(title + k, table + u64::from(k) * 32, true);
                            }
                        }
                    }
                    None => {
                        host.make_null(index);
                        if used.is_some() {
                            once!(slot, "a table entry without a view");
                        }
                    }
                }
            }
            made.push((slot, first * 32));
            laps.lap(12);
        }
        made
    }

    /// For the census of what repeats from frame to frame
    /// ([`crate::repeat`]): the draw or dispatch of `kind` with `args`
    /// about to run, told by its shape (pipeline, topology, targets) and its
    /// parts with the shape (tables, constants, root views, buffers,
    /// arguments).
    #[cfg(feature = "diagnostics")]
    fn census(&mut self, compute: bool, kind: u64, args: &[u64]) {
        use crate::repeat::mix;
        if !crate::repeat::on() {
            return;
        }
        let mut shape = mix(mix(mix(kind, self.pipeline as u64), u64::from(self.topology)), self.depth as u64);
        for &target in &self.targets {
            shape = mix(shape, target as u64);
        }
        let (mut tables, mut constants, mut views) = (shape, shape, shape);
        let roots = if compute { &self.compute } else { &self.graphics };
        for (slot, root) in roots.iter().enumerate() {
            let slot = slot as u64;
            match root {
                Root::None => {}
                Root::Table(at) => tables = mix(mix(tables, slot), *at),
                Root::Constants(values) => {
                    constants = mix(constants, slot);
                    for &v in values {
                        constants = mix(constants, u64::from(v));
                    }
                }
                Root::View(at) => views = mix(mix(views, slot), *at),
            }
        }
        let mut buffers = shape;
        if !compute {
            buffers = mix(mix(buffers, self.index.address), u64::from(self.index.size));
            for v in self.vertex.iter().filter(|v| v.size != 0) {
                buffers = mix(mix(buffers, v.address), u64::from(v.size));
            }
        }
        let arguments = args.iter().fold(shape, |h, &a| mix(h, a));
        crate::repeat::draw([shape, tables, constants, views, buffers, arguments]);
        // Indirect calls right after one with the same state in the list:
        // what one call of several records could take.
        let state = [tables, constants, views, buffers].into_iter().fold(shape, mix);
        if kind == 5 {
            crate::repeat::indirect(state == self.census_last);
        }
        self.census_last = state;
    }

    /// The first draws, at debug level: what they bind and read
    /// (diagnostics builds).
    #[cfg(feature = "diagnostics")]
    fn trace(&self, kind: &str, count: u32, first: u32) {
        static TRACED: AtomicU64 = AtomicU64::new(0);
        if !tracing::enabled!(tracing::Level::DEBUG) {
            return;
        }
        let listed = crate::capture::listed(crate::queue::presented() + 1);
        let logged = listed || TRACED.fetch_add(1, Ordering::Relaxed) < 64;
        let targets: Vec<String> = self
            .targets
            .iter()
            .filter_map(|&t| crate::resource::host_made(t as This))
            .map(|t| {
                // SAFETY: a live resource.
                let d = unsafe { t.GetDesc() };
                format!("{}x{} f{}", d.Width, d.Height, d.Format.0)
            })
            .collect();
        let v = self.vertex[0];
        let line = format!(
            "{kind} {count} from {first} topology {} pipeline {:#x} targets {targets:?} vb {:#x}+{} stride {} reads {:?}",
            self.topology,
            self.pipeline,
            v.address,
            v.size,
            v.stride,
            self.reads(false)
        );
        if logged {
            tracing::debug!("draw {line}");
        }
        let mut frame = FRAME.lock().unwrap_or_else(|e| e.into_inner());
        if frame.len() < 256 {
            frame.push(line);
        }
    }

    /// What a draw's shaders read, one line per used resource (diagnostics
    /// builds).
    #[cfg(feature = "diagnostics")]
    fn reads(&self, compute: bool) -> Vec<String> {
        let Some(p) = crate::pipeline::host_pipeline(self.pipeline as This) else {
            return vec!["no host pipeline".into()];
        };
        let mut out = Vec::new();
        for stage in &p.stages {
            let t = stage.translated();
            for u in &t.used {
                let what = match (u.root, self.roots_ref(compute).get(usize::from(u.slot))) {
                    (RootType::Table, Some(Root::Table(table))) => {
                        let handle = table + u64::from(u.table_offset_dwords) * 4;
                        describe(crate::descriptor::view(handle).as_ref())
                    }
                    (root, _) => format!("{root:?}"),
                };
                out.push(format!(
                    "{} {:?} s{}+{}: {what}",
                    stage.shader.hash, u.kind, u.slot, u.table_offset_dwords
                ));
            }
        }
        out
    }

    pub(crate) fn draw(
        &mut self,
        list: &HostList,
        vertices: u32,
        instances: u32,
        first: u32,
        first_instance: u32,
    ) {
        self.draw_ends_window();
        #[cfg(feature = "diagnostics")]
        self.trace("draw", vertices, first);
        let bounded = crate::indirect::direct_bounded(0, instances, crate::indirect::draw_bounded(vertices, instances));
        let instances = bounded.unwrap_or(0);
        let ready = bounded.is_some() && self.prepare(list, false);
        // A pipeline with the rectangle geometry stage draws the title's
        // vertices as they are; any other gets its rectangles expanded.
        let rects = self.topology == RECTANGLES && !self.completes_rects();
        let ran = ready && (!rects || self.rectangles(list, vertices, first));
        if ran {
            // Rectangles were expanded into their own vertex buffers.
            let (count, from) = if rects {
                (vertices / 3 * 6, 0)
            } else {
                (vertices, first)
            };
            #[cfg(feature = "diagnostics")]
            list.note_op(|| self.op_text(format!("draw {count}x{instances}"), false));
            // SAFETY: an open list with everything set.
            unsafe {
                list.list
                    .DrawInstanced(count, instances, from, first_instance)
            };
            DRAWN.fetch_add(1, Ordering::Relaxed);
        }
        if !ran {
            DROPPED.fetch_add(1, Ordering::Relaxed);
            #[cfg(feature = "diagnostics")]
            dropped(self.pipeline);
        }
    }

    /// Whether the bound pipeline completes rectangles itself, in the
    /// geometry stage rectangle pipelines get (`HostPipeline::rects`).
    fn completes_rects(&self) -> bool {
        crate::pipeline::host_pipeline(self.pipeline as This).is_some_and(|p| p.rects)
    }

    /// Binds vertex buffers holding the rectangles of vertices `first..first
    /// + count` as triangles, made when the list is submitted.
    fn rectangles(&self, list: &HostList, count: u32, first: u32) -> bool {
        let Some(p) = crate::pipeline::host_pipeline(self.pipeline as This) else {
            return false;
        };
        let rects = u64::from(count / 3);
        for slot in 0..32 {
            let elements: Vec<(u32, u32)> = p
                .inputs
                .iter()
                .filter(|e| e.slot == slot && !e.per_instance)
                .map(|e| (e.offset, e.format))
                .collect();
            if elements.is_empty() {
                continue;
            }
            let v = self.vertex[slot as usize];
            let stride = u64::from(v.stride);
            let from = v.address + u64::from(first) * stride;
            if v.address == 0
                || stride == 0
                || u64::from(first + count) * stride > u64::from(v.size)
                || crate::shadow::find(from, rects * 3 * stride).is_none()
            {
                once!(slot, "rectangle vertices outside their buffer");
                return false;
            }
            let Some((gpu, to)) = list.scratch(rects * 6 * stride) else {
                return false;
            };
            let (from, to, stride) = (from as usize, to as usize, stride as usize);
            let n = rects as usize;
            list.defer_host(move || {
                // The vertices are read in place, at submission: no change
                // of mapping under the read (see
                // `crate::watch::title_memory_reading`).
                let _mapped = crate::watch::title_memory_reading();
                // SAFETY: the vertices lie in a title buffer (checked when
                // recorded) and the scratch memory is the list's.
                let (src, dst) = unsafe {
                    (
                        std::slice::from_raw_parts(from as *const u8, n * 3 * stride),
                        std::slice::from_raw_parts_mut(to as *mut u8, n * 6 * stride),
                    )
                };
                crate::primitive::expand_rects(src, dst, stride, &elements);
            });
            let view = D3D12_VERTEX_BUFFER_VIEW {
                BufferLocation: gpu,
                SizeInBytes: (rects * 6 * stride as u64) as u32,
                StrideInBytes: v.stride,
            };
            // SAFETY: an open list; the view names live scratch memory.
            unsafe { list.list.IASetVertexBuffers(slot, Some(&[view])) };
        }
        true
    }

    pub(crate) fn draw_indexed(
        &mut self,
        list: &HostList,
        indices: u32,
        instances: u32,
        first: u32,
        base: i32,
        first_instance: u32,
    ) {
        self.draw_ends_window();
        #[cfg(feature = "diagnostics")]
        self.trace("indexed", indices, first);
        // Indexed rectangles are drawn by the rectangle geometry stage
        // alone (nothing expands them on the CPU).
        let lost = self.topology == RECTANGLES && !self.completes_rects();
        if lost {
            once!("indexed rectangles not drawn");
        }
        let bounded = crate::indirect::direct_bounded(0, instances, crate::indirect::draw_bounded(indices, instances));
        let instances = bounded.unwrap_or(0);
        let ran = !lost && bounded.is_some() && self.prepare(list, true);
        if ran {
            #[cfg(feature = "diagnostics")]
            list.note_op(|| self.op_text(format!("indexed {indices}x{instances}"), false));
            // SAFETY: an open list with everything set.
            unsafe {
                list.list
                    .DrawIndexedInstanced(indices, instances, first, base, first_instance)
            };
            DRAWN.fetch_add(1, Ordering::Relaxed);
        }
        if !ran {
            DROPPED.fetch_add(1, Ordering::Relaxed);
            #[cfg(feature = "diagnostics")]
            dropped(self.pipeline);
        }
    }
}

impl State {
    /// A direct draw ends the window ([`windows`]). With
    /// `VICTORY_INDIRECT_WINDOW_DRAWS=1` (off by default) only a draw whose
    /// shaders can store to buffers does: such a draw may write the
    /// arguments the window holds clamped, and no other draw touches them
    /// or the list's scratch buffer.
    fn draw_ends_window(&mut self) {
        static OVER: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        if self.window.is_none() {
            return;
        }
        let over = *OVER.get_or_init(|| std::env::var_os("VICTORY_INDIRECT_WINDOW_DRAWS").is_some_and(|v| v == "1"));
        let stores = || {
            crate::pipeline::host_pipeline(self.pipeline as This)
                .is_none_or(|p| p.stages.iter().any(|stage| stage.translated().stores))
        };
        if !over || stores() {
            self.window = None;
        }
    }

    /// Indirect execution of a one-argument signature of `kind` (PC
    /// numbering: 0 draw, 1 indexed draw, 2 dispatch, 10 mesh dispatch) with
    /// records `stride` bytes apart at title address `arguments`, and the
    /// record count at `count` if the title gives one: the GPU reads them
    /// from the buffer's host copy, where earlier dispatches may have
    /// written them. False if it cannot run on the host.
    pub(crate) fn execute_indirect(
        &mut self,
        list: &HostList,
        (kind, stride, max): (u32, u32, u32),
        arguments: u64,
        count: Option<u64>,
    ) -> bool {
        #[cfg(feature = "diagnostics")]
        self.census(kind == 2, 5, &[u64::from(kind), u64::from(stride), u64::from(max), arguments, count.unwrap_or(0)]);
        // Skip before the argument clamp can mutate the indirect buffer.
        #[cfg(windows)]
        if crate::pipeline::is_skipped(self.pipeline as This) {
            #[cfg(feature = "diagnostics")]
            crate::pipeline::note_drop(self.pipeline as This);
            return false;
        }
        if crate::pipeline::is_builder(self.pipeline as This) && !crate::pipeline::builder_runs() {
            return false;
        }
        // A program that walks ray-tracing structures nothing has built:
        // dropped before the argument clamp, like a skipped one.
        if crate::pipeline::walk_held(self.pipeline as This) {
            #[cfg(feature = "diagnostics")]
            crate::pipeline::note_why("a walk of ray-tracing structures that are not built (VICTORY_BVH_HOLD=1)".to_owned());
            return false;
        }
        const DRAW: u32 = 0;
        const INDEXED: u32 = 1;
        const DISPATCH: u32 = 2;
        const MESH: u32 = 10;
        #[cfg(feature = "diagnostics")]
        let why = |what: &str| crate::pipeline::note_why(format!("indirect kind {kind}: {what}"));
        #[cfg(not(feature = "diagnostics"))]
        let why = |_: &str| {};
        let Some(p) = crate::pipeline::host_pipeline(self.pipeline as This) else {
            #[cfg(feature = "diagnostics")]
            crate::pipeline::note_drop(self.pipeline as This);
            return false;
        };
        // Indirect rectangles (the title's tiled lighting, 256
        // indirect draws a frame, makes their corners from the vertex
        // number) are completed by the pipeline's geometry stage: the
        // arguments stay the title's, three vertices a rectangle. They
        // cannot be expanded on the CPU, which does not know their count.
        // shortcut: in a pipeline without that stage, a rectangle with no
        // per-vertex inputs is drawn as its first triangle (three vertices,
        // half of it), and one with vertex buffers is not drawn.
        let made_rects = !p.rects
            && kind == DRAW
            && self.topology == RECTANGLES
            && !p.inputs.iter().any(|e| !e.per_instance);
        if made_rects {
            once!("indirect rectangles without vertex inputs drawn as their first triangle");
        }
        let matches_kind = match kind {
            DRAW | INDEXED => {
                (self.topology != RECTANGLES || made_rects || p.rects) && !p.compute && !p.mesh
            }
            MESH => !p.compute && p.mesh,
            DISPATCH => p.compute,
            _ => false,
        };
        if !matches_kind {
            why(&format!(
                "pipeline mismatch (compute {}, mesh {}, topology {})",
                p.compute, p.mesh, self.topology
            ));
            return false;
        }
        let len = u64::from(stride) * u64::from(max.max(1));
        let mut laps = Laps::start();
        let Some(args) = crate::shadow::find_any(arguments, len) else {
            why("arguments outside title buffers");
            return false;
        };
        #[cfg(feature = "diagnostics")]
        crate::shadow::note_indirect_args(&args);
        let counter = match count {
            Some(at) => match crate::shadow::find_any(at, 4) {
                Some(s) => Some((s, at)),
                None => {
                    why("count outside title buffers");
                    return false;
                }
            },
            None => None,
        };
        let Some(signature) = matches!(kind, DRAW | INDEXED | DISPATCH | MESH)
            .then(|| host_signature(kind, stride))
            .flatten()
        else {
            why("no host signature");
            return false;
        };
        if kind == MESH && (mesh_test() == 1 || !mesh_kept(self.pipeline)) {
            why("mesh draws are switched off (VICTORY_MESH_TEST=skip)");
            return false;
        }
        // A window of records clamped in one pass (`windows`): for single
        // records without a count buffer, drawn by shaders that store to no
        // buffer. Anything else ends the window: it uses the scratch buffer
        // or may write the arguments.
        const WINDOW_USE: usize = usize::MAX - 0x77;
        let offset = args.offset(arguments);
        let clamped = crate::indirect::clamp_enabled();
        // A dispatch by a program that takes the number of its first group
        // may ask for more groups in x than the host runs in one dimension.
        // Its records are made into parts by the clamp's pass (with the
        // clamp's limits; without the clamp, with only the cut into parts),
        // and each part is run on its own, told where it begins: with the
        // clamp on or off, for as many records as the title gives.
        let parts = if kind == DISPATCH {
            group_base(p).and_then(|base| {
                host_signature(DISPATCH, crate::indirect::SPLIT_STRIDE).map(|signature| (base, signature))
            })
        } else {
            None
        };
        let (exec_buf, at) = if !clamped {
            let count_off = counter.as_ref().map(|(s, at)| s.offset(*at));
            if crate::indirect::validate_layout((kind, stride, max), offset, count_off).is_none() {
                why("direct indirect arguments not 4-byte aligned or invalid stride");
                return false;
            }
            args.refresh(list, arguments, len);
            if let Some((s, at)) = &counter {
                s.refresh(list, *at, 4);
            }
            if parts.is_some() {
                let count_arg = counter.as_ref().map(|(s, at)| (&s.buffer, s.offset(*at)));
                let Some(bounded) = crate::indirect::bound_split(
                    list,
                    (&args.buffer, offset),
                    count_arg,
                    (stride, max),
                    false,
                ) else {
                    why("not split into parts (no pass, or arguments not 4-byte aligned)");
                    return false;
                };
                // The pass bound a compute pipeline of its own.
                self.host_pipeline = 0;
                self.host_roots[1] = 0;
                (bounded, 0)
            } else {
                crate::indirect::wait_direct(list);
                (args.buffer.clone(), offset)
            }
        } else {
            let windowed = windows()
                && kind != DISPATCH
                && max == 1
                && count.is_none()
                && !p.stages.iter().any(|stage| stage.translated().stores);
            // Another host list (a new one after a cut, or this one after its
            // reset) has another scratch buffer, or this one's with old bytes.
            let first_on_list = list.first_use(WINDOW_USE);
            if !windowed || first_on_list {
                self.window = None;
            }
            // Diagnostics: why this command gets a pass of ours, if it does.
            #[cfg(feature = "diagnostics")]
            let why_not = if !windows() {
                usize::MAX
            } else if kind == DISPATCH {
                1
            } else if max != 1 {
                2
            } else if count.is_some() {
                3
            } else if !windowed {
                4
            } else if first_on_list {
                5
            } else {
                match &self.window {
                    None => 6,
                    Some(w) if !(w.copy == args.id() && w.kind == kind && w.stride == stride) => 6,
                    Some(w) if offset >= w.base && (offset - w.base) % u64::from(w.step) == 0 => 8,
                    Some(_) => 7,
                }
            };
            let same = |w: &Window| w.copy == args.id() && w.kind == kind && w.stride == stride;
            let hit = self
                .window
                .as_ref()
                .filter(|w| same(w) && offset >= w.base)
                .filter(|w| {
                    (offset - w.base) % u64::from(w.step) == 0
                        && (offset - w.base) / u64::from(w.step) < u64::from(w.records)
                })
                .map(|w| (w.scratch.clone(), offset - w.base, w.step));
            match hit {
                Some((scratch, at, step)) => {
                    // Diagnostics of windows: the record clamped again and
                    // compared (`crate::indirect::window_check`).
                    if crate::indirect::window_check()
                        && crate::indirect::check_hit(
                            list,
                            (&args.buffer, offset),
                            (kind, step),
                            &scratch,
                            at,
                        )
                    {
                        self.host_pipeline = 0;
                        self.host_roots[1] = 0;
                    }
                    #[cfg(feature = "diagnostics")]
                    WINDOW_WHY[0].fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    if !self.window_read {
                        self.window_read = true;
                        if let Some(w) = self.window.as_ref().filter(|w| w.records > 1) {
                            self.walk_step = Some((w.copy, w.kind, w.stride, w.step));
                        }
                    }
                    (scratch, at)
                }
                None => {
                    // A window opened at the remembered step that no draw read
                    // from: the title does not walk here by that step.
                    if self.window_jumped && !self.window_read {
                        self.walk_step = None;
                        #[cfg(feature = "diagnostics")]
                        WINDOW_WHY[11].fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    }
                    self.window_jumped = false;
                    // A record a short step after the last window's last one
                    // (the same step as that window's, if it had one): the
                    // title walks the buffer, so the ones that step apart
                    // after it are clamped with it.
                    let (step, records) = match &self.window {
                        Some(w) if windowed && same(w) => {
                            let last = w.base + u64::from(w.records - 1) * u64::from(w.step);
                            let step = offset.saturating_sub(last);
                            let walks = step >= u64::from(stride)
                                && step <= 256
                                && step % 4 == 0
                                && (w.records == 1 || step == u64::from(w.step));
                            if walks {
                                let fit = (args.left(arguments) / step).min(WINDOW_MOST) as u32;
                                (
                                    step as u32,
                                    (if w.records == 1 { 16 } else { w.records * 2 })
                                        .min(fit)
                                        .max(1),
                                )
                            } else {
                                (stride, max)
                            }
                        }
                        _ => (stride, max),
                    };
                    // A record that would be clamped alone, on a buffer whose
                    // step is remembered: the window the next record would
                    // open, opened here ([`jump_windows`]).
                    let (step, records) = match (windowed && records == 1 && jump_windows())
                        .then(|| jump_window(self.walk_step, (args.id(), kind, stride), args.left(arguments)))
                        .flatten()
                    {
                        Some(jumped) => {
                            self.window_jumped = true;
                            #[cfg(feature = "diagnostics")]
                            WINDOW_WHY[10].fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                            jumped
                        }
                        None => (step, records),
                    };
                    self.window_read = false;
                    args.refresh(
                        list,
                        arguments,
                        len.max(u64::from(step) * u64::from(records)),
                    );
                    if let Some((s, at)) = &counter {
                        s.refresh(list, *at, 4);
                    }
                    // Bounded first: the clamp binds a compute pipeline of its
                    // own and copies clamped active records into a scratch
                    // buffer at offset 0.
                    let count_arg = counter.as_ref().map(|(s, at)| (&s.buffer, s.offset(*at)));
                    // More than the record asked for: a window clamped ahead,
                    // whose unfilled slots must not spend the list's budget.
                    let bounded = if parts.is_some() {
                        crate::indirect::bound_split(
                            list,
                            (&args.buffer, offset),
                            count_arg,
                            (step, records),
                            true,
                        )
                    } else if windowed && records > 1 {
                        crate::indirect::bound_ahead(
                            list,
                            (&args.buffer, offset),
                            (kind, step, records),
                        )
                    } else {
                        crate::indirect::bound(
                            list,
                            (&args.buffer, offset),
                            count_arg,
                            (kind, step, records),
                        )
                    };
                    let Some(bounded) = bounded else {
                        self.window = None;
                        why("not bounded (no clamp, or arguments not 4-byte aligned)");
                        return false;
                    };
                    #[cfg(feature = "diagnostics")]
                    if why_not < WINDOW_WHY.len() {
                        WINDOW_WHY[why_not].fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        WINDOW_WHY[9].fetch_add(u64::from(records), std::sync::atomic::Ordering::Relaxed);
                        // One in 256 of the draws that miss their window, said:
                        // where the record lies against the window and against
                        // the record drawn before it.
                        if (6..=8).contains(&why_not) {
                            static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
                            if N.fetch_add(1, std::sync::atomic::Ordering::Relaxed).is_multiple_of(256) {
                                let (last_copy, last_offset) = self.last_indirect;
                                tracing::info!(
                                    "a window miss (one in 256): cause {why_not}, kind {kind}, stride {stride}, buffer {} offset {offset:#x} ({} left); the command before it on the list: buffer {last_copy} offset \
                                     {last_offset:#x} (this one is {} bytes on); the window it missed: {}; clamped now: {records} records at step {step}",
                                    args.id(),
                                    args.left(arguments),
                                    offset as i64 - last_offset as i64,
                                    match &self.window {
                                        Some(w) => format!("buffer {} kind {} stride {} base {:#x} step {} records {} (this record is {} bytes from its base)", w.copy, w.kind, w.stride, w.base, w.step, w.records, offset as i64 - w.base as i64),
                                        None => "none".to_string(),
                                    }
                                );
                            }
                        }
                    }
                    self.host_pipeline = 0;
                    self.host_roots[1] = 0;
                    self.window = windowed.then(|| Window {
                        copy: args.id(),
                        kind,
                        stride,
                        step,
                        base: offset,
                        records,
                        scratch: bounded.clone(),
                    });
                    (bounded, 0)
                }
            }
        };
        #[cfg(feature = "diagnostics")]
        {
            self.last_indirect = (args.id(), offset);
        }
        let ran = match kind {
            DRAW | INDEXED => self.prepare(list, kind == INDEXED),
            MESH => self.prepare_for(list, false, true),
            DISPATCH => self.bind(list, p, true),
            _ => false,
        };
        if !ran {
            why("prepare failed");
            return false;
        }
        #[cfg(feature = "diagnostics")]
        crate::copystat::indirect(counter.is_some(), max, fill);
        #[cfg(feature = "diagnostics")]
        list.note_op(|| {
            self.op_text(format!("indirect kind {kind} max {max} at {arguments:#x}"), kind == DISPATCH)
        });
        if kind == MESH {
            self.mesh_scissor(list, true);
        }
        // SAFETY: an open list with everything set; the argument/scratch buffer
        // and host copies are kept alive by the list, and the signature is cached.
        unsafe {
            if let Some(((param, dword), split)) = &parts {
                // One run per part, over all the records: part `n` is the
                // `n`th record of each, and begins at group `n * PART`.
                IN_PARTS[1].fetch_add(1, Ordering::Relaxed);
                for part in 0..crate::indirect::SPLIT {
                    list.list.SetComputeRoot32BitConstant(*param, part * crate::indirect::PART, *dword);
                    list.list.ExecuteIndirect(
                        split,
                        max,
                        &exec_buf,
                        u64::from(part) * crate::indirect::PART_BYTES,
                        counter.as_ref().map(|(s, _)| &s.buffer),
                        counter.as_ref().map_or(0, |(s, at)| s.offset(*at)),
                    );
                }
                list.list.SetComputeRoot32BitConstant(*param, 0, *dword);
            } else {
                list.list.ExecuteIndirect(
                    &signature,
                    max,
                    &exec_buf,
                    at,
                    counter.as_ref().map(|(s, _)| &s.buffer),
                    counter.as_ref().map_or(0, |(s, at)| s.offset(*at)),
                )
            }
        };
        if kind == MESH {
            self.mesh_scissor(list, false);
        }
        // The scratch buffer stays readable as arguments: the next draw of
        // the window reads it with no barrier, and the next clamp moves it
        // back (`crate::indirect`).
        if kind == DISPATCH {
            list.count_write();
            DISPATCHED.fetch_add(1, Ordering::Relaxed);
        } else {
            DRAWN.fetch_add(1, Ordering::Relaxed);
        }
        true
    }
}

/// Where a compute pipeline's program takes the number of its first thread
/// group (`victory_shader::emit::USER_GROUP_BASE`): the root parameter of
/// its user data and the dword in it. `None` for any other pipeline, and for
/// all of them with `VICTORY_DISPATCH_PARTS=0` (no dispatch is then run in
/// parts: the host gets the counts as the title gave them, or clamped).
fn group_base(p: &crate::pipeline::host::HostPipeline) -> Option<(u32, u32)> {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    if !*ON.get_or_init(|| std::env::var_os("VICTORY_DISPATCH_PARTS").is_none_or(|v| v != "0")) {
        return None;
    }
    let stage = p.stages.first().filter(|_| p.compute)?;
    let (param, _) = stage.user_data?;
    let base = RootType::Other(victory_shader::emit::USER_GROUP_BASE);
    let word = stage.translated().host.user_data.iter().find(|u| u.root == base)?;
    Some((param, word.index))
}

/// The host command signature for one argument of `kind` with records
/// `stride` bytes apart (no root arguments, so no root signature), made
/// once.
fn host_signature(kind: u32, stride: u32) -> Option<ID3D12CommandSignature> {
    static MADE: std::sync::Mutex<Vec<((u32, u32), ID3D12CommandSignature)>> =
        std::sync::Mutex::new(Vec::new());
    let mut made = MADE.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((_, s)) = made.iter().find(|(k, _)| *k == (kind, stride)) {
        return Some(s.clone());
    }
    let argument = D3D12_INDIRECT_ARGUMENT_DESC {
        Type: D3D12_INDIRECT_ARGUMENT_TYPE(kind as i32),
        ..Default::default()
    };
    let desc = D3D12_COMMAND_SIGNATURE_DESC {
        ByteStride: stride,
        NumArgumentDescs: 1,
        pArgumentDescs: &argument,
        NodeMask: 0,
    };
    let mut signature: Option<ID3D12CommandSignature> = None;
    // SAFETY: a description of one argument; no root arguments.
    let result = unsafe {
        crate::host::host()?.device.CreateCommandSignature(
            &desc,
            None::<&ID3D12RootSignature>,
            &mut signature,
        )
    };
    if let Err(e) = result {
        once!(error = %e, kind, stride, "host command signature refused");
        return None;
    }
    let signature = signature?;
    made.push(((kind, stride), signature.clone()));
    Some(signature)
}

/// Gets what a shader reads or writes through a view ready: buffer data
/// copied, textures moved to the state they need.
fn view_kind(view: &View) -> Option<ResourceType> {
    match view {
        View::Shader(..) | View::Placed(false, ..) => Some(ResourceType::Srv),
        View::Unordered(..) | View::Placed(true, ..) => Some(ResourceType::Uav),
        _ => None,
    }
}

/// The host state a depth view needs of its texture: the read state if the
/// view writes no plane the format has (the title's read-only flags), else
/// the write state.
/// shortcut: whole textures have one state, so a view that writes one of
/// two planes counts as writing.
fn depth_state(texture: &ID3D12Resource, desc: Option<&D3D12_DEPTH_STENCIL_VIEW_DESC>) -> u32 {
    // D16_UNORM and D32_FLOAT: depth formats without a stencil plane.
    const ONE_PLANE: [i32; 2] = [55, 40];
    let Some(desc) = desc else {
        return DEPTH_WRITE;
    };
    let format = if desc.Format.0 == 0 {
        // SAFETY: a live texture; a view without a format has its own.
        unsafe { texture.GetDesc() }.Format.0
    } else {
        desc.Format.0
    };
    let mut needed = D3D12_DSV_FLAG_READ_ONLY_DEPTH.0;
    if !ONE_PLANE.contains(&format) {
        needed |= D3D12_DSV_FLAG_READ_ONLY_STENCIL.0;
    }
    if desc.Flags.0 & needed == needed {
        DEPTH_READ
    } else {
        DEPTH_WRITE
    }
}

/// A table entry as [`State::table_entry`] makes it: the descriptor's
/// address and its 32 bytes, how the shader reads it, whether the shader
/// stores, and the depth target the draw tests.
type EntryKey = (u64, [u64; 4], ResourceType, bool, usize);

/// Skip a table entry this list made already with nothing changed since
/// (`VICTORY_TABLE_MEMO`, live: 0 off, the default; 1 on; 2 verify, which
/// makes every entry all the same and counts the skips that would have
/// changed the list). An entry is the same descriptor bytes read the same
/// way, and the same host copy of its texture; the list's
/// [`HostList::changes`] say whether a texture state, twin or written
/// texture moved since. Within a list, an entry's buffer range is noted
/// once and its texture uploaded once anyway.
///
/// Idea from victory-scarlett (Op-lite) commit `c45b43e96a`, GPL-3.0,
/// written anew here; see THIRD_PARTY.md.
fn table_memo() -> u64 {
    static ON: crate::live::Switch = crate::live::Switch::new("TABLE_MEMO", 0);
    ON.get()
}

/// What an entry showing `view` is remembered with: the host copy of the
/// texture it shows, 0 for a buffer; `None`, not remembered (a placed view,
/// whose memory may show another texture later; a view in another format
/// over a texture the host writes, copied in when its source changes).
fn memo_copy(view: &View) -> Option<usize> {
    if crate::descriptor::host::buffer_copy(view).is_some() {
        return Some(0);
    }
    let View::Shader(resource, _) = view else { return None };
    let resource = *resource as This;
    if crate::resource::alias_of(resource).is_some() {
        return None;
    }
    crate::resource::host_copy(resource).map(|c| std::sync::Arc::as_ptr(&c) as usize)
}

/// `VICTORY_TABLE_MEMO`, since the last line: entries looked at, skipped,
/// and (verify) skips that would have changed the list.
static MEMO: [std::sync::atomic::AtomicU64; 3] = [const { std::sync::atomic::AtomicU64::new(0) }; 3];

fn memo_count(hit: bool) {
    if table_memo() == 0 {
        return;
    }
    MEMO[0].fetch_add(1, Ordering::Relaxed);
    if hit {
        MEMO[1].fetch_add(1, Ordering::Relaxed);
    }
}

fn memo_wrong(handle: u64, kind: ResourceType) {
    if MEMO[2].fetch_add(1, Ordering::Relaxed) < 20 {
        tracing::warn!(handle = format_args!("{handle:#x}"), ?kind, "table memo (verify): a skipped entry would have changed the list");
    }
}

/// Count the dynamic slots tables take and are refused by why they are made
/// afresh, and the most slots held at once (`VICTORY_SLOT_REASONS=1`, live,
/// off by default; diagnostics only). On a host heap capped at 1,000,000
/// views the dynamic slots come out of the title's
/// (`descriptor::host::DYNAMIC`); this says how much of their use reading
/// constant buffers in place would give back.
pub(crate) fn slot_reasons() -> bool {
    static ON: crate::live::Switch = crate::live::Switch::new("SLOT_REASONS", 0);
    ON.on()
}

/// [`slot_reasons`] since the last line: by reason (constant buffer view,
/// 16-bit view, past the host heap), tables made, slots taken, tables
/// refused, slots refused.
static SLOT_WHY: [[std::sync::atomic::AtomicU64; 4]; 3] =
    [const { [const { std::sync::atomic::AtomicU64::new(0) }; 4] }; 3];

fn note_slot_reason(why: usize, n: u32, taken: bool) {
    let row = &SLOT_WHY[why];
    let at = if taken { 0 } else { 2 };
    row[at].fetch_add(1, Ordering::Relaxed);
    row[at + 1].fetch_add(u64::from(n), Ordering::Relaxed);
}

/// [`slot_reasons`]: the dynamic slots of constant buffer tables by what
/// keeps the table from being read in place (contract 3, "step 3"), the
/// first that does of its views ([`crate::descriptor::host::step3_reason`]).
static CBV_WHY: [std::sync::atomic::AtomicU64; crate::descriptor::host::STEP3_REASONS.len()] =
    [const { std::sync::atomic::AtomicU64::new(0) }; crate::descriptor::host::STEP3_REASONS.len()];

/// Why [`State::cbv_in_place`] answered as it did, per constant buffer view
/// asked ([`slot_reasons`]).
const IN_PLACE_WHY_NAMES: [&str; 12] = [
    "the root is not a table",
    "the slot shows no constant buffer view",
    "the slot was refused when made",
    "no host copy",
    "the copy is not in the card's memory (or marked)",
    "title pages marked rewritten in flight",
    "the slot views another copy",
    "read in place",
    "the handle is in no known heap",
    "the heap has no host copy",
    "the handle has no index in its heap",
    "the index is past the host heap",
];
const IN_PLACE_YES: usize = 7;
/// The "past the host heap" answers by how far past: bands of 2^16 slots
/// above the host heap's title slots, the last one open.
const PAST_BANDS: usize = 10;
static PAST_HEAP_BAND: [AtomicU64; PAST_BANDS] = [const { AtomicU64::new(0) }; PAST_BANDS];
static IN_PLACE_WHY: [AtomicU64; 12] = [const { AtomicU64::new(0) }; 12];

/// Step 3: constant buffer views not read in place because their slot views
/// another copy than the one refreshed ([`crate::descriptor::host::HostHeap::kept_is`]), since the
/// last line.
static KEPT_OTHER: AtomicU64 = AtomicU64::new(0);

fn note_cbv_reason(why: usize, n: u32) {
    CBV_WHY[why].fetch_add(u64::from(n), Ordering::Relaxed);
}

/// The [`slot_reasons`] line, per frame over `frames`.
#[cfg_attr(not(feature = "diagnostics"), allow(dead_code))]
pub(crate) fn slot_reasons_line(frames: f64) {
    let rows: [[u64; 4]; 3] =
        std::array::from_fn(|w| std::array::from_fn(|i| SLOT_WHY[w][i].swap(0, Ordering::Relaxed)));
    let peak = crate::descriptor::host::take_dynamic_peak();
    if !slot_reasons() || frames <= 0.0 {
        return;
    }
    let f = |v: u64| v as f64 / frames;
    let slots: u64 = rows.iter().map(|r| r[1] + r[3]).sum();
    let share = |r: &[u64; 4]| if slots == 0 { 0.0 } else { 100.0 * (r[1] + r[3]) as f64 / slots as f64 };
    tracing::info!(
        "dynamic slots per frame (VICTORY_SLOT_REASONS): constant buffer tables {:.0} slots in {:.0} ({:.0}%), refused {:.0} in {:.0}; \
         16-bit view tables {:.0} in {:.0} ({:.0}%), refused {:.0} in {:.0}; past the host heap {:.0} in {:.0} ({:.0}%), refused {:.0} in {:.0}; \
         most held at once {} of {}",
        f(rows[0][1]), f(rows[0][0]), share(&rows[0]), f(rows[0][3]), f(rows[0][2]),
        f(rows[1][1]), f(rows[1][0]), share(&rows[1]), f(rows[1][3]), f(rows[1][2]),
        f(rows[2][1]), f(rows[2][0]), share(&rows[2]), f(rows[2][3]), f(rows[2][2]),
        peak,
        crate::descriptor::host::dynamic_slots()
    );
    let why: Vec<u64> = CBV_WHY.iter().map(|w| w.swap(0, Ordering::Relaxed)).collect();
    let all: u64 = why.iter().sum();
    if all > 0 {
        let parts: Vec<String> = why
            .iter()
            .zip(crate::descriptor::host::STEP3_REASONS)
            .map(|(&w, name)| format!("{name} {:.0} ({:.1}%)", w as f64 / frames, 100.0 * w as f64 / all as f64))
            .collect();
        tracing::info!("constant buffer tables' dynamic slots per frame, by what keeps them from being read in place (step 3): {}", parts.join(", "));
    }
    let asked: Vec<u64> = IN_PLACE_WHY.iter().map(|w| w.swap(0, Ordering::Relaxed)).collect();
    let all: u64 = asked.iter().sum();
    if all > 0 {
        let parts: Vec<String> = asked
            .iter()
            .zip(IN_PLACE_WHY_NAMES)
            .filter(|(w, _)| **w > 0)
            .map(|(&w, name)| format!("{name} {:.0} ({:.1}%)", w as f64 / frames, 100.0 * w as f64 / all as f64))
            .collect();
        tracing::info!("step 3: constant buffer views per frame asked whether they are read in place: {}", parts.join(", "));
    }
    let bands: Vec<u64> = PAST_HEAP_BAND.iter().map(|w| w.swap(0, Ordering::Relaxed)).collect();
    let past: u64 = bands.iter().sum();
    if past > 0 {
        let parts: Vec<String> = bands
            .iter()
            .enumerate()
            .map(|(i, &w)| format!("+{}..{}: {:.0} ({:.1}%)", i << 16, if i + 1 == PAST_BANDS { "".to_string() } else { ((i + 1) << 16).to_string() }, w as f64 / frames, 100.0 * w as f64 / past as f64))
            .collect();
        tracing::info!("step 3: constant buffer views past the host heap per frame, by slots past it: {}", parts.join(", "));
    }
    let other = KEPT_OTHER.swap(0, Ordering::Relaxed);
    if other > 0 {
        tracing::info!(
            "step 3: constant buffer views per frame kept fresh because their slot views another copy than the one refreshed: {:.0}",
            other as f64 / frames
        );
    }
}

/// Whether a texture uploaded in part keeps the smaller count of its memory
/// while it waits, so that the memory coming back is uploaded
/// (`VICTORY_PARTLY_LESS=1`, live, off by default; see
/// `descriptor::host::TextureQueue::partly_less`).
fn partly_less() -> bool {
    static ON: crate::live::Switch = crate::live::Switch::new("PARTLY_LESS", 0);
    ON.on()
}

/// The `VICTORY_TABLE_MEMO` line, per frame over `frames`.
pub(crate) fn table_memo_line(frames: f64) {
    let [seen, hit, wrong] = std::array::from_fn::<u64, 3, _>(|i| MEMO[i].swap(0, Ordering::Relaxed));
    if seen == 0 || frames <= 0.0 {
        return;
    }
    tracing::info!(
        "table entries per frame (VICTORY_TABLE_MEMO={}): {:.0}, made earlier in their list with nothing changed {:.0} ({:.1}%); \
         of those, would have changed the list (verify) {}",
        table_memo(),
        seen as f64 / frames,
        hit as f64 / frames,
        100.0 * hit as f64 / seen as f64,
        wrong
    );
}

fn readable(list: &HostList, kind: ResourceType, view: &View) {
    const UNORDERED_ACCESS: u32 = 0x8;
    // A constant buffer view read in place (contract 3): its range is
    // copied at submission, as a buffer view's.
    if let View::Constant(address, size) = *view
        && let Some(shadow) = crate::shadow::find(address, u64::from(size))
    {
        shadow.refresh(list, address, u64::from(size));
    }
    let placed = crate::descriptor::host::placed_texture(view);
    let view = placed.as_ref().unwrap_or(view);
    // Buffer views read title memory: their range is copied at submission;
    // what shaders write through unordered access goes back to it after.
    if let Some((shadow, address, len)) = crate::descriptor::host::buffer_copy(view) {
        shadow.refresh(list, address, len);
        if kind == ResourceType::Uav {
            // A shader that cannot store to buffers leaves the range as
            // the title has it: nothing to find afterwards.
            if STORES.get() || !write_skip() {
                // Through a view of the heap: marked if the shader marks.
                shadow.gpu_writes_marked(list, address, len, MARKED.get());
            } else {
                #[cfg(feature = "diagnostics")]
                {
                    crate::copystat::add(crate::copystat::C::GpuWritesLeftOut, 1);
                    crate::copystat::add(crate::copystat::C::GpuWritesLeftOutBytes, len);
                }
            }
        }
    }
    match (view, kind) {
        (View::Shader(resource, words), ResourceType::Srv) => {
            #[cfg(feature = "diagnostics")]
            crate::bake_probe::sampled(*resource as This, placed.is_some());
            // A view the title placed, in another format, in the memory of
            // a texture the host writes: that texture's texels, copied on
            // the host (title memory does not have them).
            let aliased = crate::resource::alias_of(*resource as This)
                .is_some_and(|source| crate::texture::alias(list, *resource as This, source));
            if !aliased
                && let Some(t) = crate::resource::host_resource(*resource as This)
                && crate::texture::title_written(t)
            {
                // Of a texture the title placed, the mips the view shows.
                let first = match (&placed, words) {
                    (Some(_), Some(words)) => crate::resource::first_mip(words),
                    _ => 0,
                };
                crate::texture::upload_from(list, *resource as This, t, first);
            }
            // A depth texture shaders also write: what they wrote, first.
            list.twin_use(*resource as This, false, false);
            // The depth target the draw only tests is read in both ways at
            // once: both read states, which its preparation asks for too.
            let (tested, _) = TESTED_DEPTH.get();
            let state = if tested != 0 && tested == *resource {
                TESTED_DEPTH.set((tested, true));
                DEPTH_READ | SHADER_RESOURCE
            } else {
                SHADER_RESOURCE
            };
            crate::list::ensure_state(list, *resource as This, state);
            #[cfg(feature = "diagnostics")]
            if let Some(t) = crate::resource::host_resource(*resource as This) {
                // SAFETY: a live resource.
                let d = unsafe { t.GetDesc() };
                if d.Flags.0 & 7 == 0 {
                    tracing::debug!(
                        resource = format_args!("{resource:#x}"),
                        ?d,
                        "a shader reads a texture the title fills"
                    );
                }
            }
        }
        (View::Unordered(resource, _, words), ResourceType::Uav) => {
            #[cfg(feature = "diagnostics")]
            crate::capture::note_target(*resource as This);
            // A texture the title's CPU code reads back (`crate::texback`).
            list.texture_written(*resource as This, words.as_ref());
            // Unordered access to a depth texture is its colour twin's.
            if !list.twin_use(*resource as This, true, true) {
                crate::list::ensure_state(list, *resource as This, UNORDERED_ACCESS);
            }
        }
        _ => {}
    }
}

/// The pixel shaders whose draws get their slots listed
/// ([`State::slots_text`]): `VICTORY_LIST_SLOTS`, hash prefixes separated
/// by commas. None by default.
#[cfg(feature = "diagnostics")]
fn list_slots() -> &'static [String] {
    static LIST: std::sync::OnceLock<Vec<String>> = std::sync::OnceLock::new();
    LIST.get_or_init(|| {
        std::env::var("VICTORY_LIST_SLOTS")
            .unwrap_or_default()
            .split(',')
            .map(|s| s.trim().to_ascii_lowercase())
            .filter(|s| !s.is_empty())
            .collect()
    })
}

/// A fingerprint of the 4 KiB of title memory at `at` (FNV-1a, 64 bits; of
/// 4 KiB of zeros it is always the same value, which `slots.py` knows), or
/// "unreadable".
#[cfg(feature = "diagnostics")]
fn print_4k(at: u64) -> String {
    let mut bytes = [0u8; 4096];
    if at == 0 || !crate::host::read_into(at as *const u8, &mut bytes) {
        return "unreadable".into();
    }
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for b in bytes {
        hash = (hash ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:016x}")
}

/// The `bytes` of title memory at `at`: how many are mapped, and the
/// fingerprints of the first and the last 4 KiB (a texture's smallest
/// levels lie first, its largest last).
#[cfg(feature = "diagnostics")]
fn memory_state(at: u64, bytes: u64) -> String {
    let mapped = crate::texture::mapped_bytes(at as *const u8, bytes as usize);
    let last = at + bytes.saturating_sub(4096);
    format!("memory={at:#x} mapped={mapped}/{bytes} first4k={} last4k={}", print_4k(at), print_4k(last))
}

/// Title texture `this` for the frame listing: its size, whether it has a
/// host copy (not made here), its staging buffer's state, whether the host
/// copied into it, and its memory ([`memory_state`]).
#[cfg(feature = "diagnostics")]
fn texture_state(this: This) -> String {
    let Some((at, desc)) = crate::resource::texture_memory(this) else {
        return "not a texture".into();
    };
    // SAFETY: 56 bytes in the description's layout.
    let words = unsafe { std::ptr::from_ref(&desc).cast::<[u32; 14]>().read_unaligned() };
    let bytes = crate::resource::placed_bytes(&words).unwrap_or(0);
    let staged = match crate::texture::staged_state(this) {
        Some((uploaded, dirty)) => format!("staged=yes uploaded={uploaded} dirty={dirty}"),
        None => "staged=no".into(),
    };
    format!(
        "{}x{} f{} levels={} host_copy={} {staged} host_written={} title_fills={} {}",
        u64::from(words[4]) | u64::from(words[5]) << 32,
        words[6],
        words[8],
        words[7] >> 16,
        crate::resource::host_made(this).is_some(),
        crate::texture::host_wrote(this as usize),
        crate::resource::title_fills(this),
        memory_state(at as u64, bytes)
    )
}

/// A view of the title's heap for the frame listing: a texture with its
/// state ([`texture_state`]; a placed one that has no resource yet with
/// its memory alone), anything else in a few words.
#[cfg(feature = "diagnostics")]
fn view_state(view: Option<&View>) -> String {
    match view {
        None => "no view".into(),
        Some(View::Placed(false, r, d)) if r[0] != 1 => {
            let address = u64::from(d[10]) | u64::from(d[11]) << 32;
            let state = match crate::resource::texture_at_made(address, r) {
                Some(this) => texture_state(this),
                None => format!(
                    "{}x{} f{} levels={} resource=none {}",
                    u64::from(r[4]) | u64::from(r[5]) << 32,
                    r[6],
                    r[8],
                    r[7] >> 16,
                    memory_state(address, crate::resource::placed_bytes(r).unwrap_or(0))
                ),
            };
            // (Last, so that readers of the older lines still find
            // theirs: the level the view starts at, and how many it shows.)
            format!("placed texture view_format={} {state} view_levels={}+{}", d[0], d[4], d[5] as i32)
        }
        Some(View::Shader(r, d)) if crate::resource::texture_memory(*r as This).is_some() => {
            format!(
                "texture object {r:#x} view_format={} {} view_levels={}",
                d.as_ref().map_or(0, |d| d[0]),
                texture_state(*r as This),
                d.as_ref().map_or("all".to_owned(), |d| format!("{}+{}", d[4], d[5] as i32))
            )
        }
        Some(v) => format!("{v:?}").chars().take(80).collect(),
    }
}

/// A view in a few words (diagnostics builds).
#[cfg(feature = "diagnostics")]
fn describe(view: Option<&View>) -> String {
    let texture = |r: This| {
        crate::resource::host_made(r).map(|t| {
            // SAFETY: a live resource.
            let d = unsafe { t.GetDesc() };
            format!("{}x{} f{} flags {:#x} of {r:?}", d.Width, d.Height, d.Format.0, d.Flags.0)
        })
    };
    match view {
        None => "no view".into(),
        Some(v @ View::Shader(r, _)) => texture(*r as This)
            .unwrap_or_else(|| format!("buffer {:?}", crate::descriptor::host::buffer_range(v))),
        Some(v @ View::Unordered(r, ..)) => match texture(*r as This) {
            Some(t) => format!("uav texture {t}"),
            None => format!(
                "uav resource {r:#x} buffer {:?}",
                crate::descriptor::host::buffer_range(v)
            ),
        },
        Some(View::Placed(u, r, d)) => format!(
            "placed uav={u} dim {} {}x{} f{} layout {:#x} at {:#x}",
            r[0],
            u64::from(r[4]) | u64::from(r[5]) << 32,
            r[6],
            r[8],
            r[11],
            u64::from(d[10]) | u64::from(d[11]) << 32
        ),
        // The first four values, as a colour rectangle's constants are.
        Some(View::Constant(address, size))
            if *size >= 16 && crate::texture::committed(*address as *const u8, 16) =>
        {
            // SAFETY: 16 committed bytes of title memory.
            let v = unsafe { (*address as *const [f32; 4]).read_unaligned() };
            format!("Constant({address:#x}, {size}) {v:?}")
        }
        Some(v) => format!("{v:?}").chars().take(60).collect(),
    }
}

/// The console's own primitive topologies.
const TRIANGLE_FAN: u32 = 6;
const RECTANGLES: u32 = 17;
const LINE_LOOP: u32 = 18;
const QUADS: u32 = 19;
const QUAD_STRIP: u32 = 20;

/// Fixed-function state, straight onto the host list.
pub(crate) fn viewports(list: &HostList, viewports: &[D3D12_VIEWPORT]) {
    // SAFETY: an open list.
    unsafe { list.list.RSSetViewports(viewports) };
}

pub(crate) fn scissors(list: &HostList, rects: &[RECT]) {
    // SAFETY: an open list.
    unsafe { list.list.RSSetScissorRects(rects) };
}

pub(crate) fn blend_factor(list: &HostList, factor: Option<&[f32; 4]>) {
    // SAFETY: an open list.
    unsafe { list.list.OMSetBlendFactor(factor) };
}

pub(crate) fn stencil_ref(list: &HostList, reference: u32) {
    #[cfg(feature = "diagnostics")]
    crate::capture::op(std::ptr::from_ref(list) as usize, || format!("STENCIL REF {reference:#x}"));
    // SAFETY: an open list.
    unsafe { list.list.OMSetStencilRef(reference) };
}

pub(crate) fn depth_bounds(list: &HostList, min: f32, max: f32) {
    #[cfg(feature = "diagnostics")]
    crate::capture::op(std::ptr::from_ref(list) as usize, || {
        format!("DEPTH BOUNDS {min}..{max}")
    });
    if crate::host::host().is_some_and(|h| h.depth_bounds())
        && let Ok(l1) = list.list.cast::<ID3D12GraphicsCommandList1>()
    {
        // SAFETY: an open list on a device that supports depth bounds.
        unsafe { l1.OMSetDepthBounds(min, max) };
    }
}

#[cfg(test)]
mod early_textures_tests {
    use super::*;

    #[test]
    fn early_budget_runs_once_per_frame_and_resets_bake_budget() {
        let last_frame = AtomicU64::new(u64::MAX);
        let baked_ms = AtomicU64::new(0);
        // First draw with a view heap in frame 1 gets the early budget.
        assert_eq!(
            State::claim_early_budget(1, false, 12, 1500, &last_frame, &baked_ms),
            Some(((12, 64 << 20), false))
        );
        // Second non-bake draw in the same frame gets none.
        assert_eq!(
            State::claim_early_budget(1, false, 12, 1500, &last_frame, &baked_ms),
            None
        );
        // Bake draw in the same frame still gets the full bake budget.
        assert_eq!(
            State::claim_early_budget(1, true, 12, 1500, &last_frame, &baked_ms),
            Some(((1500, 2 << 30), true))
        );
        baked_ms.store(1500, Ordering::Relaxed);
        assert_eq!(
            State::claim_early_budget(1, true, 12, 1500, &last_frame, &baked_ms),
            None
        );
        // Next frame resets baked_ms and allows early budget again.
        assert_eq!(
            State::claim_early_budget(2, false, 12, 1500, &last_frame, &baked_ms),
            Some(((12, 64 << 20), false))
        );
        assert_eq!(baked_ms.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn a_full_card_caps_the_texture_pass_time_and_drops_the_backlog_share() {
        let ms = std::time::Duration::from_millis;
        // Its own budget, and three times it for a backlog.
        assert_eq!(super::texture_pass_budget(192, ms(250), false, None), (192, ms(250)));
        assert_eq!(super::texture_pass_budget(192, ms(250), true, None), (576, ms(750)));
        // At the card's line: the full card's time, backlog or not.
        assert_eq!(super::texture_pass_budget(192, ms(250), true, Some(ms(50))), (192, ms(50)));
        assert_eq!(super::texture_pass_budget(192, ms(250), false, Some(ms(50))), (192, ms(50)));
        // Never more than its own.
        assert_eq!(super::texture_pass_budget(192, ms(20), true, Some(ms(50))), (192, ms(20)));
    }

    #[test]
    fn early_textures_without_view_heap_does_not_burn_frame_pass() {
        let Some(host) = crate::host::host() else { return };
        let list = host.new_list().expect("host list");
        let mut state = State::default();
        let last_frame = AtomicU64::new(u64::MAX);
        let baked_ms = AtomicU64::new(0);
        // Draw 1 in frame 7 has no view heap bound: must NOT claim frame 7.
        assert!(!state.early_textures_with(&list, false, 12, 1500, 7, &last_frame, &baked_ms));
        assert_eq!(last_frame.load(Ordering::Relaxed), u64::MAX);

        // Bind a real shader-visible view heap and draw again in the SAME frame 7.
        // SAFETY: HeapDesc is repr(C) [kind, count, flags, nodes].
        let desc: crate::descriptor::HeapDesc = unsafe { std::mem::transmute([0u32, 4, 1, 0]) };
        let iid = crate::object::guid(
            0x8efb471d,
            0x616c,
            0x4f49,
            [0x90, 0xf7, 0x12, 0x7b, 0xb7, 0x63, 0xfa, 0x51],
        );
        let mut heap = std::ptr::null();
        assert_eq!(
            crate::descriptor::create_heap(std::ptr::null(), &desc, &iid, &mut heap),
            crate::object::S_OK
        );
        state.set_heaps(&[heap]);
        let before = EARLY_TEXTURE_PASSES[0].load(Ordering::Relaxed);
        assert!(state.early_textures_with(&list, false, 12, 1500, 7, &last_frame, &baked_ms));
        assert_eq!(last_frame.load(Ordering::Relaxed), 7);
        assert_eq!(EARLY_TEXTURE_PASSES[0].load(Ordering::Relaxed), before + 1);
        // A second non-bake draw in the same frame does not run again.
        assert!(!state.early_textures_with(&list, false, 12, 1500, 7, &last_frame, &baked_ms));

        list.close();
        drop(list);
        let release = crate::object::slot::<extern "system" fn(This) -> u32>(heap, 0x10);
        assert_eq!(release(heap), 0);
    }
}

/// AMD GFX10 / console `COMPUTE_DISPATCH_INITIATOR` bit 5:
/// `USE_THREAD_DIMENSIONS` (`DispatchX` arguments are raw thread counts rather
/// than thread-group counts, with trailing lanes masked off in initial `EXEC`).
const DISPATCH_USE_THREAD_DIMENSIONS: u32 = 0x20;

/// The `DispatchX` flags the host honours for a pipeline (`builder`: one
/// of the ray-tracing structure builder's). `VICTORY_DISPATCH_THREADS`:
/// unset or `builder`, thread counts are honoured for the builder's stages
/// only (they run only with `VICTORY_BVH_BUILDER=1`, so a run without it is
/// as before); `all`, for every program; `0`, for none (counts are taken
/// for groups, as before: the builder then dispatches up to 64 times its
/// threads and its stages store past the structures they build).
fn thread_dimension_flags(builder: bool, flags: u32) -> u32 {
    #[derive(Clone, Copy, PartialEq)]
    enum Scope {
        Builder,
        All,
        None,
    }
    static SCOPE: std::sync::OnceLock<Scope> = std::sync::OnceLock::new();
    let scope = *SCOPE.get_or_init(|| match std::env::var("VICTORY_DISPATCH_THREADS").as_deref() {
        Ok("all") => Scope::All,
        Ok("0") => Scope::None,
        _ => Scope::Builder,
    });
    if flags & DISPATCH_USE_THREAD_DIMENSIONS == 0 {
        return flags;
    }
    match scope {
        Scope::All => flags,
        Scope::Builder if builder => flags,
        _ => {
            if !builder {
                once!(
                    flags = format_args!("{flags:#x}"),
                    "a DispatchX of thread counts by a program outside the ray-tracing structure builder: taken for groups (VICTORY_DISPATCH_THREADS=all honours it)"
                );
            }
            flags & !DISPATCH_USE_THREAD_DIMENSIONS
        }
    }
}

/// Converts console `Dispatch` / `DispatchX(x, y, z, flags)` dimensions to
/// `(host_groups, partial_thread_limits)`. Returns `None` when a
/// thread-dimension dispatch has zero threads along any axis.
fn dispatch_dimensions(
    dims: [u32; 3],
    group: [u32; 3],
    flags: u32,
) -> Option<([u32; 3], [u32; 3])> {
    if flags & DISPATCH_USE_THREAD_DIMENSIONS == 0 {
        return Some((dims, [0; 3]));
    }
    if dims.contains(&0) {
        return None;
    }
    let mut groups = [0; 3];
    let mut partial = [0; 3];
    for i in 0..3 {
        let g = group[i].max(1);
        groups[i] = dims[i].div_ceil(g);
        if g > 1 && !dims[i].is_multiple_of(g) {
            partial[i] = dims[i];
        }
    }
    Some((groups, partial))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_lone_record_opens_a_window_at_the_remembered_step() {
        // (a mesh dispatch is kind 10, a draw kind 0)
        let remembered = Some((7u64, 10u32, 12u32, 32u32));
        // the same buffer, kind and stride: sixteen records at the title's step
        assert_eq!(jump_window(remembered, (7, 10, 12), 1 << 20), Some((32, 16)));
        // never past the buffer: 100 bytes left hold three records 32 apart
        assert_eq!(jump_window(remembered, (7, 10, 12), 100), Some((32, 3)));
        // one record fits: nothing to open
        assert_eq!(jump_window(remembered, (7, 10, 12), 40), None);
        // another buffer, kind or stride, or nothing remembered: alone, as before
        assert_eq!(jump_window(remembered, (8, 10, 12), 1 << 20), None);
        assert_eq!(jump_window(remembered, (7, 0, 12), 1 << 20), None);
        assert_eq!(jump_window(remembered, (7, 10, 16), 1 << 20), None);
        assert_eq!(jump_window(None, (7, 10, 12), 1 << 20), None);
    }

    #[test]
    fn dispatch_x_thread_dimensions_convert_to_groups_and_partial_limits() {
        // Normal Dispatch / DispatchX without bit 5 passes group counts through.
        assert_eq!(
            dispatch_dimensions([48, 1, 1], [64, 1, 1], 0),
            Some(([48, 1, 1], [0, 0, 0]))
        );
        assert_eq!(
            dispatch_dimensions([48, 1, 1], [64, 1, 1], 0x8000),
            Some(([48, 1, 1], [0, 0, 0]))
        );
        // Bit 5 (0x20 / 0x8020): raw thread counts -> ceil(threads / group) + partial limits.
        assert_eq!(
            dispatch_dimensions([48, 1, 1], [64, 1, 1], 0x20),
            Some(([1, 1, 1], [48, 0, 0]))
        );
        assert_eq!(
            dispatch_dimensions([1, 1, 1], [64, 1, 1], 0x8020),
            Some(([1, 1, 1], [1, 0, 0]))
        );
        assert_eq!(
            dispatch_dimensions([65, 1, 1], [64, 1, 1], 0x20),
            Some(([2, 1, 1], [65, 0, 0]))
        );
        // Exact multiple of group size needs no partial-lane limit.
        assert_eq!(
            dispatch_dimensions([128, 1, 1], [64, 1, 1], 0x20),
            Some(([2, 1, 1], [0, 0, 0]))
        );
        // 3D partial dimensions.
        assert_eq!(
            dispatch_dimensions([10, 16, 5], [8, 8, 4], 0x20),
            Some(([2, 2, 2], [10, 0, 5]))
        );
        // Zero threads along any axis is a no-op.
        assert_eq!(dispatch_dimensions([0, 1, 1], [64, 1, 1], 0x20), None);
    }
}
