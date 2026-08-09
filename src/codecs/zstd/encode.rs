use std::io::Write;

use crate::codecs::sliding::{Feed, Sliding};
use crate::codecs::zstd::bits::BitWriter;
use crate::codecs::zstd::fse::{EncTable, Table};
use crate::codecs::zstd::huffman;
use crate::codecs::zstd::sequences::{
    DEFAULT_LITERAL_LENGTH, DEFAULT_LITERAL_LENGTH_LOG, DEFAULT_MATCH_LENGTH, DEFAULT_MATCH_LENGTH_LOG, DEFAULT_OFFSET, DEFAULT_OFFSET_LOG,
    LITERAL_LENGTH_BASE, LITERAL_LENGTH_EXTRA, MATCH_LENGTH_BASE, MATCH_LENGTH_EXTRA,
};
use crate::utils::error::{Error, Result};
use crate::utils::xxhash::XxHash64;

pub const MAGIC: [u8; 4] = [0x28, 0xb5, 0x2f, 0xfd];

const BLOCK_MAX: usize = 128 * 1024;

const RAW_BLOCK: u32 = 0;
const RLE_BLOCK: u32 = 1;
const COMPRESSED_BLOCK: u32 = 2;

const WINDOW_LOG: u32 = 23;
/// How far back a match may reach, and so how much input a streaming writer holds.
pub const WINDOW_SIZE: usize = 1 << WINDOW_LOG;

const MAX_PREDEFINED_OFFSET_CODE: u32 = 28;

const MIN_MATCH: usize = 3;

/// How fast the search gives up on a stretch that keeps yielding literals.
///
/// After enough misses in a row the encoder starts stepping over positions
/// rather than searching every one, which is what keeps incompressible input
/// from costing a full hash chain walk per byte. Any match resets it.
const SKIP_SHIFT: u32 = 6;
const HASH_LOG: u32 = 17;
const NO_POSITION: u32 = u32::MAX;

pub const DEFAULT_DEPTH: usize = 32;

fn write_frame_header(out: &mut Vec<u8>, len: u64, checksum: bool) {
    out.extend_from_slice(&MAGIC);

    let single = len <= WINDOW_SIZE as u64;

    let (field, width) = if single {
        if len < 256 {
            (0u8, 1usize)
        } else if len < 65_536 + 256 {
            (1, 2)
        } else {
            (2, 4)
        }
    } else if len <= u32::MAX as u64 {
        (2, 4)
    } else {
        (3, 8)
    };

    let descriptor = (field << 6) | (u8::from(single) << 5) | (u8::from(checksum) << 2);
    out.push(descriptor);

    if !single {
        out.push(((WINDOW_LOG - 10) << 3) as u8);
    }

    let stored = if width == 2 { len - 256 } else { len };
    out.extend_from_slice(&stored.to_le_bytes()[..width]);
}

fn write_block_header(out: &mut Vec<u8>, kind: u32, size: usize, last: bool) {
    let value = u32::from(last) | (kind << 1) | ((size as u32) << 3);
    out.extend_from_slice(&value.to_le_bytes()[..3]);
}

fn rle_byte(block: &[u8]) -> Option<u8> {
    let first = *block.first()?;
    if block.iter().all(|&b| b == first) { Some(first) } else { None }
}

struct Sequence {
    literals: u32,
    length: u32,
    distance: u32,
}

struct Finder {
    head: Vec<u32>,
    chain: Vec<u32>,
    mask: usize,
    depth: usize,
}

impl Finder {
    fn new(len: usize, depth: usize) -> Self {
        let wanted = len.clamp(1, WINDOW_SIZE);
        let span = 1usize << (usize::BITS - 1 - wanted.leading_zeros());
        Finder { head: vec![NO_POSITION; 1 << HASH_LOG], chain: vec![NO_POSITION; span], mask: span - 1, depth: depth.max(1) }
    }

    fn hash(feed: &Feed, at: usize) -> usize {
        let value = u32::from(feed.get(at)) | (u32::from(feed.get(at + 1)) << 8) | (u32::from(feed.get(at + 2)) << 16);
        (value.wrapping_mul(2_654_435_761) >> (32 - HASH_LOG)) as usize
    }

    fn insert(&mut self, feed: &Feed, at: usize) {
        if at + MIN_MATCH > feed.end() {
            return;
        }
        let slot = Self::hash(feed, at);
        self.chain[at & self.mask] = self.head[slot];
        self.head[slot] = at as u32;
    }

