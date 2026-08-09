use std::io::{Read, Write};

use crate::codecs::deflate::{DeflateEncoder, InflateReader};
use crate::codecs::{Encoder, Level};
use crate::utils::crc32::Crc32;
use crate::utils::error::{Error, Result, Unsupported};

pub const MAGIC: [u8; 2] = [0x1f, 0x8b];

const DEFLATE: u8 = 8;

const FTEXT: u8 = 1 << 0;
const FHCRC: u8 = 1 << 1;
const FEXTRA: u8 = 1 << 2;
const FNAME: u8 = 1 << 3;
const FCOMMENT: u8 = 1 << 4;
const RESERVED: u8 = 0xe0;

const OS_UNKNOWN: u8 = 255;

pub fn is_gzip(prefix: &[u8]) -> bool {
    prefix.len() >= 2 && prefix[..2] == MAGIC
}

#[derive(Debug, Clone, Default)]
pub struct Member {
    pub name: Option<Vec<u8>>,
    pub comment: Option<Vec<u8>>,
    pub extra: Option<Vec<u8>>,
    pub mtime: u32,
    pub text: bool,
}

struct Header {
    member: Member,
    len: usize,
}

fn parse_header(data: &[u8]) -> Result<Header> {
    if data.len() < 10 {
        return Err(Error::malformed("gzip member is too short to hold a header"));
    }
    if data[..2] != MAGIC {
        return Err(Error::malformed("gzip member does not start with 1f 8b"));
    }
    if data[2] != DEFLATE {
        return Err(Error::Unsupported(Unsupported::CompressionMethod(data[2] as u16)));
    }

    let flags = data[3];
    if flags & RESERVED != 0 {
        return Err(Error::malformed("gzip header sets reserved flag bits"));
    }

    let mtime = u32::from_le_bytes([data[4], data[5], data[6], data[7]]);
    let mut at = 10usize;

    let mut member = Member { mtime, text: flags & FTEXT != 0, ..Member::default() };

    if flags & FEXTRA != 0 {
        if at + 2 > data.len() {
            return Err(Error::malformed("gzip extra field length is truncated"));
        }
        let len = u16::from_le_bytes([data[at], data[at + 1]]) as usize;
        at += 2;
        if at + len > data.len() {
            return Err(Error::malformed("gzip extra field is truncated"));
        }
        member.extra = Some(data[at..at + len].to_vec());
        at += len;
    }

    if flags & FNAME != 0 {
        let (value, next) = take_cstring(data, at, "name")?;
        member.name = Some(value);
        at = next;
    }

    if flags & FCOMMENT != 0 {
        let (value, next) = take_cstring(data, at, "comment")?;
        member.comment = Some(value);
        at = next;
    }

    if flags & FHCRC != 0 {
        if at + 2 > data.len() {
            return Err(Error::malformed("gzip header crc is truncated"));
        }
        let stored = u16::from_le_bytes([data[at], data[at + 1]]);
        let computed = (crate::utils::crc32::checksum(&data[..at]) & 0xffff) as u16;
        if stored != computed {
            return Err(Error::malformed(format!("gzip header crc mismatch: stored {stored:#06x}, computed {computed:#06x}")));
        }
        at += 2;
    }

    Ok(Header { member, len: at })
}

/// The header of the first member, carrying the original name when one was stored.
pub fn read_member(data: &[u8]) -> Result<Member> {
    parse_header(data).map(|header| header.member)
}

fn take_cstring(data: &[u8], at: usize, what: &str) -> Result<(Vec<u8>, usize)> {
    let end = data[at..].iter().position(|&b| b == 0).ok_or_else(|| Error::malformed(format!("gzip {what} field is not terminated")))?;
    Ok((data[at..at + end].to_vec(), at + end + 1))
}

/// Decode every member of a gzip stream, concatenating their contents.
pub fn decompress(data: &[u8], size_hint: usize) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(size_hint);
    let mut at = 0usize;

    while at < data.len() {
        if data[at..].iter().all(|&b| b == 0) {
            break;
        }

        let header = parse_header(&data[at..])?;
        at += header.len;

        let start = out.len();
        let mut reader = InflateReader::new(&data[at..]);
        reader.read_to_end(&mut out)?;
        at += reader.consumed();

        if at + 8 > data.len() {
            return Err(Error::malformed("gzip member has no trailer"));
        }
        let stored_crc = u32::from_le_bytes([data[at], data[at + 1], data[at + 2], data[at + 3]]);
        let stored_len = u32::from_le_bytes([data[at + 4], data[at + 5], data[at + 6], data[at + 7]]);
        at += 8;

        let produced = &out[start..];
        let computed = crate::utils::crc32::checksum(produced);
        if computed != stored_crc {
            return Err(Error::ChecksumMismatch { entry: "gzip member".to_owned(), expected: stored_crc, found: computed });
        }
        if produced.len() as u32 != stored_len {
            return Err(Error::SizeMismatch { entry: "gzip member".to_owned(), expected: stored_len as u64, found: produced.len() as u64 });
        }
    }

    Ok(out)
}

