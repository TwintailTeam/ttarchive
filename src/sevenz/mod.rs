pub mod chain;
pub mod folder;
pub mod header;
pub mod number;
pub mod reader;
pub mod source;
pub mod spec;
pub mod volumes;
pub mod writer;

pub use reader::{Location, SevenZReader};
pub use source::{ArchiveSource, BytesSource, FileSource, Source};

pub fn is_sevenz(prefix: &[u8]) -> bool {
    prefix.len() >= spec::SIGNATURE.len() && prefix[..spec::SIGNATURE.len()] == spec::SIGNATURE
}