    fn find(&self, feed: &Feed, at: usize, max_len: usize) -> Option<(usize, usize)> {
        if max_len < MIN_MATCH || at + MIN_MATCH > feed.end() {
            return None;
        }

        let earliest = at.saturating_sub(WINDOW_SIZE.min(self.chain.len())).max(feed.base());
        let mut candidate = self.head[Self::hash(feed, at)];
        let mut tries = self.depth;
        let mut best = (0usize, 0usize);

        while candidate != NO_POSITION && tries > 0 {
            let position = candidate as usize;
            if position < earliest {
                break;
            }
            tries -= 1;

            if feed.get(position + best.0) == feed.get(at + best.0) {
                let mut length = 0usize;
                while length < max_len && feed.get(position + length) == feed.get(at + length) {
                    length += 1;
                }
                if length > best.0 {
                    best = (length, at - position);
                    if length == max_len {
                        break;
                    }
                }
            }

            candidate = self.chain[position & self.mask];
        }

        if best.0 >= MIN_MATCH { Some(best) } else { None }
    }
}

struct Predefined {
    literal: EncTable,
    match_length: EncTable,
    offset: EncTable,
}

impl Predefined {
    fn new() -> Result<Self> {
        Ok(Predefined {
            literal: EncTable::new(Table::from_counts(&DEFAULT_LITERAL_LENGTH, DEFAULT_LITERAL_LENGTH_LOG)?)?,
            match_length: EncTable::new(Table::from_counts(&DEFAULT_MATCH_LENGTH, DEFAULT_MATCH_LENGTH_LOG)?)?,
            offset: EncTable::new(Table::from_counts(&DEFAULT_OFFSET, DEFAULT_OFFSET_LOG)?)?,
        })
    }
}

#[derive(Clone, Copy)]
struct Code {
    symbol: u8,
    extra: u64,
    bits: u32,
}

struct Coded {
    literal: Code,
    match_length: Code,
    offset: Code,
}

fn code_for(bases: &[u32], value: u32) -> usize {
    bases.partition_point(|&base| base <= value) - 1
}

fn encode_sequence(sequence: &Sequence) -> Result<Coded> {
    let literal = {
        let code = code_for(&LITERAL_LENGTH_BASE, sequence.literals);
        Code { symbol: code as u8, extra: (sequence.literals - LITERAL_LENGTH_BASE[code]) as u64, bits: u32::from(LITERAL_LENGTH_EXTRA[code]) }
    };

    let match_length = {
        let code = code_for(&MATCH_LENGTH_BASE, sequence.length);
        Code { symbol: code as u8, extra: (sequence.length - MATCH_LENGTH_BASE[code]) as u64, bits: u32::from(MATCH_LENGTH_EXTRA[code]) }
    };

    let base = sequence.distance + 3;
    let code = u32::BITS - 1 - base.leading_zeros();
    if code > MAX_PREDEFINED_OFFSET_CODE {
        return Err(Error::malformed(format!("zstd offset code {code} is outside the predefined table")));
    }
    let offset = Code { symbol: code as u8, extra: u64::from(base - (1 << code)), bits: code };

    Ok(Coded { literal, match_length, offset })
}

fn write_sequence_count(out: &mut Vec<u8>, count: usize) {
    if count < 128 {
        out.push(count as u8);
    } else if count < 0x7F00 {
        out.push(0x80 | (count >> 8) as u8);
        out.push(count as u8);
    } else {
        let value = count - 0x7F00;
        out.push(0xFF);
        out.push(value as u8);
        out.push((value >> 8) as u8);
    }
}

fn write_raw_literals(out: &mut Vec<u8>, literals: &[u8]) {
    let len = literals.len();

    if len < 32 {
        out.push((len as u8) << 3);
    } else if len < 4096 {
        let header = (1u32 << 2) | ((len as u32) << 4);
        out.extend_from_slice(&header.to_le_bytes()[..2]);
    } else {
        let header = (3u32 << 2) | ((len as u32) << 4);
        out.extend_from_slice(&header.to_le_bytes()[..3]);
    }

    out.extend_from_slice(literals);
}

