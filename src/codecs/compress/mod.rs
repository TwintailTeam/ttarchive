use std::io::Read;

use crate::utils::error::{Error, Result};

pub const MAGIC: [u8; 2] = [0x1f, 0x9d];

const INIT_BITS: u32 = 9;
const MAX_MAX_BITS: u32 = 16;
const CLEAR: u16 = 256;
const FIRST_FREE: u16 = 257;

const BLOCK_MODE: u8 = 0x80;
const MAX_BITS_MASK: u8 = 0x1f;

pub fn is_compress(prefix: &[u8]) -> bool {
    prefix.len() >= 2 && prefix[..2] == MAGIC
}

struct Lzw {
    prefix: Vec<u16>,
    suffix: Vec<u8>,
    stack: Vec<u8>,
    next: usize,
    width: u32,
    max_bits: u32,
    limit: usize,
    block_mode: bool,
    previous: Option<u16>,
}

impl Lzw {
    fn new(flags: u8) -> Result<Self> {
        let max_bits = (flags & MAX_BITS_MASK) as u32;
        let block_mode = flags & BLOCK_MODE != 0;

        if !(INIT_BITS..=MAX_MAX_BITS).contains(&max_bits) {
            return Err(Error::malformed(format!("compress stream declares {max_bits} maximum bits, outside 9..=16")));
        }

        let limit = 1usize << max_bits;
        let mut suffix = vec![0u8; limit];
        for (code, slot) in suffix.iter_mut().enumerate().take(256) {
            *slot = code as u8;
        }

        Ok(Lzw {
            prefix: vec![0u16; limit],
            suffix,
            stack: Vec::with_capacity(limit),
            next: if block_mode { FIRST_FREE as usize } else { CLEAR as usize },
            width: INIT_BITS,
            max_bits,
            limit,
            block_mode,
            previous: None,
        })
    }

    fn step(&mut self, code: u16, out: &mut Vec<u8>) -> Result<()> {
        if code as usize > self.next || (code as usize == self.next && self.previous.is_none()) {
            return Err(Error::malformed(format!("compress stream names code {code}, past the {} defined", self.next)));
        }

        self.stack.clear();
        let mut current = code;

        if code as usize == self.next {
            let first = self.previous.ok_or_else(|| Error::malformed("compress stream opens with a deferred code"))?;
            self.stack.push(first_byte(first, &self.prefix, &self.suffix));
            current = first;
        }

        while current >= FIRST_FREE {
            self.stack.push(self.suffix[current as usize]);
            current = self.prefix[current as usize];
            if self.stack.len() > self.limit {
                return Err(Error::malformed("compress dictionary contains a cycle"));
            }
        }
        self.stack.push(self.suffix[current as usize]);

        out.extend(self.stack.iter().rev());

        if let Some(earlier) = self.previous
            && self.next < self.limit
        {
            self.prefix[self.next] = earlier;
            self.suffix[self.next] = *self.stack.last().expect("stack holds the first byte");
            self.next += 1;

            if self.next >= (1usize << self.width) && self.width < self.max_bits {
                self.width += 1;
            }
        }

        self.previous = Some(code);
        Ok(())
    }

    fn clear(&mut self) {
        self.width = INIT_BITS;
        self.next = FIRST_FREE as usize;
        self.previous = None;
    }
}

const PRODUCE: usize = 64 * 1024;

/// LZW as `compress(1)` writes it, decoded as it is read.
///
/// Distinct from the ZIP shrink codec: codes are packed least significant bit
/// first, the width grows on a fixed schedule rather than by an explicit signal,
/// and after a clear the encoder pads to a whole group of codes. Only the
/// dictionary is held, so the stream decodes in bounded memory.
pub struct Reader<R> {
    inner: R,
    state: Option<Lzw>,
    input: Vec<u8>,
    bit_pos: usize,
    dropped_bits: usize,
    out: Vec<u8>,
    read: usize,
    finished: bool,
}

