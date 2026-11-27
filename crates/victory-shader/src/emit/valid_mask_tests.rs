//! The valid-mask bit (`vm`) of a pixel program's exports: which pixels
//! survive is EXEC at the last export that has it set; an export without it
//! leaves the survival mask as it was, and a program with none kills nothing.
//!
//! Synthetic wave32 pixel programs in the console's ISA, lowered to DXIL,
//! validated by `dxil.dll` when present, and drawn over a 16x16 target on
//! the host GPU and WARP. Pixel column x has `v0` = x + 0.5; EXEC is narrowed
//! to the columns of 8 and over (`v_cmpx_gt_f32 exec, v0, 8.0`).

use super::container::SigElement;
use super::host::{Draw, Host};
use super::lower::{self, Lowered, Parts};
use super::shader::Shader;
use super::{HostShader, validator};
use crate::native::{HwStage, NativeProgram, StageIo};
use crate::{ShaderError, io, ir, resource, structure};

const V_MOV_ONE: u32 = 0x7E00_02F2; // v_mov_b32 v0, 1.0 (the destination goes in bits 17-24)
const V_MOV_ZERO: u32 = 0x7E00_0280; // v_mov_b32 v0, 0
/// `v_mov_b32 v9, lit(8.0)`.
const V_MOV_EIGHT: [u32; 2] = [0x7E12_02FF, 0x4100_0000];
/// `v_cmpx_gt_f32 exec, v0, v9`: EXEC keeps the columns of 8 and over.
const NARROW: u32 = 0x7C28_1300;
/// `s_mov_b32 s0, exec_lo` and `s_mov_b32 exec_lo, s0`.
const SAVE_EXEC: u32 = 0xBE80_037E;
const RESTORE_EXEC: u32 = 0xBEFE_0300;
const END: u32 = 0xBF81_0000;
/// `exp mrt0` of v1-v4 (colour A, red) and of v5-v8 (colour B, blue): the
/// first word is the instruction with its flags (`done` 0x800, `vm` 0x1000).
const DONE: u32 = 0x800;
const VM: u32 = 0x1000;
const EXP_A: [u32; 2] = [0xF800_000F, 0x0403_0201];
const EXP_B: [u32; 2] = [0xF800_000F, 0x0807_0605];
const RED: [f32; 4] = [1.0, 0.0, 0.0, 1.0];
const BLUE: [f32; 4] = [0.0, 0.0, 1.0, 1.0];

fn v_mov(base: u32, dst: u32) -> u32 {
    base | dst << 17
}

/// The program's start: colour A in v1-v4, colour B in v5-v8, 8.0 in v9, the
/// first lanes' EXEC saved in s0.
fn prologue() -> Vec<u32> {
    let mut w = vec![
        v_mov(V_MOV_ONE, 1),
        v_mov(V_MOV_ZERO, 2),
        v_mov(V_MOV_ZERO, 3),
        v_mov(V_MOV_ONE, 4),
        v_mov(V_MOV_ZERO, 5),
        v_mov(V_MOV_ZERO, 6),
        v_mov(V_MOV_ONE, 7),
        v_mov(V_MOV_ONE, 8),
    ];
    w.extend(V_MOV_EIGHT);
    w.push(SAVE_EXEC);
    w
}

fn export(words: [u32; 2], flags: u32) -> [u32; 2] {
    [words[0] | flags, words[1]]
}

fn element(name: &str, kind: u8, interp: u8) -> SigElement {
    SigElement {
        name: name.into(),
        index: 0,
        kind,
        comp: 9,
        interp,
        row: Some(0),
        col: 0,
        cols: 4,
        usage: 15,
        half: false,
    }
}

fn lowered(words: &[u32]) -> Result<Lowered, ShaderError> {
    let native = NativeProgram {
        stage: HwStage::Ps,
        wave64: false,
        vgprs: 12,
        layout: vec![],
        used: vec![],
        io: StageIo::Ps {
            inputs: vec![],
            // v0 = the pixel's x position.
            input_addr: 1 << 8,
            db_shader_control: 0,
            patches: vec![],
        },
        code: words.iter().flat_map(|w| w.to_le_bytes()).collect(),
    };
    let p = ir::build(&native.patched_code(), native.wave64, native.vgprs)?;
    let st = structure::structure(&p)?;
    let res = resource::track(&p, &native.layout, native.stage, Some(&st))?;
    let io = io::raise(&p, &native)?;
    lower::lower(&Parts {
        native: &native,
        p: &p,
        st: &st,
        io: &io,
        res: &res,
        inputs: vec![element("SV_Position", 3, 4)],
        outputs: vec![element("SV_Target", 16, 0)],
        prims: Vec::new(),
        mesh_stride: 0,
    })
}