fn huffman_literals(literals: &[u8]) -> Option<Vec<u8>> {
    if literals.len() < 64 {
        return None;
    }

    let mut freqs = [0u32; 256];
    for &byte in literals {
        freqs[byte as usize] += 1;
    }

    let weights = huffman::Table::build(&freqs)?;
    let codes = huffman::Table::codes(&weights).ok()?;
    let description = huffman::Table::describe(&weights);

    let per_stream = literals.len().div_ceil(4);
    let mut streams: Vec<Vec<u8>> = Vec::with_capacity(4);

    for index in 0..4 {
        let start = (index * per_stream).min(literals.len());
        let end = ((index + 1) * per_stream).min(literals.len());

        let mut writer = BitWriter::new();
        for &byte in literals[start..end].iter().rev() {
            let (code, bits) = codes[byte as usize];
            if bits == 0 {
                return None;
            }
            writer.add(code as u64, bits as u32);
        }
        streams.push(writer.finish());
    }

    if streams.iter().any(|s| s.len() > u16::MAX as usize) {
        return None;
    }

    let compressed = description.len() + 6 + streams.iter().map(Vec::len).sum::<usize>();
    if compressed + 5 >= literals.len() {
        return None;
    }

    let mut out = Vec::with_capacity(compressed + 5);
    let value = 2u64 | (3 << 2) | ((literals.len() as u64) << 4) | ((compressed as u64) << 22);
    out.extend_from_slice(&value.to_le_bytes()[..5]);

    out.extend_from_slice(&description);
    for stream in streams.iter().take(3) {
        out.extend_from_slice(&(stream.len() as u16).to_le_bytes());
    }
    for stream in &streams {
        out.extend_from_slice(stream);
    }

    Some(out)
}

fn compressed_body(literals: &[u8], sequences: &[Sequence], tables: &Predefined) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(literals.len() + sequences.len() * 4 + 16);
    match huffman_literals(literals) {
        Some(coded) => out.extend_from_slice(&coded),
        None => write_raw_literals(&mut out, literals),
    }

    write_sequence_count(&mut out, sequences.len());
    if sequences.is_empty() {
        return Ok(out);
    }

    out.push(0);

    let coded: Vec<Coded> = sequences.iter().map(encode_sequence).collect::<Result<_>>()?;
    let (last, rest) = coded.split_last().expect("the sequence list is not empty");

    let mut writer = BitWriter::new();

    let mut literal_state = tables.literal.start(last.literal.symbol)?;
    let mut offset_state = tables.offset.start(last.offset.symbol)?;
    let mut match_state = tables.match_length.start(last.match_length.symbol)?;

    writer.add(last.literal.extra, last.literal.bits);
    writer.add(last.match_length.extra, last.match_length.bits);
    writer.add(last.offset.extra, last.offset.bits);

    for sequence in rest.iter().rev() {
        tables.offset.encode(&mut offset_state, sequence.offset.symbol, &mut writer)?;
        tables.match_length.encode(&mut match_state, sequence.match_length.symbol, &mut writer)?;
        tables.literal.encode(&mut literal_state, sequence.literal.symbol, &mut writer)?;

        writer.add(sequence.literal.extra, sequence.literal.bits);
        writer.add(sequence.match_length.extra, sequence.match_length.bits);
        writer.add(sequence.offset.extra, sequence.offset.bits);
    }

    tables.match_length.flush(match_state, &mut writer);
    tables.offset.flush(offset_state, &mut writer);
    tables.literal.flush(literal_state, &mut writer);

    out.extend_from_slice(&writer.finish());
    Ok(out)
}

fn worth_coding(length: usize, distance: usize) -> bool {
    match length {
        3 => distance < 1024,
        4 => distance < 65_536,
        _ => true,
    }
}

fn tokenise(feed: &Feed, start: usize, finder: &mut Finder, literals: &mut Vec<u8>, sequences: &mut Vec<Sequence>) -> usize {
    literals.clear();
    sequences.clear();

    let mut at = start;
    let mut pending = 0u32;
    let mut barren = 0usize;

    while at < feed.end() && at - start < BLOCK_MAX {
        let room = BLOCK_MAX - (at - start);
        let max_len = room.min(feed.end() - at);

        match finder.find(feed, at, max_len).filter(|&(length, distance)| worth_coding(length, distance)) {
            Some((length, distance)) => {
                sequences.push(Sequence { literals: pending, length: length as u32, distance: distance as u32 });
                pending = 0;

                for step in 0..length {
                    finder.insert(feed, at + step);
                }
                at += length;
                barren = 0;
            }
            None => {
                let step = 1 + (barren >> SKIP_SHIFT);
                for offset in 0..step.min(feed.end() - at).min(BLOCK_MAX - (at - start)) {
                    literals.push(feed.get(at + offset));
                    pending += 1;
                    finder.insert(feed, at + offset);
                }
                at += step.min(feed.end() - at).min(BLOCK_MAX - (at - start)).max(1);
                barren += 1;
            }
        }
    }

    at
}

/// A Zstandard frame written as its input arrives.
///
/// The frame header says the content size is unknown, since it is; blocks go
/// out as each fills, and an empty final block closes the frame. Only the last
/// window of input is held, so memory does not follow the input size.
pub struct Writer<W: Write> {
    out: W,
    tables: Predefined,
    finder: Finder,
    window: Sliding,
    literals: Vec<u8>,
    sequences: Vec<Sequence>,
    digest: XxHash64,
    at: usize,
    checksum: bool,
}

