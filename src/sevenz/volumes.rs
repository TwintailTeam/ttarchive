use std::fs::{self, File, OpenOptions};
use std::io::{self, BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use crate::sevenz::source::Source;
use crate::utils::error::{Error, Result};
use crate::utils::io::SectionReader;

pub const MIN_VOLUME_SIZE: u64 = 64 * 1024;

const MIN_DIGITS: usize = 3;

pub fn volume_name(base: &Path, index: usize) -> PathBuf {
    let name = base.file_name().unwrap_or_default().to_string_lossy().into_owned();
    base.with_file_name(format!("{name}.{index:0MIN_DIGITS$}", MIN_DIGITS = MIN_DIGITS))
}

pub fn base_name(path: &Path) -> PathBuf {
    let name = path.file_name().unwrap_or_default().to_string_lossy().into_owned();

    match name.rsplit_once('.') {
        Some((stem, digits)) if digits.len() >= MIN_DIGITS && digits.bytes().all(|b| b.is_ascii_digit()) && !stem.is_empty() => path.with_file_name(stem),
        _ => path.to_path_buf(),
    }
}

#[derive(Debug, Clone)]
pub struct VolumeSet {
    paths: Vec<PathBuf>,
    starts: Vec<u64>,
    lengths: Vec<u64>,
}

impl VolumeSet {
    pub fn discover(path: &Path) -> Result<Option<Self>> {
        let base = base_name(path);
        let first = volume_name(&base, 1);
        if !first.exists() {
            return Ok(None);
        }

        let found = numbered_beside(&base)?;
        let last = found.last().copied().unwrap_or(0);

        for index in 1..=last {
            if !found.contains(&index) {
                return Err(Error::malformed(format!("7z volume set is missing {}; volumes 1 to {last} were expected", volume_name(&base, index).display())));
            }
        }

        let paths: Vec<PathBuf> = (1..=last).map(|index| volume_name(&base, index)).collect();
        Ok(Some(VolumeSet::over(paths)?))
    }

    fn over(paths: Vec<PathBuf>) -> Result<Self> {
        let mut starts = Vec::with_capacity(paths.len());
        let mut lengths = Vec::with_capacity(paths.len());
        let mut cursor = 0u64;

        for path in &paths {
            let len = fs::metadata(path)?.len();
            starts.push(cursor);
            lengths.push(len);
            cursor += len;
        }

        Ok(VolumeSet { paths, starts, lengths })
    }

    pub fn paths(&self) -> &[PathBuf] {
        &self.paths
    }

    pub fn len(&self) -> usize {
        self.paths.len()
    }

    pub fn is_empty(&self) -> bool {
        self.paths.is_empty()
    }

    pub fn total_len(&self) -> u64 {
        self.starts.last().copied().unwrap_or(0) + self.lengths.last().copied().unwrap_or(0)
    }

    fn locate(&self, global: u64) -> (usize, u64) {
        let index = self.starts.partition_point(|&start| start <= global).saturating_sub(1);
        (index, global - self.starts[index])
    }
}

#[derive(Debug, Clone)]
pub struct VolumeSource {
    set: VolumeSet,
}

impl VolumeSource {
    pub fn new(set: VolumeSet) -> Self {
        VolumeSource { set }
    }

    pub fn set(&self) -> &VolumeSet {
        &self.set
    }
}

impl Source for VolumeSource {
    fn open(&self, start: u64, len: u64) -> Result<Box<dyn Read + Send + '_>> {
        self.check_range(start, len)?;

        let mut pieces = Vec::new();
        let mut at = start;
        let mut left = len;

        while left > 0 {
            let (index, offset) = self.set.locate(at);
            let take = (self.set.lengths[index] - offset).min(left);
            if take == 0 {
                break;
            }

            pieces.push((self.set.paths[index].clone(), offset, take));
            at += take;
            left -= take;
        }

        let span = SpanReader { pieces, at: 0, open: None };
        Ok(Box::new(BufReader::with_capacity(crate::sevenz::source::STREAM_BUFFER.min(len.max(1) as usize), span)))
    }

    fn size(&self) -> u64 {
        self.set.total_len()
    }
}

struct SpanReader {
    pieces: Vec<(PathBuf, u64, u64)>,
    at: usize,
    open: Option<SectionReader<File>>,
}

impl Read for SpanReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        while self.at < self.pieces.len() {
            if self.open.is_none() {
                let (path, offset, len) = &self.pieces[self.at];
                self.open = Some(SectionReader::new(File::open(path)?, *offset, *len)?);
            }

