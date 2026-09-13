pub mod bcj;
pub mod bzip2;
pub mod compress;
pub mod deflate;
pub mod gzip;
pub mod legacy;
pub(crate) mod lengths;
pub mod lzip;
pub mod lzma;
pub mod ppmd;
pub(crate) mod sliding;
pub mod store;
pub mod xz;
pub mod zstd;

use std::io::{Read, Write};

use crate::utils::error::{Error, Result, Unsupported};
use crate::utils::limits;
use crate::zip::spec::version;

/// A compression method, named by its ZIP method number.
///
/// This build reads all of them. [`Method::can_encode`] says which it can also
/// write.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum Method {
    /// Stored with no compression at all.
    Store,
    /// PKWARE Shrink. Read only, and long obsolete.
    Shrink,
    /// PKWARE Reduce, at one of four factors. Read only, and long obsolete.
    Reduce(u8),
    /// PKWARE Implode. Read only, and long obsolete.
    Implode,
    /// Deflate, which almost every ZIP uses.
    #[default]
    Deflate,
    /// Deflate64. Read only.
    Deflate64,
    /// bzip2.
    Bzip2,
    /// Raw LZMA. Read only.
    Lzma,
    /// Zstandard. Read only.
    Zstd,
    /// PPMd variant I. Read only.
    Ppmd,
    /// xz. Read only.
    Xz,
}

impl Method {
    /// The method a ZIP method number names.
    pub fn from_code(code: u16) -> Result<Self> {
        match code {
            0 => Ok(Method::Store),
            1 => Ok(Method::Shrink),
            2..=5 => Ok(Method::Reduce((code - 1) as u8)),
            6 => Ok(Method::Implode),
            8 => Ok(Method::Deflate),
            9 => Ok(Method::Deflate64),
            12 => Ok(Method::Bzip2),
            14 => Ok(Method::Lzma),
            93 => Ok(Method::Zstd),
            95 => Ok(Method::Xz),
            98 => Ok(Method::Ppmd),
            other => Err(Error::Unsupported(Unsupported::CompressionMethod(other))),
        }
    }

    /// The ZIP method number for this method.
    pub fn code(self) -> u16 {
        match self {
            Method::Store => 0,
            Method::Shrink => 1,
            Method::Reduce(factor) => factor as u16 + 1,
            Method::Implode => 6,
            Method::Deflate => 8,
            Method::Deflate64 => 9,
            Method::Bzip2 => 12,
            Method::Lzma => 14,
            Method::Zstd => 93,
            Method::Xz => 95,
            Method::Ppmd => 98,
        }
    }

    /// The ZIP version a reader needs to handle this method.
    pub fn version_needed(self) -> u16 {
        match self {
            Method::Store => 10,
            Method::Shrink | Method::Reduce(_) => 10,
            Method::Implode => 10,
            Method::Deflate | Method::Deflate64 => 20,
            Method::Bzip2 => version::BZIP2,
            Method::Lzma | Method::Xz | Method::Zstd | Method::Ppmd => version::LZMA,
        }
    }

    /// Whether this build can write this method, not just read it.
    pub fn can_encode(self) -> bool {
        matches!(self, Method::Store | Method::Deflate | Method::Bzip2)
    }
}

/// How hard to work at compressing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Level {
    /// Do not compress at all. Store the bytes as they are.
    None,
    /// Compress quickly, accepting a larger archive.
    Fast,
    /// A sensible balance of the two.
    #[default]
    Default,
    /// Compress as well as this build knows how, however long it takes.
    Best,
}

impl Level {
    /// How far the LZMA match finders look for a longer match.
    pub fn search_depth(self) -> usize {
        match self {
            Level::None | Level::Fast => 8,
            Level::Default => 32,
            Level::Best => 128,
        }
    }

    /// The bzip2 block size for this level, in hundreds of kilobytes.
    pub fn bzip2_block_size(self) -> u8 {
        match self {
            Level::None | Level::Fast => 1,
            Level::Default | Level::Best => 9,
        }
    }

    /// The bits a ZIP entry's flags use to record this level.
    pub fn gp_flag_bits(self) -> u16 {
        match self {
            Level::Best => 0b010,
            Level::Default => 0b000,
            Level::Fast => 0b100,
            Level::None => 0b110,
        }
    }
}

pub fn decoder<'a, R: Read + 'a>(method: Method, input: R, uncompressed_size: u64, flags: u16) -> Result<Box<dyn Read + 'a>> {
    if matches!(method, Method::Shrink | Method::Reduce(_) | Method::Implode) {
        let mut raw = Vec::new();
        let mut input = input;
        input.read_to_end(&mut raw)?;
        let hint = limits::prealloc(uncompressed_size);
        return Ok(Box::new(std::io::Cursor::new(legacy::decompress(method, &raw, flags, hint)?)));
    }

    Ok(match method {
        Method::Store => Box::new(input),
        Method::Deflate => Box::new(deflate::InflateReader::new(input)),
        Method::Deflate64 => Box::new(deflate::InflateReader::deflate64(input)),
        Method::Bzip2 => Box::new(bzip2::Bzip2Reader::new(input)),
        Method::Lzma => Box::new(lzma::reader(input, Some(uncompressed_size))?),
        Method::Zstd => Box::new(zstd::Reader::new(input, uncompressed_size)),
        Method::Xz => Box::new(xz::Reader::new(input, uncompressed_size)),
        Method::Ppmd => Box::new(ppmd::i::Reader::new(input, uncompressed_size)),
        Method::Shrink | Method::Reduce(_) | Method::Implode => unreachable!("legacy methods return early"),
    })
}

pub fn encoder<'a, W: Write + 'a>(method: Method, output: W, level: Level) -> Result<Box<dyn Encoder + 'a>> {
    match method {
        Method::Store => Ok(Box::new(store::StoreEncoder::new(output))),
        Method::Deflate => Ok(Box::new(deflate::DeflateEncoder::new(output, level))),
        Method::Bzip2 => Ok(Box::new(bzip2::Bzip2Encoder::new(output, level.bzip2_block_size()))),
        other => Err(Error::Unsupported(Unsupported::CompressionMethod(other.code()))),
    }
}

pub trait Encoder: Write {
    fn finish(self: Box<Self>) -> Result<u64>;
}
