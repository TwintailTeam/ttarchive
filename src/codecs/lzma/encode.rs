use std::io::Write;

use crate::codecs::Level;
use crate::codecs::lzma::decode::Properties;
use crate::codecs::lzma::range::{MOVE_BITS, PROB_BITS, PROB_INIT, Prob, TOP, probs};
pub use crate::codecs::sliding::{Feed, Sliding};
use crate::utils::error::Result;

const STATES: usize = 12;
const MAX_POS_BITS: u32 = 4;
const LEN_TO_POS_STATES: usize = 4;
const ALIGN_BITS: u32 = 4;
const END_POS_MODEL_INDEX: u32 = 14;
const FULL_DISTANCES: u32 = 1 << (END_POS_MODEL_INDEX >> 1);

pub const MATCH_MIN_LEN: u32 = 2;
pub const MATCH_MAX_LEN: u32 = 273;

const HASH_BITS: u32 = 18;
const HASH_SIZE: usize = 1 << HASH_BITS;
const NONE: u32 = u32::MAX;

/// How fast the search gives up on a stretch that keeps yielding literals.
///
/// After enough literals in a row the encoder searches only every other
/// position, then every third, and so on, which is what keeps incompressible
/// input from costing a full hash chain walk per byte. Any match resets it.
const SKIP_SHIFT: u32 = 6;

pub struct RangeEncoder<W> {
    out: W,
    low: u64,
    range: u32,
    cache: u8,
    pending: u64,
}

impl<W: Write> RangeEncoder<W> {
    /// Start coding into `out`.
    pub fn new(out: W) -> Self {
        RangeEncoder { out, low: 0, range: u32::MAX, cache: 0, pending: 1 }
    }

    fn shift_low(&mut self) -> Result<()> {
        if self.low < 0xFF00_0000 || self.low > 0xFFFF_FFFF {
            let carry = (self.low >> 32) as u8;
            let mut byte = self.cache;
            loop {
                self.out.write_all(&[byte.wrapping_add(carry)])?;
                byte = 0xFF;
                self.pending -= 1;
                if self.pending == 0 {
                    break;
                }
            }
            self.cache = ((self.low >> 24) & 0xFF) as u8;
        }
        self.pending += 1;
        self.low = (self.low << 8) & 0xFFFF_FFFF;
        Ok(())
    }

    #[inline]
    fn bit(&mut self, prob: &mut Prob, bit: u32) -> Result<()> {
        let value = *prob as u32;
        let bound = (self.range >> PROB_BITS) * value;

        if bit == 0 {
            self.range = bound;
            *prob = (value + (((1 << PROB_BITS) - value) >> MOVE_BITS)) as Prob;
        } else {
            self.low += bound as u64;
            self.range -= bound;
            *prob = (value - (value >> MOVE_BITS)) as Prob;
        }

        while self.range < TOP {
            self.range <<= 8;
            self.shift_low()?;
        }
        Ok(())
    }

    fn direct_bits(&mut self, value: u32, count: u32) -> Result<()> {
        for index in (0..count).rev() {
            self.range >>= 1;
            if (value >> index) & 1 != 0 {
                self.low += self.range as u64;
            }
            while self.range < TOP {
                self.range <<= 8;
                self.shift_low()?;
            }
        }
        Ok(())
    }

    fn tree(&mut self, table: &mut [Prob], bits: u32, symbol: u32) -> Result<()> {
        let mut node = 1u32;
        for index in (0..bits).rev() {
            let bit = (symbol >> index) & 1;
            self.bit(&mut table[node as usize], bit)?;
            node = (node << 1) | bit;
        }
        Ok(())
    }

    fn tree_reverse(&mut self, table: &mut [Prob], bits: u32, symbol: u32) -> Result<()> {
        let mut node = 1u32;
        let mut left = symbol;
        for _ in 0..bits {
            let bit = left & 1;
            left >>= 1;
            self.bit(&mut table[node as usize], bit)?;
            node = (node << 1) | bit;
        }
        Ok(())
    }

    /// Flush the last bytes and give the writer back.
    pub fn finish(mut self) -> Result<W> {
        for _ in 0..5 {
            self.shift_low()?;
        }
        self.out.flush()?;
        Ok(self.out)
    }
}

#[derive(Clone)]
struct LenEncoder {
    choice: Prob,
    choice2: Prob,
    low: Vec<Prob>,
    mid: Vec<Prob>,
    high: Vec<Prob>,
}

impl LenEncoder {
    fn new() -> Self {
        LenEncoder { choice: PROB_INIT, choice2: PROB_INIT, low: probs((1 << MAX_POS_BITS) * 8), mid: probs((1 << MAX_POS_BITS) * 8), high: probs(256) }
    }

