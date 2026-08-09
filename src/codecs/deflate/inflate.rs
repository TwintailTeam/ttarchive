use std::io::{self, Read};

use crate::codecs::deflate::huffman::{Decoder, MAX_BITS, fixed_distance_lengths, fixed_literal_lengths};
use crate::utils::error::{Error, Result};

const WINDOW: usize = 32_768;
const WINDOW64: usize = 65_536;
const OUT_CHUNK: usize = 65_536;
const IN_CHUNK: usize = 32_768;

pub const LENGTH_BASE: [u16; 29] = [3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131, 163, 195, 227, 258];

pub const LENGTH_EXTRA: [u8; 29] = [0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0];

pub const DIST_BASE: [u16; 30] =
    [1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537, 2049, 3073, 4097, 6145, 8193, 12_289, 16_385, 24_577];

pub const DIST_EXTRA: [u8; 30] = [0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13, 13];

const LENGTH_BASE64: [u16; 29] = [3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131, 163, 195, 227, 3];

const LENGTH_EXTRA64: [u8; 29] = [0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 16];

const DIST_BASE64: [u32; 32] = [
    1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537, 2049, 3073, 4097, 6145, 8193, 12_289, 16_385, 24_577, 32_769,
    49_153,
];

const DIST_EXTRA64: [u8; 32] = [0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13, 13, 14, 14];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Variant {
    #[default]
    Deflate,
    Deflate64,
}

impl Variant {
    const fn window(self) -> usize {
        match self {
            Variant::Deflate => WINDOW,
            Variant::Deflate64 => WINDOW64,
        }
    }

    const fn distance_codes(self) -> usize {
        match self {
            Variant::Deflate => 30,
            Variant::Deflate64 => 32,
        }
    }
}

pub const CODE_LENGTH_ORDER: [usize; 19] = [16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    BlockHeader,
    Stored(u32),
    Compressed,
    Done,
}

pub struct InflateReader<R> {
    inner: R,
    variant: Variant,
    window: usize,

    in_buf: Box<[u8]>,
    in_pos: usize,
    in_end: usize,
    in_total: usize,
    input_eof: bool,
    padding: u32,

    bit_buf: u64,
    bit_count: u32,

    out: Box<[u8]>,
    out_len: usize,
    out_read: usize,

    state: State,
    final_block: bool,
    literal: Option<Decoder>,
    distance: Option<Decoder>,

    pending_copy: Option<(usize, usize)>,
}

impl<R: Read> InflateReader<R> {
    pub fn new(inner: R) -> Self {
        Self::with_variant(inner, Variant::Deflate)
    }

    pub fn deflate64(inner: R) -> Self {
        Self::with_variant(inner, Variant::Deflate64)
    }

    pub fn with_variant(inner: R, variant: Variant) -> Self {
        let window = variant.window();
        InflateReader {
            inner,
            variant,
            window,
            in_buf: vec![0u8; IN_CHUNK].into_boxed_slice(),
            in_pos: 0,
            in_end: 0,
            in_total: 0,
            input_eof: false,
            padding: 0,
            bit_buf: 0,
            bit_count: 0,
            out: vec![0u8; window + OUT_CHUNK].into_boxed_slice(),
            out_len: 0,
            out_read: 0,
            state: State::BlockHeader,
            final_block: false,
            literal: None,
            distance: None,
            pending_copy: None,
        }
    }

    pub fn into_inner(self) -> R {
        self.inner
    }

