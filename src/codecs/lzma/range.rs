use std::io::Read;

use crate::utils::error::{Error, Result};

pub const PROB_BITS: u32 = 11;
pub const PROB_INIT: u16 = (1 << PROB_BITS) / 2;
pub const MOVE_BITS: u32 = 5;
pub const TOP: u32 = 1 << 24;
const IN_BUF: usize = 64 * 1024;
pub const HEADER_LEN: usize = 5;

pub type Prob = u16;

pub fn probs(len: usize) -> Vec<Prob> {
    vec![PROB_INIT; len]
}

pub struct RangeDecoder<R> {
    inner: R,
    data: Box<[u8]>,
    pos: usize,
    filled: usize,
    total: usize,
    range: u32,
    code: u32,
    exhausted: bool,
}

impl<R: Read> RangeDecoder<R> {
    pub fn new(mut inner: R) -> Result<Self> {
        let mut header = [0u8; 5];
        inner
            .read_exact(&mut header)
            .map_err(|e| if e.kind() == std::io::ErrorKind::UnexpectedEof { Error::malformed("lzma stream is too short to start") } else { Error::Io(e) })?;

        Ok(RangeDecoder {
            inner,
            data: vec![0u8; IN_BUF].into_boxed_slice(),
            pos: 0,
            filled: 0,
            total: 0,
            range: u32::MAX,
            code: u32::from_be_bytes([header[1], header[2], header[3], header[4]]),
            exhausted: false,
        })
    }

    pub fn is_finished(&self) -> bool {
        self.code == 0
    }

    /// Bytes pulled from the inner reader that the decoder has actually used.
    pub fn consumed(&self) -> usize {
        HEADER_LEN + self.total - (self.filled - self.pos)
    }

    pub fn into_inner(self) -> R {
        self.inner
    }

    /// Take the reader back along with the bytes read ahead of what was used.
    ///
    /// A container with a trailer after the stream has to put these back before
    /// it can read it.
    pub fn into_parts(self) -> (R, Vec<u8>) {
        let leftover = self.data[self.pos..self.filled].to_vec();
        (self.inner, leftover)
    }

    #[inline]
    fn byte(&mut self) -> u8 {
        if self.pos < self.filled {
            let b = self.data[self.pos];
            self.pos += 1;
            return b;
        }
        self.refill()
    }

    #[cold]
    fn refill(&mut self) -> u8 {
        if self.exhausted {
            return 0;
        }
        loop {
            match self.inner.read(&mut self.data) {
                Ok(0) => {
                    self.exhausted = true;
                    return 0;
                }
                Ok(n) => {
                    self.filled = n;
                    self.total += n;
                    self.pos = 1;
                    return self.data[0];
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => {
                    self.exhausted = true;
                    return 0;
                }
            }
        }
    }

    #[inline]
    fn normalize(&mut self) {
        if self.range < TOP {
            self.range <<= 8;
            self.code = (self.code << 8) | self.byte() as u32;
        }
    }

    #[inline]
    pub fn bit(&mut self, prob: &mut Prob) -> u32 {
        let value = *prob as u32;
        let bound = (self.range >> PROB_BITS) * value;

        let symbol = if self.code < bound {
            self.range = bound;
            *prob = (value + (((1 << PROB_BITS) - value) >> MOVE_BITS)) as Prob;
            0
        } else {
            self.range -= bound;
            self.code -= bound;
            *prob = (value - (value >> MOVE_BITS)) as Prob;
            1
        };

        self.normalize();
        symbol
    }

    pub fn direct_bits(&mut self, count: u32) -> u32 {
        let mut result = 0u32;
        for _ in 0..count {
            self.range >>= 1;
            self.code = self.code.wrapping_sub(self.range);
            let mask = 0u32.wrapping_sub(self.code >> 31);
            self.code = self.code.wrapping_add(self.range & mask);
            self.normalize();
            result = (result << 1).wrapping_add(mask.wrapping_add(1));
        }
        result
    }

    pub fn tree(&mut self, probs: &mut [Prob], bits: u32) -> u32 {
        let mut node = 1u32;
        for _ in 0..bits {
            node = (node << 1) + self.bit(&mut probs[node as usize]);
        }
        node - (1 << bits)
    }

    pub fn tree_reverse(&mut self, probs: &mut [Prob], bits: u32) -> u32 {
        let mut node = 1u32;
        let mut symbol = 0u32;
        for i in 0..bits {
            let bit = self.bit(&mut probs[node as usize]);
            node = (node << 1) + bit;
            symbol |= bit << i;
        }
        symbol
    }
}
