use crate::utils::error::{Error, Result};

#[derive(Debug, Clone)]
pub struct Cursor<'a> {
    buf: &'a [u8],
    pos: usize,
    base: u64,
}

impl<'a> Cursor<'a> {
    pub fn new(buf: &'a [u8], base: u64) -> Self {
        Cursor { buf, pos: 0, base }
    }

    pub fn remaining(&self) -> usize {
        self.buf.len() - self.pos
    }

    pub fn is_empty(&self) -> bool {
        self.remaining() == 0
    }

    pub fn offset(&self) -> u64 {
        self.base + self.pos as u64
    }

    fn take(&mut self, n: usize, what: &str) -> Result<&'a [u8]> {
        if self.remaining() < n {
            return Err(Error::malformed_at(format!("truncated while reading {what}: need {n} bytes, {} left", self.remaining()), self.offset()));
        }
        let out = &self.buf[self.pos..self.pos + n];
        self.pos += n;
        Ok(out)
    }

    pub fn u8(&mut self, what: &str) -> Result<u8> {
        Ok(self.take(1, what)?[0])
    }

    pub fn u16(&mut self, what: &str) -> Result<u16> {
        let b = self.take(2, what)?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }

    pub fn u32(&mut self, what: &str) -> Result<u32> {
        let b = self.take(4, what)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    pub fn u64(&mut self, what: &str) -> Result<u64> {
        let b = self.take(8, what)?;
        Ok(u64::from_le_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]]))
    }

    pub fn slice(&mut self, n: usize, what: &str) -> Result<&'a [u8]> {
        self.take(n, what)
    }

    pub fn skip(&mut self, n: usize, what: &str) -> Result<()> {
        self.take(n, what).map(|_| ())
    }
}

#[inline]
pub fn put_u16(out: &mut Vec<u8>, v: u16) {
    out.extend_from_slice(&v.to_le_bytes());
}

#[inline]
pub fn put_u32(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_le_bytes());
}

#[inline]
pub fn put_u64(out: &mut Vec<u8>, v: u64) {
    out.extend_from_slice(&v.to_le_bytes());
}

pub fn rfind(haystack: &[u8], needle: &[u8; 4]) -> Option<usize> {
    if haystack.len() < 4 {
        return None;
    }
    (0..=haystack.len() - 4).rev().find(|&i| &haystack[i..i + 4] == needle)
}
