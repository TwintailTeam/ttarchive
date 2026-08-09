use std::fs::{self, File};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use crate::utils::error::{Error, Result};

pub const SPANNING_SIGNATURE: u32 = 0x0807_4b50;
pub const TEMP_SPANNING_MARKER: u32 = 0x3030_4b50;

pub const MIN_VOLUME_SIZE: u64 = 64 * 1024;
pub const MAX_VOLUME_SIZE: u64 = 0xFFFF_FFFF;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scheme {
    Single,
    Split,
    RawSplit,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VolumeLayout {
    starts: Vec<u64>,
    lengths: Vec<u64>,
    scheme: Scheme,
}

impl VolumeLayout {
    pub fn single(len: u64) -> Self {
        VolumeLayout { starts: vec![0], lengths: vec![len], scheme: Scheme::Single }
    }

    pub fn count(&self) -> usize {
        self.starts.len()
    }

    pub fn scheme(&self) -> Scheme {
        self.scheme
    }

    pub fn total_len(&self) -> u64 {
        self.starts.last().copied().unwrap_or(0) + self.lengths.last().copied().unwrap_or(0)
    }

    pub fn global_offset(&self, disk: u32, offset: u64) -> Result<u64> {
        match self.scheme {
            Scheme::Single | Scheme::RawSplit => {
                if disk != 0 {
                    return Err(Error::malformed(format!("entry claims to start on disk {disk}, but this archive has one disk")));
                }
                Ok(offset)
            }
            Scheme::Split => {
                let start = self.starts.get(disk as usize).copied().ok_or_else(|| {
                    Error::malformed(format!(
                        "entry starts on disk {disk}, but only {} volumes were found; \
                         a segment is probably missing",
                        self.starts.len()
                    ))
                })?;
                Ok(start + offset)
            }
        }
    }

    pub fn locate(&self, global: u64) -> (u32, u64) {
        match self.scheme {
            Scheme::Single | Scheme::RawSplit => (0, global),
            Scheme::Split => {
                let disk = self.starts.partition_point(|&s| s <= global).saturating_sub(1);
                (disk as u32, global - self.starts[disk])
            }
        }
    }
}

#[derive(Debug, Clone)]
pub struct VolumeSet {
    paths: Vec<PathBuf>,
    layout: VolumeLayout,
}

impl VolumeSet {
    pub fn discover(path: &Path) -> Result<Self> {
        let base = canonical_base(path);

        if let Some(set) = discover_split(&base)? {
            return Ok(set);
        }
        if let Some(set) = discover_raw_split(&base)? {
            return Ok(set);
        }

        let len = fs::metadata(path)?.len();
        Ok(VolumeSet { paths: vec![path.to_path_buf()], layout: VolumeLayout::single(len) })
    }

    pub fn paths(&self) -> &[PathBuf] {
        &self.paths
    }

    pub fn layout(&self) -> &VolumeLayout {
        &self.layout
    }

    pub fn is_multi_volume(&self) -> bool {
        self.paths.len() > 1
    }

    pub fn open(&self) -> Result<SegmentedReader> {
        SegmentedReader::open(self.paths.clone(), self.layout.clone())
    }
}

fn canonical_base(path: &Path) -> PathBuf {
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or_default();

    if let Some((stem, suffix)) = name.rsplit_once('.')
        && suffix.len() == 3
        && suffix.bytes().all(|b| b.is_ascii_digit())
    {
        return path.with_file_name(stem);
    }

    if let Some((stem, suffix)) = name.rsplit_once('.')
        && let Some(digits) = suffix.strip_prefix('z')
        && !digits.is_empty()
        && digits.bytes().all(|b| b.is_ascii_digit())
    {
        return path.with_file_name(format!("{stem}.zip"));
    }

    path.to_path_buf()
}

pub fn split_volume_name(base: &Path, n: usize) -> PathBuf {
    let stem = base.file_stem().and_then(|s| s.to_str()).unwrap_or("archive");
    let name = if n < 100 { format!("{stem}.z{n:02}") } else { format!("{stem}.z{n}") };
    base.with_file_name(name)
}

fn discover_split(base: &Path) -> Result<Option<VolumeSet>> {
    let first = split_volume_name(base, 1);
    if !first.exists() {
        return Ok(None);
    }

    let mut paths = Vec::new();
    let mut n = 1;
    loop {
        let candidate = split_volume_name(base, n);
        if !candidate.exists() {
            break;
        }
        paths.push(candidate);
        n += 1;
        if n > 100_000 {
            return Err(Error::malformed("split archive has an implausible number of segments"));
        }
    }

    if !base.exists() {
        return Err(Error::malformed(format!(
            "split archive is missing its final segment {}; segments {} through {} were found",
            base.display(),
            split_volume_name(base, 1).display(),
            split_volume_name(base, paths.len()).display(),
        )));
    }
    paths.push(base.to_path_buf());

    Ok(Some(VolumeSet { layout: layout_for(&paths, Scheme::Split)?, paths }))
}

fn discover_raw_split(base: &Path) -> Result<Option<VolumeSet>> {
    let piece = |n: usize| {
        let name = format!("{}.{n:03}", base.file_name().unwrap_or_default().to_string_lossy());
        base.with_file_name(name)
    };

    if !piece(1).exists() {
        return Ok(None);
    }

    let mut paths = Vec::new();
    let mut n = 1;
    while piece(n).exists() {
        paths.push(piece(n));
        n += 1;
        if n > 100_000 {
            return Err(Error::malformed("raw split has an implausible number of pieces"));
        }
    }

    Ok(Some(VolumeSet { layout: layout_for(&paths, Scheme::RawSplit)?, paths }))
}

fn layout_for(paths: &[PathBuf], scheme: Scheme) -> Result<VolumeLayout> {
    let mut starts = Vec::with_capacity(paths.len());
    let mut lengths = Vec::with_capacity(paths.len());
    let mut cursor = 0u64;

    for path in paths {
        let len = fs::metadata(path)?.len();
        starts.push(cursor);
        lengths.push(len);
        cursor += len;
    }

    Ok(VolumeLayout { starts, lengths, scheme })
}

pub struct SegmentedReader {
    paths: Vec<PathBuf>,
    layout: VolumeLayout,
    open: Option<(usize, File)>,
    position: u64,
}

impl SegmentedReader {
    fn open(paths: Vec<PathBuf>, layout: VolumeLayout) -> Result<Self> {
        Ok(SegmentedReader { paths, layout, open: None, position: 0 })
    }

    pub fn layout(&self) -> &VolumeLayout {
        &self.layout
    }

    fn volume_at(&self, offset: u64) -> Option<usize> {
        if offset >= self.layout.total_len() {
            return None;
        }
        Some(self.layout.locate_index(offset))
    }

    fn focus(&mut self, index: usize, local: u64) -> io::Result<&mut File> {
        let needs_open = !matches!(&self.open, Some((i, _)) if *i == index);
        if needs_open {
            let file = File::open(&self.paths[index])?;
            self.open = Some((index, file));
        }
        let (_, file) = self.open.as_mut().expect("just opened");
        file.seek(SeekFrom::Start(local))?;
        Ok(file)
    }
}

impl VolumeLayout {
    fn locate_index(&self, global: u64) -> usize {
        self.starts.partition_point(|&s| s <= global).saturating_sub(1)
    }
}

impl Read for SegmentedReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        let Some(index) = self.volume_at(self.position) else {
            return Ok(0);
        };

        let start = self.layout.starts[index];
        let len = self.layout.lengths[index];
        let local = self.position - start;
        let room = (len - local).min(buf.len() as u64) as usize;
        if room == 0 {
            return Ok(0);
        }

        let file = self.focus(index, local)?;
        let n = file.read(&mut buf[..room])?;
        self.position += n as u64;

        if n == 0 && self.position < self.layout.total_len() {
            self.position = start + len;
            return self.read(buf);
        }

        Ok(n)
    }
}

