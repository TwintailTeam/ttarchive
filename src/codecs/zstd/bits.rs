use crate::utils::error::{Error, Result};

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

    fn pull(&mut self) {
        while self.held <= 56 && self.next > 0 {
            self.next -= 1;
            self.container = (self.container << 8) | self.data[self.next] as u64;
            self.held += 8;
        }
    }

    pub fn is_exhausted(&self) -> bool {
        self.overrun
    }

    pub fn peek(&mut self, count: u32) -> u64 {
        if count == 0 {
            return 0;
        }
        if self.held < count {
            self.pull();
        }

        if self.held >= count {
            (self.container >> (self.held - count)) & ((1u64 << count) - 1)
        } else {
            let short = count - self.held;
            (self.container & ((1u64 << self.held) - 1)) << short
        }
    }

    pub fn consume(&mut self, count: u32) {
        if self.held >= count {
            self.held -= count;
        } else {
            self.held = 0;
            self.overrun = true;
        }
    }

    pub fn bits(&mut self, count: u32) -> u64 {
        let value = self.peek(count);
        self.consume(count);
        value
    }
}

/// The writer that feeds [`BackwardBits`].
///
/// Bits pile up towards the top of the stream and whole bytes drop out from the
/// bottom, so the reader starting at the last byte sees them in the reverse of
/// the order they were added. A closing one bit marks where the final byte's
/// payload stops, which is how the reader finds its starting point.
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

    /// Add the low `count` bits of `value`. The reader gets them back from a
    /// single `bits(count)` call, but only after everything added later.
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

    /// Close the stream with its marker bit.
    pub fn finish(mut self) -> Vec<u8> {
        self.add(1, 1);
        if self.held > 0 {
            self.out.push(self.container as u8);
        }
        self.out
    }
}
