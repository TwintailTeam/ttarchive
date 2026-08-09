pub mod bits;
pub mod implode;
pub mod reduce;
pub mod shrink;

use crate::codecs::Method;
use crate::utils::error::Result;

pub fn decompress(method: Method, data: &[u8], flags: u16, size_hint: usize) -> Result<Vec<u8>> {
    match method {
        Method::Shrink => shrink::decompress(data, size_hint),
        Method::Reduce(factor) => reduce::decompress(data, factor as u16 + 1, size_hint),
        Method::Implode => implode::decompress(data, flags, size_hint),
        other => Err(crate::utils::error::Error::malformed(format!("method {} is not a legacy method", other.code()))),
    }
}
