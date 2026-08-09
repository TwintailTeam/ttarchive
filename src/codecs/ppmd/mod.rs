pub mod model;
pub mod range;

use std::io::Read;

use crate::utils::error::{Error, Result, Unsupported};
use model::{Ppmd8, RESTORE_CUT_OFF, RESTORE_RESTART, SYM_END, SYM_ERROR};

const MIN_ORDER: u32 = 2;

fn start<R: Read>(mut inner: R) -> Result<Ppmd8<R>> {
    let mut head = [0u8; 2];
    inner
        .read_exact(&mut head)
        .map_err(|e| if e.kind() == std::io::ErrorKind::UnexpectedEof { Error::malformed("ppmd entry has no parameter word") } else { Error::Io(e) })?;

    let word = u16::from_le_bytes(head) as u32;
    let order = (word & 0x0f) + 1;
    let memory_mb = ((word >> 4) & 0xff) + 1;
    let restore = word >> 12;

    if !(MIN_ORDER..=model::MAX_ORDER).contains(&order) {
        return Err(Error::malformed(format!("ppmd entry declares order {order}, outside 2..=16")));
    }
    if restore > RESTORE_CUT_OFF {
        return Err(Error::Unsupported(Unsupported::Other("a PPMd entry using the FREEZE restore method, whose two revisions disagree")));
    }
    debug_assert!(restore == RESTORE_RESTART || restore == RESTORE_CUT_OFF);

    Ppmd8::new(inner, memory_mb << 20, order, restore)
}

pub fn decompress(data: &[u8], expected: u64) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(expected.min(1 << 30) as usize);
    Reader::new(data, expected).read_to_end(&mut out)?;
    Ok(out)
}

enum Stage<R> {
    Start(R),
    Running(Box<Ppmd8<R>>),
    Done,
}

/// A PPMd entry decoded as it is read.
///
/// The compressed input and the decoded output both pass through a symbol at a
/// time. The model itself is as large as the entry asked for — up to 256 MiB —
/// and is held for as long as the entry is being read; that is inherent to
/// PPMd, so this bounds what it holds beyond the model, not the model.
pub struct Reader<R> {
    stage: Stage<R>,
    expected: u64,
    produced: u64,
}

impl<R: Read> Reader<R> {
    /// Wrap `inner` at the start of a PPMd entry `expected` bytes long.
    pub fn new(inner: R, expected: u64) -> Self {
        Reader { stage: Stage::Start(inner), expected, produced: 0 }
    }
}

impl<R: Read> Read for Reader<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }

        if matches!(self.stage, Stage::Start(_)) {
            let Stage::Start(inner) = std::mem::replace(&mut self.stage, Stage::Done) else { unreachable!("the stage was just checked") };
            self.stage = Stage::Running(Box::new(start(inner)?));
        }

        let Stage::Running(model) = &mut self.stage else { return Ok(0) };

        let mut filled = 0usize;
        while filled < buf.len() && self.produced < self.expected {
            let symbol = model.decode_symbol();
            if symbol < 0 {
                if symbol == SYM_ERROR {
                    return Err(Error::malformed("ppmd stream is corrupt").into());
                }
                debug_assert_eq!(symbol, SYM_END);
                self.stage = Stage::Done;
                return Ok(filled);
            }
            buf[filled] = symbol as u8;
            filled += 1;
            self.produced += 1;
        }

        if self.produced >= self.expected {
            self.stage = Stage::Done;
        }
        Ok(filled)
    }
}