pub fn compress(data: &[u8], level: Level, member: &Member) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    write_header(&mut out, member);

    let mut encoder = Box::new(DeflateEncoder::new(&mut out, level));
    encoder.write_all(data)?;
    encoder.finish()?;

    out.extend_from_slice(&crate::utils::crc32::checksum(data).to_le_bytes());
    out.extend_from_slice(&(data.len() as u32).to_le_bytes());
    Ok(out)
}

fn write_header(out: &mut Vec<u8>, member: &Member) {
    let mut flags = 0u8;
    if member.text {
        flags |= FTEXT;
    }
    if member.extra.is_some() {
        flags |= FEXTRA;
    }
    if member.name.is_some() {
        flags |= FNAME;
    }
    if member.comment.is_some() {
        flags |= FCOMMENT;
    }

    out.extend_from_slice(&MAGIC);
    out.push(DEFLATE);
    out.push(flags);
    out.extend_from_slice(&member.mtime.to_le_bytes());
    out.push(0);
    out.push(OS_UNKNOWN);

    if let Some(extra) = &member.extra {
        out.extend_from_slice(&(extra.len() as u16).to_le_bytes());
        out.extend_from_slice(extra);
    }
    if let Some(name) = &member.name {
        out.extend_from_slice(name);
        out.push(0);
    }
    if let Some(comment) = &member.comment {
        out.extend_from_slice(comment);
        out.push(0);
    }
}

/// Streaming gzip writer: deflate plus the header and CRC-32/ISIZE trailer.
pub struct GzipEncoder<W: Write> {
    inner: Option<DeflateEncoder<W>>,
    crc: Crc32,
    length: u64,
}

impl<W: Write> GzipEncoder<W> {
    pub fn new(mut out: W, level: Level) -> Result<Self> {
        let mut header = Vec::new();
        write_header(&mut header, &Member::default());
        out.write_all(&header)?;

        Ok(GzipEncoder { inner: Some(DeflateEncoder::new(out, level)), crc: Crc32::new(), length: 0 })
    }

    pub fn finish(mut self) -> Result<W> {
        let encoder = self.inner.take().expect("encoder is present until finish");
        let mut out = encoder.finish_inner()?;

        out.write_all(&self.crc.finish().to_le_bytes())?;
        out.write_all(&(self.length as u32).to_le_bytes())?;
        out.flush()?;
        Ok(out)
    }
}

impl<W: Write> Write for GzipEncoder<W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let encoder = self.inner.as_mut().expect("encoder is present until finish");
        encoder.write_all(buf)?;
        self.crc.update(buf);
        self.length += buf.len() as u64;
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

struct Feed<R> {
    pending: Vec<u8>,
    at: usize,
    inner: R,
}

impl<R: Read> Feed<R> {
    fn new(pending: Vec<u8>, inner: R) -> Self {
        Feed { pending, at: 0, inner }
    }

    fn into_parts(self) -> (R, Vec<u8>) {
        (self.inner, self.pending[self.at..].to_vec())
    }

    fn exact(&mut self, count: usize) -> std::io::Result<Option<Vec<u8>>> {
        let mut out = vec![0u8; count];
        let mut filled = 0usize;

        while filled < count {
            match self.read(&mut out[filled..])? {
                0 => break,
                n => filled += n,
            }
        }

        if filled == 0 {
            return Ok(None);
        }
        if filled < count {
            return Err(Error::malformed("gzip member ends mid-header").into());
        }
        Ok(Some(out))
    }

    fn byte(&mut self) -> std::io::Result<u8> {
        let mut one = [0u8; 1];
        match self.read(&mut one)? {
            0 => Err(Error::malformed("gzip header field is not terminated").into()),
            _ => Ok(one[0]),
        }
    }
}

impl<R: Read> Read for Feed<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.at < self.pending.len() {
            let n = (self.pending.len() - self.at).min(buf.len());
            buf[..n].copy_from_slice(&self.pending[self.at..self.at + n]);
            self.at += n;
            return Ok(n);
        }
        self.inner.read(buf)
    }
}

