//! Indirect execution's arguments, bounded before the GPU reads them. The
//! title's own compute passes write most of them; a pass we drop or
//! translate wrongly leaves whatever the memory held, and a draw of four
//! billion vertices or a dispatch past the host's limits runs until the
//! host resets the display driver. So active records are copied and clamped
//! on the GPU into a device-local scratch buffer, just before execution, by a
//! compute shader of ours (HLSL compiled by the system's compiler; a 5.1
//! shader with its root signature inside), leaving the title's argument and
//! count buffers untouched.
//!
//! The scratch buffer is left readable as indirect arguments after a clamp
//! and stays so until the next clamp into it, which moves it straight back
//! to unordered access; it goes back to the common state once, when the
//! list closes. The executions that read it record no barrier of their
//! own, so draws that read records clamped together (`crate::draw`'s
//! windows) follow each other with nothing between them. Before, each
//! execution was followed by a move of the scratch buffer back to the
//! common state, which drivers may treat as a full wait.
//!
//! When a count buffer is given, threads at or past `min(max, count)` return
//! immediately without charging the list's budget or reading uninitialized
//! tail slots. Each list has a budget (2^27 vertex instances or threads,
//! groups counted as 64 threads, mesh groups as 128): active records past it
//! run empty, saturating without 32-bit wraparound so a list of thousands of
//! garbage records cannot run for seconds either.
//!
//! A dispatch by a program that takes the number of its first group
//! (`victory_shader::emit::USER_GROUP_BASE`) may ask for more groups in x than
//! the host runs in one dimension (65,535); the console runs them. Those
//! are run in parts, with or without the clamp: [`bound_split`] makes the
//! parts of an indirect dispatch (the clamp's pass in another mode), and the
//! direct ones are cut by [`dispatch_parts`].
//!
//! shortcut: the bounds (2^24 vertices, 2^26 vertex instances, 65,535
//! groups per dimension and 2^20 per dispatch, one-dimensional mesh
//! dispatches, the budget) cut legitimate work beyond them; raise them if
//! the title needs more.

use windows::Win32::Graphics::Direct3D::Fxc::D3DCompile;
use windows::Win32::Graphics::Direct3D::ID3DBlob;
use windows::Win32::Graphics::Direct3D12::*;
use windows::core::s;

use crate::host::{HostList, host, transition};

const SOURCE: &str = r#"
#define RS "RootConstants(num32BitConstants=6, b0), SRV(t0), SRV(t1), UAV(u0), UAV(u1)"
cbuffer Record : register(b0) {
    uint kind;
    uint stride;
    uint records;
    uint args_offset;
    // Bit 0: a count buffer bounds the records. Bit 1: the records are a
    // window clamped ahead of their draws: one that asks for more than can
    // run is left empty and costs nothing. Bit 2: dispatches, each written as
    // SPLIT records of at most 65,535 groups in x each (a program that takes
    // the number of its first group runs them one after the other). Bit 3
    // (with bit 2): no clamp and no budget, the groups as the title asked up
    // to what SPLIT records hold.
    uint has_count;
    uint count_offset;
};
ByteAddressBuffer src_args : register(t0);
ByteAddressBuffer count_buf : register(t1);
RWByteAddressBuffer dst_args : register(u0);
// The list's work so far (vertex instances, threads): past the budget,
// records run empty.
RWByteAddressBuffer budget : register(u1);
static const uint BUDGET = 1u << 27;
// The vertices or indices an instance from which a draw record runs empty.
static const uint MOST_DRAW = 1u << 23;
// Groups of one dispatch; more run empty.
static const uint GROUPS = 1u << 20;
// Records a dispatch is made in, at most (`SPLIT` of this file's Rust side).
static const uint SPLIT = 4;
// Each group charges the budget once: the costs of its records go here,
// one thread charges their sum, saturating as each record would in lane
// order, and every thread then reads whether its own record fitted. A
// compare and swap per record, 64 threads racing for one word, cost a
// round for each record of a window, and in memory the CPU maps (where the
// budget was, and is with VICTORY_INDIRECT_CARD_BUDGET=0) every atomic
// crosses the bus on a card of its own.
groupshared uint costs[64];
groupshared uint fits[64];
groupshared uint largest_asked;
groupshared uint all_asked;
[RootSignature(RS)]
[numthreads(64, 1, 1)]
void main(uint3 id : SV_DispatchThreadID, uint gi : SV_GroupIndex) {
    uint eff_count = records;
    bool ahead = (has_count & 2u) != 0;
    bool split = (has_count & 4u) != 0;
    bool unclamped = split && (has_count & 8u) != 0;
    if ((has_count & 1u) != 0) {
        eff_count = min(eff_count, count_buf.Load(count_offset));
    }
    // Threads past the records take part in the group's charge (cost 0)
    // and store nothing.
    bool active = id.x < eff_count;
    uint src_at = args_offset + id.x * stride;
    // In parts, the records of one dispatch lie SPLIT of them apart.
    uint dst_at = id.x * (split ? SPLIT * 12u : stride);
    uint cost = 0;
    uint asked = 0;
    uint2 draw_counts = uint2(0, 0);
    uint3 groups = uint3(0, 0, 0);
    if (gi == 0) {
        largest_asked = 0;
        all_asked = 0;
    }
    if (active && kind <= 1) {
        // Draws: vertex or index count, then instances.
        uint2 want = src_args.Load2(src_at);
        if (ahead && (want.x > (1u << 24) || want.y > (1u << 24))) {
            want = uint2(0, 0);
        }
        uint count = min(want.x, 1u << 24);
        uint instances = min(want.y, (1u << 26) / max(count, 1u));
        cost = count * instances;
        // A record that asks for MOST_DRAW vertices or indices an instance
        // or more is no draw of the title's (no mesh of a console title has
        // 2.8 million triangles): it runs empty and charges nothing. Run
        // 206's window check found one list with 90 draws of 2^24 vertices
        // and one instance each. Clamped one draw at a time, the list's
        // budget lets eight of them run (2^27 in all) and empties the other
        // 82; served from a window, whose records are charged once however
        // often they are drawn, all 90 ran: 1.5 thousand million vertices
        // in one list, with windows on in the runs that lost the device
        // (201, 204).
        if (count >= MOST_DRAW) {
            count = 0;
            instances = 0;
            cost = 0;
        }
        draw_counts = uint2(count, instances);
    } else if (active && kind == 2) {
        uint3 wanted = src_args.Load3(src_at);
        uint3 g;
        if (split) {
            // Diagnostics, in the budget buffer's fourth word: the largest
            // x a dispatch in parts asked for.
            uint was;
            budget.InterlockedMax(12, wanted.x, was);
        }
        if (unclamped) {
            // As the title asked; only the groups past the last part are left
            // out, and nothing is charged.
            g = uint3(min(wanted.x, SPLIT * 65535u), wanted.y, wanted.z);
            cost = 0;
        } else {
            g = min(wanted, uint3(65535u, 65535u, 65535u));
            // In parts: one dimension, and not the garbage of an argument
            // nothing wrote.
            if (split && g.y <= 1u && g.z <= 1u && wanted.x <= SPLIT * 65535u) {
                g.x = wanted.x;
            }
            // x * y fits 32 bits; the whole count may not.
            if (g.z != 0 && g.x * g.y > GROUPS / g.z) {
                g = uint3(0, 0, 0);
            }
            cost = g.x * g.y * g.z * 64u;
        }
        groups = g;
    } else if (active) {
        // Mesh dispatches: the console's are one-dimensional.
        uint3 g = src_args.Load3(src_at);
        // Ahead of its draw, a record over the most a dispatch runs is a
        // slot the title has not filled (the title leaves all
        // ones there): empty, and not counted below either.
        if (ahead && g.x > 65535u) {
            g = uint3(0, 0, 0);
        }
        asked = g.x;
        g = uint3(min(g.x, 65535u), min(g.y, 1u), min(g.z, 1u));
        cost = g.x * g.y * g.z * 128u;
        groups = g;
    }
    costs[gi] = cost;
    GroupMemoryBarrierWithGroupSync();
    if (active && kind > 2) {
        // Diagnostics, in the budget buffer's next words (below): the
        // largest group count a mesh record asked for, and the groups
        // asked in all.
        uint was;
        InterlockedMax(largest_asked, asked, was);
        InterlockedAdd(all_asked, min(asked, 1u << 24), was);
    }
    // The group's own records: none for a group past a count buffer's
    // count (an execution may ask for thousands and count a few), which
    // touches the budget not at all, as its threads did not before.
    uint first = id.x - gi;
    uint n = eff_count > first ? min(64u, eff_count - first) : 0u;
    if (gi == 0 && n > 0) {
        // What the records charge, from `before` on, in lane order: a
        // record that does not fit whole runs empty and takes the rest.
        // Only the group's own records are walked (one, clamped alone).
        uint before;
        budget.InterlockedOr(0, 0, before);
        [allow_uav_condition]
        while (before < BUDGET) {
            uint next = before;
            for (uint j = 0; j < n; j++) {
                uint c = costs[j];
                if (c > 0 && next < BUDGET) {
                    next = (BUDGET - next <= c) ? BUDGET : (next + c);
                }
            }
            if (next == before) break;
            uint prev;
            budget.InterlockedCompareExchange(0, before, next, prev);
            if (prev == before) break;
            before = prev;
        }
        uint at = before;
        for (uint k = 0; k < n; k++) {
            uint c = costs[k];
            uint fit = 1;
            if (c > 0) {
                fit = (at < BUDGET && BUDGET - at >= c) ? 1u : 0u;
                at = (at >= BUDGET || BUDGET - at <= c) ? BUDGET : (at + c);
            }
            fits[k] = fit;
        }
    }
    GroupMemoryBarrierWithGroupSync();
    if (gi == 0 && kind > 2 && n > 0) {
        uint was;
        budget.InterlockedMax(4, largest_asked, was);
        budget.InterlockedAdd(8, all_asked, was);
    }
    if (!active) {
        // Past the count, with bit 2: an empty record, charging nothing.
        if ((has_count & 4u) != 0 && id.x < records) {
            for (uint off = 0; off < stride; off += 4) {
                dst_args.Store(dst_at + off, 0u);
            }
        }
        return;
    }
    bool over = fits[gi] == 0;
    if (kind <= 1) {
        if (over) {
            draw_counts = uint2(0, 0);
        }
        dst_args.Store2(dst_at, draw_counts);
        for (uint off = 8; off < stride; off += 4) {
            dst_args.Store(dst_at + off, src_args.Load(src_at + off));
        }
    } else {
        if (over) {
            groups = uint3(0, 0, 0);
        }
        if (split) {
            for (uint part = 0; part < SPLIT; part++) {
                uint before = part * 65535u;
                uint x = groups.x > before ? min(groups.x - before, 65535u) : 0u;
                // A part with no groups is empty whatever else it says.
                uint3 rest = x == 0u ? uint3(0, 0, 0) : groups;
                dst_args.Store3(dst_at + part * 12u, uint3(x, rest.yz));
            }
            return;
        }
        dst_args.Store3(dst_at, groups);
        for (uint off = 12; off < stride; off += 4) {
            dst_args.Store(dst_at + off, src_args.Load(src_at + off));
        }
    }
}
"#;

struct Clamp {
    root: ID3D12RootSignature,
    pso: ID3D12PipelineState,
}

// SAFETY: D3D12 objects are free-threaded.
unsafe impl Send for Clamp {}
// SAFETY: as above.
unsafe impl Sync for Clamp {}

fn clamp() -> Option<&'static Clamp> {
    static CLAMP: std::sync::OnceLock<Option<Clamp>> = std::sync::OnceLock::new();
    CLAMP
        .get_or_init(|| match make() {
            Ok(c) => Some(c),
            Err(e) => {
                tracing::warn!(error = %e, "no argument clamp: indirect execution dropped");
                None
            }
        })
        .as_ref()
}

fn make() -> windows::core::Result<Clamp> {
    let device = &host().ok_or_else(windows::core::Error::empty)?.device;
    make_for(device)
}

fn make_for(device: &ID3D12Device) -> windows::core::Result<Clamp> {
    make_from(device, SOURCE)
}

/// The clamp compiled from `source` (ours, or in the benchmark the one it
/// replaced).
fn make_from(device: &ID3D12Device, source: &str) -> windows::core::Result<Clamp> {
    let mut code: Option<ID3DBlob> = None;
    let mut errors: Option<ID3DBlob> = None;
    // SAFETY: the source and names live through the call; out pointers ours.
    let compiled = unsafe {
        D3DCompile(
            source.as_ptr().cast(),
            source.len(),
            s!("clamp"),
            None,
            None,
            s!("main"),
            s!("cs_5_1"),
            0,
            0,
            &mut code,
            Some(&mut errors),
        )
    };
    if let Err(e) = compiled {
        if let Some(errors) = errors {
            // SAFETY: the blob's own bytes.
            let text = unsafe {
                std::slice::from_raw_parts(
                    errors.GetBufferPointer().cast::<u8>(),
                    errors.GetBufferSize(),
                )
            };
            tracing::warn!("argument clamp: {}", String::from_utf8_lossy(text));
        }
        return Err(e);
    }
    let code = code.ok_or_else(windows::core::Error::empty)?;
    // SAFETY: the blob's own bytes; the root signature is inside the shader.
    unsafe {
        let bytes =
            std::slice::from_raw_parts(code.GetBufferPointer().cast::<u8>(), code.GetBufferSize());
        let root: ID3D12RootSignature = device.CreateRootSignature(0, bytes)?;
        let pso = device.CreateComputePipelineState(&D3D12_COMPUTE_PIPELINE_STATE_DESC {
            pRootSignature: std::mem::transmute_copy(&root),
            CS: D3D12_SHADER_BYTECODE {
                pShaderBytecode: bytes.as_ptr().cast(),
                BytecodeLength: bytes.len(),
            },
            ..Default::default()
        })?;
        Ok(Clamp { root, pso })
    }
}

/// Minimum record byte stride for a supported PC indirect argument `kind`.
fn min_stride(kind: u32) -> Option<u32> {
    match kind {
        0 => Some(16),
        1 => Some(20),
        2 | 10 => Some(12),
        _ => None,
    }
}

/// Parses `VICTORY_INDIRECT_CLAMP`: enabled by default; explicit `"0"`,
/// `"false"`, `"no"`, or `"off"` (ASCII case-insensitive) bypasses the
/// argument-clamp compute pass so `ExecuteIndirect` reads the title's
/// argument buffer directly.
pub(crate) fn parse_clamp_enabled(value: Option<&std::ffi::OsStr>) -> bool {
    let Some(value) = value else {
        return true;
    };
    let Some(text) = value.to_str() else {
        return true;
    };
    let trimmed = text.trim();
    !(trimmed == "0"
        || trimmed.eq_ignore_ascii_case("false")
        || trimmed.eq_ignore_ascii_case("no")
        || trimmed.eq_ignore_ascii_case("off"))
}

/// Whether indirect arguments are copied and clamped by our compute shader
/// before `ExecuteIndirect` (`VICTORY_INDIRECT_CLAMP=0` disables it).
pub(crate) fn clamp_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| parse_clamp_enabled(std::env::var_os("VICTORY_INDIRECT_CLAMP").as_deref()))
}

/// Validates record alignment and returns the scratch byte size `stride * records`.
pub(crate) fn validate_layout(
    (kind, stride, records): (u32, u32, u32),
    src_offset: u64,
    count_offset: Option<u64>,
) -> Option<u64> {
    let min = min_stride(kind)?;
    if stride < min || !stride.is_multiple_of(4) || !src_offset.is_multiple_of(4) {
        return None;
    }
    if count_offset.is_some_and(|off| !off.is_multiple_of(4)) {
        return None;
    }
    let bytes = u64::from(stride).checked_mul(u64::from(records))?;
    (bytes <= u64::from(u32::MAX)).then_some(bytes)
}

/// Records, in `list`, the non-destructive clamp of up to `records` records of
/// `kind` (PC argument numbering) `stride` bytes apart from `src` (and bounded
/// by the 32-bit count in `count` when given) into a device-local scratch
/// buffer, at offset `0`. The scratch buffer is left in `INDIRECT_ARGUMENT`
/// until the next clamp into it: executions read it with no barrier of
/// their own. Replaces the list's compute root signature and pipeline
/// (callers bind theirs after). `None` if the clamp or alignment is not
/// available. `fill`: the records from the count on are written empty, for
/// a command run without the count buffer ([`no_count`]).
pub(crate) fn bound(
    list: &HostList,
    src: (&ID3D12Resource, u64),
    count: Option<(&ID3D12Resource, u64)>,
    spec: (u32, u32, u32),
    fill: bool,
) -> Option<ID3D12Resource> {
    let bytes = validate_layout(spec, src.1, count.map(|(_, off)| off))?;
    let (c, budget, dst) = (clamp()?, list.budget()?, list.indirect_scratch(bytes)?);
    clamp_into(list, c, &dst, budget, src, count, spec, 0).then_some(dst)
}

/// Ensures earlier compute writes to indirect argument/count buffers are
/// visible before a direct (unclamped) `ExecuteIndirect`, without running
/// the clamp compute shader or invalidating compute pipeline state.
pub(crate) fn wait_direct(list: &HostList) {
    // SAFETY: an open command list; global UAV barrier has no resource hazards.
    unsafe { list.list.ResourceBarrier(&[uav_barrier()]) };
}

/// [`bound`] for a window of records clamped ahead of the draws that will
/// read them (`crate::draw`'s windows; no count buffer). The records past
/// the ones the title filled are whatever the buffer holds, and each
/// record clamped charges the list's budget when it is clamped: a slot of
/// all ones, clamped to the most a mesh dispatch runs (65,535 groups),
/// charged a sixteenth of the budget, so sixteen of them emptied every
/// draw of the list after them (run 93: "the largest group count asked for
/// 4294967295", 43 to 153 such records a frame). Here a record that asks
/// for more than can run is left empty and charges nothing; a record
/// clamped alone is clamped and charged as before.
pub(crate) fn bound_ahead(
    list: &HostList,
    src: (&ID3D12Resource, u64),
    spec: (u32, u32, u32),
) -> Option<ID3D12Resource> {
    let bytes = validate_layout(spec, src.1, None)?;
    let (c, budget, dst) = (clamp()?, list.budget()?, list.indirect_scratch(bytes)?);
    clamp_into(list, c, &dst, budget, src, None, spec, AHEAD).then_some(dst)
}

/// The parts [`bound_split`] makes of a dispatch at most, the most groups of
/// each, and the bytes of one. The host runs 65,535 groups in one dimension;
/// the console runs more. The one the title is known to ask for: 115,200 in
/// x (its final gather of indirect light, 8 groups for each 16x16 tile of
/// 2560x1440), which is two parts; four parts (262,140 groups) leave room
/// for a 3840x2160 gather (259,200). Mirrored by `SPLIT` in the clamp's
/// source. A dispatch of more groups than all the parts hold keeps what they
/// hold (clamped to the first part when the clamp's own limits are on: it
/// is then more likely the garbage of arguments nothing wrote).
pub(crate) const SPLIT: u32 = 4;
pub(crate) const PART: u32 = 65_535;
pub(crate) const PART_BYTES: u64 = 12;

/// The bytes between a dispatch's records in [`bound_split`]'s buffer, which
/// is the stride of the command signature that runs them.
pub(crate) const SPLIT_STRIDE: u32 = SPLIT * PART_BYTES as u32;

/// The parts a direct dispatch of `x` groups in one dimension is run in:
/// each the number of its first group and its groups, at most [`PART`] of
/// them (none for no groups). Any number of parts: a direct dispatch's
/// count is known here.
pub(crate) fn dispatch_parts(x: u32) -> impl Iterator<Item = (u32, u32)> {
    (0..x.div_ceil(PART)).map(move |part| {
        let base = part * PART;
        (base, (x - base).min(PART))
    })
}

/// [`bound`] for the dispatches of a program that takes the number of its
/// first group (`victory_shader::emit::USER_GROUP_BASE`): every source
/// record is made [`SPLIT`] records that follow each other in the scratch
/// buffer, of which the first has the first [`PART`] groups in x and the
/// next the ones after (empty if there are none). The caller runs part `n`
/// with `ExecuteIndirect` at byte `n * PART_BYTES` of the buffer, records
/// [`SPLIT_STRIDE`] apart, after setting the program's group base to
/// `n * PART`. `limits`: the clamp's own (a dispatch past what the parts
/// hold, or more than one dimension past what one dispatch runs, is cut or
/// emptied, and the list's budget is charged); without them
/// (`VICTORY_INDIRECT_CLAMP=0`) the groups are as the title asked, in parts.
pub(crate) fn bound_split(
    list: &HostList,
    src: (&ID3D12Resource, u64),
    count: Option<(&ID3D12Resource, u64)>,
    (stride, records): (u32, u32),
    limits: bool,
) -> Option<ID3D12Resource> {
    let spec = (2, stride, records);
    validate_layout(spec, src.1, count.map(|(_, off)| off))?;
    let bytes = u64::from(SPLIT_STRIDE).checked_mul(u64::from(records))?;
    let (c, budget, dst) = (clamp()?, list.budget()?, list.indirect_scratch(bytes)?);
    let how = IN_PARTS | if limits { 0 } else { UNCLAMPED };
    clamp_into(list, c, &dst, budget, src, count, spec, how).then_some(dst)
}

/// Bits of the clamp's `has_count` besides the count's own (bit 0).
const AHEAD: u32 = 2;
const IN_PARTS: u32 = 4;
const UNCLAMPED: u32 = 8;

/// [`record_clamp_as`] into `list`'s scratch buffer `dst`, from the state
/// the list left it in; afterwards the list knows it is readable as
/// indirect arguments.
#[allow(clippy::too_many_arguments)]
fn clamp_into(
    list: &HostList,
    c: &Clamp,
    dst: &ID3D12Resource,
    budget: u64,
    src: (&ID3D12Resource, u64),
    count: Option<(&ID3D12Resource, u64)>,
    spec: (u32, u32, u32),
    how: u32,
) -> bool {
    let before = list.indirect_state(dst);
    let recorded = record_clamp_as(c, &list.list, (dst, before), budget, src, count, spec, how);
    if recorded {
        list.indirect_readable(dst);
    }
    recorded
}

/// Records the clamp into `dst`, which is in state `before` (common, or
/// indirect arguments after an earlier clamp into it), and leaves `dst` in
/// `INDIRECT_ARGUMENT`. `how`: [`AHEAD`], the records are a window clamped
/// ahead of their draws ([`bound_ahead`]); [`IN_PARTS`], dispatches in parts
/// ([`bound_split`], with [`UNCLAMPED`] if without the clamp's limits).
/// False, with nothing recorded, if the layout or the addresses do not fit
/// the clamp.
#[allow(clippy::too_many_arguments)]
fn record_clamp_as(
    c: &Clamp,
    l: &ID3D12GraphicsCommandList,
    dst: (&ID3D12Resource, D3D12_RESOURCE_STATES),
    budget: u64,
    src: (&ID3D12Resource, u64),
    count: Option<(&ID3D12Resource, u64)>,
    spec: (u32, u32, u32),
    ahead: bool,
) -> bool {
    record_clamp_filled(c, l, dst, budget, src, count, spec, (ahead, false))
}

/// [`record_clamp_as`]; with `fill`, the records from the count to
/// `records` are written empty (all zeros, charging nothing), for a command
/// run without the count buffer ([`no_count`]).
#[allow(clippy::too_many_arguments)]
fn record_clamp_filled(
    c: &Clamp,
    l: &ID3D12GraphicsCommandList,
    (dst, before): (&ID3D12Resource, D3D12_RESOURCE_STATES),
    budget: u64,
    (src, src_offset): (&ID3D12Resource, u64),
    count: Option<(&ID3D12Resource, u64)>,
    (kind, stride, records): (u32, u32, u32),
    how: u32,
) -> bool {
    let Some(bytes) = validate_layout(
        (kind, stride, records),
        src_offset,
        count.map(|(_, off)| off),
    ) else {
        return false;
    };
    // SAFETY: live D3D12 buffer resources.
    let (src_base, dst_gpu) = unsafe { (src.GetGPUVirtualAddress(), dst.GetGPUVirtualAddress()) };
    if !src_base.is_multiple_of(4) || !dst_gpu.is_multiple_of(4) || !budget.is_multiple_of(4) {
        return false;
    }
    let (src_gpu, args_offset) = match src_offset
        .checked_add(bytes)
        .filter(|&end| end <= u64::from(u32::MAX))
    {
        Some(_) => (src_base, src_offset as u32),
        None => {
            let Some(rebased) = src_base.checked_add(src_offset) else {
                return false;
            };
            (rebased, 0)
        }
    };
    let (count_gpu, has_count, count_offset) = match count {
        Some((buf, off)) => {
            // SAFETY: a live D3D12 buffer resource.
            let base = unsafe { buf.GetGPUVirtualAddress() };
            if !base.is_multiple_of(4) {
                return false;
            }
            match off.checked_add(4).filter(|&end| end <= u64::from(u32::MAX)) {
                Some(_) => (base, 1u32, off as u32),
                None => {
                    let Some(rebased) = base.checked_add(off) else {
                        return false;
                    };
                    (rebased, 1u32, 0u32)
                }
            }
        }
        None => (src_gpu, 0u32, 0u32),
    };
    let constants = [kind, stride, records, args_offset, has_count | how, count_offset];
    // SAFETY: an open list; our root signature and pipeline; live source,
    // count, scratch destination, and budget buffers. `src` and `count` stay
    // in `COMMON` / `GENERIC_READ` via implicit buffer promotion while `dst`
    // transitions `before -> UNORDERED_ACCESS -> INDIRECT_ARGUMENT`. Moving
    // it out of `INDIRECT_ARGUMENT` orders this clamp's writes after every
    // earlier execution's reads of the scratch buffer; nothing else writes it.
    // Diagnostics: counted; `VICTORY_BARRIER_TEST=indirect` leaves the two
    // waits for every earlier shader write out (the state changes stay).
    #[cfg(feature = "diagnostics")]
    crate::copystat::add(crate::copystat::C::IndirectBounded, 1);
    #[cfg(feature = "diagnostics")]
    let waits = crate::copystat::barrier_test() & 2 == 0;
    #[cfg(not(feature = "diagnostics"))]
    let waits = true;
    let first = [
        uav_barrier(),
        transition(dst, before, D3D12_RESOURCE_STATE_UNORDERED_ACCESS),
    ];
    let second = [
        uav_barrier(),
        transition(
            dst,
            D3D12_RESOURCE_STATE_UNORDERED_ACCESS,
            D3D12_RESOURCE_STATE_INDIRECT_ARGUMENT,
        ),
    ];
    let skip = usize::from(!waits);
    unsafe {
        // Earlier shader writes (the title's argument and count passes) first.
        l.ResourceBarrier(&first[skip..]);
        if records > 0 {
            l.SetComputeRootSignature(&c.root);
            l.SetPipelineState(&c.pso);
            l.SetComputeRoot32BitConstants(0, 6, constants.as_ptr().cast(), 0);
            l.SetComputeRootShaderResourceView(1, src_gpu);
            l.SetComputeRootShaderResourceView(2, count_gpu);
            l.SetComputeRootUnorderedAccessView(3, dst_gpu);
            l.SetComputeRootUnorderedAccessView(4, budget);
            l.Dispatch(records.div_ceil(64), 1, 1);
        }
        l.ResourceBarrier(&second[skip..]);
    }
    true
}