    fn encode<W: Write>(&mut self, rc: &mut RangeEncoder<W>, symbol: u32, pos_state: usize) -> Result<()> {
        if symbol < 8 {
            rc.bit(&mut self.choice, 0)?;
            return rc.tree(&mut self.low[pos_state * 8..pos_state * 8 + 8], 3, symbol);
        }

        rc.bit(&mut self.choice, 1)?;
        if symbol < 16 {
            rc.bit(&mut self.choice2, 0)?;
            return rc.tree(&mut self.mid[pos_state * 8..pos_state * 8 + 8], 3, symbol - 8);
        }

        rc.bit(&mut self.choice2, 1)?;
        rc.tree(&mut self.high, 8, symbol - 16)
    }
}

#[derive(Clone)]
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
    len: LenEncoder,
    rep_len: LenEncoder,
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
            len: LenEncoder::new(),
            rep_len: LenEncoder::new(),
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct Match {
    dist_code: u32,
    len: u32,
    rep: Option<usize>,
}

pub struct Finder {
    head: Vec<u32>,
    chain: Vec<u32>,
    mask: usize,
    depth: usize,
}

impl Finder {
    pub fn new(len: usize, window: usize, depth: usize) -> Self {
        let wanted = len.clamp(1, window.max(1));
        let span = 1usize << (usize::BITS - 1 - wanted.leading_zeros());
        Finder { head: vec![NONE; HASH_SIZE], chain: vec![NONE; span], mask: span - 1, depth }
    }

    #[inline]
    fn hash(feed: &Feed, at: usize) -> usize {
        let quad = u32::from_le_bytes([feed.get(at), feed.get(at + 1), feed.get(at + 2), feed.get(at + 3)]);
        (quad.wrapping_mul(2_654_435_761) >> (32 - HASH_BITS)) as usize
    }

    pub fn insert(&mut self, feed: &Feed, at: usize) {
        if at + 4 > feed.end() {
            return;
        }
        let slot = Self::hash(feed, at);
        self.chain[at & self.mask] = self.head[slot];
        self.head[slot] = at as u32;
    }

    fn longest(&self, feed: &Feed, at: usize, window: usize, best_already: u32) -> Option<(u32, u32)> {
        if at + 4 > feed.end() {
            return None;
        }

        let limit = (MATCH_MAX_LEN as usize).min(feed.end() - at);
        let mut best_len = best_already.max(MATCH_MIN_LEN - 1) as usize;
        if best_len >= limit {
            return None;
        }
        let mut best_dist = 0u32;

        let oldest = at.saturating_sub(window.min(self.chain.len())).max(feed.base());
        let mut candidate = self.head[Self::hash(feed, at)];
        let mut tries = self.depth;

        while candidate != NONE && tries > 0 {
            let pos = candidate as usize;
            if pos < oldest {
                break;
            }
            tries -= 1;
            candidate = self.chain[pos & self.mask];

            if feed.get(pos + best_len) != feed.get(at + best_len) {
                continue;
            }

            let mut len = 0usize;
            while len < limit && feed.get(pos + len) == feed.get(at + len) {
                len += 1;
            }

            if len > best_len {
                best_len = len;
                best_dist = (at - pos) as u32;
                if len >= limit {
                    break;
                }
            }
        }

        if best_dist == 0 { None } else { Some((best_dist, best_len as u32)) }
    }
}

fn pos_slot_of(dist: u32) -> u32 {
    if dist < 4 {
        return dist;
    }
    let high = 31 - dist.leading_zeros();
    (high << 1) | ((dist >> (high - 1)) & 1)
}

fn match_length(feed: &Feed, at: usize, distance: usize) -> u32 {
    if distance == 0 || distance > at - feed.base() {
        return 0;
    }
    let limit = (MATCH_MAX_LEN as usize).min(feed.end() - at);
    let from = at - distance;
    let mut len = 0usize;
    while len < limit && feed.get(from + len) == feed.get(at + len) {
        len += 1;
    }
    len as u32
}

#[derive(Clone)]
pub struct Encoder {
    props: Properties,
    model: Model,
    state: usize,
    reps: [u32; 4],
}

impl Encoder {
    pub fn new(props: Properties) -> Self {
        Encoder { props, model: Model::new(props), state: 0, reps: [0; 4] }
    }

    fn literal_state(state: usize) -> usize {
        if state < 4 {
            0
        } else if state < 10 {
            state - 3
        } else {
            state - 6
        }
    }

