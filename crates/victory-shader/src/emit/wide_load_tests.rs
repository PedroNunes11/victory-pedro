//! Scalar buffer loads of several dwords from a raw view, read one dword at a
//! time and with `VICTORY_WIDE_LOADS` in runs of up to four
//! (`Lower::raw_dwords`): synthetic GFX10 programs lowered both ways,
//! validated by `dxil.dll` when present, and run on the host GPU and WARP
//! over a view that ends inside the loaded dwords, at aligned, unaligned and
//! register offsets.

use super::host::{Dispatch, Host, Root, Stride, View};
use super::lower::TEST_WIDE_LOADS;
use super::valu_tests::ONE_AT_A_TIME;
use super::{HostShader, lower, validator};
use crate::native::{
    ComputeIo, HwStage, NativeProgram, ResourceType, RootParam, RootType, StageIo, UsedResource,
};
use crate::{io, ir, resource, structure};

const NULL: u8 = 125;

/// SMEM `op s[sdata..], s[sbase..], offset` with `soffset` (an SGPR, or [`NULL`]).
const fn smem(op: u32, sdata: u8, sbase: u8, offset: u32, soffset: u8) -> [u32; 2] {
    [
        0x3D << 26 | op << 18 | (sdata as u32) << 6 | (sbase as u32 >> 1),
        (soffset as u32) << 25 | (offset & 0x1F_FFFF),
    ]
}

/// `s_buffer_load_dword` .. `s_buffer_load_dwordx16` by dwords.
const fn buffer_load_op(dwords: u32) -> u32 {
    match dwords {
        1 => 0x08,
        2 => 0x09,
        4 => 0x0A,
        8 => 0x0B,
        _ => 0x0C,
    }
}

/// A one-thread wave32 program: root constants in s0..s3, the table address
/// in s4; a raw SRV at table entry 0 and a raw UAV at entry 1. It loads
/// `dwords` dwords of the SRV at byte `offset` (plus s0 with `by_register`)
/// into s16.. and stores them to the UAV's first dwords.
fn program(dwords: u32, offset: u32, by_register: bool) -> NativeProgram {
    let mut words = vec![0xBE85_0380]; // s_mov_b32 s5, 0
    words.extend(smem(0x02, 8, 4, 0, NULL)); // s_load_dwordx4 s[8:11], s[4:5], 0
    words.extend(smem(0x02, 12, 4, 32, NULL)); // s_load_dwordx4 s[12:15], s[4:5], 32
    let soffset = if by_register { 0 } else { NULL };
    words.extend(smem(buffer_load_op(dwords), 16, 8, offset, soffset));
    for k in 0..dwords {
        words.push(0x7E00_0210 + k); // v_mov_b32 v0, s(16 + k)
        // buffer_store_dword v0, off, s[12:15], 0 offset:4k
        words.extend([0xE070_0000 | (4 * k), 0x8003_0000]);
    }
    words.push(0xBF81_0000); // s_endpgm
    let param = |root, sgprs, user_data, slot| RootParam {
        root,
        table_offset_dwords: 0,
        sgprs,
        base_register: 0,
        user_data,
        slot,
    };
    let view = |kind, table_offset_dwords| UsedResource {
        kind,
        root: RootType::Table,
        table_offset_dwords,
        lower_bound: 0,
        stride: 0,
        dwords: 4,
        user_data: 4,
        slot: 1,
    };
    NativeProgram {
        stage: HwStage::Cs,
        wave64: false,
        vgprs: 4,
        layout: vec![
            param(RootType::Constants, 4, 0, 0),
            param(RootType::Table, 1, 4, 1),
        ],
        used: vec![view(ResourceType::Srv, 0), view(ResourceType::Uav, 8)],
        io: StageIo::Cs(ComputeIo {
            rsrc2: 5 << 1,
            threads: [1, 1, 1],
        }),
        code: words.iter().flat_map(|w| w.to_le_bytes()).collect(),
    }
}

/// `native` lowered with wide loads on or off: the shader, validated when
/// `dxil.dll` is there, and the dwords of each raw load the scalar load made.
fn lowered(native: &NativeProgram, wide: bool) -> (HostShader, Vec<u32>) {
    let p = ir::build(&native.code, native.wave64, native.vgprs).expect("ir::build");
    let st = structure::structure(&p).expect("structure");
    let res = resource::track(&p, &native.layout, native.stage, Some(&st)).expect("track");
    let io = io::raise(&p, native).expect("io::raise");
    let before = TEST_WIDE_LOADS.replace(Some(wide));
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
    });
    TEST_WIDE_LOADS.set(before);
    let l = l.expect("lower");
    let shader = HostShader {
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
    if let Ok(mut v) = validator::Validator::load(validator::Validator::default_path().as_deref()) {
        v.validate(&mut shader.dxil.clone())
            .expect("DXIL validation failed");
    }
    (shader, l.raw_loads)
}