/// Whether every draw served from a window is checked against a fresh
/// clamp of its record (`VICTORY_INDIRECT_WINDOW_CHECK=1`, with
/// `VICTORY_INDIRECT_WINDOW=1`; off by default). A window clamps its
/// records once, ahead of their draws; if anything wrote a record between
/// the window's clamp and its draw, the draw reads the old one. The check
/// clamps the record again just before the draw (into a buffer of its own,
/// charging a budget of its own) and compares the two on the card: the
/// counts are logged ("window check"), and the first record that differs in
/// each run of a list is named. For telling whether windows can be trusted
/// in a scene before they are on by default; it costs a clamp and a compare
/// per window draw.
pub(crate) fn window_check() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        std::env::var_os("VICTORY_INDIRECT_WINDOW_CHECK").is_some_and(|v| v == "1")
    })
}

/// Whether clamps of indirect draws are recorded in a list of their own
/// before the list that draws, so that a run of draws is not interleaved
/// with our dispatches (`VICTORY_HOIST_CLAMPS=0`: each before its draw, as
/// before). On by default, where waits for memory values cut lists
/// (`crate::queue::cuts`): the lists are made from those parts.
///
/// A run of indirect draws had a dispatch of ours and three barriers between
/// each two draws: 9,300 a frame in the world, 190 of 225 ms of GPU time
/// (measured by the fork this is ported from: 225 to 72 ms a frame).
pub(crate) fn hoisting() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("VICTORY_HOIST_CLAMPS").is_none_or(|v| v != "0"))
        && crate::queue::cuts()
}

/// As [`bound`], recorded in `clamps`, a list that runs before the one that
/// executes: the buffer and the offset of the clamped records, readable as
/// indirect arguments once `clamps` ended ([`HostList::clamp_end`]).
/// `fill` as for [`bound`].
pub(crate) fn bound_before(
    clamps: &HostList,
    (src, src_offset): (&ID3D12Resource, u64),
    count: Option<(&ID3D12Resource, u64)>,
    (kind, stride, records): (u32, u32, u32),
    fill: bool,
) -> Option<(ID3D12Resource, u64)> {
    let bytes = validate_layout((kind, stride, records), src_offset, count.map(|(_, off)| off))?;
    if records == 0 {
        return None;
    }
    let (c, budget) = (clamp()?, clamps.budget()?);
    // SAFETY: live buffers.
    let (src_base, count_base) =
        unsafe { (src.GetGPUVirtualAddress(), count.map(|(b, _)| b.GetGPUVirtualAddress())) };
    // Addresses the shader's offsets (32 bits) reach, as in `record_clamp`.
    let fits = |offset: u64, len: u64| offset.checked_add(len).is_some_and(|end| end <= u64::from(u32::MAX));
    let (src_gpu, args_offset) = if fits(src_offset, bytes) {
        (src_base, src_offset as u32)
    } else {
        (src_base.checked_add(src_offset)?, 0)
    };
    let (count_gpu, has_count, count_offset) = match (count, count_base) {
        (Some((_, off)), Some(base)) if fits(off, 4) => (base, 1u32, off as u32),
        (Some((_, off)), Some(base)) => (base.checked_add(off)?, 1, 0),
        _ => (src_gpu, 0, 0),
    };
    if !src_gpu.is_multiple_of(4) || !count_gpu.is_multiple_of(4) || !budget.is_multiple_of(4) {
        return None;
    }
    let (dst, at) = clamps.clamp_room(bytes)?;
    // SAFETY: a live buffer.
    let dst_base = unsafe { dst.GetGPUVirtualAddress() };
    let dst_gpu = dst_base + at;
    let fill = fill && has_count != 0;
    let constants = [kind, stride, records, args_offset, has_count | (u32::from(fill) << 2), count_offset];
    #[cfg(feature = "diagnostics")]
    crate::copystat::add(crate::copystat::C::IndirectBounded, 1);
    // Batched (`VICTORY_CLAMP_BATCH=1`): kept for its batch's pass, which
    // writes the same records to the same place, charged in the same order,
    // before the list's clamps end.
    {
        let mut batch = clamps.clamp_batch();
        if *batch.on.get_or_insert_with(|| batch_switch() && batch_pass().is_some()) {
            let kept = Kept {
                spec: (kind, stride, records),
                args: (src_gpu, args_offset),
                count: (has_count != 0).then_some((count_gpu, count_offset)),
                fill,
                out: (dst_base, at as u32),
            };
            if !batch.take(kept, budget) {
                flush_held(clamps, &mut batch);
                if !batch.take(kept, budget) {
                    // shortcut: does not happen (an empty batch takes any
                    // clamp `clamp_room` has room for); clamped alone, in
                    // its place.
                    record_hoisted(c, &clamps.list, constants, (src_gpu, count_gpu, dst_gpu, budget), true);
                }
            }
            return Some((dst, at));
        }
    }
    // Measurement only (`VICTORY_CLAMP_NO_WAIT=1`, live, off by default):
    // the wait between clamps left out, to learn what the chain costs the
    // card. The budget is then charged out of order and draws may flicker.
    record_hoisted(c, &clamps.list, constants, (src_gpu, count_gpu, dst_gpu, budget), !clamp_no_wait());
    Some((dst, at))
}

/// Records one hoisted clamp as [`bound_before`] binds it: `constants`
/// ([`SOURCE`]'s), the argument and count views, where its records go and
/// the list's budget; then, if `wait`, the wait for its writes, which
/// orders it before the next clamp.
fn record_hoisted(
    c: &Clamp,
    l: &ID3D12GraphicsCommandList,
    constants: [u32; 6],
    (src_gpu, count_gpu, dst_gpu, budget): (u64, u64, u64, u64),
    wait: bool,
) {
    // SAFETY: an open list; our root signature and pipeline; live source,
    // count, destination (writable: `clamp_room`) and budget buffers. The
    // clamps write apart; one after another (a barrier between them), so
    // that the budget is charged in the order of the draws as it is when
    // each clamp precedes its draw: without it whole mesh objects flickered,
    // as records past the list's budget were not the same from frame to
    // frame.
    unsafe {
        l.SetComputeRootSignature(&c.root);
        l.SetPipelineState(&c.pso);
        l.SetComputeRoot32BitConstants(0, 6, constants.as_ptr().cast(), 0);
        l.SetComputeRootShaderResourceView(1, src_gpu);
        l.SetComputeRootShaderResourceView(2, count_gpu);
        l.SetComputeRootUnorderedAccessView(3, dst_gpu);
        l.SetComputeRootUnorderedAccessView(4, budget);
        l.Dispatch(constants[2].div_ceil(64), 1, 1);
        if wait {
            l.ResourceBarrier(&[uav_barrier()]);
        }
    }
}

/// [`bound_before`] without the wait between clamps (measurement only).
fn clamp_no_wait() -> bool {
    static ON: crate::live::Switch = crate::live::Switch::new("CLAMP_NO_WAIT", 0);
    ON.on()
}

/// Most records of an indirect command that [`no_count`] runs without the
/// title's count buffer: each record from the count on is clamped to an
/// empty one, so the clamp writes them all.
pub(crate) const NO_COUNT_MOST: u32 = 16;

/// Whether indirect commands of up to [`NO_COUNT_MOST`] records that the
/// title bounds with a count buffer run without it
/// (`VICTORY_INDIRECT_NO_COUNT=1`, live, off by default): their clamp
/// ([`bound`], [`bound_before`] one by one or batched) writes the records
/// from the count to the most as empty ones (all zeros: no vertices, no
/// instances, no groups), charging nothing, and the command reads all of
/// them with no count buffer. The records under the count are clamped as
/// before. On the host card one dispatch with a count buffer took 1.4 to
/// 1.55 us a call, without one 0.05 to 0.25 us (`boundary_tests`), and the
/// title issues some 5,000 indirect commands a frame. Read once a command
/// (`crate::draw`), for its clamp and its execution alike.
pub(crate) fn no_count() -> bool {
    static ON: crate::live::Switch = crate::live::Switch::new("INDIRECT_NO_COUNT", 0);
    ON.on()
}

/// Whether hoisted clamps are batched (`VICTORY_CLAMP_BATCH=1`, live, off
/// by default; read once a recording, at its first hoisted clamp). The
/// clamps of a list of hoisted clamps are kept as they come
/// ([`bound_before`]) and recorded as one pass ([`BATCH_SOURCE`]) when the
/// list's clamps end ([`HostList::clamp_end`]), in place of a dispatch and
/// a wait for every clamp: about 4,100 a frame in the world, where leaving
/// out only the waits (`VICTORY_CLAMP_NO_WAIT`) took 74.0 to 69.5 ms a
/// frame, and whole objects flickered then, as the budget was charged out
/// of order. The pass charges it in the chain's order.
fn batch_switch() -> bool {
    static ON: crate::live::Switch = crate::live::Switch::new("CLAMP_BATCH", 0);
    ON.on()
}

/// Most buffers (argument and count buffers, by the address their view
/// starts at) one batch pass reads: [`BATCH_SOURCE`]'s `s0` to `s15`. A
/// clamp whose buffers do not fit with the batch's starts the next batch.
const VIEWS: usize = 16;
/// Most clamps and records one pass takes, so that its tables (32 bytes a
/// clamp, 4 a record: 512 KiB at most) fit in a chunk of the list's
/// scratch memory. A clamp has at most 21,845 records (`clamp_room` gives
/// 256 KiB at most).
const BATCH_CLAMPS: usize = 8192;
const BATCH_RECORDS: u32 = 1 << 16;

/// The pass of a batch of hoisted clamps ([`batch_switch`]): the clamps of
/// [`SOURCE`] one after another, as [`bound_before`] records them, in one
/// dispatch of one group of 1,024 threads. The group walks the batch's
/// records in the chain's order, a tile of 1,024 at a time; each thread
/// clamps one record exactly as [`SOURCE`] does (same bounds, same empty
/// records, same notes on mesh records); the tile's costs are added up in
/// order, saturating at the budget (for costs of 0 or more, `min(a + b,
/// BUDGET)` is associative, so a prefix sum gives what charging record
/// after record gives); a record that does not fit runs empty and takes
/// the rest, as there; what the tile charged carries over to the next.
/// The budget word is read once and written once: nothing else charges it
/// while the pass runs (a wait stands before and after it). No group waits
/// for another: there is one.
///
/// Across groups of one clamp, the chain's charge is in the order the card
/// runs them; the pass's is the records' order (one of those orders, the
/// only one when a clamp is one group).
///
/// The records are read where each clamp would read them, through root
/// views of the argument and count buffers (`s0`-`s15`, chosen per clamp by
/// the table), with no copies and no descriptors: nothing in the list
/// writes those buffers between its clamps (title memory is brought up to
/// date when the list is submitted, before it runs), so reading them at
/// the pass reads what each clamp read. Tables (upload memory): eight words a
/// clamp (kind | argument view << 8 | count view << 16 | counted << 24 |
/// records past the count written empty << 25 (as `SOURCE`'s bit 2),
/// stride, records, argument offset, count offset, where its records go in
/// `dst_args`, its first record in the batch, 0), then a word a record (its
/// clamp).
const BATCH_SOURCE: &str = r#"
#define RS "RootConstants(num32BitConstants=4, b0), SRV(t0), UAV(u0), UAV(u1)$VIEWS"
#define TILE 1024
cbuffer Batch : register(b0) {
    uint clamps;
    uint records;
    // Where the word a record starts in `table` (bytes).
    uint record_at;
    uint unused;
};
ByteAddressBuffer table : register(t0);
RWByteAddressBuffer dst_args : register(u0);
RWByteAddressBuffer budget : register(u1);
$DECLS
static const uint BUDGET = 1u << 27;
static const uint MOST_DRAW = 1u << 23;
static const uint GROUPS = 1u << 20;
// A word of view `view`; only views the table names are read, only where a
// clamp of the chain would read.
uint load1(uint view, uint at) {
    uint v = 0;
    [forcecase] switch (view) {
$LOAD1    }
    return v;
}
uint3 load3(uint view, uint at) {
    uint3 v = uint3(0, 0, 0);
    [forcecase] switch (view) {
$LOAD3    }
    return v;
}
groupshared uint scan[TILE];
groupshared uint carry;
groupshared uint largest_asked;
groupshared uint all_asked;
[RootSignature(RS)]
[numthreads(TILE, 1, 1)]
void main(uint gi : SV_GroupIndex) {
    if (gi == 0) {
        carry = budget.Load(0);
        largest_asked = 0;
        all_asked = 0;
    }
    GroupMemoryBarrierWithGroupSync();
    for (uint first = 0; first < records; first += TILE) {
        uint r = first + gi;
        bool active = false;
        bool empty = false;
        uint kind = 0;
        uint view = 0;
        uint stride = 0;
        uint src_at = 0;
        uint dst_at = 0;
        [branch] if (r < records) {
            uint c = table.Load(record_at + 4 * r);
            uint4 a = table.Load4(32 * c);
            uint4 b = table.Load4(32 * c + 16);
            kind = a.x & 0xffu;
            view = (a.x >> 8) & 0xffu;
            stride = a.y;
            uint eff_count = a.z;
            [branch] if (((a.x >> 24) & 1u) != 0) {
                eff_count = min(eff_count, load1((a.x >> 16) & 0xffu, b.x));
            }
            uint k = r - b.z;
            // Records at or past the count: no cost, nothing stored, or an
            // empty record (bit 25).
            active = k < eff_count;
            empty = !active && ((a.x >> 25) & 1u) != 0;
            src_at = a.w + k * stride;
            dst_at = b.y + k * stride;
        }
        uint cost = 0;
        uint asked = 0;
        uint3 counts = uint3(0, 0, 0);
        [branch] if (active) {
            // Every kind's record is 12 bytes or more.
            uint3 want = load3(view, src_at);
            if (kind <= 1) {
                // As SOURCE's draws (never ahead here).
                uint count = min(want.x, 1u << 24);
                uint instances = min(want.y, (1u << 26) / max(count, 1u));
                cost = count * instances;
                if (count >= MOST_DRAW) {
                    count = 0;
                    instances = 0;
                    cost = 0;
                }
                counts = uint3(count, instances, 0);
            } else if (kind == 2) {
                uint3 g = min(want, uint3(65535u, 65535u, 65535u));
                if (g.z != 0 && g.x * g.y > GROUPS / g.z) {
                    g = uint3(0, 0, 0);
                }
                cost = g.x * g.y * g.z * 64u;
                counts = g;
            } else {
                asked = want.x;
                uint3 g = uint3(min(want.x, 65535u), min(want.y, 1u), min(want.z, 1u));
                cost = g.x * g.y * g.z * 128u;
                counts = g;
            }
        }
        scan[gi] = cost;
        uint start = carry;
        GroupMemoryBarrierWithGroupSync();
        if (active && kind > 2) {
            uint was;
            InterlockedMax(largest_asked, asked, was);
            InterlockedAdd(all_asked, min(asked, 1u << 24), was);
        }
        // What the tile's records charge up to each, saturating.
        for (uint d = 1; d < TILE; d <<= 1) {
            uint v = scan[gi];
            uint w = gi >= d ? scan[gi - d] : 0u;
            GroupMemoryBarrierWithGroupSync();
            scan[gi] = min(v + w, BUDGET);
            GroupMemoryBarrierWithGroupSync();
        }
        // The budget before this record, as SOURCE's walk has it.
        uint at = start >= BUDGET ? start : min(start + (gi > 0 ? scan[gi - 1] : 0u), BUDGET);
        bool fits = cost == 0 || (at < BUDGET && BUDGET - at >= cost);
        uint total = scan[TILE - 1];
        [branch] if (active) {
            if (!fits) {
                counts = uint3(0, 0, 0);
            }
            if (kind <= 1) {
                dst_args.Store2(dst_at, counts.xy);
                for (uint off = 8; off < stride; off += 4) {
                    dst_args.Store(dst_at + off, load1(view, src_at + off));
                }
            } else {
                dst_args.Store3(dst_at, counts);
                for (uint off = 12; off < stride; off += 4) {
                    dst_args.Store(dst_at + off, load1(view, src_at + off));
                }
            }
        }
        [branch] if (empty) {
            for (uint off = 0; off < stride; off += 4) {
                dst_args.Store(dst_at + off, 0u);
            }
        }
        GroupMemoryBarrierWithGroupSync();
        if (gi == 0) {
            carry = start >= BUDGET ? start : min(start + total, BUDGET);
        }
        GroupMemoryBarrierWithGroupSync();
    }
    if (gi == 0) {
        uint was;
        budget.Store(0, carry);
        budget.InterlockedMax(4, largest_asked, was);
        budget.InterlockedAdd(8, all_asked, was);
    }
}
"#;

/// [`BATCH_SOURCE`] with its [`VIEWS`] views.
fn batch_source() -> String {
    let (mut views, mut decls, mut load1, mut load3) = (String::new(), String::new(), String::new(), String::new());
    for v in 0..VIEWS {
        let case = if v + 1 == VIEWS { "default".to_string() } else { format!("case {v}") };
        views += &format!(", SRV(t{})", v + 1);
        decls += &format!("ByteAddressBuffer s{v} : register(t{});\n", v + 1);
        load1 += &format!("        {case}: v = s{v}.Load(at); break;\n");
        load3 += &format!("        {case}: v = s{v}.Load3(at); break;\n");
    }
    BATCH_SOURCE
        .replace("$VIEWS", &views)
        .replace("$DECLS", &decls)
        .replace("$LOAD1", &load1)
        .replace("$LOAD3", &load3)
}

/// The pass of [`batch_switch`], made at its first use.
fn batch_pass() -> Option<&'static Clamp> {
    static PASS: std::sync::OnceLock<Option<Clamp>> = std::sync::OnceLock::new();
    PASS.get_or_init(|| {
        let device = &host()?.device;
        match make_from(device, &batch_source()) {
            Ok(c) => Some(c),
            Err(e) => {
                tracing::warn!(error = %e, "no batch pass: hoisted clamps are clamped one by one");
                None
            }
        }
    })
    .as_ref()
}

/// A hoisted clamp kept for its batch's pass: its layout (kind, stride,
/// records), its argument records and its count as a view's address and an
/// offset from it (as [`record_hoisted`] binds them), whether the records
/// past the count are written empty ([`no_count`]), and where its clamped
/// records go (a buffer's address and the offset in it).
#[derive(Clone, Copy, Debug, PartialEq)]
struct Kept {
    spec: (u32, u32, u32),
    args: (u64, u32),
    count: Option<(u64, u32)>,
    fill: bool,
    out: (u64, u32),
}

impl Kept {
    /// [`SOURCE`]'s constants for this clamp alone.
    fn constants(&self) -> [u32; 6] {
        let (kind, stride, records) = self.spec;
        let (counted, count_offset) = self.count.map_or((0, 0), |(_, off)| (1, off));
        let mode = counted | (u32::from(self.fill && counted != 0) << 2);
        [kind, stride, records, self.args.1, mode, count_offset]
    }
}

/// The hoisted clamps of a list kept for one pass ([`batch_switch`]), in
/// the order of their draws: all write to one buffer and charge one
/// budget, through at most [`VIEWS`] views.
#[derive(Default)]
pub(crate) struct Batch {
    /// Whether this recording's hoisted clamps are batched: the switch as
    /// its first hoisted clamp read it, until the list is reset.
    on: Option<bool>,
    kept: Vec<Kept>,
    views: Vec<u64>,
    records: u32,
    budget: u64,
}

impl Batch {
    /// Takes `k` if it fits with what the batch holds: the same buffer for
    /// the records and the same budget, room in the tables, views for its
    /// buffers. False, with nothing taken, if not.
    fn take(&mut self, k: Kept, budget: u64) -> bool {
        let joins = self.kept.first().is_none_or(|first| first.out.0 == k.out.0 && self.budget == budget);
        if !joins || self.kept.len() >= BATCH_CLAMPS || self.records + k.spec.2 > BATCH_RECORDS {
            return false;
        }
        let wanted = [Some(k.args.0), k.count.map(|(at, _)| at).filter(|&at| at != k.args.0)];
        let new = wanted.iter().flatten().filter(|at| !self.views.contains(at)).count();
        if self.views.len() + new > VIEWS {
            return false;
        }
        for at in wanted.into_iter().flatten() {
            if !self.views.contains(&at) {
                self.views.push(at);
            }
        }
        self.kept.push(k);
        self.records += k.spec.2;
        self.budget = budget;
        true
    }

    fn view(&self, at: u64) -> u32 {
        self.views.iter().position(|&v| v == at).unwrap_or(0) as u32
    }

    /// The bytes of the pass's tables.
    fn table_bytes(&self) -> u64 {
        (self.kept.len() * 32 + self.records as usize * 4) as u64
    }

    /// The pass's tables ([`BATCH_SOURCE`]).
    fn table(&self) -> Vec<u32> {
        let mut words = Vec::with_capacity(self.kept.len() * 8 + self.records as usize);
        let mut first = 0;
        for k in &self.kept {
            let (kind, stride, records) = k.spec;
            let (counted, count_view, count_offset) = match k.count {
                Some((at, off)) => (1, self.view(at), off),
                None => (0, 0, 0),
            };
            let fill = u32::from(k.fill) & counted;
            let head = kind | (self.view(k.args.0) << 8) | (count_view << 16) | (counted << 24) | (fill << 25);
            words.extend_from_slice(&[head, stride, records, k.args.1, count_offset, k.out.1, first, 0]);
            first += records;
        }
        for (i, k) in self.kept.iter().enumerate() {
            words.extend(std::iter::repeat_n(i as u32, k.spec.2 as usize));
        }
        words
    }

    /// Records what the batch holds into `l` and empties it: its pass
    /// (`pass`, its tables written to `table`, upload or mapped memory of
    /// [`Batch::table_bytes`]), followed by a wait for its writes; or,
    /// without both, each clamp alone as [`bound_before`] records it, with
    /// `chain`. True for the pass.
    fn record(
        &mut self,
        l: &ID3D12GraphicsCommandList,
        pass: Option<&Clamp>,
        chain: &Clamp,
        table: Option<(u64, *mut u8)>,
    ) -> bool {
        if self.kept.is_empty() {
            return false;
        }
        let one = match (pass, table) {
            (Some(pass), Some((gpu, data))) if gpu.is_multiple_of(16) => {
                let words = self.table();
                // SAFETY: the caller's memory holds the tables.
                unsafe { std::ptr::copy_nonoverlapping(words.as_ptr().cast::<u8>(), data, 4 * words.len()) };
                let constants = [self.kept.len() as u32, self.records, self.kept.len() as u32 * 32, 0];
                // SAFETY: an open list; the pass's own root signature and
                // pipeline; the tables, the clamps' argument and count
                // buffers (read as by the clamps), the buffer of their
                // records (writable: `clamp_room`) and the budget, live.
                unsafe {
                    l.SetComputeRootSignature(&pass.root);
                    l.SetPipelineState(&pass.pso);
                    l.SetComputeRoot32BitConstants(0, 4, constants.as_ptr().cast(), 0);
                    l.SetComputeRootShaderResourceView(1, gpu);
                    l.SetComputeRootUnorderedAccessView(2, self.kept[0].out.0);
                    l.SetComputeRootUnorderedAccessView(3, self.budget);
                    for v in 0..VIEWS {
                        // Views no clamp names are not read: the tables.
                        l.SetComputeRootShaderResourceView(4 + v as u32, self.views.get(v).copied().unwrap_or(gpu));
                    }
                    l.Dispatch(1, 1, 1);
                    l.ResourceBarrier(&[uav_barrier()]);
                }
                true
            }
            _ => {
                for k in &self.kept {
                    let count = k.count.map_or(k.args.0, |(at, _)| at);
                    let dst = k.out.0 + u64::from(k.out.1);
                    record_hoisted(chain, l, k.constants(), (k.args.0, count, dst, self.budget), true);
                }
                false
            }
        };
        self.kept.clear();
        self.views.clear();
        self.records = 0;
        one
    }
}

/// Records the hoisted clamps `list` keeps for a batch, if any: when its
/// clamps end ([`HostList::clamp_end`]), and should it close without that,
/// when it closes.
pub(crate) fn flush_batch(list: &HostList) {
    let mut batch = list.clamp_batch();
    flush_held(list, &mut batch);
}

/// [`Batch::record`] into `list`, the tables in its scratch memory.
fn flush_held(list: &HostList, batch: &mut Batch) {
    if batch.kept.is_empty() {
        return;
    }
    // A clamp is kept only once the clamp is made.
    let Some(chain) = clamp() else {
        return;
    };
    let pass = batch_pass();
    let table = pass.and_then(|_| list.scratch(batch.table_bytes()));
    let (clamps, records, views) = (batch.kept.len(), batch.records, batch.views.len());
    #[cfg(feature = "diagnostics")]
    list.note_op(|| format!("clamp batch of {clamps} hoisted clamps, {records} records"));
    let one = batch.record(&list.list, pass, chain, table);
    static SEEN: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    if SEEN.fetch_add(1, std::sync::atomic::Ordering::Relaxed) < 4 {
        tracing::info!(clamps, records, views, one_pass = one, "hoisted clamps batched (VICTORY_CLAMP_BATCH)");
    }
}

