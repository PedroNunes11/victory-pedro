//! The plan and the features' bookkeeping, with a plugin of the test's own.

use super::*;

const S: Context = Context {
    flags: fidelity::HDR | fidelity::DEPTH_INVERTED | fidelity::AUTO_EXPOSURE,
    max_render: [1280, 720],
    max_upscale: [1920, 1080],
};

fn dispatch() -> Dispatch {
    Dispatch {
        context: 1,
        jitter: [0.25, -0.375],
        motion_scale: [1280.0, -720.0],
        render: [1280, 720],
        upscale: [1920, 1080],
        pre_exposure: 1.0,
        reset: false,
    }
}

fn sizes() -> Sizes {
    Sizes {
        colour: [1280, 720],
        depth: [1280, 720],
        motion: [1280, 720],
        motion_format: 34,
        output: [1920, 1080],
        writable: true,
        exposure: false,
    }
}

/// The Series S dispatch: sizes, the quality mode and the flags; jitter and
/// motion scale go over with their signs.
#[test]
fn a_dispatch_is_converted() {
    let (c, e) = plan(Some(S), &dispatch(), &sizes(), 11).expect("a plan");
    assert_eq!((c.width, c.height, c.out_width, c.out_height), (1280, 720, 1920, 1080));
    assert_eq!((c.quality, c.preset), (2, 11));
    assert_eq!(c.flags, flag::HDR | flag::LOW_MOTION | flag::DEPTH_INVERTED | flag::AUTO_EXPOSURE);
    assert_eq!((e.width, e.height), (1280, 720));
    assert_eq!((e.jitter, e.motion_scale), ([0.25, -0.375], [1280.0, -720.0]));
    assert_eq!((e.pre_exposure, e.reset), (1.0, 0));
}

#[test]
fn every_flag_has_its_counterpart() {
    let flags = |f: u32, exposure: bool| {
        let s = Sizes { motion: [1920, 1080], exposure, ..sizes() };
        plan(Some(Context { flags: f, ..S }), &dispatch(), &s, 0).expect("a plan").0.flags
    };
    let auto = flag::AUTO_EXPOSURE;
    // Neither automatic nor a texture: one of the two it must be.
    assert_eq!(flags(0, false), flag::LOW_MOTION | auto);
    assert_eq!(flags(0, true), flag::LOW_MOTION);
    assert_eq!(flags(fidelity::AUTO_EXPOSURE, true), flag::LOW_MOTION | auto);
    assert_eq!(flags(fidelity::HDR, true), flag::LOW_MOTION | flag::HDR);
    assert_eq!(flags(fidelity::DISPLAY_MOTION, true), 0);
    assert_eq!(flags(fidelity::JITTERED_MOTION, true), flag::LOW_MOTION | flag::JITTERED_MOTION);
    assert_eq!(flags(fidelity::DEPTH_INVERTED, true), flag::LOW_MOTION | flag::DEPTH_INVERTED);
    // Debugging and infinite depth have none.
    assert_eq!(flags(1 << 4 | 1 << 7, true), flag::LOW_MOTION);
}

#[test]
fn a_rendered_size_that_changes_has_one_feature() {
    let dynamic = Context { flags: S.flags | fidelity::DYNAMIC_RESOLUTION, ..S };
    let d = Dispatch { render: [1120, 630], ..dispatch() };
    let (c, e) = plan(Some(dynamic), &d, &sizes(), 0).expect("a plan");
    assert_eq!((c.width, c.height, e.width, e.height), (1280, 720, 1120, 630));
    let (c, _) = plan(Some(S), &d, &sizes(), 0).expect("a plan");
    assert_eq!((c.width, c.height), (1120, 630));
}

#[test]
fn the_quality_follows_the_ratio() {
    let q = |w, h| quality([w, h], [3840, 2160]);
    assert_eq!([q(3840, 2160), q(2560, 1440), q(2227, 1253), q(1920, 1080), q(1280, 720)], [5, 2, 1, 0, 3]);
    assert_eq!(quality([1280, 720], [1920, 1080]), 2);
}

