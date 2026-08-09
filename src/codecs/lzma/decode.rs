use std::io::Read;

use crate::codecs::lzma::range::{PROB_INIT, Prob, RangeDecoder, probs};
use crate::codecs::lzma::window::Window;
use crate::utils::error::{Error, Result};

const MAX_POS_BITS: u32 = 4;
const STATES: usize = 12;
const LEN_TO_POS_STATES: usize = 4;
const ALIGN_BITS: u32 = 4;
const END_POS_MODEL_INDEX: u32 = 14;
const FULL_DISTANCES: u32 = 1 << (END_POS_MODEL_INDEX >> 1);
pub const MATCH_MIN_LEN: u32 = 2;
const END_MARKER: u32 = u32::MAX;

const OUT_CHUNK: usize = 64 * 1024;
const MAX_DICT: u32 = 1 << 30;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Properties {
    pub lc: u32,
    pub lp: u32,
    pub pb: u32,
    pub dict_size: u32,
}

impl Properties {
    pub fn from_byte(byte: u8, dict_size: u32) -> Result<Self> {
        if byte >= 9 * 5 * 5 {
            return Err(Error::malformed(format!("invalid lzma properties byte {byte:#04x}")));
        }
        let mut value = byte as u32;
        let lc = value % 9;
        value /= 9;
        let lp = value % 5;
        let pb = value / 5;

        if dict_size > MAX_DICT {
            return Err(Error::malformed(format!("lzma dictionary of {dict_size} bytes exceeds the {MAX_DICT} byte limit")));
        }

        Ok(Properties { lc, lp, pb, dict_size: dict_size.max(1 << 12) })
    }

    pub fn from_bytes(bytes: [u8; 5]) -> Result<Self> {
        let dict = u32::from_le_bytes([bytes[1], bytes[2], bytes[3], bytes[4]]);
        Properties::from_byte(bytes[0], dict)
    }
}

struct LenDecoder {
    choice: Prob,
    choice2: Prob,
    low: Vec<Prob>,
    mid: Vec<Prob>,
    high: Vec<Prob>,
}

impl LenDecoder {
    fn new() -> Self {
        LenDecoder { choice: PROB_INIT, choice2: PROB_INIT, low: probs((1 << MAX_POS_BITS) * 8), mid: probs((1 << MAX_POS_BITS) * 8), high: probs(256) }
    }

    fn decode<R: Read>(&mut self, rc: &mut RangeDecoder<R>, pos_state: usize) -> u32 {
        if rc.bit(&mut self.choice) == 0 {
            return rc.tree(&mut self.low[pos_state * 8..pos_state * 8 + 8], 3);
        }
        if rc.bit(&mut self.choice2) == 0 {
            return 8 + rc.tree(&mut self.mid[pos_state * 8..pos_state * 8 + 8], 3);
        }
        16 + rc.tree(&mut self.high, 8)
    }
}

struct Model {
    literal: Vec<Prob>,
    is_match: Vec<Prob>,
    is_rep: Vec<Prob>,
    is_rep_g0: Vec<Prob>,
    is_rep_g1: Vec<Prob>,
    is_rep_g2: Vec<Prob>,
    is_rep0_long: Vec<Prob>,
    pos_slot: Vec<Prob>,
    pos_special: Vec<Prob>,
    pos_align: Vec<Prob>,
    len: LenDecoder,
    rep_len: LenDecoder,
}

