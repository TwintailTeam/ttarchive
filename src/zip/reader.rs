use std::io::{Read, Seek};

use crate::codecs::{self, Method};
use crate::crypto::Password;
use crate::crypto::stream::DecryptReader;
use crate::crypto::winzip_aes::Strength;
use crate::pipeline::entry::{Entry, ZipDetail};
use crate::utils::error::{Error, Result, Unsupported};
use crate::utils::io::{CrcReader, SectionReader};
use crate::utils::progress::Reporter;
use crate::zip::parsers::{central, eocd, local};
use crate::zip::spec::flags;
use crate::zip::volumes::VolumeLayout;

pub struct ZipReader<R> {
    source: R,
    entries: Vec<Entry>,
    comment: Vec<u8>,
    zip64: bool,
    layout: VolumeLayout,
}

impl<R: Read + Seek> ZipReader<R> {
    pub fn new(mut source: R) -> Result<Self> {
        let len = source.seek(std::io::SeekFrom::End(0))?;
        Self::with_layout(source, VolumeLayout::single(len))
    }

    pub fn with_layout(mut source: R, layout: VolumeLayout) -> Result<Self> {
        let directory = eocd::find(&mut source)?;

        let directory_offset = layout.global_offset(directory.directory_disk, directory.offset)?;
        eocd::validate_range(directory_offset, directory.size, layout.total_len())?;

        let mut buf = vec![0u8; directory.size as usize];
        source.seek(std::io::SeekFrom::Start(directory_offset))?;
        source.read_exact(&mut buf).map_err(|e| {
            if e.kind() == std::io::ErrorKind::UnexpectedEof { Error::malformed_at("central directory is truncated", directory_offset) } else { Error::Io(e) }
        })?;

        let entries = central::parse_all(&buf, directory_offset, directory.entries)?;

        Ok(ZipReader { source, entries, comment: directory.comment, zip64: directory.zip64, layout })
    }

    pub fn layout(&self) -> &VolumeLayout {
        &self.layout
    }

    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn comment(&self) -> &[u8] {
        &self.comment
    }

    pub fn is_zip64(&self) -> bool {
        self.zip64
    }

    pub fn index_of(&self, name: &str) -> Option<usize> {
        self.entries.iter().position(|e| e.name == name)
    }

    pub fn total_uncompressed_size(&self) -> u64 {
        self.entries.iter().map(|e| e.size).sum()
    }

    pub fn into_inner(self) -> R {
        self.source
    }

    pub fn check_readable(entry: &Entry, password: Option<&Password>) -> Result<Method> {
        let zip = zip_detail(entry)?;
        if zip.flags & flags::STRONG_ENCRYPTION != 0 && zip.aes.is_none() {
            return Err(Error::Unsupported(Unsupported::StrongEncryption));
        }
        if zip.is_encrypted() && password.is_none() {
            return Err(Error::PasswordRequired { entry: entry.name.clone() });
        }
        zip.method()
    }

    pub fn encryption_of(entry: &Entry) -> EntryEncryption {
        let Ok(zip) = zip_detail(entry) else { return EntryEncryption::None };
        if !zip.is_encrypted() {
            return EntryEncryption::None;
        }
        match &zip.aes {
            Some(aes) => EntryEncryption::Aes(aes.strength),
            None => EntryEncryption::ZipCrypto,
        }
    }

    pub fn entry_range(&mut self, index: usize) -> Result<EntryRange> {
        let entry = self.entries.get(index).ok_or_else(|| Error::malformed(format!("entry index {index} out of range")))?;

        let zip = zip_detail(entry)?;

        let header_offset = self.layout.global_offset(zip.disk_start, zip.local_header_offset)?;
        let header = local::read_at(&mut self.source, header_offset)?;

        let compressed_size = if zip.has_data_descriptor() || header.compressed_size == 0 { zip.compressed_size } else { header.compressed_size };

        let check_byte = if zip.has_data_descriptor() { (header.mod_time >> 8) as u8 } else { (zip.crc32 >> 24) as u8 };

        let verify_crc = zip.aes.as_ref().is_none_or(|aes| aes.crc_is_meaningful());

        Ok(EntryRange {
            offset: header.data_offset,
            compressed_size,
            uncompressed_size: entry.size,
            crc32: zip.crc32,
            method_code: zip.effective_method_code(),
            encryption: Self::encryption_of(entry),
            check_byte,
            verify_crc,
            flags: zip.flags,
        })
    }

