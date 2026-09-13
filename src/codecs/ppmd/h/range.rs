use std::io::Read;

use crate::utils::error::{Error, Result};

const TOP: u32 = 1 << 24;

pub const BIN_SHIFT: u32 = 14;

pub struct RangeDecoder<R> {
    inner: R,
    range: u32,
    code: u32,
    exhausted: bool,
}

impl<R: Read> RangeDecoder<R> {
    pub fn new(mut inner: R) -> Result<Self> {
        let mut header = [0u8; 5];
        inner.read_exact(&mut header).map_err(|_| Error::malformed("ppmd stream is too short to start"))?;

        if header[0] != 0 {
            return Err(Error::malformed("ppmd var. H stream does not open with its zero byte"));
        }

        Ok(RangeDecoder { inner, range: u32::MAX, code: u32::from_be_bytes([header[1], header[2], header[3], header[4]]), exhausted: false })
    }

    pub fn is_finished(&self) -> bool {
        self.code == 0
    }

    pub fn ran_dry(&self) -> bool {
        self.exhausted
    }

    fn byte(&mut self) -> u8 {
        if self.exhausted {
            return 0;
        }
        let mut byte = [0u8; 1];
        match self.inner.read(&mut byte) {
            Ok(1) => byte[0],
            _ => {
                self.exhausted = true;
                0
            }
        }
    }

    fn normalize(&mut self) {
        if self.range < TOP {
            self.code = (self.code << 8) | self.byte() as u32;
            self.range <<= 8;
            if self.range < TOP {
                self.code = (self.code << 8) | self.byte() as u32;
                self.range <<= 8;
            }
        }
    }

    pub fn threshold(&mut self, total: u32) -> u32 {
        if total == 0 {
            return 0;
        }
        self.range /= total;
        self.code / self.range
    }

    pub fn decode(&mut self, start: u32, size: u32) {
        self.code = self.code.wrapping_sub(start.wrapping_mul(self.range));
        self.range = self.range.wrapping_mul(size);
        self.normalize();
    }

    pub fn decode_bit(&mut self, size0: u32) -> u32 {
        let bound = (self.range >> BIN_SHIFT).wrapping_mul(size0);
        let bit = if self.code < bound {
            self.range = bound;
            0
        } else {
            self.code -= bound;
            self.range -= bound;
            1
        };
        self.normalize();
        bit
    }
}
