use crate::codecs::legacy::bits::BitReader;
use crate::utils::error::{Error, Result};

pub const FLAG_LARGE_DICTIONARY: u16 = 1 << 1;
pub const FLAG_LITERAL_TREE: u16 = 1 << 2;

const MAX_CODE_BITS: u32 = 16;

struct Tree {
    lengths: Vec<u8>,
    codes: Vec<u16>,
}

impl Tree {
    fn parse(data: &[u8], symbols: usize) -> Result<(Self, usize)> {
        let count = *data.first().ok_or_else(|| Error::malformed("implode tree has no length byte"))? as usize + 1;
        let mut lengths = Vec::with_capacity(symbols);

        for index in 0..count {
            let byte = *data.get(1 + index).ok_or_else(|| Error::malformed("implode tree runs past the end of the entry"))?;
            let run = (byte >> 4) as usize + 1;
            let length = (byte & 0x0f) + 1;
            for _ in 0..run {
                if lengths.len() == symbols {
                    return Err(Error::malformed("implode tree describes more symbols than the alphabet holds"));
                }
                lengths.push(length);
            }
        }

        if lengths.len() != symbols {
            return Err(Error::malformed(format!("implode tree describes {} of {symbols} symbols", lengths.len())));
        }

        Ok((Tree::from_lengths(lengths)?, 1 + count))
    }

    fn from_lengths(lengths: Vec<u8>) -> Result<Self> {
        let max = lengths.iter().copied().max().unwrap_or(0) as u32;
        if max == 0 || max > MAX_CODE_BITS {
            return Err(Error::malformed("implode tree has an unusable code length"));
        }

        let mut order: Vec<usize> = (0..lengths.len()).collect();
        order.sort_by_key(|&symbol| (lengths[symbol], symbol));

        let mut codes = vec![0u16; lengths.len()];
        let mut code = 0u32;
        let mut increment = 0u32;
        let mut previous_length = 0u8;

        for &symbol in order.iter().rev() {
            code += increment;
            if lengths[symbol] != previous_length {
                previous_length = lengths[symbol];
                increment = 1 << (16 - previous_length as u32);
            }
            let length = lengths[symbol] as u32;
            codes[symbol] = reverse_bits(code >> (16 - length), length) as u16;
        }

        Ok(Tree { lengths, codes })
    }

    fn decode(&self, bits: &mut BitReader<'_>) -> Option<usize> {
        let mut value = 0u16;
        for length in 1..=MAX_CODE_BITS {
            value |= (bits.bit()? as u16) << (length - 1);
            for (symbol, &l) in self.lengths.iter().enumerate() {
                if l as u32 == length && self.codes[symbol] == value {
                    return Some(symbol);
                }
            }
        }
        None
    }
}

pub fn decompress(data: &[u8], flags: u16, size_hint: usize) -> Result<Vec<u8>> {
    let large = flags & FLAG_LARGE_DICTIONARY != 0;
    let literal_tree = flags & FLAG_LITERAL_TREE != 0;

    let distance_low_bits = if large { 7 } else { 6 };
    let min_length = if literal_tree { 3 } else { 2 };

    let mut offset = 0usize;
    let literals = if literal_tree {
        let (tree, used) = Tree::parse(&data[offset..], 256)?;
        offset += used;
        Some(tree)
    } else {
        None
    };

    let (lengths, used) = Tree::parse(&data[offset..], 64)?;
    offset += used;
    let (distances, used) = Tree::parse(&data[offset..], 64)?;
    offset += used;

    let mut bits = BitReader::new(&data[offset..]);
    let mut out = Vec::with_capacity(size_hint);

    while let Some(is_literal) = bits.bit() {
        if is_literal == 1 {
            let byte = match &literals {
                Some(tree) => match tree.decode(&mut bits) {
                    Some(symbol) => symbol as u8,
                    None => break,
                },
                None => match bits.bits(8) {
                    Some(value) => value as u8,
                    None => break,
                },
            };
            out.push(byte);
            continue;
        }

        let Some(low) = bits.bits(distance_low_bits) else { break };
        let Some(high) = distances.decode(&mut bits) else { break };
        let distance = ((high << distance_low_bits) | low as usize) + 1;

        let Some(mut length) = lengths.decode(&mut bits) else { break };
        if length == 63 {
            let Some(extra) = bits.bits(8) else { break };
            length += extra as usize;
        }
        length += min_length;

        if distance > out.len() {
            return Err(Error::malformed(format!("implode match reaches {distance} bytes back, past the {} produced", out.len())));
        }

        let mut source = out.len() - distance;
        for _ in 0..length {
            let byte = out[source];
            out.push(byte);
            source += 1;
        }
    }

    Ok(out)
}

fn reverse_bits(value: u32, count: u32) -> u32 {
    value.reverse_bits() >> (32 - count)
}
