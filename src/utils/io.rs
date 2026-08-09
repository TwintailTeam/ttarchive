use std::io::{self, BufRead, Read, Seek, SeekFrom, Write};

use crate::utils::crc32::Crc32;
use crate::utils::progress::Reporter;

pub const COPY_BUF: usize = 128 * 1024;

#[derive(Debug)]
pub struct Limited<R> {
    inner: R,
    remaining: u64,
}

impl<R: Read> Limited<R> {
    pub fn new(inner: R, limit: u64) -> Self {
        Limited { inner, remaining: limit }
    }

    pub fn remaining(&self) -> u64 {
        self.remaining
    }

    pub fn into_inner(self) -> R {
        self.inner
    }

    pub fn get_mut(&mut self) -> &mut R {
        &mut self.inner
    }
}

impl<R: Read> Read for Limited<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.remaining == 0 {
            return Ok(0);
        }
        let cap = buf.len().min(self.remaining as usize);
        let n = self.inner.read(&mut buf[..cap])?;
        self.remaining -= n as u64;
        Ok(n)
    }
}

impl<R: BufRead> BufRead for Limited<R> {
    fn fill_buf(&mut self) -> io::Result<&[u8]> {
        if self.remaining == 0 {
            return Ok(&[]);
        }
        let buf = self.inner.fill_buf()?;
        let cap = buf.len().min(self.remaining as usize);
        Ok(&buf[..cap])
    }

    fn consume(&mut self, amt: usize) {
        let amt = amt.min(self.remaining as usize);
        self.remaining -= amt as u64;
        self.inner.consume(amt);
    }
}

#[derive(Debug)]
pub struct CrcReader<R> {
    inner: R,
    crc: Crc32,
    count: u64,
}

impl<R: Read> CrcReader<R> {
    pub fn new(inner: R) -> Self {
        CrcReader { inner, crc: Crc32::new(), count: 0 }
    }

    pub fn crc(&self) -> u32 {
        self.crc.finish()
    }

    pub fn count(&self) -> u64 {
        self.count
    }

    pub fn into_inner(self) -> R {
        self.inner
    }
}

impl<R: Read> Read for CrcReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.crc.update(&buf[..n]);
        self.count += n as u64;
        Ok(n)
    }
}

#[derive(Debug)]
pub struct CrcWriter<W> {
    inner: W,
    crc: Crc32,
    count: u64,
}

impl<W: Write> CrcWriter<W> {
    pub fn new(inner: W) -> Self {
        CrcWriter { inner, crc: Crc32::new(), count: 0 }
    }

    pub fn crc(&self) -> u32 {
        self.crc.finish()
    }

    pub fn count(&self) -> u64 {
        self.count
    }

    pub fn into_inner(self) -> W {
        self.inner
    }
}

impl<W: Write> Write for CrcWriter<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let n = self.inner.write(buf)?;
        self.crc.update(&buf[..n]);
        self.count += n as u64;
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

#[derive(Debug)]
pub struct CountingWriter<W> {
    inner: W,
    offset: u64,
}

impl<W: Write> CountingWriter<W> {
    pub fn new(inner: W, start: u64) -> Self {
        CountingWriter { inner, offset: start }
    }

    pub fn offset(&self) -> u64 {
        self.offset
    }

    pub fn into_inner(self) -> W {
        self.inner
    }

    pub fn get_mut(&mut self) -> &mut W {
        &mut self.inner
    }
}

impl<W: Write> Write for CountingWriter<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let n = self.inner.write(buf)?;
        self.offset += n as u64;
        Ok(n)
    }

    fn write_all(&mut self, buf: &[u8]) -> io::Result<()> {
        self.inner.write_all(buf)?;
        self.offset += buf.len() as u64;
        Ok(())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

#[derive(Debug)]
pub struct ProgressWriter<W> {
    inner: W,
    reporter: Reporter,
}

impl<W: Write> ProgressWriter<W> {
    pub fn new(inner: W, reporter: Reporter) -> Self {
        ProgressWriter { inner, reporter }
    }

    pub fn into_inner(self) -> W {
        self.inner
    }
}

impl<W: Write> Write for ProgressWriter<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let n = self.inner.write(buf)?;
        self.reporter.add_bytes(n as u64);
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

#[derive(Debug)]
pub struct SectionReader<R> {
    inner: R,
    start: u64,
    len: u64,
    pos: u64,
}

impl<R: Read + Seek> SectionReader<R> {
    pub fn new(mut inner: R, start: u64, len: u64) -> io::Result<Self> {
        inner.seek(SeekFrom::Start(start))?;
        Ok(SectionReader { inner, start, len, pos: 0 })
    }

    pub fn remaining(&self) -> u64 {
        self.len - self.pos
    }
}

impl<R: Read + Seek> Read for SectionReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let remaining = self.remaining();
        if remaining == 0 {
            return Ok(0);
        }
        let cap = buf.len().min(remaining as usize);
        let n = self.inner.read(&mut buf[..cap])?;
        self.pos += n as u64;
        Ok(n)
    }
}

impl<R: Read + Seek> Seek for SectionReader<R> {
    fn seek(&mut self, from: SeekFrom) -> io::Result<u64> {
        let target = match from {
            SeekFrom::Start(n) => n as i64,
            SeekFrom::Current(d) => self.pos as i64 + d,
            SeekFrom::End(d) => self.len as i64 + d,
        };
        if target < 0 {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "seek before start of section"));
        }
        let target = (target as u64).min(self.len);
        self.inner.seek(SeekFrom::Start(self.start + target))?;
        self.pos = target;
        Ok(target)
    }
}

pub fn copy_buffered<R: Read + ?Sized, W: Write + ?Sized>(reader: &mut R, writer: &mut W, buf: &mut [u8]) -> io::Result<u64> {
    let mut total = 0u64;
    loop {
        let n = match reader.read(buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(ref e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        };
        writer.write_all(&buf[..n])?;
        total += n as u64;
    }
    Ok(total)
}
