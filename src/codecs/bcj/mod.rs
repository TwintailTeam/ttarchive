pub mod bcj2;

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
    convert(id, props, data, Way::Decode)
}

pub fn encode(id: u64, props: &[u8], data: &mut [u8]) -> Result<()> {
    convert(id, props, data, Way::Encode)
}

pub const fn reserve(id: u64) -> usize {
    match id {
        X86 => 5,
        IA64 => 16,
        RISCV => 8,
        DELTA => 0,
        _ => 4,
    }
}

pub struct Converter {
    filter: u64,
    properties: Vec<u8>,
    way: Way,
    position: u32,
    mask: u32,
    history: Vec<u8>,
}

impl Converter {
    pub fn decode(filter: u64, properties: &[u8]) -> Result<Self> {
        Converter::new(filter, properties, Way::Decode)
    }

    pub fn encode(filter: u64, properties: &[u8]) -> Result<Self> {
        Converter::new(filter, properties, Way::Encode)
    }

    fn new(filter: u64, properties: &[u8], way: Way) -> Result<Self> {
        let position = match filter {
            DELTA => {
                distance_of(properties)?;
                0
            }
            other => start_offset(properties, alignment(other)?)?,
        };

        Ok(Converter { filter, properties: properties.to_vec(), way, position, mask: 0, history: Vec::new() })
    }

    pub fn reserve(&self) -> usize {
        reserve(self.filter)
    }

    pub fn convert(&mut self, data: &mut [u8], last: bool) -> Result<usize> {
        if self.filter == DELTA {
            let distance = distance_of(&self.properties)?;
            let taken = self.delta(data, distance);
            return Ok(taken);
        }

        let done = match self.filter {
            X86 => x86_convert(data, self.position, &mut self.mask, self.way),
            POWERPC => powerpc_convert(data, self.position, self.way),
            ARM => arm_convert(data, self.position, self.way),
            ARM_THUMB => arm_thumb_convert(data, self.position, self.way),
            SPARC => sparc_convert(data, self.position, self.way),
            ARM64 => arm64_convert(data, self.position, self.way),
            IA64 => ia64_convert(data, self.position, self.way),
            RISCV => match self.way {
                Way::Decode => riscv_decode(data, self.position),
                Way::Encode => riscv_encode(data, self.position),
            },
            LZMA2 => return Err(Error::malformed("a filter chain places LZMA2 before its end")),
            _ => return Err(Error::Unsupported(Unsupported::Other("a filter this build does not know"))),
        };

        let done = if last { data.len() } else { done };
        self.position = self.position.wrapping_add(done as u32);
        Ok(done)
    }

    fn delta(&mut self, data: &mut [u8], distance: usize) -> usize {
        let mut work = std::mem::take(&mut self.history);
        let offset = work.len();
        work.extend_from_slice(data);

        for at in offset..work.len() {
            let previous = if at >= distance { work[at - distance] } else { 0 };
            work[at] = match self.way {
                Way::Decode => work[at].wrapping_add(previous),
                Way::Encode => work[at].wrapping_sub(previous),
            };

            // The encoder's own output is not what the next byte looks back at:
            // it subtracts from the bytes as they arrived, so the history it
            // keeps has to be the input rather than the output.
            if self.way == Way::Encode {
                let restored = work[at].wrapping_add(previous);
                data[at - offset] = work[at];
                work[at] = restored;
            } else {
                data[at - offset] = work[at];
            }
        }

        self.history = work;
        let drop = self.history.len().saturating_sub(distance);
        self.history.drain(..drop);
        data.len()
    }
}

fn alignment(id: u64) -> Result<u32> {
    match id {
        X86 => Ok(1),
        POWERPC | ARM | SPARC | ARM64 => Ok(4),
        ARM_THUMB | RISCV => Ok(2),
        IA64 => Ok(16),
        LZMA2 => Err(Error::malformed("a filter chain places LZMA2 before its end")),
        _ => Err(Error::Unsupported(Unsupported::Other("a filter this build does not know"))),
    }
}

