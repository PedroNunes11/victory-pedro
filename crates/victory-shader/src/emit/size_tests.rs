//! End-to-end tests of a texture descriptor the code reads only as numbers:
//! the console compiler asks a texture for its dimensions by reading the
//! width, height and base level out of its descriptor, a synthetic GFX10
//! program does the same arithmetic and stores what it got, and the host
//! view in that slot must give the same values as the console's descriptor
//! would. Lowered to DXIL, validated by `dxil.dll` when present, and run on
//! the host GPU and WARP.

use super::host::View;
use super::waterfall_tests::run;
use super::{HostShader, lower, validator};
use crate::native::{
    ComputeIo, HwStage, NativeProgram, ResourceType, RootParam, RootType, StageIo, UsedResource,
};
use crate::{ShaderError, io, ir, resource, structure};

/// A wave32 compute program of 32 threads that loads the descriptor at the
/// first entry of the view heap (`load` is the `s_load_dword*` that reads it
/// into s[24:31]; from the table at s0, or with `heap` through the heap root
/// at s0) and reads its size the way the console compiler does (the width -
/// 1 in dword 1 from bit 30 and dword 2 up to bit 13, the height - 1 in
/// dword 2 from bit 14, the base level in dword 3 from bit 12, the last
/// level from bit 16), then stores, from every thread, to the buffer at
/// entry 2 of the heap: the width and height shifted down by the base
/// level, the level count, and dwords 1, 2 and 3 as they are.
fn program(heap: bool, load: [u32; 2]) -> Vec<u32> {
    let mut w = vec![
        0xBE84_0300, // s_mov_b32 s4, s0
        0xBE85_0380, // s_mov_b32 s5, 0
    ];
    // The buffer's descriptor comes from the table: s0's, or s1's with the
    // heap root at s0.
    let base = if heap {
        w.extend([
            0xBE86_0301, // s_mov_b32 s6, s1
            0xBE87_0380, // s_mov_b32 s7, 0
        ]);
        3
    } else {
        2
    };
    w.extend(load); // s_load_dword* s[24:..], s[4:5], 0x0
    w.extend([
        0xF408_0A00 | base,
        0xFA00_0040, // s_load_dwordx4 s[40:43], s[base*2:+1], 0x40
        0x7E20_0280, // v_mov_b32 v16, 0
        0xBF8C_C07F, // s_waitcnt lgkmcnt(0)
        0x900C_9E19, // s_lshr_b32 s12, s25, 30
        0x8F14_821A, // s_lshl_b32 s20, s26, 2
        0x938F_FF1A,
        0x0010_000E, // s_bfe_u32 s15, s26, lit(0x0010000e)
        0x8814_140C, // s_or_b32 s20, s12, s20
        0x938C_FF1B,
        0x0004_000C, // s_bfe_u32 s12, s27, lit(0x0004000c)
        0x938D_FF1B,
        0x0004_0010, // s_bfe_u32 s13, s27, lit(0x00040010)
        0x8714_FF14,
        0x0000_FFFF, // s_and_b32 s20, s20, lit(0x0000ffff)
        0x810F_810F, // s_add_i32 s15, s15, 1
        0x8114_8114, // s_add_i32 s20, s20, 1
        0x810D_810D, // s_add_i32 s13, s13, 1
        0x900F_0C0F, // s_lshr_b32 s15, s15, s12
        0x900C_0C14, // s_lshr_b32 s12, s20, s12
        0x7E02_020C, // v_mov_b32 v1, s12
        0x7E04_020F, // v_mov_b32 v2, s15
        0x7E06_020D, // v_mov_b32 v3, s13
        0x7E08_0219, // v_mov_b32 v4, s25
        0x7E0A_021A, // v_mov_b32 v5, s26
        0x7E0C_021B, // v_mov_b32 v6, s27
    ]);
    for k in 0..6u32 {
        // buffer_store_dword v<1+k>, v16, s[40:43], 0 offen offset:<4k>
        w.extend([0xE070_1000 | (4 * k), 0x800A_0010 | ((1 + k) << 8)]);
    }
    w.push(0xBF81_0000); // s_endpgm
    w
}

const LOAD_X8: [u32; 2] = [0xF40C_0602, 0xFA00_0000]; // s_load_dwordx8 s[24:31], s[4:5], 0x0
const LOAD_X4: [u32; 2] = [0xF408_0602, 0xFA00_0000]; // s_load_dwordx4 s[24:27], s[4:5], 0x0

