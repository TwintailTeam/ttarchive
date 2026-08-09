use std::io::{self, Write};

use crate::codecs::deflate::bitwriter::BitWriter;
use crate::codecs::deflate::huffman::MAX_BITS;
use crate::codecs::deflate::inflate::{CODE_LENGTH_ORDER, DIST_BASE, DIST_EXTRA, LENGTH_BASE, LENGTH_EXTRA};
use crate::codecs::deflate::lz77::{MAX_MATCH, MatchFinder, Token};
use crate::codecs::lengths::assign_lengths;
use crate::codecs::{Encoder, Level};
use crate::utils::error::Result;

pub const BLOCK_SIZE: usize = 256 * 1024;

const PROBE_SIZE: usize = 32 * 1024;

const INCOMPRESSIBLE_PERCENT: usize = 2;

const NUM_LITERAL: usize = 288;
const NUM_DIST: usize = 30;
const NUM_CODE_LENGTH: usize = 19;
const END_OF_BLOCK: usize = 256;
const MAX_STORED: usize = 65_535;

static LENGTH_CODE: [u8; MAX_MATCH + 1] = build_length_codes();

const fn build_length_codes() -> [u8; MAX_MATCH + 1] {
    let mut table = [0u8; MAX_MATCH + 1];
    let mut len = 3;
    while len <= MAX_MATCH {
        let mut idx = 0;
        let mut i = 0;
        while i < 29 {
            if LENGTH_BASE[i] as usize <= len {
                idx = i;
            }
            i += 1;
        }
        table[len] = idx as u8;
        len += 1;
    }
    table
}

static DIST_CODE: [u8; 512] = build_dist_codes();

const fn build_dist_codes() -> [u8; 512] {
    let mut table = [0u8; 512];

    let mut dist = 1;
    while dist <= 256 {
        let mut idx = 0;
        let mut i = 0;
        while i < 30 {
            if DIST_BASE[i] as usize <= dist {
                idx = i;
            }
            i += 1;
        }
        table[dist - 1] = idx as u8;
        dist += 1;
    }

    let mut dist = 257;
    while dist <= 32_768 {
        let mut idx = 0;
        let mut i = 0;
        while i < 30 {
            if DIST_BASE[i] as usize <= dist {
                idx = i;
            }
            i += 1;
        }
        table[256 + ((dist - 1) >> 7)] = idx as u8;
        dist += 1;
    }

    table
}

#[inline]
fn dist_code(dist: usize) -> usize {
    if dist <= 256 { DIST_CODE[dist - 1] as usize } else { DIST_CODE[256 + ((dist - 1) >> 7)] as usize }
}

fn canonical_codes(lengths: &[u8]) -> Vec<u16> {
    let mut counts = [0u16; MAX_BITS + 1];
    for &l in lengths {
        if l > 0 {
            counts[l as usize] += 1;
        }
    }

    let mut next_code = [0u16; MAX_BITS + 2];
    let mut code = 0u16;
    for len in 1..=MAX_BITS {
        code = (code + counts[len - 1]) << 1;
        next_code[len] = code;
    }

    lengths
        .iter()
        .map(|&l| {
            if l == 0 {
                return 0;
            }
            let c = next_code[l as usize];
            next_code[l as usize] += 1;
            c.reverse_bits() >> (16 - l as u32)
        })
        .collect()
}

struct Freqs {
    literal: [u32; NUM_LITERAL],
    distance: [u32; NUM_DIST],
}

impl Freqs {
    fn count(tokens: &[Token]) -> Self {
        let mut f = Freqs { literal: [0; NUM_LITERAL], distance: [0; NUM_DIST] };
        for &t in tokens {
            match t {
                Token::Literal(b) => f.literal[b as usize] += 1,
                Token::Match { len, dist } => {
                    f.literal[257 + LENGTH_CODE[len as usize] as usize] += 1;
                    f.distance[dist_code(dist as usize)] += 1;
                }
            }
        }
        f.literal[END_OF_BLOCK] += 1;
        f
    }
}

fn payload_bits(tokens: &[Token], lit_len: &[u8], dist_len: &[u8]) -> u64 {
    let mut bits = 0u64;
    for &t in tokens {
        match t {
            Token::Literal(b) => bits += lit_len[b as usize] as u64,
            Token::Match { len, dist } => {
                let lc = LENGTH_CODE[len as usize] as usize;
                bits += lit_len[257 + lc] as u64 + LENGTH_EXTRA[lc] as u64;
                let dc = dist_code(dist as usize);
                bits += dist_len[dc] as u64 + DIST_EXTRA[dc] as u64;
            }
        }
    }
    bits + lit_len[END_OF_BLOCK] as u64
}

