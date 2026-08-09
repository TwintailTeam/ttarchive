pub mod bits;
pub mod encode;
pub mod fse;
pub mod huffman;
pub mod sequences;

use std::io::Read;

use crate::codecs::lzma::window::Window;
use crate::utils::error::{Error, Result, Unsupported};
use crate::utils::xxhash::XxHash64;

const MAGIC: u32 = 0xFD2F_B528;
const SKIPPABLE_MAGIC_LOW: u32 = 0x184D_2A50;
const SKIPPABLE_MAGIC_HIGH: u32 = 0x184D_2A5F;

const MAX_WINDOW: u64 = 1 << 31;
const MAX_BLOCK: usize = 128 * 1024;

struct FrameHeader {
    window_size: u64,
    content_size: Option<u64>,
    has_checksum: bool,
}

impl FrameHeader {
    fn length_of(descriptor: u8) -> usize {
        let content_size_flag = descriptor >> 6;
        let single_segment = descriptor & 0x20 != 0;

        let dictionary_bytes = match descriptor & 0x03 {
            0 => 0,
            1 => 1,
            2 => 2,
            _ => 4,
        };

        let content_bytes = match content_size_flag {
            0 if single_segment => 1,
            0 => 0,
            1 => 2,
            2 => 4,
            _ => 8,
        };

        1 + usize::from(!single_segment) + dictionary_bytes + content_bytes
    }

    fn parse(data: &[u8]) -> Result<Self> {
        let descriptor = *data.first().ok_or_else(|| Error::malformed("zstd frame header is empty"))?;

        let content_size_flag = descriptor >> 6;
        let single_segment = descriptor & 0x20 != 0;
        let has_checksum = descriptor & 0x04 != 0;
        let dictionary_flag = descriptor & 0x03;

        if descriptor & 0x08 != 0 {
            return Err(Error::malformed("zstd frame header uses a reserved bit"));
        }

        let mut offset = 1usize;

        let window_size = if single_segment {
            0
        } else {
            let byte = *data.get(offset).ok_or_else(|| Error::malformed("zstd frame header is truncated"))?;
            offset += 1;
            let exponent = (byte >> 3) as u64;
            let mantissa = (byte & 0x07) as u64;
            let base = 1u64 << (10 + exponent);
            base + (base / 8) * mantissa
        };

        let dictionary_bytes = match dictionary_flag {
            0 => 0,
            1 => 1,
            2 => 2,
            _ => 4,
        };
        if dictionary_bytes > 0 {
            let present = data.get(offset..offset + dictionary_bytes).ok_or_else(|| Error::malformed("zstd frame header is truncated"))?;
            if present.iter().any(|&b| b != 0) {
                return Err(Error::Unsupported(Unsupported::Other("a zstd frame that requires an external dictionary")));
            }
            offset += dictionary_bytes;
        }

        let content_bytes = match content_size_flag {
            0 if single_segment => 1,
            0 => 0,
            1 => 2,
            2 => 4,
            _ => 8,
        };
        let content_size = if content_bytes == 0 {
            None
        } else {
            let field = data.get(offset..offset + content_bytes).ok_or_else(|| Error::malformed("zstd frame header is truncated"))?;
            let mut value = 0u64;
            for (i, &byte) in field.iter().enumerate() {
                value |= (byte as u64) << (8 * i);
            }
            Some(if content_bytes == 2 { value + 256 } else { value })
        };

        let window_size = if single_segment { content_size.unwrap_or(0) } else { window_size };
        if window_size > MAX_WINDOW {
            return Err(Error::malformed(format!("zstd frame declares a {window_size} byte window")));
        }

        Ok(FrameHeader { window_size, content_size, has_checksum })
    }
}

#[derive(Default)]
struct Frame {
    literals: Option<huffman::Table>,
    tables: sequences::Tables,
    repeats: [u32; 3],
}

/// Decode every frame in a Zstandard stream.
pub fn decompress(data: &[u8], size_hint: usize) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(size_hint.min(64 << 20));
    Reader::new(data, size_hint as u64).read_to_end(&mut out)?;
    Ok(out)
}

struct Open {
    window_size: u64,
    content_size: Option<u64>,
    has_checksum: bool,
    start: u64,
    hashed: u64,
    digest: XxHash64,
    frame: Frame,
}

/// A Zstandard stream decoded as it is read.
///
/// Only the frame's declared window is held, so a stream decodes in bounded
/// memory however long it is. Frames that follow one another are all decoded,
/// which is what `zstd` itself produces when its output is concatenated.
pub struct Reader<R> {
    inner: R,
    window: Window,
    open: Option<Open>,
    finished: bool,
}

impl<R: Read> Reader<R> {
    /// Wrap `inner`. The hint only sizes the first allocation.
    pub fn new(inner: R, _size_hint: u64) -> Self {
        Reader { inner, window: Window::new(1 << 20), open: None, finished: false }
    }

