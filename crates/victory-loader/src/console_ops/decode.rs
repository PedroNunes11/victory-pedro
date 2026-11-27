//! Decoding for [`super`]: the instructions it emulates, and the loops
//! around `clzero` and `clwb` it can finish in one go.
//!
//! Pure functions over byte slices, so every encoding is tested without
//! executing anything. Written from the encoding chapter of AMD's AMD64
//! Architecture Programmer's Manual, volume 3 (legacy prefixes, REX,
//! ModRM, SIB, displacements), the instruction pages of volumes 3 and 4,
//! and Intel's SDM volume 2 (instruction format; `clwb`, `rdpid`).

/// Why [`decode`] gave no instruction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Miss {
    /// The bytes end inside the instruction.
    Short,
    /// Not an instruction (or form) we emulate.
    Other,
}

/// Where `extrq` and `insertq` take their field length and bit index from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Fields {
    /// Two immediate bytes: the length, then the index (bits 5:0 of each).
    Imm { len: u8, idx: u8 },
    /// The source XMM register: `extrq` reads bits 5:0 (length) and 13:8
    /// (index) of its low quadword, `insertq` bits 69:64 and 77:72.
    Source,
}

/// An instruction we emulate. Register numbers are 0 to 15, REX included.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Op {
    /// `0f 01 fc`: zero the cache line holding rAX.
    Clzero,
    /// `0f 01 fa`: arm an address monitor.
    Monitorx,
    /// `0f 01 fb`: wait for the monitor (or a timer).
    Mwaitx,
    /// `0f 01 fd`: read the processor register numbered in ECX.
    Rdpru,
    /// `66 0f ae /6`, a memory operand: write the line back. `base` is the
    /// register when the operand is exactly `[base]` (no index, no
    /// displacement), the one shape the loop recognizer accepts.
    Clwb { base: Option<u8> },
    /// `f3 0f c7 /7`, register form: the processor id into `dst` (64-bit).
    Rdpid { dst: u8 },
    /// `66 0f 78 /0 ib ib` (`dst` is ModRM.rm) or `66 0f 79 /r` (`dst` is
    /// ModRM.reg, the fields in ModRM.rm's register, `src`).
    Extrq { dst: u8, src: u8, fields: Fields },
    /// `f2 0f 78 /r ib ib` or `f2 0f 79 /r`: `dst` is ModRM.reg, `src`
    /// ModRM.rm.
    Insertq { dst: u8, src: u8, fields: Fields },
}

/// A decoded instruction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Insn {
    pub op: Op,
    /// Bytes, prefixes included.
    pub len: usize,
    /// An address-size prefix (`67`): rAX is EAX.
    pub addr32: bool,
    /// The last segment-override prefix, if any.
    pub seg: Option<u8>,
}

/// The longest instruction the architecture allows.
pub(crate) const MAX_LEN: usize = 15;

