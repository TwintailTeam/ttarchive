use std::io::Write;

use crate::codecs::lzma::decode::Properties;
use crate::codecs::lzma::lzma2;
use crate::utils::crc64::Crc64;
use crate::utils::error::Result;

pub const MAGIC: [u8; 6] = [0xfd, b'7', b'z', b'X', b'Z', 0x00];
const FOOTER_MAGIC: [u8; 2] = [b'Y', b'Z'];

const CHECK_CRC64: u8 = 0x04;
const CHECK_LEN: usize = 8;

const FILTER_LZMA2: u64 = 0x21;

fn put_varint(out: &mut Vec<u8>, mut value: u64) {
    while value >= 0x80 {
        out.push((value as u8) | 0x80);
        value >>= 7;
    }
    out.push(value as u8);
}

fn pad_to_four(out: &mut Vec<u8>, from: usize) {
    while !(out.len() - from).is_multiple_of(4) {
        out.push(0);
    }
}

fn crc32_of(bytes: &[u8]) -> [u8; 4] {
    crate::utils::crc32::checksum(bytes).to_le_bytes()
}

fn stream_flags() -> [u8; 2] {
    [0x00, CHECK_CRC64]
}

fn write_stream_header(out: &mut Vec<u8>) {
    out.extend_from_slice(&MAGIC);
    let flags = stream_flags();
    out.extend_from_slice(&flags);
    out.extend_from_slice(&crc32_of(&flags));
}

fn block_header(dict_code: u8) -> Vec<u8> {
    let mut body = Vec::new();
    body.push(0x00);
    put_varint(&mut body, FILTER_LZMA2);
    put_varint(&mut body, 1);
    body.push(dict_code);

    let size = (1 + body.len() + 4).div_ceil(4) * 4;

    let mut header = Vec::with_capacity(size);
    header.push((size / 4 - 1) as u8);
    header.extend_from_slice(&body);
    while header.len() < size - 4 {
        header.push(0);
    }

    let crc = crc32_of(&header);
    header.extend_from_slice(&crc);
    header
}

/// Compress into a single-block xz stream with a CRC-64 check.
pub fn compress(data: &[u8], props: Properties, depth: usize) -> Result<Vec<u8>> {
    let payload = lzma2::compress(data, props, depth)?;
    let dict_code = lzma2::dictionary_code(props.dict_size);

    let mut out = Vec::with_capacity(payload.len() + 128);
    write_stream_header(&mut out);

    let header = block_header(dict_code);
    let header_len = header.len();
    out.extend_from_slice(&header);

    let block_start = out.len();
    out.extend_from_slice(&payload);

    let unpadded = header_len + (out.len() - block_start) + CHECK_LEN;
    pad_to_four(&mut out, block_start);

    let mut check = Crc64::new();
    check.update(data);
    out.extend_from_slice(&check.finish().to_le_bytes());

    let index_start = out.len();
    out.push(0x00);
    put_varint(&mut out, 1);
    put_varint(&mut out, unpadded as u64);
    put_varint(&mut out, data.len() as u64);
    pad_to_four(&mut out, index_start);

    let index_crc = crc32_of(&out[index_start..]);
    out.extend_from_slice(&index_crc);
    let index_len = out.len() - index_start;

    let backward = (index_len / 4 - 1) as u32;
    let flags = stream_flags();

    let mut footer = Vec::with_capacity(6);
    footer.extend_from_slice(&backward.to_le_bytes());
    footer.extend_from_slice(&flags);

    out.extend_from_slice(&crc32_of(&footer));
    out.extend_from_slice(&footer);
    out.extend_from_slice(&FOOTER_MAGIC);

    Ok(out)
}

struct Counted<W> {
    inner: W,
    written: u64,
}

impl<W: Write> Write for Counted<W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let n = self.inner.write(buf)?;
        self.written += n as u64;
        Ok(n)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

/// A single-block xz stream written as its input arrives.
///
/// The block header leaves out the optional size fields, so nothing has to be
/// known in advance; the index and footer are written from running totals once
/// the input ends. One block keeps matches reaching across the whole stream,
/// which several blocks would not.
pub struct Writer<W: Write> {
    inner: lzma2::Writer<Counted<W>>,
    check: Crc64,
    uncompressed: u64,
    header_len: usize,
}

impl<W: Write> Writer<W> {
    /// Start a stream with a dictionary sized for `level`.
    pub fn new(mut out: W, depth: usize, level: crate::codecs::Level) -> Result<Self> {
        let dict = crate::codecs::lzma::encode::dictionary_at(usize::MAX, level);
        let props = Properties { lc: 3, lp: 0, pb: 2, dict_size: dict };

        let mut head = Vec::with_capacity(64);
        write_stream_header(&mut head);
        let header = block_header(lzma2::dictionary_code(dict));
        let header_len = header.len();
        head.extend_from_slice(&header);
        out.write_all(&head)?;

        let counted = Counted { inner: out, written: 0 };
        Ok(Writer { inner: lzma2::Writer::new(counted, props, depth, usize::MAX), check: Crc64::new(), uncompressed: 0, header_len })
    }

    /// Hand over more input, encoding whatever has become complete.
    pub fn push(&mut self, bytes: &[u8]) -> Result<()> {
        self.check.update(bytes);
        self.uncompressed += bytes.len() as u64;
        self.inner.push(bytes)
    }

    /// Close the block, write the index and footer, and give back the writer.
    pub fn finish(self) -> Result<W> {
        let Writer { inner, check, uncompressed, header_len } = self;

        let counted = inner.finish()?;
        let payload = counted.written;
        let mut out = counted.inner;

        let unpadded = header_len as u64 + payload + CHECK_LEN as u64;

        let mut tail = Vec::with_capacity(64);
        while !(payload as usize + tail.len()).is_multiple_of(4) {
            tail.push(0);
        }
        tail.extend_from_slice(&check.finish().to_le_bytes());

        let index_start = tail.len();
        tail.push(0x00);
        put_varint(&mut tail, 1);
        put_varint(&mut tail, unpadded);
        put_varint(&mut tail, uncompressed);
        pad_to_four(&mut tail, index_start);

        let index_crc = crc32_of(&tail[index_start..]);
        tail.extend_from_slice(&index_crc);
        let index_len = tail.len() - index_start;

        let backward = (index_len / 4 - 1) as u32;
        let flags = stream_flags();

        let mut footer = Vec::with_capacity(6);
        footer.extend_from_slice(&backward.to_le_bytes());
        footer.extend_from_slice(&flags);

        tail.extend_from_slice(&crc32_of(&footer));
        tail.extend_from_slice(&footer);
        tail.extend_from_slice(&FOOTER_MAGIC);

        out.write_all(&tail)?;
        out.flush()?;
        Ok(out)
    }
}

/// Compress with the crate's default LZMA properties.
pub fn compress_default(data: &[u8], depth: usize) -> Result<Vec<u8>> {
    compress_at(data, depth, crate::codecs::Level::Default)
}

/// Compress with a dictionary sized for `level`.
pub fn compress_at(data: &[u8], depth: usize, level: crate::codecs::Level) -> Result<Vec<u8>> {
    let dict = crate::codecs::lzma::encode::dictionary_at(data.len(), level);
    let props = Properties { lc: 3, lp: 0, pb: 2, dict_size: dict };
    compress(data, props, depth)
}