#[test]
fn what_dlss_cannot_take_is_refused() {
    let no = |d: Dispatch, s: Sizes| plan(Some(S), &d, &s, 0).err().expect("refused");
    let (d, s) = (dispatch(), sizes());
    assert!(plan(None, &d, &s, 0).is_err());
    no(Dispatch { render: [0, 720], ..d }, s);
    no(Dispatch { upscale: [1280, 600], ..d }, s);
    no(Dispatch { render: [32, 16], upscale: [48, 24], ..d }, s);
    no(d, Sizes { colour: [1279, 720], ..s });
    no(d, Sizes { depth: [640, 360], ..s });
    no(d, Sizes { motion: [640, 360], ..s });
    no(d, Sizes { output: [1280, 720], ..s });
    no(d, Sizes { writable: false, ..s });
    // Packed or normalised motion is in no unit we know.
    no(d, Sizes { motion_format: 28, ..s });
    no(Dispatch { motion_scale: [0.0, 1.0], ..d }, s);
    no(Dispatch { motion_scale: [1.0, f32::NAN], ..d }, s);
    no(Dispatch { jitter: [16.0, 0.0], ..d }, s);
    no(Dispatch { jitter: [f32::INFINITY, 0.0], ..d }, s);
    // Motion at the output's size, in a texture of the rendered one.
    assert!(plan(Some(Context { flags: fidelity::DISPLAY_MOTION, ..S }), &d, &s, 0).is_err());
}

#[test]
fn what_is_left_out_has_a_default() {
    let d = Dispatch { upscale: [0, 0], pre_exposure: 0.0, reset: true, ..dispatch() };
    let (c, e) = plan(Some(S), &d, &sizes(), 0).expect("a plan");
    assert_eq!((c.out_width, c.out_height), (1920, 1080));
    assert_eq!((e.pre_exposure, e.reset), (1.0, 1));
}

#[test]
fn the_variables_are_read() {
    assert_eq!([preset(None), preset(Some("K")), preset(Some(" m ")), preset(Some("J"))], [0, 11, 13, 10]);
    // Not for use, says the guide, or no preset at all.
    assert_eq!([preset(Some("A")), preset(Some("KK")), preset(Some(""))], [0; 3]);
    assert_eq!([forced_flags(Some("0x29")), forced_flags(Some("29"))], [Some(0x29); 2]);
    assert_eq!([forced_flags(None), forced_flags(Some("so"))], [None; 2]);
}

#[test]
fn failures_are_told_from_successes() {
    assert!(!failed(1));
    assert!(failed(0xBAD0_0000) && failed(0xBAD0_000B));
}

/// A plugin that is not there, or is no plugin, is an error and no crash.
#[test]
fn a_bad_plugin_is_refused() {
    let no = |f: &str| load(std::path::Path::new(f)).err().expect("refused");
    assert!(no("victory_dlss.dll").contains("full path"));
    no(r"C:\nowhere\victory_dlss.dll");
    assert!(no(r"C:\Windows\System32\version.dll").contains("victory_dlss_version"));
}

/// A plugin of the test's own: what it was told, per thread.
mod fake {
    use super::*;
    use std::cell::RefCell;

    #[derive(Default)]
    pub struct Told {
        pub made: Vec<Create>,
        pub frames: Vec<(usize, Evaluate)>,
        pub released: Vec<usize>,
        /// What the next calls answer, if not success.
        pub create: u32,
        pub evaluate: u32,
    }
    thread_local!(pub static TOLD: RefCell<Told> = RefCell::default());

