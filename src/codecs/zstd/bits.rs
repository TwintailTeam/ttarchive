use crate::utils::error::{Error, Result};

#[inline(always)]
fn mask(count: u32) -> u64 {
    if count >= 64 { u64::MAX } else { (1u64 << count) - 1 }
}

pub struct BackwardBits<'a> {
    data: &'a [u8],
    container: u64,
    held: u32,
    next: usize,
    overrun: bool,
}

impl<'a> BackwardBits<'a> {
    pub fn new(data: &'a [u8]) -> Result<Self> {
        let last = *data.last().ok_or_else(|| Error::malformed("zstd bitstream is empty"))?;
        if last == 0 {
            return Err(Error::malformed("zstd bitstream ends in a zero byte, so its start cannot be located"));
        }

        let mut reader = BackwardBits { data, container: 0, held: 0, next: data.len(), overrun: false };
        reader.pull();

        let padding = last.leading_zeros() + 1;
        reader.held -= padding;
        Ok(reader)
    }

    #[inline]
    fn pull(&mut self) {
        if self.held <= 56 && self.next >= 8 {
            let whole = ((64 - self.held) / 8) as usize;
            let window = self.data[self.next - 8..self.next].try_into().unwrap_or([0; 8]);
            let group = u64::from_le_bytes(window) >> ((8 - whole) * 8);
            self.container = self.container.checked_shl(8 * whole as u32).unwrap_or(0) | group;
            self.held += 8 * whole as u32;
            self.next -= whole;
            return;
        }

        while self.held <= 56 && self.next > 0 {
            self.next -= 1;
            self.container = (self.container << 8) | self.data[self.next] as u64;
            self.held += 8;
        }
    }

    pub fn is_exhausted(&self) -> bool {
        self.overrun
    }

    #[inline]
    pub fn peek(&mut self, count: u32) -> u64 {
        if self.held >= count {
            return (self.container >> (self.held - count)) & mask(count);
        }
        self.peek_refilled(count)
    }

    #[cold]
    fn peek_refilled(&mut self, count: u32) -> u64 {
        if count == 0 {
            return 0;
        }
        self.pull();

        if self.held >= count {
            (self.container >> (self.held - count)) & mask(count)
        } else {
            let short = count - self.held;
            (self.container & mask(self.held)) << short
        }
    }

    #[inline]
    pub fn consume(&mut self, count: u32) {
        if self.held >= count {
            self.held -= count;
        } else {
            self.held = 0;
            self.overrun = true;
        }
    }

    #[inline]
    pub fn bits(&mut self, count: u32) -> u64 {
        let value = self.peek(count);
        self.consume(count);
        value
    }
}

#[derive(Default)]
pub struct BitWriter {
    out: Vec<u8>,
    container: u64,
    held: u32,
}

impl BitWriter {
    pub fn new() -> Self {
        BitWriter::default()
    }

    pub fn add(&mut self, value: u64, count: u32) {
        if count == 0 {
            return;
        }

        let masked = if count >= 64 { value } else { value & ((1u64 << count) - 1) };
        self.container |= masked << self.held;
        self.held += count;

        while self.held >= 8 {
            self.out.push(self.container as u8);
            self.container >>= 8;
            self.held -= 8;
        }
    }

    pub fn finish(mut self) -> Vec<u8> {
        self.add(1, 1);
        if self.held > 0 {
            self.out.push(self.container as u8);
        }
        self.out
    }
}