impl<W: Write> Writer<W> {
    /// Start a frame, searching `depth` candidates per position.
    pub fn new(mut out: W, checksum: bool, depth: usize) -> Result<Self> {
        let mut header = Vec::with_capacity(8);
        header.extend_from_slice(&MAGIC);
        header.push(u8::from(checksum) << 2);
        header.push(((WINDOW_LOG - 10) << 3) as u8);
        out.write_all(&header)?;

        Ok(Writer {
            out,
            tables: Predefined::new()?,
            finder: Finder::new(usize::MAX, depth),
            window: Sliding::new(WINDOW_SIZE + BLOCK_MAX),
            literals: Vec::with_capacity(BLOCK_MAX),
            sequences: Vec::new(),
            digest: XxHash64::new(0),
            at: 0,
            checksum,
        })
    }

    /// Hand over more input, encoding whatever has become complete.
    pub fn push(&mut self, bytes: &[u8]) -> Result<()> {
        self.digest.update(bytes);
        self.window.push(bytes);

        while self.window.end() - self.at >= BLOCK_MAX + MIN_MATCH {
            self.block()?;
        }

        self.window.retain(self.at, WINDOW_SIZE + MIN_MATCH);
        Ok(())
    }

    /// Encode what is left, close the frame and give back the writer.
    pub fn finish(mut self) -> Result<W> {
        while self.at < self.window.end() {
            self.block()?;
        }

        let mut tail = Vec::with_capacity(8);
        write_block_header(&mut tail, RAW_BLOCK, 0, true);
        if self.checksum {
            tail.extend_from_slice(&(self.digest.finish() as u32).to_le_bytes());
        }

        self.out.write_all(&tail)?;
        self.out.flush()?;
        Ok(self.out)
    }

    fn block(&mut self) -> Result<()> {
        let Writer { out, tables, finder, window, literals, sequences, at, .. } = self;

        let feed = window.feed();
        let end = tokenise(&feed, *at, finder, literals, sequences);
        let block = feed.slice(*at, end);

        let mut framed = Vec::with_capacity(block.len() + 16);
        match rle_byte(block) {
            Some(byte) => {
                write_block_header(&mut framed, RLE_BLOCK, block.len(), false);
                framed.push(byte);
            }
            None => {
                let body = compressed_body(literals, sequences, tables)?;

                if body.len() < block.len() {
                    write_block_header(&mut framed, COMPRESSED_BLOCK, body.len(), false);
                    framed.extend_from_slice(&body);
                } else {
                    write_block_header(&mut framed, RAW_BLOCK, block.len(), false);
                    framed.extend_from_slice(block);
                }
            }
        }

        out.write_all(&framed)?;
        *at = end;
        Ok(())
    }
}

/// Compress into a Zstandard frame.
pub fn compress(data: &[u8], checksum: bool) -> Result<Vec<u8>> {
    compress_at(data, checksum, DEFAULT_DEPTH)
}

/// Compress with a given match finder depth: higher searches harder.
pub fn compress_at(data: &[u8], checksum: bool, depth: usize) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(data.len() / 3 + 64);
    write_frame_header(&mut out, data.len() as u64, checksum);

    if data.is_empty() {
        write_block_header(&mut out, RAW_BLOCK, 0, true);
    } else {
        let tables = Predefined::new()?;
        let mut finder = Finder::new(data.len(), depth);
        let mut literals = Vec::with_capacity(BLOCK_MAX);
        let mut sequences = Vec::new();
        let feed = Feed::whole(data);

        let mut at = 0usize;
        while at < data.len() {
            let end = tokenise(&feed, at, &mut finder, &mut literals, &mut sequences);
            let block = &data[at..end];
            let last = end == data.len();

            match rle_byte(block) {
                Some(byte) => {
                    write_block_header(&mut out, RLE_BLOCK, block.len(), last);
                    out.push(byte);
                }
                None => {
                    let body = compressed_body(&literals, &sequences, &tables)?;

                    if body.len() < block.len() {
                        write_block_header(&mut out, COMPRESSED_BLOCK, body.len(), last);
                        out.extend_from_slice(&body);
                    } else {
                        write_block_header(&mut out, RAW_BLOCK, block.len(), last);
                        out.extend_from_slice(block);
                    }
                }
            }

            at = end;
        }
    }

    if checksum {
        let digest = XxHash64::hash(data) as u32;
        out.extend_from_slice(&digest.to_le_bytes());
    }

    Ok(out)
}
