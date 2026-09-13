pub mod model;
pub mod range;

use std::io::Read;

use crate::utils::error::{Error, Result};
use model::{Ppmd7, SYM_END, SYM_ERROR};

pub const MIN_MEM_SIZE: u32 = 2048;

pub fn from_properties<R: Read>(inner: R, properties: &[u8], expected: u64) -> Result<Reader<R>> {
    let props: [u8; 5] =
        properties.try_into().map_err(|_| Error::malformed(format!("7z PPMd coder carries {} property bytes; the format has 5", properties.len())))?;

    let order = props[0] as u32;
    let mem_size = u32::from_le_bytes([props[1], props[2], props[3], props[4]]);
    Reader::new(inner, expected, order, mem_size)
}

pub struct Reader<R> {
    model: Option<Box<Ppmd7<R>>>,
    expected: u64,
    produced: u64,
}

impl<R: Read> Reader<R> {
    pub fn new(inner: R, expected: u64, order: u32, mem_size: u32) -> Result<Self> {
        if !(model::MIN_ORDER..=model::MAX_ORDER).contains(&order) {
            return Err(Error::malformed(format!("ppmd var. H declares order {order}, outside {}..={}", model::MIN_ORDER, model::MAX_ORDER)));
        }
        if mem_size < MIN_MEM_SIZE {
            return Err(Error::malformed(format!("ppmd var. H declares a {mem_size} byte sub-allocator, below the {MIN_MEM_SIZE} byte minimum")));
        }
        crate::utils::limits::codec_memory(mem_size, "a PPMd coder asking for more memory than this build will set aside")?;

        Ok(Reader { model: Some(Box::new(Ppmd7::new(inner, mem_size, order)?)), expected, produced: 0 })
    }
}

impl<R: Read> Read for Reader<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        let Some(model) = &mut self.model else { return Ok(0) };

        let mut filled = 0usize;
        let mut produced = self.produced;
        let mut ended = false;

        while filled < buf.len() && produced < self.expected {
            let symbol = model.decode_symbol();
            if symbol < 0 {
                if symbol == SYM_ERROR {
                    return Err(Error::malformed("ppmd var. H stream is corrupt").into());
                }
                debug_assert_eq!(symbol, SYM_END);
                ended = true;
                break;
            }
            buf[filled] = symbol as u8;
            filled += 1;
            produced += 1;
        }

        let finished = model.is_finished();
        let ran_dry = model.ran_dry();
        self.produced = produced;

        if ran_dry {
            self.model = None;
            return Err(Error::malformed(format!("ppmd var. H folder runs out of packed bytes after {produced} of the {} it declares", self.expected)).into());
        }

        if ended && produced < self.expected {
            return Err(Error::malformed(format!("ppmd var. H folder ended after {produced} bytes, not the {} it declares", self.expected)).into());
        }

        if produced >= self.expected {
            self.model = None;
            if !finished {
                return Err(Error::malformed("ppmd var. H folder does not end where its range coder does").into());
            }
        }
        Ok(filled)
    }
}
