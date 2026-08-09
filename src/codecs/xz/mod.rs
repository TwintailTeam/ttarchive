pub mod encode;
pub mod filters;

use std::io::Read;

use crate::codecs::lzma::lzma2::{Lzma2Decoder, dictionary_size};
use crate::crypto::sha256::Sha256;
use crate::utils::crc32::Crc32;
use crate::utils::crc64::Crc64;
use crate::utils::error::{Error, Result, Unsupported};

const MAGIC: [u8; 6] = [0xFD, b'7', b'z', b'X', b'Z', 0x00];
const FOOTER_MAGIC: [u8; 2] = [b'Y', b'Z'];

const MAX_FILTERS: usize = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Check {
    None,
    Crc32,
    Crc64,
    Sha256,
}

impl Check {
    fn from_id(id: u8) -> Result<Self> {
        match id {
            0x00 => Ok(Check::None),
            0x01 => Ok(Check::Crc32),
            0x04 => Ok(Check::Crc64),
            0x0A => Ok(Check::Sha256),
            other => Err(Error::Unsupported(Unsupported::Other(match other {
                0x02 | 0x03 => "an xz stream using a reserved CRC-32 check variant",
                0x05 | 0x06 => "an xz stream using a reserved CRC-64 check variant",
                _ => "an xz stream using an unassigned integrity check",
            }))),
        }
    }

    fn len(self) -> usize {
        match self {
            Check::None => 0,
            Check::Crc32 => 4,
            Check::Crc64 => 8,
            Check::Sha256 => 32,
        }
    }
}

enum Checker {
    None,
    Crc32(Crc32),
    Crc64(Crc64),
    Sha256(Box<Sha256>),
}

impl Checker {
    fn new(check: Check) -> Self {
        match check {
            Check::None => Checker::None,
            Check::Crc32 => Checker::Crc32(Crc32::new()),
            Check::Crc64 => Checker::Crc64(Crc64::new()),
            Check::Sha256 => Checker::Sha256(Box::default()),
        }
    }

    fn update(&mut self, data: &[u8]) {
        match self {
            Checker::None => {}
            Checker::Crc32(c) => c.update(data),
            Checker::Crc64(c) => c.update(data),
            Checker::Sha256(h) => h.update(data),
        }
    }

    fn verify(self, stored: &[u8]) -> Result<()> {
        let computed = match self {
            Checker::None => return Ok(()),
            Checker::Crc32(c) => c.finish().to_le_bytes().to_vec(),
            Checker::Crc64(c) => c.finish().to_le_bytes().to_vec(),
            Checker::Sha256(h) => h.finish().to_vec(),
        };
        if computed != stored {
            return Err(Error::malformed("xz block failed its integrity check; the compressed data was altered"));
        }
        Ok(())
    }
}

fn varint(input: &mut impl Read) -> Result<u64> {
    let mut value = 0u64;
    for shift in 0..9 {
        let mut b = [0u8; 1];
        input.read_exact(&mut b).map_err(|_| Error::malformed("xz multibyte integer runs past the end of the stream"))?;
        value |= ((b[0] & 0x7f) as u64) << (shift * 7);
        if b[0] & 0x80 == 0 {
            if b[0] == 0 && shift != 0 {
                return Err(Error::malformed("xz multibyte integer has a redundant trailing zero"));
            }
            return Ok(value);
        }
    }
    Err(Error::malformed("xz multibyte integer is longer than nine bytes"))
}

fn varint_slice(input: &mut &[u8]) -> Result<u64> {
    let mut cursor = *input;
    let before = cursor.len();
    let value = varint(&mut cursor)?;
    *input = &input[before - cursor.len()..];
    Ok(value)
}

pub fn decompress(data: &[u8], size_hint: usize) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(size_hint);
    let mut input = data;

    loop {
        let Some(rest) = decode_stream(input, &mut out)? else { return Ok(out) };

        input = skip_stream_padding(rest)?;
        if input.is_empty() {
            return Ok(out);
        }
    }
}