    /// Take the reader back along with the bytes it read past the stream.
    ///
    /// The bit reader buffers ahead, so a caller that has more to parse after
    /// the deflate data — a gzip trailer, then perhaps another member — needs
    /// those bytes handed back rather than lost. The partial byte the stream
    /// stopped inside is dropped, since deflate data is followed by padding to
    /// the next byte boundary.
    pub fn into_parts(mut self) -> (R, Vec<u8>) {
        let partial = self.bit_count % 8;
        self.bit_buf >>= partial;
        self.bit_count -= partial;

        let mut rest = Vec::with_capacity((self.bit_count / 8) as usize + (self.in_end - self.in_pos));
        while self.bit_count >= 8 {
            rest.push(self.bit_buf as u8);
            self.bit_buf >>= 8;
            self.bit_count -= 8;
        }
        rest.extend_from_slice(&self.in_buf[self.in_pos..self.in_end]);

        (self.inner, rest)
    }

    pub fn is_finished(&self) -> bool {
        self.state == State::Done && self.out_read == self.out_len
    }

    /// Bytes of the compressed input the deflate stream actually used.
    ///
    /// A gzip or zlib wrapper needs this to find its trailer, since the reader
    /// buffers ahead and may hold bytes belonging to whatever follows.
    pub fn consumed(&self) -> usize {
        self.in_total - (self.in_end - self.in_pos) - (self.bit_count / 8) as usize
    }