/// The compare of [`window_check`]: `words` dwords of the record a window
/// clamped (`t0`) against the same record clamped just now (`t1`). Counts
/// the checks and the records that differ in `u0`, keeps the first record
/// that differs there (eight dwords of each), and zeroes the fresh clamp's
/// own budget (`u1`) for the next check.
const CHECK_SOURCE: &str = r#"
#define RS "RootConstants(num32BitConstants=1, b0), SRV(t0), SRV(t1), UAV(u0), UAV(u1)"
cbuffer Check : register(b0) {
    uint words;
};
ByteAddressBuffer window_record : register(t0);
ByteAddressBuffer fresh_record : register(t1);
RWByteAddressBuffer results : register(u0);
RWByteAddressBuffer fresh_budget : register(u1);
[RootSignature(RS)]
[numthreads(1, 1, 1)]
void main() {
    bool same = true;
    for (uint i = 0; i < words; i++) {
        if (window_record.Load(4 * i) != fresh_record.Load(4 * i)) {
            same = false;
        }
    }
    uint was;
    results.InterlockedAdd(4, 1, was);
    if (!same) {
        uint before;
        results.InterlockedAdd(0, 1, before);
        if (before == 0) {
            for (uint k = 0; k < min(words, 8u); k++) {
                results.Store(8 + 4 * k, window_record.Load(4 * k));
                results.Store(40 + 4 * k, fresh_record.Load(4 * k));
            }
        }
    }
    fresh_budget.Store3(0, uint3(0, 0, 0));
}
"#;

fn compare() -> Option<&'static Clamp> {
    static COMPARE: std::sync::OnceLock<Option<Clamp>> = std::sync::OnceLock::new();
    COMPARE
        .get_or_init(|| {
            let device = &host()?.device;
            match make_from(device, CHECK_SOURCE) {
                Ok(c) => Some(c),
                Err(e) => {
                    tracing::warn!(error = %e, "no window check");
                    None
                }
            }
        })
        .as_ref()
}

/// A list's buffers for [`window_check`]: the fresh clamps' records and
/// budget (the card's memory), and the results (memory the CPU maps:
/// records that differed, checks, then the first differing record as the
/// window had it and as the fresh clamp made it, eight dwords each).
pub(crate) struct WindowCheck {
    args: ID3D12Resource,
    budget: ID3D12Resource,
    results: ID3D12Resource,
    data: usize,
}

// SAFETY: D3D12 objects are free-threaded; the mapping is read only at
// submission, when the list's last run has finished.
unsafe impl Send for WindowCheck {}
// SAFETY: as above.
unsafe impl Sync for WindowCheck {}

impl WindowCheck {
    pub(crate) fn new() -> Option<WindowCheck> {
        let host = host()?;
        let (results, data) = host.shared_buffer(256)?;
        // SAFETY: the mapping holds 256 bytes; nothing runs yet.
        unsafe { std::ptr::write_bytes(data, 0, 256) };
        Some(WindowCheck {
            args: host.uav_buffer(64 << 10)?,
            budget: host.uav_buffer(256)?,
            results,
            data: data as usize,
        })
    }

    /// What the list's last run found, zeroed for the next: checks, records
    /// that differed, and the first that did (as the window had it, as the
    /// fresh clamp made it). Only between runs of the list.
    pub(crate) fn take(&self) -> (u32, u32, [u32; 8], [u32; 8]) {
        let words = self.data as *mut u32;
        // SAFETY: the list's own mapped words; its last run has finished.
        unsafe {
            let at = |k: usize| words.add(k).read_volatile();
            let window = std::array::from_fn(|k| at(2 + k));
            let fresh = std::array::from_fn(|k| at(10 + k));
            let found = (at(1), at(0), window, fresh);
            std::ptr::write_bytes(words, 0, 18);
            found
        }
    }
}

/// Checks and differing records seen by [`window_check`] so far.
static CHECKED: [std::sync::atomic::AtomicU64; 2] =
    [const { std::sync::atomic::AtomicU64::new(0) }; 2];

/// [`window_check`] for a draw served from a window: the record of `kind`
/// at `src`, `step` bytes long, which the window clamped into `scratch` at
/// `at`, is clamped again into the list's check buffer and compared. Runs
/// compute work of ours, so the caller binds its pipeline again after it.
/// False if nothing was recorded.
pub(crate) fn check_hit(
    list: &HostList,
    src: (&ID3D12Resource, u64),
    (kind, step): (u32, u32),
    scratch: &ID3D12Resource,
    at: u64,
) -> bool {
    const CHECK_USE: usize = usize::MAX - 0x78;
    let (Some(c), Some(cmp), Some(check)) = (clamp(), compare(), list.window_check()) else {
        return false;
    };
    if list.first_use(CHECK_USE) {
        // At this recording's submission: what the list's last run found.
        let data = check.data;
        let words = WindowCheck {
            args: check.args.clone(),
            budget: check.budget.clone(),
            results: check.results.clone(),
            data,
        };
        list.defer_host(move || {
            let (checked, differed, window, fresh) = words.take();
            if checked == 0 {
                return;
            }
            use std::sync::atomic::Ordering::Relaxed;
            let total = CHECKED[0].fetch_add(u64::from(checked), Relaxed) + u64::from(checked);
            let bad = CHECKED[1].fetch_add(u64::from(differed), Relaxed) + u64::from(differed);
            if differed > 0 {
                tracing::warn!(
                    differed,
                    checked,
                    ?window,
                    ?fresh,
                    "window check: a draw served from a window read a record that \
                     differs from a fresh clamp of it"
                );
            }
            if total / 10_000 != (total - u64::from(checked)) / 10_000 {
                tracing::info!(checked = total, differed = bad, "window check");
            }
        });
    }
    // SAFETY: live buffers.
    let (budget, results, fresh, window) = unsafe {
        (
            check.budget.GetGPUVirtualAddress(),
            check.results.GetGPUVirtualAddress(),
            check.args.GetGPUVirtualAddress(),
            scratch.GetGPUVirtualAddress() + at,
        )
    };
    // The record again, by the window's rules, into the check's buffer
    // (common before and after every check).
    let common = D3D12_RESOURCE_STATE_COMMON;
    let spec = (kind, step, 1);
    if !record_clamp_as(c, &list.list, (&check.args, common), budget, src, None, spec, AHEAD) {
        return false;
    }
    let args = D3D12_RESOURCE_STATE_INDIRECT_ARGUMENT;
    let read = D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE;
    let words = step / 4;
    // SAFETY: an open list; the compare's own root signature and pipeline;
    // the scratch buffer is readable as arguments (and stays so), the
    // check's buffer was left so by the clamp above.
    unsafe {
        list.list.ResourceBarrier(&[
            transition(scratch, args, D3D12_RESOURCE_STATES(args.0 | read.0)),
            transition(&check.args, args, read),
        ]);
        list.list.SetComputeRootSignature(&cmp.root);
        list.list.SetPipelineState(&cmp.pso);
        list.list.SetComputeRoot32BitConstants(0, 1, (&raw const words).cast(), 0);
        list.list.SetComputeRootShaderResourceView(1, window);
        list.list.SetComputeRootShaderResourceView(2, fresh);
        list.list.SetComputeRootUnorderedAccessView(3, results);
        list.list.SetComputeRootUnorderedAccessView(4, budget);
        list.list.Dispatch(1, 1, 1);
        list.list.ResourceBarrier(&[
            transition(scratch, D3D12_RESOURCE_STATES(args.0 | read.0), args),
            transition(&check.args, read, common),
        ]);
    }
    true
}

