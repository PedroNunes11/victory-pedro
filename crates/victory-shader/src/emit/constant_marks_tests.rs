//! Constant buffers a program picks from the view heap, with
//! `VICTORY_CONSTANT_MARKS` off and on (`shader::constant_marks`): a
//! synthetic GFX10 program that reads a slot of the heap as a constant
//! buffer, run on the hardware GPU and on WARP with the host's table of
//! strides by slot (`SLOT_STRIDES`) as the host would fill it.

use super::shader::{SLOT_STRIDE_CONSTANT, SLOT_STRIDES, TEST_CONSTANT_MARKS};
use super::{lower, validator};
use crate::native::{ComputeIo, HwStage, NativeProgram, ResourceType, RootParam, RootType, StageIo, UsedResource};
use crate::{io, ir, resource, structure};

const NULL: u8 = 125;

/// SMEM `op s[sdata..], s[sbase..], offset`.
const fn smem(op: u32, sdata: u8, sbase: u8, offset: u32) -> [u32; 2] {
    [0x3D << 26 | op << 18 | (sdata as u32) << 6 | (sbase as u32 >> 1), (NULL as u32) << 25 | (offset & 0x1F_FFFF)]
}

/// A one-thread wave32 program: the view heap's address in s0, a table in
/// s4 with a raw UAV at entry 0. It loads the descriptor at heap slot 0,
/// reads four dwords of it as a constant buffer, and stores them to the UAV.
fn program() -> NativeProgram {
    let mut words = vec![0xBE81_0380, 0xBE85_0380]; // s_mov_b32 s1, 0; s_mov_b32 s5, 0
    words.extend(smem(0x02, 8, 0, 0)); // s_load_dwordx4 s[8:11], s[0:1], 0 (heap slot 0)
    words.extend(smem(0x02, 12, 4, 0)); // s_load_dwordx4 s[12:15], s[4:5], 0
    words.extend(smem(0x0A, 16, 8, 0)); // s_buffer_load_dwordx4 s[16:19], s[8:11], 0
    for k in 0..4 {
        words.push(0x7E00_0210 + k); // v_mov_b32 v0, s(16 + k)
        words.extend([0xE070_0000 | (4 * k), 0x8003_0000]); // buffer_store_dword v0, off, s[12:15], 0 offset:4k
    }
    words.push(0xBF81_0000); // s_endpgm
    let param = |root, user_data, slot| RootParam { root, table_offset_dwords: 0, sgprs: 1, base_register: 0, user_data, slot };
    let layout = vec![param(RootType::ResourceHeap, 0, 0), param(RootType::Table, 4, 1)];
    let used = vec![UsedResource {
        kind: ResourceType::Uav,
        root: RootType::Table,
        table_offset_dwords: 0,
        lower_bound: 0,
        stride: 0,
        dwords: 4,
        user_data: 4,
        slot: 1,
    }];
    NativeProgram {
        stage: HwStage::Cs,
        wave64: false,
        vgprs: 4,
        layout,
        used,
        io: StageIo::Cs(ComputeIo { rsrc2: 6 << 1, threads: [1, 1, 1] }),
        code: words.iter().flat_map(|w| w.to_le_bytes()).collect(),
    }
}

