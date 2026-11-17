use crate::utils::error::{Error, Result};

#[cold]
fn too_far(distance: usize, held: usize, what: &str) -> Error {
    Error::malformed(format!("lzma {what} reaches {distance} bytes back, past the {held} held"))
}

const DRAIN_THRESHOLD: usize = 64 * 1024;

const SLACK: usize = 32;

const WILD: usize = 16;

pub struct Window {
    buf: Vec<u8>,
    filled: usize,
    read: usize,
    total: u64,
    dict_size: usize,
    floor: usize,
}

impl Window {
    pub fn new(dict_size: usize) -> Self {
        Window { buf: vec![0u8; SLACK], filled: 0, read: 0, total: 0, dict_size, floor: 0 }
    }

    pub fn total(&self) -> u64 {
        self.total
    }

    pub fn pending(&self) -> usize {
        self.filled - self.read
    }

    pub fn is_empty(&self) -> bool {
        self.history() == 0
    }

    pub fn reset_dictionary(&mut self) {
        self.floor = self.filled;
    }

    #[inline]
    pub fn history(&self) -> usize {
        self.filled - self.floor
    }

    pub fn set_dictionary_size(&mut self, size: usize) {
        self.dict_size = size;
    }

    pub fn since(&self, from: u64) -> &[u8] {
        let behind = (self.total - from) as usize;
        &self.buf[self.filled - behind..self.filled]
    }

    pub fn take(&mut self, buf: &mut [u8]) -> usize {
        let n = self.pending().min(buf.len());
        buf[..n].copy_from_slice(&self.buf[self.read..self.read + n]);
        self.read += n;
        n
    }

    pub fn drain(&mut self) {
        let keep = self.dict_size.min(self.history());
        let droppable = self.filled - keep;
        let removable = self.read.min(droppable);

        let threshold = (self.dict_size / 4).clamp(DRAIN_THRESHOLD, 16 * 1024 * 1024);
        if removable >= threshold {
            self.buf.drain(..removable);
            self.filled -= removable;
            self.read -= removable;
            self.floor = self.floor.saturating_sub(removable);
        }
    }

    #[inline]
    fn room(&mut self, extra: usize) {
        let want = self.filled + extra + SLACK;
        if self.buf.len() < want {
            self.buf.resize(want, 0);
        }
    }

    #[inline]
    pub fn push(&mut self, byte: u8) {
        self.room(1);
        self.buf[self.filled] = byte;
        self.filled += 1;
        self.total += 1;
    }

    pub fn reserve(&mut self, extra: usize) {
        self.buf.reserve(extra + SLACK);
    }

    pub fn repeat(&mut self, byte: u8, count: usize) {
        self.room(count);
        self.buf[self.filled..self.filled + count].fill(byte);
        self.filled += count;
        self.total += count as u64;
    }

    pub fn extend(&mut self, data: &[u8]) {
        self.room(data.len());
        self.buf[self.filled..self.filled + data.len()].copy_from_slice(data);
        self.filled += data.len();
        self.total += data.len() as u64;
    }

    #[inline]
    pub fn extend_padded(&mut self, data: &[u8], len: usize) {
        debug_assert!(data.len() >= len);
        self.room(len.max(WILD));

        let mut done = 0usize;
        while done < len {
            let take = WILD.min(data.len() - done);
            self.buf[self.filled + done..self.filled + done + take].copy_from_slice(&data[done..done + take]);
            done += take;
        }

        self.filled += len;
        self.total += len as u64;
    }

    pub fn extend_from_reader(&mut self, reader: &mut impl std::io::Read, len: usize) -> std::io::Result<()> {
        self.room(len);
        reader.read_exact(&mut self.buf[self.filled..self.filled + len])?;
        self.filled += len;
        self.total += len as u64;
        Ok(())
    }

    #[inline]
    pub fn back(&self, distance: u32) -> Result<u8> {
        let distance = distance as usize;
        if distance == 0 || distance > self.history() {
            return Err(too_far(distance, self.history(), "reference"));
        }
        Ok(self.buf[self.filled - distance])
    }

    #[inline]
    pub fn last(&self) -> u8 {
        if self.history() == 0 { 0 } else { self.buf[self.filled - 1] }
    }

    #[inline(always)]
    pub fn copy_match(&mut self, distance: u32, len: u32) -> Result<()> {
        let distance = distance as usize;
        if distance == 0 || distance > self.history() {
            return Err(too_far(distance, self.history(), "match"));
        }

        let len = len as usize;
        self.room(len);

        if distance >= WILD && len <= WILD {
            let at = self.filled;
            self.buf.copy_within(at - distance..at - distance + WILD, at);
            self.filled = at + len;
            self.total += len as u64;
            return Ok(());
        }

        self.spread(distance, len);
        Ok(())
    }

    #[inline(never)]
    fn spread(&mut self, distance: usize, len: usize) {
        let start = self.filled - distance;
        let at = self.filled;

        if distance >= WILD {
            let mut done = 0usize;
            while done < len {
                self.buf.copy_within(start + done..start + done + WILD, at + done);
                done += WILD;
            }
        } else if distance == 1 {
            let byte = self.buf[start];
            self.buf[at..at + len].fill(byte);
        } else {
            let mut produced = 0usize;
            while produced < len {
                let take = (distance + produced).min(len - produced);
                self.buf.copy_within(start..start + take, at + produced);
                produced += take;
            }
        }

        self.filled += len;
        self.total += len as u64;
    }
}
