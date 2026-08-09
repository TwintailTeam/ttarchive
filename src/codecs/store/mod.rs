use std::io::{self, Write};

use crate::codecs::Encoder;
use crate::utils::error::Result;

#[derive(Debug)]
pub struct StoreEncoder<W> {
    inner: W,
    written: u64,
}

impl<W: Write> StoreEncoder<W> {
    pub fn new(inner: W) -> Self {
        StoreEncoder { inner, written: 0 }
    }
}

impl<W: Write> Write for StoreEncoder<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let n = self.inner.write(buf)?;
        self.written += n as u64;
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }

    fn write_all(&mut self, buf: &[u8]) -> io::Result<()> {
        self.inner.write_all(buf)?;
        self.written += buf.len() as u64;
        Ok(())
    }
}

impl<W: Write> Encoder for StoreEncoder<W> {
    fn finish(mut self: Box<Self>) -> Result<u64> {
        self.inner.flush()?;
        Ok(self.written)
    }
}
