//! The water light's tile lists on a host of 32-lane waves: one list slot a lane.
//!
//! `water_shading:CS_TransmissionVolumetrics_GenerateTiles` (the title, 1.0.2.1) puts every water
//! tile into one of two lists: tiles without lamps from the front, tiles with lamps from the
//! back. The console takes the slots for a whole 64-lane wave at once: the wave's first lane
//! adds the wave's count to the list's counter, and every lane's slot is that lane's rank
//! among the wave's lanes on top of what the counter held. Run as two host waves, the rank of
//! a lane of the second half needs the first half's lane mask, exchanged through group memory.
//! On 32-lane hosts the second half's tiles overwrote slots of the first half: those tiles got
//! no light that frame and open water appeared black/dark in 16x16 blocks.
//!
//! Here every lane of the list takes its own slot: it adds 1 to the counter and its slot is
//! what the counter held. Slots are still each given once and the counters end the same; only
//! the order within a list differs, which nothing reads.
//!
//! Ported from HotScarlett (`088c2d3`) / fxceless (`cd93552`), after xss3009's fix.
//! `VICTORY_WATER_TILE_SLOTS=0`: leave the program as it is.

use crate::native::{HwStage, NativeProgram};

/// The container's digest (bytes 4..20 of the container).
pub(super) const CONTAINER: [u8; 16] = [
    0xf5, 0xd1, 0x0c, 0xde, 0x74, 0x33, 0x37, 0x82, 0xf3, 0x0f, 0x9b, 0xb5, 0x56, 0xc0, 0x73, 0xd4,
];
/// The program's length and the FNV-1a hash of its bytes.
const BYTES: usize = 2112;
const CODE: u64 = 0x377a_8d97_4fb8_631f;

/// `v_mov_b32 v5, 0` (its source becomes the constant 1) and a `s_nop`.
const MOVE_ZERO: usize = 0x6c4;
const NOP: usize = 0x6d8;

struct Append {
    body: std::ops::Range<usize>,
    kept: &'static [std::ops::Range<usize>],
}

/// Tiles without lamps: `buffer_atomic_add v5, .., 0 glc`, `s_waitcnt vmcnt(0)`.
const FRONT: Append = Append {
    body: 0x6a8..0x704,
    kept: &[0x6dc..0x6e4, 0x6f0..0x6f4],
};
/// Tiles with lamps: `buffer_atomic_add v5, .., offset:32 glc`, the load of the list's size,
/// `s_waitcnt vmcnt(0)`, `s_waitcnt lgkmcnt(0)`.
const BACK: Append = Append {
    body: 0x70c..0x774,
    kept: &[0x740..0x748, 0x74c..0x754, 0x75c..0x760, 0x770..0x774],
};

pub(super) fn on() -> bool {
    std::env::var_os("VICTORY_WATER_TILE_SLOTS").is_none_or(|v| v != "0")
}

fn fnv(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |h, &b| {
        (h ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

fn known(native: &NativeProgram) -> bool {
    native.stage == HwStage::Cs && native.wave64 && native.code.len() == BYTES && fnv(&native.code) == CODE
}

/// Rewrites the two appends of the known program; any other program is left untouched.
pub(super) fn rewrite(native: &mut NativeProgram) -> bool {
    if !known(native) {
        return false;
    }
    let before = native.code.clone();
    let mut one: [u8; 4] = before[MOVE_ZERO..MOVE_ZERO + 4].try_into().expect("four bytes");
    // Source operand 9-bit encoding: 0x80 is constant 0, 0x81 is constant 1.
    one[0] |= 1;
    let nop: [u8; 4] = before[NOP..NOP + 4].try_into().expect("four bytes");
    for append in [FRONT, BACK] {
        let mut new = one.to_vec();
        for piece in append.kept {
            new.extend_from_slice(&before[piece.clone()]);
        }
        let body = &mut native.code[append.body];
        debug_assert!(new.len() <= body.len() && body.len() % 4 == 0);
        body[..new.len()].copy_from_slice(&new);
        for word in body[new.len()..].chunks_exact_mut(4) {
            word.copy_from_slice(&nop);
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stand_in() -> Vec<u8> {
        let mut code = vec![0xaa; BYTES];
        code[MOVE_ZERO..MOVE_ZERO + 4].copy_from_slice(&0x7e0a_0280u32.to_le_bytes());
        code[NOP..NOP + 4].copy_from_slice(&0xbf80_0000u32.to_le_bytes());
        for (n, piece) in FRONT.kept.iter().chain(BACK.kept).enumerate() {
            code[piece.clone()].fill(n as u8 + 1);
        }
        code
    }

    #[test]
    fn another_program_is_left_as_it_is() {
        let code = stand_in();
        let mut native = NativeProgram {
            stage: HwStage::Cs,
            wave64: true,
            vgprs: 32,
            layout: Vec::new(),
            used: Vec::new(),
            io: crate::native::StageIo::Cs(crate::native::ComputeIo {
                rsrc2: 0,
                threads: [8, 8, 1],
            }),
            code: code.clone(),
        };
        assert!(!rewrite(&mut native));
        assert_eq!(native.code, code);
    }

    #[test]
    fn an_append_becomes_one_add_a_lane_and_keeps_its_length() {
        let before = stand_in();
        let mut code = before.clone();
        let one = 0x7e0a_0281u32.to_le_bytes();
        let nop = 0xbf80_0000u32.to_le_bytes();
        for append in [FRONT, BACK] {
            let mut new = one.to_vec();
            for piece in append.kept {
                new.extend_from_slice(&before[piece.clone()]);
            }
            let body = &mut code[append.body];
            body[..new.len()].copy_from_slice(&new);
            for word in body[new.len()..].chunks_exact_mut(4) {
                word.copy_from_slice(&nop);
            }
        }
        assert_eq!(code.len(), before.len());
        assert_eq!(&code[0x6a8..0x6ac], &one);
        assert_eq!(&code[0x6ac..0x6b4], &[1; 8]);
        assert_eq!(&code[0x6b4..0x6b8], &[2; 4]);
        assert!(code[0x6b8..0x704].chunks_exact(4).all(|w| w == nop));
        assert_eq!(&code[0x70c..0x710], &one);
        assert_eq!(&code[0x710..0x718], &[3; 8]);
        assert_eq!(&code[0x718..0x720], &[4; 8]);
        assert_eq!(&code[0x720..0x724], &[5; 4]);
        assert_eq!(&code[0x724..0x728], &[6; 4]);
        assert!(code[0x728..0x774].chunks_exact(4).all(|w| w == nop));
        assert_eq!(&code[..0x6a8], &before[..0x6a8]);
        assert_eq!(&code[0x704..0x70c], &before[0x704..0x70c]);
        assert_eq!(&code[0x774..], &before[0x774..]);
    }
}