struct CodeLengthStream {
    items: Vec<(u8, u8, u8)>,
    freqs: [u32; NUM_CODE_LENGTH],
}

impl CodeLengthStream {
    fn build(all_lengths: &[u8]) -> Self {
        let mut items = Vec::with_capacity(all_lengths.len());
        let mut freqs = [0u32; NUM_CODE_LENGTH];

        let mut i = 0;
        while i < all_lengths.len() {
            let value = all_lengths[i];
            let mut run = 1;
            while i + run < all_lengths.len() && all_lengths[i + run] == value {
                run += 1;
            }

            if value == 0 {
                while run >= 11 {
                    let n = run.min(138);
                    items.push((18, (n - 11) as u8, 7));
                    freqs[18] += 1;
                    run -= n;
                }
                while run >= 3 {
                    let n = run.min(10);
                    items.push((17, (n - 3) as u8, 3));
                    freqs[17] += 1;
                    run -= n;
                }
            } else {
                items.push((value, 0, 0));
                freqs[value as usize] += 1;
                run -= 1;

                while run >= 3 {
                    let n = run.min(6);
                    items.push((16, (n - 3) as u8, 2));
                    freqs[16] += 1;
                    run -= n;
                }
            }

            for _ in 0..run {
                items.push((value, 0, 0));
                freqs[value as usize] += 1;
            }

            i += {
                let mut n = 1;
                while i + n < all_lengths.len() && all_lengths[i + n] == value {
                    n += 1;
                }
                n
            };
        }

        CodeLengthStream { items, freqs }
    }
}

struct DynamicHeader {
    hlit: usize,
    hdist: usize,
    hclen: usize,
    lit_lengths: Vec<u8>,
    dist_lengths: Vec<u8>,
    cl_lengths: Vec<u8>,
    stream: CodeLengthStream,
}

impl DynamicHeader {
    fn build(freqs: &Freqs) -> Self {
        let mut lit_lengths = assign_lengths(&freqs.literal, MAX_BITS);
        let mut dist_lengths = assign_lengths(&freqs.distance, MAX_BITS);

        if dist_lengths.iter().all(|&l| l == 0) {
            dist_lengths[0] = 1;
            dist_lengths[1] = 1;
        }

        let hlit = lit_lengths.iter().rposition(|&l| l != 0).map_or(256, |p| p.max(256)) + 1;
        let hdist = dist_lengths.iter().rposition(|&l| l != 0).map_or(0, |p| p) + 1;
        lit_lengths.truncate(hlit);
        dist_lengths.truncate(hdist);

        let mut all = Vec::with_capacity(hlit + hdist);
        all.extend_from_slice(&lit_lengths);
        all.extend_from_slice(&dist_lengths);
        let stream = CodeLengthStream::build(&all);

        let cl_lengths = assign_lengths(&stream.freqs, 7);

        let mut hclen = NUM_CODE_LENGTH;
        while hclen > 4 && cl_lengths[CODE_LENGTH_ORDER[hclen - 1]] == 0 {
            hclen -= 1;
        }

        DynamicHeader { hlit, hdist, hclen, lit_lengths, dist_lengths, cl_lengths, stream }
    }

    fn header_bits(&self) -> u64 {
        let mut bits = 5 + 5 + 4 + 3 * self.hclen as u64;
        for &(sym, _, extra) in &self.stream.items {
            bits += self.cl_lengths[sym as usize] as u64 + extra as u64;
        }
        bits
    }
}

struct BlockWriter {
    bits: BitWriter,
    finder: MatchFinder,
    tokens: Vec<Token>,
    fixed_lit_lengths: Vec<u8>,
    fixed_dist_lengths: Vec<u8>,
    fixed_lit_codes: Vec<u16>,
    fixed_dist_codes: Vec<u16>,
}

impl BlockWriter {
    fn new(level: Level) -> Self {
        let fixed_lit_lengths = crate::codecs::deflate::huffman::fixed_literal_lengths().to_vec();
        let fixed_dist_lengths = crate::codecs::deflate::huffman::fixed_distance_lengths().to_vec();
        let fixed_lit_codes = canonical_codes(&fixed_lit_lengths);
        let fixed_dist_codes = canonical_codes(&fixed_dist_lengths);

        BlockWriter {
            bits: BitWriter::with_capacity(BLOCK_SIZE / 2),
            finder: MatchFinder::new(level),
            tokens: Vec::new(),
            fixed_lit_lengths,
            fixed_dist_lengths,
            fixed_lit_codes,
            fixed_dist_codes,
        }
    }

