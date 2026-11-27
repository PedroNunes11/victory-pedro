//! Float image stores with `VICTORY_NAN_SCRUB` off and on
//! (`lower::nan_scrub`): a synthetic GFX10 program that stores four
//! constants to an RGBA32F texture, run on the hardware GPU and on WARP.
//!
//! shortcut: the colours a pixel shader writes go through the same
//! `Lower::scrub_nan`; only the image store is run here.

use super::{lower, validator};
use crate::native::{ComputeIo, HwStage, NativeProgram, ResourceType, RootParam, RootType, StageIo, UsedResource};
use crate::{io, ir, resource, structure};

const NULL: u8 = 125;

/// SMEM `op s[sdata..], s[sbase..], offset`.
const fn smem(op: u32, sdata: u8, sbase: u8, offset: u32) -> [u32; 2] {
    [0x3D << 26 | op << 18 | (sdata as u32) << 6 | (sbase as u32 >> 1), (NULL as u32) << 25 | (offset & 0x1F_FFFF)]
}

/// `v_mov_b32 v[dst], src` (src: an SGPR number or an inline constant).
const fn v_mov(dst: u32, src: u32) -> u32 {
    0x7E00_0200 | dst << 17 | src
}

/// A one-thread wave32 program: the table address in s4, a constant buffer
/// at table entry 0, an RGBA32F texture UAV at entry 1. It stores the
/// constant buffer's first four dwords to texel (0, 0).
fn program() -> NativeProgram {
    let mut words = vec![0xBE85_0380]; // s_mov_b32 s5, 0
    words.extend(smem(0x02, 8, 4, 0)); // s_load_dwordx4 s[8:11], s[4:5], 0
    words.extend(smem(0x03, 16, 4, 32)); // s_load_dwordx8 s[16:23], s[4:5], 32
    words.extend(smem(0x0A, 12, 8, 0)); // s_buffer_load_dwordx4 s[12:15], s[8:11], 0
    for k in 0..4 {
        words.push(v_mov(k, 12 + k)); // v_mov_b32 v(k), s(12 + k)
    }
    words.push(v_mov(4, 128)); // v_mov_b32 v4, 0
    words.push(v_mov(5, 128)); // v_mov_b32 v5, 0
    // image_store v[0:3], v[4:5], s[16:23] dmask:0xf dim:2D unorm
    words.extend([0xF020_1F08, 0x0004_0004]);
    words.push(0xBF81_0000); // s_endpgm
    let param = |root, sgprs, user_data, slot| RootParam { root, table_offset_dwords: 0, sgprs, base_register: 0, user_data, slot };
    let view = |kind, table_offset_dwords, dwords| UsedResource {
        kind,
        root: RootType::Table,
        table_offset_dwords,
        lower_bound: 0,
        stride: 0,
        dwords,
        user_data: 4,
        slot: 1,
    };
    let layout = vec![param(RootType::Constants, 4, 0, 0), param(RootType::Table, 1, 4, 1)];
    let used = vec![view(ResourceType::Cbv, 0, 4), view(ResourceType::Uav, 8, 8)];
    NativeProgram {
        stage: HwStage::Cs,
        wave64: false,
        vgprs: 8,
        layout,
        used,
        io: StageIo::Cs(ComputeIo { rsrc2: 5 << 1, threads: [1, 1, 1] }),
        code: words.iter().flat_map(|w| w.to_le_bytes()).collect(),
    }
}

/// `native` lowered with the scrub off or on (on this thread only),
/// validated when `dxil.dll` is there, as a host shader.
fn lowered(native: &NativeProgram, scrub: bool) -> super::HostShader {
    let before = lower::TEST_NAN_SCRUB.replace(Some(scrub));
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
    lower::TEST_NAN_SCRUB.set(before);
    if let Ok(mut v) = validator::Validator::load(validator::Validator::default_path().as_deref()) {
        v.validate(&mut l.dxil.clone()).expect("DXIL validation failed");
    }
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

/// Runs `shader` with `data` as its constant buffer, on the hardware GPU
/// and on WARP (each skipped without a device): per one that ran, the texel.
fn run(shader: &super::HostShader, data: &[u32]) -> Vec<(&'static str, Vec<u32>)> {
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
        let texel = host
            .dispatch(
                &root,
                &pso,
                &Dispatch {
                    roots: vec![(0, Root::Constants(ud))],
                    views: &[View::Cbv(data), View::TextureUav(1, 1)],
                    samplers: &[],
                    groups: [1, 1, 1],
                },
            )
            .unwrap_or_else(|e| panic!("{label}: dispatch failed: {e}"));
        out.push((label, texel[0][..4].to_vec()));
    }
    out
}

const NAN: u32 = 0x7FC0_0000;
const NEG_NAN: u32 = 0xFFC0_0001;
const INF: u32 = 0x7F80_0000;
const NEG_INF: u32 = 0xFF80_0000;

fn padded(texel: [u32; 4]) -> Vec<u32> {
    let mut data = texel.to_vec();
    data.resize(64, 0);
    data
}

/// Off: NaN and infinities are stored as they are (what the console does).
/// On: they become 0, and finite values, the largest included, pass.
#[test]
fn a_scrubbed_image_store_writes_0_for_nan_and_infinities() {
    let _one = super::valu_tests::ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let native = program();
    let (off, on) = (lowered(&native, false), lowered(&native, true));
    assert_ne!(off.dxil, on.dxil, "on: the stored values are checked");
    let bad = padded([NAN, INF, NEG_INF, NEG_NAN]);
    let good = padded([1.5f32.to_bits(), f32::MAX.to_bits(), (-2.0f32).to_bits(), f32::MIN_POSITIVE.to_bits()]);
    let mut ran = false;
    for (label, got) in run(&off, &bad) {
        let f: Vec<f32> = got.iter().map(|&b| f32::from_bits(b)).collect();
        assert!(f[0].is_nan() && f[3].is_nan(), "{label}: off keeps NaN: {got:08x?}");
        assert_eq!([got[1], got[2]], [INF, NEG_INF], "{label}: off keeps the infinities");
        ran = true;
    }
    for (label, got) in run(&on, &bad) {
        assert_eq!(got, [0, 0, 0, 0], "{label}: on writes 0 for NaN and infinities");
    }
    for (label, got) in run(&on, &good) {
        assert_eq!(got, good[..4], "{label}: on passes finite values");
    }
    if !ran {
        eprintln!("no SM 6.6 device; the scrubbed stores were not run");
    }
}