/// Decodes the instruction at the start of `code`.
///
/// Prefixes: any number of legacy prefixes in any order, then at most one
/// REX right before the opcode (a REX followed by anything else but the
/// opcode is refused, as is a lone REX: the manual has the CPU ignore such a
/// REX, a shape compilers do not emit). `lock` makes all of these undefined,
/// so it is refused. The mandatory prefix of an SSE-style opcode (`66`,
/// `f2`, `f3`) must be the only one of the three present: combinations,
/// which the manual resolves by precedence, are left to the CPU.
pub(crate) fn decode(code: &[u8]) -> Result<Insn, Miss> {
    let byte = |at: usize| code.get(at).copied().ok_or(Miss::Short);
    let (mut opsize, mut addr32, mut lock, mut rep_f2, mut rep_f3) =
        (false, false, false, false, false);
    let mut seg = None;
    let mut at = 0;
    loop {
        match byte(at)? {
            0x66 => opsize = true,
            0x67 => addr32 = true,
            0xf0 => lock = true,
            0xf2 => rep_f2 = true,
            0xf3 => rep_f3 = true,
            b @ (0x26 | 0x2e | 0x36 | 0x3e | 0x64 | 0x65) => seg = Some(b),
            _ => break,
        }
        at += 1;
        if at >= MAX_LEN {
            return Err(Miss::Other);
        }
    }
    let mut rex = 0;
    if byte(at)? & 0xf0 == 0x40 {
        rex = byte(at)?;
        at += 1;
    }
    if lock || byte(at)? != 0x0f {
        return Err(Miss::Other);
    }
    let opcode = byte(at + 1)?;
    if !matches!(opcode, 0x01 | 0xae | 0xc7 | 0x78 | 0x79) {
        return Err(Miss::Other);
    }
    let modrm_at = at + 2;
    let modrm = byte(modrm_at)?;
    let (md, reg, rm) = (modrm >> 6, (modrm >> 3) & 7, modrm & 7);
    let reg_x = reg | (rex & 4) << 1;
    let rm_x = rm | (rex & 1) << 3;
    let none = !opsize && !rep_f2 && !rep_f3;
    let only_66 = opsize && !rep_f2 && !rep_f3;
    let only_f2 = rep_f2 && !opsize && !rep_f3;
    let only_f3 = rep_f3 && !opsize && !rep_f2;
    let (op, len) = match opcode {
        // 0f 01 with a fixed third byte: no ModRM operands.
        0x01 if none => {
            let op = match modrm {
                0xfa => Op::Monitorx,
                0xfb => Op::Mwaitx,
                0xfc => Op::Clzero,
                0xfd => Op::Rdpru,
                _ => return Err(Miss::Other),
            };
            (op, modrm_at + 1)
        }
        0xae if only_66 && md != 3 && reg == 6 => {
            let (end, base) = memory_operand(code, modrm_at, rex)?;
            (Op::Clwb { base }, end)
        }
        0xc7 if only_f3 && md == 3 && reg == 7 => (Op::Rdpid { dst: rm_x }, modrm_at + 1),
        0x78 if only_66 && md == 3 && reg == 0 => {
            let fields = Fields::Imm {
                len: byte(modrm_at + 1)?,
                idx: byte(modrm_at + 2)?,
            };
            (
                Op::Extrq {
                    dst: rm_x,
                    src: rm_x,
                    fields,
                },
                modrm_at + 3,
            )
        }
        0x79 if only_66 && md == 3 => (
            Op::Extrq {
                dst: reg_x,
                src: rm_x,
                fields: Fields::Source,
            },
            modrm_at + 1,
        ),
        0x78 if only_f2 && md == 3 => {
            let fields = Fields::Imm {
                len: byte(modrm_at + 1)?,
                idx: byte(modrm_at + 2)?,
            };
            (
                Op::Insertq {
                    dst: reg_x,
                    src: rm_x,
                    fields,
                },
                modrm_at + 3,
            )
        }
        0x79 if only_f2 && md == 3 => (
            Op::Insertq {
                dst: reg_x,
                src: rm_x,
                fields: Fields::Source,
            },
            modrm_at + 1,
        ),
        _ => return Err(Miss::Other),
    };
    if len > MAX_LEN {
        return Err(Miss::Other);
    }
    Ok(Insn {
        op,
        len,
        addr32,
        seg,
    })
}

/// The end of a memory operand whose ModRM byte is at `at` (mod is not
/// 11), and its base register when it is exactly `[base]`.
///
/// The 64-bit and 32-bit (`67`) addressing forms have the same shape:
/// r/m 100 adds a SIB byte; mod 00 with r/m 101 is rIP-relative with a
/// 32-bit displacement; a SIB base of 101 under mod 00 means no base and a
/// 32-bit displacement; mod 01 adds 8 bits of displacement, mod 10 adds
/// 32. REX.B does not change any of these special cases; a SIB index of
/// 100 means no index unless REX.X makes it r12.
fn memory_operand(code: &[u8], at: usize, rex: u8) -> Result<(usize, Option<u8>), Miss> {
    let modrm = code.get(at).copied().ok_or(Miss::Short)?;
    let (md, rm) = (modrm >> 6, modrm & 7);
    let mut end = at + 1;
    let mut base = None;
    if rm == 4 {
        let sib = code.get(end).copied().ok_or(Miss::Short)?;
        end += 1;
        let (index, sib_base) = ((sib >> 3) & 7, sib & 7);
        let no_index = index == 4 && rex & 2 == 0;
        if md == 0 && sib_base == 5 {
            end += 4;
        } else if md == 0 && no_index {
            base = Some(sib_base | (rex & 1) << 3);
        }
    } else if md == 0 && rm == 5 {
        end += 4;
    } else if md == 0 {
        base = Some(rm | (rex & 1) << 3);
    }
    end += match md {
        1 => 1,
        2 => 4,
        _ => 0,
    };
    if code.len() < end {
        return Err(Miss::Short);
    }
    Ok((end, base))
}

/// When a loop goes round again, as a test of the stepped pointer against
/// the end register (unsigned).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Again {
    /// While `ptr < end`.
    Below,
    /// While `ptr <= end`.
    BelowOrEqual,
    /// While `ptr != end`.
    NotEqual,
}

/// A loop that steps a pointer by one 64-byte line per pass:
///
/// ```text
/// top: clzero            (or clwb [ptr])
///      add  ptr, 0x40    (or lea ptr, [ptr + 0x40])
///      cmp  ptr, end     (or cmp end, ptr)
///      jcc  top
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Loop {
    /// The stepped register.
    pub ptr: u8,
    /// The register it is compared with.
    pub end: u8,
    /// `cmp ptr, end` (else `cmp end, ptr`): which way round the flags
    /// are computed.
    pub ptr_first: bool,
    pub again: Again,
    /// Bytes from the body's first byte to the instruction after the jump.
    pub len: usize,
}