    fn write_block(&mut self, data: &[u8], is_final: bool, level: Level) {
        if data.is_empty() {
            if is_final {
                self.write_fixed(&[], true);
            }
            return;
        }

        if level == Level::None {
            self.write_stored(data, is_final);
            return;
        }

        let mut finder = std::mem::replace(&mut self.finder, MatchFinder::new(level));
        let mut tokens = std::mem::take(&mut self.tokens);

        if data.len() >= PROBE_SIZE * 2 {
            finder.tokenize(&data[..PROBE_SIZE], &mut tokens);
            let matched: usize = tokens
                .iter()
                .map(|t| match t {
                    Token::Match { len, .. } => *len as usize,
                    Token::Literal(_) => 0,
                })
                .sum();

            if matched * 100 < PROBE_SIZE * INCOMPRESSIBLE_PERCENT {
                self.finder = finder;
                self.tokens = tokens;
                self.write_stored(data, is_final);
                return;
            }
        }

        finder.tokenize(data, &mut tokens);
        self.finder = finder;

        let freqs = Freqs::count(&tokens);

        let fixed_bits = 3 + payload_bits(&tokens, &self.fixed_lit_lengths, &self.fixed_dist_lengths);

        let dynamic = DynamicHeader::build(&freqs);
        let dynamic_bits = 3 + dynamic.header_bits() + payload_bits(&tokens, &dynamic.lit_lengths, &dynamic.dist_lengths);

        let chunks = data.len().div_ceil(MAX_STORED).max(1) as u64;
        let stored_bits = chunks * (3 + 32 + 7) + data.len() as u64 * 8;

        if stored_bits <= fixed_bits && stored_bits <= dynamic_bits {
            self.write_stored(data, is_final);
        } else if fixed_bits <= dynamic_bits {
            self.write_fixed(&tokens, is_final);
        } else {
            self.write_dynamic(&tokens, &dynamic, is_final);
        }

        self.tokens = tokens;
    }

    fn write_stored(&mut self, data: &[u8], is_final: bool) {
        let mut remaining = data;
        loop {
            let n = remaining.len().min(MAX_STORED);
            let last = is_final && n == remaining.len();

            self.bits.write_bits(last as u32, 1);
            self.bits.write_bits(0, 2);
            self.bits.align_to_byte();
            self.bits.write_bits(n as u32 & 0xffff, 16);
            self.bits.write_bits(!(n as u32) & 0xffff, 16);
            self.bits.write_bytes(&remaining[..n]);

            remaining = &remaining[n..];
            if remaining.is_empty() {
                break;
            }
        }
    }

    fn write_fixed(&mut self, tokens: &[Token], is_final: bool) {
        self.bits.write_bits(is_final as u32, 1);
        self.bits.write_bits(1, 2);

        let lit_codes = std::mem::take(&mut self.fixed_lit_codes);
        let lit_lengths = std::mem::take(&mut self.fixed_lit_lengths);
        let dist_codes = std::mem::take(&mut self.fixed_dist_codes);
        let dist_lengths = std::mem::take(&mut self.fixed_dist_lengths);

        emit_tokens(&mut self.bits, tokens, &lit_codes, &lit_lengths, &dist_codes, &dist_lengths);

        self.fixed_lit_codes = lit_codes;
        self.fixed_lit_lengths = lit_lengths;
        self.fixed_dist_codes = dist_codes;
        self.fixed_dist_lengths = dist_lengths;
    }

    fn write_dynamic(&mut self, tokens: &[Token], header: &DynamicHeader, is_final: bool) {
        self.bits.write_bits(is_final as u32, 1);
        self.bits.write_bits(2, 2);

        self.bits.write_bits((header.hlit - 257) as u32, 5);
        self.bits.write_bits((header.hdist - 1) as u32, 5);
        self.bits.write_bits((header.hclen - 4) as u32, 4);

        for &slot in CODE_LENGTH_ORDER.iter().take(header.hclen) {
            self.bits.write_bits(header.cl_lengths[slot] as u32, 3);
        }

        let cl_codes = canonical_codes(&header.cl_lengths);
        for &(sym, extra_value, extra_bits) in &header.stream.items {
            self.bits.write_bits(cl_codes[sym as usize] as u32, header.cl_lengths[sym as usize] as u32);
            if extra_bits > 0 {
                self.bits.write_bits(extra_value as u32, extra_bits as u32);
            }
        }

        let lit_codes = canonical_codes(&header.lit_lengths);
        let dist_codes = canonical_codes(&header.dist_lengths);
        emit_tokens(&mut self.bits, tokens, &lit_codes, &header.lit_lengths, &dist_codes, &header.dist_lengths);
    }
}