impl Seek for SegmentedReader {
    fn seek(&mut self, from: SeekFrom) -> io::Result<u64> {
        let total = self.layout.total_len() as i64;
        let target = match from {
            SeekFrom::Start(n) => n as i64,
            SeekFrom::Current(d) => self.position as i64 + d,
            SeekFrom::End(d) => total + d,
        };
        if target < 0 {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "seek before start of archive"));
        }
        self.position = target as u64;
        Ok(self.position)
    }
}

pub trait Sink: Write + Seek {
    fn begin_record(&mut self, len: u64) -> io::Result<()> {
        let _ = len;
        Ok(())
    }

    fn locate(&self, global: u64) -> (u32, u64) {
        (0, global)
    }

    fn disks(&self) -> u32 {
        1
    }
}

#[derive(Debug)]
pub struct SingleSink<W>(pub W);

impl<W: Write> Write for SingleSink<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.write(buf)
    }
    fn write_all(&mut self, buf: &[u8]) -> io::Result<()> {
        self.0.write_all(buf)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.0.flush()
    }
}

impl<W: Seek> Seek for SingleSink<W> {
    fn seek(&mut self, from: SeekFrom) -> io::Result<u64> {
        self.0.seek(from)
    }
}

impl<W: Write + Seek> Sink for SingleSink<W> {}

