use crate::codecs::legacy::bits::BitReader;
use crate::utils::error::{Error, Result};

const ESCAPE: u8 = 0x90;

const LENGTH_MASK: [u8; 5] = [0, 0x7f, 0x3f, 0x1f, 0x0f];
const DISTANCE_SHIFT: [u32; 5] = [0, 7, 6, 5, 4];
const DISTANCE_MASK: [u32; 5] = [0, 0x01, 0x03, 0x07, 0x0f];

fn index_bits(set_size: usize) -> u32 {
    match set_size {
        0 => 0,
        1 => 1,
        n => (usize::BITS - (n - 1).leading_zeros()).max(1),
    }
}

fn expand_followers(data: &[u8]) -> Result<Vec<u8>> {
    let mut bits = BitReader::new(data);

    let mut sets: Vec<Vec<u8>> = vec![Vec::new(); 256];
    for value in (0..256).rev() {
        let count = bits.need(6, "a reduce follower set size")? as usize;
        if count > 32 {
            return Err(Error::malformed(format!("reduce follower set of {count} entries; 32 is the maximum")));
        }
        let mut set = Vec::with_capacity(count);
        for _ in 0..count {
            set.push(bits.need(8, "a reduce follower")? as u8);
        }
        sets[value] = set;
    }

    let mut out = Vec::new();
    let mut last = 0u8;

    loop {
        let set = &sets[last as usize];
        let byte = if set.is_empty() {
            match bits.bits(8) {
                Some(b) => b as u8,
                None => break,
            }
        } else {
            match bits.bit() {
                None => break,
                Some(1) => match bits.bits(8) {
                    Some(b) => b as u8,
                    None => break,
                },
                Some(_) => {
                    let width = index_bits(set.len());
                    let Some(index) = bits.bits(width) else { break };
                    *set.get(index as usize).ok_or_else(|| Error::malformed("reduce follower index is outside its set"))?
                }
            }
        };

        out.push(byte);
        last = byte;
    }

    Ok(out)
}

fn expand_pairs(data: &[u8], factor: usize, expected: usize) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(expected);
    let mut i = 0usize;

    while i < data.len() {
        let byte = data[i];
        i += 1;

        if byte != ESCAPE {
            out.push(byte);
            continue;
        }

        let Some(&control) = data.get(i) else { break };
        i += 1;
        if control == 0 {
            out.push(ESCAPE);
            continue;
        }

        let mut length = (control & LENGTH_MASK[factor]) as usize;
        if length == LENGTH_MASK[factor] as usize {
            let Some(&extra) = data.get(i) else { break };
            i += 1;
            length += extra as usize;
        }
        length += 3;

        let Some(&low) = data.get(i) else { break };
        i += 1;
        let high = (control as u32 >> DISTANCE_SHIFT[factor]) & DISTANCE_MASK[factor];
        let distance = ((high << 8) | low as u32) as usize + 1;

        for _ in 0..length {
            let byte = if distance > out.len() { 0 } else { out[out.len() - distance] };
            out.push(byte);
        }
    }

    Ok(out)
}

pub fn decompress(data: &[u8], method: u16, size_hint: usize) -> Result<Vec<u8>> {
    let factor = match method {
        2..=5 => (method - 1) as usize,
        other => return Err(Error::malformed(format!("method {other} is not one of the reduce methods"))),
    };

    let intermediate = expand_followers(data)?;
    expand_pairs(&intermediate, factor, size_hint)
}
