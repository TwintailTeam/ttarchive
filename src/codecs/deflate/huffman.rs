use crate::utils::error::{Error, Result};

pub const FAST_BITS: u32 = 10;

pub const MAX_BITS: usize = 15;

type Entry = u16;

const SYMBOL_MASK: u16 = 0x0FFF;

#[inline]
const fn entry(symbol: u16, length: u8) -> Entry {
    ((length as u16) << 12) | (symbol & SYMBOL_MASK)
}

#[inline]
const fn entry_len(e: Entry) -> u32 {
    (e >> 12) as u32
}

#[inline]
const fn entry_symbol(e: Entry) -> u16 {
    e & SYMBOL_MASK
}

#[derive(Debug, Clone)]
pub struct Decoder {
    fast: Box<[Entry]>,
    counts: [u16; MAX_BITS + 1],
    symbols: Box<[u16]>,
}

impl Decoder {
    pub fn new(lengths: &[u8]) -> Result<Self> {
        let mut counts = [0u16; MAX_BITS + 1];
        for &l in lengths {
            if l as usize > MAX_BITS {
                return Err(Error::malformed(format!("huffman code length {l} exceeds 15")));
            }
            counts[l as usize] += 1;
        }
        counts[0] = 0;

        let mut left = 1i32;
        for &count in &counts[1..=MAX_BITS] {
            left <<= 1;
            left -= count as i32;
            if left < 0 {
                return Err(Error::malformed("over-subscribed huffman code table"));
            }
        }

        let total_codes: u32 = counts[1..=MAX_BITS].iter().map(|&c| c as u32).sum();
        if left > 0 && total_codes != 1 {
            return Err(Error::malformed("incomplete huffman code table"));
        }

        let mut offsets = [0u16; MAX_BITS + 2];
        for (len, &count) in counts.iter().enumerate().take(MAX_BITS + 1).skip(1) {
            offsets[len + 1] = offsets[len] + count;
        }

        let total = offsets[MAX_BITS + 1] as usize;
        let mut symbols = vec![0u16; total].into_boxed_slice();
        let mut next = offsets;
        for (sym, &l) in lengths.iter().enumerate() {
            if l != 0 {
                symbols[next[l as usize] as usize] = sym as u16;
                next[l as usize] += 1;
            }
        }

        let mut fast = vec![0 as Entry; 1 << FAST_BITS].into_boxed_slice();
        let mut code = 0u32;
        let mut index = 0usize;
        for (len, &at_len) in counts.iter().enumerate().take(MAX_BITS + 1).skip(1) {
            let count = at_len as usize;
            if len as u32 <= FAST_BITS {
                for i in 0..count {
                    let sym = symbols[index + i];
                    let reversed = reverse_bits(code + i as u32, len as u32);
                    let e = entry(sym, len as u8);
                    let stride = 1usize << len;
                    let mut slot = reversed as usize;
                    while slot < fast.len() {
                        fast[slot] = e;
                        slot += stride;
                    }
                }
            }
            index += count;
            code = (code + count as u32) << 1;
        }

        Ok(Decoder { fast, counts, symbols })
    }

    #[inline]
    pub fn decode(&self, bits: u64) -> Result<(u16, u32)> {
        let e = self.fast[(bits & ((1 << FAST_BITS) - 1)) as usize];
        if e != 0 {
            return Ok((entry_symbol(e), entry_len(e)));
        }
        self.decode_slow(bits)
    }

    #[cold]
    fn decode_slow(&self, bits: u64) -> Result<(u16, u32)> {
        let mut code = 0i32;
        let mut first = 0i32;
        let mut index = 0i32;

        for len in 1..=MAX_BITS {
            code |= ((bits >> (len - 1)) & 1) as i32;
            let count = self.counts[len] as i32;
            if code - first < count {
                let sym = self.symbols[(index + (code - first)) as usize];
                return Ok((sym, len as u32));
            }
            index += count;
            first = (first + count) << 1;
            code <<= 1;
        }

        Err(Error::malformed("invalid huffman code"))
    }
}

#[inline]
fn reverse_bits(v: u32, n: u32) -> u32 {
    v.reverse_bits() >> (32 - n)
}

pub fn fixed_literal_lengths() -> [u8; 288] {
    let mut l = [0u8; 288];
    l[0..144].fill(8);
    l[144..256].fill(9);
    l[256..280].fill(7);
    l[280..288].fill(8);
    l
}

pub fn fixed_distance_lengths() -> [u8; 32] {
    [5u8; 32]
}