/// `native` lowered with the marks off or on (on this thread only),
/// validated when `dxil.dll` is there, as a host shader.
fn lowered(native: &NativeProgram, marks: bool) -> super::HostShader {
    let before = TEST_CONSTANT_MARKS.replace(Some(marks));
    let p = ir::build(&native.code, native.wave64, native.vgprs).expect("ir::build");
    let st = structure::structure(&p).expect("structure");
    let res = resource::track(&p, &native.layout, native.stage, Some(&st)).expect("track");
    let io = io::raise(&p, native).expect("io::raise");
    let l = lower::lower(&lower::Parts {
        native,
        p: &p,
        st: &st,
        io: &io,
        res: &res,
        inputs: Vec::new(),
        outputs: Vec::new(),
        prims: Vec::new(),
        mesh_stride: 0,
        view_marks: false,
        cbv_row_shift: false,
    })
    .expect("lower");
    TEST_CONSTANT_MARKS.set(before);
    if let Ok(mut v) = validator::Validator::load(validator::Validator::default_path().as_deref()) {
        v.validate(&mut l.dxil.clone()).expect("DXIL validation failed");
    }
    assert!(l.root_srvs.contains(&SLOT_STRIDES), "the heap-picked view reads the table of strides");
    super::HostShader {
        stage: HwStage::Cs,
        dxil: l.dxil,
        user_data: l.user_data,
        samplers: l.samplers,
        static_samplers: Vec::new(),
        root_cbvs: l.root_cbvs,
        root_srvs: l.root_srvs,
        root_uavs: l.root_uavs,
        root_indexed_views: Vec::new(),
        half_views: l.half_views,
    }
}

/// Runs `shader` over a constant buffer view of `data` at heap slot 0 and
/// the UAV at slot 1, with `strides` as the host's table by slot, on the
/// hardware GPU and on WARP (each skipped without a device): per one that
/// ran, the four dwords the program stored.
fn run(shader: &super::HostShader, data: &[u32], strides: [u32; 2]) -> Vec<(&'static str, Vec<u32>)> {
    use super::host::{Dispatch, Host, Root, View};
    let mut out = Vec::new();
    for (label, warp) in [("hardware", false), ("warp", true)] {
        let Ok(host) = Host::new(warp) else {
            eprintln!("no SM 6.6 {label} device; skipped");
            continue;
        };
        let (root, pso) = host.compute_pipeline(shader).unwrap_or_else(|e| panic!("{label}: compute_pipeline failed: {e}"));
        let mut ud = vec![0u32; shader.user_data_dwords().max(1) as usize];
        for u in &shader.user_data {
            ud[u.index as usize] = match (u.root, u.slot) {
                // The heap from its start; the table at slot 1 (32 bytes).
                (RootType::ResourceHeap, 0) => 0,
                (RootType::Table, 1) => 32,
                other => panic!("unexpected user data {other:?}"),
            };
        }
        // Root parameter 0 is user data; the root views follow by slot.
        let roots = vec![(0, Root::Constants(ud)), (1, Root::View { srv: true, data: strides.to_vec() })];
        let uav = host
            .dispatch(&root, &pso, &Dispatch { roots, views: &[View::Cbv(data), View::RawUav(16)], samplers: &[], groups: [1, 1, 1] })
            .unwrap_or_else(|e| panic!("{label}: dispatch failed: {e}"));
        out.push((label, uav[0][..4].to_vec()));
    }
    out
}

/// Off: a heap slot with no stride is read as a constant buffer, as today.
/// On: only a slot marked as one is; an unmarked one (a texture's) reads
/// zeros.
#[test]
fn only_a_marked_slot_is_read_as_a_constant_buffer() {
    let _one = super::valu_tests::ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let data: Vec<u32> = (0..64).map(|n| 0xC000 + n).collect();
    let row0 = data[..4].to_vec();
    let native = program();
    let (off, on) = (lowered(&native, false), lowered(&native, true));
    assert_ne!(off.dxil, on.dxil, "on: the slot's mark is checked");
    let mut ran = false;
    for (label, got) in run(&off, &data, [0, 0]) {
        assert_eq!(got, row0, "{label}: off reads a slot with no stride as a constant buffer");
        ran = true;
    }
    for (label, got) in run(&on, &data, [SLOT_STRIDE_CONSTANT, 0]) {
        assert_eq!(got, row0, "{label}: on reads a marked slot as a constant buffer");
    }
    for (label, got) in run(&on, &data, [0, 0]) {
        assert_eq!(got, [0, 0, 0, 0], "{label}: on reads zeros from an unmarked slot");
    }
    if !ran {
        eprintln!("no SM 6.6 device; the marked reads were not run");
    }
}
