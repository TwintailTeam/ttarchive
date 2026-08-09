pub mod crc;
pub mod decode;
pub mod encode;

pub use decode::{Bzip2Reader, decompress};
pub use encode::{Bzip2Encoder, compress};