    fn encode_literal<W: Write>(&mut self, rc: &mut RangeEncoder<W>, feed: &Feed, at: usize) -> Result<()> {
        let previous = if at == 0 { 0 } else { feed.get(at - 1) };
        let low_bits = (at as u32) & ((1 << self.props.lp) - 1);
        let context = ((low_bits << self.props.lc) + (previous as u32 >> (8 - self.props.lc))) as usize;

        let table = &mut self.model.literal[0x300 * context..0x300 * (context + 1)];
        let target = feed.get(at) as u32;

        let mut symbol = 1u32;
        let mut index = 0u32;

        if self.state >= 7 {
            let mut matched = feed.get(at - (self.reps[0] as usize + 1)) as u32;
            while index < 8 {
                let match_bit = (matched >> 7) & 1;
                matched = (matched << 1) & 0xff;

                let bit = (target >> (7 - index)) & 1;
                rc.bit(&mut table[(((1 + match_bit) << 8) + symbol) as usize], bit)?;
                symbol = (symbol << 1) | bit;
                index += 1;

                if match_bit != bit {
                    break;
                }
            }
        }

        while index < 8 {
            let bit = (target >> (7 - index)) & 1;
            rc.bit(&mut table[symbol as usize], bit)?;
            symbol = (symbol << 1) | bit;
            index += 1;
        }

        self.state = Self::literal_state(self.state);
        Ok(())
    }

    fn encode_distance<W: Write>(&mut self, rc: &mut RangeEncoder<W>, dist_code: u32, len_symbol: u32) -> Result<()> {
        let len_state = (len_symbol as usize).min(LEN_TO_POS_STATES - 1);
        let slot = pos_slot_of(dist_code);

        rc.tree(&mut self.model.pos_slot[len_state * 64..len_state * 64 + 64], 6, slot)?;

        if slot < 4 {
            return Ok(());
        }

        let direct_bits = (slot >> 1) - 1;
        let base = (2 | (slot & 1)) << direct_bits;

        if slot < END_POS_MODEL_INDEX {
            let offset = (base - slot) as usize;
            rc.tree_reverse(&mut self.model.pos_special[offset..], direct_bits, dist_code - base)
        } else {
            rc.direct_bits((dist_code - base) >> ALIGN_BITS, direct_bits - ALIGN_BITS)?;
            rc.tree_reverse(&mut self.model.pos_align, ALIGN_BITS, (dist_code - base) & ((1 << ALIGN_BITS) - 1))
        }
    }

    fn best_at(&self, feed: &Feed, at: usize, finder: &Finder) -> Option<Match> {
        let window = self.props.dict_size as usize;

        let mut best: Option<Match> = None;

        for (index, &rep) in self.reps.iter().enumerate() {
            let distance = rep as usize + 1;
            let len = match_length(feed, at, distance);
            if len >= MATCH_MIN_LEN && best.is_none_or(|b| len > b.len) {
                best = Some(Match { dist_code: rep, len, rep: Some(index) });
            }
        }

        let floor = best.map_or(0, |b| b.len);
        if let Some((distance, len)) = finder.longest(feed, at, window, floor)
            && len >= MATCH_MIN_LEN
            && len > floor
        {
            best = Some(Match { dist_code: distance - 1, len, rep: None });
        }

        best
    }

    /// Compress `data` into a raw LZMA stream, without any container framing.
    pub fn encode<W: Write>(&mut self, data: &[u8], out: W, depth: usize) -> Result<W> {
        let mut finder = Finder::new(data.len(), self.props.dict_size as usize, depth);
        self.encode_range(&Feed::whole(data), 0, data.len(), &mut finder, out)
    }

    /// Compress `feed[from..to]` as one range-coded stream.
    ///
    /// Matches may reach back before `from`, so successive calls share one
    /// dictionary; the model and rep distances carry over unless reset by the
    /// caller. This is what lets LZMA2 chunk without losing context.
    pub fn encode_range<W: Write>(&mut self, feed: &Feed, from: usize, to: usize, finder: &mut Finder, out: W) -> Result<W> {
        let mut rc = RangeEncoder::new(out);
        self.encode_span(feed, from, to, finder, &mut rc)?;
        rc.finish()
    }

