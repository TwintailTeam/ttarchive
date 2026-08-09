use crate::codecs::zstd::bits::{BackwardBits, BitWriter};
use crate::utils::error::{Error, Result};

pub const MIN_TABLE_LOG: u32 = 5;
pub const MAX_TABLE_LOG: u32 = 9;
pub const MAX_WEIGHT_TABLE_LOG: u32 = 6;

#[derive(Debug, Clone, Copy, Default)]
pub struct Entry {
    pub symbol: u8,
    pub bits: u8,
    pub base: u16,
}

#[derive(Debug, Clone, Default)]
pub struct Table {
    pub log: u32,
    pub entries: Vec<Entry>,
}

impl Table {
    pub fn size(&self) -> usize {
        self.entries.len()
    }

    pub fn single(symbol: u8) -> Self {
        Table { log: 0, entries: vec![Entry { symbol, bits: 0, base: 0 }] }
    }

    pub fn from_counts(counts: &[i32], log: u32) -> Result<Self> {
        if !(MIN_TABLE_LOG..=MAX_TABLE_LOG).contains(&log) {
            return Err(Error::malformed(format!("zstd FSE table log {log} is outside the {MIN_TABLE_LOG} to {MAX_TABLE_LOG} the format allows")));
        }

        let size = 1usize << log;
        let mut entries = vec![Entry::default(); size];

        let mut high = size;
        for (symbol, &count) in counts.iter().enumerate() {
            if count == -1 {
                high -= 1;
                entries[high].symbol = symbol as u8;
            }
        }

        let step = (size >> 1) + (size >> 3) + 3;
        let mask = size - 1;
        let mut position = 0usize;

        for (symbol, &count) in counts.iter().enumerate() {
            if count <= 0 {
                continue;
            }
            for _ in 0..count {
                entries[position].symbol = symbol as u8;
                position = (position + step) & mask;
                while position >= high {
                    position = (position + step) & mask;
                }
            }
        }

        if position != 0 {
            return Err(Error::malformed("zstd FSE distribution does not fill its table"));
        }

        let mut next: Vec<u32> = counts.iter().map(|&c| if c == -1 { 1 } else { c.max(0) as u32 }).collect();
        for entry in entries.iter_mut() {
            let symbol = entry.symbol as usize;
            let slot = next[symbol];
            next[symbol] += 1;
            let bits = log - (u32::BITS - 1 - slot.leading_zeros());
            entry.bits = bits as u8;
            entry.base = ((slot << bits) - size as u32) as u16;
        }

        Ok(Table { log, entries })
    }

    pub fn parse(data: &[u8], max_log: u32, max_symbol: usize) -> Result<(Self, usize)> {
        let mut reader = ForwardBits::new(data);

        let log = reader.bits(4)? + 5;
        if log > max_log {
            return Err(Error::malformed(format!("zstd FSE table log {log} exceeds the {max_log} allowed here")));
        }

        let size = 1i32 << log;
        let mut remaining = size + 1;
        let mut counts = Vec::with_capacity(max_symbol + 1);
        let mut threshold = size;
        let mut bits_needed = log + 1;
        let mut previous_was_zero = false;

        while remaining > 1 && counts.len() <= max_symbol {
            if previous_was_zero {
                let mut zeros = 0usize;
                loop {
                    let pair = reader.bits(2)?;
                    zeros += pair as usize;
                    if pair != 3 {
                        break;
                    }
                }
                for _ in 0..zeros {
                    if counts.len() > max_symbol {
                        break;
                    }
                    counts.push(0);
                }
                previous_was_zero = false;
                continue;
            }

            let max = (2 * threshold - 1 - remaining) as u32;
            let low = reader.peek(bits_needed - 1)? as i32;
            let value = if (low as u32) < max {
                reader.skip(bits_needed - 1);
                low
            } else {
                let wide = reader.bits(bits_needed)? as i32;
                if wide >= threshold { wide - max as i32 } else { wide }
            };

            let count = value - 1;
            remaining -= count.abs();
            counts.push(count);
            previous_was_zero = count == 0;

            while remaining < threshold {
                bits_needed -= 1;
                threshold >>= 1;
            }
        }

        if remaining != 1 {
            return Err(Error::malformed("zstd FSE table description does not account for every state"));
        }

        Ok((Table::from_counts(&counts, log)?, reader.consumed()))
    }
}

