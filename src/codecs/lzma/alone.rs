use std::io::{Read, Write};

use crate::codecs::lzma::decode::{LzmaDecoder, Properties};
use crate::codecs::lzma::encode;
use crate::utils::error::{Error, Result};

pub const HEADER_LEN: usize = 13;

const UNKNOWN_SIZE: u64 = u64::MAX;

/// A bare `.lzma` stream: five property bytes then an eight byte size.
///
/// This is the LZMA_Alone container, not the four byte framing a ZIP entry puts
/// in front of the same property bytes.
pub fn reader<R: Read>(mut input: R) -> Result<LzmaDecoder<R>> {
    let mut header = [0u8; HEADER_LEN];
    input.read_exact(&mut header).map_err(|e| {
        if e.kind() == std::io::ErrorKind::UnexpectedEof { Error::malformed("lzma stream is too short to hold its header") } else { Error::Io(e) }
    })?;

    let properties = Properties::from_bytes([header[0], header[1], header[2], header[3], header[4]])?;

    let size = u64::from_le_bytes([header[5], header[6], header[7], header[8], header[9], header[10], header[11], header[12]]);
    let expected = if size == UNKNOWN_SIZE { None } else { Some(size) };

    LzmaDecoder::new(input, properties, expected)
}

pub fn decompress(data: &[u8], size_hint: usize) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(size_hint.min(64 << 20));
    reader(data)?.read_to_end(&mut out)?;
    Ok(out)
}

pub fn is_alone(prefix: &[u8]) -> bool {
    if prefix.len() < 5 {
        return false;
    }
    if prefix[0] >= 9 * 5 * 5 {
        return false;
    }
    let dict = u32::from_le_bytes([prefix[1], prefix[2], prefix[3], prefix[4]]);
    (1 << 12..=1 << 30).contains(&dict)
}

const DEFAULT_PROPS: Properties = Properties { lc: 3, lp: 0, pb: 2, dict_size: 1 << 23 };

/// Write a `.lzma` (LZMA_Alone) stream: five property bytes, an eight byte size,
/// then the raw stream.
///
/// The dictionary size written is the one actually used, after clamping, so a
/// decoder sizes its window the same way this encoder did.
pub fn compress(data: &[u8], depth: usize) -> Result<Vec<u8>> {
    compress_at(data, depth, crate::codecs::Level::Default)
}

/// A `.lzma` stream written as its input arrives.
///
/// The size field says unknown and the stream ends with a marker, because the
/// length is only known once the last byte has gone by. Memory is one
/// dictionary, not one archive.
pub struct Writer<W: Write> {
    coder: encode::RangeEncoder<W>,
    encoder: encode::Encoder,
    finder: encode::Finder,
    window: encode::Sliding,
    props: Properties,
    at: usize,
}

impl<W: Write> Writer<W> {
    /// Start a stream with a dictionary sized for `level`.
    pub fn new(mut out: W, depth: usize, level: crate::codecs::Level) -> Result<Self> {
        let dict = encode::dictionary_at(usize::MAX, level);
        let props = Properties::from_byte(encode::properties_byte(DEFAULT_PROPS), dict)?;

        out.write_all(&[encode::properties_byte(props)])?;
        out.write_all(&props.dict_size.to_le_bytes())?;
        out.write_all(&UNKNOWN_SIZE.to_le_bytes())?;

        let window = dict as usize;

        Ok(Writer {
            coder: encode::RangeEncoder::new(out),
            encoder: encode::Encoder::new(props),
            finder: encode::Finder::new(usize::MAX, window, depth),
            window: encode::Sliding::new(window + encode::MATCH_MAX_LEN as usize),
            props,
            at: 0,
        })
    }

    /// Hand over more input, encoding whatever has become complete.
    pub fn push(&mut self, bytes: &[u8]) -> Result<()> {
        self.window.push(bytes);
        self.drain(false)
    }

    /// Encode what is left, mark the end and give back the writer.
    pub fn finish(mut self) -> Result<W> {
        self.drain(true)?;
        let at = self.at;
        self.encoder.encode_end_marker(at, &mut self.coder)?;
        self.coder.finish()
    }

    fn drain(&mut self, last: bool) -> Result<()> {
        const CHUNK: usize = 1 << 15;
        let reserve = if last { 0 } else { encode::MATCH_MAX_LEN as usize + 1 };

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

        self.window.retain(self.at, self.props.dict_size as usize + encode::MATCH_MAX_LEN as usize);
        Ok(())
    }
}

/// Compress with a dictionary sized for `level`.
pub fn compress_at(data: &[u8], depth: usize, level: crate::codecs::Level) -> Result<Vec<u8>> {
    let dict = encode::dictionary_at(data.len(), level);
    let properties = Properties::from_byte(encode::properties_byte(DEFAULT_PROPS), dict)?;

    let mut out = Vec::with_capacity(data.len() / 2 + 64);
    out.push(encode::properties_byte(properties));
    out.extend_from_slice(&properties.dict_size.to_le_bytes());
    out.extend_from_slice(&(data.len() as u64).to_le_bytes());

    let body = encode::compress_raw(data, properties, depth)?;
    out.extend_from_slice(&body);
    Ok(out)
}