fn distance_of(props: &[u8]) -> Result<usize> {
    Ok(*props.first().ok_or_else(|| Error::malformed("delta filter carries no distance"))? as usize + 1)
}

fn convert(id: u64, props: &[u8], data: &mut [u8], way: Way) -> Result<()> {
    match id {
        DELTA => {
            let distance = *props.first().ok_or_else(|| Error::malformed("xz delta filter carries no distance"))? as usize + 1;
            match way {
                Way::Decode => delta_decode(data, distance),
                Way::Encode => delta_encode(data, distance),
            }
            Ok(())
        }
        X86 => {
            let mut mask = 0u32;
            x86_convert(data, start_offset(props, 1)?, &mut mask, way);
            Ok(())
        }
        POWERPC => {
            powerpc_convert(data, start_offset(props, 4)?, way);
            Ok(())
        }
        ARM => {
            arm_convert(data, start_offset(props, 4)?, way);
            Ok(())
        }
        ARM_THUMB => {
            arm_thumb_convert(data, start_offset(props, 2)?, way);
            Ok(())
        }
        SPARC => {
            sparc_convert(data, start_offset(props, 4)?, way);
            Ok(())
        }
        ARM64 => {
            arm64_convert(data, start_offset(props, 4)?, way);
            Ok(())
        }
        IA64 => {
            ia64_convert(data, start_offset(props, 16)?, way);
            Ok(())
        }
        RISCV => {
            match way {
                Way::Decode => riscv_decode(data, start_offset(props, 2)?),
                Way::Encode => riscv_encode(data, start_offset(props, 2)?),
            };
            Ok(())
        }
        LZMA2 => Err(Error::malformed("xz block places LZMA2 before the end of its filter chain")),
        _ => Err(Error::Unsupported(Unsupported::Other("an xz block using an unassigned filter"))),
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Way {
    Encode,
    Decode,
}

impl Way {
    fn shift(self, source: u32, position: u32) -> u32 {
        match self {
            Way::Encode => source.wrapping_add(position),
            Way::Decode => source.wrapping_sub(position),
        }
    }
}

fn start_offset(props: &[u8], alignment: u32) -> Result<u32> {
    let start = match props.len() {
        0 => 0,
        4 => u32::from_le_bytes(props.try_into().expect("four bytes")),
        other => return Err(Error::malformed(format!("xz branch filter carries {other} property bytes; 0 or 4 are legal"))),
    };

    if start & (alignment - 1) != 0 {
        return Err(Error::malformed(format!("a branch filter starts at {start:#x}, which is not a multiple of the {alignment} bytes its instructions take")));
    }
    Ok(start)
}

fn delta_decode(data: &mut [u8], distance: usize) {
    for i in distance..data.len() {
        data[i] = data[i].wrapping_add(data[i - distance]);
    }
}

fn delta_encode(data: &mut [u8], distance: usize) {
    for i in (distance..data.len()).rev() {
        data[i] = data[i].wrapping_sub(data[i - distance]);
    }
}

fn x86_convert(data: &mut [u8], start: u32, mask: &mut u32, way: Way) -> usize {
    let test_msbyte = |b: u8| b.wrapping_add(1) & 0xFE == 0;

    if data.len() < 5 {
        return 0;
    }

    let limit = data.len() - 4;
    let ip = start.wrapping_add(5);
    let mut pos = 0usize;

    loop {
        let mut at = pos;
        while at < limit && data[at] & 0xFE != 0xE8 {
            at += 1;
        }

        let gap = at - pos;
        pos = at;

        if at >= limit {
            *mask = if gap > 2 { 0 } else { *mask >> gap };
            return pos;
        }

        if gap > 2 {
            *mask = 0;
        } else {
            *mask >>= gap;
            if *mask != 0 && (*mask > 4 || *mask == 3 || test_msbyte(data[pos + (*mask >> 1) as usize + 1])) {
                *mask = (*mask >> 1) | 4;
                pos += 1;
                continue;
            }
        }

        if test_msbyte(data[pos + 4]) {
            let mut value = u32::from_le_bytes([data[pos + 1], data[pos + 2], data[pos + 3], data[pos + 4]]);
            let current = ip.wrapping_add(pos as u32);
            pos += 5;

            value = way.shift(value, current);
            if *mask != 0 {
                let shift = (*mask & 6) << 2;
                if test_msbyte((value >> shift) as u8) {
                    value ^= (0x100u32 << shift).wrapping_sub(1);
                    value = way.shift(value, current);
                }
                *mask = 0;
            }

            data[pos - 4] = value as u8;
            data[pos - 3] = (value >> 8) as u8;
            data[pos - 2] = (value >> 16) as u8;
            data[pos - 1] = 0u8.wrapping_sub(((value >> 24) & 1) as u8);
        } else {
            *mask = (*mask >> 1) | 4;
            pos += 1;
        }
    }
}

fn powerpc_convert(data: &mut [u8], start: u32, way: Way) -> usize {
    let mut i = 0usize;
    while i + 4 <= data.len() {
        if data[i] & 0xFC == 0x48 && data[i + 3] & 0x03 == 1 {
            let source = (((data[i] & 0x03) as u32) << 24) | ((data[i + 1] as u32) << 16) | ((data[i + 2] as u32) << 8) | (data[i + 3] & 0xFC) as u32;
            let destination = way.shift(source, start.wrapping_add(i as u32));
            data[i] = 0x48 | ((destination >> 24) & 0x03) as u8;
            data[i + 1] = (destination >> 16) as u8;
            data[i + 2] = (destination >> 8) as u8;
            data[i + 3] = (data[i + 3] & 0x03) | destination as u8 & 0xFC;
        }
        i += 4;
    }

    i
}

fn arm_convert(data: &mut [u8], start: u32, way: Way) -> usize {
    let mut i = 0usize;
    while i + 4 <= data.len() {
        if data[i + 3] == 0xEB {
            let source = ((data[i + 2] as u32) << 16) | ((data[i + 1] as u32) << 8) | (data[i] as u32);
            let source = source << 2;
            let destination = way.shift(source, start.wrapping_add(i as u32).wrapping_add(8));
            let destination = destination >> 2;
            data[i + 2] = (destination >> 16) as u8;
            data[i + 1] = (destination >> 8) as u8;
            data[i] = destination as u8;
        }
        i += 4;
    }

    i
}

fn arm_thumb_convert(data: &mut [u8], start: u32, way: Way) -> usize {
    let mut i = 0usize;
    while i + 4 <= data.len() {
        if data[i + 1] & 0xF8 == 0xF0 && data[i + 3] & 0xF8 == 0xF8 {
            let source = (((data[i + 1] & 0x07) as u32) << 19) | ((data[i] as u32) << 11) | (((data[i + 3] & 0x07) as u32) << 8) | (data[i + 2] as u32);
            let source = source << 1;
            let destination = way.shift(source, start.wrapping_add(i as u32).wrapping_add(4));
            let destination = destination >> 1;
            data[i + 1] = 0xF0 | ((destination >> 19) & 0x07) as u8;
            data[i] = (destination >> 11) as u8;
            data[i + 3] = 0xF8 | ((destination >> 8) & 0x07) as u8;
            data[i + 2] = destination as u8;
            i += 2;
        }
        i += 2;
    }

    i
}

fn sparc_convert(data: &mut [u8], start: u32, way: Way) -> usize {
    let mut i = 0usize;
    while i + 4 <= data.len() {
        let is_call = (data[i] == 0x40 && data[i + 1] & 0xC0 == 0) || (data[i] == 0x7F && data[i + 1] & 0xC0 == 0xC0);
        if is_call {
            let source = u32::from_be_bytes([data[i], data[i + 1], data[i + 2], data[i + 3]]);
            let source = source << 2;
            let destination = way.shift(source, start.wrapping_add(i as u32));
            let destination = destination >> 2;
            let destination = ((0x4000_0000u32.wrapping_sub(destination & 0x0040_0000)) | 0x4000_0000 | (destination & 0x003F_FFFF)).to_be_bytes();
            data[i..i + 4].copy_from_slice(&destination);
        }
        i += 4;
    }

    i
}

fn ia64_convert(data: &mut [u8], start: u32, way: Way) -> usize {
    let mut i = 0usize;
    while i + 16 <= data.len() {
        let first = (0x334B_0000u32 >> (data[i] & 0x1E)) & 3;
        if first == 0 {
            i += 16;
            continue;
        }

        for slot in first + 1..=4 {
            let at = i + slot as usize * 5 - 8;
            if (data[at + 3] >> slot) & 0x0F != 0x05 || ((data[at - 1] as u32 | (data[at] as u32) << 8) >> slot) & 0x70 != 0 {
                continue;
            }

            let word = u32::from_le_bytes([data[at], data[at + 1], data[at + 2], data[at + 3]]);
            let field = word >> slot;

            let source = (field & 0x000F_FFFF) | ((field >> 3) & 0x0010_0000);
            let shifted = way.shift(source << 4, start.wrapping_add(i as u32)) >> 4;
            let destination = (shifted & 0x001F_FFFF).wrapping_add(0x0070_0000) & 0x008F_FFFF;

            let rewritten = (word & !(0x008F_FFFFu32 << slot)) | (destination << slot);
            data[at..at + 4].copy_from_slice(&rewritten.to_le_bytes());
        }

        i += 16;
    }

    i
}

fn riscv_encode(data: &mut [u8], start: u32) -> usize {
    if data.len() < 8 {
        return 0;
    }
    let limit = data.len() - 8;

    let mut i = 0usize;
    while i <= limit {
        let opcode = data[i] as u32;

        if opcode == 0xEF {
            let b1 = data[i + 1] as u32;
            if b1 & 0x0D != 0 {
                i += 2;
                continue;
            }

            let b2 = data[i + 2] as u32;
            let b3 = data[i + 3] as u32;

            let address = (((b1 & 0xF0) << 8) | ((b2 & 0x0F) << 16) | ((b2 & 0x10) << 7) | ((b2 & 0xE0) >> 4) | ((b3 & 0x7F) << 4) | ((b3 & 0x80) << 13))
                .wrapping_add(start.wrapping_add(i as u32));

            data[i + 1] = ((b1 & 0x0F) | ((address >> 13) & 0xF0)) as u8;
            data[i + 2] = (address >> 9) as u8;
            data[i + 3] = (address >> 1) as u8;
            i += 4;
        } else if opcode & 0x7F == 0x17 {
            let mut first = opcode | ((data[i + 1] as u32) << 8) | ((data[i + 2] as u32) << 16) | ((data[i + 3] as u32) << 24);
            let second;

            if first & 0xE80 != 0 {
                let paired = u32::from_le_bytes([data[i + 4], data[i + 5], data[i + 6], data[i + 7]]);
                if ((first << 8) ^ paired.wrapping_sub(3)) & 0x000F_8003 != 0 {
                    i += 6;
                    continue;
                }

                let address =
                    (first & 0xFFFF_F000).wrapping_add((paired >> 20).wrapping_sub((paired >> 19) & 0x1000)).wrapping_add(start.wrapping_add(i as u32));

                second = address.swap_bytes();
                first = 0x17 | (2 << 7) | (paired << 12);
            } else {
                let fake_rs1 = first >> 27;
                if (first.wrapping_sub(0x3117) << 18) >= (fake_rs1 & 0x1D) {
                    i += 4;
                    continue;
                }

                let fake_address = u32::from_le_bytes([data[i + 4], data[i + 5], data[i + 6], data[i + 7]]);
                second = (first >> 12) | (fake_address << 20);
                first = 0x17 | (fake_rs1 << 7) | (fake_address & 0xFFFF_F000);
            }

            data[i..i + 4].copy_from_slice(&first.to_le_bytes());
            data[i + 4..i + 8].copy_from_slice(&second.to_le_bytes());
            i += 8;
        } else {
            i += 2;
        }
    }
    i
}

fn riscv_decode(data: &mut [u8], start: u32) -> usize {
    if data.len() < 8 {
        return 0;
    }
    let limit = data.len() - 8;

    let mut i = 0usize;
    while i <= limit {
        let opcode = data[i] as u32;

        if opcode == 0xEF {
            let b1 = data[i + 1] as u32;
            if b1 & 0x0D != 0 {
                i += 2;
                continue;
            }

            let b2 = data[i + 2] as u32;
            let b3 = data[i + 3] as u32;
            let address = (((b1 & 0xF0) << 13) | (b2 << 9) | (b3 << 1)).wrapping_sub(start.wrapping_add(i as u32));

            data[i + 1] = ((b1 & 0x0F) | ((address >> 8) & 0xF0)) as u8;
            data[i + 2] = (((address >> 16) & 0x0F) | ((address >> 7) & 0x10) | ((address << 4) & 0xE0)) as u8;
            data[i + 3] = (((address >> 4) & 0x7F) | ((address >> 13) & 0x80)) as u8;
            i += 4;
        } else if opcode & 0x7F == 0x17 {
            let mut first = opcode | ((data[i + 1] as u32) << 8) | ((data[i + 2] as u32) << 16) | ((data[i + 3] as u32) << 24);
            let second;

            if first & 0xE80 != 0 {
                let paired = u32::from_le_bytes([data[i + 4], data[i + 5], data[i + 6], data[i + 7]]);
                if ((first << 8) ^ paired.wrapping_sub(3)) & 0x000F_8003 != 0 {
                    i += 6;
                    continue;
                }
                second = (first & 0xFFFF_F000).wrapping_add(paired >> 20);
                first = 0x17 | (2 << 7) | (paired << 12);
            } else {
                let rs1 = first >> 27;
                if (first.wrapping_sub(0x3117) << 18) >= (rs1 & 0x1D) {
                    i += 4;
                    continue;
                }
                let address = u32::from_be_bytes([data[i + 4], data[i + 5], data[i + 6], data[i + 7]]).wrapping_sub(start.wrapping_add(i as u32));
                second = (first >> 12) | (address << 20);
                first = 0x17 | (rs1 << 7) | (address.wrapping_add(0x800) & 0xFFFF_F000);
            }

            data[i..i + 4].copy_from_slice(&first.to_le_bytes());
            data[i + 4..i + 8].copy_from_slice(&second.to_le_bytes());
            i += 8;
        } else {
            i += 2;
        }
    }
    i
}

fn arm64_convert(data: &mut [u8], start: u32, way: Way) -> usize {
    let mut i = 0usize;
    while i + 4 <= data.len() {
        let word = u32::from_le_bytes([data[i], data[i + 1], data[i + 2], data[i + 3]]);

        if word >> 26 == 0x25 {
            let source = word & 0x03FF_FFFF;
            let destination = way.shift(source, (start.wrapping_add(i as u32)) >> 2);
            let rewritten = 0x9400_0000 | (destination & 0x03FF_FFFF);
            data[i..i + 4].copy_from_slice(&rewritten.to_le_bytes());
        } else if word & 0x9F00_0000 == 0x9000_0000 {
            let source = ((word >> 29) & 3) | ((word >> 3) & 0x001F_FFFC);
            if (source.wrapping_add(0x0002_0000) & 0x001C_0000) == 0 {
                let destination = way.shift(source, (start.wrapping_add(i as u32)) >> 12);
                let sign = 0u32.wrapping_sub(destination & 0x0002_0000) & 0x00E0_0000;
                let rewritten = (word & 0x9000_001F) | ((destination & 3) << 29) | ((destination & 0x0003_FFFC) << 3) | sign;
                data[i..i + 4].copy_from_slice(&rewritten.to_le_bytes());
            }
        }
        i += 4;
    }

    i
}