#[derive(Debug, Clone, Copy)]
pub struct State {
    value: usize,
}

impl State {
    pub fn new(table: &Table, bits: &mut BackwardBits<'_>) -> Self {
        State { value: bits.bits(table.log) as usize }
    }

    pub fn symbol(&self, table: &Table) -> u8 {
        table.entries[self.value].symbol
    }

    pub fn advance(&mut self, table: &Table, bits: &mut BackwardBits<'_>) {
        let entry = table.entries[self.value];
        let extra = bits.bits(entry.bits as u32) as usize;
        self.value = entry.base as usize + extra;
    }
}

/// A decode table read the other way round.
///
/// FSE encodes a symbol run backwards: the state that follows a symbol is
/// already fixed, and the encoder picks the state that leads to it. Every
/// decode entry covers the successors `[base, base + 2^bits)`, and for one
/// symbol those ranges tile the whole table, so exactly one state always fits.
pub struct EncTable {
    table: Table,
    size: usize,
    leads: Vec<u16>,
}

const NO_STATE: u16 = u16::MAX;

impl EncTable {
    pub fn new(table: Table) -> Result<Self> {
        let size = table.size();
        let symbols = table.entries.iter().map(|e| e.symbol as usize).max().unwrap_or(0) + 1;

        let mut leads = vec![NO_STATE; symbols * size];
        for (state, entry) in table.entries.iter().enumerate() {
            let base = entry.base as usize;
            let span = 1usize << entry.bits;
            if base + span > size {
                return Err(Error::malformed("zstd FSE state covers successors outside the table"));
            }
            for successor in base..base + span {
                leads[entry.symbol as usize * size + successor] = state as u16;
            }
        }

        Ok(EncTable { table, size, leads })
    }

    pub fn log(&self) -> u32 {
        self.table.log
    }

    fn lead(&self, symbol: u8, successor: usize) -> Result<usize> {
        let slot = (symbol as usize).checked_mul(self.size).and_then(|base| self.leads.get(base + successor));
        match slot {
            Some(&state) if state != NO_STATE => Ok(state as usize),
            _ => Err(Error::malformed(format!("zstd FSE table cannot encode symbol {symbol}"))),
        }
    }

    /// The state to start from, which is the one the last symbol of the run
    /// decodes from. Any state carrying the symbol works, since the decoder
    /// only reads it back as a plain table index.
    pub fn start(&self, symbol: u8) -> Result<usize> {
        self.table.entries.iter().position(|e| e.symbol == symbol).ok_or_else(|| Error::malformed(format!("zstd FSE table cannot encode symbol {symbol}")))
    }

    /// Emit `symbol`, stepping the state back towards the start of the run.
    pub fn encode(&self, state: &mut usize, symbol: u8, out: &mut BitWriter) -> Result<()> {
        let lead = self.lead(symbol, *state)?;
        let entry = self.table.entries[lead];
        out.add((*state - entry.base as usize) as u64, entry.bits as u32);
        *state = lead;
        Ok(())
    }

    /// Write the state the decoder starts from.
    pub fn flush(&self, state: usize, out: &mut BitWriter) {
        out.add(state as u64, self.table.log);
    }
}

pub struct ForwardBits<'a> {
    data: &'a [u8],
    bit: usize,
}

impl<'a> ForwardBits<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        ForwardBits { data, bit: 0 }
    }

    pub fn consumed(&self) -> usize {
        self.bit.div_ceil(8)
    }

    pub fn peek(&self, count: u32) -> Result<u32> {
        let mut value = 0u32;
        for i in 0..count {
            let index = self.bit + i as usize;
            let byte = *self.data.get(index / 8).unwrap_or(&0);
            value |= (((byte >> (index % 8)) & 1) as u32) << i;
        }
        if (self.bit + count as usize).div_ceil(8) > self.data.len() + 1 {
            return Err(Error::malformed("zstd FSE table description runs past its section"));
        }
        Ok(value)
    }

    pub fn skip(&mut self, count: u32) {
        self.bit += count as usize;
    }

    pub fn bits(&mut self, count: u32) -> Result<u32> {
        let value = self.peek(count)?;
        self.skip(count);
        Ok(value)
    }
}