fn skip_stream_padding(input: &[u8]) -> Result<&[u8]> {
    let zeros = input.iter().take_while(|&&b| b == 0).count();
    if !zeros.is_multiple_of(4) {
        return Err(Error::malformed("xz stream padding is not a multiple of four bytes"));
    }
    Ok(&input[zeros..])
}

fn decode_stream<'a>(data: &'a [u8], out: &mut Vec<u8>) -> Result<Option<&'a [u8]>> {
    let mut input = data;

    let mut header = [0u8; 12];
    input.read_exact(&mut header).map_err(|_| Error::malformed("xz stream is too short to hold its header"))?;
    if header[..6] != MAGIC {
        return Err(Error::malformed("not an xz stream: wrong magic bytes"));
    }
    if header[6] != 0 || header[7] & 0xf0 != 0 {
        return Err(Error::malformed("xz stream flags use reserved bits"));
    }
    let flags_crc = u32::from_le_bytes([header[8], header[9], header[10], header[11]]);
    if crate::utils::crc32::checksum(&header[6..8]) != flags_crc {
        return Err(Error::malformed("xz stream header checksum does not match its flags"));
    }
    let check = Check::from_id(header[7] & 0x0f)?;

    loop {
        let mut first = [0u8; 1];
        input.read_exact(&mut first).map_err(|_| Error::malformed("xz stream ends before its index"))?;

        if first[0] == 0 {
            break;
        }

        let header_len = (first[0] as usize + 1) * 4;
        let mut block_header = vec![0u8; header_len];
        block_header[0] = first[0];
        input.read_exact(&mut block_header[1..]).map_err(|_| Error::malformed("xz block header runs past the end of the stream"))?;

        let stored = u32::from_le_bytes(block_header[header_len - 4..].try_into().expect("four trailing bytes"));
        if crate::utils::crc32::checksum(&block_header[..header_len - 4]) != stored {
            return Err(Error::malformed("xz block header checksum does not match"));
        }

        let block = BlockHeader::parse(&block_header[1..header_len - 4])?;

        let available = input.len();
        let packed = block.compressed_size.unwrap_or(available as u64) as usize;
        if packed > available {
            return Err(Error::malformed("xz block claims more data than the stream holds"));
        }

        let (body, rest) = input.split_at(packed);
        let produced = block.decode(body, block.uncompressed_size)?;

        if let Some(want) = block.uncompressed_size
            && produced.len() as u64 != want
        {
            return Err(Error::malformed(format!("xz block produced {} bytes, not the {want} its header declared", produced.len())));
        }

        let mut checker = Checker::new(check);
        checker.update(&produced);
        out.extend_from_slice(&produced);

        input = rest;

        let Some(_) = block.compressed_size else { return Ok(None) };

        let padding = (4 - (packed % 4)) % 4;
        if input.len() < padding + check.len() {
            return Err(Error::malformed("xz block ends before its integrity check"));
        }
        if input[..padding].iter().any(|&b| b != 0) {
            return Err(Error::malformed("xz block padding is not zero"));
        }
        checker.verify(&input[padding..padding + check.len()])?;
        input = &input[padding + check.len()..];
    }

    Ok(Some(verify_tail(input, &header[6..8])?))
}

fn verify_tail<'a>(input: &'a [u8], stream_flags: &[u8]) -> Result<&'a [u8]> {
    let mut cursor = input;
    let records = varint(&mut cursor)?;
    for _ in 0..records {
        varint(&mut cursor)?;
        varint(&mut cursor)?;
    }

    let consumed = input.len() - cursor.len() + 1;
    let padding = (4 - (consumed % 4)) % 4;
    if cursor.len() < padding + 4 {
        return Err(Error::malformed("xz index ends before its checksum"));
    }
    let cursor = &cursor[padding + 4..];

    if cursor.len() < 12 {
        return Err(Error::malformed("xz stream ends before its footer"));
    }
    let footer = &cursor[..12];
    if footer[10..12] != FOOTER_MAGIC {
        return Err(Error::malformed("xz stream footer has the wrong magic bytes"));
    }
    let stored = u32::from_le_bytes(footer[..4].try_into().expect("four bytes"));
    if crate::utils::crc32::checksum(&footer[4..10]) != stored {
        return Err(Error::malformed("xz stream footer checksum does not match"));
    }
    if footer[8..10] != *stream_flags {
        return Err(Error::malformed("xz stream footer flags disagree with the header; the stream was spliced"));
    }

    Ok(&cursor[12..])
}