fn uav_barrier() -> D3D12_RESOURCE_BARRIER {
    D3D12_RESOURCE_BARRIER {
        Type: D3D12_RESOURCE_BARRIER_TYPE_UAV,
        Flags: D3D12_RESOURCE_BARRIER_FLAG_NONE,
        Anonymous: D3D12_RESOURCE_BARRIER_0 {
            UAV: std::mem::ManuallyDrop::new(D3D12_RESOURCE_UAV_BARRIER {
                pResource: std::mem::ManuallyDrop::new(None),
            }),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::Graphics::Direct3D::D3D_FEATURE_LEVEL_12_0;
    use windows::Win32::Graphics::Dxgi::Common::DXGI_SAMPLE_DESC;
    use windows::Win32::Graphics::Dxgi::*;
    use windows::Win32::System::Threading::{CreateEventW, INFINITE, WaitForSingleObject};
    use windows::core::Interface;

    const WORKER_CS: &str = r#"
#define RS "UAV(u0)"
RWByteAddressBuffer hits : register(u0);
[RootSignature(RS)]
[numthreads(1, 1, 1)]
void main() {
    uint prev;
    hits.InterlockedAdd(0, 1, prev);
}
"#;

    struct WarpHarness {
        device: ID3D12Device,
        queue: ID3D12CommandQueue,
        allocator: ID3D12CommandAllocator,
        list: ID3D12GraphicsCommandList,
        fence: ID3D12Fence,
        submitted: u64,
        event: HANDLE,
        clamp: Clamp,
        worker: Clamp,
    }

    impl WarpHarness {
        fn new() -> Option<Self> {
            // SAFETY: plain DXGI WARP query and D3D12 test harness setup.
            unsafe {
                let factory: IDXGIFactory4 =
                    CreateDXGIFactory2(DXGI_CREATE_FACTORY_FLAGS(0)).ok()?;
                let adapter: IDXGIAdapter = factory.EnumWarpAdapter().ok()?;
                let desc = adapter.cast::<IDXGIAdapter1>().ok()?.GetDesc1().ok()?;
                if desc.Flags & DXGI_ADAPTER_FLAG_SOFTWARE.0 as u32 == 0 {
                    return None;
                }
                let mut device: Option<ID3D12Device> = None;
                D3D12CreateDevice(&adapter, D3D_FEATURE_LEVEL_12_0, &mut device).ok()?;
                let device = device?;
                let queue: ID3D12CommandQueue = device
                    .CreateCommandQueue(&D3D12_COMMAND_QUEUE_DESC {
                        Type: D3D12_COMMAND_LIST_TYPE_DIRECT,
                        ..Default::default()
                    })
                    .ok()?;
                let allocator: ID3D12CommandAllocator = device
                    .CreateCommandAllocator(D3D12_COMMAND_LIST_TYPE_DIRECT)
                    .ok()?;
                let list: ID3D12GraphicsCommandList = device
                    .CreateCommandList(0, D3D12_COMMAND_LIST_TYPE_DIRECT, &allocator, None)
                    .ok()?;
                let fence: ID3D12Fence = device.CreateFence(0, D3D12_FENCE_FLAG_NONE).ok()?;
                let event = CreateEventW(None, false, false, None).ok()?;
                let clamp = make_for(&device).expect("clamp shader must compile on WARP");
                let worker = compile_cs(&device, WORKER_CS).expect("worker shader must compile");
                Some(Self {
                    device,
                    queue,
                    allocator,
                    list,
                    fence,
                    submitted: 0,
                    event,
                    clamp,
                    worker,
                })
            }
        }

        /// CPU-mapped L0 write-back buffer matching `Shadow`'s heap type.
        fn shared_buffer(&self, size: u64) -> (ID3D12Resource, *mut u8, u64) {
            let heap = D3D12_HEAP_PROPERTIES {
                Type: D3D12_HEAP_TYPE_CUSTOM,
                CPUPageProperty: D3D12_CPU_PAGE_PROPERTY_WRITE_BACK,
                MemoryPoolPreference: D3D12_MEMORY_POOL_L0,
                ..Default::default()
            };
            let desc = D3D12_RESOURCE_DESC {
                Dimension: D3D12_RESOURCE_DIMENSION_BUFFER,
                Width: size.max(4),
                Height: 1,
                DepthOrArraySize: 1,
                MipLevels: 1,
                SampleDesc: DXGI_SAMPLE_DESC {
                    Count: 1,
                    Quality: 0,
                },
                Layout: D3D12_TEXTURE_LAYOUT_ROW_MAJOR,
                Flags: D3D12_RESOURCE_FLAG_ALLOW_UNORDERED_ACCESS,
                ..Default::default()
            };
            let mut buffer: Option<ID3D12Resource> = None;
            let mut data = std::ptr::null_mut();
            // SAFETY: plain buffer creation and persistent CPU mapping.
            unsafe {
                self.device
                    .CreateCommittedResource(
                        &heap,
                        D3D12_HEAP_FLAG_NONE,
                        &desc,
                        D3D12_RESOURCE_STATE_COMMON,
                        None,
                        &mut buffer,
                    )
                    .unwrap();
                let buffer = buffer.unwrap();
                buffer.Map(0, None, Some(&mut data)).unwrap();
                let gpu = buffer.GetGPUVirtualAddress();
                (buffer, data.cast(), gpu)
            }
        }

        /// Device-local `DEFAULT`-heap UAV scratch buffer.
        fn default_uav(&self, size: u64) -> ID3D12Resource {
            let heap = D3D12_HEAP_PROPERTIES {
                Type: D3D12_HEAP_TYPE_DEFAULT,
                ..Default::default()
            };
            let desc = D3D12_RESOURCE_DESC {
                Dimension: D3D12_RESOURCE_DIMENSION_BUFFER,
                Width: size.max(64 << 10),
                Height: 1,
                DepthOrArraySize: 1,
                MipLevels: 1,
                SampleDesc: DXGI_SAMPLE_DESC {
                    Count: 1,
                    Quality: 0,
                },
                Layout: D3D12_TEXTURE_LAYOUT_ROW_MAJOR,
                Flags: D3D12_RESOURCE_FLAG_ALLOW_UNORDERED_ACCESS,
                ..Default::default()
            };
            let mut buffer: Option<ID3D12Resource> = None;
            // SAFETY: plain device-local buffer creation.
            unsafe {
                self.device
                    .CreateCommittedResource(
                        &heap,
                        D3D12_HEAP_FLAG_NONE,
                        &desc,
                        D3D12_RESOURCE_STATE_COMMON,
                        None,
                        &mut buffer,
                    )
                    .unwrap();
            }
            buffer.unwrap()
        }

        fn signature(&self, kind: u32, stride: u32) -> ID3D12CommandSignature {
            let arg = D3D12_INDIRECT_ARGUMENT_DESC {
                Type: D3D12_INDIRECT_ARGUMENT_TYPE(kind as i32),
                ..Default::default()
            };
            let desc = D3D12_COMMAND_SIGNATURE_DESC {
                ByteStride: stride,
                NumArgumentDescs: 1,
                pArgumentDescs: &arg,
                NodeMask: 0,
            };
            let mut sig: Option<ID3D12CommandSignature> = None;
            // SAFETY: valid single-argument command signature description.
            unsafe {
                self.device
                    .CreateCommandSignature(&desc, None::<&ID3D12RootSignature>, &mut sig)
                    .unwrap();
            }
            sig.unwrap()
        }

        fn submit_and_wait(&mut self) {
            self.submitted += 1;
            // SAFETY: closing, executing, waiting on fence, and resetting our list.
            unsafe {
                self.list.Close().unwrap();
                let cmd: ID3D12CommandList = self.list.cast().unwrap();
                self.queue.ExecuteCommandLists(&[Some(cmd)]);
                self.queue.Signal(&self.fence, self.submitted).unwrap();
                self.fence
                    .SetEventOnCompletion(self.submitted, self.event)
                    .unwrap();
                WaitForSingleObject(self.event, INFINITE);
                self.allocator.Reset().unwrap();
                self.list.Reset(&self.allocator, None).unwrap();
            }
        }
    }

    fn compile_cs(device: &ID3D12Device, source: &str) -> windows::core::Result<Clamp> {
        let mut code: Option<ID3DBlob> = None;
        // SAFETY: source and entry names outlive `D3DCompile`.
        unsafe {
            D3DCompile(
                source.as_ptr().cast(),
                source.len(),
                s!("worker"),
                None,
                None,
                s!("main"),
                s!("cs_5_1"),
                0,
                0,
                &mut code,
                None,
            )?;
            let code = code.unwrap();
            let bytes = std::slice::from_raw_parts(
                code.GetBufferPointer().cast::<u8>(),
                code.GetBufferSize(),
            );
            let root: ID3D12RootSignature = device.CreateRootSignature(0, bytes)?;
            let pso = device.CreateComputePipelineState(&D3D12_COMPUTE_PIPELINE_STATE_DESC {
                pRootSignature: std::mem::transmute_copy(&root),
                CS: D3D12_SHADER_BYTECODE {
                    pShaderBytecode: bytes.as_ptr().cast(),
                    BytecodeLength: bytes.len(),
                },
                ..Default::default()
            })?;
            Ok(Clamp { root, pso })
        }
    }

    /// The compare of copies in the card's memory (`crate::host`), on the
    /// software adapter. Told 1 it flags the pieces whose bytes are not
    /// what was hashed last and leaves the hashes: a piece a shader changed
    /// and a later one changed back then looks untouched, which is right
    /// only if the piece was fetched after both. Told 2 (submissions sent
    /// ahead with the relay, whose pieces are fetched flight by flight) it
    /// stores the hash it made: each compare flags what changed since the
    /// compare before it, also a change back.
    #[test]
    fn warp_card_compare_stores_the_hash_it_made_when_told() {
        let Some(mut warp) = WarpHarness::new() else {
            return;
        };
        let compare = compile_cs(&warp.device, crate::host::CARD_COMPARE).expect("the card compare must compile on WARP");
        const PIECE: usize = 64 << 10;
        const PIECES: u32 = 3;
        let size = PIECE * PIECES as usize;
        let (_now, now_ptr, now_gpu) = warp.shared_buffer(size as u64);
        let (_hashes, _, hashes_gpu) = warp.shared_buffer(16 * u64::from(PIECES));
        let (_differs, differs_ptr, differs_gpu) = warp.shared_buffer(64);
        let content: Vec<u8> = (0..size).map(|i| (i * 31 + i / 251) as u8).collect();
        // SAFETY: the mapping holds `size` bytes.
        unsafe { std::ptr::copy_nonoverlapping(content.as_ptr(), now_ptr, size) };
        // One pass over the three pieces; what it left in `differs` (7s
        // where it stored nothing).
        let run = |warp: &mut WarpHarness, mode: u32| -> [u32; 3] {
            // SAFETY: our open list, live buffers, the compare's own root
            // signature; the mappings hold what is read and written.
            unsafe {
                for piece in 0..PIECES as usize {
                    differs_ptr.cast::<u32>().add(piece).write_volatile(7);
                }
                warp.list.SetComputeRootSignature(&compare.root);
                warp.list.SetPipelineState(&compare.pso);
                let constants = [0u32, 0, size as u32, PIECES, 0, mode];
                warp.list.SetComputeRoot32BitConstants(0, 6, constants.as_ptr().cast(), 0);
                warp.list.SetComputeRootShaderResourceView(1, now_gpu);
                warp.list.SetComputeRootUnorderedAccessView(2, hashes_gpu);
                warp.list.SetComputeRootUnorderedAccessView(3, differs_gpu);
                warp.list.Dispatch(PIECES, 1, 1);
            }
            warp.submit_and_wait();
            // SAFETY: as above.
            [0, 1, 2].map(|piece| unsafe { differs_ptr.cast::<u32>().add(piece).read_volatile() })
        };
        let at = PIECE + 1234;
        // SAFETY (each): inside the mapping.
        let set = |value: u8| unsafe { now_ptr.add(at).write_volatile(value) };
        // 0: the hashes are stored, nothing is flagged.
        assert_eq!(run(&mut warp, 0), [7, 7, 7]);
        assert_eq!(run(&mut warp, 1), [0, 0, 0]);
        // A change in the second piece: flagged, and with 1 flagged again
        // (the hash stays the old one).
        set(content[at] ^ 0x40);
        assert_eq!(run(&mut warp, 1), [0, 1, 0]);
        assert_eq!(run(&mut warp, 1), [0, 1, 0]);
        // Changed back: with 1 the piece looks untouched.
        set(content[at]);
        assert_eq!(run(&mut warp, 1), [0, 0, 0]);
        // With 2 the change is flagged once, and the hash is the new one.
        set(content[at] ^ 0x40);
        assert_eq!(run(&mut warp, 2), [0, 1, 0]);
        assert_eq!(run(&mut warp, 2), [0, 0, 0]);
        // Changed back: flagged, where 1 saw nothing.
        set(content[at]);
        assert_eq!(run(&mut warp, 2), [0, 1, 0]);
        assert_eq!(run(&mut warp, 2), [0, 0, 0]);
        // Every lane's slice is hashed: first byte of piece 0 (lane 0),
        // last byte of piece 2 (lane 63), and swapping two 16-byte blocks
        // across lanes in piece 1 must all be detected.
        for &(offset, expected) in &[
            (0usize, [1, 0, 0]),
            (PIECE * 3 - 1, [0, 0, 1]),
            (63 * 16 + 7, [1, 0, 0]),
        ] {
            // SAFETY: offset is inside the mapped buffer.
            unsafe { now_ptr.add(offset).write_volatile(content[offset] ^ 0x01) };
            assert_eq!(run(&mut warp, 1), expected);
            // SAFETY: restore original byte.
            unsafe { now_ptr.add(offset).write_volatile(content[offset]) };
            assert_eq!(run(&mut warp, 1), [0, 0, 0]);
        }
        // Swap lane 0's first 16 bytes with lane 1's first 16 bytes in piece 1.
        // SAFETY: both 16-byte blocks are inside piece 1 of the mapped buffer.
        unsafe {
            std::ptr::copy_nonoverlapping(content[PIECE + 16..PIECE + 32].as_ptr(), now_ptr.add(PIECE), 16);
            std::ptr::copy_nonoverlapping(content[PIECE..PIECE + 16].as_ptr(), now_ptr.add(PIECE + 16), 16);
        }
        assert_eq!(run(&mut warp, 1), [0, 1, 0]);
        // Restore piece 1, then flip the sign bit of the first u32 in both
        // lane 0's and lane 32's final 16-byte loads of piece 0 (offsets
        // 64,512 and 65,024). The asymmetric reduction must not let the pair
        // cancel out.
        // SAFETY: all offsets are inside the mapped buffer.
        unsafe {
            std::ptr::copy_nonoverlapping(content[PIECE..PIECE + 32].as_ptr(), now_ptr.add(PIECE), 32);
            now_ptr.add(64_512 + 3).write_volatile(content[64_512 + 3] ^ 0x80);
            now_ptr.add(65_024 + 3).write_volatile(content[65_024 + 3] ^ 0x80);
        }
        assert_eq!(run(&mut warp, 1), [1, 0, 0]);
    }

    /// The compare by marks (`crate::host::CARD_MARKS`), on the software
    /// adapter: a range's pieces are flagged where the copy's bit (its base
    /// plus the piece) is set, every other bit of the bitmap is left as it
    /// was, the bits taken are cleared (the next compare finds only what was
    /// marked since), and a range starting past the copy's first piece reads
    /// the bits of its own pieces.
    #[test]
    fn warp_card_marks_flag_marked_pieces_and_clear_their_bits() {
        let Some(mut warp) = WarpHarness::new() else {
            return;
        };
        let marks = compile_cs(&warp.device, crate::host::CARD_MARKS).expect("the card marks compare must compile on WARP");
        const WORDS: usize = 8;
        let (_bits, bits_ptr, bits_gpu) = warp.shared_buffer(4 * WORDS as u64);
        let (_differs, differs_ptr, differs_gpu) = warp.shared_buffer(4 * 64);
        let bits = |i: usize| bits_ptr.cast::<u32>().wrapping_add(i);
        let flags = |i: usize| differs_ptr.cast::<u32>().wrapping_add(i);
        // The copy's bits begin at 37 (in word 1); pieces 0, 2, 3 and 40
        // (bit 77, word 2) are marked. Bits of other copies: 36 just below,
        // and all of word 0 and word 7.
        let before: [u32; WORDS] = [
            0xffff_ffff,
            (1 << 5) | (1 << 7) | (1 << 8) | (1 << 4),
            1 << 13,
            0,
            0,
            0,
            0,
            0xffff_ffff,
        ];
        // SAFETY: mappings of these sizes; nothing runs yet.
        unsafe {
            for (i, w) in before.iter().enumerate() {
                bits(i).write_volatile(*w);
            }
            for i in 0..64 {
                flags(i).write_volatile(7);
            }
        }
        let run = |warp: &mut WarpHarness, first: u32, pieces: u32, out_at: u32| {
            // SAFETY: our open list, live buffers, the shader's own root
            // signature; the mappings hold what is read and written.
            unsafe {
                warp.list.SetComputeRootSignature(&marks.root);
                warp.list.SetPipelineState(&marks.pso);
                let constants = [first, pieces, out_at, 37u32];
                warp.list.SetComputeRoot32BitConstants(0, 4, constants.as_ptr().cast(), 0);
                warp.list.SetComputeRootUnorderedAccessView(1, bits_gpu);
                warp.list.SetComputeRootUnorderedAccessView(2, differs_gpu);
                warp.list.Dispatch(pieces.div_ceil(64), 1, 1);
            }
            warp.submit_and_wait();
        };
        // Pieces 0-4 to dwords 0-4, pieces 38-41 to dwords 10-13.
        run(&mut warp, 0, 5, 0);
        run(&mut warp, 38, 4, 40);
        // SAFETY: the work is done; the mappings.
        let (got, left) = unsafe { ((0..14).map(|i| flags(i).read_volatile()).collect::<Vec<_>>(), (0..WORDS).map(|i| bits(i).read_volatile()).collect::<Vec<_>>()) };
        assert_eq!(&got[..5], [1, 0, 1, 1, 0], "pieces 0, 2 and 3 were marked");
        assert_eq!(&got[5..10], [7; 5], "dwords no range was given stay as they were");
        assert_eq!(&got[10..14], [0, 0, 1, 0], "piece 40 was marked");
        assert_eq!(left[0], 0xffff_ffff, "bits below the copy's are another copy's");
        assert_eq!(left[1], 1 << 4, "the copy's bits are cleared, bit 36 is another copy's");
        assert_eq!(left[2], 0, "piece 40's bit is cleared");
        assert_eq!(left[7], 0xffff_ffff, "bits past the ranges are left");
        // A second compare of the same pieces finds nothing: the bits were taken.
        run(&mut warp, 0, 5, 0);
        // SAFETY: as above.
        let again: Vec<u32> = (0..5).map(|i| unsafe { flags(i).read_volatile() }).collect();
        assert_eq!(again, [0; 5]);
    }

    /// The scatter (`crate::host::CARD_SCATTER`), on the software adapter,
    /// with the records `crate::shadow::scatter_records` makes of ranges
    /// of every alignment: the card's side gets the title's bytes in the
    /// ranges and keeps its own everywhere else, the other bytes of the
    /// dwords the ranges touch among them (what a shader of a flight wrote
    /// beside a title byte: joined over, it was lost and run 76's walls
    /// went black). More records than one group of threads, taken in two
    /// dispatches, the second from a record past the first.
    #[test]
    fn warp_card_scatter_puts_in_the_title_bytes_and_leaves_their_neighbours() {
        let Some(mut warp) = WarpHarness::new() else {
            return;
        };
        let scatter = compile_cs(&warp.device, crate::host::CARD_SCATTER).expect("the scatter must compile on WARP");
        const LEN: usize = 1024;
        // The CPU's side holds the title's bytes; the card's side another
        // byte everywhere, as if shaders had written all of it.
        let side: Vec<u8> = (0..LEN).map(|i| (i as u8).wrapping_mul(29).wrapping_add(3)).collect();
        let card: Vec<u8> = side.iter().map(|b| b ^ 0xa5).collect();
        // One byte; three dwords, unaligned at both ends; two ranges in one
        // dword; whole dwords; the copy's last byte; and a hundred single
        // bytes, each at another place of its dword.
        let mut ranges = vec![(1usize, 2usize), (5, 14), (21, 22), (23, 25), (32, 40), (LEN - 1, LEN)];
        ranges.extend((0..100).map(|i| (200 + i * 8 + i % 4, 200 + i * 8 + i % 4 + 1)));
        ranges.sort_unstable();
        let mut records = Vec::new();
        assert!(crate::shadow::scatter_records(&ranges, LEN, |at| u32::from_le_bytes(side[at..at + 4].try_into().unwrap()), &mut records).is_empty());
        assert!(records.len() > 64, "more than one group of threads");
        let (_copy, copy_ptr, copy_gpu) = warp.shared_buffer(LEN as u64);
        let (_table, table_ptr, table_gpu) = warp.shared_buffer(std::mem::size_of_val(&records[..]) as u64);
        // SAFETY: mappings of these sizes; nothing runs yet.
        unsafe {
            std::ptr::copy_nonoverlapping(card.as_ptr(), copy_ptr, LEN);
            std::ptr::copy_nonoverlapping(records.as_ptr().cast::<u8>(), table_ptr, std::mem::size_of_val(&records[..]));
        }
        let half = records.len() as u32 / 2;
        // SAFETY: our open list, live buffers, the shader's own root
        // signature; the mappings hold what is read and written.
        unsafe {
            warp.list.SetComputeRootSignature(&scatter.root);
            warp.list.SetPipelineState(&scatter.pso);
            warp.list.SetComputeRootShaderResourceView(1, table_gpu);
            warp.list.SetComputeRootUnorderedAccessView(2, copy_gpu);
            for (first, n) in [(0, half), (half, records.len() as u32 - half)] {
                let constants = [first, n];
                warp.list.SetComputeRoot32BitConstants(0, 2, constants.as_ptr().cast(), 0);
                warp.list.Dispatch(n.div_ceil(64), 1, 1);
            }
        }
        warp.submit_and_wait();
        // SAFETY: the work is done; the mapping.
        let got: Vec<u8> = (0..LEN).map(|i| unsafe { copy_ptr.add(i).read_volatile() }).collect();
        for at in 0..LEN {
            let title = ranges.iter().any(|&(a, b)| (a..b).contains(&at));
            assert_eq!(got[at], if title { side[at] } else { card[at] }, "byte {at} ({})", if title { "the title's" } else { "the card's own" });
        }
    }

    /// The fetch inside flights (`crate::host::CARD_FETCH`), on the software
    /// adapter, in one list behind the compare as `Host::card_before`
    /// records it: each flagged piece takes a ticket and, while it is
    /// within the slots of the lowest ticket in the air, its bytes go to
    /// its slot; an unflagged piece's entry says so; a ticket past the
    /// slots is written but copies nothing.
    #[test]
    fn warp_card_fetch_copies_flagged_pieces_to_their_slots() {
        use crate::host::{CARD_COMPARE, CARD_FETCH, INFLIGHT_NONE, INFLIGHT_SLOT};
        let Some(mut warp) = WarpHarness::new() else {
            return;
        };
        let compare = compile_cs(&warp.device, CARD_COMPARE).expect("the card compare must compile on WARP");
        let fetch = compile_cs(&warp.device, CARD_FETCH).expect("the card fetch must compile on WARP");
        const PIECE: usize = 64 << 10;
        const PIECES: u32 = 3;
        // The copy starts 40 bytes into its piece 0: piece 0 is short, the
        // others straddle title pieces and start off a multiple of 16.
        const SKEW: u32 = 40;
        let size = PIECE * PIECES as usize;
        let (_now, now_ptr, now_gpu) = warp.shared_buffer(size as u64);
        let (_hashes, _, hashes_gpu) = warp.shared_buffer(16 * u64::from(PIECES + 1));
        let (_differs, _, differs_gpu) = warp.shared_buffer(64);
        let (_table, table_ptr, table_gpu) = warp.shared_buffer(4 + 64);
        let slots = 1u32;
        let (_fetched, fetched_ptr, fetched_gpu) = warp.shared_buffer(INFLIGHT_SLOT * u64::from(slots));
        let content: Vec<u8> = (0..size).map(|i| (i * 31 + i / 251) as u8).collect();
        // SAFETY: the mappings hold what is written.
        unsafe {
            std::ptr::copy_nonoverlapping(content.as_ptr(), now_ptr, size);
            table_ptr.cast::<u32>().write_volatile(0);
        }
        let pieces = PIECES + 1;
        let run = |warp: &mut WarpHarness, mode: u32, landed: u32| -> Vec<u32> {
            // SAFETY: our open list, live buffers, the shaders' own root
            // signatures; the mappings hold what is read and written.
            unsafe {
                warp.list.SetComputeRootSignature(&compare.root);
                warp.list.SetPipelineState(&compare.pso);
                let constants = [0u32, SKEW, size as u32, pieces, 0, mode];
                warp.list.SetComputeRoot32BitConstants(0, 6, constants.as_ptr().cast(), 0);
                warp.list.SetComputeRootShaderResourceView(1, now_gpu);
                warp.list.SetComputeRootUnorderedAccessView(2, hashes_gpu);
                warp.list.SetComputeRootUnorderedAccessView(3, differs_gpu);
                warp.list.Dispatch(pieces, 1, 1);
                if mode == 2 {
                    warp.list.ResourceBarrier(&[uav_barrier()]);
                    warp.list.SetComputeRootSignature(&fetch.root);
                    warp.list.SetPipelineState(&fetch.pso);
                    // Two groups for four pieces: the loop over pieces.
                    let constants = [0u32, SKEW, size as u32, pieces, 0, landed, slots, 2];
                    warp.list.SetComputeRoot32BitConstants(0, 8, constants.as_ptr().cast(), 0);
                    warp.list.SetComputeRootShaderResourceView(1, now_gpu);
                    warp.list.SetComputeRootUnorderedAccessView(2, differs_gpu);
                    warp.list.SetComputeRootUnorderedAccessView(3, table_gpu);
                    warp.list.SetComputeRootUnorderedAccessView(4, fetched_gpu);
                    warp.list.Dispatch(2, 1, 1);
                }
            }
            warp.submit_and_wait();
            // SAFETY: as above.
            (0..=pieces as usize).map(|i| unsafe { table_ptr.cast::<u32>().add(i).read_volatile() }).collect()
        };
        // SAFETY (each): inside the mapping of the copy.
        let set = |at: usize, value: u8| unsafe { now_ptr.add(at).write_volatile(value) };
        // SAFETY (each): the slot's mapping holds `INFLIGHT_SLOT` bytes.
        let slot = |len: usize| unsafe { std::slice::from_raw_parts(fetched_ptr, len).to_vec() };
        run(&mut warp, 0, 0);
        // Piece 2 of the copy (title piece 2: bytes 2 * 64 KiB - 40 on), its
        // slot from the multiple of 16 below to the one above its end.
        let from = 2 * PIECE - SKEW as usize;
        let widened = from & !15;
        assert_ne!(widened, from);
        set(from + 100, content[from + 100] ^ 0x40);
        let table = run(&mut warp, 2, 0);
        assert_eq!(table, [1, INFLIGHT_NONE, INFLIGHT_NONE, 0, INFLIGHT_NONE]);
        let mut wanted = content[widened..widened + INFLIGHT_SLOT as usize].to_vec();
        wanted[from + 100 - widened] ^= 0x40;
        assert_eq!(slot(INFLIGHT_SLOT as usize), wanted, "piece 2 in slot 0");
        // Two pieces flagged with one slot, ticket 0 not landed: tickets 1
        // and 2 get none and the slot keeps what it held.
        set(5, content[5] ^ 1);
        set(PIECE + 7, content[PIECE + 7] ^ 1);
        let table = run(&mut warp, 2, 0);
        assert_eq!(table[0], 3);
        let mut tickets = [table[1], table[2]];
        tickets.sort_unstable();
        assert_eq!(tickets, [1, 2]);
        assert_eq!(&table[3..], [INFLIGHT_NONE, INFLIGHT_NONE]);
        assert_eq!(slot(INFLIGHT_SLOT as usize), wanted);
        // Every ticket up to 2 landed: the next one, 3, gets slot 0. Piece
        // 0 starts at the copy's start.
        set(9, content[9] ^ 2);
        let table = run(&mut warp, 2, 3);
        assert_eq!(table, [4, 3, INFLIGHT_NONE, INFLIGHT_NONE, INFLIGHT_NONE]);
        let mut piece0 = content[..PIECE - SKEW as usize].to_vec();
        piece0[5] ^= 1;
        piece0[9] ^= 2;
        assert_eq!(slot(PIECE - SKEW as usize), piece0);
    }

    /// The clamp compiles with the system's compiler and the host takes it.
    #[test]
    fn argument_clamp_builds() {
        if super::host().is_none() {
            return; // no host GPU here
        }
        assert!(super::clamp().is_some());
    }

    #[test]
    fn rejects_unaligned_or_undersized_layouts() {
        assert_eq!(validate_layout((0, 16, 4), 0, None), Some(64));
        assert_eq!(validate_layout((1, 20, 4), 64, Some(128)), Some(80));
        assert_eq!(validate_layout((2, 12, 0), 0, Some(0)), Some(0));
        assert_eq!(validate_layout((10, 16, 2), 8, None), Some(32));
        // Undersized strides for each kind.
        assert_eq!(validate_layout((0, 12, 1), 0, None), None);
        assert_eq!(validate_layout((1, 16, 1), 0, None), None);
        assert_eq!(validate_layout((2, 8, 1), 0, None), None);
        assert_eq!(validate_layout((10, 8, 1), 0, None), None);
        // Unknown kind.
        assert_eq!(validate_layout((3, 16, 1), 0, None), None);
        // Unaligned stride, source offset, or count offset.
        assert_eq!(validate_layout((0, 18, 1), 0, None), None);
        assert_eq!(validate_layout((0, 16, 1), 2, None), None);
        assert_eq!(validate_layout((0, 16, 1), 0, Some(6)), None);
        // Total byte size exceeding 32-bit ByteAddressBuffer indexing.
        assert_eq!(validate_layout((1, 20, u32::MAX), 0, None), None);
    }

    /// WARP D3D12 execution test proving:
    /// (a) `countBuffer` with a small count (`3`) and `4,093` garbage tail slots
    ///     (`max = 4096`) does NOT blow `BUDGET` or zero valid dispatches in
    ///     this or subsequent `ExecuteIndirect` calls in the same list,
    /// (b) `src_args` (and `count_buf`) are NOT mutated in-place, and
    /// (c) full `stride` bytes (`stride = 24` > `12`) are copied for active
    ///     records into `dst_args` at offset `0`.
    #[test]
    fn warp_count_buffer_ignores_garbage_tail_and_preserves_source() {
        let Some(mut warp) = WarpHarness::new() else {
            return;
        };
        const KIND_DISPATCH: u32 = 2;
        const STRIDE: u32 = 24; // 6 dwords: 3 dispatch group counts + 3 extra payload dwords
        const MAX: u32 = 4096;
        const ARGS_OFFSET: usize = 64; // non-zero offset with sentinel prefix
        const COUNT_OFFSET_1: usize = 128;
        const COUNT_OFFSET_2: usize = 192;

        let src_bytes = ARGS_OFFSET + (STRIDE as usize) * (MAX as usize) + 64;
        let (src_buf, src_ptr, _) = warp.shared_buffer(src_bytes as u64);
        let (count_buf, count_ptr, _) = warp.shared_buffer(512);
        let (_budget_buf, budget_ptr, budget_gpu) = warp.shared_buffer(256);
        let (_hits_buf, hits_ptr, hits_gpu) = warp.shared_buffer(256);
        let (readback_buf, readback_ptr, _) = warp.shared_buffer((STRIDE * 8) as u64);
        let dst_buf = warp.default_uav(u64::from(STRIDE) * u64::from(MAX));
        let sig = warp.signature(KIND_DISPATCH, STRIDE);

        // Fill the entire source buffer with non-zero sentinel bytes, then
        // populate 3 valid records at `ARGS_OFFSET` and 4,093 huge garbage
        // records in `3..MAX` that would exhaust `BUDGET` ~2,000x over if read.
        let mut initial_src = vec![0xA5u8; src_bytes];
        let write_record = |buf: &mut [u8], idx: usize, words: [u32; 6]| {
            let base = ARGS_OFFSET + idx * (STRIDE as usize);
            for (w, val) in words.into_iter().enumerate() {
                buf[base + w * 4..base + (w + 1) * 4].copy_from_slice(&val.to_le_bytes());
            }
        };
        write_record(
            &mut initial_src,
            0,
            [3, 2, 1, 0x1111_2222, 0x3333_4444, 0x5555_6666],
        );
        write_record(
            &mut initial_src,
            1,
            [5, 1, 1, 0x7777_8888, 0x9999_AAAA, 0xBBBB_CCCC],
        );
        // Record 2 exceeds 65,535 in X -> clamped to (65535, 1, 1).
        write_record(
            &mut initial_src,
            2,
            [100_000, 1, 1, 0xDDDD_EEEE, 0xFFFF_0001, 0x0123_4567],
        );
        for slot in 3..MAX as usize {
            // Each garbage slot would clamp to 65535 * 16 * 1 = 1,048,560 groups
            // (`cost = 67,107,840 ~= BUDGET / 2`), exhausting `BUDGET` immediately!
            write_record(
                &mut initial_src,
                slot,
                [0xFFFF_FFFF, 16, 1, 0xDEAD_BEEF, 0xBAD0_C0DE, slot as u32],
            );
        }

        // Second indirect call's source records (at offset 0 of a second buffer,
        // or later in `count_buf`): put 2 active records + garbage tail in a
        // second range to run back-to-back in the SAME command list.
        let mut initial_count = vec![0x5Au8; 512];
        initial_count[COUNT_OFFSET_1..COUNT_OFFSET_1 + 4].copy_from_slice(&3u32.to_le_bytes());
        initial_count[COUNT_OFFSET_2..COUNT_OFFSET_2 + 4].copy_from_slice(&2u32.to_le_bytes());

        // SAFETY: buffers are mapped with exact capacities above.
        unsafe {
            std::ptr::copy_nonoverlapping(initial_src.as_ptr(), src_ptr, src_bytes);
            std::ptr::copy_nonoverlapping(initial_count.as_ptr(), count_ptr, 512);
            (budget_ptr as *mut u32).write_volatile(0);
            (hits_ptr as *mut u32).write_volatile(0);
            std::ptr::write_bytes(readback_ptr, 0, (STRIDE * 8) as usize);
        }

        // Record Call #1 (`eff_count = 3` out of `MAX = 4096`), copy the first
        // 5 records of `dst_buf` out for inspection, then record Call #2
        // (`eff_count = 2` out of `MAX = 4096`) reusing `dst_buf` in the same list!
        assert!(record_clamp_as(
            &warp.clamp,
            &warp.list,
            (&dst_buf, D3D12_RESOURCE_STATE_COMMON),
            budget_gpu,
            (&src_buf, ARGS_OFFSET as u64),
            Some((&count_buf, COUNT_OFFSET_1 as u64)),
            (KIND_DISPATCH, STRIDE, MAX),
            0,
        ));
        // SAFETY: open command list, valid worker PSO/root and buffers.
        unsafe {
            warp.list.SetComputeRootSignature(&warp.worker.root);
            warp.list.SetPipelineState(&warp.worker.pso);
            warp.list.SetComputeRootUnorderedAccessView(0, hits_gpu);
            warp.list.ExecuteIndirect(
                &sig,
                MAX,
                &dst_buf,
                0,
                Some(&count_buf),
                COUNT_OFFSET_1 as u64,
            );
        }

        // Snapshot `dst_buf` after Call #1 (copy 5 * STRIDE bytes, then leave
        // `dst_buf` readable as arguments again, as the clamp left it).
        // SAFETY: open command list; the clamp left `dst_buf` in `INDIRECT_ARGUMENT`.
        unsafe {
            warp.list.ResourceBarrier(&[transition(
                &dst_buf,
                D3D12_RESOURCE_STATE_INDIRECT_ARGUMENT,
                D3D12_RESOURCE_STATE_COPY_SOURCE,
            )]);
            warp.list
                .CopyBufferRegion(&readback_buf, 0, &dst_buf, 0, u64::from(STRIDE * 5));
            warp.list.ResourceBarrier(&[transition(
                &dst_buf,
                D3D12_RESOURCE_STATE_COPY_SOURCE,
                D3D12_RESOURCE_STATE_INDIRECT_ARGUMENT,
            )]);
        }

        // Call #2 in the SAME command list (`eff_count = 2` -> records 0 and 1:
        // 6 + 5 = 11 groups). Would run empty if Call #1's tail blew `BUDGET`!
        // The scratch buffer goes straight from arguments back to unordered
        // access.
        assert!(record_clamp_as(
            &warp.clamp,
            &warp.list,
            (&dst_buf, D3D12_RESOURCE_STATE_INDIRECT_ARGUMENT),
            budget_gpu,
            (&src_buf, ARGS_OFFSET as u64),
            Some((&count_buf, COUNT_OFFSET_2 as u64)),
            (KIND_DISPATCH, STRIDE, MAX),
            0,
        ));
        // SAFETY: open command list, valid worker PSO/root and buffers.
        unsafe {
            warp.list.SetComputeRootSignature(&warp.worker.root);
            warp.list.SetPipelineState(&warp.worker.pso);
            warp.list.SetComputeRootUnorderedAccessView(0, hits_gpu);
            warp.list.ExecuteIndirect(
                &sig,
                MAX,
                &dst_buf,
                0,
                Some(&count_buf),
                COUNT_OFFSET_2 as u64,
            );
        }

        warp.submit_and_wait();

        // SAFETY: GPU execution finished; read mapped host results.
        let (hits, spent, after_src, after_count, clamped_words) = unsafe {
            let hits = (hits_ptr as *const u32).read_volatile();
            let spent = (budget_ptr as *const u32).read_volatile();
            let after_src = std::slice::from_raw_parts(src_ptr.cast_const(), src_bytes).to_vec();
            let after_count = std::slice::from_raw_parts(count_ptr.cast_const(), 512).to_vec();
            let words = std::slice::from_raw_parts(
                readback_ptr.cast::<u32>().cast_const(),
                (STRIDE as usize / 4) * 5,
            )
            .to_vec();
            (hits, spent, after_src, after_count, words)
        };

        // Call #1: (3*2*1) + (5*1*1) + (65535*1*1) = 65,546 groups.
        // Call #2: (3*2*1) + (5*1*1) = 11 groups.
        let expected_groups = (6 + 5 + 65_535) + (6 + 5);
        assert_eq!(
            hits, expected_groups,
            "valid indirect dispatches must all execute"
        );
        assert_eq!(
            spent,
            expected_groups * 64,
            "budget must reflect only active records below countBuffer"
        );
        assert_eq!(
            after_src, initial_src,
            "src_args must never be mutated in-place"
        );
        assert_eq!(
            after_count, initial_count,
            "count_buf must never be mutated"
        );

        // Check `dst_args` records 0..3: clamped group counts + preserved extra stride words.
        assert_eq!(
            &clamped_words[0..6],
            &[3, 2, 1, 0x1111_2222, 0x3333_4444, 0x5555_6666]
        );
        assert_eq!(
            &clamped_words[6..12],
            &[5, 1, 1, 0x7777_8888, 0x9999_AAAA, 0xBBBB_CCCC]
        );
        assert_eq!(
            &clamped_words[12..18],
            &[65_535, 1, 1, 0xDDDD_EEEE, 0xFFFF_0001, 0x0123_4567]
        );
        // Inactive slot 3 was never copied from `src_args`.
        assert_ne!(clamped_words[21], 0xDEAD_BEEF);
    }

    /// WARP D3D12 test proving `BUDGET` saturates without 32-bit unsigned
    /// wraparound across >64 huge indexed-draw records (`count = None`), while
    /// preserving `StartIndexLocation`, `BaseVertexLocation`,
    /// `StartInstanceLocation`, and custom stride padding, and leaving
    /// `src_args` untouched.
    /// A window of mesh records clamped ahead: the filled ones come out as
    /// they are and are charged, the slots of all ones come out empty and
    /// charge nothing; clamped as single records the same slots are clamped
    /// to the most and spend the budget.
    #[test]
    fn warp_window_leaves_unfilled_slots_empty_and_uncharged() {
        let Some(mut warp) = WarpHarness::new() else {
            return;
        };
        const KIND_MESH: u32 = 10;
        const STRIDE: u32 = 32;
        const RECORDS: u32 = 16;
        let filled = [5u32, 0, 519];
        let total = (STRIDE * RECORDS) as usize;
        let mut src = vec![0xffu8; total];
        for (i, groups) in filled.into_iter().enumerate() {
            let words = [groups, 1, 1, 0xAB00 + i as u32];
            for (w, word) in words.into_iter().enumerate() {
                let at = i * STRIDE as usize + w * 4;
                src[at..at + 4].copy_from_slice(&word.to_le_bytes());
            }
        }
        for ahead in [true, false] {
            let (src_buf, src_ptr, _) = warp.shared_buffer(total as u64);
            let (_budget_buf, budget_ptr, budget_gpu) = warp.shared_buffer(256);
            let (readback_buf, readback_ptr, _) = warp.shared_buffer(total as u64);
            let dst_buf = warp.default_uav(total as u64);
            // SAFETY: mapped buffers of these sizes.
            unsafe {
                std::ptr::copy_nonoverlapping(src.as_ptr(), src_ptr, total);
                std::ptr::write_bytes(budget_ptr, 0, 256);
                std::ptr::write_bytes(readback_ptr, 0, total);
            }
            assert!(record_clamp_as(
                &warp.clamp,
                &warp.list,
                (&dst_buf, D3D12_RESOURCE_STATE_COMMON),
                budget_gpu,
                (&src_buf, 0),
                None,
                (KIND_MESH, STRIDE, RECORDS),
                if ahead { AHEAD } else { 0 },
            ));
            // SAFETY: an open list; the scratch buffer, which the clamp left
            // readable as arguments, is copied for the check.
            unsafe {
                warp.list.ResourceBarrier(&[transition(
                    &dst_buf,
                    D3D12_RESOURCE_STATE_INDIRECT_ARGUMENT,
                    D3D12_RESOURCE_STATE_COPY_SOURCE,
                )]);
                warp.list.CopyBufferRegion(&readback_buf, 0, &dst_buf, 0, total as u64);
            }
            warp.submit_and_wait();
            // SAFETY: the work is done; mapped buffers.
            let (spent, largest, out) = unsafe {
                (
                    (budget_ptr as *const u32).read_volatile(),
                    (budget_ptr as *const u32).add(1).read_volatile(),
                    std::slice::from_raw_parts(readback_ptr.cast::<u32>().cast_const(), total / 4).to_vec(),
                )
            };
            for (i, groups) in filled.into_iter().enumerate() {
                let rec = &out[i * 8..i * 8 + 4];
                assert_eq!(rec, [groups, u32::from(groups > 0).max(1).min(1), 1, 0xAB00 + i as u32], "filled record {i}, ahead {ahead}");
            }
            let charged_filled = (5 + 519) * 128;
            if ahead {
                for i in filled.len()..RECORDS as usize {
                    assert_eq!(out[i * 8..i * 8 + 3], [0, 0, 0], "unfilled slot {i} must come out empty");
                    assert_eq!(out[i * 8 + 3], u32::MAX, "the words after the record are copied as they are");
                }
                assert_eq!(spent, charged_filled, "unfilled slots charge nothing");
                assert_eq!(largest, 519, "and are not counted as asked for");
            } else {
                // Thirteen slots at 65,535 groups: sixteen would be all of it.
                assert_eq!(spent, charged_filled + 13 * 65535 * 128, "clamped one by one, each slot is charged the most");
                assert_eq!(largest, u32::MAX);
            }
        }
    }

    /// The clamp's source and the host side agree on the parts: how many
    /// records a dispatch is made in, and how many groups each holds.
    #[test]
    fn the_clamp_and_the_host_agree_on_the_parts() {
        assert!(SOURCE.contains(&format!("static const uint SPLIT = {SPLIT};")));
        assert!(SOURCE.contains(&format!("{PART}u")));
        assert_eq!(SPLIT_STRIDE, SPLIT * 12);
        assert_eq!(u64::from(SPLIT_STRIDE), u64::from(SPLIT) * PART_BYTES);
    }

    /// The parts of a direct dispatch: none for no groups, a part of 65,535
    /// at most, and each part begins where the one before it ended.
    #[test]
    fn a_direct_dispatch_is_cut_into_parts_of_the_most_the_host_runs() {
        let parts = |x| dispatch_parts(x).collect::<Vec<_>>();
        assert_eq!(parts(0), []);
        assert_eq!(parts(1), [(0, 1)]);
        assert_eq!(parts(65_535), [(0, 65_535)]);
        assert_eq!(parts(65_536), [(0, 65_535), (65_535, 1)]);
        assert_eq!(parts(115_200), [(0, 65_535), (65_535, 49_665)]);
        // Past the indirect parts too: a direct dispatch's count is known.
        assert_eq!(parts(300_000).len(), 5);
        assert_eq!(parts(300_000).last(), Some(&(262_140, 37_860)));
        // Every group once, whatever the count, up to the most there is.
        for x in [1, 2, 65_534, 65_535, 65_536, 131_070, 131_071, 1 << 20, u32::MAX] {
            let all = parts(x);
            assert!(all.iter().all(|&(_, groups)| (1..=PART).contains(&groups)));
            let mut next = 0u64;
            for &(base, groups) in &all {
                assert_eq!(u64::from(base), next, "x {x}");
                next += u64::from(groups);
            }
            assert_eq!(next, u64::from(x));
        }
    }

    /// Makes `records` (three words each, `stride` bytes apart, then a tail
    /// word of 0xabcd_0000 + the record's number) the parts of dispatches
    /// with [`record_clamp_as`] (`how`: [`IN_PARTS`] with or without
    /// [`UNCLAMPED`]; `count`: the record count a count buffer gives), and
    /// returns the words the pass wrote for each record: [`SPLIT`] records
    /// of three words.
    fn made_in_parts(
        warp: &mut WarpHarness,
        records: &[[u32; 3]],
        stride: u32,
        count: Option<u32>,
        how: u32,
    ) -> Vec<u32> {
        let n = records.len();
        let out_words = n * SPLIT as usize * 3;
        let (src_buf, src_ptr, _) = warp.shared_buffer(u64::from(stride) * n as u64);
        let (count_buf, count_ptr, _) = warp.shared_buffer(16);
        let (_budget_buf, budget_ptr, budget_gpu) = warp.shared_buffer(256);
        let (readback_buf, readback_ptr, _) = warp.shared_buffer(out_words as u64 * 4);
        let dst_buf = warp.default_uav(out_words as u64 * 4);
        // SAFETY: mapped buffers of these sizes.
        unsafe {
            for (i, record) in records.iter().enumerate() {
                let at = src_ptr.cast::<u32>().add(i * stride as usize / 4);
                for (w, &word) in record.iter().enumerate() {
                    at.add(w).write_volatile(word);
                }
                for w in 3..stride as usize / 4 {
                    at.add(w).write_volatile(0xabcd_0000 + i as u32);
                }
            }
            count_ptr.cast::<u32>().write_volatile(count.unwrap_or(0));
            std::ptr::write_bytes(budget_ptr, 0, 256);
            std::ptr::write_bytes(readback_ptr, 0xee, out_words * 4);
        }
        assert!(record_clamp_as(
            &warp.clamp,
            &warp.list,
            (&dst_buf, D3D12_RESOURCE_STATE_COMMON),
            budget_gpu,
            (&src_buf, 0),
            count.map(|_| (&count_buf, 0)),
            (2, stride, n as u32),
            how,
        ));
        done_on(&warp.list, &dst_buf);
        // SAFETY: an open list; the scratch buffer is copied for the check.
        unsafe {
            warp.list.ResourceBarrier(&[transition(
                &dst_buf,
                D3D12_RESOURCE_STATE_COMMON,
                D3D12_RESOURCE_STATE_COPY_SOURCE,
            )]);
            warp.list.CopyBufferRegion(&readback_buf, 0, &dst_buf, 0, out_words as u64 * 4);
            warp.list.ResourceBarrier(&[transition(
                &dst_buf,
                D3D12_RESOURCE_STATE_COPY_SOURCE,
                D3D12_RESOURCE_STATE_COMMON,
            )]);
        }
        warp.submit_and_wait();
        // SAFETY: the work is done; the mapping holds `out_words` words.
        unsafe { std::slice::from_raw_parts(readback_ptr.cast::<u32>().cast_const(), out_words).to_vec() }
    }

    /// The words of [`SPLIT`] parts, each `(x, y, z)`; a part of no groups
    /// is `(0, 0, 0)`.
    fn parts_of(asked: [u32; SPLIT as usize], rest: [u32; 2]) -> Vec<u32> {
        asked
            .into_iter()
            .flat_map(|x| if x == 0 { [0, 0, 0] } else { [x, rest[0], rest[1]] })
            .collect()
    }

    /// One dispatch in parts, with the clamp's limits: groups past what one
    /// dispatch runs go into the later records; what nothing wrote stays
    /// clamped, or empty; a dispatch of more than one dimension is clamped
    /// as before.
    #[test]
    fn warp_large_dispatch_is_made_in_parts() {
        let Some(mut warp) = WarpHarness::new() else {
            return;
        };
        let cases: [([u32; 3], Vec<u32>); 9] = [
            ([115_200, 1, 1], parts_of([65_535, 49_665, 0, 0], [1, 1])),
            ([65_535, 1, 1], parts_of([65_535, 0, 0, 0], [1, 1])),
            ([65_536, 1, 1], parts_of([65_535, 1, 0, 0], [1, 1])),
            // Exactly what the parts hold: all four, full.
            ([262_140, 1, 1], parts_of([65_535; 4], [1, 1])),
            // One group past them: the garbage of arguments nothing wrote,
            // clamped to one part as a dispatch not in parts is.
            ([262_141, 1, 1], parts_of([65_535, 0, 0, 0], [1, 1])),
            ([u32::MAX, 1, 1], parts_of([65_535, 0, 0, 0], [1, 1])),
            // More than one dimension: not in parts.
            ([5, 3, 2], parts_of([5, 0, 0, 0], [3, 2])),
            ([100_000, 2, 1], parts_of([65_535, 0, 0, 0], [2, 1])),
            ([0, 1, 1], parts_of([0; 4], [1, 1])),
        ];
        for (asked, parts) in cases {
            let made = made_in_parts(&mut warp, &[asked], 12, None, IN_PARTS);
            assert_eq!(made, parts, "{asked:?}");
        }
    }

    /// The same without the clamp (`VICTORY_INDIRECT_CLAMP=0`): the groups
    /// as the title asked, cut into parts, and only what the parts cannot
    /// hold is left out; nothing is cut to garbage limits.
    #[test]
    fn warp_large_dispatch_is_made_in_parts_without_the_clamp() {
        let Some(mut warp) = WarpHarness::new() else {
            return;
        };
        let cases: [([u32; 3], Vec<u32>); 8] = [
            ([115_200, 1, 1], parts_of([65_535, 49_665, 0, 0], [1, 1])),
            ([65_535, 1, 1], parts_of([65_535, 0, 0, 0], [1, 1])),
            ([65_536, 1, 1], parts_of([65_535, 1, 0, 0], [1, 1])),
            ([300_000, 1, 1], parts_of([65_535; 4], [1, 1])),
            // Other dimensions go through as they were asked, in every part.
            ([100_000, 3, 2], parts_of([65_535, 34_465, 0, 0], [3, 2])),
            ([70_000, 0, 5], parts_of([65_535, 4_465, 0, 0], [0, 5])),
            ([5, 3, 2], parts_of([5, 0, 0, 0], [3, 2])),
            ([0, 7, 9], parts_of([0; 4], [7, 9])),
        ];
        for (asked, parts) in cases {
            let made = made_in_parts(&mut warp, &[asked], 12, None, IN_PARTS | UNCLAMPED);
            assert_eq!(made, parts, "{asked:?}");
        }
    }

    /// Several records, strides past 12 and a count buffer: record `i`'s
    /// parts lie [`SPLIT_STRIDE`] bytes after record `i - 1`'s, whatever the
    /// source stride is, and records past the count are not made.
    #[test]
    fn warp_dispatches_in_parts_keep_their_records_apart() {
        let Some(mut warp) = WarpHarness::new() else {
            return;
        };
        let records = [[115_200, 1, 1], [7, 1, 1], [70_000, 1, 1], [9, 1, 1]];
        for how in [IN_PARTS, IN_PARTS | UNCLAMPED] {
            let made = made_in_parts(&mut warp, &records, 24, Some(3), how);
            let per = SPLIT as usize * 3;
            assert_eq!(made[..per], parts_of([65_535, 49_665, 0, 0], [1, 1]), "how {how}");
            assert_eq!(made[per..2 * per], parts_of([7, 0, 0, 0], [1, 1]), "how {how}");
            assert_eq!(made[2 * per..3 * per], parts_of([65_535, 4_465, 0, 0], [1, 1]), "how {how}");
            // The fourth is past the count: not made (the scratch buffer
            // holds what it did, not the record's 9 groups).
            assert_ne!(made[3 * per..4 * per], parts_of([9, 0, 0, 0], [1, 1]), "how {how}");
        }
    }

    /// A program given the number of its first group runs every group once
    /// when a dispatch of 115,200 is run in parts, from an indirect record
    /// (with and without the clamp) and from a direct dispatch.
    #[test]
    fn warp_a_dispatch_in_parts_runs_every_group_once() {
        let Some(mut warp) = WarpHarness::new() else {
            return;
        };
        const BASE_CS: &str = r#"
#define RS "RootConstants(num32BitConstants=1, b0), UAV(u0)"
cbuffer First : register(b0) { uint base; };
RWByteAddressBuffer seen : register(u0);
[RootSignature(RS)]
[numthreads(1, 1, 1)]
void main(uint3 group : SV_GroupID) {
    uint was;
    // Groups run, the highest number + 1, the sum of the numbers (mod 2^32).
    seen.InterlockedAdd(0, 1, was);
    seen.InterlockedMax(4, base + group.x + 1, was);
    seen.InterlockedAdd(8, base + group.x, was);
}
"#;
        let program = compile_cs(&warp.device, BASE_CS).expect("the test program must compile");
        const GROUPS: u32 = 115_200;
        let expected = [GROUPS, GROUPS, (u64::from(GROUPS) * u64::from(GROUPS - 1) / 2) as u32];
        let split = warp.signature(2, SPLIT_STRIDE);
        for way in ["indirect, clamp on", "indirect, clamp off", "direct"] {
            let (src_buf, src_ptr, _) = warp.shared_buffer(12);
            let (_budget_buf, budget_ptr, budget_gpu) = warp.shared_buffer(256);
            let (_seen_buf, seen_ptr, seen_gpu) = warp.shared_buffer(64);
            let dst_buf = warp.default_uav(u64::from(SPLIT_STRIDE));
            // SAFETY: mapped buffers of these sizes.
            unsafe {
                for (i, word) in [GROUPS, 1, 1].into_iter().enumerate() {
                    src_ptr.cast::<u32>().add(i).write_volatile(word);
                }
                std::ptr::write_bytes(budget_ptr, 0, 256);
                std::ptr::write_bytes(seen_ptr, 0, 64);
            }
            let direct = way == "direct";
            if !direct {
                let how = if way == "indirect, clamp on" { IN_PARTS } else { IN_PARTS | UNCLAMPED };
                assert!(record_clamp_as(
                    &warp.clamp,
                    &warp.list,
                    (&dst_buf, D3D12_RESOURCE_STATE_COMMON),
                    budget_gpu,
                    (&src_buf, 0),
                    None,
                    (2, 12, 1),
                    how,
                ));
            }
            // SAFETY: an open list; our root signature, program and buffers.
            unsafe {
                warp.list.SetComputeRootSignature(&program.root);
                warp.list.SetPipelineState(&program.pso);
                warp.list.SetComputeRootUnorderedAccessView(1, seen_gpu);
                if direct {
                    for (base, groups) in dispatch_parts(GROUPS) {
                        warp.list.SetComputeRoot32BitConstant(0, base, 0);
                        warp.list.Dispatch(groups, 1, 1);
                    }
                } else {
                    for part in 0..SPLIT {
                        warp.list.SetComputeRoot32BitConstant(0, part * PART, 0);
                        warp.list.ExecuteIndirect(&split, 1, &dst_buf, u64::from(part) * PART_BYTES, None, 0);
                    }
                }
            }
            if !direct {
                done_on(&warp.list, &dst_buf);
            }
            warp.submit_and_wait();
            // SAFETY: the work is done; the mapping holds the words.
            let words = unsafe { [0, 1, 2].map(|i| seen_ptr.cast::<u32>().add(i).read_volatile()) };
            assert_eq!(words, expected, "{way}");
        }
    }

    #[test]
    fn warp_budget_saturates_without_wraparound_and_preserves_draw_fields() {
        let Some(mut warp) = WarpHarness::new() else {
            return;
        };
        const KIND_INDEXED: u32 = 1;
        const STRIDE: u32 = 24; // 5 indexed-draw words + 1 trailing custom word
        const RECORDS: u32 = 128; // 2 waves of 64: old InterlockedAdd wrapped 2^32 at 64!
        const BUDGET_CAP: u32 = 1 << 27;

        let total_bytes = (STRIDE * RECORDS) as usize;
        let (src_buf, src_ptr, _) = warp.shared_buffer(total_bytes as u64);
        let (_budget_buf, budget_ptr, budget_gpu) = warp.shared_buffer(256);
        let (readback_buf, readback_ptr, _) = warp.shared_buffer(total_bytes as u64);
        let dst_buf = warp.default_uav(total_bytes as u64);

        let mut initial_src = vec![0u8; total_bytes];
        for i in 0..RECORDS as usize {
            // The first record is no draw (2^24 indices): it runs empty
            // and charges nothing.
            let words: [u32; 6] = [
                if i == 0 { 1 << 24 } else { 1 << 22 }, // IndexCount
                if i == 0 { 1 } else { u32::MAX }, // InstanceCount -> clamps to 16 (cost = 1 << 26)
                1000 + i as u32,             // StartIndexLocation
                (-200i32 + i as i32) as u32, // BaseVertexLocation
                5000 + i as u32,             // StartInstanceLocation
                0xCAFE_0000 | (i as u32),    // Extra stride word
            ];
            let base = i * (STRIDE as usize);
            for (w, val) in words.into_iter().enumerate() {
                initial_src[base + w * 4..base + (w + 1) * 4].copy_from_slice(&val.to_le_bytes());
            }
        }

        // SAFETY: mapped buffers have sufficient capacity.
        unsafe {
            std::ptr::copy_nonoverlapping(initial_src.as_ptr(), src_ptr, total_bytes);
            (budget_ptr as *mut u32).write_volatile(0);
            std::ptr::write_bytes(readback_ptr, 0, total_bytes);
        }

        assert!(record_clamp_as(
            &warp.clamp,
            &warp.list,
            (&dst_buf, D3D12_RESOURCE_STATE_COMMON),
            budget_gpu,
            (&src_buf, 0),
            None,
            (KIND_INDEXED, STRIDE, RECORDS),
            0,
        ));

        // SAFETY: open command list; copy clamped scratch buffer (left
        // readable as arguments) for verification.
        unsafe {
            warp.list.ResourceBarrier(&[transition(
                &dst_buf,
                D3D12_RESOURCE_STATE_INDIRECT_ARGUMENT,
                D3D12_RESOURCE_STATE_COPY_SOURCE,
            )]);
            warp.list
                .CopyBufferRegion(&readback_buf, 0, &dst_buf, 0, total_bytes as u64);
        }
        warp.submit_and_wait();

        // SAFETY: GPU work completed.
        let (spent, after_src, out_words) = unsafe {
            let spent = (budget_ptr as *const u32).read_volatile();
            let after_src = std::slice::from_raw_parts(src_ptr.cast_const(), total_bytes).to_vec();
            let out_words = std::slice::from_raw_parts(
                readback_ptr.cast::<u32>().cast_const(),
                total_bytes / 4,
            )
            .to_vec();
            (spent, after_src, out_words)
        };

        assert_eq!(
            spent, BUDGET_CAP,
            "budget must saturate at BUDGET without wrapping"
        );
        assert_eq!(after_src, initial_src, "src_args must remain untouched");

        let mut kept = 0usize;
        let mut zeroed = 0usize;
        // Each group of 64 charges its records in lane order, but which of
        // the two groups charges first is the GPU's to choose: the two that
        // fit are the first two real draws of one group (1 and 2, or 64 and
        // 65).
        let mut fitted = Vec::new();
        for i in 0..RECORDS as usize {
            let rec = &out_words[i * 6..(i + 1) * 6];
            if rec[0] == (1 << 22) && rec[1] == 16 {
                fitted.push(i);
                kept += 1;
            } else {
                assert_eq!((rec[0], rec[1]), (0, 0), "excess draw must be zeroed");
                zeroed += 1;
            }
            assert_eq!(rec[2], 1000 + i as u32);
            assert_eq!(rec[3], (-200i32 + i as i32) as u32);
            assert_eq!(rec[4], 5000 + i as u32);
            assert_eq!(rec[5], 0xCAFE_0000 | (i as u32));
        }
        assert_eq!((kept, zeroed), (2, (RECORDS as usize) - 2));
        assert!(
            fitted == [1, 2] || fitted == [64, 65],
            "the first two real draws of one group fit the budget: {fitted:?}"
        );
    }

    /// Exercises `HostList::indirect_scratch`, `bound`, the scratch buffer's
    /// tracked state, `Host::execute`, and `HostList::reset` end-to-end on
    /// the host device.
    #[test]
    fn host_list_reuses_and_grows_scratch_across_submissions() {
        let Some(host) = super::host() else {
            return;
        };
        // What earlier tests left in the debug layer's queue, if it is on.
        let _ = host.log_messages();
        let Ok(list) = host.new_list() else {
            return;
        };
        let Some((src_buf, src_ptr)) = host.shared_buffer(256 << 10) else {
            return;
        };
        let Some((count_buf, count_ptr)) = host.shared_buffer(256) else {
            return;
        };
        // SAFETY: mapped host buffers.
        unsafe {
            std::ptr::write_bytes(src_ptr, 0x7F, 256 << 10);
            (count_ptr as *mut u32).write_volatile(2);
        }

        let args = D3D12_RESOURCE_STATE_INDIRECT_ARGUMENT;
        // First two calls fit inside the initial 64 KiB scratch buffer and
        // must reuse the exact same `ID3D12Resource`, which stays readable
        // as arguments between them.
        let b1 = bound(&list, (&src_buf, 0), Some((&count_buf, 0)), (0, 16, 64), false).unwrap();
        assert_eq!(
            list.indirect_state(&b1),
            args,
            "a clamp leaves the scratch readable as arguments"
        );
        let b2 = bound(&list, (&src_buf, 64), Some((&count_buf, 0)), (1, 20, 128), false).unwrap();
        assert_eq!(
            b1.as_raw(),
            b2.as_raw(),
            "scratch buffer within capacity must be reused"
        );
        assert_eq!(list.indirect_state(&b2), args);

        // A call needing >64 KiB grows the scratch buffer mid-list while keeping
        // `b1` alive until reset. The new buffer starts out common.
        let grown = list.indirect_scratch(5000 * 20).unwrap();
        assert_eq!(
            list.indirect_state(&grown),
            D3D12_RESOURCE_STATE_COMMON,
            "a new scratch buffer is common"
        );
        let b3 = bound(&list, (&src_buf, 0), Some((&count_buf, 0)), (1, 20, 5000), false).unwrap();
        assert_eq!(b3.as_raw(), grown.as_raw());
        assert_ne!(
            b3.as_raw(),
            b1.as_raw(),
            "larger request must grow scratch buffer"
        );
        assert_eq!(list.indirect_state(&b3), args);
        assert_eq!(
            list.indirect_state(&b1),
            D3D12_RESOURCE_STATE_COMMON,
            "the replaced buffer went back to common"
        );

        list.close();
        // With the debug layer on (`VICTORY_D3D12_DEBUG=1`, diagnostics):
        // every barrier recorded named the state the buffer was in. Read
        // before the submission, which moves the messages to the log.
        assert_eq!(host.log_messages(), None, "the debug layer found an error");
        host.execute(&[&list]);
        list.reset();

        // After reset, requests up to the grown high-water mark reuse `b3` with
        // zero new allocations; the buffer decayed to common when the
        // submission ran.
        assert_eq!(
            list.indirect_state(&b3),
            D3D12_RESOURCE_STATE_COMMON,
            "a reset list starts with a common scratch buffer"
        );
        let b4 = bound(&list, (&src_buf, 0), Some((&count_buf, 0)), (1, 20, 4096), false).unwrap();
        assert_eq!(
            b4.as_raw(),
            b3.as_raw(),
            "grown scratch buffer must persist across reset"
        );
        assert_eq!(list.indirect_state(&b4), args);
        list.close();
        assert_eq!(host.log_messages(), None, "the debug layer found an error");
        host.execute(&[&list]);
        list.reset();
    }

    #[test]
    fn parse_clamp_enabled_defaults_on_and_respects_explicit_disable() {
        use std::ffi::OsStr;
        assert!(parse_clamp_enabled(None));
        for on in ["", "1", " 1 ", "true", "TRUE", "yes", "on"] {
            assert!(
                parse_clamp_enabled(Some(OsStr::new(on))),
                "{on:?} must keep indirect clamp enabled"
            );
        }
        for off in ["0", " 0 ", "false", "FALSE", "no", "NO", "off", "OFF"] {
            assert!(
                !parse_clamp_enabled(Some(OsStr::new(off))),
                "{off:?} must disable indirect clamp"
            );
        }
    }

    #[test]
    fn validate_layout_rejects_misaligned_or_overflowing_direct_arguments() {
        assert_eq!(validate_layout((0, 16, 4), 0, None), Some(64));
        assert_eq!(validate_layout((10, 12, 8), 16, Some(4)), Some(96));
        assert_eq!(validate_layout((0, 12, 1), 0, None), None);
        assert_eq!(validate_layout((0, 16, 1), 2, None), None);
        assert_eq!(validate_layout((0, 16, 1), 0, Some(2)), None);
        assert_eq!(validate_layout((0, 16, u32::MAX), 0, None), None);
    }

    /// The scratch buffer back to the common state after a clamp, for
    /// tests that copy it out on a list of their own.
    fn done_on(list: &ID3D12GraphicsCommandList, scratch: &ID3D12Resource) {
        // SAFETY: an open list and a live scratch buffer in INDIRECT_ARGUMENT.
        unsafe {
            list.ResourceBarrier(&[transition(
                scratch,
                D3D12_RESOURCE_STATE_INDIRECT_ARGUMENT,
                D3D12_RESOURCE_STATE_COMMON,
            )])
        };
    }

    /// A group charges its records in lane order: with 300 left of the
    /// budget, the first draw (100) fits, the second (250) does not and
    /// takes the rest, and the ones after it run empty. (Racing for the
    /// word record by record, which of them fitted was up to the GPU.)
    #[test]
    fn warp_a_group_charges_its_records_in_lane_order() {
        let Some(mut warp) = WarpHarness::new() else {
            return;
        };
        const KIND_DRAW: u32 = 0;
        const STRIDE: u32 = 16;
        const BUDGET_CAP: u32 = 1 << 27;
        let counts = [100u32, 250, 50, 100];
        let total = counts.len() * STRIDE as usize;
        let (src_buf, src_ptr, _) = warp.shared_buffer(total as u64);
        let (_budget_buf, budget_ptr, budget_gpu) = warp.shared_buffer(256);
        let (readback_buf, readback_ptr, _) = warp.shared_buffer(total as u64);
        let dst_buf = warp.default_uav(total as u64);
        // SAFETY: mapped buffers of these sizes.
        unsafe {
            for (i, &count) in counts.iter().enumerate() {
                let words = [count, 1, 7, 9];
                std::ptr::copy_nonoverlapping(
                    words.as_ptr().cast::<u8>(),
                    src_ptr.add(i * STRIDE as usize),
                    16,
                );
            }
            std::ptr::write_bytes(budget_ptr, 0, 256);
            (budget_ptr as *mut u32).write_volatile(BUDGET_CAP - 300);
        }
        assert!(record_clamp_as(
            &warp.clamp,
            &warp.list,
            (&dst_buf, D3D12_RESOURCE_STATE_COMMON),
            budget_gpu,
            (&src_buf, 0),
            None,
            (KIND_DRAW, STRIDE, counts.len() as u32),
            0,
        ));
        // SAFETY: an open list; the clamped records are copied for the check.
        unsafe {
            warp.list.ResourceBarrier(&[transition(
                &dst_buf,
                D3D12_RESOURCE_STATE_INDIRECT_ARGUMENT,
                D3D12_RESOURCE_STATE_COPY_SOURCE,
            )]);
            warp.list.CopyBufferRegion(&readback_buf, 0, &dst_buf, 0, total as u64);
        }
        warp.submit_and_wait();
        // SAFETY: the work is done; mapped buffers.
        let (spent, out) = unsafe {
            (
                (budget_ptr as *const u32).read_volatile(),
                std::slice::from_raw_parts(readback_ptr.cast::<u32>().cast_const(), total / 4)
                    .to_vec(),
            )
        };
        assert_eq!(out[0..4], [100, 1, 7, 9], "the first record fits");
        for i in 1..counts.len() {
            assert_eq!(out[i * 4..i * 4 + 4], [0, 0, 7, 9], "record {i} runs empty");
        }
        assert_eq!(spent, BUDGET_CAP, "the record that did not fit took the rest");
    }

    /// The list's budget starts empty at each submission, wherever it
    /// lives (the card's copy is zeroed in the list): a recording whose
    /// clamps spend all of it is followed, after the reset, by one whose
    /// small record still runs.
    #[test]
    fn host_list_budget_starts_empty_at_each_submission() {
        let Some(host) = super::host() else {
            return;
        };
        let Ok(list) = host.new_list() else {
            return;
        };
        let _ = host.log_messages();
        let worker = compile_cs(&host.device, WORKER_CS).expect("worker shader must compile");
        const KIND_DISPATCH: u32 = 2;
        const STRIDE: u32 = 12;
        let signature = {
            let arg = D3D12_INDIRECT_ARGUMENT_DESC {
                Type: D3D12_INDIRECT_ARGUMENT_TYPE(KIND_DISPATCH as i32),
                ..Default::default()
            };
            let desc = D3D12_COMMAND_SIGNATURE_DESC {
                ByteStride: STRIDE,
                NumArgumentDescs: 1,
                pArgumentDescs: &arg,
                NodeMask: 0,
            };
            let mut sig: Option<ID3D12CommandSignature> = None;
            // SAFETY: a valid single-argument command signature description.
            unsafe {
                host.device
                    .CreateCommandSignature(&desc, None::<&ID3D12RootSignature>, &mut sig)
                    .unwrap();
            }
            sig.unwrap()
        };
        // Three records of 65,535 x 16 groups: about half the budget each.
        let Some((big, big_ptr)) = host.shared_buffer(3 * u64::from(STRIDE)) else {
            return;
        };
        let Some((small, small_ptr)) = host.shared_buffer(u64::from(STRIDE)) else {
            return;
        };
        let Some((hits_buf, hits_ptr)) = host.shared_buffer(256) else {
            return;
        };
        // SAFETY: mapped buffers of these sizes; nothing runs yet.
        unsafe {
            for i in 0..3 {
                let words = [65535u32, 16, 1];
                std::ptr::copy_nonoverlapping(
                    words.as_ptr().cast::<u8>(),
                    big_ptr.add(i * STRIDE as usize),
                    12,
                );
            }
            std::ptr::copy_nonoverlapping([1u32, 1, 1].as_ptr().cast::<u8>(), small_ptr, 12);
            std::ptr::write_bytes(hits_ptr, 0, 256);
        }
        // SAFETY: a live buffer.
        let hits_gpu = unsafe { hits_buf.GetGPUVirtualAddress() };
        for round in 0..2 {
            // Spends the budget (clamped, not executed), then one group.
            bound(&list, (&big, 0), None, (KIND_DISPATCH, STRIDE, 3), false).unwrap();
            let scratch = bound(&list, (&small, 0), None, (KIND_DISPATCH, STRIDE, 1), false).unwrap();
            // SAFETY: an open list; the worker's root signature and
            // pipeline; buffers the test holds until the list has run.
            unsafe {
                list.list.SetComputeRootSignature(&worker.root);
                list.list.SetPipelineState(&worker.pso);
                list.list.SetComputeRootUnorderedAccessView(0, hits_gpu);
                list.list.ExecuteIndirect(&signature, 1, &scratch, 0, None, 0);
            }
            list.close();
            assert_eq!(host.log_messages(), None, "the debug layer found an error");
            host.execute(&[&list]);
            list.reset();
            // SAFETY: the list has run; a mapped buffer.
            let hits = unsafe { (hits_ptr as *const u32).read_volatile() };
            // The small record comes after the big ones spent the budget:
            // it runs empty, in every round alike (none carries over).
            assert_eq!(hits, 0, "round {round}: the spent budget empties the record after it");
        }
        // A recording with only the small record: the budget is empty
        // again, so it runs.
        let scratch = bound(&list, (&small, 0), None, (KIND_DISPATCH, STRIDE, 1), false).unwrap();
        // SAFETY: as above.
        unsafe {
            list.list.SetComputeRootSignature(&worker.root);
            list.list.SetPipelineState(&worker.pso);
            list.list.SetComputeRootUnorderedAccessView(0, hits_gpu);
            list.list.ExecuteIndirect(&signature, 1, &scratch, 0, None, 0);
        }
        list.close();
        assert_eq!(host.log_messages(), None, "the debug layer found an error");
        host.execute(&[&list]);
        list.reset();
        // SAFETY: the list has run; a mapped buffer.
        let hits = unsafe { (hits_ptr as *const u32).read_volatile() };
        assert_eq!(hits, 1, "the budget starts empty at the next submission");
    }

    /// The window check: a record nothing wrote since its window clamped
    /// it compares equal; one written on the card after the window's clamp
    /// (an untracked writer) is found, and named as the window had it and
    /// as it is now.
    #[test]
    fn host_list_window_check_finds_a_record_written_under_its_window() {
        let Some(host) = super::host() else {
            return;
        };
        let Ok(list) = host.new_list() else {
            return;
        };
        let _ = host.log_messages();
        const KIND_DISPATCH: u32 = 2;
        const STEP: u32 = 32;
        let records = [[3u32, 1, 1], [5, 1, 1], [7, 1, 1], [9, 1, 1]];
        let total = records.len() * STEP as usize;
        let (Some((src, src_ptr)), Some((patch, patch_ptr))) =
            (host.shared_buffer(total as u64), host.shared_buffer(64))
        else {
            return;
        };
        // SAFETY: mapped buffers of these sizes; nothing runs yet.
        unsafe {
            std::ptr::write_bytes(src_ptr, 0, total);
            for (i, words) in records.iter().enumerate() {
                std::ptr::copy_nonoverlapping(
                    words.as_ptr().cast::<u8>(),
                    src_ptr.add(i * STEP as usize),
                    12,
                );
            }
            std::ptr::copy_nonoverlapping([11u32, 2, 1].as_ptr().cast::<u8>(), patch_ptr, 12);
        }
        let spec = (KIND_DISPATCH, STEP, records.len() as u32);
        let scratch = bound_ahead(&list, (&src, 0), spec).unwrap();
        // Record 0: nothing wrote it.
        assert!(check_hit(&list, (&src, 0), (KIND_DISPATCH, STEP), &scratch, 0));
        // Record 1 is written on the card after the window's clamp.
        let read = D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE;
        // SAFETY: an open list; `src` was read by the clamp (promoted to a
        // shader read), `patch` is common; both live until the list has run.
        unsafe {
            list.list.ResourceBarrier(&[transition(&src, read, D3D12_RESOURCE_STATE_COPY_DEST)]);
            list.list.CopyBufferRegion(&src, u64::from(STEP), &patch, 0, 12);
            list.list.ResourceBarrier(&[transition(&src, D3D12_RESOURCE_STATE_COPY_DEST, read)]);
        }
        let at = u64::from(STEP);
        assert!(check_hit(&list, (&src, at), (KIND_DISPATCH, STEP), &scratch, at));
        list.close();
        assert_eq!(host.log_messages(), None, "the debug layer found an error");
        host.execute(&[&list]);
        let (checked, differed, window, fresh) = list.window_check().unwrap().take();
        list.reset();
        assert_eq!((checked, differed), (2, 1), "two checks, the written record differs");
        assert_eq!(window[..3], [5, 1, 1], "as the window clamped it");
        assert_eq!(fresh[..3], [11, 2, 1], "as it is now");
    }

    /// Records the dispatch records `groups` (three words each, `step`
    /// bytes apart, the rest of each record ones) into a fresh mapped
    /// buffer of `warp`'s device.
    fn dispatch_records(warp: &WarpHarness, groups: &[u32], step: u32) -> ID3D12Resource {
        let total = groups.len() * step as usize;
        let (buffer, ptr, _) = warp.shared_buffer(total as u64);
        let mut bytes = vec![0xffu8; total];
        for (i, &g) in groups.iter().enumerate() {
            for (w, word) in [g, 1, 1].into_iter().enumerate() {
                let at = i * step as usize + w * 4;
                bytes[at..at + 4].copy_from_slice(&word.to_le_bytes());
            }
        }
        // SAFETY: the mapping holds `total` bytes.
        unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr, total) };
        buffer
    }

    /// Two windows in one list, as `crate::draw` records them: each clamps
    /// its records ahead, then one execution per record reads the scratch
    /// buffer with no barrier between them; the second clamp moves the
    /// scratch buffer straight from arguments back to unordered access.
    /// Every record runs once, with the group count it asked for.
    #[test]
    fn warp_window_executions_read_their_records_with_no_barrier_between() {
        let Some(mut warp) = WarpHarness::new() else {
            return;
        };
        const KIND_DISPATCH: u32 = 2;
        // The title names 12 bytes and keeps its records 32 apart.
        const STRIDE: u32 = 12;
        const STEP: u32 = 32;
        let windows: [&[u32]; 2] = [&[1, 2, 3, 4], &[10, 11, 12, 13]];
        let sources: Vec<ID3D12Resource> =
            windows.iter().map(|g| dispatch_records(&warp, g, STEP)).collect();
        let (_budget_buf, budget_ptr, budget_gpu) = warp.shared_buffer(256);
        let (_hits_buf, hits_ptr, hits_gpu) = warp.shared_buffer(256);
        let dst_buf = warp.default_uav(u64::from(STEP) * 4);
        let sig = warp.signature(KIND_DISPATCH, STRIDE);
        // SAFETY: mapped buffers of these sizes.
        unsafe {
            std::ptr::write_bytes(budget_ptr, 0, 256);
            std::ptr::write_bytes(hits_ptr, 0, 256);
        }
        let mut state = D3D12_RESOURCE_STATE_COMMON;
        for (groups, src) in windows.iter().zip(&sources) {
            assert!(record_clamp_as(
                &warp.clamp,
                &warp.list,
                (&dst_buf, state),
                budget_gpu,
                (src, 0),
                None,
                (KIND_DISPATCH, STEP, groups.len() as u32),
                AHEAD,
            ));
            state = D3D12_RESOURCE_STATE_INDIRECT_ARGUMENT;
            // SAFETY: an open list; the worker's root signature and
            // pipeline; the scratch buffer is readable as arguments.
            unsafe {
                warp.list.SetComputeRootSignature(&warp.worker.root);
                warp.list.SetPipelineState(&warp.worker.pso);
                warp.list.SetComputeRootUnorderedAccessView(0, hits_gpu);
                for i in 0..groups.len() as u64 {
                    warp.list.ExecuteIndirect(&sig, 1, &dst_buf, i * u64::from(STEP), None, 0);
                }
            }
        }
        warp.submit_and_wait();
        // SAFETY: the work is done; mapped buffers.
        let (hits, spent) = unsafe {
            (
                (hits_ptr as *const u32).read_volatile(),
                (budget_ptr as *const u32).read_volatile(),
            )
        };
        let expected: u32 = windows.iter().flat_map(|g| g.iter()).sum();
        assert_eq!(hits, expected, "every record of both windows runs once");
        assert_eq!(spent, expected * 64, "and is charged what it asked for");
    }

    /// The windows of `warp_window_executions_read_their_records_with_no_barrier_between`
    /// through a host list (`bound_ahead`, the list's tracked scratch state,
    /// `Host::execute`), twice: on the first recording the scratch buffer
    /// starts common, on the second (after the reset) it starts common
    /// again. With the debug layer on (`VICTORY_D3D12_DEBUG=1` in a
    /// diagnostics build) no barrier names a state the buffer is not in.
    #[test]
    fn host_list_window_executions_need_no_barrier_of_their_own() {
        let Some(host) = super::host() else {
            return;
        };
        // What earlier tests left in the debug layer's queue, if it is on.
        let _ = host.log_messages();
        let Ok(list) = host.new_list() else {
            return;
        };
        let worker = compile_cs(&host.device, WORKER_CS).expect("worker shader must compile");
        const KIND_DISPATCH: u32 = 2;
        const STRIDE: u32 = 12;
        const STEP: u32 = 32;
        let signature = {
            let arg = D3D12_INDIRECT_ARGUMENT_DESC {
                Type: D3D12_INDIRECT_ARGUMENT_TYPE(KIND_DISPATCH as i32),
                ..Default::default()
            };
            let desc = D3D12_COMMAND_SIGNATURE_DESC {
                ByteStride: STRIDE,
                NumArgumentDescs: 1,
                pArgumentDescs: &arg,
                NodeMask: 0,
            };
            let mut sig: Option<ID3D12CommandSignature> = None;
            // SAFETY: a valid single-argument command signature description.
            unsafe {
                host.device
                    .CreateCommandSignature(&desc, None::<&ID3D12RootSignature>, &mut sig)
                    .unwrap();
            }
            sig.unwrap()
        };
        let windows: [&[u32]; 2] = [&[1, 2, 3, 4], &[10, 11, 12, 13]];
        let mut sources = Vec::new();
        for groups in windows {
            let total = groups.len() * STEP as usize;
            let Some((buffer, ptr)) = host.shared_buffer(total as u64) else {
                return;
            };
            let mut bytes = vec![0xffu8; total];
            for (i, &g) in groups.iter().enumerate() {
                for (w, word) in [g, 1, 1].into_iter().enumerate() {
                    let at = i * STEP as usize + w * 4;
                    bytes[at..at + 4].copy_from_slice(&word.to_le_bytes());
                }
            }
            // SAFETY: the mapping holds `total` bytes.
            unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr, total) };
            sources.push(buffer);
        }
        let Some((hits_buf, hits_ptr)) = host.shared_buffer(256) else {
            return;
        };
        let expected: u32 = windows.iter().flat_map(|g| g.iter()).sum();
        // SAFETY: a live buffer.
        let hits_gpu = unsafe { hits_buf.GetGPUVirtualAddress() };
        for _ in 0..2 {
            // SAFETY: the mapping holds 256 bytes; nothing runs.
            unsafe { std::ptr::write_bytes(hits_ptr, 0, 256) };
            for (groups, src) in windows.iter().zip(&sources) {
                let spec = (KIND_DISPATCH, STEP, groups.len() as u32);
                let scratch = bound_ahead(&list, (src, 0), spec).unwrap();
                assert_eq!(list.indirect_state(&scratch), D3D12_RESOURCE_STATE_INDIRECT_ARGUMENT);
                // SAFETY: an open list; the worker's root signature and
                // pipeline; buffers the test holds until the list has run.
                unsafe {
                    list.list.SetComputeRootSignature(&worker.root);
                    list.list.SetPipelineState(&worker.pso);
                    list.list.SetComputeRootUnorderedAccessView(0, hits_gpu);
                    for i in 0..groups.len() as u64 {
                        let at = i * u64::from(STEP);
                        list.list.ExecuteIndirect(&signature, 1, &scratch, at, None, 0);
                    }
                }
            }
            list.close();
            // With the debug layer on: nothing it objects to was recorded.
            // Read before the submission, which moves the messages to the log.
            assert_eq!(host.log_messages(), None, "the debug layer found an error");
            host.execute(&[&list]);
            list.reset();
            // SAFETY: the list has run; a mapped buffer.
            let hits = unsafe { (hits_ptr as *const u32).read_volatile() };
            assert_eq!(hits, expected, "every record of both windows runs once");
        }
    }

    /// A hoisted clamp of a [`Chain`]: its layout, its records in source
    /// buffer `src` at `src_at`, its count (if any) in count buffer
    /// `count.0` at `count.1`, whether the records past the count are
    /// written empty (`no_count`), and where its records go: chunk `chunk`
    /// at `at`, as `clamp_room` places them.
    #[derive(Clone, Copy, Debug)]
    struct Link {
        kind: u32,
        stride: u32,
        records: u32,
        src: usize,
        src_at: u32,
        count: Option<(usize, u32)>,
        fill: bool,
        chunk: usize,
        at: u32,
    }

    /// Hoisted clamps one after another with their buffers, CPU-mapped on
    /// the software adapter (the bytes they were made with kept for the
    /// model): sources, counts, chunks of `chunk_bytes`; and the budget's
    /// three words before them.
    struct Chain {
        links: Vec<Link>,
        sources: Vec<(ID3D12Resource, u64, Vec<u8>)>,
        counts: Vec<(ID3D12Resource, u64, Vec<u8>)>,
        chunks: Vec<(ID3D12Resource, *mut u8, u64)>,
        chunk_bytes: usize,
        start: [u32; 3],
    }

    const CAP: u32 = 1 << 27;
    /// What a chunk holds where no record was written.
    const UNWRITTEN: u32 = 0xCDCD_CDCD;

    fn word(bytes: &[u8], at: usize) -> u32 {
        u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap())
    }

    /// Where `clamp_room` puts `bytes` of records when it last gave room up
    /// to `next` (chunk, offset), in chunks of `chunk_bytes`.
    fn place(next: &mut (usize, usize), bytes: u32, chunk_bytes: usize) -> (usize, u32) {
        let bytes = (bytes as usize).next_multiple_of(16);
        if next.1 + bytes > chunk_bytes {
            *next = (next.0 + 1, 0);
        }
        let at = (next.0, next.1 as u32);
        next.1 += bytes;
        at
    }

    fn chain_of(
        warp: &WarpHarness,
        links: Vec<Link>,
        (sources, counts): (Vec<Vec<u8>>, Vec<Vec<u8>>),
        chunk_bytes: usize,
        start: [u32; 3],
    ) -> Chain {
        let up = |bytes: Vec<u8>| {
            let (buffer, ptr, gpu) = warp.shared_buffer(bytes.len() as u64);
            // SAFETY: the mapping holds `bytes.len()` bytes.
            unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr, bytes.len()) };
            (buffer, gpu, bytes)
        };
        let chunks = links.iter().map(|l| l.chunk + 1).max().unwrap_or(0);
        Chain {
            links,
            sources: sources.into_iter().map(&up).collect(),
            counts: counts.into_iter().map(&up).collect(),
            chunks: (0..chunks).map(|_| warp.shared_buffer(chunk_bytes as u64)).collect(),
            chunk_bytes,
            start,
        }
    }

    /// One record as `SOURCE` clamps a hoisted clamp's: the words that
    /// replace its first ones and how many, its cost, and the group count
    /// a mesh record asked for.
    fn clamped(kind: u32, w: [u32; 3]) -> ([u32; 3], usize, u32, Option<u32>) {
        match kind {
            0 | 1 => {
                let count = w[0].min(1 << 24);
                let instances = w[1].min((1 << 26) / count.max(1));
                if count >= 1 << 23 {
                    ([0; 3], 2, 0, None)
                } else {
                    ([count, instances, 0], 2, count * instances, None)
                }
            }
            2 => {
                let mut g = w.map(|x| x.min(65535));
                if g[2] != 0 && g[0] * g[1] > (1 << 20) / g[2] {
                    g = [0; 3];
                }
                (g, 3, g[0] * g[1] * g[2] * 64, None)
            }
            _ => {
                let g = [w[0].min(65535), w[1].min(1), w[2].min(1)];
                (g, 3, g[0] * g[1] * g[2] * 128, Some(w[0]))
            }
        }
    }

    /// What a [`Chain`] must leave, made on the CPU with every record
    /// charged after the one before it, in the chain's order.
    struct Model {
        chunks: Vec<Vec<u32>>,
        budget: [u32; 3],
        /// Per link: clamps recorded one by one may leave other records
        /// than this order does (more than one group of 64, whose costs do
        /// not all fit what is left: which fit then depends on the order
        /// the card runs the groups in).
        loose: Vec<bool>,
        /// Records left empty past the budget; links whose count left them
        /// no record; the first link with an emptied record; records past a
        /// count written empty.
        emptied: usize,
        uncounted: usize,
        first_emptied: Option<usize>,
        filled: usize,
    }

    fn model(chain: &Chain) -> Model {
        let mut chunks = vec![vec![UNWRITTEN; chain.chunk_bytes / 4]; chain.chunks.len()];
        let [mut at, mut largest, mut all] = chain.start;
        let (mut loose, mut emptied, mut uncounted, mut first_emptied, mut filled) = (Vec::new(), 0, 0, None, 0);
        for (i, l) in chain.links.iter().enumerate() {
            let src = &chain.sources[l.src].2;
            let mut records = l.records;
            if let Some((b, off)) = l.count {
                records = records.min(word(&chain.counts[b].2, off as usize));
                uncounted += usize::from(records == 0);
            }
            let (before, mut asked_cost) = (at, 0u64);
            for k in 0..records {
                let base = (l.src_at + k * l.stride) as usize;
                let w = [word(src, base), word(src, base + 4), word(src, base + 8)];
                let (first, n, cost, asked) = clamped(l.kind, w);
                if let Some(asked) = asked {
                    largest = largest.max(asked);
                    all = all.wrapping_add(asked.min(1 << 24));
                }
                let fits = cost == 0 || (at < CAP && CAP - at >= cost);
                if cost > 0 {
                    at = if at >= CAP || CAP - at <= cost { CAP } else { at + cost };
                }
                if !fits {
                    emptied += 1;
                    first_emptied.get_or_insert(i);
                }
                asked_cost += u64::from(cost);
                let out = &mut chunks[l.chunk][(l.at + k * l.stride) as usize / 4..];
                for (j, o) in out[..l.stride as usize / 4].iter_mut().enumerate() {
                    *o = if j >= n { word(src, base + 4 * j) } else if fits { first[j] } else { 0 };
                }
            }
            if l.fill && l.count.is_some() {
                for k in records..l.records {
                    let at = (l.at + k * l.stride) as usize / 4;
                    chunks[l.chunk][at..at + l.stride as usize / 4].fill(0);
                    filled += 1;
                }
            }
            loose.push(records > 64 && before < CAP && u64::from(before) + asked_cost > u64::from(CAP));
        }
        Model { chunks, budget: [at, largest, all], loose, emptied, uncounted, first_emptied, filled }
    }

    /// Runs `chain` on `warp`: one clamp after another as `bound_before`
    /// records them (no `pass`), or kept in batches and recorded by `pass`
    /// as with `VICTORY_CLAMP_BATCH=1`. What the chunks and the budget's
    /// words hold afterwards, and each batch's clamps and records.
    fn run(warp: &mut WarpHarness, chain: &Chain, pass: Option<&Clamp>) -> (Vec<Vec<u32>>, [u32; 3], Vec<(usize, u32)>) {
        let records: u64 = chain.links.iter().map(|l| u64::from(l.records)).sum();
        let (_budget, budget_ptr, budget_gpu) = warp.shared_buffer(256);
        let table_len = chain.links.len() as u64 * (32 + 256) + 4 * records + 256;
        let (_table, table_ptr, table_gpu) = warp.shared_buffer(table_len);
        // SAFETY: mappings of these sizes; nothing runs.
        unsafe {
            for (_, ptr, _) in &chain.chunks {
                std::slice::from_raw_parts_mut(ptr.cast::<u32>(), chain.chunk_bytes / 4).fill(UNWRITTEN);
            }
            std::ptr::write_bytes(budget_ptr, 0, 256);
            std::ptr::copy_nonoverlapping(chain.start.as_ptr().cast::<u8>(), budget_ptr, 12);
        }
        let l = warp.list.clone();
        let mut passes = Vec::new();
        {
            let mut used = 0;
            let mut flush = |batch: &mut Batch| {
                if batch.kept.is_empty() {
                    return;
                }
                passes.push((batch.kept.len(), batch.records));
                let at = used;
                used += batch.table_bytes().next_multiple_of(256);
                assert!(used <= table_len);
                // SAFETY: inside the table's mapping.
                let table = (table_gpu + at, unsafe { table_ptr.add(at as usize) });
                assert!(batch.record(&l, pass, &warp.clamp, Some(table)), "a batch is one pass");
            };
            let mut batch = Batch::default();
            for link in &chain.links {
                let kept = Kept {
                    spec: (link.kind, link.stride, link.records),
                    args: (chain.sources[link.src].1, link.src_at),
                    count: link.count.map(|(b, at)| (chain.counts[b].1, at)),
                    fill: link.fill,
                    out: (chain.chunks[link.chunk].2, link.at),
                };
                if pass.is_some() {
                    if !batch.take(kept, budget_gpu) {
                        flush(&mut batch);
                        assert!(batch.take(kept, budget_gpu), "an empty batch takes any clamp");
                    }
                } else {
                    let count = kept.count.map_or(kept.args.0, |(at, _)| at);
                    let dst = kept.out.0 + u64::from(kept.out.1);
                    record_hoisted(&warp.clamp, &l, kept.constants(), (kept.args.0, count, dst, budget_gpu), true);
                }
            }
            flush(&mut batch);
        }
        warp.submit_and_wait();
        // SAFETY: the work is done; mappings of these sizes.
        unsafe {
            let chunks = chain.chunks.iter().map(|(_, ptr, _)| std::slice::from_raw_parts(ptr.cast::<u32>(), chain.chunk_bytes / 4).to_vec()).collect();
            let budget = [0, 1, 2].map(|k| budget_ptr.cast::<u32>().add(k).read_volatile());
            (chunks, budget, passes)
        }
    }

    /// The first word in which `got` and `want` differ, outside the
    /// records of the links `skip` names, with the link that wrote it.
    fn differs(chain: &Chain, got: &[Vec<u32>], want: &[Vec<u32>], skip: impl Fn(usize) -> bool) -> Option<String> {
        let owner = |chunk: usize, at: usize| {
            chain.links.iter().position(|l| l.chunk == chunk && (l.at as usize..(l.at + l.records * l.stride) as usize).contains(&at))
        };
        for (c, (got, want)) in got.iter().zip(want).enumerate() {
            for (i, (&g, &w)) in got.iter().zip(want).enumerate() {
                if g == w {
                    continue;
                }
                let link = owner(c, 4 * i);
                if !link.is_some_and(&skip) {
                    let named = link.map(|k| chain.links[k]);
                    return Some(format!("chunk {c}, word {i}: {g:#x}, want {w:#x} (link {link:?}: {named:?})"));
                }
            }
        }
        None
    }

    /// xorshift64*, for chains that are the same on every run.
    struct Rng(u64);

    impl Rng {
        fn next(&mut self) -> u32 {
            self.0 ^= self.0 >> 12;
            self.0 ^= self.0 << 25;
            self.0 ^= self.0 >> 27;
            (self.0.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 32) as u32
        }

        fn below(&mut self, n: u32) -> u32 {
            self.next() % n.max(1)
        }

        fn pick(&mut self, from: &[u32]) -> u32 {
            from[self.below(from.len() as u32) as usize]
        }

        fn bytes(&mut self, len: u32) -> Vec<u8> {
            (0..len).map(|_| self.next() as u8).collect()
        }
    }

    /// The first words of a record of `kind` as the title may leave them:
    /// plausible (draws of up to `size` vertices), or one time in `wild`
    /// garbage or at the bounds.
    fn record_words(rng: &mut Rng, kind: u32, (wild, size): (u32, u32)) -> [u32; 3] {
        let r = rng.next();
        match (kind, rng.below(wild) == 0) {
            (0 | 1, true) => [
                rng.pick(&[1 << 22, (1 << 23) - 1, 1 << 23, 1 << 24, (1 << 24) + 1, u32::MAX, r]),
                rng.pick(&[0, 1, 16, 1000, u32::MAX, r >> 3]),
                rng.next(),
            ],
            (0 | 1, false) => [rng.below(size), rng.pick(&[0, 1, 1, 2, 4]), rng.next()],
            (2, true) => [0; 3].map(|_| rng.pick(&[0, 1, 1024, 65535, 65536, u32::MAX, r])),
            (2, false) => [rng.below(1 + size / 46), rng.below(1 + size / 46), rng.below(3)],
            (_, true) => [rng.pick(&[65535, 65536, u32::MAX, r]), rng.pick(&[0, 1, 2, u32::MAX]), rng.pick(&[0, 1, 2, u32::MAX])],
            (_, false) => [rng.below(size / 5), rng.pick(&[0, 1, 1, 1]), rng.pick(&[0, 1, 1, 1])],
        }
    }

    /// A chain of `n` hoisted clamps of every kind from `seed`: records 1
    /// to 1,500 (most one, some in more than one group of 64, some more
    /// than a tile of the pass), from 20 source buffers (more than a
    /// batch's views), some bounded by counts (none, some, all of the
    /// records, more, garbage), into 256 KiB chunks; the budget before them
    /// anywhere from empty to spent. Seeds 0 to 11 take each mix of garbage
    /// (one record in 8, 40, 400, none) and of plausible costs once.
    fn random_chain(warp: &WarpHarness, seed: u64, n: usize) -> Chain {
        const SOURCES: usize = 20;
        const COUNTS: usize = 3;
        let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
        let mix = ([8, 40, 400, u32::MAX][seed as usize % 4], [3000, 300, 30][seed as usize / 4 % 3]);
        let mut src_len = [0u32; SOURCES];
        let mut count_len = [0u32; COUNTS];
        let mut next = (0, 0);
        let mut links = Vec::new();
        for _ in 0..n {
            let kind = rng.pick(&[0, 0, 1, 1, 10, 10, 2]);
            let stride = min_stride(kind).unwrap() + 4 * rng.below(6);
            let records = match rng.below(20) {
                0..=10 => 1,
                11..=15 => 2 + rng.below(63),
                16..=18 => 65 + rng.below(136),
                _ => 201 + rng.below(1300),
            };
            let src = rng.below(SOURCES as u32) as usize;
            let src_at = src_len[src] + 4 * rng.below(16);
            src_len[src] = src_at + records * stride;
            let count = (rng.below(5) < 2).then(|| {
                let b = rng.below(COUNTS as u32) as usize;
                let at = count_len[b];
                count_len[b] += 4 * (1 + rng.below(3));
                (b, at)
            });
            let (chunk, at) = place(&mut next, records * stride, 256 << 10);
            // Every other counted clamp of a few records written whole.
            let fill = count.is_some() && records <= NO_COUNT_MOST && links.len() % 2 == 0;
            links.push(Link { kind, stride, records, src, src_at, count, fill, chunk, at });
        }
        let mut sources: Vec<Vec<u8>> = src_len.iter().map(|&len| rng.bytes(len + 64)).collect();
        let mut counts: Vec<Vec<u8>> = count_len.iter().map(|&len| rng.bytes(len + 64)).collect();
        for link in &links {
            for k in 0..link.records {
                let at = (link.src_at + k * link.stride) as usize;
                for (i, w) in record_words(&mut rng, link.kind, mix).into_iter().enumerate() {
                    sources[link.src][at + 4 * i..at + 4 * i + 4].copy_from_slice(&w.to_le_bytes());
                }
            }
            if let Some((b, at)) = link.count {
                let value = match rng.below(20) {
                    0..=3 => 0,
                    4..=9 => rng.below(link.records),
                    10..=12 => link.records,
                    13..=15 => link.records + 1 + rng.below(1000),
                    _ => u32::MAX,
                };
                counts[b][at as usize..at as usize + 4].copy_from_slice(&value.to_le_bytes());
            }
        }
        let window = rng.pick(&[CAP, 1 << 22, 1 << 16, 1]);
        let start = [CAP - rng.below(window), rng.pick(&[0, 7, 70_000]), rng.next()];
        chain_of(warp, links, (sources, counts), 256 << 10, start)
    }

    /// Batched hoisted clamps (`VICTORY_CLAMP_BATCH=1`) leave what clamps
    /// recorded one after another leave, on the software adapter: random
    /// chains of every kind of record (garbage and records at the bounds
    /// among them, records past the budget, counts of none, some, all and
    /// more), in batches ended by views, chunks and the chain's end. Every
    /// word of every chunk (records written, and records past a count left
    /// alone) and the budget's three words are compared both ways, and
    /// with the chain's order on the CPU. The one-by-one clamps are
    /// compared with the batch except in clamps of more than one group
    /// whose records do not all fit what is left: which fit there depends
    /// on the order the card runs the groups in (the batch takes the
    /// records' order, which the model checks).
    #[test]
    fn warp_clamp_batch_leaves_what_clamps_one_after_another_leave() {
        let Some(mut warp) = WarpHarness::new() else {
            return;
        };
        let pass = make_from(&warp.device, &batch_source()).expect("the batch pass must compile on WARP");
        let (mut emptied, mut uncounted, mut loose, mut batches, mut widest, mut late, mut filled) = (0, 0, 0, 0, 0, 0, 0);
        const SEEDS: u64 = 12;
        const LINKS: usize = 300;
        for seed in 0..SEEDS {
            let chain = random_chain(&warp, seed, LINKS);
            let want = model(&chain);
            let (one_by_one, budget_one, _) = run(&mut warp, &chain, None);
            let (batched, budget_batched, passes) = run(&mut warp, &chain, Some(&pass));
            assert_eq!(budget_batched, want.budget, "seed {seed}: the batch's budget words");
            assert_eq!(budget_one, budget_batched, "seed {seed}: the budget words, one by one and batched");
            if let Some(d) = differs(&chain, &batched, &want.chunks, |_| false) {
                panic!("seed {seed}: batched, against the chain's order: {d}");
            }
            if let Some(d) = differs(&chain, &one_by_one, &batched, |k| want.loose[k]) {
                panic!("seed {seed}: one by one, against batched: {d}");
            }
            emptied += want.emptied;
            uncounted += want.uncounted;
            loose += want.loose.iter().filter(|&&l| l).count();
            batches += passes.len();
            widest = widest.max(passes.iter().map(|p| p.1).max().unwrap_or(0));
            late += usize::from(want.first_emptied.is_some_and(|i| i >= LINKS / 10));
            filled += want.filled;
            eprintln!(
                "seed {seed}: {} batches for {LINKS} clamps, first emptied record in link {:?}, {} emptied, budget {:?}",
                passes.len(),
                want.first_emptied,
                want.emptied,
                want.budget
            );
        }
        eprintln!("{emptied} records emptied past the budget, {uncounted} clamps counted to none, {loose} clamps left out of the one-by-one compare, {batches} batches, widest {widest} records");
        // What the chains must have held for the compare to say something.
        assert!(emptied > 1000, "records past the budget");
        assert!(uncounted > 10, "clamps whose count leaves no record");
        assert!(filled > 50, "records past a count written empty ({filled})");
        assert!(late >= 2, "chains whose budget runs out after their first tenth");
        assert!(widest > 2048, "batches of more than two tiles of the pass");
        assert!(batches > SEEDS as usize && batches < SEEDS as usize * LINKS / 4, "batches of many clamps, ended by views and chunks");
    }

    /// The batch charges a clamp's records in their order across the
    /// pass's tiles of 1,024: with 1,000 left of the budget, the first
    /// 1,000 of 1,500 one-vertex draws run and the others run empty; after
    /// them a draw that costs runs empty, one that costs nothing keeps its
    /// counts. Clamped alone, the clamp's groups charge in an order the
    /// card picks: 1,000 of its draws run there too.
    #[test]
    fn warp_clamp_batch_charges_a_clamp_in_record_order_across_tiles() {
        let Some(mut warp) = WarpHarness::new() else {
            return;
        };
        let pass = make_from(&warp.device, &batch_source()).expect("the batch pass must compile on WARP");
        const STRIDE: u32 = 16;
        let mut src: Vec<u8> = (0..1500u32).flat_map(|i| [1, 1, i, 7]).flat_map(u32::to_le_bytes).collect();
        let tail = src.len() as u32;
        src.extend([5u32, 1, 9, 9, 0, 3, 8, 8].into_iter().flat_map(u32::to_le_bytes));
        let link = |records, src_at, at| Link { kind: 0, stride: STRIDE, records, src: 0, src_at, count: None, fill: false, chunk: 0, at };
        let links = vec![link(1500, 0, 0), link(1, tail, 1500 * STRIDE), link(1, tail + 16, 1501 * STRIDE)];
        let chain = chain_of(&warp, links, (vec![src], Vec::new()), 64 << 10, [CAP - 1000, 0, 0]);
        let want = model(&chain);
        assert!(want.loose[0], "one by one, which draws run depends on the order of the groups");
        let (batched, budget, passes) = run(&mut warp, &chain, Some(&pass));
        assert_eq!(passes, [(3, 1502)], "one pass");
        assert_eq!(budget, [CAP, 0, 0], "the last draw that fitted spent the budget");
        let record = |out: &[Vec<u32>], i: usize| out[0][i * 4..i * 4 + 4].to_vec();
        for i in 0..1500 {
            let ran = u32::from(i < 1000);
            assert_eq!(record(&batched, i), [ran, ran, i as u32, 7], "draw {i}");
        }
        assert_eq!(record(&batched, 1500), [0, 0, 9, 9], "a draw that costs runs empty");
        assert_eq!(record(&batched, 1501), [0, 3, 8, 8], "a draw that costs nothing keeps its counts");
        assert_eq!(differs(&chain, &batched, &want.chunks, |_| false), None);
        let (one_by_one, budget_one, _) = run(&mut warp, &chain, None);
        assert_eq!(budget_one, budget);
        let ran = (0..1500).filter(|&i| record(&one_by_one, i)[..2] == [1, 1]).count();
        assert_eq!(ran, 1000, "one by one, 1,000 of the draws run");
        assert_eq!(differs(&chain, &one_by_one, &batched, |k| k == 0), None);
    }

    /// Hoisted clamps through a host list (`bound_before`, `clamp_end`),
    /// with `VICTORY_CLAMP_BATCH` off and on: the records the draws would
    /// read are the same, byte for byte (counted clamps of up to 16 records
    /// written whole, empty past the count), and with the debug layer on
    /// (`VICTORY_D3D12_DEBUG=1`, diagnostics) nothing it objects to is
    /// recorded.
    #[test]
    fn host_list_clamp_batch_writes_what_the_chain_writes() {
        let Some(host) = super::host() else {
            return;
        };
        let _one = crate::live::tests_lock();
        struct Reset;
        impl Drop for Reset {
            fn drop(&mut self) {
                crate::live::reset("CLAMP_BATCH");
            }
        }
        let _reset = Reset;
        let _ = host.log_messages();
        // Sixty clamps of draws, indexed draws and mesh records (one group
        // each) from two buffers, a quarter bounded by counts; the budget
        // runs out partway.
        let mut rng = Rng(0x5eed);
        let mut bytes = [rng.bytes(64 << 10), rng.bytes(64 << 10), rng.bytes(4096)];
        let mut ends = [0u32; 2];
        let mut specs = Vec::new();
        for i in 0..60u32 {
            let kind = [0, 1, 10][i as usize % 3];
            let stride = min_stride(kind).unwrap() + 4 * (i % 3);
            let records = 1 + rng.below(60);
            let b = i as usize % 2;
            let at = ends[b];
            ends[b] += records * stride;
            for k in 0..records {
                let base = (at + k * stride) as usize;
                for (w, word) in record_words(&mut rng, kind, (6, 3000)).into_iter().enumerate() {
                    bytes[b][base + 4 * w..base + 4 * w + 4].copy_from_slice(&word.to_le_bytes());
                }
            }
            let count = (i % 4 == 0).then(|| {
                let value = rng.pick(&[0, 1, records / 2, records, u32::MAX]);
                bytes[2][4 * i as usize..4 * i as usize + 4].copy_from_slice(&value.to_le_bytes());
                (u64::from(4 * i), value)
            });
            specs.push((b, u64::from(at), count, (kind, stride, records)));
        }
        let mut buffers = Vec::new();
        for data in &bytes {
            let Some((buffer, ptr)) = host.shared_buffer(data.len() as u64) else {
                return;
            };
            // SAFETY: the mapping holds `data.len()` bytes; nothing runs.
            unsafe { std::ptr::copy_nonoverlapping(data.as_ptr(), ptr, data.len()) };
            buffers.push(buffer);
        }
        let total: u64 = specs.iter().map(|s| u64::from(s.3.1 * s.3.2)).sum();
        let mut outs = Vec::new();
        for on in [0, 1] {
            crate::live::set("CLAMP_BATCH", on).unwrap();
            let (Ok(list), Some((back, back_ptr))) = (host.new_list(), host.shared_buffer(total)) else {
                return;
            };
            let placed: Vec<_> = specs
                .iter()
                .map(|&(b, at, count, spec)| {
                    let fill = count.is_some() && spec.2 <= NO_COUNT_MOST;
                    bound_before(&list, (&buffers[b], at), count.map(|c| (&buffers[2], c.0)), spec, fill).expect("a hoisted clamp")
                })
                .collect();
            assert_eq!(list.clamp_batch().on, Some(on == 1), "the switch, read at the first clamp");
            list.clamp_end();
            let mut chunks: Vec<&ID3D12Resource> = Vec::new();
            for (buffer, _) in &placed {
                if !chunks.iter().any(|c| c.as_raw() == buffer.as_raw()) {
                    chunks.push(buffer);
                }
            }
            let args = D3D12_RESOURCE_STATE_INDIRECT_ARGUMENT;
            let copy = D3D12_RESOURCE_STATE_COPY_SOURCE;
            // SAFETY: an open list; the chunks are readable as arguments
            // after `clamp_end` and go back to that; buffers the test holds
            // until the list has run.
            unsafe {
                let out: Vec<_> = chunks.iter().map(|c| transition(c, args, copy)).collect();
                list.list.ResourceBarrier(&out);
                let mut to = 0;
                for ((buffer, at), s) in placed.iter().zip(&specs) {
                    let len = u64::from(s.3.1 * s.3.2);
                    list.list.CopyBufferRegion(&back, to, buffer, *at, len);
                    to += len;
                }
                let back_in: Vec<_> = chunks.iter().map(|c| transition(c, copy, args)).collect();
                list.list.ResourceBarrier(&back_in);
            }
            list.close();
            assert_eq!(host.log_messages(), None, "the debug layer found an error");
            host.execute(&[&list]);
            list.reset();
            // SAFETY: the list has run; the mapping holds `total` bytes.
            outs.push(unsafe { std::slice::from_raw_parts(back_ptr.cast_const(), total as usize).to_vec() });
        }
        // Only records under their count are written (the chunks' other
        // bytes are whatever the card's memory held).
        let (mut from, mut filled) = (0, 0);
        for (i, &(_, _, count, (_, stride, records))) in specs.iter().enumerate() {
            let under = count.map_or(records, |c| records.min(c.1));
            let fill = count.is_some() && records <= NO_COUNT_MOST;
            let written = if fill { records } else { under };
            let range = from..from + (stride * written) as usize;
            assert!(outs[0][range.clone()] == outs[1][range], "clamp {i}: the batch writes the records the clamp alone writes");
            if fill {
                let past = from + (stride * under) as usize..from + (stride * records) as usize;
                assert!(outs[1][past.clone()].iter().all(|&b| b == 0), "clamp {i}: records past the count are empty");
                filled += past.len();
            }
            from += (stride * records) as usize;
        }
        assert!(filled > 0, "some records past a count were written empty");
    }

    /// A point-list pipeline on `device` whose vertices go nowhere (no
    /// render target): what its draws ran is what the input assembler read,
    /// counted by a pipeline statistics query.
    fn point_pipeline(device: &ID3D12Device) -> Clamp {
        const VS: &str = r#"
#define RS "RootFlags(0)"
[RootSignature(RS)]
float4 main(uint v : SV_VertexID) : SV_Position { return float4(0, 0, 0.5, 1); }
"#;
        let mut code: Option<ID3DBlob> = None;
        // SAFETY: the source and names outlive the call; the blob's own
        // bytes; the description points at live data and never releases its
        // pointer to the root signature.
        unsafe {
            D3DCompile(VS.as_ptr().cast(), VS.len(), s!("points"), None, None, s!("main"), s!("vs_5_1"), 0, 0, &mut code, None)
                .expect("the point shader must compile");
            let code = code.unwrap();
            let bytes = std::slice::from_raw_parts(code.GetBufferPointer().cast::<u8>(), code.GetBufferSize());
            let root: ID3D12RootSignature = device.CreateRootSignature(0, bytes).unwrap();
            let mut blend = D3D12_BLEND_DESC::default();
            blend.RenderTarget[0].RenderTargetWriteMask = 0xf;
            let desc = D3D12_GRAPHICS_PIPELINE_STATE_DESC {
                pRootSignature: std::mem::transmute_copy(&root),
                VS: D3D12_SHADER_BYTECODE { pShaderBytecode: bytes.as_ptr().cast(), BytecodeLength: bytes.len() },
                BlendState: blend,
                SampleMask: u32::MAX,
                RasterizerState: D3D12_RASTERIZER_DESC {
                    FillMode: D3D12_FILL_MODE_SOLID,
                    CullMode: D3D12_CULL_MODE_NONE,
                    DepthClipEnable: true.into(),
                    ..Default::default()
                },
                PrimitiveTopologyType: D3D12_PRIMITIVE_TOPOLOGY_TYPE_POINT,
                SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                ..Default::default()
            };
            let pso = device.CreateGraphicsPipelineState(&desc).unwrap();
            Clamp { root, pso }
        }
    }

    /// What one indirect command did, each way: what ran (a bit per record
    /// that ran: record `i` asks for `1 << i`), the clamped records, the
    /// budget's first word.
    struct Ran {
        ran: [u64; 2],
        records: [Vec<u32>; 2],
        budget: [u32; 2],
    }

    /// One indirect command of `kind`, `max` records `stride` bytes apart
    /// (record `i` asks for `1 << i` groups, vertices or indices, one
    /// instance; from record `garbage` on all ones), with `count` in a count
    /// buffer: clamped and run with the count buffer (way 0), and clamped
    /// with the records past the count written empty and run without it
    /// (way 1). Dispatches are counted by the worker, draws by pipeline
    /// statistics; mesh records are clamped only.
    fn run_counted(warp: &mut WarpHarness, points: &Clamp, (kind, stride, max): (u32, u32, u32), count: u32, garbage: u32) -> Ran {
        use windows::Win32::Foundation::RECT;
        use windows::Win32::Graphics::Direct3D::D3D_PRIMITIVE_TOPOLOGY_POINTLIST;
        use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_R16_UINT;
        let bytes = stride * max;
        let mut words = Vec::new();
        for i in 0..max {
            let first = match kind {
                2 | 10 => vec![1 << i, 1, 1],
                0 => vec![1 << i, 1, 0, 0],
                _ => vec![1 << i, 1, 0, 0, 0],
            };
            let mut record: Vec<u32> = (0..stride / 4).map(|w| 0xA000 + 16 * i + w).collect();
            record[..first.len()].copy_from_slice(&first);
            if i >= garbage {
                record.fill(u32::MAX);
            }
            words.extend(record);
        }
        let (src_buf, src_ptr, _) = warp.shared_buffer(u64::from(bytes));
        let (count_buf, count_ptr, _) = warp.shared_buffer(16);
        let (_indices, _, indices_gpu) = warp.shared_buffer(2 << 16);
        let (stats, stats_ptr, _) = warp.shared_buffer(2 * 256);
        let (budgets, ran_bufs): (Vec<_>, Vec<_>) = (0..2).map(|_| (warp.shared_buffer(256), warp.shared_buffer(256))).unzip();
        // Where the records go, holding records of ones before (a group or
        // a vertex each: what a record the clamp did not write would run).
        let dst: Vec<_> = (0..2).map(|_| warp.shared_buffer(u64::from(bytes))).collect();
        let sig = (kind != 10).then(|| warp.signature(kind, stride));
        let mut heap: Option<ID3D12QueryHeap> = None;
        // SAFETY: mapped buffers of these sizes (the index buffer is zeros
        // as made); a valid query heap description; nothing runs yet.
        unsafe {
            std::ptr::copy_nonoverlapping(words.as_ptr().cast::<u8>(), src_ptr, bytes as usize);
            std::ptr::write_bytes(count_ptr, 0x5a, 16);
            count_ptr.cast::<u32>().add(1).write_volatile(count);
            for ((_, b, _), (_, r, _)) in budgets.iter().zip(&ran_bufs) {
                std::ptr::write_bytes(*b, 0, 256);
                std::ptr::write_bytes(*r, 0, 256);
            }
            for (_, d, _) in &dst {
                std::slice::from_raw_parts_mut(d.cast::<u32>(), (bytes / 4) as usize).fill(1);
            }
            warp.device
                .CreateQueryHeap(
                    &D3D12_QUERY_HEAP_DESC { Type: D3D12_QUERY_HEAP_TYPE_PIPELINE_STATISTICS, Count: 2, NodeMask: 0 },
                    &mut heap,
                )
                .unwrap();
        }
        let heap = heap.unwrap();
        let l = warp.list.clone();
        for way in 0..2 {
            assert!(record_clamp_filled(
                &warp.clamp,
                &l,
                (&dst[way].0, D3D12_RESOURCE_STATE_COMMON),
                budgets[way].2,
                (&src_buf, 0),
                Some((&count_buf, 4)),
                (kind, stride, max),
                (false, way == 1),
            ));
            let (counted, count_at) = if way == 0 { (Some(&count_buf), 4) } else { (None, 0) };
            // SAFETY: an open list; the worker's or the point pipeline's
            // root signature and pipeline; the clamp left `dst` readable as
            // arguments (read back through its mapping once the list has
            // run); buffers live until then.
            unsafe {
                if let Some(sig) = &sig {
                    if kind == 2 {
                        l.SetComputeRootSignature(&warp.worker.root);
                        l.SetPipelineState(&warp.worker.pso);
                        l.SetComputeRootUnorderedAccessView(0, ran_bufs[way].2);
                        l.ExecuteIndirect(sig, max, &dst[way].0, 0, counted, count_at);
                    } else {
                        l.SetGraphicsRootSignature(&points.root);
                        l.SetPipelineState(&points.pso);
                        l.IASetPrimitiveTopology(D3D_PRIMITIVE_TOPOLOGY_POINTLIST);
                        let view = D3D12_INDEX_BUFFER_VIEW { BufferLocation: indices_gpu, SizeInBytes: 2 << 16, Format: DXGI_FORMAT_R16_UINT };
                        l.IASetIndexBuffer(Some(&view));
                        l.RSSetViewports(&[D3D12_VIEWPORT { Width: 1.0, Height: 1.0, MaxDepth: 1.0, ..Default::default() }]);
                        l.RSSetScissorRects(&[RECT { left: 0, top: 0, right: 1, bottom: 1 }]);
                        l.BeginQuery(&heap, D3D12_QUERY_TYPE_PIPELINE_STATISTICS, way as u32);
                        l.ExecuteIndirect(sig, max, &dst[way].0, 0, counted, count_at);
                        l.EndQuery(&heap, D3D12_QUERY_TYPE_PIPELINE_STATISTICS, way as u32);
                    }
                }
            }
        }
        if kind <= 1 {
            // SAFETY: an open list; both queries ended; the statistics
            // buffer holds both.
            unsafe { l.ResolveQueryData(&heap, D3D12_QUERY_TYPE_PIPELINE_STATISTICS, 0, 2, &stats, 0) };
        }
        warp.submit_and_wait();
        let size = std::mem::size_of::<D3D12_QUERY_DATA_PIPELINE_STATISTICS>();
        // SAFETY: the work is done; mapped buffers of these sizes.
        unsafe {
            let ran = [0, 1].map(|way| match kind {
                2 => u64::from(ran_bufs[way].1.cast::<u32>().read_volatile()),
                0 | 1 => stats_ptr.add(way * size).cast::<D3D12_QUERY_DATA_PIPELINE_STATISTICS>().read_unaligned().IAVertices,
                _ => 0,
            });
            let records = [0, 1].map(|way| std::slice::from_raw_parts(dst[way].1.cast::<u32>(), (bytes / 4) as usize).to_vec());
            let budget = [0, 1].map(|way| budgets[way].1.cast::<u32>().read_volatile());
            Ran { ran, records, budget }
        }
    }

    /// Indirect commands run without their count buffer (`no_count`) run
    /// what they run with it, on the software adapter: dispatches (counted
    /// by the worker), draws and indexed draws (counted by the input
    /// assembler), and mesh records (clamped only), of 1, 5 and 16 records,
    /// with counts of none, one, half, all, more than all and all ones, and
    /// with garbage records under the count and past it. Each record that
    /// runs asks for its own power of two, so what ran names which records
    /// did. The records under the count are clamped as with the count
    /// buffer, the ones past it are written all zeros, and both ways charge
    /// the budget alike.
    #[test]
    fn warp_no_count_runs_what_the_count_buffer_runs() {
        let Some(mut warp) = WarpHarness::new() else {
            return;
        };
        let points = point_pipeline(&warp.device);
        for (kind, stride) in [(2, 16), (0, 20), (1, 24), (10, 16)] {
            for max in [1u32, 5, 16] {
                let mut cases = vec![(0, u32::MAX), (1, u32::MAX), (max / 2, u32::MAX), (max, u32::MAX), (max + 3, u32::MAX), (u32::MAX, u32::MAX)];
                if max > 3 {
                    cases.extend([(3, 3), (max, 3)]);
                }
                for (count, garbage) in cases {
                    let what = format!("kind {kind}, {max} records, count {count:#x}, garbage from {garbage:#x}");
                    let got = run_counted(&mut warp, &points, (kind, stride, max), count, garbage);
                    let runs = count.min(max).min(garbage);
                    let mask = (1u64 << runs) - 1;
                    if kind != 10 {
                        assert_eq!(got.ran, [mask; 2], "{what}: what ran, with the count buffer and without");
                    }
                    // What the records under the count cost (a mesh record of
                    // all ones is clamped to the most, not emptied).
                    let first = |i: u32| if i >= garbage { [u32::MAX; 3] } else { [1 << i, 1, u32::from(kind >= 2)] };
                    let cost: u32 = (0..count.min(max)).map(|i| clamped(kind, first(i)).2).sum();
                    assert_eq!(got.budget, [cost; 2], "{what}: the budget charged, with the count buffer and without");
                    let under = (count.min(max) * stride / 4) as usize;
                    assert_eq!(got.records[1][..under], got.records[0][..under], "{what}: records under the count");
                    assert!(got.records[1][under..].iter().all(|&w| w == 0), "{what}: records past the count are empty");
                }
            }
        }
    }
}

