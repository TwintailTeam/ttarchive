use std::io::{Read, Write};

use crate::codecs::lzma::decode::{LzmaDecoder, Properties};
use crate::codecs::lzma::encode::{Encoder, Finder, MATCH_MAX_LEN, RangeEncoder, Sliding};
use crate::utils::error::{Error, Result, Unsupported};
use crate::utils::io::CountingWriter;

pub const MAGIC: [u8; 4] = *b"LZIP";

const HEADER_LEN: usize = 6;
const TRAILER_LEN: usize = 20;

const LC: u32 = 3;
const LP: u32 = 0;
const PB: u32 = 2;

pub fn is_lzip(prefix: &[u8]) -> bool {
    prefix.len() >= 4 && prefix[..4] == MAGIC
}

fn dictionary_code(wanted: u32) -> u8 {
    for exponent in 12..=29u32 {
        if 1u32 << exponent >= wanted {
            return exponent as u8;
        }
    }
    29
}

fn dictionary_size(coded: u8) -> Result<u32> {
    let exponent = (coded & 0x1f) as u32;
    if !(12..=29).contains(&exponent) {
        return Err(Error::malformed(format!("lzip dictionary exponent {exponent} is outside 12..=29")));
    }

    let base = 1u32 << exponent;
    let subtract = (base / 16) * ((coded >> 5) & 7) as u32;
    Ok(base - subtract)
}

struct Pending<R> {
    held: Vec<u8>,
    at: usize,
    inner: R,
}

impl<R: Read> Read for Pending<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.at < self.held.len() {
            let n = (self.held.len() - self.at).min(buf.len());
            buf[..n].copy_from_slice(&self.held[self.at..self.at + n]);
            self.at += n;
            return Ok(n);
        }
        self.inner.read(buf)
    }
}

impl<R: Read> Pending<R> {
    fn new(inner: R) -> Self {
        Pending { held: Vec::new(), at: 0, inner }
    }

    fn put_back(&mut self, bytes: Vec<u8>) {
        self.held = bytes;
        self.at = 0;
    }

    fn exactly(&mut self, buf: &mut [u8]) -> Result<bool> {
        let mut filled = 0usize;
        while filled < buf.len() {
            match self.read(&mut buf[filled..]) {
                Ok(0) if filled == 0 => return Ok(false),
                Ok(0) => return Err(Error::malformed("lzip stream ends in the middle of a field")),
                Ok(n) => filled += n,
                Err(ref e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(e) => return Err(Error::from(e)),
            }
        }
        Ok(true)
    }
}

enum Stage<R> {
    Between(Pending<R>),
    Member(Box<LzmaDecoder<Pending<R>>>, crate::utils::crc32::Crc32, u64),
    Done,
}

/// An lzip stream decoded as it is read.
///
/// Members follow one another, each with its own dictionary size and a trailer
/// holding the CRC and length this reader checks as it goes. Only one
/// dictionary is held, so the stream decodes in bounded memory.
pub struct Reader<R> {
    stage: Stage<R>,
}

impl<R: Read> Reader<R> {
    /// Wrap `inner` at the start of an lzip stream.
    pub fn new(inner: R) -> Self {
        Reader { stage: Stage::Between(Pending::new(inner)) }
    }

    fn start_member(&mut self, mut source: Pending<R>) -> Result<()> {
        let mut header = [0u8; HEADER_LEN];
        if !source.exactly(&mut header)? {
            self.stage = Stage::Done;
            return Ok(());
        }

        if header.iter().all(|&b| b == 0) {
            self.stage = Stage::Done;
            return Ok(());
        }
        if header[..4] != MAGIC {
            return Err(Error::malformed("lzip member does not start with the LZIP magic"));
        }
        if header[4] != 1 {
            return Err(Error::Unsupported(Unsupported::Other("an lzip stream of a version other than 1")));
        }

        let dict = dictionary_size(header[5])?;
        let props = Properties { lc: LC, lp: LP, pb: PB, dict_size: dict };

        self.stage = Stage::Member(Box::new(LzmaDecoder::new(source, props, None)?), crate::utils::crc32::Crc32::new(), 0);
        Ok(())
    }

    fn finish_member(&mut self, decoder: LzmaDecoder<Pending<R>>, crc: crate::utils::crc32::Crc32, produced: u64) -> Result<()> {
        let (mut source, leftover) = decoder.into_parts();
        source.put_back(leftover);

        let mut trailer = [0u8; TRAILER_LEN];
        if !source.exactly(&mut trailer)? {
            return Err(Error::malformed("lzip member has no trailer"));
        }

        let stored_crc = u32::from_le_bytes([trailer[0], trailer[1], trailer[2], trailer[3]]);
        let stored_size = u64::from_le_bytes(trailer[4..12].try_into().expect("eight bytes"));

        let computed = crc.finish();
        if computed != stored_crc {
            return Err(Error::ChecksumMismatch { entry: "lzip member".to_owned(), expected: stored_crc, found: computed });
        }
        if produced != stored_size {
            return Err(Error::SizeMismatch { entry: "lzip member".to_owned(), expected: stored_size, found: produced });
        }

        self.start_member(source)
    }
}

impl<R: Read> Read for Reader<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }

        loop {
            match std::mem::replace(&mut self.stage, Stage::Done) {
                Stage::Done => return Ok(0),

                Stage::Between(source) => self.start_member(source)?,

                Stage::Member(mut decoder, crc, produced) => {
                    let n = decoder.read(buf)?;
                    if n > 0 {
                        let mut crc = crc;
                        crc.update(&buf[..n]);
                        self.stage = Stage::Member(decoder, crc, produced + n as u64);
                        return Ok(n);
                    }
                    self.finish_member(*decoder, crc, produced)?;
                }
            }
        }
    }
}