/// Streaming gzip reader across every member of the stream.
///
/// Nothing larger than one inflate window is held at a time, so an archive of
/// any size decodes in constant memory.
pub struct GzipReader<R> {
    stage: Stage<R>,
    crc: crate::utils::crc32::Crc32,
    produced: u32,
}

enum Stage<R> {
    Between(Option<(R, Vec<u8>)>),
    Member(Box<InflateReader<Feed<R>>>),
    Done,
}

impl<R: Read> GzipReader<R> {
    pub fn new(inner: R) -> Self {
        GzipReader { stage: Stage::Between(Some((inner, Vec::new()))), crc: crate::utils::crc32::Crc32::new(), produced: 0 }
    }

    fn start_member(&mut self, inner: R, pending: Vec<u8>) -> std::io::Result<bool> {
        let mut feed = Feed::new(pending, inner);

        let Some(fixed) = feed.exact(10)? else {
            self.stage = Stage::Done;
            return Ok(false);
        };

        if fixed.iter().all(|&b| b == 0) {
            self.stage = Stage::Done;
            return Ok(false);
        }
        if fixed[..2] != MAGIC {
            return Err(Error::malformed("gzip member does not start with 1f 8b").into());
        }
        if fixed[2] != DEFLATE {
            return Err(Error::Unsupported(Unsupported::CompressionMethod(fixed[2] as u16)).into());
        }

        let flags = fixed[3];
        if flags & RESERVED != 0 {
            return Err(Error::malformed("gzip header sets reserved flag bits").into());
        }

        if flags & FEXTRA != 0 {
            let len = feed.exact(2)?.ok_or_else(|| Error::malformed("gzip extra field has no length"))?;
            let len = u16::from_le_bytes([len[0], len[1]]) as usize;
            feed.exact(len)?.ok_or_else(|| Error::malformed("gzip extra field runs past the member"))?;
        }
        for present in [flags & FNAME != 0, flags & FCOMMENT != 0] {
            if present {
                while feed.byte()? != 0 {}
            }
        }
        if flags & FHCRC != 0 {
            feed.exact(2)?.ok_or_else(|| Error::malformed("gzip header checksum is truncated"))?;
        }

        self.crc = crate::utils::crc32::Crc32::new();
        self.produced = 0;
        self.stage = Stage::Member(Box::new(InflateReader::new(feed)));
        Ok(true)
    }

    fn finish_member(&mut self, reader: InflateReader<Feed<R>>) -> std::io::Result<()> {
        let (feed, rest) = reader.into_parts();
        let (inner, unread) = feed.into_parts();

        let mut leftover = rest;
        leftover.extend_from_slice(&unread);

        let mut feed = Feed::new(leftover, inner);
        let trailer = feed.exact(8)?.ok_or_else(|| Error::malformed("gzip member has no trailer"))?;

        let stored_crc = u32::from_le_bytes([trailer[0], trailer[1], trailer[2], trailer[3]]);
        let stored_len = u32::from_le_bytes([trailer[4], trailer[5], trailer[6], trailer[7]]);

        let computed = self.crc.finish();
        if computed != stored_crc {
            return Err(Error::ChecksumMismatch { entry: "gzip member".to_owned(), expected: stored_crc, found: computed }.into());
        }
        if self.produced != stored_len {
            return Err(Error::SizeMismatch { entry: "gzip member".to_owned(), expected: stored_len as u64, found: self.produced as u64 }.into());
        }

        let (inner, pending) = feed.into_parts();
        self.stage = Stage::Between(Some((inner, pending)));
        Ok(())
    }
}

impl<R: Read> Read for GzipReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }

        loop {
            match &mut self.stage {
                Stage::Done => return Ok(0),

                Stage::Between(slot) => {
                    let (inner, pending) = slot.take().expect("a stream between members always has its reader");
                    if !self.start_member(inner, pending)? {
                        return Ok(0);
                    }
                }

                Stage::Member(reader) => {
                    let n = reader.read(buf)?;
                    if n > 0 {
                        self.crc.update(&buf[..n]);
                        self.produced = self.produced.wrapping_add(n as u32);
                        return Ok(n);
                    }

                    let Stage::Member(reader) = std::mem::replace(&mut self.stage, Stage::Done) else { unreachable!("the stage was just matched as a member") };
                    self.finish_member(*reader)?;
                }
            }
        }
    }
}