/// The benchmark of the clamp and its barriers (ignored by default).
#[cfg(test)]
mod bench;

/// What one small draw costs this card, bare and with one thing changed
/// between every two draws: a measurement, not a check (it asserts only
/// that the card answered). The timing run of the lit scene (run 187) has
/// some 11,000 small commands a frame at 5 to 16 microseconds each, and no
/// log says whether that is what we put around a command or what the card
/// takes for one. Each variant here is 10,000 one-triangle draws into a
/// 2560x1440 target, timed on the card by one pair of timestamps around
/// the whole batch (not one per command, which would time the stamps).
///
/// Run by name, no game needed:
/// `cargo test -p victory-gpu --release -- --ignored bare_command_cost --nocapture`
#[cfg(test)]
mod bare_cost {
    use windows::Win32::Foundation::{HANDLE, RECT};
    use windows::Win32::Graphics::Direct3D::{D3D_FEATURE_LEVEL_12_0, D3D_PRIMITIVE_TOPOLOGY};
    use windows::Win32::Graphics::Direct3D12::*;
    use windows::Win32::Graphics::Dxgi::Common::*;
    use windows::Win32::Graphics::Dxgi::*;
    use windows::Win32::System::Threading::{CreateEventW, INFINITE, WaitForSingleObject};
    use windows::core::{Interface, s};