/// An lzip stream written as its input arrives.
///
/// One member, whose trailer records the CRC and length once the input ends.
/// Memory is one dictionary, not one archive.
pub struct Writer<W: Write> {
    coder: RangeEncoder<CountingWriter<W>>,
    encoder: Encoder,
    finder: Finder,
    window: Sliding,
    crc: crate::utils::crc32::Crc32,
    props: Properties,
    at: usize,
    header_len: u64,
}

impl<W: Write> Writer<W> {
    /// Start a member with a dictionary sized for `level`.
    pub fn new(mut out: W, depth: usize, level: crate::codecs::Level) -> Result<Self> {
        let dict = crate::codecs::lzma::encode::dictionary_at(usize::MAX, level).clamp(1 << 12, 1 << 29);
        let code = dictionary_code(dict);
        let dict = 1u32 << code;

        out.write_all(&MAGIC)?;
        out.write_all(&[1, code])?;
        let out = CountingWriter::new(out, 0);

        let props = Properties { lc: LC, lp: LP, pb: PB, dict_size: dict };
        let window = dict as usize;

        Ok(Writer {
            coder: RangeEncoder::new(out),
            encoder: Encoder::new(props),
            finder: Finder::new(usize::MAX, window, depth),
            window: Sliding::new(window + MATCH_MAX_LEN as usize),
            crc: crate::utils::crc32::Crc32::new(),
            props,
            at: 0,
            header_len: HEADER_LEN as u64,
        })
    }

    /// Hand over more input, encoding whatever has become complete.
    pub fn push(&mut self, bytes: &[u8]) -> Result<()> {
        self.crc.update(bytes);
        self.window.push(bytes);
        self.drain(false)
    }

    /// Encode what is left, write the trailer and give back the writer.
    pub fn finish(mut self) -> Result<W> {
        self.drain(true)?;

        let at = self.at;
        self.encoder.encode_end_marker(at, &mut self.coder)?;

        let crc = self.crc.finish();
        let header_len = self.header_len;

        let counted = self.coder.finish()?;
        let member_size = header_len + counted.offset() + TRAILER_LEN as u64;
        let mut out = counted.into_inner();

        out.write_all(&crc.to_le_bytes())?;
        out.write_all(&(at as u64).to_le_bytes())?;
        out.write_all(&member_size.to_le_bytes())?;
        out.flush()?;
        Ok(out)
    }

    fn drain(&mut self, last: bool) -> Result<()> {
        const CHUNK: usize = 1 << 15;
        let reserve = if last { 0 } else { MATCH_MAX_LEN as usize + 1 };

        loop {
            let usable = (self.window.end() - self.at).saturating_sub(reserve);
            if usable == 0 || (!last && usable < CHUNK) {
                break;
            }

            let end = self.at + usable.min(CHUNK);
            let Writer { coder, encoder, finder, window, at, .. } = self;
            encoder.encode_span(&window.feed(), *at, end, finder, coder)?;
            *at = end;
        }

        self.window.retain(self.at, self.props.dict_size as usize + MATCH_MAX_LEN as usize);
        Ok(())
    }
}

/// Encode `data` as a single lzip member.
pub fn compress_at(data: &[u8], depth: usize, level: crate::codecs::Level) -> Result<Vec<u8>> {
    let mut writer = Writer::new(Vec::with_capacity(data.len() / 2 + 64), depth, level)?;
    writer.push(data)?;
    writer.finish()
}

/// Decode every member of an lzip stream, concatenating their contents.
pub fn decompress(data: &[u8], size_hint: usize) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(size_hint.min(64 << 20));
    let mut at = 0usize;

    while at < data.len() {
        if data[at..].iter().all(|&b| b == 0) {
            break;
        }
        if at + HEADER_LEN > data.len() {
            return Err(Error::malformed("lzip member is too short to hold a header"));
        }
        if data[at..at + 4] != MAGIC {
            return Err(Error::malformed("lzip member does not start with the LZIP magic"));
        }

        let version = data[at + 4];
        if version != 1 {
            return Err(Error::Unsupported(Unsupported::Other("an lzip stream of a version other than 1")));
        }

        let dict = dictionary_size(data[at + 5])?;
        at += HEADER_LEN;

        let properties = Properties { lc: LC, lp: LP, pb: PB, dict_size: dict };
        let start = out.len();

        let mut decoder = LzmaDecoder::new(&data[at..], properties, None)?;
        decoder.read_to_end(&mut out)?;
        at += decoder.consumed();

        if at + TRAILER_LEN > data.len() {
            return Err(Error::malformed("lzip member has no trailer"));
        }

        let stored_crc = u32::from_le_bytes([data[at], data[at + 1], data[at + 2], data[at + 3]]);
        let stored_size = u64::from_le_bytes(data[at + 4..at + 12].try_into().expect("eight bytes"));
        at += TRAILER_LEN;

        let produced = &out[start..];
        let computed = crate::utils::crc32::checksum(produced);
        if computed != stored_crc {
            return Err(Error::ChecksumMismatch { entry: "lzip member".to_owned(), expected: stored_crc, found: computed });
        }
        if produced.len() as u64 != stored_size {
            return Err(Error::SizeMismatch { entry: "lzip member".to_owned(), expected: stored_size, found: produced.len() as u64 });
        }
    }

    Ok(out)
}
