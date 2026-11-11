//! Applying each arm64 relocation to the instruction it's on, as a linker does: pure functions of
//! the instruction, where it is and where its target is. Each checks the instruction is one the
//! relocation can be on, and that the target is in reach, and refuses otherwise.

use crate::{Refused, refuse};

/// How far a `b` or `bl` reaches either way: 128 MB.
pub(crate) const BRANCH_REACH: i64 = 1 << 27;

/// How far an `adrp` reaches either way: 4 GB.
const PAGE_REACH: i64 = 1 << 32;

/// A `b` or `bl` at `place`, made to go to `target`.
pub(crate) fn branch26(insn: u32, place: u64, target: u64) -> Result<u32, Refused> {
    if insn & 0x7c00_0000 != 0x1400_0000 {
        return refuse(format!("a branch relocation is on {insn:#010x}, which isn't `b` or `bl`"));
    }
    let delta = displacement(place, target)?;
    if delta % 4 != 0 || !(-BRANCH_REACH..BRANCH_REACH).contains(&delta) {
        return refuse(format!("a branch can't reach {delta} bytes"));
    }
    let field =
        u32::try_from((delta >> 2) & 0x03ff_ffff).map_err(|_| Refused("a branch overflows".to_owned()))?;
    Ok((insn & 0xfc00_0000) | field)
}

/// An `adrp` at `place`, made to give the 4 KB page `target` is on.
pub(crate) fn page21(insn: u32, place: u64, target: u64) -> Result<u32, Refused> {
    if insn & 0x9f00_0000 != 0x9000_0000 {
        return refuse(format!("a page relocation is on {insn:#010x}, which isn't `adrp`"));
    }
    let delta = displacement(place & !0xfff, target & !0xfff)?;
    if !(-PAGE_REACH..PAGE_REACH).contains(&delta) {
        return refuse(format!("an `adrp` can't reach {delta} bytes"));
    }
    let pages =
        u32::try_from((delta >> 12) & 0x1f_ffff).map_err(|_| Refused("an `adrp` overflows".to_owned()))?;
    let (low, high) = (pages & 3, pages >> 2);
    Ok((insn & 0x9f00_001f) | (low << 29) | (high << 5))
}

/// An `add` or a load or store at an unsigned offset, made to add `target`'s offset in its page,
/// scaled as the access is. The offset has to be a multiple of the access's size.
pub(crate) fn pageoff12(insn: u32, target: u64) -> Result<u32, Refused> {
    let offset = u32::try_from(target & 0xfff).map_err(|_| Refused("an offset overflows".to_owned()))?;
    let scale = if insn & 0x7f80_0000 == 0x1100_0000 {
        // `add` of an immediate, 32 or 64 bits, which doesn't scale.
        if insn & (1 << 22) != 0 {
            return refuse(format!("a page offset is on {insn:#010x}, an `add` with its offset shifted"));
        }
        0
    } else if insn & 0x3b00_0000 == 0x3900_0000 {
        // A load or store at an unsigned offset, scaled by its size: a 128-bit vector one by 16.
        let size = insn >> 30;
        if (insn >> 26) & 1 == 1 && (insn >> 23) & 1 == 1 && size == 0 { 4 } else { size }
    } else {
        return refuse(format!("a page offset is on {insn:#010x}, which isn't `add` or a load or store"));
    };
    if offset % (1 << scale) != 0 {
        return refuse(format!(
            "a page offset of {offset} isn't aligned to its access of {} bytes",
            1 << scale
        ));
    }
    Ok((insn & !(0xfff << 10)) | ((offset >> scale) << 10))
}

/// The bytes of a branch stub reaching `target` from anywhere: `ldr x16, #8; br x16; .quad target`.
pub(crate) fn stub(target: u64) -> [u8; 16] {
    let mut stub = [0; 16];
    let (load, branch) = (0x5800_0050_u32.to_le_bytes(), 0xd61f_0200_u32.to_le_bytes());
    for (slot, byte) in stub.iter_mut().zip(load.iter().chain(&branch).chain(&target.to_le_bytes())) {
        *slot = *byte;
    }
    stub
}

/// How far `target` is from `place`, as a signed number.
fn displacement(place: u64, target: u64) -> Result<i64, Refused> {
    let (place, target) = (i128::from(place), i128::from(target));
    i64::try_from(target - place).map_or_else(|_| refuse("a target is too far to say"), Ok)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `bl` at 0x1000 to 0x2000, and back, as a linker writes them.
    #[test]
    fn a_branch_is_written_as_a_linker_writes_it() {
        assert_eq!(branch26(0x9400_0000, 0x1000, 0x2000), Ok(0x9400_0400));
        assert_eq!(branch26(0x9400_0000, 0x2000, 0x1000), Ok(0x97ff_fc00));
        assert!(branch26(0x9400_0000, 0, 1 << 27).is_err(), "out of reach");
        assert!(branch26(0x9400_0000, 0, 2).is_err(), "not a whole instruction away");
        assert!(branch26(0x9100_0000, 0, 4).is_err(), "not a branch");
    }

    #[test]
    fn an_adrp_takes_the_pages_between() {
        // adrp x8, at 0x1_0000_0ffc to 0x1_0000_3010: three pages on.
        assert_eq!(page21(0x9000_0008, 0x1_0000_0ffc, 0x1_0000_3010), Ok(0xf000_0008));
        assert!(page21(0x9000_0008, 0, 1 << 33).is_err(), "out of reach");
        assert!(page21(0x9100_0008, 0, 0).is_err(), "not adrp");
    }

    #[test]
    fn a_page_offset_scales_with_its_access() {
        // add x0, x0, #off
        assert_eq!(pageoff12(0x9100_0000, 0x123), Ok(0x9104_8c00));
        // ldr x0, [x0, #off]: scaled by 8.
        assert_eq!(pageoff12(0xf940_0000, 0x18), Ok(0xf940_0c00));
        assert!(pageoff12(0xf940_0000, 0x1c).is_err(), "not aligned to 8");
        // ldr q0, [x0, #off]: scaled by 16.
        assert_eq!(pageoff12(0x3dc0_0000, 0x20), Ok(0x3dc0_0800));
        assert!(pageoff12(0xd503_201f, 0).is_err(), "a nop");
    }

    #[test]
    fn a_stub_loads_its_target_and_branches_to_it() {
        let stub = stub(0x0123_4567_89ab_cdef);
        assert_eq!(stub.get(..8), Some(&[0x50, 0, 0, 0x58, 0, 0x02, 0x1f, 0xd6][..]));
        assert_eq!(stub.get(8..), Some(&0x0123_4567_89ab_cdef_u64.to_le_bytes()[..]));
    }
}