struct BlockHeader {
    compressed_size: Option<u64>,
    uncompressed_size: Option<u64>,
    filters: Vec<(u64, Vec<u8>)>,
}

impl BlockHeader {
    fn parse(mut body: &[u8]) -> Result<Self> {
        let flags = body.first().copied().ok_or_else(|| Error::malformed("xz block header is empty"))?;
        body = &body[1..];

        if flags & 0x3C != 0 {
            return Err(Error::malformed("xz block flags use reserved bits"));
        }
        let filter_count = (flags & 0x03) as usize + 1;

        let compressed_size = if flags & 0x40 != 0 { Some(varint_slice(&mut body)?) } else { None };
        let uncompressed_size = if flags & 0x80 != 0 { Some(varint_slice(&mut body)?) } else { None };

        if filter_count > MAX_FILTERS {
            return Err(Error::malformed("xz block declares too many filters"));
        }

        let mut filters = Vec::with_capacity(filter_count);
        for _ in 0..filter_count {
            let id = varint_slice(&mut body)?;
            let props_len = varint_slice(&mut body)? as usize;
            if props_len > body.len() {
                return Err(Error::malformed("xz filter properties run past the header"));
            }
            let (props, rest) = body.split_at(props_len);
            filters.push((id, props.to_vec()));
            body = rest;
        }

        if body.iter().any(|&b| b != 0) {
            return Err(Error::malformed("xz block header padding is not zero"));
        }

        Ok(BlockHeader { compressed_size, uncompressed_size, filters })
    }

    fn decode(&self, data: &[u8], size_hint: Option<u64>) -> Result<Vec<u8>> {
        let (compressor_id, compressor_props) = self.filters.last().ok_or_else(|| Error::malformed("xz block declares no filters"))?;

        if *compressor_id != filters::LZMA2 {
            return Err(Error::Unsupported(Unsupported::Other("an xz block whose final filter is not LZMA2")));
        }
        let dict_byte = *compressor_props.first().ok_or_else(|| Error::malformed("xz LZMA2 filter carries no properties"))?;

        let mut out = Vec::with_capacity(size_hint.unwrap_or(0).min(1 << 26) as usize);
        Lzma2Decoder::new(data, dictionary_size(dict_byte)?).read_to_end(&mut out)?;

        for (id, props) in self.filters[..self.filters.len() - 1].iter().rev() {
            filters::decode(*id, props, &mut out)?;
        }

        Ok(out)
    }
}

struct Counting<R> {
    inner: R,
    count: u64,
}

impl<R: Read> Read for Counting<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.count += n as u64;
        Ok(n)
    }
}