    fn exactly(&mut self, buf: &mut [u8]) -> Result<bool> {
        let mut filled = 0usize;
        while filled < buf.len() {
            match self.inner.read(&mut buf[filled..]) {
                Ok(0) if filled == 0 => return Ok(false),
                Ok(0) => return Err(Error::malformed("zstd stream ends in the middle of a field")),
                Ok(n) => filled += n,
                Err(ref e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(e) => return Err(Error::from(e)),
            }
        }
        Ok(true)
    }

    fn step(&mut self) -> Result<()> {
        match self.open.is_some() {
            false => self.start_frame(),
            true => self.block(),
        }
    }

    fn start_frame(&mut self) -> Result<()> {
        let mut magic = [0u8; 4];
        if !self.exactly(&mut magic)? {
            self.finished = true;
            return Ok(());
        }
        let magic = u32::from_le_bytes(magic);

        if (SKIPPABLE_MAGIC_LOW..=SKIPPABLE_MAGIC_HIGH).contains(&magic) {
            let mut size = [0u8; 4];
            self.exactly(&mut size)?;
            let size = u32::from_le_bytes(size) as u64;
            std::io::copy(&mut (&mut self.inner).take(size), &mut std::io::sink())?;
            return Ok(());
        }

        if magic != MAGIC {
            return Err(Error::malformed(format!("not a zstd frame: magic {magic:#010x}")));
        }

        let mut descriptor = [0u8; 1];
        self.exactly(&mut descriptor)?;
        let mut header = vec![0u8; FrameHeader::length_of(descriptor[0])];
        header[0] = descriptor[0];
        if header.len() > 1 {
            self.exactly(&mut header[1..])?;
        }

        let parsed = FrameHeader::parse(&header)?;
        let window_size = if parsed.window_size == 0 { 1 << 20 } else { parsed.window_size };

        self.window.reset_dictionary();
        self.window.set_dictionary_size(window_size.min(MAX_WINDOW) as usize);

        self.open = Some(Open {
            window_size: parsed.window_size,
            content_size: parsed.content_size,
            has_checksum: parsed.has_checksum,
            start: self.window.total(),
            hashed: self.window.total(),
            digest: XxHash64::new(0),
            frame: Frame { repeats: [1, 4, 8], ..Default::default() },
        });
        Ok(())
    }

    fn block(&mut self) -> Result<()> {
        let mut head = [0u8; 3];
        self.exactly(&mut head)?;
        let value = head[0] as u32 | ((head[1] as u32) << 8) | ((head[2] as u32) << 16);

        let last = value & 1 != 0;
        let kind = (value >> 1) & 0x3;
        let size = (value >> 3) as usize;

        let stored = if kind == 1 { 1 } else { size };
        if stored > MAX_BLOCK {
            return Err(Error::malformed(format!("zstd block claims {stored} bytes, past the format's limit")));
        }
        let mut body = vec![0u8; stored];
        if stored > 0 {
            self.exactly(&mut body)?;
        }

        let open = self.open.as_mut().expect("a frame is open");
        let window_size = open.window_size;

        match kind {
            0 => self.window.extend(&body),
            1 => {
                let byte = *body.first().ok_or_else(|| Error::malformed("zstd RLE block has no byte"))?;
                for _ in 0..size {
                    self.window.push(byte);
                }
            }
            2 => decode_compressed_block(&body, &mut self.window, &mut open.frame, window_size)?,
            _ => return Err(Error::malformed("zstd block uses the reserved type 3")),
        }

        let open = self.open.as_mut().expect("a frame is open");
        if open.has_checksum {
            open.digest.update(self.window.since(open.hashed));
            open.hashed = self.window.total();
        }

        if !last {
            return Ok(());
        }

        let produced = self.window.total() - open.start;
        if let Some(want) = open.content_size
            && produced != want
        {
            return Err(Error::malformed(format!("zstd frame produced {produced} bytes, not the {want} its header declared")));
        }

        let expected = open.has_checksum.then(|| open.digest.finish() as u32);
        self.open = None;

        if let Some(expected) = expected {
            let mut stored = [0u8; 4];
            self.exactly(&mut stored)?;
            if expected != u32::from_le_bytes(stored) {
                return Err(Error::malformed("zstd frame failed its content checksum"));
            }
        }

        Ok(())
    }
}

impl<R: Read> Read for Reader<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        while self.window.pending() == 0 && !self.finished {
            self.window.drain();
            self.step()?;
        }
        Ok(self.window.take(buf))
    }
}

fn decode_compressed_block(body: &[u8], out: &mut Window, frame: &mut Frame, window_size: u64) -> Result<()> {
    let (literals, used) = decode_literals(body, frame)?;
    let mut target = sequences::Target { out, window_size };
    sequences::execute(&body[used..], &literals, &mut target, &mut frame.tables, &mut frame.repeats)
}

