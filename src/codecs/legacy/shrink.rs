use crate::codecs::legacy::bits::BitReader;
use crate::utils::error::{Error, Result};

const MIN_CODE_BITS: u32 = 9;
const MAX_CODE_BITS: u32 = 13;
const TABLE_SIZE: usize = 1 << MAX_CODE_BITS;
const ESCAPE: u16 = 256;
const FIRST_FREE: usize = 257;

const FREE: u16 = u16::MAX;
const ROOT: u16 = u16::MAX - 1;

struct Table {
    parent: Vec<u16>,
    value: Vec<u8>,
    next: usize,
}

impl Table {
    fn new() -> Self {
        let mut parent = vec![FREE; TABLE_SIZE];
        let mut value = vec![0u8; TABLE_SIZE];
        for (code, slot) in parent.iter_mut().enumerate().take(256) {
            *slot = ROOT;
            value[code] = code as u8;
        }
        parent[ESCAPE as usize] = ROOT;
        Table { parent, value, next: FIRST_FREE }
    }

    fn is_free(&self, code: u16) -> bool {
        self.parent[code as usize] == FREE
    }

    fn allocate(&mut self, parent: u16, value: u8) {
        while self.next < TABLE_SIZE && self.parent[self.next] != FREE {
            self.next += 1;
        }
        if self.next < TABLE_SIZE {
            self.parent[self.next] = parent;
            self.value[self.next] = value;
            self.next += 1;
        }
    }

    fn partial_clear(&mut self) {
        let mut is_prefix = vec![false; TABLE_SIZE];
        for code in FIRST_FREE..TABLE_SIZE {
            let parent = self.parent[code];
            if parent != FREE && parent != ROOT {
                is_prefix[parent as usize] = true;
            }
        }

        for (code, &prefix) in is_prefix.iter().enumerate().take(TABLE_SIZE).skip(FIRST_FREE) {
            if !prefix {
                self.parent[code] = FREE;
            }
        }
        self.next = FIRST_FREE;
    }

    fn expand(&self, code: u16, out: &mut Vec<u8>) -> Result<u8> {
        let start = out.len();
        let mut current = code;

        for _ in 0..=TABLE_SIZE {
            match self.parent[current as usize] {
                FREE => return Err(Error::malformed("shrink code refers to an empty table slot")),
                ROOT => {
                    out.push(self.value[current as usize]);
                    out[start..].reverse();
                    return Ok(out[start]);
                }
                parent => {
                    out.push(self.value[current as usize]);
                    current = parent;
                }
            }
        }

        Err(Error::malformed("shrink dictionary contains a cycle"))
    }
}

pub fn decompress(data: &[u8], size_hint: usize) -> Result<Vec<u8>> {
    let mut bits = BitReader::new(data);
    let mut table = Table::new();
    let mut out = Vec::with_capacity(size_hint);
    let mut code_bits = MIN_CODE_BITS;

    let Some(first) = bits.bits(code_bits) else { return Ok(out) };
    let mut previous = first as u16;
    if previous >= 256 {
        return Err(Error::malformed("shrink stream does not begin with a literal"));
    }
    out.push(previous as u8);

    let mut piece = Vec::new();

    while let Some(raw) = bits.bits(code_bits) {
        let code = raw as u16;

        if code == ESCAPE {
            match bits.bits(code_bits) {
                Some(1) => {
                    code_bits += 1;
                    if code_bits > MAX_CODE_BITS {
                        return Err(Error::malformed("shrink stream widens codes past 13 bits"));
                    }
                }
                Some(2) => table.partial_clear(),
                Some(other) => {
                    return Err(Error::malformed(format!("unknown shrink control code {other}")));
                }
                None => break,
            }
            continue;
        }

        piece.clear();
        let first_byte = if table.is_free(code) {
            let f = table.expand(previous, &mut piece)?;
            piece.push(f);
            f
        } else {
            table.expand(code, &mut piece)?
        };

        out.extend_from_slice(&piece);
        table.allocate(previous, first_byte);
        previous = code;
    }

    Ok(out)
}