fn read_exact_or_none<R: Read>(input: &mut R, buf: &mut [u8]) -> Result<bool> {
    let mut filled = 0usize;
    while filled < buf.len() {
        match input.read(&mut buf[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(ref e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(Error::from(e)),
        }
    }

    if filled == 0 {
        return Ok(false);
    }
    if filled < buf.len() {
        return Err(Error::malformed("xz stream ends mid-field"));
    }
    Ok(true)
}

fn read_varint<R: Read>(input: &mut R) -> Result<(u64, usize)> {
    let mut value = 0u64;
    for index in 0..9 {
        let mut byte = [0u8; 1];
        if !read_exact_or_none(input, &mut byte)? {
            return Err(Error::malformed("xz multibyte integer is truncated"));
        }
        value |= ((byte[0] & 0x7f) as u64) << (index * 7);
        if byte[0] & 0x80 == 0 {
            return Ok((value, index + 1));
        }
    }
    Err(Error::malformed("xz multibyte integer is longer than nine bytes"))
}

enum Stage<R> {
    Header(Option<R>),
    Between(Option<R>),
    Block(Box<Lzma2Decoder<Counting<R>>>),
    Done,
}

/// Streaming xz reader, one block at a time.
///
/// Only blocks whose sole filter is LZMA2 can be read this way; a block that
/// stacks a delta or branch filter on top has to be held whole for the filter
/// to run over it, and is reported as unsupported here so the caller can fall
/// back to [`decompress`].
pub struct Reader<R> {
    stage: Stage<R>,
    check: Check,
    flags: [u8; 2],
    checker: Checker,
    produced: u64,
    declared: Option<u64>,
}

impl<R: Read> Reader<R> {
    pub fn new(inner: R, _size_hint: u64) -> Self {
        Reader { stage: Stage::Header(Some(inner)), check: Check::None, flags: [0; 2], checker: Checker::None, produced: 0, declared: None }
    }

    fn read_stream_header(&mut self, mut inner: R) -> Result<()> {
        let mut header = [0u8; 12];
        loop {
            if !read_exact_or_none(&mut inner, &mut header[..4])? {
                self.stage = Stage::Done;
                return Ok(());
            }
            if header[..4] != [0, 0, 0, 0] {
                break;
            }
        }

        if !read_exact_or_none(&mut inner, &mut header[4..])? {
            return Err(Error::malformed("xz stream is too short to hold its header"));
        }

        if header[..6] != MAGIC {
            return Err(Error::malformed("not an xz stream: wrong magic bytes"));
        }
        if header[6] != 0 || header[7] & 0xf0 != 0 {
            return Err(Error::malformed("xz stream flags use reserved bits"));
        }
        let stored = u32::from_le_bytes([header[8], header[9], header[10], header[11]]);
        if crate::utils::crc32::checksum(&header[6..8]) != stored {
            return Err(Error::malformed("xz stream header checksum does not match its flags"));
        }

        self.check = Check::from_id(header[7] & 0x0f)?;
        self.flags = [header[6], header[7]];
        self.stage = Stage::Between(Some(inner));
        Ok(())
    }

    fn start_block(&mut self, mut inner: R) -> Result<()> {
        let mut first = [0u8; 1];
        if !read_exact_or_none(&mut inner, &mut first)? {
            return Err(Error::malformed("xz stream ends before its index"));
        }

        if first[0] == 0 {
            return self.finish_stream(inner);
        }

        let header_len = (first[0] as usize + 1) * 4;
        let mut header = vec![0u8; header_len];
        header[0] = first[0];
        if !read_exact_or_none(&mut inner, &mut header[1..])? {
            return Err(Error::malformed("xz block header runs past the end of the stream"));
        }

        let stored = u32::from_le_bytes(header[header_len - 4..].try_into().expect("four trailing bytes"));
        if crate::utils::crc32::checksum(&header[..header_len - 4]) != stored {
            return Err(Error::malformed("xz block header checksum does not match"));
        }

        let block = BlockHeader::parse(&header[1..header_len - 4])?;
        let (id, props) = block.filters.last().ok_or_else(|| Error::malformed("xz block declares no filters"))?;

        if *id != filters::LZMA2 {
            return Err(Error::Unsupported(Unsupported::Other("an xz block whose final filter is not LZMA2")));
        }
        if block.filters.len() > 1 {
            return Err(Error::Unsupported(Unsupported::Other("reading an xz block that stacks another filter on LZMA2 a piece at a time")));
        }

        let dict = dictionary_size(*props.first().ok_or_else(|| Error::malformed("xz LZMA2 filter carries no properties"))?)?;

        self.checker = Checker::new(self.check);
        self.produced = 0;
        self.declared = block.uncompressed_size;
        self.stage = Stage::Block(Box::new(Lzma2Decoder::new(Counting { inner, count: 0 }, dict)));
        Ok(())
    }

    fn finish_block(&mut self, decoder: Lzma2Decoder<Counting<R>>) -> Result<()> {
        if let Some(want) = self.declared
            && self.produced != want
        {
            return Err(Error::malformed(format!("xz block produced {} bytes, not the {want} its header declared", self.produced)));
        }

        let Counting { mut inner, count } = decoder.into_inner();

        let padding = (4 - (count % 4) as usize) % 4;
        let mut tail = vec![0u8; padding + self.check.len()];
        if !tail.is_empty() && !read_exact_or_none(&mut inner, &mut tail)? {
            return Err(Error::malformed("xz block ends before its integrity check"));
        }
        if tail[..padding].iter().any(|&b| b != 0) {
            return Err(Error::malformed("xz block padding is not zero"));
        }

        let checker = std::mem::replace(&mut self.checker, Checker::None);
        checker.verify(&tail[padding..])?;

        self.stage = Stage::Between(Some(inner));
        Ok(())
    }

    fn finish_stream(&mut self, mut inner: R) -> Result<()> {
        let (records, mut consumed) = read_varint(&mut inner)?;
        consumed += 1;

        for _ in 0..records {
            consumed += read_varint(&mut inner)?.1;
            consumed += read_varint(&mut inner)?.1;
        }

        let padding = (4 - (consumed % 4)) % 4;
        let mut tail = vec![0u8; padding + 4 + 12];
        if !read_exact_or_none(&mut inner, &mut tail)? {
            return Err(Error::malformed("xz stream ends before its footer"));
        }

        let footer = &tail[padding + 4..];
        if footer[10..12] != FOOTER_MAGIC {
            return Err(Error::malformed("xz stream footer has the wrong magic bytes"));
        }
        let stored = u32::from_le_bytes(footer[..4].try_into().expect("four bytes"));
        if crate::utils::crc32::checksum(&footer[4..10]) != stored {
            return Err(Error::malformed("xz stream footer checksum does not match"));
        }
        if footer[8..10] != self.flags {
            return Err(Error::malformed("xz stream footer flags disagree with the header; the stream was spliced"));
        }

        self.stage = Stage::Header(Some(inner));
        Ok(())
    }
}

impl<R: Read> Read for Reader<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }

        loop {
            match &mut self.stage {
                Stage::Done => return Ok(0),

                Stage::Header(slot) => {
                    let inner = slot.take().expect("a stream awaiting its header has its reader");
                    self.read_stream_header(inner)?;
                }

                Stage::Between(slot) => {
                    let inner = slot.take().expect("a stream between blocks has its reader");
                    self.start_block(inner)?;
                }

                Stage::Block(decoder) => {
                    let n = decoder.read(buf)?;
                    if n > 0 {
                        self.checker.update(&buf[..n]);
                        self.produced += n as u64;
                        return Ok(n);
                    }

                    let Stage::Block(decoder) = std::mem::replace(&mut self.stage, Stage::Done) else { unreachable!("the stage was just matched as a block") };
                    self.finish_block(*decoder)?;
                }
            }
        }
    }
}

pub struct SliceReader<R> {
    inner: Option<R>,
    out: Vec<u8>,
    read: usize,
    size_hint: usize,
}

impl<R: Read> SliceReader<R> {
    pub fn new(inner: R, size_hint: u64) -> Self {
        SliceReader { inner: Some(inner), out: Vec::new(), read: 0, size_hint: size_hint.min(1 << 30) as usize }
    }
}

impl<R: Read> Read for SliceReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if let Some(mut inner) = self.inner.take() {
            let mut raw = Vec::new();
            inner.read_to_end(&mut raw)?;
            self.out = decompress(&raw, self.size_hint)?;
        }

        let n = (self.out.len() - self.read).min(buf.len());
        buf[..n].copy_from_slice(&self.out[self.read..self.read + n]);
        self.read += n;
        Ok(n)
    }
}
