use crate::utils::error::{Error, Result, Unsupported};

pub const DELTA: u64 = 0x03;
pub const X86: u64 = 0x04;
pub const POWERPC: u64 = 0x05;
pub const IA64: u64 = 0x06;
pub const ARM: u64 = 0x07;
pub const ARM_THUMB: u64 = 0x08;
pub const SPARC: u64 = 0x09;
pub const ARM64: u64 = 0x0A;
pub const RISCV: u64 = 0x0B;
pub const LZMA2: u64 = 0x21;

pub fn decode(id: u64, props: &[u8], data: &mut [u8]) -> Result<()> {
    match id {
        DELTA => {
            let distance = *props.first().ok_or_else(|| Error::malformed("xz delta filter carries no distance"))? as usize + 1;
            delta_decode(data, distance);
            Ok(())
        }
        X86 => {
            x86_decode(data, start_offset(props)?);
            Ok(())
        }
        POWERPC => {
            powerpc_decode(data, start_offset(props)?);
            Ok(())
        }
        ARM => {
            arm_decode(data, start_offset(props)?);
            Ok(())
        }
        ARM_THUMB => {
            arm_thumb_decode(data, start_offset(props)?);
            Ok(())
        }
        SPARC => {
            sparc_decode(data, start_offset(props)?);
            Ok(())
        }
        ARM64 => {
            arm64_decode(data, start_offset(props)?);
            Ok(())
        }
        IA64 | RISCV => Err(Error::Unsupported(Unsupported::Other("an xz block using the IA-64 or RISC-V branch converter"))),
        LZMA2 => Err(Error::malformed("xz block places LZMA2 before the end of its filter chain")),
        _ => Err(Error::Unsupported(Unsupported::Other("an xz block using an unassigned filter"))),
    }
}

fn start_offset(props: &[u8]) -> Result<u32> {
    match props.len() {
        0 => Ok(0),
        4 => Ok(u32::from_le_bytes(props.try_into().expect("four bytes"))),
        other => Err(Error::malformed(format!("xz branch filter carries {other} property bytes; 0 or 4 are legal"))),
    }
}

fn delta_decode(data: &mut [u8], distance: usize) {
    for i in distance..data.len() {
        data[i] = data[i].wrapping_add(data[i - distance]);
    }
}

fn x86_decode(data: &mut [u8], start: u32) {
    const MASK_TO_ALLOWED: [bool; 8] = [true, true, true, false, true, false, false, false];
    const MASK_TO_BIT: [u32; 8] = [0, 1, 2, 2, 3, 3, 3, 3];

    if data.len() < 5 {
        return;
    }

    let mut previous_mask = 0u32;
    let mut previous_position = 0i64;
    let mut i = 0usize;

    while i + 4 < data.len() {
        if data[i] & 0xFE != 0xE8 {
            i += 1;
            continue;
        }

        let position = i as i64;
        let gap = position - previous_position;
        previous_position = position;

        if gap > 3 {
            previous_mask = 0;
        } else {
            previous_mask = (previous_mask << (gap - 1)) & 7;
            if previous_mask != 0 {
                let index = MASK_TO_BIT[previous_mask as usize] as usize;
                if !MASK_TO_ALLOWED[previous_mask as usize] || data[i + 4 - index] == 0xFF {
                    previous_mask = ((previous_mask << 1) & 7) | 1;
                    i += 1;
                    continue;
                }
            }
        }

        if data[i + 4] == 0 || data[i + 4] == 0xFF {
            let mut source = u32::from_le_bytes([data[i + 1], data[i + 2], data[i + 3], data[i + 4]]);
            let mut destination;
            loop {
                destination = source.wrapping_sub(start.wrapping_add(i as u32).wrapping_add(5));
                if previous_mask == 0 {
                    break;
                }
                let index = MASK_TO_BIT[previous_mask as usize] * 8;
                let byte = ((destination >> (24 - index)) & 0xFF) as u8;
                if byte != 0 && byte != 0xFF {
                    break;
                }
                source = destination ^ ((1u32 << (32 - index)).wrapping_sub(1));
            }

            destination &= 0x01FF_FFFF;
            destination |= 0u32.wrapping_sub(destination & 0x0100_0000);
            data[i + 1..i + 5].copy_from_slice(&destination.to_le_bytes());
            i += 5;
            previous_mask = 0;
        } else {
            previous_mask = ((previous_mask << 1) & 7) | 1;
            i += 1;
        }
    }
}