fn pixel_shader(l: Lowered) -> HostShader {
    HostShader {
        stage: HwStage::Ps,
        dxil: l.dxil,
        user_data: l.user_data,
        samplers: Vec::new(),
        static_samplers: Vec::new(),
        root_cbvs: l.root_cbvs,
        root_srvs: l.root_srvs,
        root_uavs: l.root_uavs,
        written_root_uavs: l.written_root_uavs,
        writes_guest_memory: l.writes_guest_memory,
        root_indexed_views: Vec::new(),
    }
}

/// A vertex shader passing POSITION through to SV_Position.
fn pass_through() -> HostShader {
    use super::bitcode::{Attr, Term};
    let mut s = Shader::new(false);
    let mut input = element("POSITION", 0, 0);
    input.usage = 15;
    s.inputs = vec![input];
    s.outputs = vec![element("SV_Position", 3, 4)];
    for col in 0..4 {
        let args = [s.m.i32(0), s.m.i32(0), s.m.i8(col), s.m.undef(s.t.i32)];
        let v = s.op(s.entry, "loadInput.f32", Attr::ReadNone, s.t.f32, 4, &args);
        let args = [s.m.i32(0), s.m.i32(0), s.m.i8(col), v];
        s.op(s.entry, "storeOutput.f32", Attr::NoUnwind, s.t.void, 5, &args);
    }
    s.m.terminate(s.entry, Term::Ret);
    HostShader {
        stage: HwStage::Vs,
        dxil: s.finish(),
        user_data: Vec::new(),
        samplers: Vec::new(),
        static_samplers: Vec::new(),
        root_cbvs: Vec::new(),
        root_srvs: Vec::new(),
        root_uavs: Vec::new(),
        written_root_uavs: Vec::new(),
        writes_guest_memory: false,
        root_indexed_views: Vec::new(),
    }
}

fn validate(shader: &HostShader) {
    if let Ok(mut v) = validator::Validator::load(validator::Validator::default_path().as_deref())
    {
        v.validate(&mut shader.dxil.clone())
            .unwrap_or_else(|e| panic!("dxil.dll: {e}"));
    }
}

/// The 16x16 texels the program draws (row-major), on each device there is.
fn draw(l: Lowered) -> Vec<(&'static str, Vec<[f32; 4]>)> {
    let ps = pixel_shader(l);
    let vs = pass_through();
    validate(&vs);
    validate(&ps);
    let _one = super::valu_tests::ONE_AT_A_TIME
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    // One triangle over the whole target.
    let vertices = [
        -1.0, 1.0, 0.5, 1.0, -1.0, -3.0, 0.5, 1.0, 3.0, 1.0, 0.5, 1.0,
    ];
    let mut out = Vec::new();
    for (label, warp) in [("hardware", false), ("warp", true)] {
        let Ok(host) = Host::new(warp) else {
            eprintln!("no SM 6.6 {label} device; skipped");
            continue;
        };
        let (root, pso) = host
            .pipeline(&vs, &ps)
            .unwrap_or_else(|e| panic!("{label}: pipeline: {e}"));
        let texels = host
            .draw(
                &root,
                &pso,
                &Draw {
                    roots: Vec::new(),
                    textures: &[],
                    samplers: &[],
                    vertices: &vertices,
                    stride: 16,
                    size: (16, 16),
                    mesh_groups: 0,
                },
            )
            .unwrap_or_else(|e| panic!("{label}: draw: {e}"));
        eprintln!("{label}: drew the pixel program");
        out.push((label, texels));
    }
    if out.is_empty() {
        eprintln!("no device with SM 6.6; GPU checks skipped");
    }
    out
}

/// The texel of column `x`, row `y`.
fn at(texels: &[[f32; 4]], x: usize, y: usize) -> [f32; 4] {
    texels[y * 16 + x]
}

/// Every row of the target holds `left` in columns 0-7 and `right` in 8-15
/// (`None`: not checked).
fn check(label: &str, texels: &[[f32; 4]], left: Option<[f32; 4]>, right: [f32; 4]) {
    assert_eq!(texels.len(), 256, "{label}");
    for y in 0..16 {
        for x in 0..16 {
            let want = if x < 8 { left } else { Some(right) };
            if let Some(want) = want {
                assert_eq!(at(texels, x, y), want, "{label}: pixel ({x}, {y})");
            }
        }
    }
}

fn program(body: &[u32]) -> Vec<u32> {
    let mut w = prologue();
    w.extend_from_slice(body);
    w.push(END);
    w
}

