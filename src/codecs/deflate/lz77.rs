use crate::codecs::Level;

pub const MIN_MATCH: usize = 3;
pub const MAX_MATCH: usize = 258;
pub const MAX_DISTANCE: usize = 32_768;

const HASH_BITS: u32 = 16;
const HASH_SIZE: usize = 1 << HASH_BITS;

const HASH_BYTES: usize = 4;
const NIL: i32 = i32::MIN;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Token {
    Literal(u8),
    Match { len: u16, dist: u16 },
}

#[derive(Debug, Clone, Copy)]
struct Config {
    max_chain: u32,
    good_length: usize,
    nice_length: usize,
    lazy: bool,
    insert_all: bool,
}

impl Config {
    fn for_level(level: Level) -> Self {
        match level {
            Level::None => Config { max_chain: 0, good_length: 0, nice_length: 0, lazy: false, insert_all: false },
            Level::Fast => Config { max_chain: 16, good_length: 8, nice_length: 32, lazy: false, insert_all: false },
            Level::Default => Config { max_chain: 96, good_length: 32, nice_length: 128, lazy: true, insert_all: true },
            Level::Best => Config { max_chain: 1024, good_length: 128, nice_length: MAX_MATCH, lazy: true, insert_all: true },
        }
    }
}

pub struct MatchFinder {
    head: Vec<i32>,
    prev: Vec<i32>,
    config: Config,
    base: i32,
    span: i32,
}

impl MatchFinder {
    pub fn new(level: Level) -> Self {
        MatchFinder { head: vec![NIL; HASH_SIZE], prev: vec![NIL; MAX_DISTANCE], config: Config::for_level(level), base: 0, span: 0 }
    }

    fn begin_block(&mut self, len: usize) {
        let advance = self.span as i64 + 1;
        if self.base as i64 + advance >= i32::MAX as i64 {
            self.head.fill(NIL);
            self.prev.fill(NIL);
            self.base = 0;
        } else {
            self.base += advance as i32;
        }
        self.span = len as i32;
    }

    #[inline]
    fn hash(data: &[u8], pos: usize) -> usize {
        let key = u32::from_le_bytes([data[pos], data[pos + 1], data[pos + 2], data[pos + 3]]);
        ((key.wrapping_mul(0x9E37_79B1)) >> (32 - HASH_BITS)) as usize
    }

    #[inline]
    fn match_length(data: &[u8], a: usize, b: usize, limit: usize) -> usize {
        let mut n = 0;
        while n + 8 <= limit {
            let x = u64::from_le_bytes(data[a + n..a + n + 8].try_into().unwrap());
            let y = u64::from_le_bytes(data[b + n..b + n + 8].try_into().unwrap());
            if x != y {
                return n + ((x ^ y).trailing_zeros() / 8) as usize;
            }
            n += 8;
        }
        while n < limit && data[a + n] == data[b + n] {
            n += 1;
        }
        n
    }

    fn find(&self, data: &[u8], pos: usize, min_len: usize, start: i32) -> Option<(usize, usize)> {
        let limit = (data.len() - pos).min(MAX_MATCH);
        if limit < MIN_MATCH {
            return None;
        }

        let earliest = pos.saturating_sub(MAX_DISTANCE);
        let mut candidate = start;
        let mut best_len = min_len.max(MIN_MATCH - 1);
        let mut best_dist = 0usize;

        let mut chain = if best_len >= self.config.good_length { (self.config.max_chain / 4).max(1) } else { self.config.max_chain };

        while chain > 0 {
            if candidate < self.base {
                break;
            }

            if best_len >= limit {
                break;
            }

            let cpos = (candidate - self.base) as usize;
            if cpos < earliest {
                break;
            }

            if data[cpos + best_len] == data[pos + best_len] {
                let len = Self::match_length(data, cpos, pos, limit);
                if len > best_len {
                    best_len = len;
                    best_dist = pos - cpos;
                    if len >= self.config.nice_length {
                        break;
                    }
                    if len >= self.config.good_length {
                        chain = chain.min((self.config.max_chain / 4).max(1));
                    }
                }
            }

            candidate = self.prev[cpos % MAX_DISTANCE];
            chain -= 1;
        }

        if best_len >= MIN_MATCH && best_dist > 0 { Some((best_len, best_dist)) } else { None }
    }

    #[inline]
    fn insert(&mut self, data: &[u8], pos: usize) -> i32 {
        let h = Self::hash(data, pos);
        let previous = self.head[h];
        self.prev[pos % MAX_DISTANCE] = previous;
        self.head[h] = self.base + pos as i32;
        previous
    }

    pub fn tokenize(&mut self, data: &[u8], out: &mut Vec<Token>) {
        out.clear();
        self.begin_block(data.len());

        if self.config.max_chain == 0 {
            out.extend(data.iter().map(|&b| Token::Literal(b)));
            return;
        }

        let mut pos = 0usize;
        let hash_end = data.len().saturating_sub(HASH_BYTES - 1);

        while pos < data.len() {
            if pos >= hash_end {
                out.push(Token::Literal(data[pos]));
                pos += 1;
                continue;
            }

            let candidate = self.insert(data, pos);
            let found = self.find(data, pos, MIN_MATCH - 1, candidate);

            let Some((mut len, mut dist)) = found else {
                out.push(Token::Literal(data[pos]));
                pos += 1;
                continue;
            };

            if self.config.lazy && len < self.config.nice_length && pos + 1 < hash_end {
                let next_candidate = self.insert(data, pos + 1);
                if let Some((next_len, next_dist)) = self.find(data, pos + 1, len, next_candidate) {
                    out.push(Token::Literal(data[pos]));
                    pos += 1;
                    len = next_len;
                    dist = next_dist;
                }
            }

            out.push(Token::Match { len: len as u16, dist: dist as u16 });

            let end = pos + len;
            if self.config.insert_all {
                let mut p = pos + 1;
                while p < end && p < hash_end {
                    self.insert(data, p);
                    p += 1;
                }
            } else if end > 0 && end - 1 < hash_end {
                self.insert(data, end - 1);
            }
            pos = end;
        }
    }
}
