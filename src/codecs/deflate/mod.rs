pub mod bitwriter;
pub mod compress;
pub mod huffman;
pub mod inflate;
pub mod lz77;

pub use compress::{DeflateEncoder, compress, compress_chunk};
pub use inflate::{InflateReader, decompress};