    use crate::host::{compile, transition};

    /// Draws in a batch.
    const DRAWS: u32 = 10_000;
    const WIDTH: u32 = 2560;
    const HEIGHT: u32 = 1440;

    /// One small triangle in the middle of the target (about 180 pixels),
    /// and two pixel programs that read one texel of a texture through a
    /// table, so that the view heap set is in use.
    const DRAW: &str = r#"
#define RS "DescriptorTable(SRV(t0))"
Texture2D<float4> image : register(t0);
[RootSignature(RS)]
float4 corner(uint i : SV_VertexID) : SV_Position {
    float2 p = float2((i << 1) & 2, i & 2);
    return float4(p * 0.02 - 0.01, 0.5, 1.0);
}
[RootSignature(RS)]
float4 first(float4 at : SV_Position) : SV_Target {
    return image.Load(int3(0, 0, 0));
}
[RootSignature(RS)]
float4 second(float4 at : SV_Position) : SV_Target {
    return 1.0 - image.Load(int3(0, 0, 0));
}
"#;

    /// A compute pass of next to nothing, as our clamp of an indirect
    /// record is: one group of one thread that writes one word.
    const WORK: &str = r#"
#define RS "UAV(u0)"
RWByteAddressBuffer words : register(u0);
[RootSignature(RS)]
[numthreads(1, 1, 1)]
void work() {
    words.Store(0, words.Load(0) + 1);
}
"#;

