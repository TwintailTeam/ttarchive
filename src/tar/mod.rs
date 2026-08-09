pub mod header;
pub mod pax;
pub mod reader;
pub mod sparse;
pub mod writer;

pub use header::{Format, Header, Kind};
pub use reader::{TarEntry, TarReader};
pub use writer::TarWriter;

pub fn is_tar(prefix: &[u8]) -> bool {
    if prefix.len() < header::MAGIC.0 + header::MAGIC.1 {
        return false;
    }
    let magic = &prefix[header::MAGIC.0..header::MAGIC.0 + header::MAGIC.1];
    magic.starts_with(b"ustar")
}