            let reader = self.open.as_mut().expect("a piece is open");
            match reader.read(buf)? {
                0 => {
                    self.open = None;
                    self.at += 1;
                }
                n => return Ok(n),
            }
        }

        Ok(0)
    }
}

fn numbered_beside(base: &Path) -> Result<Vec<usize>> {
    let directory = match base.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.to_path_buf(),
        _ => PathBuf::from("."),
    };
    let prefix = format!("{}.", base.file_name().unwrap_or_default().to_string_lossy());

    let mut found = Vec::new();
    for entry in fs::read_dir(&directory)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();

        let Some(digits) = name.strip_prefix(&prefix) else { continue };
        if digits.len() < MIN_DIGITS || !digits.bytes().all(|b| b.is_ascii_digit()) {
            continue;
        }
        if let Ok(index) = digits.parse::<usize>()
            && index > 0
        {
            found.push(index);
        }
    }

    found.sort_unstable();
    found.dedup();
    Ok(found)
}

pub struct VolumeSink {
    base: PathBuf,
    volume_size: u64,
    paths: Vec<PathBuf>,
    lengths: Vec<u64>,
    open: Option<(usize, File)>,
    position: u64,
}

impl VolumeSink {
    pub fn create(base: &Path, volume_size: u64) -> Result<Self> {
        if base.is_file() {
            fs::remove_file(base)?;
        }

        let mut sink = VolumeSink {
            base: base.to_path_buf(),
            volume_size: volume_size.max(MIN_VOLUME_SIZE),
            paths: Vec::new(),
            lengths: Vec::new(),
            open: None,
            position: 0,
        };
        sink.add_volume()?;
        Ok(sink)
    }

    pub fn finish(mut self) -> Result<Vec<PathBuf>> {
        self.flush()?;
        self.open = None;

        for index in numbered_beside(&self.base)? {
            if index > self.paths.len() {
                fs::remove_file(volume_name(&self.base, index))?;
            }
        }

        Ok(std::mem::take(&mut self.paths))
    }

    fn add_volume(&mut self) -> io::Result<()> {
        let path = volume_name(&self.base, self.paths.len() + 1);
        let file = File::create(&path)?;

        self.open = Some((self.paths.len(), file));
        self.paths.push(path);
        self.lengths.push(0);
        Ok(())
    }

    fn total_len(&self) -> u64 {
        self.lengths.iter().sum()
    }

    fn file_for(&mut self, index: usize) -> io::Result<&mut File> {
        if self.open.as_ref().is_none_or(|(open, _)| *open != index) {
            if let Some((_, file)) = &mut self.open {
                file.flush()?;
            }
            let file = OpenOptions::new().write(true).open(&self.paths[index])?;
            self.open = Some((index, file));
        }

        Ok(&mut self.open.as_mut().expect("a volume is open").1)
    }
}

impl Write for VolumeSink {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }

        if self.position < self.total_len() {
            let mut index = 0usize;
            let mut start = 0u64;
            while index + 1 < self.lengths.len() && start + self.lengths[index] <= self.position {
                start += self.lengths[index];
                index += 1;
            }

            let offset = self.position - start;
            let room = (self.lengths[index] - offset).min(buf.len() as u64) as usize;
            let file = self.file_for(index)?;
            file.seek(SeekFrom::Start(offset))?;
            let written = file.write(&buf[..room])?;
            self.position += written as u64;
            return Ok(written);
        }

        let last = self.lengths.len() - 1;
        if self.lengths[last] >= self.volume_size {
            self.add_volume()?;
        }

        let index = self.lengths.len() - 1;
        let room = (self.volume_size - self.lengths[index]).min(buf.len() as u64) as usize;
        let offset = self.lengths[index];

        let file = self.file_for(index)?;
        file.seek(SeekFrom::Start(offset))?;
        let written = file.write(&buf[..room])?;

        self.lengths[index] += written as u64;
        self.position += written as u64;
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        if let Some((_, file)) = &mut self.open {
            file.flush()?;
        }
        Ok(())
    }
}

impl Seek for VolumeSink {
    fn seek(&mut self, from: SeekFrom) -> io::Result<u64> {
        let target = match from {
            SeekFrom::Start(n) => n as i64,
            SeekFrom::Current(delta) => self.position as i64 + delta,
            SeekFrom::End(delta) => self.total_len() as i64 + delta,
        };

        if target < 0 {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "seek before the start of a 7z volume set"));
        }
        if (target as u64) > self.total_len() {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "seek past the end of a 7z volume set"));
        }

        self.position = target as u64;
        Ok(self.position)
    }
}
