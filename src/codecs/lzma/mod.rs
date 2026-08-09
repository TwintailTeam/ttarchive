pub mod alone;
pub mod decode;
pub mod encode;
pub mod lzma2;
pub mod range;
pub mod window;

use std::io::Read;

use crate::utils::error::{Error, Result};

pub use decode::{LzmaDecoder, Properties};

const HEADER_LEN: usize = 4;

pub fn reader<R: Read>(mut input: R, expected: Option<u64>) -> Result<LzmaDecoder<R>> {
    let mut header = [0u8; HEADER_LEN];
    input.read_exact(&mut header).map_err(|e| {
        if e.kind() == std::io::ErrorKind::UnexpectedEof { Error::malformed("lzma entry is too short to hold its header") } else { Error::Io(e) }
    })?;

    let property_len = u16::from_le_bytes([header[2], header[3]]) as usize;
    if property_len != 5 {
        return Err(Error::malformed(format!("lzma entry declares {property_len} property bytes; the format has exactly 5")));
    }

    let mut properties = [0u8; 5];
    input.read_exact(&mut properties)?;

    LzmaDecoder::new(input, Properties::from_bytes(properties)?, expected)
}

pub fn decompress(data: &[u8], expected: Option<u64>) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(expected.unwrap_or(0).min(64 << 20) as usize);
    reader(data, expected)?.read_to_end(&mut out)?;
    Ok(out)
}
