pub const LOCAL_HEADER_SIG: u32 = 0x0403_4b50;
pub const CENTRAL_HEADER_SIG: u32 = 0x0201_4b50;
pub const EOCD_SIG: u32 = 0x0605_4b50;
pub const ZIP64_EOCD_SIG: u32 = 0x0606_4b50;
pub const ZIP64_LOCATOR_SIG: u32 = 0x0706_4b50;
pub const DATA_DESCRIPTOR_SIG: u32 = 0x0807_4b50;
pub const ARCHIVE_EXTRA_SIG: u32 = 0x0806_4b50;
pub const DIGITAL_SIGNATURE_SIG: u32 = 0x0505_4b50;

pub const LOCAL_HEADER_LEN: usize = 30;
pub const CENTRAL_HEADER_LEN: usize = 46;
pub const EOCD_LEN: usize = 22;
pub const ZIP64_LOCATOR_LEN: usize = 20;
pub const ZIP64_EOCD_LEN: usize = 56;

pub const U16_MAX: u16 = 0xFFFF;
pub const U32_MAX: u32 = 0xFFFF_FFFF;

pub const MAX_COMMENT_LEN: usize = 0xFFFF;

pub mod flags {
    pub const ENCRYPTED: u16 = 1 << 0;
    pub const COMPRESSION_OPTION_1: u16 = 1 << 1;
    pub const COMPRESSION_OPTION_2: u16 = 1 << 2;
    pub const DATA_DESCRIPTOR: u16 = 1 << 3;
    pub const STRONG_ENCRYPTION: u16 = 1 << 6;
    pub const UTF8: u16 = 1 << 11;
    pub const MASKED_LOCAL_VALUES: u16 = 1 << 13;
}

pub mod extra_id {
    pub const ZIP64: u16 = 0x0001;
    pub const NTFS: u16 = 0x000a;
    pub const UNIX_OLD: u16 = 0x000d;
    pub const EXTENDED_TIMESTAMP: u16 = 0x5455;
    pub const INFOZIP_UNIX1: u16 = 0x5855;
    pub const INFOZIP_UNIX2: u16 = 0x7875;
    pub const AES: u16 = 0x9901;
}

pub mod host {
    pub const MSDOS: u8 = 0;
    pub const UNIX: u8 = 3;
    pub const NTFS: u8 = 10;
    pub const DARWIN: u8 = 19;

    pub fn has_unix_mode(host: u8) -> bool {
        matches!(host, UNIX | DARWIN)
    }
}

pub mod version {
    pub const DEFAULT: u16 = 10;
    pub const DEFLATE: u16 = 20;
    pub const ZIP64: u16 = 45;
    pub const BZIP2: u16 = 46;
    pub const LZMA: u16 = 63;
}

pub fn split_version_made_by(value: u16) -> (u8, u8) {
    ((value >> 8) as u8, (value & 0xff) as u8)
}

pub fn make_version_made_by(host: u8, spec: u8) -> u16 {
    ((host as u16) << 8) | spec as u16
}