/// The step: 64 added to `ptr`.
///
/// `add r64, imm8` (`REX.W 83 /0 40`), `add r64, imm32` (`REX.W 81 /0`),
/// `add rax, imm32` (`REX.W 05`) and `lea r64, [r64 + 40h]` (`REX.W 8d`,
/// mod 01, with the SIB byte rsp and r12 need).
fn step(code: &[u8], ptr: u8) -> Option<usize> {
    let (low, high) = (ptr & 7, ptr >> 3);
    let w_b = 0x48 | high;
    if code.starts_with(&[w_b, 0x83, 0xc0 | low, 0x40]) {
        return Some(4);
    }
    if code.starts_with(&[w_b, 0x81, 0xc0 | low, 0x40, 0, 0, 0]) {
        return Some(7);
    }
    if ptr == 0 && code.starts_with(&[0x48, 0x05, 0x40, 0, 0, 0]) {
        return Some(6);
    }
    let w_r_b = 0x48 | high << 2 | high;
    let modrm = 0x40 | low << 3 | low;
    if low == 4 {
        // SIB 24: no index, base rsp or r12.
        code.starts_with(&[w_r_b, 0x8d, modrm, 0x24, 0x40])
            .then_some(5)
    } else {
        code.starts_with(&[w_r_b, 0x8d, modrm, 0x40]).then_some(4)
    }
}

/// `cmp` of `ptr` with another 64-bit register, either way round:
/// `REX.W 39 /r` (`cmp r/m64, r64`) or `REX.W 3b /r` (`cmp r64, r/m64`),
/// mod 11. Gives the other register and whether `ptr` is the first operand.
fn compare(code: &[u8], ptr: u8) -> Option<(u8, bool)> {
    let &[rex, opcode, modrm] = code.get(..3)? else {
        return None;
    };
    // REX.W, REX.X clear (no index register in a register form anyway).
    if rex & 0xfa != 0x48 || modrm >> 6 != 3 {
        return None;
    }
    let reg = (modrm >> 3) & 7 | (rex & 4) << 1;
    let rm = modrm & 7 | (rex & 1) << 3;
    let (first, second) = match opcode {
        0x39 => (rm, reg),
        0x3b => (reg, rm),
        _ => return None,
    };
    match (first == ptr, second == ptr) {
        (true, false) => Some((second, true)),
        (false, true) => Some((first, false)),
        _ => None,
    }
}

/// A conditional jump at offset `at` (from the body's start) back to the
/// body's start: `7x rel8` or `0f 8x rel32`. Gives the condition (the low
/// nibble of the opcode) and the offset after the jump.
fn jump_back(code: &[u8], at: usize) -> Option<(u8, usize)> {
    let first = *code.get(at)?;
    let (cc, end, rel) = if first & 0xf0 == 0x70 {
        let end = at + 2;
        (first & 15, end, i64::from(*code.get(at + 1)? as i8))
    } else if first == 0x0f && code.get(at + 1)? & 0xf0 == 0x80 {
        let end = at + 6;
        let bytes: [u8; 4] = code.get(at + 2..end)?.try_into().ok()?;
        (code[at + 1] & 15, end, i64::from(i32::from_le_bytes(bytes)))
    } else {
        return None;
    };
    (rel == -(end as i64)).then_some((cc, end))
}

/// The loop starting with a body of `body_len` bytes at the start of
/// `code` whose pointer register is `ptr`, if the bytes after the body are
/// exactly one of the shapes [`Loop`] describes.
///
/// Only conditions under which a growing pointer stops are taken:
/// `jb`/`jbe`/`jne` after `cmp ptr, end`; `ja`/`jae`/`jne` after
/// `cmp end, ptr`. Signed conditions and anything else are left to the
/// one-line-at-a-time path.
pub(crate) fn find_loop(code: &[u8], body_len: usize, ptr: u8) -> Option<Loop> {
    let step_len = step(code.get(body_len..)?, ptr)?;
    let cmp_at = body_len + step_len;
    let (end, ptr_first) = compare(code.get(cmp_at..)?, ptr)?;
    let (cc, len) = jump_back(code, cmp_at + 3)?;
    let again = match (cc, ptr_first) {
        (0x2, true) | (0x7, false) => Again::Below,
        (0x6, true) | (0x3, false) => Again::BelowOrEqual,
        (0x5, _) => Again::NotEqual,
        _ => return None,
    };
    Some(Loop {
        ptr,
        end,
        ptr_first,
        again,
        len,
    })
}

/// Most lines a loop is finished in one go for: 64 GiB, far beyond any
/// real buffer, so a garbage end register is not chased.
const MAX_PASSES: u64 = 1 << 30;

