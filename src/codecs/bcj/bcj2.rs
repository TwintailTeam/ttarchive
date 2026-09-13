use std::io::{self, BufReader, Read};

use crate::codecs::lzma::range::{Prob, RangeDecoder, probs};
use crate::utils::error::Result;

const PROB_COUNT: usize = 256 + 2;
const E9: usize = 256;
const CONDITIONAL: usize = 257;

fn is_jump(previous: u8, byte: u8) -> bool {
    byte & 0xFE == 0xE8 || (previous == 0x0F && byte & 0xF0 == 0x80)
}

fn next_byte<R: Read>(reader: &mut R) -> io::Result<Option<u8>> {
    let mut byte = [0u8; 1];
    loop {
        match reader.read(&mut byte) {
            Ok(0) => return Ok(None),
            Ok(_) => return Ok(Some(byte[0])),
            Err(ref e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        }
    }
}

pub struct Bcj2Decoder<S: Read> {
    main: BufReader<S>,
    call: BufReader<S>,
    jump: BufReader<S>,
    range: RangeDecoder<S>,
    probs: Vec<Prob>,
    previous: u8,
    position: u32,
    pending: [u8; 4],
    pending_at: usize,
    limit: u64,
    produced: u64,
    drained: bool,
}

impl<S: Read> Bcj2Decoder<S> {
    pub fn new(main: S, call: S, jump: S, range: S, limit: u64) -> Result<Self> {
        Ok(Bcj2Decoder {
            main: BufReader::new(main),
            call: BufReader::new(call),
            jump: BufReader::new(jump),
            range: RangeDecoder::new(range)?,
            probs: probs(PROB_COUNT),
            previous: 0,
            position: 0,
            pending: [0; 4],
            pending_at: 4,
            limit,
            produced: 0,
            drained: false,
        })
    }

    fn emit(&mut self, byte: u8, buf: &mut [u8], written: &mut usize) {
        buf[*written] = byte;
        *written += 1;
        self.position = self.position.wrapping_add(1);
        self.produced += 1;
    }
}

impl<S: Read> Read for Bcj2Decoder<S> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let mut written = 0;

        while written < buf.len() && self.produced < self.limit {
            if self.pending_at < 4 {
                let byte = self.pending[self.pending_at];
                self.pending_at += 1;
                self.emit(byte, buf, &mut written);
                continue;
            }
            if self.drained {
                break;
            }

            let Some(byte) = next_byte(&mut self.main)? else {
                self.drained = true;
                break;
            };
            self.emit(byte, buf, &mut written);

            if !is_jump(self.previous, byte) {
                self.previous = byte;
                continue;
            }

            let slot = match byte {
                0xE8 => self.previous as usize,
                0xE9 => E9,
                _ => CONDITIONAL,
            };
            if self.range.bit(&mut self.probs[slot]) == 0 {
                self.previous = byte;
                continue;
            }

            let mut absolute = [0u8; 4];
            if byte == 0xE8 {
                self.call.read_exact(&mut absolute)?;
            } else {
                self.jump.read_exact(&mut absolute)?;
            }

            let relative = u32::from_be_bytes(absolute).wrapping_sub(self.position.wrapping_add(4));
            self.pending = relative.to_le_bytes();
            self.pending_at = 0;
            self.previous = (relative >> 24) as u8;
        }

        Ok(written)
    }
}