fn decode_literals(body: &[u8], frame: &mut Frame) -> Result<(Vec<u8>, usize)> {
    let first = *body.first().ok_or_else(|| Error::malformed("zstd literals section is empty"))?;
    let kind = first & 0x3;
    let format = (first >> 2) & 0x3;

    match kind {
        0 | 1 => {
            let (regenerated, header_len) = match format {
                0 | 2 => ((first >> 3) as usize, 1),
                1 => {
                    let second = *body.get(1).ok_or_else(|| Error::malformed("zstd literals header is truncated"))?;
                    (((first >> 4) as usize) | ((second as usize) << 4), 2)
                }
                _ => {
                    let second = *body.get(1).ok_or_else(|| Error::malformed("zstd literals header is truncated"))?;
                    let third = *body.get(2).ok_or_else(|| Error::malformed("zstd literals header is truncated"))?;
                    (((first >> 4) as usize) | ((second as usize) << 4) | ((third as usize) << 12), 3)
                }
            };

            if kind == 0 {
                let data = body.get(header_len..header_len + regenerated).ok_or_else(|| Error::malformed("zstd raw literals run past the section"))?;
                Ok((data.to_vec(), header_len + regenerated))
            } else {
                let byte = *body.get(header_len).ok_or_else(|| Error::malformed("zstd RLE literals have no byte"))?;
                Ok((vec![byte; regenerated], header_len + 1))
            }
        }

        _ => {
            let treeless = kind == 3;
            let (regenerated, compressed, streams, header_len) = parse_compressed_literals_header(body, format)?;

            let section = body.get(header_len..header_len + compressed).ok_or_else(|| Error::malformed("zstd compressed literals run past the section"))?;

            let table_len = if treeless {
                0
            } else {
                let (table, used) = huffman::Table::parse(section)?;
                frame.literals = Some(table);
                used
            };

            let table = frame.literals.as_ref().ok_or_else(|| Error::malformed("zstd treeless literals reuse a table that was never sent"))?;

            let payload = &section[table_len..];
            let out =
                if streams == 1 { decode_literal_stream(payload, table, regenerated)? } else { decode_four_literal_streams(payload, table, regenerated)? };

            Ok((out, header_len + compressed))
        }
    }
}

fn parse_compressed_literals_header(body: &[u8], format: u8) -> Result<(usize, usize, usize, usize)> {
    let need = match format {
        0 | 1 => 3,
        2 => 4,
        _ => 5,
    };
    let head = body.get(..need).ok_or_else(|| Error::malformed("zstd literals header is truncated"))?;

    let value = head.iter().enumerate().fold(0u64, |acc, (i, &b)| acc | ((b as u64) << (8 * i)));
    let streams = if format == 0 { 1 } else { 4 };

    let (regenerated, compressed) = match format {
        0 | 1 => (((value >> 4) & 0x3FF) as usize, ((value >> 14) & 0x3FF) as usize),
        2 => (((value >> 4) & 0x3FFF) as usize, ((value >> 18) & 0x3FFF) as usize),
        _ => (((value >> 4) & 0x3FFFF) as usize, ((value >> 22) & 0x3FFFF) as usize),
    };

    Ok((regenerated, compressed, streams, need))
}

fn decode_literal_stream(data: &[u8], table: &huffman::Table, count: usize) -> Result<Vec<u8>> {
    let mut bits = bits::BackwardBits::new(data)?;
    let mut out = Vec::with_capacity(count);
    for _ in 0..count {
        out.push(table.decode(&mut bits));
    }
    Ok(out)
}

fn decode_four_literal_streams(data: &[u8], table: &huffman::Table, count: usize) -> Result<Vec<u8>> {
    let jump = data.get(..6).ok_or_else(|| Error::malformed("zstd four-stream literals have no jump table"))?;
    let first = u16::from_le_bytes([jump[0], jump[1]]) as usize;
    let second = u16::from_le_bytes([jump[2], jump[3]]) as usize;
    let third = u16::from_le_bytes([jump[4], jump[5]]) as usize;

    let body = &data[6..];
    let fourth = body.len().checked_sub(first + second + third).ok_or_else(|| Error::malformed("zstd literal stream sizes exceed the section"))?;

    let per_stream = count.div_ceil(4);
    let mut out = Vec::with_capacity(count);
    let mut offset = 0usize;

    for (index, size) in [first, second, third, fourth].into_iter().enumerate() {
        let slice = body.get(offset..offset + size).ok_or_else(|| Error::malformed("zstd literal stream runs past the section"))?;
        offset += size;

        let wanted = if index == 3 { count - per_stream * 3 } else { per_stream };
        out.extend_from_slice(&decode_literal_stream(slice, table, wanted)?);
    }

    Ok(out)
}