    pub fn entry_reader(&mut self, index: usize) -> Result<EntryReader<'_>> {
        self.entry_reader_with(index, None)
    }

    pub fn entry_reader_with(&mut self, index: usize, password: Option<&Password>) -> Result<EntryReader<'_>> {
        let entry = self.entries.get(index).ok_or_else(|| Error::malformed(format!("entry index {index} out of range")))?.clone();

        let method = Self::check_readable(&entry, password)?;
        let range = self.entry_range(index)?;

        let section = SectionReader::new(&mut self.source, range.offset, range.compressed_size)?;
        let plain = open_decrypted(section, &range, password)?;
        let decoded = codecs::decoder(method, plain, entry.size, range.flags)?;

        Ok(EntryReader {
            inner: CrcReader::new(decoded),
            expected_crc: range.crc32,
            expected_size: entry.size,
            name: entry.name,
            verify_crc: range.verify_crc,
            finished: false,
        })
    }

    pub fn read_entry(&mut self, index: usize) -> Result<Vec<u8>> {
        self.read_entry_with(index, None)
    }

    pub fn read_entry_with(&mut self, index: usize, password: Option<&Password>) -> Result<Vec<u8>> {
        let capacity = self.entries[index].size.min(64 * 1024 * 1024) as usize;
        let mut out = Vec::with_capacity(capacity);
        self.entry_reader_with(index, password)?.read_to_end(&mut out)?;
        Ok(out)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EntryRange {
    pub offset: u64,
    pub compressed_size: u64,
    pub uncompressed_size: u64,
    pub crc32: u32,
    pub method_code: u16,
    pub encryption: EntryEncryption,
    pub check_byte: u8,
    pub verify_crc: bool,
    pub flags: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryEncryption {
    None,
    ZipCrypto,
    Aes(Strength),
}

pub struct EntryReader<'a> {
    inner: CrcReader<Box<dyn Read + 'a>>,
    expected_crc: u32,
    expected_size: u64,
    name: String,
    verify_crc: bool,
    finished: bool,
}

impl EntryReader<'_> {
    fn verify(&mut self) -> std::io::Result<()> {
        if self.finished {
            return Ok(());
        }
        self.finished = true;

        let found = self.inner.count();
        if found != self.expected_size {
            return Err(Error::SizeMismatch { entry: self.name.clone(), expected: self.expected_size, found }.into());
        }

        let crc = self.inner.crc();
        if self.verify_crc && crc != self.expected_crc {
            return Err(Error::ChecksumMismatch { entry: self.name.clone(), expected: self.expected_crc, found: crc }.into());
        }

        Ok(())
    }
}

impl Read for EntryReader<'_> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.inner.read(buf)?;
        if n == 0 {
            self.verify()?;
        }
        Ok(n)
    }
}

pub fn read_range<R: Read + Seek>(
    source: R,
    range: &EntryRange,
    name: &str,
    out: &mut impl std::io::Write,
    reporter: &Reporter,
    buffer: &mut [u8],
    password: Option<&Password>,
) -> Result<()> {
    let method = Method::from_code(range.method_code)?;
    let section = SectionReader::new(source, range.offset, range.compressed_size)?;
    let plain = open_decrypted(section, range, password)?;
    let mut decoded = CrcReader::new(codecs::decoder(method, plain, range.uncompressed_size, range.flags)?);

    let mut written = 0u64;
    loop {
        let n = match decoded.read(buffer) {
            Ok(0) => break,
            Ok(n) => n,
            Err(ref e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(Error::from(e)),
        };
        out.write_all(&buffer[..n])?;
        written += n as u64;
        reporter.add_bytes(n as u64);
    }

    if written != range.uncompressed_size {
        return Err(Error::SizeMismatch { entry: name.to_owned(), expected: range.uncompressed_size, found: written });
    }

    let crc = decoded.crc();
    if range.verify_crc && crc != range.crc32 {
        return Err(Error::ChecksumMismatch { entry: name.to_owned(), expected: range.crc32, found: crc });
    }

    Ok(())
}

fn open_decrypted<'a, R: Read + 'a>(source: R, range: &EntryRange, password: Option<&Password>) -> Result<Box<dyn Read + 'a>> {
    match range.encryption {
        EntryEncryption::None => Ok(Box::new(source)),

        EntryEncryption::ZipCrypto => {
            let password = password.ok_or(Error::WrongPassword)?;
            Ok(Box::new(DecryptReader::zipcrypto(source, password, range.compressed_size, range.check_byte)?))
        }

        EntryEncryption::Aes(strength) => {
            let password = password.ok_or(Error::WrongPassword)?;
            Ok(Box::new(DecryptReader::winzip_aes(source, password, range.compressed_size, strength)?))
        }
    }
}

fn zip_detail(entry: &Entry) -> Result<&ZipDetail> {
    entry.zip().ok_or_else(|| Error::malformed(format!("entry {:?} is not a zip entry", entry.name)))
}