    /// Compress `feed[from..to]` into a range coder that outlives the call.
    ///
    /// A container that frames each chunk separately wants
    /// [`Encoder::encode_range`]; one long stream wants this, so the coder is
    /// flushed once at the end.
    pub fn encode_span<W: Write>(&mut self, feed: &Feed, from: usize, to: usize, finder: &mut Finder, rc: &mut RangeEncoder<W>) -> Result<()> {
        let pos_mask = (1u32 << self.props.pb) - 1;
        let mut at = from;
        let mut barren = 0usize;

        while at < to {
            let pos_state = ((at as u32) & pos_mask) as usize;
            let match_index = (self.state << MAX_POS_BITS) + pos_state;

            let mut chosen = if barren >> SKIP_SHIFT > 0 && !at.is_multiple_of(1 + (barren >> SKIP_SHIFT)) { None } else { self.best_at(feed, at, finder) };

            finder.insert(feed, at);

            if let Some(current) = chosen
                && current.len < MATCH_MAX_LEN
                && at + 1 < to
                && let Some(next) = self.best_at(feed, at + 1, finder)
                && next.len > current.len + 1
            {
                chosen = None;
            }

            let chosen = chosen.filter(|m| m.len >= MATCH_MIN_LEN).map(|mut m| {
                m.len = m.len.min((to - at) as u32);
                m
            });
            let chosen = chosen.filter(|m| m.len >= MATCH_MIN_LEN);

            let Some(found) = chosen else {
                rc.bit(&mut self.model.is_match[match_index], 0)?;
                self.encode_literal(rc, feed, at)?;
                at += 1;
                barren += 1;
                continue;
            };

            barren = 0;

            rc.bit(&mut self.model.is_match[match_index], 1)?;
            let len_symbol = found.len - MATCH_MIN_LEN;

            match found.rep {
                Some(index) => {
                    rc.bit(&mut self.model.is_rep[self.state], 1)?;

                    if index == 0 {
                        rc.bit(&mut self.model.is_rep_g0[self.state], 0)?;
                        rc.bit(&mut self.model.is_rep0_long[match_index], 1)?;
                    } else {
                        rc.bit(&mut self.model.is_rep_g0[self.state], 1)?;
                        if index == 1 {
                            rc.bit(&mut self.model.is_rep_g1[self.state], 0)?;
                            self.reps.swap(0, 1);
                        } else {
                            rc.bit(&mut self.model.is_rep_g1[self.state], 1)?;
                            rc.bit(&mut self.model.is_rep_g2[self.state], if index == 2 { 0 } else { 1 })?;

                            let picked = if index == 2 {
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
                    }

                    self.model.rep_len.encode(rc, len_symbol, pos_state)?;
                    self.state = if self.state < 7 { 8 } else { 11 };
                }

                None => {
                    rc.bit(&mut self.model.is_rep[self.state], 0)?;
                    self.model.len.encode(rc, len_symbol, pos_state)?;
                    self.state = if self.state < 7 { 7 } else { 10 };

                    self.encode_distance(rc, found.dist_code, len_symbol)?;

                    self.reps[3] = self.reps[2];
                    self.reps[2] = self.reps[1];
                    self.reps[1] = self.reps[0];
                    self.reps[0] = found.dist_code;
                }
            }

            for step in 1..found.len as usize {
                finder.insert(feed, at + step);
            }
            at += found.len as usize;
        }

        Ok(())
    }

    /// Encode the marker that tells a decoder the stream ends here.
    ///
    /// A container that records the uncompressed size does not need it; one
    /// that writes the size as unknown does.
    pub fn encode_end_marker<W: Write>(&mut self, at: usize, rc: &mut RangeEncoder<W>) -> Result<()> {
        let pos_state = ((at as u32) & ((1u32 << self.props.pb) - 1)) as usize;
        let match_index = (self.state << MAX_POS_BITS) + pos_state;

        rc.bit(&mut self.model.is_match[match_index], 1)?;
        rc.bit(&mut self.model.is_rep[self.state], 0)?;
        self.model.len.encode(rc, 0, pos_state)?;
        self.state = if self.state < 7 { 7 } else { 10 };
        self.encode_distance(rc, u32::MAX, 0)
    }
}

/// Choose a dictionary size, clamped to what the format and decoder accept.
pub fn dictionary_for(len: usize) -> u32 {
    dictionary_at(len, Level::Default)
}

/// Choose a dictionary size for a level, clamped to what the format accepts.
///
/// A larger dictionary finds more distant matches but costs four bytes of
/// match chain per byte it spans, so the level decides how much of that to
/// spend. The default matches what `xz -6` uses.
pub fn dictionary_at(len: usize, level: Level) -> u32 {
    let ceiling = match level {
        Level::None | Level::Fast => 1 << 20,
        Level::Default => 1 << 23,
        Level::Best => 1 << 26,
    };
    let wanted = len.min(ceiling).next_power_of_two().clamp(1 << 16, ceiling) as u32;
    wanted.max(1 << 12)
}

pub fn properties_byte(props: Properties) -> u8 {
    ((props.pb * 5 + props.lp) * 9 + props.lc) as u8
}

/// Compress into a raw LZMA stream using the default lc/lp/pb.
pub fn compress_raw(data: &[u8], props: Properties, depth: usize) -> Result<Vec<u8>> {
    let mut encoder = Encoder::new(props);
    encoder.encode(data, Vec::new(), depth)
}