    unsafe extern "C" fn init(_: *const Init) -> u32 {
        1
    }
    unsafe extern "C" fn create(_: *mut c_void, c: *const Create, out: *mut *mut c_void) -> u32 {
        TOLD.with_borrow_mut(|t| {
            // SAFETY: the engine's structures.
            t.made.push(unsafe { *c });
            if t.create != 0 {
                return t.create;
            }
            // SAFETY: as above.
            unsafe { out.write(t.made.len() as *mut c_void) };
            1
        })
    }
    unsafe extern "C" fn evaluate(_: *mut c_void, f: *mut c_void, e: *const Evaluate) -> u32 {
        TOLD.with_borrow_mut(|t| {
            // SAFETY: as above.
            t.frames.push((f as usize, unsafe { *e }));
            if t.evaluate != 0 { t.evaluate } else { 1 }
        })
    }
    unsafe extern "C" fn release(f: *mut c_void) -> u32 {
        TOLD.with_borrow_mut(|t| t.released.push(f as usize));
        1
    }
    pub const API: Api = Api { init, create, evaluate, release };
}

fn go(engine: &mut Engine, context: usize, d: &Dispatch) -> Result<(), (&'static str, u32)> {
    let (create, frame) = plan(Some(S), d, &sizes(), 0).expect("a plan");
    // SAFETY: the test's plugin looks at neither list nor textures.
    unsafe { engine.record(std::ptr::null_mut(), context, &create, frame) }
}

/// A feature is made once, starts anew once, and anew again after a
/// dispatch that went another way or failed.
#[test]
fn history_is_dropped_when_frames_were_missed() {
    let mut e = Engine::new(fake::API);
    let resets = || fake::TOLD.with_borrow(|t| t.frames.iter().map(|f| f.1.reset).collect::<Vec<_>>());
    for _ in 0..3 {
        go(&mut e, 1, &dispatch()).expect("upscaled");
    }
    assert_eq!(resets(), [1, 0, 0]);
    e.interrupted();
    go(&mut e, 1, &dispatch()).expect("upscaled");
    go(&mut e, 1, &dispatch()).expect("upscaled");
    assert_eq!(resets()[3..], [1, 0]);
    fake::TOLD.with_borrow_mut(|t| t.evaluate = 0xBAD0_0005);
    assert_eq!(go(&mut e, 1, &dispatch()).err().map(|e| e.1), Some(0xBAD0_0005));
    fake::TOLD.with_borrow_mut(|t| t.evaluate = 0);
    go(&mut e, 1, &dispatch()).expect("upscaled");
    assert_eq!(resets()[5..], [0, 1]);
    assert_eq!(fake::TOLD.with_borrow(|t| t.made.len()), 1);
}

/// A feature that could not be made is not asked for every frame; another
/// still is.
#[test]
fn a_refused_feature_is_asked_for_once() {
    let mut e = Engine::new(fake::API);
    fake::TOLD.with_borrow_mut(|t| t.create = 0xBAD0_000D);
    for _ in 0..5 {
        assert!(go(&mut e, 1, &dispatch()).is_err());
    }
    assert_eq!(fake::TOLD.with_borrow(|t| (t.made.len(), t.frames.len())), (1, 0));
    fake::TOLD.with_borrow_mut(|t| t.create = 0);
    go(&mut e, 1, &Dispatch { render: [1120, 630], ..dispatch() }).expect("upscaled");
}

/// Two contexts have a history each; a feature goes only after frames enough
/// that no list can still name it.
#[test]
fn features_go_when_no_list_can_name_them() {
    let mut e = Engine::new(fake::API);
    go(&mut e, 1, &dispatch()).expect("upscaled");
    go(&mut e, 2, &dispatch()).expect("upscaled");
    let gone = || fake::TOLD.with_borrow(|t| t.released.clone());
    for _ in 0..KEPT - 1 {
        go(&mut e, 2, &dispatch()).expect("upscaled");
    }
    assert!(gone().is_empty());
    go(&mut e, 2, &dispatch()).expect("upscaled");
    assert_eq!(gone(), [1]);
    assert_eq!(e.features.len(), 1);
}