impl Model {
    fn new(props: Properties) -> Self {
        Model {
            literal: probs(0x300 << (props.lc + props.lp)),
            is_match: probs(STATES << MAX_POS_BITS),
            is_rep: probs(STATES),
            is_rep_g0: probs(STATES),
            is_rep_g1: probs(STATES),
            is_rep_g2: probs(STATES),
            is_rep0_long: probs(STATES << MAX_POS_BITS),
            pos_slot: probs(LEN_TO_POS_STATES * 64),
            pos_special: probs(1 + FULL_DISTANCES as usize - END_POS_MODEL_INDEX as usize),
            pos_align: probs(1 << ALIGN_BITS),
            len: LenDecoder::new(),
            rep_len: LenDecoder::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stop {
    Limit,
    Marker,
}

pub struct LzmaCore {
    props: Properties,
    model: Model,
    state: usize,
    reps: [u32; 4],
}

impl LzmaCore {
    pub fn new(props: Properties) -> Self {
        LzmaCore { props, model: Model::new(props), state: 0, reps: [0; 4] }
    }

    pub fn reset(&mut self, props: Properties) {
        self.props = props;
        self.model = Model::new(props);
        self.state = 0;
        self.reps = [0; 4];
    }

    pub fn decode<R: Read>(&mut self, rc: &mut RangeDecoder<R>, window: &mut Window, limit: u64) -> Result<Stop> {
        while window.total() < limit {
            let pos_state = (window.total() as usize) & ((1 << self.props.pb) - 1);
            let match_index = (self.state << MAX_POS_BITS) + pos_state;

            if rc.bit(&mut self.model.is_match[match_index]) == 0 {
                self.literal(rc, window)?;
                continue;
            }

            let len;
            if rc.bit(&mut self.model.is_rep[self.state]) != 0 {
                if window.is_empty() {
                    return Err(Error::malformed("lzma stream repeats a distance before producing anything"));
                }

                if rc.bit(&mut self.model.is_rep_g0[self.state]) == 0 {
                    if rc.bit(&mut self.model.is_rep0_long[match_index]) == 0 {
                        let byte = window.back(self.reps[0] + 1)?;
                        window.push(byte);
                        self.state = if self.state < 7 { 9 } else { 11 };
                        continue;
                    }
                } else if rc.bit(&mut self.model.is_rep_g1[self.state]) == 0 {
                    self.reps.swap(0, 1);
                } else {
                    let picked = if rc.bit(&mut self.model.is_rep_g2[self.state]) == 0 {
                        self.reps[2]
                    } else {
                        let third = self.reps[3];
                        self.reps[3] = self.reps[2];
                        third
                    };
                    self.reps[2] = self.reps[1];
                    self.reps[1] = self.reps[0];
                    self.reps[0] = picked;
                }

                len = self.model.rep_len.decode(rc, pos_state);
                self.state = if self.state < 7 { 8 } else { 11 };
            } else {
                self.reps[3] = self.reps[2];
                self.reps[2] = self.reps[1];
                self.reps[1] = self.reps[0];

                len = self.model.len.decode(rc, pos_state);
                self.state = if self.state < 7 { 7 } else { 10 };

                let distance = self.distance(rc, len);
                if distance == END_MARKER {
                    return Ok(Stop::Marker);
                }
                if distance >= self.props.dict_size {
                    return Err(Error::malformed(format!(
                        "lzma match distance {distance} exceeds the declared \
                         {} byte dictionary",
                        self.props.dict_size
                    )));
                }
                self.reps[0] = distance;
            }

            window.copy_match(self.reps[0] + 1, len + MATCH_MIN_LEN)?;
        }

        Ok(Stop::Limit)
    }

    fn literal<R: Read>(&mut self, rc: &mut RangeDecoder<R>, window: &mut Window) -> Result<()> {
        let previous = window.last();
        let low_bits = (window.total() as u32) & ((1 << self.props.lp) - 1);
        let context = ((low_bits << self.props.lc) + (previous as u32 >> (8 - self.props.lc))) as usize;

        let matched = if self.state >= 7 { Some(window.back(self.reps[0] + 1)?) } else { None };

        let table = &mut self.model.literal[0x300 * context..0x300 * (context + 1)];
        let mut symbol = 1u32;

        if let Some(mut match_byte) = matched {
            loop {
                let match_bit = (match_byte >> 7) as u32 & 1;
                match_byte <<= 1;
                let bit = rc.bit(&mut table[(((1 + match_bit) << 8) + symbol) as usize]);
                symbol = (symbol << 1) | bit;
                if match_bit != bit || symbol >= 0x100 {
                    break;
                }
            }
        }

        while symbol < 0x100 {
            symbol = (symbol << 1) | rc.bit(&mut table[symbol as usize]);
        }

        window.push(symbol as u8);
        self.state = if self.state < 4 {
            0
        } else if self.state < 10 {
            self.state - 3
        } else {
            self.state - 6
        };
        Ok(())
    }

    fn distance<R: Read>(&mut self, rc: &mut RangeDecoder<R>, len: u32) -> u32 {
        let len_state = (len as usize).min(LEN_TO_POS_STATES - 1);
        let slot = rc.tree(&mut self.model.pos_slot[len_state * 64..len_state * 64 + 64], 6);

        if slot < 4 {
            return slot;
        }

        let direct_bits = (slot >> 1) - 1;
        let mut dist = (2 | (slot & 1)) << direct_bits;

        if slot < END_POS_MODEL_INDEX {
            let base = (dist - slot) as usize;
            dist += rc.tree_reverse(&mut self.model.pos_special[base..], direct_bits);
        } else {
            dist += rc.direct_bits(direct_bits - ALIGN_BITS) << ALIGN_BITS;
            dist += rc.tree_reverse(&mut self.model.pos_align, ALIGN_BITS);
        }

        dist
    }
}

pub struct LzmaDecoder<R> {
    rc: RangeDecoder<R>,
    core: LzmaCore,
    window: Window,
    expected: Option<u64>,
    finished: bool,
}

impl<R: Read> LzmaDecoder<R> {
    /// Bytes of the compressed input the stream actually used.
    ///
    /// A container with a trailer, such as lzip, needs this to find it, since the
    /// range decoder buffers ahead of what it consumes.
    pub fn consumed(&self) -> usize {
        self.rc.consumed()
    }

    /// Take the reader back along with the bytes the range decoder read ahead.
    pub fn into_parts(self) -> (R, Vec<u8>) {
        self.rc.into_parts()
    }

    pub fn new(inner: R, props: Properties, expected: Option<u64>) -> Result<Self> {
        Ok(LzmaDecoder { rc: RangeDecoder::new(inner)?, core: LzmaCore::new(props), window: Window::new(props.dict_size as usize), expected, finished: false })
    }
}

impl<R: Read> Read for LzmaDecoder<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }

        while self.window.pending() == 0 && !self.finished {
            self.window.drain();

            let limit = match self.expected {
                Some(want) if self.window.total() >= want => {
                    self.finished = true;
                    break;
                }
                Some(want) => want.min(self.window.total() + OUT_CHUNK as u64),
                None => self.window.total() + OUT_CHUNK as u64,
            };

            if self.core.decode(&mut self.rc, &mut self.window, limit)? == Stop::Marker {
                self.finished = true;
            }
        }

        Ok(self.window.take(buf))
    }
}
