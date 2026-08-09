pub mod attributes;
pub mod parsers;
pub mod reader;
pub mod spec;
pub mod volumes;
pub mod writer;

pub use reader::ZipReader;
pub use volumes::{VolumeLayout, VolumeSet};
pub use writer::ZipWriter;

pub const MAGIC: [u8; 4] = [0x50, 0x4b, 0x03, 0x04];

pub const EMPTY_MAGIC: [u8; 4] = [0x50, 0x4b, 0x05, 0x06];

pub const SPANNED_MAGIC: [u8; 4] = [0x50, 0x4b, 0x07, 0x08];

pub const TEMP_SPANNING_MAGIC: [u8; 4] = [0x50, 0x4b, 0x30, 0x30];

pub fn is_zip(prefix: &[u8]) -> bool {
    prefix.len() >= 4 && (prefix[..4] == MAGIC || prefix[..4] == EMPTY_MAGIC || prefix[..4] == SPANNED_MAGIC || prefix[..4] == TEMP_SPANNING_MAGIC)
}