fn powerpc_decode(data: &mut [u8], start: u32) {
    let mut i = 0usize;
    while i + 4 <= data.len() {
        if data[i] & 0xFC == 0x48 && data[i + 3] & 0x03 == 1 {
            let source = (((data[i] & 0x03) as u32) << 24) | ((data[i + 1] as u32) << 16) | ((data[i + 2] as u32) << 8) | (data[i + 3] & 0xFC) as u32;
            let destination = source.wrapping_sub(start.wrapping_add(i as u32));
            data[i] = 0x48 | ((destination >> 24) & 0x03) as u8;
            data[i + 1] = (destination >> 16) as u8;
            data[i + 2] = (destination >> 8) as u8;
            data[i + 3] = (data[i + 3] & 0x03) | destination as u8 & 0xFC;
        }
        i += 4;
    }
}

fn arm_decode(data: &mut [u8], start: u32) {
    let mut i = 0usize;
    while i + 4 <= data.len() {
        if data[i + 3] == 0xEB {
            let source = ((data[i + 2] as u32) << 16) | ((data[i + 1] as u32) << 8) | (data[i] as u32);
            let source = source << 2;
            let destination = source.wrapping_sub(start.wrapping_add(i as u32).wrapping_add(8));
            let destination = destination >> 2;
            data[i + 2] = (destination >> 16) as u8;
            data[i + 1] = (destination >> 8) as u8;
            data[i] = destination as u8;
        }
        i += 4;
    }
}

fn arm_thumb_decode(data: &mut [u8], start: u32) {
    let mut i = 0usize;
    while i + 4 <= data.len() {
        if data[i + 1] & 0xF8 == 0xF0 && data[i + 3] & 0xF8 == 0xF8 {
            let source = (((data[i + 1] & 0x07) as u32) << 19) | ((data[i] as u32) << 11) | (((data[i + 3] & 0x07) as u32) << 8) | (data[i + 2] as u32);
            let source = source << 1;
            let destination = source.wrapping_sub(start.wrapping_add(i as u32).wrapping_add(4));
            let destination = destination >> 1;
            data[i + 1] = 0xF0 | ((destination >> 19) & 0x07) as u8;
            data[i] = (destination >> 11) as u8;
            data[i + 3] = 0xF8 | ((destination >> 8) & 0x07) as u8;
            data[i + 2] = destination as u8;
            i += 2;
        }
        i += 2;
    }
}

fn sparc_decode(data: &mut [u8], start: u32) {
    let mut i = 0usize;
    while i + 4 <= data.len() {
        let is_call = (data[i] == 0x40 && data[i + 1] & 0xC0 == 0) || (data[i] == 0x7F && data[i + 1] & 0xC0 == 0xC0);
        if is_call {
            let source = u32::from_be_bytes([data[i], data[i + 1], data[i + 2], data[i + 3]]);
            let source = source << 2;
            let destination = source.wrapping_sub(start.wrapping_add(i as u32));
            let destination = destination >> 2;
            let destination = ((0x4000_0000u32.wrapping_sub(destination & 0x0040_0000)) | 0x4000_0000 | (destination & 0x003F_FFFF)).to_be_bytes();
            data[i..i + 4].copy_from_slice(&destination);
        }
        i += 4;
    }
}

fn arm64_decode(data: &mut [u8], start: u32) {
    let mut i = 0usize;
    while i + 4 <= data.len() {
        let word = u32::from_le_bytes([data[i], data[i + 1], data[i + 2], data[i + 3]]);

        if word >> 26 == 0x25 {
            let source = word & 0x03FF_FFFF;
            let destination = source.wrapping_sub((start.wrapping_add(i as u32)) >> 2);
            let rewritten = 0x9400_0000 | (destination & 0x03FF_FFFF);
            data[i..i + 4].copy_from_slice(&rewritten.to_le_bytes());
        } else if word & 0x9F00_0000 == 0x9000_0000 {
            let source = ((word >> 29) & 3) | ((word >> 3) & 0x001F_FFFC);
            if (source.wrapping_add(0x0002_0000) & 0x001C_0000) == 0 {
                let destination = source.wrapping_sub((start.wrapping_add(i as u32)) >> 12);
                let rewritten = (word & 0x9000_001F) | ((destination & 3) << 29) | ((destination & 0x0003_FFFC) << 3);
                data[i..i + 4].copy_from_slice(&rewritten.to_le_bytes());
            }
        }
        i += 4;
    }
}