/// Without a plugin nothing is recorded on a title's list.
#[test]
fn without_a_plugin_there_is_no_dlss() {
    if std::env::var_os("VICTORY_DLSS_PLUGIN").is_some() {
        return;
    }
    assert!(!loaded());
    let null = std::ptr::null();
    let textures = Textures { colour: null, depth: null, motion: null, exposure: null, output: null };
    assert!(!crate::list::upscale_dlss(8 as *const c_void, &dispatch(), &textures));
}

#[test]
fn halves_are_read_as_the_floats_they_are() {
    use super::check::half;
    assert_eq!(half(0x3c00), 1.0);
    assert_eq!(half(0xc000), -2.0);
    assert_eq!(half(0x0000), 0.0);
    assert_eq!(half(0x0001), 2f32.powi(-24));
    assert_eq!(half(0x0200), 2f32.powi(-15));
    assert_eq!(half(0x3555), 0.333_251_95);
    assert!(half(0x7c00).is_infinite() && half(0x7e00).is_nan());
}

#[test]
fn the_check_reads_depth_and_motion_of_the_rendered_part_only() {
    use super::check::{Plane, depth_stats, half, jitter_spread, motion_stats};
    // Depth: 4 by 2 texels of floats in rows of 32 bytes, 3 by 2 rendered;
    // the fourth column holds what must not be read.
    let plane = Plane { offset: 16, pitch: 32, width: 4, height: 2, format: 40 };
    let mut bytes = vec![0u8; 16 + 64];
    for (row, values) in [[0.0f32, 0.25, 1.0, 9.0], [0.0, 0.5, f32::NAN, 9.0]].iter().enumerate() {
        for (i, v) in values.iter().enumerate() {
            let at = 16 + row * 32 + i * 4;
            bytes[at..at + 4].copy_from_slice(&v.to_le_bytes());
        }
    }
    let s = depth_stats(&bytes, &plane, [3, 2]).unwrap();
    assert_eq!((s.looked, s.bad, s.zeros, s.ones), (6, 1, 2, 1));
    assert_eq!((s.min, s.max), (0.0, 1.0));
    assert!((s.mean - 0.35).abs() < 1e-6);
    // A format it does not read, and a plane that does not fit its bytes.
    assert!(depth_stats(&bytes, &Plane { format: 28, ..plane }, [3, 2]).is_none());
    assert!(depth_stats(&bytes[..40], &plane, [3, 2]).is_none());

    // Motion: 2 by 1 texels of two halves, scaled to pixels.
    let plane = Plane { offset: 0, pitch: 256, width: 2, height: 1, format: 34 };
    let mut bytes = vec![0u8; 256];
    // (0.5, -0.25) and (0, 0).
    bytes[0..2].copy_from_slice(&0x3800u16.to_le_bytes());
    bytes[2..4].copy_from_slice(&0xb400u16.to_le_bytes());
    assert_eq!((half(0x3800), half(0xb400)), (0.5, -0.25));
    let s = motion_stats(&bytes, &plane, [2, 1], [1280.0, -720.0]).unwrap();
    assert_eq!((s.looked, s.still, s.bad), (2, 1, 0));
    assert_eq!((s.x, s.y), ([0.0, 640.0], [0.0, 180.0]));
    // 640 pixels is longer than an image two texels wide.
    assert_eq!(s.long, 1);
    assert!(motion_stats(&bytes, &Plane { format: 2, ..plane }, [2, 1], [1.0, 1.0]).is_none());

    // Jitters: three, two of them the same.
    let jitters: std::collections::VecDeque<[f32; 2]> = [[0.25, -0.5], [0.25, -0.5], [-0.125, 0.375]].into();
    assert_eq!(jitter_spread(&jitters), (2, [-0.125, 0.25], [-0.5, 0.375]));
}