    /// What changes between every two draws of a batch.
    #[derive(Clone, Copy, PartialEq)]
    enum Between {
        /// Nothing.
        Nothing,
        /// The pipeline: two alternate.
        Pipeline,
        /// A second texture goes from target to shader read and back.
        BarrierPair,
        /// The depth texture bound for testing goes from write to read or
        /// back.
        DepthMove,
        /// The pair of descriptor heaps: two pairs alternate.
        Heaps,
        /// The colour target: two alternate.
        Targets,
        /// A compute pass of ours of one group, and the pipeline back.
        Compute,
        /// The same between two waits for every earlier shader write, as
        /// the clamp records them.
        ComputeWithWaits,
    }

    struct Rig {
        queue: ID3D12CommandQueue,
        allocator: ID3D12CommandAllocator,
        list: ID3D12GraphicsCommandList,
        fence: ID3D12Fence,
        submitted: u64,
        event: HANDLE,
        root: ID3D12RootSignature,
        pipelines: [ID3D12PipelineState; 2],
        work_root: ID3D12RootSignature,
        work: ID3D12PipelineState,
        words: ID3D12Resource,
        other: ID3D12Resource,
        depth: ID3D12Resource,
        /// Kept alive: the views below name them.
        _kept: Vec<ID3D12Resource>,
        _target_heaps: [ID3D12DescriptorHeap; 2],
        heap_pairs: [[Option<ID3D12DescriptorHeap>; 2]; 2],
        tables: [D3D12_GPU_DESCRIPTOR_HANDLE; 2],
        targets: [D3D12_CPU_DESCRIPTOR_HANDLE; 2],
        depth_tested: D3D12_CPU_DESCRIPTOR_HANDLE,
        stamps: ID3D12QueryHeap,
        back: ID3D12Resource,
        card: String,
    }

