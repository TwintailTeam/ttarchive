use crate::utils::error::{Error, Result};

pub struct BitReader<'a> {
    data: &'a [u8],
    pos: usize,
    accumulator: u32,
    held: u32,
}

impl<'a> BitReader<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        BitReader { data, pos: 0, accumulator: 0, held: 0 }
    }

    pub fn is_empty(&self) -> bool {
        self.held == 0 && self.pos >= self.data.len()
    }

    pub fn bits(&mut self, count: u32) -> Option<u32> {
        while self.held < count {
            let byte = *self.data.get(self.pos)?;
            self.pos += 1;
            self.accumulator |= (byte as u32) << self.held;
            self.held += 8;
        }
        let value = self.accumulator & ((1u32 << count) - 1);
        self.accumulator >>= count;
        self.held -= count;
        Some(value)
    }

    pub fn bit(&mut self) -> Option<u32> {
        self.bits(1)
    }

    pub fn need(&mut self, count: u32, what: &str) -> Result<u32> {
        self.bits(count).ok_or_else(|| Error::malformed(format!("stream ends in the middle of {what}")))
    }
}