/// How many passes a loop starting with the pointer at `start` makes
/// before it falls through, and the pointer after the last: `None` when it
/// would wrap around the address space, never stop, or run implausibly far.
///
/// Each pass runs the body with the pointer as it is, adds 64, then tests
/// it against `end`, so there is always at least one pass.
pub(crate) fn passes(start: u64, end: u64, again: Again) -> Option<(u64, u64)> {
    let n = match again {
        Again::Below => end.saturating_sub(start).div_ceil(64).max(1),
        Again::BelowOrEqual => match end.checked_sub(start) {
            Some(d) => d / 64 + 1,
            None => 1,
        },
        Again::NotEqual => {
            let d = end.checked_sub(start)?;
            if d == 0 || d % 64 != 0 {
                return None;
            }
            d / 64
        }
    };
    if n > MAX_PASSES {
        return None;
    }
    Some((n, start.checked_add(n * 64)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn op(code: &[u8]) -> Result<(Op, usize), Miss> {
        decode(code).map(|i| (i.op, i.len))
    }

    #[test]
    fn decodes_the_0f_01_group() {
        for (third, want) in [
            (0xfa, Op::Monitorx),
            (0xfb, Op::Mwaitx),
            (0xfc, Op::Clzero),
            (0xfd, Op::Rdpru),
        ] {
            let plain = decode(&[0x0f, 0x01, third]).unwrap();
            assert_eq!(
                plain,
                Insn {
                    op: want,
                    len: 3,
                    addr32: false,
                    seg: None
                }
            );
            // REX is allowed and changes nothing.
            assert_eq!(op(&[0x48, 0x0f, 0x01, third]), Ok((want, 4)));
            // An address-size prefix: rAX is EAX.
            let short = decode(&[0x67, 0x0f, 0x01, third]).unwrap();
            assert!(short.addr32 && short.len == 4);
            // A segment override is noted.
            assert_eq!(decode(&[0x2e, 0x0f, 0x01, third]).unwrap().seg, Some(0x2e));
            assert_eq!(decode(&[0x65, 0x67, 0x0f, 0x01, third]).unwrap().len, 5);
            // Mandatory-style prefixes and lock make other or invalid instructions.
            for prefix in [0x66, 0xf2, 0xf3, 0xf0] {
                assert_eq!(op(&[prefix, 0x0f, 0x01, third]), Err(Miss::Other));
            }
        }
        // Other members of the group: rdtscp, swapgs, monitor, mwait.
        for third in [0xf9, 0xf8, 0xc8, 0xc9] {
            assert_eq!(op(&[0x0f, 0x01, third]), Err(Miss::Other));
        }
        assert_eq!(op(&[0x0f, 0x01]), Err(Miss::Short));
    }

    #[test]
    fn decodes_clwb_with_every_operand_shape() {
        let d32 = [0x78, 0x56, 0x34, 0x12];
        let cases: &[(&[u8], usize, Option<u8>)] = &[
            (&[0x66, 0x0f, 0xae, 0x31], 4, Some(1)),              // [rcx]
            (&[0x66, 0x41, 0x0f, 0xae, 0x31], 5, Some(9)),        // [r9]
            (&[0x66, 0x0f, 0xae, 0x30], 4, Some(0)),              // [rax]
            (&[0x66, 0x0f, 0xae, 0x34, 0x24], 5, Some(4)),        // [rsp], SIB
            (&[0x66, 0x41, 0x0f, 0xae, 0x34, 0x24], 6, Some(12)), // [r12], SIB
            (&[0x66, 0x0f, 0xae, 0x34, 0xc8], 5, None),           // [rax + rcx*8]
            (&[0x66, 0x42, 0x0f, 0xae, 0x34, 0x20], 6, None),     // [rax + r12]: REX.X index
            (&[0x66, 0x0f, 0xae, 0x75, 0x08], 5, None),           // [rbp + 8]
            (&[0x66, 0x41, 0x0f, 0xae, 0x75, 0x00], 6, None),     // [r13 + 0]
            (&[0x66, 0x0f, 0xae, 0x74, 0x24, 0x10], 6, None),     // [rsp + 10h]
            (&[0x66, 0x0f, 0xae, 0x71, 0xf0], 5, None),           // [rcx - 10h]
        ];
        for &(code, len, base) in cases {
            assert_eq!(op(code), Ok((Op::Clwb { base }, len)), "{code:02x?}");
        }
        // The 32-bit displacement shapes.
        let with = |head: &[u8]| [head, &d32[..]].concat();
        // [rip + d32]
        assert_eq!(
            op(&with(&[0x66, 0x0f, 0xae, 0x35])),
            Ok((Op::Clwb { base: None }, 8))
        );
        // [rax + d32]
        assert_eq!(
            op(&with(&[0x66, 0x0f, 0xae, 0xb0])),
            Ok((Op::Clwb { base: None }, 8))
        );
        // [d32] through a SIB with neither base nor index
        assert_eq!(
            op(&with(&[0x66, 0x0f, 0xae, 0x34, 0x25])),
            Ok((Op::Clwb { base: None }, 9))
        );
        // [rcx*4 + d32]: SIB base 101 under mod 00, with REX.B (still no base)
        let r = with(&[0x66, 0x41, 0x0f, 0xae, 0x34, 0x8d]);
        assert_eq!(op(&r), Ok((Op::Clwb { base: None }, 10)));
        // [rax + rcx*8 + d32]
        assert_eq!(
            op(&with(&[0x66, 0x0f, 0xae, 0xb4, 0xc8])),
            Ok((Op::Clwb { base: None }, 9))
        );
        // [r13 + d32], REX.B with r/m 101 under mod 10
        assert_eq!(
            op(&with(&[0x66, 0x41, 0x0f, 0xae, 0xb5])),
            Ok((Op::Clwb { base: None }, 9))
        );
        // Legacy prefixes before 66 or after it, and address size.
        let i = decode(&[0x67, 0x66, 0x0f, 0xae, 0x31]).unwrap();
        assert!(i.addr32 && i.len == 5 && i.op == Op::Clwb { base: Some(1) });
        let i = decode(&[0x66, 0x3e, 0x0f, 0xae, 0x31]).unwrap();
        assert_eq!((i.seg, i.len), (Some(0x3e), 5));
        // Not clwb: register form, /7 (clflushopt), no 66 (xsaveopt), f3 too.
        assert_eq!(op(&[0x66, 0x0f, 0xae, 0xf1]), Err(Miss::Other));
        assert_eq!(op(&[0x66, 0x0f, 0xae, 0x39]), Err(Miss::Other));
        assert_eq!(op(&[0x0f, 0xae, 0x31]), Err(Miss::Other));
        assert_eq!(op(&[0xf3, 0x66, 0x0f, 0xae, 0x31]), Err(Miss::Other));
        // A REX that does not sit right before the opcode.
        assert_eq!(op(&[0x41, 0x66, 0x0f, 0xae, 0x31]), Err(Miss::Other));
        // Cut short: the displacement or SIB is missing.
        assert_eq!(op(&[0x66, 0x0f, 0xae, 0xb0, 0x01, 0x02]), Err(Miss::Short));
        assert_eq!(op(&[0x66, 0x0f, 0xae, 0x34]), Err(Miss::Short));
        assert_eq!(op(&[0x66, 0x0f]), Err(Miss::Short));
        // Longer than 15 bytes.
        let mut long = vec![0x2e; 10];
        long.extend_from_slice(&with(&[0x66, 0x0f, 0xae, 0xb4, 0xc8]));
        assert_eq!(op(&long), Err(Miss::Other));
        let mut long = vec![0x2e; 16];
        long.extend_from_slice(&[0x66, 0x0f, 0xae, 0x31]);
        assert_eq!(op(&long), Err(Miss::Other));
    }

    #[test]
    fn decodes_rdpid() {
        assert_eq!(op(&[0xf3, 0x0f, 0xc7, 0xf8]), Ok((Op::Rdpid { dst: 0 }, 4)));
        assert_eq!(op(&[0xf3, 0x0f, 0xc7, 0xfb]), Ok((Op::Rdpid { dst: 3 }, 4)));
        assert_eq!(
            op(&[0xf3, 0x41, 0x0f, 0xc7, 0xff]),
            Ok((Op::Rdpid { dst: 15 }, 5))
        );
        // REX.W changes nothing (the operand is 64-bit anyway).
        assert_eq!(
            op(&[0xf3, 0x48, 0x0f, 0xc7, 0xf9]),
            Ok((Op::Rdpid { dst: 1 }, 5))
        );
        // Memory form, rdseed (no f3), 66 as well, /6 (rdrand's slot).
        assert_eq!(op(&[0xf3, 0x0f, 0xc7, 0x38]), Err(Miss::Other));
        assert_eq!(op(&[0x0f, 0xc7, 0xf8]), Err(Miss::Other));
        assert_eq!(op(&[0x66, 0xf3, 0x0f, 0xc7, 0xf8]), Err(Miss::Other));
        assert_eq!(op(&[0xf3, 0x0f, 0xc7, 0xf0]), Err(Miss::Other));
    }

    #[test]
    fn decodes_extrq_and_insertq() {
        let imm = |len, idx| Fields::Imm { len, idx };
        // extrq xmm1, 8, 20
        let e = Op::Extrq {
            dst: 1,
            src: 1,
            fields: imm(8, 20),
        };
        assert_eq!(op(&[0x66, 0x0f, 0x78, 0xc1, 0x08, 0x14]), Ok((e, 6)));
        // extrq xmm15 (REX.B)
        let e = Op::Extrq {
            dst: 15,
            src: 15,
            fields: imm(0, 0),
        };
        assert_eq!(op(&[0x66, 0x41, 0x0f, 0x78, 0xc7, 0, 0]), Ok((e, 7)));
        // extrq xmm0, xmm1
        let e = Op::Extrq {
            dst: 0,
            src: 1,
            fields: Fields::Source,
        };
        assert_eq!(op(&[0x66, 0x0f, 0x79, 0xc1]), Ok((e, 4)));
        // extrq xmm8, xmm9 (REX.R and REX.B)
        let e = Op::Extrq {
            dst: 8,
            src: 9,
            fields: Fields::Source,
        };
        assert_eq!(op(&[0x66, 0x45, 0x0f, 0x79, 0xc1]), Ok((e, 5)));
        // insertq xmm0, xmm1, 16, 16
        let i = Op::Insertq {
            dst: 0,
            src: 1,
            fields: imm(16, 16),
        };
        assert_eq!(op(&[0xf2, 0x0f, 0x78, 0xc1, 0x10, 0x10]), Ok((i, 6)));
        // insertq xmm8, xmm1, ... (REX.R)
        let i = Op::Insertq {
            dst: 8,
            src: 1,
            fields: imm(1, 2),
        };
        assert_eq!(op(&[0xf2, 0x44, 0x0f, 0x78, 0xc1, 1, 2]), Ok((i, 7)));
        // insertq xmm1, xmm2
        let i = Op::Insertq {
            dst: 1,
            src: 2,
            fields: Fields::Source,
        };
        assert_eq!(op(&[0xf2, 0x0f, 0x79, 0xca]), Ok((i, 4)));
        // insertq xmm0, xmm10 (REX.B)
        let i = Op::Insertq {
            dst: 0,
            src: 10,
            fields: Fields::Source,
        };
        assert_eq!(op(&[0xf2, 0x41, 0x0f, 0x79, 0xc2]), Ok((i, 5)));
        // Only register operands exist; extrq's immediate form wants /0.
        assert_eq!(op(&[0x66, 0x0f, 0x79, 0x00]), Err(Miss::Other));
        assert_eq!(op(&[0x66, 0x0f, 0x78, 0x01, 1, 2]), Err(Miss::Other));
        assert_eq!(op(&[0xf2, 0x0f, 0x79, 0x41, 0x10]), Err(Miss::Other));
        assert_eq!(op(&[0x66, 0x0f, 0x78, 0xc9, 1, 2]), Err(Miss::Other));
        // No prefix (vmread/vmwrite), both prefixes, f3.
        assert_eq!(op(&[0x0f, 0x78, 0xc1]), Err(Miss::Other));
        assert_eq!(op(&[0x66, 0xf2, 0x0f, 0x79, 0xc1]), Err(Miss::Other));
        assert_eq!(op(&[0xf3, 0x0f, 0x79, 0xc1]), Err(Miss::Other));
        // Immediates missing.
        assert_eq!(op(&[0x66, 0x0f, 0x78, 0xc1, 0x08]), Err(Miss::Short));
        assert_eq!(op(&[0xf2, 0x0f, 0x78, 0xc1]), Err(Miss::Short));
    }

    #[test]
    fn leaves_everything_else() {
        assert_eq!(op(&[0x0f, 0x0b]), Err(Miss::Other)); // ud2
        assert_eq!(op(&[0x90, 0x90, 0x90]), Err(Miss::Other)); // nop
        assert_eq!(op(&[0xcc, 0x0f, 0x01, 0xfc]), Err(Miss::Other)); // int3 first
        assert_eq!(op(&[0x0f, 0x31]), Err(Miss::Other)); // rdtsc
        assert_eq!(op(&[]), Err(Miss::Short));
        assert_eq!(op(&[0x66]), Err(Miss::Short));
    }

    /// `clzero; add rax, 40h; cmp rax, rcx; jb top`.
    const CLZERO_LOOP: [u8; 12] = [
        0x0f, 0x01, 0xfc, 0x48, 0x83, 0xc0, 0x40, 0x48, 0x39, 0xc8, 0x72, 0xf4,
    ];

    #[test]
    fn finds_clzero_loops() {
        let want = Loop {
            ptr: 0,
            end: 1,
            ptr_first: true,
            again: Again::Below,
            len: 12,
        };
        assert_eq!(find_loop(&CLZERO_LOOP, 3, 0), Some(want));
        // Trailing bytes do not matter.
        assert_eq!(
            find_loop(&[&CLZERO_LOOP[..], &[0xcc; 8]].concat(), 3, 0),
            Some(want)
        );

        let with = |step: &[u8], cmp: &[u8], jump: &[u8]| {
            [&[0x0f, 0x01, 0xfc][..], step, cmp, jump].concat()
        };
        let add8 = [0x48, 0x83, 0xc0, 0x40];
        let cmp_rax_rcx = [0x48, 0x39, 0xc8];
        let found =
            |code: &[u8]| find_loop(code, 3, 0).map(|l| (l.end, l.ptr_first, l.again, l.len));
        // Each step form; the jump's offset follows the length.
        let add32 = [0x48, 0x81, 0xc0, 0x40, 0, 0, 0];
        assert_eq!(
            found(&with(&add32, &cmp_rax_rcx, &[0x72, 0xf1])),
            Some((1, true, Again::Below, 15))
        );
        let add_rax = [0x48, 0x05, 0x40, 0, 0, 0];
        assert_eq!(
            found(&with(&add_rax, &cmp_rax_rcx, &[0x72, 0xf2])),
            Some((1, true, Again::Below, 14))
        );
        let lea = [0x48, 0x8d, 0x40, 0x40];
        assert_eq!(
            found(&with(&lea, &cmp_rax_rcx, &[0x72, 0xf4])),
            Some((1, true, Again::Below, 12))
        );
        // Conditions: jbe, jne; and cmp the other way round (3b form: cmp rcx, rax).
        assert_eq!(
            found(&with(&add8, &cmp_rax_rcx, &[0x76, 0xf4])),
            Some((1, true, Again::BelowOrEqual, 12))
        );
        assert_eq!(
            found(&with(&add8, &cmp_rax_rcx, &[0x75, 0xf4])),
            Some((1, true, Again::NotEqual, 12))
        );
        let cmp_rcx_rax = [0x48, 0x3b, 0xc8];
        assert_eq!(
            found(&with(&add8, &cmp_rcx_rax, &[0x77, 0xf4])),
            Some((1, false, Again::Below, 12))
        );
        assert_eq!(
            found(&with(&add8, &cmp_rcx_rax, &[0x73, 0xf4])),
            Some((1, false, Again::BelowOrEqual, 12))
        );
        assert_eq!(
            found(&with(&add8, &cmp_rcx_rax, &[0x75, 0xf4])),
            Some((1, false, Again::NotEqual, 12))
        );
        // cmp rax, rcx written as 3b (reg rax, rm rcx); cmp rcx, rax written as 39.
        assert_eq!(
            found(&with(&add8, &[0x48, 0x3b, 0xc1], &[0x72, 0xf4])),
            Some((1, true, Again::Below, 12))
        );
        assert_eq!(
            found(&with(&add8, &[0x48, 0x39, 0xc1], &[0x77, 0xf4])),
            Some((1, false, Again::Below, 12))
        );
        // End register r8 (REX.R) and r15 (REX.B on the 3b form).
        assert_eq!(
            found(&with(&add8, &[0x4c, 0x39, 0xc0], &[0x72, 0xf4])),
            Some((8, true, Again::Below, 12))
        );
        assert_eq!(
            found(&with(&add8, &[0x49, 0x3b, 0xc7], &[0x72, 0xf4])),
            Some((15, true, Again::Below, 12))
        );
        // Near jump: 0f 82 rel32.
        let near = [0x0f, 0x82, 0xf0, 0xff, 0xff, 0xff];
        assert_eq!(
            found(&with(&add8, &cmp_rax_rcx, &near)),
            Some((1, true, Again::Below, 16))
        );

        // Not taken: wrong targets, a forward jump, conditions under which a
        // growing pointer never stops, signed conditions, another step,
        // comparing with itself, another register, cut short.
        let none = |code: Vec<u8>| assert_eq!(find_loop(&code, 3, 0), None, "{code:02x?}");
        none(with(&add8, &cmp_rax_rcx, &[0x72, 0xf6]));
        none(with(&add8, &cmp_rax_rcx, &[0x72, 0xf3]));
        none(with(&add8, &cmp_rax_rcx, &[0x72, 0x10]));
        none(with(
            &add8,
            &cmp_rax_rcx,
            &[0x0f, 0x82, 0xf4, 0xff, 0xff, 0xff],
        ));
        none(with(&add8, &cmp_rax_rcx, &[0x73, 0xf4])); // jae after cmp rax, rcx
        none(with(&add8, &cmp_rax_rcx, &[0x77, 0xf4])); // ja
        none(with(&add8, &cmp_rcx_rax, &[0x72, 0xf4])); // jb after cmp rcx, rax
        none(with(&add8, &cmp_rax_rcx, &[0x7c, 0xf4])); // jl
        none(with(&add8, &cmp_rax_rcx, &[0x7e, 0xf4])); // jle
        none(with(&add8, &cmp_rax_rcx, &[0xeb, 0xf4])); // jmp
        none(with(&[0x48, 0x83, 0xc0, 0x20], &cmp_rax_rcx, &[0x72, 0xf4])); // add 20h
        none(with(&[0x48, 0x83, 0xc1, 0x40], &cmp_rax_rcx, &[0x72, 0xf4])); // add rcx
        none(with(&[0x40, 0x83, 0xc0, 0x40], &cmp_rax_rcx, &[0x72, 0xf4])); // 32-bit add
        none(with(&add8, &[0x48, 0x39, 0xc0], &[0x72, 0xf4])); // cmp rax, rax
        none(with(&add8, &[0x48, 0x39, 0xd1], &[0x72, 0xf4])); // cmp rcx, rdx
        none(with(&add8, &[0x40, 0x39, 0xc8], &[0x72, 0xf4])); // 32-bit cmp
        none(with(&add8, &[0x48, 0x39, 0x08], &[0x72, 0xf4])); // cmp [rax], rcx
        none(CLZERO_LOOP[..11].to_vec());
        none(CLZERO_LOOP[..5].to_vec());
    }

    #[test]
    fn finds_clwb_loops() {
        // clwb [rcx]; add rcx, 40h; cmp rcx, rax; jbe top
        let code = [
            0x66, 0x0f, 0xae, 0x31, 0x48, 0x83, 0xc1, 0x40, 0x48, 0x39, 0xc1, 0x76, 0xf3,
        ];
        let base = match decode(&code).unwrap().op {
            Op::Clwb { base } => base.unwrap(),
            other => panic!("{other:?}"),
        };
        let want = Loop {
            ptr: 1,
            end: 0,
            ptr_first: true,
            again: Again::BelowOrEqual,
            len: 13,
        };
        assert_eq!(find_loop(&code, 4, base), Some(want));
        // clwb [r9]; add r9, 40h; cmp r9, rdx; jb top
        let code = [
            0x66, 0x41, 0x0f, 0xae, 0x31, 0x49, 0x83, 0xc1, 0x40, 0x49, 0x39, 0xd1, 0x72, 0xf2,
        ];
        let want = Loop {
            ptr: 9,
            end: 2,
            ptr_first: true,
            again: Again::Below,
            len: 14,
        };
        assert_eq!(find_loop(&code, 5, 9), Some(want));
        // clwb [rsp]; lea rsp, [rsp + 40h]; cmp rsp, rdi; jb top (the SIB lea)
        let code = [
            0x66, 0x0f, 0xae, 0x34, 0x24, 0x48, 0x8d, 0x64, 0x24, 0x40, 0x48, 0x39, 0xfc, 0x72,
            0xf1,
        ];
        let want = Loop {
            ptr: 4,
            end: 7,
            ptr_first: true,
            again: Again::Below,
            len: 15,
        };
        assert_eq!(find_loop(&code, 5, 4), Some(want));
        // clwb [r13 + 0] has no plain base: the loop is not looked for.
        assert_eq!(
            decode(&[0x66, 0x41, 0x0f, 0xae, 0x75, 0x00]).unwrap().op,
            Op::Clwb { base: None }
        );
    }

    #[test]
    fn counts_passes() {
        let s = 0x1000_0000;
        // Below: stops at the first pointer >= end.
        assert_eq!(passes(s, s + 1024, Again::Below), Some((16, s + 1024)));
        assert_eq!(passes(s, s + 1025, Again::Below), Some((17, s + 1088)));
        assert_eq!(passes(s, s + 64, Again::Below), Some((1, s + 64)));
        assert_eq!(passes(s, s + 1, Again::Below), Some((1, s + 64)));
        assert_eq!(passes(s, s, Again::Below), Some((1, s + 64)));
        assert_eq!(passes(s, s - 640, Again::Below), Some((1, s + 64)));
        // BelowOrEqual: one more pass when the end is reached exactly.
        assert_eq!(
            passes(s, s + 1024, Again::BelowOrEqual),
            Some((17, s + 1088))
        );
        assert_eq!(
            passes(s, s + 1023, Again::BelowOrEqual),
            Some((16, s + 1024))
        );
        assert_eq!(passes(s, s + 63, Again::BelowOrEqual), Some((1, s + 64)));
        assert_eq!(passes(s, s - 1, Again::BelowOrEqual), Some((1, s + 64)));
        // NotEqual: only when the end is a whole number of lines ahead.
        assert_eq!(passes(s, s + 1024, Again::NotEqual), Some((16, s + 1024)));
        assert_eq!(
            passes(s + 8, s + 8 + 128, Again::NotEqual),
            Some((2, s + 136))
        );
        assert_eq!(passes(s, s + 1000, Again::NotEqual), None);
        assert_eq!(passes(s, s, Again::NotEqual), None);
        assert_eq!(passes(s, s - 64, Again::NotEqual), None);
        // Wrapping and implausible lengths are refused.
        assert_eq!(passes(u64::MAX - 63, u64::MAX, Again::BelowOrEqual), None);
        assert_eq!(passes(u64::MAX - 100, u64::MAX, Again::Below), None);
        assert_eq!(passes(0, u64::MAX, Again::Below), None);
        assert_eq!(
            passes(s, s + (64 << 30), Again::Below),
            Some((1 << 30, s + (64 << 30)))
        );
        assert_eq!(passes(s, s + (64 << 30) + 1, Again::Below), None);
    }
}