impl<R: Read> Reader<R> {
    /// Wrap `inner` at the start of a `compress(1)` stream.
    pub fn new(inner: R) -> Self {
        Reader { inner, state: None, input: Vec::new(), bit_pos: 0, dropped_bits: 0, out: Vec::new(), read: 0, finished: false }
    }

    fn have(&mut self, bits: usize) -> Result<bool> {
        loop {
            if (self.input.len() * 8).saturating_sub(self.bit_pos) >= bits {
                return Ok(true);
            }
            let mut chunk = [0u8; 16 * 1024];
            match self.inner.read(&mut chunk) {
                Ok(0) => return Ok(false),
                Ok(n) => self.input.extend_from_slice(&chunk[..n]),
                Err(ref e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(e) => return Err(Error::from(e)),
            }
        }
    }

    fn compact(&mut self) {
        let whole = self.bit_pos / 8;
        if whole >= 8 * 1024 {
            self.input.drain(..whole);
            self.bit_pos -= whole * 8;
            self.dropped_bits += whole * 8;
        }
        if self.read >= PRODUCE {
            self.out.drain(..self.read);
            self.read = 0;
        }
    }

    fn header(&mut self) -> Result<()> {
        if !self.have(24)? {
            return Err(Error::malformed("compress stream is too short to hold a header"));
        }
        if self.input[..2] != MAGIC {
            return Err(Error::malformed("compress stream does not start with 1f 9d"));
        }
        self.state = Some(Lzw::new(self.input[2])?);
        self.input.drain(..3);
        self.bit_pos = 0;
        Ok(())
    }

    fn produce(&mut self) -> Result<()> {
        if self.state.is_none() {
            return self.header();
        }

        while self.out.len() - self.read < PRODUCE {
            let width = self.state.as_ref().expect("the header was read").width as usize;
            if !self.have(width)? {
                self.finished = true;
                return Ok(());
            }

            let code = take_code(&self.input, self.bit_pos, width as u32).expect("the bits were just checked");
            self.bit_pos += width;

            let state = self.state.as_mut().expect("the header was read");
            if state.block_mode && code == CLEAR {
                let absolute = self.dropped_bits + self.bit_pos;
                self.bit_pos = skip_to_group(absolute, width as u32) - self.dropped_bits;
                state.clear();
                continue;
            }

            let mut out = std::mem::take(&mut self.out);
            let result = state.step(code, &mut out);
            self.out = out;
            result?;
        }

        self.compact();
        Ok(())
    }
}

impl<R: Read> Read for Reader<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        while self.out.len() == self.read && !self.finished {
            self.produce()?;
        }
        let n = (self.out.len() - self.read).min(buf.len());
        buf[..n].copy_from_slice(&self.out[self.read..self.read + n]);
        self.read += n;
        Ok(n)
    }
}

/// Decode a whole `compress(1)` stream.
pub fn decompress(data: &[u8], size_hint: usize) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(size_hint.min(256 << 20));
    Reader::new(data).read_to_end(&mut out)?;
    Ok(out)
}

fn first_byte(mut code: u16, prefix: &[u16], suffix: &[u8]) -> u8 {
    while code >= FIRST_FREE {
        code = prefix[code as usize];
    }
    suffix[code as usize]
}

fn take_code(data: &[u8], bit_pos: usize, width: u32) -> Option<u16> {
    let end = bit_pos + width as usize;
    if end > data.len() * 8 {
        return None;
    }

    let mut value = 0u32;
    for offset in 0..width as usize {
        let at = bit_pos + offset;
        let bit = (data[at / 8] >> (at % 8)) & 1;
        value |= (bit as u32) << offset;
    }
    Some(value as u16)
}

fn skip_to_group(bit_pos: usize, width: u32) -> usize {
    let group = width as usize * 8;
    let into = bit_pos % group;
    if into == 0 { bit_pos } else { bit_pos + (group - into) }
}