pub struct VolumeSink {
    base: PathBuf,
    volume_size: u64,
    files: Vec<File>,
    paths: Vec<PathBuf>,
    starts: Vec<u64>,
    lengths: Vec<u64>,
    position: u64,
}

impl VolumeSink {
    pub fn create(base: &Path, volume_size: u64) -> Result<Self> {
        let volume_size = volume_size.clamp(MIN_VOLUME_SIZE, MAX_VOLUME_SIZE);

        let mut sink =
            VolumeSink { base: base.to_path_buf(), volume_size, files: Vec::new(), paths: Vec::new(), starts: Vec::new(), lengths: Vec::new(), position: 0 };
        sink.add_volume()?;

        sink.write_all(&SPANNING_SIGNATURE.to_le_bytes())?;

        Ok(sink)
    }

    fn add_volume(&mut self) -> io::Result<()> {
        let index = self.files.len() + 1;
        let path = split_volume_name(&self.base, index);
        let file = File::create(&path)?;

        let start = self.starts.last().copied().unwrap_or(0) + self.lengths.last().copied().unwrap_or(0);
        self.starts.push(start);
        self.lengths.push(0);
        self.paths.push(path);
        self.files.push(file);
        Ok(())
    }

    fn current_len(&self) -> u64 {
        self.lengths.last().copied().unwrap_or(0)
    }

    fn total_len(&self) -> u64 {
        self.starts.last().copied().unwrap_or(0) + self.current_len()
    }

    pub fn finish(mut self) -> Result<Vec<PathBuf>> {
        if self.files.len() == 1 {
            self.seek(SeekFrom::Start(0))?;
            self.write_all(&TEMP_SPANNING_MARKER.to_le_bytes())?;
        }

        for file in &mut self.files {
            file.flush()?;
        }
        self.files.clear();

        let last = self.paths.pop().expect("at least one segment");
        let _ = fs::remove_file(&self.base);
        fs::rename(&last, &self.base)?;
        self.paths.push(self.base.clone());

        Ok(std::mem::take(&mut self.paths))
    }

    fn volume_at(&self, offset: u64) -> usize {
        self.starts.partition_point(|&s| s <= offset).saturating_sub(1)
    }
}

impl Write for VolumeSink {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }

        let appending = self.position >= self.total_len();

        if appending {
            if self.current_len() >= self.volume_size {
                self.add_volume()?;
            }

            let room = (self.volume_size - self.current_len()).min(buf.len() as u64) as usize;
            let index = self.files.len() - 1;
            let local = self.current_len();

            let file = &mut self.files[index];
            file.seek(SeekFrom::Start(local))?;
            let n = file.write(&buf[..room.max(1).min(buf.len())])?;

            let last = self.lengths.len() - 1;
            self.lengths[last] += n as u64;
            self.position += n as u64;
            return Ok(n);
        }

        let index = self.volume_at(self.position);
        let local = self.position - self.starts[index];
        let room = (self.lengths[index] - local).min(buf.len() as u64) as usize;

        let file = &mut self.files[index];
        file.seek(SeekFrom::Start(local))?;
        let n = file.write(&buf[..room])?;
        self.position += n as u64;
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        for file in &mut self.files {
            file.flush()?;
        }
        Ok(())
    }
}

impl Seek for VolumeSink {
    fn seek(&mut self, from: SeekFrom) -> io::Result<u64> {
        let target = match from {
            SeekFrom::Start(n) => n as i64,
            SeekFrom::Current(d) => self.position as i64 + d,
            SeekFrom::End(d) => self.total_len() as i64 + d,
        };
        if target < 0 {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "seek before start of archive"));
        }
        self.position = target as u64;
        Ok(self.position)
    }
}

impl Sink for VolumeSink {
    fn begin_record(&mut self, len: u64) -> io::Result<()> {
        if self.position < self.total_len() {
            return Ok(());
        }
        if self.current_len() > 0 && self.current_len() + len > self.volume_size {
            self.add_volume()?;
            self.position = self.total_len();
        }
        Ok(())
    }

    fn locate(&self, global: u64) -> (u32, u64) {
        let index = self.volume_at(global);
        (index as u32, global - self.starts[index])
    }

    fn disks(&self) -> u32 {
        self.files.len().max(1) as u32
    }
}
