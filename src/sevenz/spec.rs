use crate::utils::error::{Error, Result, Unsupported};

pub const SIGNATURE: [u8; 6] = [0x37, 0x7A, 0xBC, 0xAF, 0x27, 0x1C];

pub const SIGNATURE_HEADER_LEN: usize = 32;

pub const START_HEADER_LEN: usize = 20;

pub const VERSION_MAJOR: u8 = 0;

pub const VERSION_MINOR: u8 = 4;

pub mod property {
    pub const END: u8 = 0x00;
    pub const HEADER: u8 = 0x01;
    pub const ARCHIVE_PROPERTIES: u8 = 0x02;
    pub const ADDITIONAL_STREAMS_INFO: u8 = 0x03;
    pub const MAIN_STREAMS_INFO: u8 = 0x04;
    pub const FILES_INFO: u8 = 0x05;
    pub const PACK_INFO: u8 = 0x06;
    pub const UNPACK_INFO: u8 = 0x07;
    pub const SUB_STREAMS_INFO: u8 = 0x08;
    pub const SIZE: u8 = 0x09;
    pub const CRC: u8 = 0x0A;
    pub const FOLDER: u8 = 0x0B;
    pub const CODERS_UNPACK_SIZE: u8 = 0x0C;
    pub const NUM_UNPACK_STREAM: u8 = 0x0D;
    pub const EMPTY_STREAM: u8 = 0x0E;
    pub const EMPTY_FILE: u8 = 0x0F;
    pub const ANTI: u8 = 0x10;
    pub const NAME: u8 = 0x11;
    pub const CTIME: u8 = 0x12;
    pub const ATIME: u8 = 0x13;
    pub const MTIME: u8 = 0x14;
    pub const WIN_ATTRIBUTES: u8 = 0x15;
    pub const COMMENT: u8 = 0x16;
    pub const ENCODED_HEADER: u8 = 0x17;
    pub const START_POS: u8 = 0x18;
    pub const DUMMY: u8 = 0x19;
}

pub mod coder_flags {
    pub const ID_SIZE: u8 = 0x0F;
    pub const COMPLEX: u8 = 0x10;
    pub const HAS_ATTRIBUTES: u8 = 0x20;
}

pub mod attribute {
    pub const READONLY: u32 = 0x0000_0001;
    pub const DIRECTORY: u32 = 0x0000_0010;
    pub const REPARSE_POINT: u32 = 0x0000_0400;

    pub const UNIX_EXTENSION: u32 = 0x0000_8000;
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct CoderId {
    bytes: [u8; 15],
    len: u8,
}

impl CoderId {
    pub fn new(bytes: &[u8]) -> Result<Self> {
        if bytes.is_empty() || bytes.len() > 15 {
            return Err(Error::malformed(format!("7z coder id is {} bytes; 1 to 15 are legal", bytes.len())));
        }
        let mut id = CoderId { bytes: [0; 15], len: bytes.len() as u8 };
        id.bytes[..bytes.len()].copy_from_slice(bytes);
        Ok(id)
    }

