use std::fs::File;
use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};

use crate::utils::error::{Error, Result};
use crate::utils::io::SectionReader;

pub const STREAM_BUFFER: usize = 64 * 1024;

pub trait Source: Send + Sync {
    fn open(&self, start: u64, len: u64) -> Result<Box<dyn Read + Send + '_>>;

    fn size(&self) -> u64;

    fn read_at(&self, start: u64, len: u64) -> Result<Vec<u8>> {
        let mut bytes = Vec::new();
        self.open(start, len)?.read_to_end(&mut bytes)?;
        if bytes.len() as u64 != len {
            return Err(Error::malformed(format!("7z archive ends {} bytes early", len - bytes.len() as u64)));
        }
        Ok(bytes)
    }

    fn check_range(&self, start: u64, len: u64) -> Result<()> {
        match start.checked_add(len) {
            Some(end) if end <= self.size() => Ok(()),
            _ => Err(Error::malformed(format!("7z archive points at bytes {start}..{} but is only {} long", start.saturating_add(len), self.size()))),
        }
    }
}

#[derive(Debug, Clone)]
pub struct FileSource {
    path: PathBuf,
    size: u64,
}

impl FileSource {
    pub fn new(path: impl Into<PathBuf>) -> Result<Self> {
        let path = path.into();
        let size = std::fs::metadata(&path)?.len();
        Ok(FileSource { path, size })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Source for FileSource {
    fn open(&self, start: u64, len: u64) -> Result<Box<dyn Read + Send + '_>> {
        self.check_range(start, len)?;
        let section = SectionReader::new(File::open(&self.path)?, start, len)?;
        Ok(Box::new(BufReader::with_capacity(STREAM_BUFFER.min(len.max(1) as usize), section)))
    }

    fn size(&self) -> u64 {
        self.size
    }
}

#[derive(Debug, Clone)]
pub struct BytesSource {
    bytes: Vec<u8>,
}

impl BytesSource {
    pub fn new(bytes: Vec<u8>) -> Self {
        BytesSource { bytes }
    }

    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
}

impl Source for BytesSource {
    fn open(&self, start: u64, len: u64) -> Result<Box<dyn Read + Send + '_>> {
        self.check_range(start, len)?;
        let start = start as usize;
        Ok(Box::new(&self.bytes[start..start + len as usize]))
    }

    fn size(&self) -> u64 {
        self.bytes.len() as u64
    }
}

#[derive(Debug, Clone)]
pub enum ArchiveSource {
    Single(FileSource),
    Volumes(crate::sevenz::volumes::VolumeSource),
}

impl ArchiveSource {
    pub fn open(path: &Path) -> Result<Self> {
        use crate::sevenz::volumes::{VolumeSet, VolumeSource, base_name};

        let named_a_volume = base_name(path) != path;
        if !named_a_volume && path.exists() {
            return Ok(ArchiveSource::Single(FileSource::new(path)?));
        }

        match VolumeSet::discover(path)? {
            Some(set) => Ok(ArchiveSource::Volumes(VolumeSource::new(set))),
            None => Ok(ArchiveSource::Single(FileSource::new(path)?)),
        }
    }

    pub fn paths(&self) -> Vec<PathBuf> {
        match self {
            ArchiveSource::Single(file) => vec![file.path().to_path_buf()],
            ArchiveSource::Volumes(volumes) => volumes.set().paths().to_vec(),
        }
    }
}

impl Source for ArchiveSource {
    fn open(&self, start: u64, len: u64) -> Result<Box<dyn Read + Send + '_>> {
        match self {
            ArchiveSource::Single(file) => file.open(start, len),
            ArchiveSource::Volumes(volumes) => volumes.open(start, len),
        }
    }

    fn size(&self) -> u64 {
        match self {
            ArchiveSource::Single(file) => file.size(),
            ArchiveSource::Volumes(volumes) => volumes.size(),
        }
    }
}
