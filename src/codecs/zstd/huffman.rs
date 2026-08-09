use crate::codecs::zstd::bits::BackwardBits;
use crate::codecs::zstd::fse::{self, State};
use crate::utils::error::{Error, Result};

const MAX_BITS: u32 = 11;
const SYMBOLS: usize = 256;

pub struct Table {
    symbols: Vec<u8>,
    lengths: Vec<u8>,
    max_bits: u32,
}

impl Table {
    fn from_weights(weights: &[u8]) -> Result<Self> {
        let total: u32 = weights.iter().map(|&w| if w > 0 { 1u32 << (w - 1) } else { 0 }).sum();
        if total == 0 {
            return Err(Error::malformed("zstd huffman table has no symbols"));
        }

        let max_bits = u32::BITS - total.leading_zeros();
        let size = 1u32 << max_bits;
        let gap = size - total;
        if !gap.is_power_of_two() {
            return Err(Error::malformed("zstd huffman weights leave a gap that is not one symbol's worth"));
        }

        let mut weights = weights.to_vec();
        weights.push((u32::BITS - gap.leading_zeros()) as u8);
        Self::from_complete(&weights)
    }

    fn from_complete(weights: &[u8]) -> Result<Self> {
        let total: u32 = weights.iter().map(|&w| if w > 0 { 1u32 << (w - 1) } else { 0 }).sum();
        if total == 0 || !total.is_power_of_two() {
            return Err(Error::malformed("zstd huffman weights do not add up to a whole table"));
        }

        let max_bits = total.trailing_zeros();
        let size = total;
        let weights = weights.to_vec();

        if max_bits > MAX_BITS {
            return Err(Error::malformed(format!("zstd huffman code of {max_bits} bits exceeds the 11 allowed")));
        }

        let mut symbols = vec![0u8; size as usize];
        let mut lengths = vec![0u8; size as usize];

        let heaviest = weights.iter().copied().max().unwrap_or(0);
        let mut position = 0usize;
        for weight in 1..=heaviest {
            let length = max_bits + 1 - weight as u32;
            let run = 1usize << (weight - 1);
            for (symbol, &w) in weights.iter().enumerate() {
                if w != weight {
                    continue;
                }
                if position + run > symbols.len() {
                    return Err(Error::malformed("zstd huffman weights overflow the table"));
                }
                for slot in position..position + run {
                    symbols[slot] = symbol as u8;
                    lengths[slot] = length as u8;
                }
                position += run;
            }
        }

        if position != symbols.len() {
            return Err(Error::malformed("zstd huffman weights do not fill the table"));
        }

        Ok(Table { symbols, lengths, max_bits })
    }

    pub fn parse(data: &[u8]) -> Result<(Self, usize)> {
        let header = *data.first().ok_or_else(|| Error::malformed("zstd huffman description is empty"))?;

        if header >= 128 {
            let count = header as usize - 127;
            let bytes = count.div_ceil(2);
            if data.len() < 1 + bytes {
                return Err(Error::malformed("zstd huffman weights run past the section"));
            }
            let mut weights = Vec::with_capacity(count);
            for i in 0..count {
                let byte = data[1 + i / 2];
                weights.push(if i % 2 == 0 { byte >> 4 } else { byte & 0x0f });
            }
            return Ok((Table::from_weights(&weights)?, 1 + bytes));
        }

        let size = header as usize;
        if data.len() < 1 + size {
            return Err(Error::malformed("zstd huffman weight stream runs past the section"));
        }
        let body = &data[1..1 + size];

        let (table, used) = fse::Table::parse(body, fse::MAX_WEIGHT_TABLE_LOG, 255)?;
        let mut bits = BackwardBits::new(&body[used..])?;

        let mut even = State::new(&table, &mut bits);
        let mut odd = State::new(&table, &mut bits);

        let mut weights = Vec::with_capacity(SYMBOLS);
        loop {
            weights.push(even.symbol(&table));
            even.advance(&table, &mut bits);
            if bits.is_exhausted() {
                weights.push(odd.symbol(&table));
                break;
            }

            weights.push(odd.symbol(&table));
            odd.advance(&table, &mut bits);
            if bits.is_exhausted() {
                weights.push(even.symbol(&table));
                break;
            }

            if weights.len() > SYMBOLS {
                return Err(Error::malformed("zstd huffman description describes more than 256 symbols"));
            }
        }

        Ok((Table::from_weights(&weights)?, 1 + size))
    }

    pub fn decode(&self, bits: &mut BackwardBits<'_>) -> u8 {
        let index = bits.peek(self.max_bits) as usize;
        let symbol = self.symbols[index];
        bits.consume(self.lengths[index] as u32);
        symbol
    }
}

/// The most symbols a table can describe with plain four bit weights.
///
/// The description writes one weight per symbol below the last one, and its
/// header can only count to 128 of them. Beyond that the weights themselves
/// have to be FSE coded, which this encoder does not do, so a block whose
/// literals reach that far is stored raw instead.
pub const MAX_DIRECT_SYMBOLS: usize = 129;

impl Table {
    /// Build a table over the symbols `freqs` counts, or `None` when the
    /// literals cannot be described with direct weights.
    pub fn build(freqs: &[u32; SYMBOLS]) -> Option<Vec<u8>> {
        let highest = freqs.iter().rposition(|&f| f > 0)?;
        if highest >= MAX_DIRECT_SYMBOLS {
            return None;
        }

        let used = freqs.iter().filter(|&&f| f > 0).count();
        if used < 2 {
            return None;
        }

        let lengths = crate::codecs::lengths::assign_lengths(&freqs[..=highest], MAX_BITS as usize);
        let longest = *lengths.iter().max()? as u32;

        let weights: Vec<u8> = lengths.iter().map(|&len| if len == 0 { 0 } else { (longest + 1 - len as u32) as u8 }).collect();

        Some(weights)
    }

    /// The code for every symbol, as (value, bit count), from a weight list.
    ///
    /// Codes are canonical: symbols are ordered by weight, heaviest first, and
    /// within a weight by symbol number, which is the order the decoder's table
    /// is filled in.
    pub fn codes(weights: &[u8]) -> Result<Vec<(u16, u8)>> {
        let table = Table::from_complete(weights)?;

        let mut codes = vec![(0u16, 0u8); SYMBOLS];
        let mut next = vec![0u32; (table.max_bits + 2) as usize];

        let mut counts = vec![0u32; (table.max_bits + 2) as usize];
        for slot in 0..table.symbols.len() {
            if slot == 0 || table.symbols[slot] != table.symbols[slot - 1] {
                counts[table.lengths[slot] as usize] += 1;
            }
        }

        let mut code = 0u32;
        for length in (1..=table.max_bits).rev() {
            next[length as usize] = code;
            code = (code + counts[length as usize]) >> 1;
        }

        let mut seen = vec![false; SYMBOLS];
        for slot in 0..table.symbols.len() {
            let symbol = table.symbols[slot] as usize;
            let length = table.lengths[slot];
            if seen[symbol] || length == 0 {
                continue;
            }
            seen[symbol] = true;
            codes[symbol] = (next[length as usize] as u16, length);
            next[length as usize] += 1;
        }

        Ok(codes)
    }

    /// Write a weight list in the plain four bit form.
    pub fn describe(weights: &[u8]) -> Vec<u8> {
        let count = weights.len() - 1;
        let mut out = Vec::with_capacity(2 + count / 2);
        out.push((127 + count) as u8);

        for pair in weights[..count].chunks(2) {
            let high = pair[0] << 4;
            let low = pair.get(1).copied().unwrap_or(0);
            out.push(high | low);
        }

        out
    }
}
