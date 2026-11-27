//! Constant buffer reads shifted by a row the host keeps per view
//! (`Options::cbv_row_shift`, contract 3): a synthetic GFX10 program that reads
//! a constant buffer from a table, lowered with the option off and on, and
//! validated by `dxil.dll` when present.
//!
//! shortcut: root CBVs (a descriptor the program builds around a root
//! address, shifted by the `USER_CBV_SHIFT` user-data word) are not covered
//! here; the host's check mode (`VICTORY_CBV_SHIFT_CHECK`, contract 3) covers
//! both kinds on the card.

use super::shader::SLOT_CBV_SHIFTS;
use super::{lower, validator};
use crate::native::{ComputeIo, HwStage, NativeProgram, ResourceType, RootParam, RootType, StageIo, UsedResource};
use crate::{io, ir, resource, structure};

const NULL: u8 = 125;

/// SMEM `op s[sdata..], s[sbase..], offset`.
const fn smem(op: u32, sdata: u8, sbase: u8, offset: u32) -> [u32; 2] {
    [0x3D << 26 | op << 18 | (sdata as u32) << 6 | (sbase as u32 >> 1), (NULL as u32) << 25 | (offset & 0x1F_FFFF)]
}

/// A one-thread wave32 program: the table address in s4, a constant buffer
/// of kind `kind` at table entry 0, a raw UAV
/// at entry 1. It reads four dwords of row 2 of the constant buffer and
/// stores them to the UAV.
fn program(kind: ResourceType) -> NativeProgram {
    let mut words = vec![0xBE85_0380]; // s_mov_b32 s5, 0
    words.extend(smem(0x02, 8, 4, 0)); // s_load_dwordx4 s[8:11], s[4:5], 0
    words.extend(smem(0x02, 12, 4, 32)); // s_load_dwordx4 s[12:15], s[4:5], 32
    words.extend(smem(0x0A, 16, 8, 32)); // s_buffer_load_dwordx4 s[16:19], s[8:11], 32
    for k in 0..4 {
        words.push(0x7E00_0210 + k); // v_mov_b32 v0, s(16 + k)
        words.extend([0xE070_0000 | (4 * k), 0x8003_0000]); // buffer_store_dword v0, off, s[12:15], 0 offset:4k
    }
    words.push(0xBF81_0000); // s_endpgm
    let param = |root, sgprs, user_data, slot| RootParam { root, table_offset_dwords: 0, sgprs, base_register: 0, user_data, slot };
    let view = |kind, root, table_offset_dwords, user_data, slot| UsedResource {
        kind,
        root,
        table_offset_dwords,
        lower_bound: 0,
        stride: 0,
        dwords: 4,
        user_data,
        slot,
    };
    let layout = vec![param(RootType::Constants, 4, 0, 0), param(RootType::Table, 1, 4, 1)];
    let used = vec![view(kind, RootType::Table, 0, 4, 1), view(ResourceType::Uav, RootType::Table, 8, 4, 1)];
    NativeProgram {
        stage: HwStage::Cs,
        wave64: false,
        vgprs: 4,
        layout,
        used,
        io: StageIo::Cs(ComputeIo { rsrc2: 5 << 1, threads: [1, 1, 1] }),
        code: words.iter().flat_map(|w| w.to_le_bytes()).collect(),
    }
}

/// `native` lowered with the shift off or on, validated when `dxil.dll` is
/// there.
fn lowered(native: &NativeProgram, shift: bool) -> lower::Lowered {
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
        cbv_row_shift: shift,
    })
    .expect("lower");
    if let Ok(mut v) = validator::Validator::load(validator::Validator::default_path().as_deref()) {
        v.validate(&mut l.dxil.clone()).expect("DXIL validation failed");
    }
    l
}

#[test]
fn the_shift_changes_nothing_without_a_constant_buffer() {
    let native = program(ResourceType::Srv);
    let (off, on) = (lowered(&native, false), lowered(&native, true));
    assert_eq!(off.dxil, on.dxil);
    assert!(!on.root_srvs.contains(&SLOT_CBV_SHIFTS));
}

#[test]
fn a_table_constant_buffer_reads_its_shift_from_the_side_table() {
    let native = program(ResourceType::Cbv);
    let (off, on) = (lowered(&native, false), lowered(&native, true));
    assert!(!off.root_srvs.contains(&SLOT_CBV_SHIFTS), "off: no side table");
    assert!(on.root_srvs.contains(&SLOT_CBV_SHIFTS), "on: the side table of shifts is read");
    assert_ne!(off.dxil, on.dxil, "on: the row is shifted");
}

/// Runs `shader` over a constant buffer view of `data` at heap index 0 (the
/// view the host makes on the 256 bytes below the title's constants) with
/// the side table of shifts `shifts` (root SRV `t250`, a dword per heap
/// slot), on the hardware GPU and on WARP (each skipped without a device):
/// per one that ran, the four dwords the program stored.
fn run(shader: &super::HostShader, data: &[u32], shifts: Option<Vec<u32>>) -> Vec<(&'static str, Vec<u32>)> {
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
                (RootType::Constants, 0) | (RootType::Table, 1) => 0,
                other => panic!("unexpected user data {other:?}"),
            };
        }
        // Root parameter 0 is user data; the root views follow by slot.
        let mut roots = vec![(0, Root::Constants(ud))];
        if let Some(shifts) = &shifts {
            roots.push((1, Root::View { srv: true, data: shifts.clone() }));
        }
        let uav = host
            .dispatch(&root, &pso, &Dispatch { roots, views: &[View::Cbv(data), View::RawUav(16)], samplers: &[], groups: [1, 1, 1] })
            .unwrap_or_else(|e| panic!("{label}: dispatch failed: {e}"));
        out.push((label, uav[0][..4].to_vec()));
    }
    out
}

/// The program reads row 2 of its constant buffer. The title's constants
/// begin 3 rows (48 bytes) into the view, so with the shift it must read
/// row 5 of the view, and without it row 2.
#[test]
fn shifted_rows_read_the_title_constants_on_the_card() {
    let _one = super::valu_tests::ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let data: Vec<u32> = (0..64).map(|n| 0xC000 + n).collect();
    let row = |r: u32| (0..4).map(|k| 0xC000 + 4 * r + k).collect::<Vec<u32>>();
    let native = program(ResourceType::Cbv);
    let (off, on) = (lowered(&native, false), lowered(&native, true));
    let as_shader = |l: lower::Lowered| super::HostShader {
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
    };
    let (off, on) = (as_shader(off), as_shader(on));
    let mut ran = false;
    for (label, got) in run(&off, &data, None) {
        assert_eq!(got, row(2), "{label}: off reads row 2 as today");
        ran = true;
    }
    // Slot 0 shifted by 3 rows; other slots 0.
    for (label, got) in run(&on, &data, Some(vec![3, 0, 0, 0])) {
        assert_eq!(got, row(5), "{label}: on reads the title's row 2, row 5 of the view");
    }
    for (label, got) in run(&on, &data, Some(vec![0, 0, 0, 0])) {
        assert_eq!(got, row(2), "{label}: a slot not shifted reads as today");
    }
    if !ran {
        eprintln!("no SM 6.6 device; the shifted reads were not run");
    }
}