/// Lowers `words`: the descriptor's table range (an eight-dword view) and
/// the buffer's at dword 16 of a table, both from s0, or with `heap` the
/// descriptor from the heap root at s0 and the buffer's table at s1.
fn lowered(words: &[u32], heap: bool) -> Result<HostShader, ShaderError> {
    let param = |root, user_data| RootParam {
        root,
        table_offset_dwords: 0,
        sgprs: 1,
        base_register: 0,
        user_data,
        slot: user_data,
    };
    let range = |kind, table_offset_dwords, dwords, user_data| UsedResource {
        kind,
        root: RootType::Table,
        table_offset_dwords,
        lower_bound: 0,
        stride: 0,
        dwords,
        user_data,
        slot: user_data,
    };
    let (layout, used) = if heap {
        (
            vec![param(RootType::ResourceHeap, 0), param(RootType::Table, 1)],
            vec![range(ResourceType::Uav, 16, 4, 1)],
        )
    } else {
        (
            vec![param(RootType::Table, 0)],
            vec![
                range(ResourceType::Srv, 0, 8, 0),
                range(ResourceType::Uav, 16, 4, 0),
            ],
        )
    };
    let native = NativeProgram {
        stage: HwStage::Cs,
        wave64: false,
        vgprs: 32,
        layout,
        used,
        io: StageIo::Cs(ComputeIo {
            rsrc2: 2 << 1,
            threads: [32, 1, 1],
        }),
        code: words.iter().flat_map(|w| w.to_le_bytes()).collect(),
    };
    let p = ir::build(&native.code, native.wave64, native.vgprs).expect("ir::build");
    let st = structure::structure(&p).expect("structure");
    let res = resource::track(&p, &native.layout, native.stage, Some(&st)).expect("track");
    let io = io::raise(&p, &native).expect("io::raise");
    let lowered = lower::lower(&lower::Parts {
        native: &native,
        p: &p,
        st: &st,
        io: &io,
        res: &res,
        inputs: Vec::new(),
        outputs: Vec::new(),
        prims: Vec::new(),
        mesh_stride: 0,
    })?;
    let shader = HostShader {
        stage: HwStage::Cs,
        dxil: lowered.dxil,
        user_data: lowered.user_data,
        samplers: lowered.samplers,
        static_samplers: Vec::new(),
        root_cbvs: lowered.root_cbvs,
        root_srvs: lowered.root_srvs,
        root_uavs: lowered.root_uavs,
        written_root_uavs: lowered.written_root_uavs,
        writes_guest_memory: lowered.writes_guest_memory,
        root_indexed_views: Vec::new(),
    };
    if let Ok(mut v) = validator::Validator::load(validator::Validator::default_path().as_deref()) {
        v.validate(&mut shader.dxil.clone())
            .expect("DXIL validation failed");
    } else {
        eprintln!("no dxil.dll; validation skipped");
    }
    Ok(shader)
}

/// The words the console's descriptor of a `width` x `height` texture of
/// one level has where the program above reads them.
fn console_words(width: u32, height: u32) -> [u32; 3] {
    let w = width - 1;
    [
        (w & 3) << 30,
        (w >> 2) | (height - 1) << 14,
        // identity swizzle, base level 0, last level 0, type 2D
        0x9 << 28 | 0xFAC,
    ]
}

#[test]
fn a_textures_size_is_read_from_the_host_view_in_the_slot() {
    // 300 x 2: the width needs more than the two bits of dword 1, and the
    // second texture, of another size, is not the one asked.
    let texels = vec![[0.0f32; 4]; 300 * 2];
    let other = [[0.0f32; 4]; 1];
    for heap in [false, true] {
        let shader =
            lowered(&program(heap, LOAD_X8), heap).unwrap_or_else(|e| panic!("heap {heap}: {e:?}"));
        let views = [
            View::Texture(300, 2, &texels),
            View::Texture(1, 1, &other),
            View::RawUav(32),
        ];
        let [d1, d2, d3] = console_words(300, 2);
        for (label, got) in run(&shader, &views) {
            assert_eq!(
                got[..6],
                [300, 2, 1, d1, d2, d3],
                "{label}, heap {heap}: width, height, levels, then dwords 1 to 3"
            );
            assert!(got[6..].iter().all(|&x| x == 0), "{label}: {got:?}");
        }
    }
}

#[test]
fn the_size_of_a_larger_texture_fills_the_height_field() {
    // 5 x 129: no bit of dword 1 is set (width - 1 = 4), the height - 1
    // (128) reaches above bit 14 of dword 2.
    let texels = vec![[0.0f32; 4]; 5 * 129];
    let shader = lowered(&program(false, LOAD_X8), false).expect("lower");
    let views = [
        View::Texture(5, 129, &texels),
        View::Texture(1, 1, &[[0.0; 4]]),
        View::RawUav(32),
    ];
    let [d1, d2, d3] = console_words(5, 129);
    assert_eq!((d1, d2), (0, 1 | 128 << 14));
    for (label, got) in run(&shader, &views) {
        assert_eq!(got[..6], [5, 129, 1, d1, d2, d3], "{label}");
    }
}

#[test]
fn only_an_eight_dword_descriptor_is_taken_for_an_image() {
    // The same arithmetic over four dwords (a buffer's descriptor, which
    // has no width and height in these places) keeps a refusal: as a
    // descriptor read only as data, or, from the heap, as a buffer
    // descriptor of no use whose dwords are read as numbers.
    for heap in [false, true] {
        let got = lowered(&program(heap, LOAD_X4), heap);
        assert!(
            matches!(&got, Err(ShaderError::Unsupported { what, .. })
                if *what == "a descriptor read only as data"
                    || *what == "a descriptor's address or format read as data"),
            "heap {heap}: {:?}",
            got.err()
        );
    }
}
