use crate::utils::error::{Error, Result};

const DRAIN_THRESHOLD: usize = 64 * 1024;

pub struct Window {
    buf: Vec<u8>,
    read: usize,
    total: u64,
    dict_size: usize,
    floor: usize,
}

impl Window {
    pub fn new(dict_size: usize) -> Self {
        Window { buf: Vec::with_capacity(DRAIN_THRESHOLD * 2), read: 0, total: 0, dict_size, floor: 0 }
    }

    pub fn total(&self) -> u64 {
        self.total
    }

    pub fn pending(&self) -> usize {
        self.buf.len() - self.read
    }

    pub fn is_empty(&self) -> bool {
        self.history() == 0
    }

    pub fn reset_dictionary(&mut self) {
        self.floor = self.buf.len();
    }

    /// How much output is reachable: bytes produced since the last dictionary
    /// reset, which is what bounds a match distance.
    #[inline]
    pub fn history(&self) -> usize {
        self.buf.len() - self.floor
    }

    /// Change how much history is kept when draining.
    ///
    /// A container whose members declare their own window sizes sets this as
    /// each one starts.
    pub fn set_dictionary_size(&mut self, size: usize) {
        self.dict_size = size;
    }

    /// The bytes produced from absolute position `from` onward.
    ///
    /// Only valid before they are drained, which is what lets a caller checksum
    /// output it is about to hand on.
    pub fn since(&self, from: u64) -> &[u8] {
        let behind = (self.total - from) as usize;
        &self.buf[self.buf.len() - behind..]
    }

    pub fn take(&mut self, buf: &mut [u8]) -> usize {
        let n = self.pending().min(buf.len());
        buf[..n].copy_from_slice(&self.buf[self.read..self.read + n]);
        self.read += n;
        n
    }

    pub fn drain(&mut self) {
        let keep = self.dict_size.min(self.history());
        let droppable = self.buf.len() - keep;
        let removable = self.read.min(droppable);
        if removable >= DRAIN_THRESHOLD {
            self.buf.drain(..removable);
            self.read -= removable;
            self.floor = self.floor.saturating_sub(removable);
        }
    }

    #[inline]
    pub fn push(&mut self, byte: u8) {
        self.buf.push(byte);
        self.total += 1;
    }

    pub fn extend(&mut self, data: &[u8]) {
        self.buf.extend_from_slice(data);
        self.total += data.len() as u64;
    }

    #[inline]
    pub fn back(&self, distance: u32) -> Result<u8> {
        let distance = distance as usize;
        if distance == 0 || distance > self.history() {
            return Err(Error::malformed(format!("lzma reference reaches {distance} bytes back, past the {} held", self.history())));
        }
        Ok(self.buf[self.buf.len() - distance])
    }

    #[inline]
    pub fn last(&self) -> u8 {
        if self.history() == 0 { 0 } else { self.buf[self.buf.len() - 1] }
    }

    pub fn copy_match(&mut self, distance: u32, len: u32) -> Result<()> {
        let distance = distance as usize;
        if distance == 0 || distance > self.history() {
            return Err(Error::malformed(format!("lzma match reaches {distance} bytes back, past the {} held", self.history())));
        }

        let len = len as usize;
        self.buf.reserve(len);

        if distance >= len {
            let start = self.buf.len() - distance;
            self.buf.extend_from_within(start..start + len);
        } else {
            let mut copied = 0usize;
            while copied < len {
                let take = distance.min(len - copied);
                let start = self.buf.len() - distance;
                self.buf.extend_from_within(start..start + take);
                copied += take;
            }
        }

        self.total += len as u64;
        Ok(())
    }
}