/// An export with VM, under a narrowed EXEC, then one without it under the
/// whole wave's: the second must not bring the pixels the first killed
/// back. (The first used to be refused: a conditional export that is not
/// the last.)
#[test]
fn an_export_without_vm_does_not_revive_the_pixels_a_vm_export_killed() {
    let mut body = vec![NARROW];
    body.extend(export(EXP_A, VM));
    body.push(RESTORE_EXEC);
    body.extend(export(EXP_B, DONE));
    let l = lowered(&program(&body)).unwrap();
    assert_eq!(l.discards, 1, "the pixels outside EXEC at the VM export are killed");
    for (label, texels) in draw(l) {
        // Columns 0-7: killed, so still the target's clear colour. Columns
        // 8-15: both exports ran there, the second one's colour is the last.
        check(label, &texels, Some([0.0; 4]), BLUE);
    }
}

/// A VM export under the whole wave's EXEC, then one under a narrower EXEC
/// that has it too: the last one decides.
#[test]
fn a_second_vm_export_under_a_narrower_exec_kills_what_it_leaves_out() {
    let mut body = Vec::new();
    body.extend(export(EXP_A, VM));
    body.push(NARROW);
    body.extend(export(EXP_B, DONE | VM));
    let l = lowered(&program(&body)).unwrap();
    assert_eq!(l.discards, 1);
    for (label, texels) in draw(l) {
        check(label, &texels, Some([0.0; 4]), BLUE);
    }
}

/// The last VM export is under the wider EXEC: its mask is the survival
/// mask, so what the first one left out lives. (A discard cannot be
/// undone, so the first one kills nothing.)
#[test]
fn the_last_vm_export_decides_even_when_it_is_wider() {
    let mut body = vec![NARROW];
    body.extend(export(EXP_A, VM));
    body.push(RESTORE_EXEC);
    body.extend(export(EXP_B, DONE | VM));
    let l = lowered(&program(&body)).unwrap();
    assert_eq!(l.discards, 0);
    for (label, texels) in draw(l) {
        // Everything lives; every pixel's last export wrote blue.
        check(label, &texels, Some(BLUE), BLUE);
    }
}

/// A single VM export under a narrowed EXEC: the usual kill.
#[test]
fn a_vm_export_under_a_narrowed_exec_kills_the_rest() {
    let mut body = vec![NARROW];
    body.extend(export(EXP_A, DONE | VM));
    let l = lowered(&program(&body)).unwrap();
    assert_eq!(l.discards, 1);
    for (label, texels) in draw(l) {
        check(label, &texels, Some([0.0; 4]), RED);
    }
}

/// Exports without VM, one of them with `done`: nothing says a pixel dies,
/// whatever EXEC is where they run. The lanes outside a narrowed EXEC store
/// nothing (what they then show is the host's), the others show the colour.
#[test]
fn exports_without_vm_kill_nothing_even_under_a_narrowed_exec() {
    // Under the whole wave's EXEC: every pixel shows the colour.
    let l = lowered(&program(&export(EXP_A, DONE))).unwrap();
    assert_eq!(l.discards, 0);
    for (label, texels) in draw(l) {
        check(label, &texels, Some(RED), RED);
    }
    // Under a narrowed one.
    let mut body = vec![NARROW];
    body.extend(export(EXP_A, DONE));
    let l = lowered(&program(&body)).unwrap();
    assert_eq!(l.discards, 0, "an export without VM kills no pixel");
    for (label, texels) in draw(l) {
        check(label, &texels, None, RED);
    }
}

/// A VM export under a narrowed EXEC with another one in a block it can
/// reach (the valid mask could be widened there): a discard cannot be undone
/// and the lanes of the later block are not known, so it is refused.
#[test]
fn a_narrow_vm_export_before_another_in_a_later_block_is_refused() {
    let mut body = vec![NARROW];
    body.extend(export(EXP_A, VM));
    // s_cbranch_execz over the next export.
    body.push(0xBF88_0002);
    body.extend(export(EXP_B, DONE | VM));
    let e = lowered(&program(&body)).err().expect("refused").to_string();
    assert!(e.contains("a later export may widen"), "{e}");
}

/// Unrelated to EXEC: a program whose exports all have VM, the common case
/// (`exp ... done vm` at the end, the lanes that were killed outside EXEC),
/// kills the same pixels as before.
#[test]
fn the_common_kill_pattern_is_unchanged() {
    // exp mrt0 A; v_cmpx; exp mrt0 B done vm (nothing in v_cmpx's way).
    let mut body = Vec::new();
    body.extend(export(EXP_A, 0));
    body.push(NARROW);
    body.extend(export(EXP_B, DONE | VM));
    let l = lowered(&program(&body)).unwrap();
    assert_eq!(l.discards, 1);
    for (label, texels) in draw(l) {
        check(label, &texels, Some([0.0; 4]), BLUE);
    }
}