    fn fill_input(&mut self) -> Result<()> {
        if self.input_eof {
            return Ok(());
        }
        self.in_pos = 0;
        self.in_end = 0;
        loop {
            match self.inner.read(&mut self.in_buf[self.in_end..]) {
                Ok(0) => {
                    self.input_eof = true;
                    break;
                }
                Ok(n) => {
                    self.in_end += n;
                    self.in_total += n;
                    break;
                }
                Err(ref e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(Error::from(e)),
            }
        }
        Ok(())
    }

    #[inline]
    fn need(&mut self, n: u32) -> Result<()> {
        while self.bit_count < n {
            if self.in_pos == self.in_end {
                self.fill_input()?;
                if self.in_pos == self.in_end {
                    self.padding += 8;
                    if self.padding > 64 {
                        return Err(Error::malformed("truncated deflate stream: ran out of input mid-symbol"));
                    }
                    self.bit_count += 8;
                    continue;
                }
            }
            self.bit_buf |= (self.in_buf[self.in_pos] as u64) << self.bit_count;
            self.in_pos += 1;
            self.bit_count += 8;
        }
        Ok(())
    }

    #[inline]
    fn drop_bits(&mut self, n: u32) {
        self.bit_buf >>= n;
        self.bit_count -= n;
    }

    #[inline]
    fn bits(&mut self, n: u32) -> Result<u32> {
        if n == 0 {
            return Ok(0);
        }
        self.need(n)?;
        let v = (self.bit_buf & ((1u64 << n) - 1)) as u32;
        self.drop_bits(n);
        Ok(v)
    }

    #[inline]
    fn align_to_byte(&mut self) {
        let extra = self.bit_count % 8;
        self.drop_bits(extra);
    }

    #[inline]
    fn decode_symbol(&mut self, decoder: &Decoder) -> Result<u16> {
        self.need(MAX_BITS as u32)?;
        let (symbol, used) = decoder.decode(self.bit_buf)?;
        self.drop_bits(used);
        Ok(symbol)
    }

    fn read_block_header(&mut self) -> Result<()> {
        if self.final_block {
            self.state = State::Done;
            return Ok(());
        }

        self.final_block = self.bits(1)? == 1;
        let btype = self.bits(2)?;

        match btype {
            0 => {
                self.align_to_byte();
                let len = self.bits(16)? as u16;
                let nlen = self.bits(16)? as u16;
                if len != !nlen {
                    return Err(Error::malformed(format!("stored block length check failed: len={len:#06x} nlen={nlen:#06x}")));
                }
                self.state = State::Stored(len as u32);
            }
            1 => {
                self.literal = Some(Decoder::new(&fixed_literal_lengths())?);
                self.distance = Some(Decoder::new(&fixed_distance_lengths())?);
                self.state = State::Compressed;
            }
            2 => {
                self.read_dynamic_tables()?;
                self.state = State::Compressed;
            }
            _ => return Err(Error::malformed("reserved deflate block type 3")),
        }
        Ok(())
    }

    fn read_dynamic_tables(&mut self) -> Result<()> {
        let hlit = self.bits(5)? as usize + 257;
        let hdist = self.bits(5)? as usize + 1;
        let hclen = self.bits(4)? as usize + 4;

        if hlit > 286 {
            return Err(Error::malformed(format!("too many literal codes: {hlit}")));
        }
        if hdist > self.variant.distance_codes() {
            return Err(Error::malformed(format!("too many distance codes: {hdist}")));
        }

        let mut code_lengths = [0u8; 19];
        for &slot in CODE_LENGTH_ORDER.iter().take(hclen) {
            code_lengths[slot] = self.bits(3)? as u8;
        }
        let code_decoder = Decoder::new(&code_lengths)?;

        let total = hlit + hdist;
        let mut lengths = vec![0u8; total];
        let mut i = 0;
        while i < total {
            let sym = self.decode_symbol(&code_decoder)?;
            match sym {
                0..=15 => {
                    lengths[i] = sym as u8;
                    i += 1;
                }
                16 => {
                    if i == 0 {
                        return Err(Error::malformed("code length repeat with no previous length"));
                    }
                    let prev = lengths[i - 1];
                    let count = 3 + self.bits(2)? as usize;
                    if i + count > total {
                        return Err(Error::malformed("code length repeat overruns table"));
                    }
                    lengths[i..i + count].fill(prev);
                    i += count;
                }
                17 => {
                    let count = 3 + self.bits(3)? as usize;
                    if i + count > total {
                        return Err(Error::malformed("zero-length run overruns table"));
                    }
                    i += count;
                }
                18 => {
                    let count = 11 + self.bits(7)? as usize;
                    if i + count > total {
                        return Err(Error::malformed("zero-length run overruns table"));
                    }
                    i += count;
                }
                _ => return Err(Error::malformed(format!("invalid code length symbol {sym}"))),
            }
        }

        if lengths[256] == 0 {
            return Err(Error::malformed("no end-of-block code in dynamic block"));
        }

        self.literal = Some(Decoder::new(&lengths[..hlit])?);
        self.distance = Some(Decoder::new(&lengths[hlit..])?);
        Ok(())
    }

    fn read_stored(&mut self, mut remaining: u32) -> Result<()> {
        while remaining > 0 && self.out_len < self.out.len() {
            if self.bit_count >= 8 {
                self.out[self.out_len] = (self.bit_buf & 0xff) as u8;
                self.out_len += 1;
                self.drop_bits(8);
                remaining -= 1;
                continue;
            }

            if self.in_pos == self.in_end {
                self.fill_input()?;
                if self.in_pos == self.in_end {
                    return Err(Error::malformed("truncated stored block"));
                }
            }

            let want = (remaining as usize).min(self.out.len() - self.out_len);
            let avail = (self.in_end - self.in_pos).min(want);
            self.out[self.out_len..self.out_len + avail].copy_from_slice(&self.in_buf[self.in_pos..self.in_pos + avail]);
            self.in_pos += avail;
            self.out_len += avail;
            remaining -= avail as u32;
        }

        self.state = if remaining == 0 { State::BlockHeader } else { State::Stored(remaining) };
        Ok(())
    }

    #[inline]
    fn copy_match(&mut self, length: usize, distance: usize) -> Result<usize> {
        if distance > self.out_len {
            return Err(Error::malformed(format!("back-reference distance {distance} exceeds {} bytes of history", self.out_len)));
        }

        let space = self.out.len() - self.out_len;
        let n = length.min(space);
        let src = self.out_len - distance;

        if distance >= n {
            self.out.copy_within(src..src + n, self.out_len);
        } else {
            for i in 0..n {
                self.out[self.out_len + i] = self.out[src + i];
            }
        }
        self.out_len += n;

        Ok(length - n)
    }

    fn read_compressed(&mut self) -> Result<()> {
        let literal = self.literal.take().ok_or_else(|| Error::malformed("no literal table"))?;
        let distance = self.distance.take().ok_or_else(|| Error::malformed("no distance table"))?;

        let result = self.decode_block(&literal, &distance);

        self.literal = Some(literal);
        self.distance = Some(distance);
        result
    }

    fn decode_block(&mut self, literal: &Decoder, distance: &Decoder) -> Result<()> {
        if let Some((len, dist)) = self.pending_copy.take() {
            let left = self.copy_match(len, dist)?;
            if left > 0 {
                self.pending_copy = Some((left, dist));
                return Ok(());
            }
        }

        while self.out_len < self.out.len() {
            let sym = self.decode_symbol(literal)?;

            match sym {
                0..=255 => {
                    self.out[self.out_len] = sym as u8;
                    self.out_len += 1;
                }
                256 => {
                    self.state = State::BlockHeader;
                    return Ok(());
                }
                257..=285 => {
                    let idx = (sym - 257) as usize;
                    let (base, extra) = match self.variant {
                        Variant::Deflate => (LENGTH_BASE[idx] as usize, LENGTH_EXTRA[idx]),
                        Variant::Deflate64 => (LENGTH_BASE64[idx] as usize, LENGTH_EXTRA64[idx]),
                    };
                    let length = base + self.bits(extra as u32)? as usize;

                    let dsym = self.decode_symbol(distance)? as usize;
                    if dsym >= self.variant.distance_codes() {
                        return Err(Error::malformed(format!("invalid distance symbol {dsym}")));
                    }
                    let (base, extra) = match self.variant {
                        Variant::Deflate => (DIST_BASE[dsym] as usize, DIST_EXTRA[dsym]),
                        Variant::Deflate64 => (DIST_BASE64[dsym] as usize, DIST_EXTRA64[dsym]),
                    };
                    let dist = base + self.bits(extra as u32)? as usize;

                    let left = self.copy_match(length, dist)?;
                    if left > 0 {
                        self.pending_copy = Some((left, dist));
                        return Ok(());
                    }
                }
                _ => {
                    return Err(Error::malformed(format!("invalid literal/length symbol {sym} (286 and 287 are never valid)")));
                }
            }
        }

        Ok(())
    }

    fn compact(&mut self) {
        debug_assert_eq!(self.out_read, self.out_len);

        if self.out_len <= self.window {
            return;
        }
        let keep = self.window;
        let from = self.out_len - keep;
        self.out.copy_within(from..self.out_len, 0);
        self.out_len = keep;
        self.out_read = keep;
    }

    fn produce(&mut self) -> Result<()> {
        while self.out_read == self.out_len && self.state != State::Done {
            self.compact();

            match self.state {
                State::BlockHeader => self.read_block_header()?,
                State::Stored(remaining) => self.read_stored(remaining)?,
                State::Compressed => self.read_compressed()?,
                State::Done => break,
            }
        }
        Ok(())
    }
}

impl<R: Read> Read for InflateReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }

        if self.out_read == self.out_len {
            self.produce()?;
        }

        let available = self.out_len - self.out_read;
        if available == 0 {
            return Ok(0);
        }

        let n = available.min(buf.len());
        buf[..n].copy_from_slice(&self.out[self.out_read..self.out_read + n]);
        self.out_read += n;
        Ok(n)
    }
}

pub fn decompress(data: &[u8], size_hint: usize) -> Result<Vec<u8>> {
    decompress_variant(data, size_hint, Variant::Deflate)
}

pub fn decompress_variant(data: &[u8], size_hint: usize, variant: Variant) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(size_hint);
    let mut reader = InflateReader::with_variant(data, variant);
    reader.read_to_end(&mut out)?;
    Ok(out)
}