#[test]
fn wide_scalar_loads_read_runs_that_stop_at_16_bytes() {
    // (dwords, constant offset, by a register) -> loads with the switch on
    let cases: [(u32, u32, bool, &[u32]); 8] = [
        (2, 8, false, &[2]),
        (2, 12, false, &[1, 1]),
        (4, 0, false, &[4]),
        (4, 4, false, &[3, 1]),
        (8, 16, false, &[4, 4]),
        (16, 8, false, &[2, 4, 4, 4, 2]),
        // A register offset: runs from the constant part's place.
        (16, 0, true, &[4, 4, 4, 4]),
        (8, 24, true, &[2, 4, 2]),
    ];
    for (dwords, offset, by_register, wide) in cases {
        let native = program(dwords, offset, by_register);
        let (_, narrow) = lowered(&native, false);
        assert_eq!(
            narrow,
            vec![1; dwords as usize],
            "x{dwords} at {offset}, off"
        );
        let (_, got) = lowered(&native, true);
        assert_eq!(
            got, wide,
            "x{dwords} at {offset} (register: {by_register}), on"
        );
    }
}

/// Runs `shader` with root constant s0 = `s0` over a raw SRV of `data` whose
/// view holds its first `view` dwords, on the hardware GPU and on WARP (each
/// skipped without a device): per one that ran, the first `dwords` dwords
/// the program stored.
fn run(
    shader: &HostShader,
    data: &[u32],
    view: u32,
    s0: u32,
    dwords: usize,
) -> Vec<(&'static str, Vec<u32>)> {
    let mut out = Vec::new();
    for (label, warp) in [("hardware", false), ("warp", true)] {
        let Ok(host) = Host::new(warp) else {
            eprintln!("no SM 6.6 {label} device; skipped");
            continue;
        };
        let (root, pso) = host
            .compute_pipeline(shader)
            .unwrap_or_else(|e| panic!("{label}: compute_pipeline failed: {e}"));
        let mut ud = vec![0u32; shader.user_data_dwords().max(1) as usize];
        for u in &shader.user_data {
            ud[u.index as usize] = match (u.root, u.slot, u.dword) {
                (RootType::Constants, 0, 0) => s0,
                (RootType::Constants, 0, _) | (RootType::Table, 1, _) => 0,
                other => panic!("unexpected user data {other:?}"),
            };
        }
        let uav = host
            .dispatch(
                &root,
                &pso,
                &Dispatch {
                    roots: vec![(0, Root::Constants(ud))],
                    views: &[
                        View::Buffer {
                            data,
                            stride: Stride::RawFirst(view),
                        },
                        View::RawUav(16),
                    ],
                    samplers: &[],
                    groups: [1, 1, 1],
                },
            )
            .unwrap_or_else(|e| panic!("{label}: dispatch failed: {e}"));
        out.push((label, uav[0][..dwords].to_vec()));
    }
    out
}

#[test]
fn wide_scalar_loads_read_what_dword_loads_read() {
    let _one = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    // Dword n holds 0x1000 + n; the view ends after dword 21, the buffer
    // goes on (a load past the view must not read on into it).
    let data: Vec<u32> = (0..64).map(|n| 0x1000 + n).collect();
    let view = 22u32;
    // What the view gives for the dword at byte `byte`: zero past its end.
    let expect = |byte: u32| {
        let n = byte / 4;
        if n < view { 0x1000 + n } else { 0 }
    };
    // (dwords, constant offset, s0 when the offset is by register)
    let cases: [(u32, u32, Option<u32>); 12] = [
        (16, 16, None),     // in the view, 16-byte runs
        (16, 8, None),      // in the view, runs from 8 bytes on
        (8, 60, None),      // dwords 15..22: the last one past the end
        (4, 80, None),      // dwords 20..23: half past the end
        (2, 84, None),      // dwords 21, 22
        (16, 0, Some(0)),   // by register
        (16, 0, Some(24)),  // dwords 6..21, the view's last
        (16, 0, Some(40)),  // dwords 10..25: four past the end
        (16, 0, Some(100)), // all past the end
        (8, 4, Some(48)),   // dwords 13..20, runs of 3, 4, 1
        (16, 0, Some(2)),   // a register offset that is not a whole dword
        (4, 0, Some(86)),   // the same, half past the end
    ];
    let mut ran = false;
    for (dwords, offset, s0) in cases {
        let native = program(dwords, offset, s0.is_some());
        let (narrow, _) = lowered(&native, false);
        let (wide, _) = lowered(&native, true);
        let s0 = s0.unwrap_or(0);
        let old = run(&narrow, &data, view, s0, dwords as usize);
        let new = run(&wide, &data, view, s0, dwords as usize);
        for ((label, old), (_, new)) in old.iter().zip(&new) {
            assert_eq!(
                new, old,
                "{label}: x{dwords} at {offset} + {s0}: wide loads differ from dword loads"
            );
            if s0 % 4 == 0 {
                let want: Vec<u32> = (0..dwords).map(|k| expect(offset + s0 + 4 * k)).collect();
                assert_eq!(old, &want, "{label}: x{dwords} at {offset} + {s0}");
            }
            ran = true;
        }
    }
    if !ran {
        eprintln!("no SM 6.6 device; the wide and dword loads were not run");
    }
}