    pub fn as_slice(&self) -> &[u8] {
        &self.bytes[..self.len as usize]
    }
}

impl std::fmt::Debug for CoderId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for byte in self.as_slice() {
            write!(f, "{byte:02X}")?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Branch {
    X86,
    Ppc,
    Ia64,
    Arm,
    ArmThumb,
    Sparc,
    Arm64,
    RiscV,
}

impl Branch {
    pub fn xz_filter_id(self) -> u64 {
        use crate::codecs::bcj;
        match self {
            Branch::X86 => bcj::X86,
            Branch::Ppc => bcj::POWERPC,
            Branch::Ia64 => bcj::IA64,
            Branch::Arm => bcj::ARM,
            Branch::ArmThumb => bcj::ARM_THUMB,
            Branch::Sparc => bcj::SPARC,
            Branch::Arm64 => bcj::ARM64,
            Branch::RiscV => bcj::RISCV,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Codec {
    Copy,
    Delta,
    Branch(Branch),
    Bcj2,
    Lzma,
    Lzma2,
    Ppmd,
    Bzip2,
    Deflate,
    Deflate64,
    Aes256Sha256,
}

impl Codec {
    pub fn from_id(id: CoderId) -> Result<Self> {
        let codec = match id.as_slice() {
            [0x00] => Codec::Copy,
            [0x03] => Codec::Delta,
            [0x04] | [0x03, 0x03, 0x01, 0x03] => Codec::Branch(Branch::X86),
            [0x05] | [0x03, 0x03, 0x02, 0x05] => Codec::Branch(Branch::Ppc),
            [0x06] | [0x03, 0x03, 0x04, 0x01] => Codec::Branch(Branch::Ia64),
            [0x07] | [0x03, 0x03, 0x05, 0x01] => Codec::Branch(Branch::Arm),
            [0x08] | [0x03, 0x03, 0x07, 0x01] => Codec::Branch(Branch::ArmThumb),
            [0x09] | [0x03, 0x03, 0x08, 0x05] => Codec::Branch(Branch::Sparc),
            [0x0A] => Codec::Branch(Branch::Arm64),
            [0x0B] => Codec::Branch(Branch::RiscV),
            [0x21] => Codec::Lzma2,
            [0x03, 0x01, 0x01] => Codec::Lzma,
            [0x03, 0x03, 0x01, 0x1B] => Codec::Bcj2,
            [0x03, 0x04, 0x01] => Codec::Ppmd,
            [0x04, 0x01, 0x08] => Codec::Deflate,
            [0x04, 0x01, 0x09] => Codec::Deflate64,
            [0x04, 0x02, 0x02] => Codec::Bzip2,
            [0x06, 0xF1, 0x07, 0x01] => Codec::Aes256Sha256,
            other => return Err(Error::Unsupported(Unsupported::Other(foreign_coder(other)))),
        };
        Ok(codec)
    }

    pub fn name(self) -> &'static str {
        match self {
            Codec::Copy => "Copy",
            Codec::Delta => "Delta",
            Codec::Branch(Branch::X86) => "BCJ x86",
            Codec::Branch(Branch::Ppc) => "BCJ PPC",
            Codec::Branch(Branch::Ia64) => "BCJ IA64",
            Codec::Branch(Branch::Arm) => "BCJ ARM",
            Codec::Branch(Branch::ArmThumb) => "BCJ ARMT",
            Codec::Branch(Branch::Sparc) => "BCJ SPARC",
            Codec::Branch(Branch::Arm64) => "BCJ ARM64",
            Codec::Branch(Branch::RiscV) => "BCJ RISC-V",
            Codec::Bcj2 => "BCJ2",
            Codec::Lzma => "LZMA",
            Codec::Lzma2 => "LZMA2",
            Codec::Ppmd => "PPMd",
            Codec::Bzip2 => "BZip2",
            Codec::Deflate => "Deflate",
            Codec::Deflate64 => "Deflate64",
            Codec::Aes256Sha256 => "7zAES",
        }
    }
}

fn foreign_coder(id: &[u8]) -> &'static str {
    match id {
        [0x04, 0xF7, 0x11, 0x01] => "a 7z archive compressed with Zstandard, which only the 7-Zip ZS fork writes",
        [0x04, 0xF7, 0x11, 0x02] => "a 7z archive compressed with Brotli, which only the 7-Zip ZS fork writes",
        [0x04, 0xF7, 0x11, 0x04] => "a 7z archive compressed with LZ4, which only the 7-Zip ZS fork writes",
        [0x04, 0xF7, 0x11, 0x05] => "a 7z archive compressed with LZ5, which only the 7-Zip ZS fork writes",
        [0x04, 0xF7, 0x11, 0x06] => "a 7z archive compressed with Lizard, which only the 7-Zip ZS fork writes",
        [0x03, 0x03, 0x03, 0x01] => "a 7z archive using the Alpha branch converter",
        [0x03, 0x03, 0x06, 0x05] => "a 7z archive using the Motorola 68000 branch converter",
        [0x04, 0x01, 0x00] => "a 7z archive using the raw Zip coder",
        [0x04, 0x03] => "a 7z archive using the Unix compress coder",
        [0x04, 0x04] => "a 7z archive using the Reserved coder",
        [0x06, 0xF1, 0x03, 0x01] => "a 7z archive using the Rar29 coder",
        _ => "a 7z archive using an unrecognised coder",
    }
}
