use std::io::Read;

use crate::utils::error::{Error, Result};

const TOP: u32 = 1 << 24;
const BOT: u32 = 1 << 15;

pub const BIN_SCALE: u32 = 1 << 14;

pub struct RangeDecoder<R> {
    inner: R,
    range: u32,
    code: u32,
    low: u32,
    exhausted: bool,
}

impl<R: Read> RangeDecoder<R> {
    pub fn new(mut inner: R) -> Result<Self> {
        let mut header = [0u8; 4];
        inner.read_exact(&mut header).map_err(|_| Error::malformed("ppmd stream is too short to start"))?;

        let code = u32::from_be_bytes(header);
        if code == u32::MAX {
            return Err(Error::malformed("ppmd stream begins with an impossible code value"));
        }

        Ok(RangeDecoder { inner, range: u32::MAX, code, low: 0, exhausted: false })
    }

    pub fn is_finished(&self) -> bool {
        self.code == 0
    }

    #[inline]
    pub fn range(&self) -> u32 {
        self.range
    }
    #[inline]
    pub fn code(&self) -> u32 {
        self.code
    }
    #[inline]
    pub fn set_range(&mut self, range: u32) {
        self.range = range;
    }

    fn byte(&mut self) -> u8 {
        if self.exhausted {
            return 0;
        }
        let mut b = [0u8; 1];
        match self.inner.read(&mut b) {
            Ok(1) => b[0],
            _ => {
                self.exhausted = true;
                0
            }
        }
    }

    pub fn normalize(&mut self) {
        loop {
            if (self.low ^ self.low.wrapping_add(self.range)) >= TOP {
                if self.range >= BOT {
                    break;
                }
                self.range = self.low.wrapping_neg() & (BOT - 1);
            }
            self.code = (self.code << 8) | self.byte() as u32;
            self.range <<= 8;
            self.low <<= 8;
        }
    }

    #[inline]
    pub fn normalize_remote(&mut self) {
        self.normalize();
    }

    #[inline]
    pub fn threshold(&mut self, total: u32) -> u32 {
        self.range /= total;
        self.code / self.range
    }

    #[inline]
    pub fn decode(&mut self, start: u32, size: u32) {
        let start = start.wrapping_mul(self.range);
        self.low = self.low.wrapping_add(start);
        self.code = self.code.wrapping_sub(start);
        self.range = self.range.wrapping_mul(size);
    }

    #[inline]
    pub fn decode_bit1(&mut self, size0: u32) {
        self.low = self.low.wrapping_add(size0);
        self.code = self.code.wrapping_sub(size0);
        self.range = (self.range & !(BIN_SCALE - 1)).wrapping_sub(size0);
    }

    #[inline]
    pub fn correct_sum_range(&self, sum: &mut u32) {
        if *sum > self.range {
            *sum = self.range;
        }
    }
}