fn emit_tokens(bits: &mut BitWriter, tokens: &[Token], lit_codes: &[u16], lit_lengths: &[u8], dist_codes: &[u16], dist_lengths: &[u8]) {
    for &t in tokens {
        match t {
            Token::Literal(b) => {
                bits.write_bits(lit_codes[b as usize] as u32, lit_lengths[b as usize] as u32);
            }
            Token::Match { len, dist } => {
                let lc = LENGTH_CODE[len as usize] as usize;
                let sym = 257 + lc;
                bits.write_bits(lit_codes[sym] as u32, lit_lengths[sym] as u32);
                let extra = LENGTH_EXTRA[lc] as u32;
                if extra > 0 {
                    bits.write_bits(len as u32 - LENGTH_BASE[lc] as u32, extra);
                }

                let dc = dist_code(dist as usize);
                bits.write_bits(dist_codes[dc] as u32, dist_lengths[dc] as u32);
                let extra = DIST_EXTRA[dc] as u32;
                if extra > 0 {
                    bits.write_bits(dist as u32 - DIST_BASE[dc] as u32, extra);
                }
            }
        }
    }

    bits.write_bits(lit_codes[END_OF_BLOCK] as u32, lit_lengths[END_OF_BLOCK] as u32);
}

pub struct DeflateEncoder<W: Write> {
    inner: W,
    level: Level,
    blocks: BlockWriter,
    pending: Vec<u8>,
    written: u64,
}

impl<W: Write> DeflateEncoder<W> {
    pub fn new(inner: W, level: Level) -> Self {
        DeflateEncoder { inner, level, blocks: BlockWriter::new(level), pending: Vec::with_capacity(BLOCK_SIZE), written: 0 }
    }

    /// Finish the stream and hand back the underlying writer.
    pub fn finish_inner(mut self) -> Result<W> {
        let block = std::mem::take(&mut self.pending);
        let level = self.level;
        self.blocks.write_block(&block, true, level);

        let bytes = self.blocks.bits.take();
        if !bytes.is_empty() {
            self.inner.write_all(&bytes)?;
            self.written += bytes.len() as u64;
        }
        Ok(self.inner)
    }

    fn emit_ready(&mut self) -> io::Result<()> {
        while self.pending.len() >= BLOCK_SIZE {
            let rest = self.pending.split_off(BLOCK_SIZE);
            let block = std::mem::replace(&mut self.pending, rest);
            self.blocks.write_block(&block, false, self.level);
            self.flush_bits()?;
        }
        Ok(())
    }

    fn flush_bits(&mut self) -> io::Result<()> {
        let bytes = self.blocks.bits.drain_complete_bytes();
        if !bytes.is_empty() {
            self.inner.write_all(&bytes)?;
            self.written += bytes.len() as u64;
        }
        Ok(())
    }
}

impl<W: Write> Write for DeflateEncoder<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.pending.extend_from_slice(buf);
        self.emit_ready()?;
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

impl<W: Write> Encoder for DeflateEncoder<W> {
    fn finish(mut self: Box<Self>) -> Result<u64> {
        let block = std::mem::take(&mut self.pending);
        let level = self.level;
        self.blocks.write_block(&block, true, level);

        let bytes = self.blocks.bits.take();
        if !bytes.is_empty() {
            self.inner.write_all(&bytes)?;
            self.written += bytes.len() as u64;
        }
        self.inner.flush()?;
        Ok(self.written)
    }
}

pub fn compress_chunk(data: &[u8], level: Level, is_final: bool) -> Vec<u8> {
    let mut blocks = BlockWriter::new(level);
    let mut out = Vec::new();

    let mut offset = 0;
    while offset < data.len() {
        let end = (offset + BLOCK_SIZE).min(data.len());
        let last = is_final && end == data.len();
        blocks.write_block(&data[offset..end], last, level);
        out.extend_from_slice(&blocks.bits.drain_complete_bytes());
        offset = end;
    }

    if data.is_empty() && is_final {
        blocks.write_block(&[], true, level);
    }

    if !is_final {
        blocks.write_stored(&[], false);
    }

    out.extend_from_slice(&blocks.bits.take());
    out
}

pub fn compress(data: &[u8], level: Level) -> Vec<u8> {
    let mut blocks = BlockWriter::new(level);
    let mut out = Vec::new();

    let mut offset = 0;
    while offset < data.len() {
        let end = (offset + BLOCK_SIZE).min(data.len());
        blocks.write_block(&data[offset..end], end == data.len(), level);
        out.extend_from_slice(&blocks.bits.drain_complete_bytes());
        offset = end;
    }
    if data.is_empty() {
        blocks.write_block(&[], true, level);
    }

    out.extend_from_slice(&blocks.bits.take());
    out
}