    fn texture(
        device: &ID3D12Device,
        (width, height): (u32, u32),
        format: DXGI_FORMAT,
        flags: D3D12_RESOURCE_FLAGS,
        state: D3D12_RESOURCE_STATES,
    ) -> ID3D12Resource {
        let heap = D3D12_HEAP_PROPERTIES {
            Type: D3D12_HEAP_TYPE_DEFAULT,
            ..Default::default()
        };
        let desc = D3D12_RESOURCE_DESC {
            Dimension: D3D12_RESOURCE_DIMENSION_TEXTURE2D,
            Width: u64::from(width),
            Height: height,
            DepthOrArraySize: 1,
            MipLevels: 1,
            Format: format,
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            Flags: flags,
            ..Default::default()
        };
        let mut out: Option<ID3D12Resource> = None;
        // SAFETY: plain texture creation.
        unsafe {
            device
                .CreateCommittedResource(&heap, D3D12_HEAP_FLAG_NONE, &desc, state, None, &mut out)
                .unwrap();
        }
        out.unwrap()
    }

    fn buffer(
        device: &ID3D12Device,
        size: u64,
        kind: D3D12_HEAP_TYPE,
        flags: D3D12_RESOURCE_FLAGS,
        state: D3D12_RESOURCE_STATES,
    ) -> ID3D12Resource {
        let heap = D3D12_HEAP_PROPERTIES {
            Type: kind,
            ..Default::default()
        };
        let desc = D3D12_RESOURCE_DESC {
            Dimension: D3D12_RESOURCE_DIMENSION_BUFFER,
            Width: size,
            Height: 1,
            DepthOrArraySize: 1,
            MipLevels: 1,
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            Layout: D3D12_TEXTURE_LAYOUT_ROW_MAJOR,
            Flags: flags,
            ..Default::default()
        };
        let mut out: Option<ID3D12Resource> = None;
        // SAFETY: plain buffer creation.
        unsafe {
            device
                .CreateCommittedResource(&heap, D3D12_HEAP_FLAG_NONE, &desc, state, None, &mut out)
                .unwrap();
        }
        out.unwrap()
    }

    fn heap(device: &ID3D12Device, kind: D3D12_DESCRIPTOR_HEAP_TYPE, count: u32, visible: bool) -> ID3D12DescriptorHeap {
        let flags = if visible {
            D3D12_DESCRIPTOR_HEAP_FLAG_SHADER_VISIBLE
        } else {
            D3D12_DESCRIPTOR_HEAP_FLAG_NONE
        };
        // SAFETY: plain heap creation.
        unsafe {
            device
                .CreateDescriptorHeap(&D3D12_DESCRIPTOR_HEAP_DESC {
                    Type: kind,
                    NumDescriptors: count,
                    Flags: flags,
                    NodeMask: 0,
                })
                .unwrap()
        }
    }

    fn pipeline(device: &ID3D12Device, root: &ID3D12RootSignature, vs: &[u8], ps: &[u8]) -> ID3D12PipelineState {
        let code = |bytes: &[u8]| D3D12_SHADER_BYTECODE {
            pShaderBytecode: bytes.as_ptr().cast(),
            BytecodeLength: bytes.len(),
        };
        let mut blend = D3D12_BLEND_DESC::default();
        blend.RenderTarget[0].RenderTargetWriteMask = 0xf;
        let mut formats = [DXGI_FORMAT(0); 8];
        formats[0] = DXGI_FORMAT_R8G8B8A8_UNORM;
        // SAFETY: the description points at live data and never releases
        // its pointer to the root signature.
        unsafe {
            let desc = D3D12_GRAPHICS_PIPELINE_STATE_DESC {
                pRootSignature: std::mem::transmute_copy(root),
                VS: code(vs),
                PS: code(ps),
                BlendState: blend,
                SampleMask: u32::MAX,
                RasterizerState: D3D12_RASTERIZER_DESC {
                    FillMode: D3D12_FILL_MODE_SOLID,
                    CullMode: D3D12_CULL_MODE_NONE,
                    DepthClipEnable: true.into(),
                    ..Default::default()
                },
                // Depth is tested and never written, so the texture may be
                // in the write state or in the read state under one view.
                DepthStencilState: D3D12_DEPTH_STENCIL_DESC {
                    DepthEnable: true.into(),
                    DepthWriteMask: D3D12_DEPTH_WRITE_MASK_ZERO,
                    DepthFunc: D3D12_COMPARISON_FUNC_LESS_EQUAL,
                    ..Default::default()
                },
                PrimitiveTopologyType: D3D12_PRIMITIVE_TOPOLOGY_TYPE_TRIANGLE,
                NumRenderTargets: 1,
                RTVFormats: formats,
                DSVFormat: DXGI_FORMAT_D32_FLOAT,
                SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                ..Default::default()
            };
            device.CreateGraphicsPipelineState(&desc).unwrap()
        }
    }

    impl Rig {
        /// The default adapter, as the layer takes it; `None` without one
        /// or on a software adapter.
        fn new() -> Option<Self> {
            let vs = compile(DRAW, s!("corner"), s!("vs_5_1")).ok()?;
            let ps = [
                compile(DRAW, s!("first"), s!("ps_5_1")).ok()?,
                compile(DRAW, s!("second"), s!("ps_5_1")).ok()?,
            ];
            let cs = compile(WORK, s!("work"), s!("cs_5_1")).ok()?;
            // SAFETY: plain object creation on a device of our own; the
            // views are made in heaps that live as long as the rig.
            unsafe {
                let mut device: Option<ID3D12Device> = None;
                D3D12CreateDevice(None, D3D_FEATURE_LEVEL_12_0, &mut device).ok()?;
                let device = device?;
                let desc = CreateDXGIFactory2::<IDXGIFactory4>(DXGI_CREATE_FACTORY_FLAGS(0))
                    .and_then(|factory| factory.EnumAdapterByLuid::<IDXGIAdapter3>(device.GetAdapterLuid()))
                    .ok()?
                    .cast::<IDXGIAdapter1>()
                    .ok()?
                    .GetDesc1()
                    .ok()?;
                if desc.Flags & DXGI_ADAPTER_FLAG_SOFTWARE.0 as u32 != 0 {
                    return None;
                }
                let card = String::from_utf16_lossy(&desc.Description)
                    .trim_end_matches('\0')
                    .to_owned();
                let queue: ID3D12CommandQueue = device
                    .CreateCommandQueue(&D3D12_COMMAND_QUEUE_DESC {
                        Type: D3D12_COMMAND_LIST_TYPE_DIRECT,
                        ..Default::default()
                    })
                    .ok()?;
                let allocator: ID3D12CommandAllocator =
                    device.CreateCommandAllocator(D3D12_COMMAND_LIST_TYPE_DIRECT).ok()?;
                let list: ID3D12GraphicsCommandList = device
                    .CreateCommandList(0, D3D12_COMMAND_LIST_TYPE_DIRECT, &allocator, None)
                    .ok()?;
                let fence: ID3D12Fence = device.CreateFence(0, D3D12_FENCE_FLAG_NONE).ok()?;
                let event = CreateEventW(None, false, false, None).ok()?;

                let root: ID3D12RootSignature = device.CreateRootSignature(0, &vs[..]).ok()?;
                let pipelines = [
                    pipeline(&device, &root, &vs, &ps[0]),
                    pipeline(&device, &root, &vs, &ps[1]),
                ];
                let work_root: ID3D12RootSignature = device.CreateRootSignature(0, &cs[..]).ok()?;
                let work: ID3D12PipelineState = device
                    .CreateComputePipelineState(&D3D12_COMPUTE_PIPELINE_STATE_DESC {
                        pRootSignature: std::mem::transmute_copy(&work_root),
                        CS: D3D12_SHADER_BYTECODE {
                            pShaderBytecode: cs.as_ptr().cast(),
                            BytecodeLength: cs.len(),
                        },
                        ..Default::default()
                    })
                    .ok()?;

                let size = (WIDTH, HEIGHT);
                let colour = |state| texture(&device, size, DXGI_FORMAT_R8G8B8A8_UNORM, D3D12_RESOURCE_FLAG_ALLOW_RENDER_TARGET, state);
                let target = [
                    colour(D3D12_RESOURCE_STATE_RENDER_TARGET),
                    colour(D3D12_RESOURCE_STATE_RENDER_TARGET),
                ];
                let other = colour(D3D12_RESOURCE_STATE_RENDER_TARGET);
                let depth = texture(
                    &device,
                    size,
                    DXGI_FORMAT_D32_FLOAT,
                    D3D12_RESOURCE_FLAG_ALLOW_DEPTH_STENCIL,
                    D3D12_RESOURCE_STATE_DEPTH_WRITE,
                );
                let image = texture(
                    &device,
                    (4, 4),
                    DXGI_FORMAT_R8G8B8A8_UNORM,
                    D3D12_RESOURCE_FLAG_NONE,
                    D3D12_RESOURCE_STATE_PIXEL_SHADER_RESOURCE,
                );
                let words = buffer(
                    &device,
                    64 << 10,
                    D3D12_HEAP_TYPE_DEFAULT,
                    D3D12_RESOURCE_FLAG_ALLOW_UNORDERED_ACCESS,
                    D3D12_RESOURCE_STATE_UNORDERED_ACCESS,
                );
                let back = buffer(
                    &device,
                    16,
                    D3D12_HEAP_TYPE_READBACK,
                    D3D12_RESOURCE_FLAG_NONE,
                    D3D12_RESOURCE_STATE_COPY_DEST,
                );
                let mut stamps: Option<ID3D12QueryHeap> = None;
                device
                    .CreateQueryHeap(
                        &D3D12_QUERY_HEAP_DESC {
                            Type: D3D12_QUERY_HEAP_TYPE_TIMESTAMP,
                            Count: 2,
                            NodeMask: 0,
                        },
                        &mut stamps,
                    )
                    .ok()?;
                let stamps = stamps?;

                // Target and depth views.
                let rtvs = heap(&device, D3D12_DESCRIPTOR_HEAP_TYPE_RTV, 2, false);
                let rtv_step = device.GetDescriptorHandleIncrementSize(D3D12_DESCRIPTOR_HEAP_TYPE_RTV) as usize;
                let mut targets = [rtvs.GetCPUDescriptorHandleForHeapStart(); 2];
                targets[1].ptr += rtv_step;
                device.CreateRenderTargetView(&target[0], None, targets[0]);
                device.CreateRenderTargetView(&target[1], None, targets[1]);
                let dsvs = heap(&device, D3D12_DESCRIPTOR_HEAP_TYPE_DSV, 2, false);
                let dsv_step = device.GetDescriptorHandleIncrementSize(D3D12_DESCRIPTOR_HEAP_TYPE_DSV) as usize;
                let depth_written = dsvs.GetCPUDescriptorHandleForHeapStart();
                let mut depth_tested = depth_written;
                depth_tested.ptr += dsv_step;
                device.CreateDepthStencilView(&depth, None, depth_written);
                let tested = D3D12_DEPTH_STENCIL_VIEW_DESC {
                    Format: DXGI_FORMAT_D32_FLOAT,
                    ViewDimension: D3D12_DSV_DIMENSION_TEXTURE2D,
                    Flags: D3D12_DSV_FLAG_READ_ONLY_DEPTH,
                    ..Default::default()
                };
                device.CreateDepthStencilView(&depth, Some(&tested), depth_tested);

                // Two pairs of shader-visible heaps: views (one view of the
                // small texture each) and samplers (none used).
                let view_heaps = [
                    heap(&device, D3D12_DESCRIPTOR_HEAP_TYPE_CBV_SRV_UAV, 1, true),
                    heap(&device, D3D12_DESCRIPTOR_HEAP_TYPE_CBV_SRV_UAV, 1, true),
                ];
                let sampler_heaps = [
                    heap(&device, D3D12_DESCRIPTOR_HEAP_TYPE_SAMPLER, 1, true),
                    heap(&device, D3D12_DESCRIPTOR_HEAP_TYPE_SAMPLER, 1, true),
                ];
                for views in &view_heaps {
                    device.CreateShaderResourceView(&image, None, views.GetCPUDescriptorHandleForHeapStart());
                }
                let tables = [
                    view_heaps[0].GetGPUDescriptorHandleForHeapStart(),
                    view_heaps[1].GetGPUDescriptorHandleForHeapStart(),
                ];
                let heap_pairs = [
                    [Some(view_heaps[0].clone()), Some(sampler_heaps[0].clone())],
                    [Some(view_heaps[1].clone()), Some(sampler_heaps[1].clone())],
                ];

                // The first list: both targets and the depth texture cleared.
                list.ClearRenderTargetView(targets[0], &[0.0; 4], None);
                list.ClearRenderTargetView(targets[1], &[0.0; 4], None);
                list.ClearDepthStencilView(depth_written, D3D12_CLEAR_FLAG_DEPTH, 1.0, 0, None);
                let [first_target, second_target] = target;
                let mut rig = Rig {
                    queue,
                    allocator,
                    list,
                    fence,
                    submitted: 0,
                    event,
                    root,
                    pipelines,
                    work_root,
                    work,
                    words,
                    other,
                    depth,
                    _kept: vec![first_target, second_target, image],
                    _target_heaps: [rtvs, dsvs],
                    heap_pairs,
                    tables,
                    targets,
                    depth_tested,
                    stamps,
                    back,
                    card,
                };
                rig.finish();
                Some(rig)
            }
        }

        /// Closes the open list, runs it, waits for the card and opens the
        /// list again.
        fn finish(&mut self) {
            self.submitted += 1;
            // SAFETY: our list, queue and fence; the allocator is reset
            // only after the card finished everything recorded from it.
            unsafe {
                self.list.Close().unwrap();
                let run: ID3D12CommandList = self.list.cast().unwrap();
                self.queue.ExecuteCommandLists(&[Some(run)]);
                self.queue.Signal(&self.fence, self.submitted).unwrap();
                self.fence.SetEventOnCompletion(self.submitted, self.event).unwrap();
                WaitForSingleObject(self.event, INFINITE);
                self.allocator.Reset().unwrap();
                self.list.Reset(&self.allocator, None).unwrap();
            }
        }

        /// What every list starts with: a new list has nothing set.
        fn begin(&self) {
            let l = &self.list;
            // SAFETY: an open list and live objects and views.
            unsafe {
                l.SetDescriptorHeaps(&self.heap_pairs[0]);
                l.SetGraphicsRootSignature(&self.root);
                l.SetPipelineState(&self.pipelines[0]);
                l.SetGraphicsRootDescriptorTable(0, self.tables[0]);
                l.OMSetRenderTargets(1, Some(&self.targets[0]), false, Some(&self.depth_tested));
                l.RSSetViewports(&[D3D12_VIEWPORT {
                    TopLeftX: 0.0,
                    TopLeftY: 0.0,
                    Width: WIDTH as f32,
                    Height: HEIGHT as f32,
                    MinDepth: 0.0,
                    MaxDepth: 1.0,
                }]);
                l.RSSetScissorRects(&[RECT {
                    left: 0,
                    top: 0,
                    right: WIDTH as i32,
                    bottom: HEIGHT as i32,
                }]);
                // A triangle list.
                l.IASetPrimitiveTopology(D3D_PRIMITIVE_TOPOLOGY(4));
            }
        }

        /// One batch of [`DRAWS`] draws in `lists` lists run one by one,
        /// with `between` recorded before every draw: the microseconds a
        /// draw took on the card, and the milliseconds recording took.
        fn batch(&mut self, between: Between, lists: u32) -> (f64, f64) {
            let (target, read, written, tested) = (
                D3D12_RESOURCE_STATE_RENDER_TARGET,
                D3D12_RESOURCE_STATE_PIXEL_SHADER_RESOURCE,
                D3D12_RESOURCE_STATE_DEPTH_WRITE,
                D3D12_RESOURCE_STATE_DEPTH_READ,
            );
            let started = std::time::Instant::now();
            let mut drawn = 0u32;
            for part in 0..lists {
                let l = &self.list;
                // SAFETY: an open list; our query heap; live objects.
                unsafe {
                    if part == 0 {
                        l.EndQuery(&self.stamps, D3D12_QUERY_TYPE_TIMESTAMP, 0);
                    }
                }
                self.begin();
                // SAFETY: an open list with everything set; live objects.
                // Every state moved is moved back within the batch (the
                // draws of a list are an even number).
                unsafe {
                    for _ in 0..DRAWS / lists {
                        let odd = (drawn % 2) as usize;
                        match between {
                            Between::Nothing => {}
                            Between::Pipeline => l.SetPipelineState(&self.pipelines[odd]),
                            Between::BarrierPair => {
                                l.ResourceBarrier(&[transition(&self.other, target, read)]);
                                l.ResourceBarrier(&[transition(&self.other, read, target)]);
                            }
                            Between::DepthMove => {
                                let (from, to) = if odd == 0 { (written, tested) } else { (tested, written) };
                                l.ResourceBarrier(&[transition(&self.depth, from, to)]);
                            }
                            Between::Heaps => {
                                l.SetDescriptorHeaps(&self.heap_pairs[odd]);
                                l.SetGraphicsRootDescriptorTable(0, self.tables[odd]);
                            }
                            Between::Targets => {
                                l.OMSetRenderTargets(1, Some(&self.targets[odd]), false, Some(&self.depth_tested));
                            }
                            Between::Compute | Between::ComputeWithWaits => {
                                let waits = between == Between::ComputeWithWaits;
                                if waits {
                                    l.ResourceBarrier(&[super::uav_barrier()]);
                                }
                                l.SetComputeRootSignature(&self.work_root);
                                l.SetPipelineState(&self.work);
                                l.SetComputeRootUnorderedAccessView(0, self.words.GetGPUVirtualAddress());
                                l.Dispatch(1, 1, 1);
                                if waits {
                                    l.ResourceBarrier(&[super::uav_barrier()]);
                                }
                                l.SetPipelineState(&self.pipelines[0]);
                            }
                        }
                        l.DrawInstanced(3, 1, 0, 0);
                        drawn += 1;
                    }
                    if part + 1 < lists {
                        // The next list of the batch: this one is sent on
                        // its own and the list is opened again at once
                        // (the allocator is not reset before the card is
                        // done with all of them).
                        l.Close().unwrap();
                        let run: ID3D12CommandList = l.cast().unwrap();
                        self.queue.ExecuteCommandLists(&[Some(run)]);
                        l.Reset(&self.allocator, None).unwrap();
                    } else {
                        l.EndQuery(&self.stamps, D3D12_QUERY_TYPE_TIMESTAMP, 1);
                        l.ResolveQueryData(&self.stamps, D3D12_QUERY_TYPE_TIMESTAMP, 0, 2, &self.back, 0);
                    }
                }
            }
            let recording = started.elapsed().as_secs_f64() * 1e3;
            self.finish();
            let mut data = std::ptr::null_mut();
            let range = D3D12_RANGE { Begin: 0, End: 16 };
            // SAFETY: a read-back buffer the finished list resolved two
            // timestamps into.
            let ticks = unsafe {
                self.back.Map(0, Some(&range), Some(&mut data)).unwrap();
                let stamps = std::slice::from_raw_parts(data.cast::<u64>(), 2).to_vec();
                self.back.Unmap(0, None);
                stamps[1].saturating_sub(stamps[0])
            };
            // SAFETY: a plain query of our queue.
            let frequency = unsafe { self.queue.GetTimestampFrequency() }.unwrap_or(1).max(1);
            (ticks as f64 * 1e6 / frequency as f64 / f64::from(drawn.max(1)), recording)
        }
    }

    #[test]
    #[ignore = "a measurement on the card: run it by name with --ignored --nocapture"]
    fn bare_command_cost() {
        let Some(mut rig) = Rig::new() else {
            eprintln!("no hardware adapter, or our shaders did not compile; skipped");
            return;
        };
        eprintln!("bare command cost on {}: {DRAWS} one-triangle draws into {WIDTH}x{HEIGHT}, three batches each", rig.card);
        // The first batch pays for whatever the driver does at first use.
        rig.batch(Between::Nothing, 1);
        let variants = [
            ("(a) nothing changes between draws", Between::Nothing, 1),
            ("(b) another pipeline every draw", Between::Pipeline, 1),
            ("(c) a barrier pair on a second texture every draw", Between::BarrierPair, 1),
            ("(d) the tested depth texture moved between write and read every draw", Between::DepthMove, 1),
            ("(e) another pair of descriptor heaps every draw", Between::Heaps, 1),
            ("(f) another colour target every draw", Between::Targets, 1),
            ("(g) nothing changes, in 100 lists run one by one", Between::Nothing, 100),
            ("(h) a compute pass of ours of one group before every draw", Between::Compute, 1),
            ("(i) the same pass between two waits for earlier shader writes", Between::ComputeWithWaits, 1),
        ];
        let mut least = Vec::new();
        for (name, between, lists) in variants {
            let mut runs: Vec<(f64, f64)> = (0..3).map(|_| rig.batch(between, lists)).collect();
            runs.sort_by(|a, b| a.0.total_cmp(&b.0));
            eprintln!(
                "{name}: {:.2} microseconds a draw on the card (the three: {:.2}, {:.2}, {:.2}); recording took {:.1} ms",
                runs[0].0, runs[0].0, runs[1].0, runs[2].0, runs[0].1
            );
            least.push(runs[0].0);
        }
        assert!(least.iter().all(|us| us.is_finite() && *us > 0.0), "the card gave no time for a batch");
    }
}
