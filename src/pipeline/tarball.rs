use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::Path;

use crate::codecs::{Level, bzip2, compress, gzip, lzip, lzma, xz, zstd};
use crate::pipeline::layout::{self, Claim, Claims, Rejected};
use crate::pipeline::{CreateOptions, CreateSummary, ExtractOptions, ExtractSummary, MEMORY_BUDGET, pool, thread_count};
use crate::platform::{EntryKind, sys};
use crate::tar::header::{Format, Kind};
use crate::tar::{TarReader, TarWriter};
use crate::utils::error::{Error, Result, Unsupported};
use crate::utils::io::COPY_BUF;
use crate::utils::progress::Reporter;

/// How a tarball's bytes are wrapped, if at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wrapper {
    None,
    Gzip,
    Bzip2,
    Xz,
    Zstd,
    Lzma,
    Compress,
    Lzip,
}

impl Wrapper {
    /// Every wrapper, so callers that must cover all of them can iterate.
    ///
    /// A new variant has to be added here as well as to the matches below,
    /// which the compiler will not let you skip.
    pub const ALL: [Wrapper; 8] = [Wrapper::None, Wrapper::Gzip, Wrapper::Bzip2, Wrapper::Xz, Wrapper::Zstd, Wrapper::Lzma, Wrapper::Compress, Wrapper::Lzip];

    pub fn can_write(self) -> bool {
        match self {
            Wrapper::None | Wrapper::Gzip | Wrapper::Bzip2 | Wrapper::Lzma | Wrapper::Xz | Wrapper::Zstd | Wrapper::Lzip => true,
            Wrapper::Compress => false,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Wrapper::None => "tar",
            Wrapper::Gzip => "tar.gz",
            Wrapper::Bzip2 => "tar.bz2",
            Wrapper::Xz => "tar.xz",
            Wrapper::Zstd => "tar.zst",
            Wrapper::Lzma => "tar.lzma",
            Wrapper::Compress => "tar.Z",
            Wrapper::Lzip => "tar.lz",
        }
    }

    fn unsupported(self) -> Error {
        Error::Unsupported(Unsupported::Other(match self {
            Wrapper::Compress => "writing .tar.Z (the Unix compress encoder is not implemented yet)",
            _ => "this tarball wrapper",
        }))
    }

    fn streams(self) -> bool {
        match self {
            Wrapper::None | Wrapper::Gzip | Wrapper::Bzip2 | Wrapper::Lzma | Wrapper::Xz | Wrapper::Zstd | Wrapper::Lzip | Wrapper::Compress => true,
        }
    }

    fn stream<'a, R: Read + 'a>(self, input: R) -> Result<Box<dyn Read + 'a>> {
        match self {
            Wrapper::None => Ok(Box::new(input)),
            Wrapper::Gzip => Ok(Box::new(gzip::GzipReader::new(input))),
            Wrapper::Bzip2 => Ok(Box::new(bzip2::Bzip2Reader::new(input))),
            Wrapper::Lzma => Ok(Box::new(lzma::alone::reader(input)?)),
            Wrapper::Xz => Ok(Box::new(xz::Reader::new(input, 0))),
            Wrapper::Zstd => Ok(Box::new(zstd::Reader::new(input, 0))),
            Wrapper::Lzip => Ok(Box::new(lzip::Reader::new(input))),
            Wrapper::Compress => Ok(Box::new(compress::Reader::new(input))),
        }
    }

    fn splits(self) -> bool {
        match self {
            Wrapper::Gzip | Wrapper::Bzip2 => true,
            Wrapper::None | Wrapper::Xz | Wrapper::Zstd | Wrapper::Lzma | Wrapper::Compress | Wrapper::Lzip => false,
        }
    }

    fn worker_memory(self, level: Level) -> usize {
        match self {
            Wrapper::Bzip2 => 24 * 100_000 * level.bzip2_block_size() as usize,
            _ => 3 << 20,
        }
    }

    fn compress_piece(self, piece: &[u8], level: Level) -> Result<Vec<u8>> {
        match self {
            Wrapper::Gzip => gzip::compress(piece, level, &gzip::Member::default()),
            Wrapper::Bzip2 => bzip2::compress(piece, level.bzip2_block_size()),
            Wrapper::Xz => xz::encode::compress_at(piece, search_depth(level), level),
            Wrapper::Zstd => zstd::encode::compress_at(piece, true, search_depth(level)),
            Wrapper::None | Wrapper::Lzma | Wrapper::Compress | Wrapper::Lzip => Err(self.unsupported()),
        }
    }

    fn decode(self, packed: Vec<u8>) -> Result<Vec<u8>> {
        let hint = packed.len().saturating_mul(4);
        match self {
            Wrapper::None => Ok(packed),
            Wrapper::Gzip => gzip::decompress(&packed, hint),
            Wrapper::Bzip2 => bzip2::decompress(&packed, hint),
            Wrapper::Xz => xz::decompress(&packed, 0),
            Wrapper::Zstd => zstd::decompress(&packed, 0),
            Wrapper::Lzma => lzma::alone::decompress(&packed, hint),
            Wrapper::Compress => compress::decompress(&packed, hint),
            Wrapper::Lzip => lzip::decompress(&packed, hint),
        }
    }
}

const SPARSE_MINIMUM: u64 = 1 << 20;

struct Scratch {
    path: std::path::PathBuf,
}

impl Scratch {
    fn create(beside: &Path) -> Option<(Self, File)> {
        let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
        let path = beside.join(format!(".ttarchive-scratch-{}-{stamp}", std::process::id()));

        let file = File::create(&path).ok()?;
        Some((Scratch { path }, file))
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

enum Source {
    Plain(std::path::PathBuf),
    Buffered(Vec<u8>),
    Spilled(Scratch),
    Reopen(std::path::PathBuf, Wrapper),
}

impl Source {
    fn once(path: &Path, wrapper: Wrapper) -> Result<Self> {
        if wrapper == Wrapper::None {
            return Ok(Source::Plain(path.to_path_buf()));
        }
        if wrapper.streams() {
            return Ok(Source::Reopen(path.to_path_buf(), wrapper));
        }
        Ok(Source::Buffered(wrapper.decode(fs::read(path)?)?))
    }

    fn repeatable(path: &Path, wrapper: Wrapper, scratch: &Path) -> Result<Self> {
        if wrapper == Wrapper::None {
            return Ok(Source::Plain(path.to_path_buf()));
        }
        if !wrapper.streams() {
            return Ok(Source::Buffered(wrapper.decode(fs::read(path)?)?));
        }

        let mut reader = wrapper.stream(BufReader::with_capacity(COPY_BUF, File::open(path)?))?;
        let mut held = Vec::new();
        let mut chunk = vec![0u8; COPY_BUF];

        loop {
            let n = reader.read(&mut chunk)?;
            if n == 0 {
                return Ok(Source::Buffered(held));
            }
            held.extend_from_slice(&chunk[..n]);
            if held.len() as u64 > MEMORY_BUDGET {
                break;
            }
        }

        let Some((scratch, file)) = Scratch::create(scratch) else {
            return Ok(Source::Reopen(path.to_path_buf(), wrapper));
        };

        let mut out = BufWriter::with_capacity(COPY_BUF, file);
        out.write_all(&held)?;
        held = Vec::new();
        drop(held);
        std::io::copy(&mut reader, &mut out)?;
        out.flush()?;

        Ok(Source::Spilled(scratch))
    }

    fn random_access(&self) -> Option<Access<'_>> {
        match self {
            Source::Plain(path) | Source::Reopen(path, Wrapper::None) => Some(Access::File(path)),
            Source::Spilled(scratch) => Some(Access::File(&scratch.path)),
            Source::Buffered(plain) => Some(Access::Memory(plain)),
            Source::Reopen(..) => None,
        }
    }

    fn reader(&self) -> Result<Box<dyn Read + '_>> {
        match self {
            Source::Plain(path) => Ok(Box::new(BufReader::with_capacity(COPY_BUF, File::open(path)?))),
            Source::Buffered(plain) => Ok(Box::new(plain.as_slice())),
            Source::Spilled(scratch) => Ok(Box::new(BufReader::with_capacity(COPY_BUF, File::open(&scratch.path)?))),
            Source::Reopen(path, wrapper) => wrapper.stream(BufReader::with_capacity(COPY_BUF, File::open(path)?)),
        }
    }
}

#[derive(Clone, Copy)]
enum Access<'a> {
    File(&'a Path),
    Memory(&'a [u8]),
}

impl Access<'_> {
    fn open(&self) -> Result<Box<dyn ReadSeek + Send + '_>> {
        Ok(match self {
            Access::File(path) => Box::new(BufReader::with_capacity(COPY_BUF, File::open(path)?)),
            Access::Memory(plain) => Box::new(std::io::Cursor::new(*plain)),
        })
    }
}

trait ReadSeek: Read + std::io::Seek {}
impl<T: Read + std::io::Seek> ReadSeek for T {}

fn search_depth(level: Level) -> usize {
    match level {
        Level::None | Level::Fast => 8,
        Level::Default => 32,
        Level::Best => 128,
    }
}

trait Encoding: Sized {
    fn start(out: BufWriter<File>, depth: usize, level: Level) -> Result<Self>;
    fn push(&mut self, bytes: &[u8]) -> Result<()>;
    fn finish(self) -> Result<BufWriter<File>>;

    fn once(plain: &[u8], depth: usize, level: Level) -> Result<Vec<u8>>;
    fn threshold(level: Level) -> usize;
}

impl Encoding for lzma::alone::Writer<BufWriter<File>> {
    fn start(out: BufWriter<File>, depth: usize, level: Level) -> Result<Self> {
        lzma::alone::Writer::new(out, depth, level)
    }
    fn push(&mut self, bytes: &[u8]) -> Result<()> {
        lzma::alone::Writer::push(self, bytes)
    }
    fn finish(self) -> Result<BufWriter<File>> {
        lzma::alone::Writer::finish(self)
    }

    fn once(plain: &[u8], depth: usize, level: Level) -> Result<Vec<u8>> {
        lzma::alone::compress_at(plain, depth, level)
    }
    fn threshold(level: Level) -> usize {
        lzma::encode::dictionary_at(usize::MAX, level) as usize
    }
}

impl Encoding for zstd::encode::Writer<BufWriter<File>> {
    fn start(out: BufWriter<File>, depth: usize, _level: Level) -> Result<Self> {
        zstd::encode::Writer::new(out, true, depth)
    }
    fn push(&mut self, bytes: &[u8]) -> Result<()> {
        zstd::encode::Writer::push(self, bytes)
    }
    fn finish(self) -> Result<BufWriter<File>> {
        zstd::encode::Writer::finish(self)
    }

    fn once(plain: &[u8], depth: usize, _level: Level) -> Result<Vec<u8>> {
        zstd::encode::compress_at(plain, true, depth)
    }
    fn threshold(_level: Level) -> usize {
        zstd::encode::WINDOW_SIZE
    }
}

impl Encoding for lzip::Writer<BufWriter<File>> {
    fn start(out: BufWriter<File>, depth: usize, level: Level) -> Result<Self> {
        lzip::Writer::new(out, depth, level)
    }
    fn push(&mut self, bytes: &[u8]) -> Result<()> {
        lzip::Writer::push(self, bytes)
    }
    fn finish(self) -> Result<BufWriter<File>> {
        lzip::Writer::finish(self)
    }

    fn once(plain: &[u8], depth: usize, level: Level) -> Result<Vec<u8>> {
        lzip::compress_at(plain, depth, level)
    }
    fn threshold(level: Level) -> usize {
        lzma::encode::dictionary_at(usize::MAX, level) as usize
    }
}

impl Encoding for xz::encode::Writer<BufWriter<File>> {
    fn start(out: BufWriter<File>, depth: usize, level: Level) -> Result<Self> {
        xz::encode::Writer::new(out, depth, level)
    }
    fn push(&mut self, bytes: &[u8]) -> Result<()> {
        xz::encode::Writer::push(self, bytes)
    }
    fn finish(self) -> Result<BufWriter<File>> {
        xz::encode::Writer::finish(self)
    }

    fn once(plain: &[u8], depth: usize, level: Level) -> Result<Vec<u8>> {
        xz::encode::compress_at(plain, depth, level)
    }
    fn threshold(level: Level) -> usize {
        lzma::encode::dictionary_at(usize::MAX, level) as usize
    }
}

struct Staged<E: Encoding> {
    held: Vec<u8>,
    stream: Option<E>,
    file: Option<BufWriter<File>>,
    depth: usize,
    level: Level,
}

impl<E: Encoding> Staged<E> {
    fn new(file: BufWriter<File>, depth: usize, level: Level) -> Self {
        Staged { held: Vec::new(), stream: None, file: Some(file), depth, level }
    }

    fn push(&mut self, bytes: &[u8]) -> Result<()> {
        if let Some(stream) = &mut self.stream {
            return stream.push(bytes);
        }

        self.held.extend_from_slice(bytes);
        if self.held.len() <= E::threshold(self.level) {
            return Ok(());
        }

        let file = self.file.take().ok_or_else(|| Error::malformed("internal: the tarball sink was already finished"))?;
        let mut stream = E::start(file, self.depth, self.level)?;
        stream.push(&std::mem::take(&mut self.held))?;
        self.stream = Some(stream);
        Ok(())
    }

    fn finish(self) -> Result<()> {
        let mut file = match self.stream {
            Some(stream) => stream.finish()?,
            None => {
                let mut file = self.file.ok_or_else(|| Error::malformed("internal: the tarball sink was already finished"))?;
                file.write_all(&E::once(&self.held, self.depth, self.level)?)?;
                file
            }
        };
        file.flush()?;
        Ok(())
    }
}

struct Parallel {
    out: BufWriter<File>,
    wrapper: Wrapper,
    level: Level,
    filling: Vec<u8>,
    queued: Vec<Vec<u8>>,
    chunk: usize,
    threads: usize,
}

impl Parallel {
    const PIECE: usize = 2 << 20;

    fn new(out: BufWriter<File>, wrapper: Wrapper, level: Level, threads: Option<usize>) -> Self {
        let wanted = thread_count(threads, usize::MAX);

        let each = 2 * Self::PIECE + wrapper.worker_memory(level);
        let threads = ((MEMORY_BUDGET as usize / each).max(1)).min(wanted);

        Parallel { out, wrapper, level, filling: Vec::with_capacity(Self::PIECE), queued: Vec::new(), chunk: Self::PIECE, threads }
    }

    fn wave(&mut self) -> Result<()> {
        if self.queued.is_empty() {
            return Ok(());
        }

        let (wrapper, level) = (self.wrapper, self.level);
        let pieces = std::mem::take(&mut self.queued);
        let packed: Vec<std::sync::Mutex<Option<Vec<u8>>>> = (0..pieces.len()).map(|_| std::sync::Mutex::new(None)).collect();

        pool::for_each(
            pieces.len(),
            self.threads.min(pieces.len()),
            || Ok(()),
            |(), slot| {
                let done = wrapper.compress_piece(&pieces[slot], level)?;
                *packed[slot].lock().unwrap_or_else(|p| p.into_inner()) = Some(done);
                Ok(())
            },
        )?;

        for slot in packed {
            let done = slot.into_inner().unwrap_or_else(|p| p.into_inner()).ok_or_else(|| Error::malformed("internal: a piece was not compressed"))?;
            self.out.write_all(&done)?;
        }

        Ok(())
    }

    fn finish(mut self) -> Result<()> {
        if !self.filling.is_empty() {
            let piece = std::mem::take(&mut self.filling);
            self.queued.push(piece);
        }
        self.wave()?;
        self.out.flush()?;
        Ok(())
    }
}

impl Write for Parallel {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let mut rest = buf;

        while !rest.is_empty() {
            let room = self.chunk - self.filling.len();
            let take = room.min(rest.len());
            self.filling.extend_from_slice(&rest[..take]);
            rest = &rest[take..];

            if self.filling.len() == self.chunk {
                let piece = std::mem::replace(&mut self.filling, Vec::with_capacity(self.chunk));
                self.queued.push(piece);

                if self.queued.len() >= self.threads {
                    self.wave()?;
                }
            }
        }

        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

enum Sink {
    Plain(BufWriter<File>),
    Split(Box<Parallel>),
    Lzma(Box<Staged<lzma::alone::Writer<BufWriter<File>>>>),
    Xz(Box<Staged<xz::encode::Writer<BufWriter<File>>>>),
    Zstd(Box<Staged<zstd::encode::Writer<BufWriter<File>>>>),
    Lzip(Box<Staged<lzip::Writer<BufWriter<File>>>>),
}

impl Write for Sink {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        match self {
            Sink::Plain(inner) => inner.write(buf),
            Sink::Split(inner) => inner.write(buf),
            Sink::Lzma(inner) => {
                inner.push(buf).map_err(std::io::Error::other)?;
                Ok(buf.len())
            }
            Sink::Xz(inner) => {
                inner.push(buf).map_err(std::io::Error::other)?;
                Ok(buf.len())
            }
            Sink::Zstd(inner) => {
                inner.push(buf).map_err(std::io::Error::other)?;
                Ok(buf.len())
            }
            Sink::Lzip(inner) => {
                inner.push(buf).map_err(std::io::Error::other)?;
                Ok(buf.len())
            }
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        match self {
            Sink::Plain(inner) => inner.flush(),
            Sink::Split(inner) => inner.flush(),
            Sink::Lzma(..) | Sink::Xz(..) | Sink::Zstd(..) | Sink::Lzip(..) => Ok(()),
        }
    }
}

impl Sink {
    fn open(path: &Path, wrapper: Wrapper, level: Level, threads: Option<usize>) -> Result<Self> {
        let file = BufWriter::with_capacity(COPY_BUF, File::create(path)?);

        Ok(match wrapper {
            Wrapper::None => Sink::Plain(file),
            w if w.splits() => Sink::Split(Box::new(Parallel::new(file, wrapper, level, threads))),
            Wrapper::Xz => Sink::Xz(Box::new(Staged::new(file, search_depth(level), level))),
            Wrapper::Zstd => Sink::Zstd(Box::new(Staged::new(file, search_depth(level), level))),
            Wrapper::Lzma => Sink::Lzma(Box::new(Staged::new(file, search_depth(level), level))),
            Wrapper::Lzip => Sink::Lzip(Box::new(Staged::new(file, search_depth(level), level))),
            other => return Err(other.unsupported()),
        })
    }

    fn finish(self) -> Result<()> {
        match self {
            Sink::Plain(mut file) => {
                file.flush()?;
                Ok(())
            }
            Sink::Split(inner) => inner.finish(),
            Sink::Lzma(inner) => inner.finish(),
            Sink::Xz(inner) => inner.finish(),
            Sink::Zstd(inner) => inner.finish(),
            Sink::Lzip(inner) => inner.finish(),
        }
    }
}

pub fn create<I, P>(archive: &Path, inputs: I, wrapper: Wrapper, options: &CreateOptions, reporter: &Reporter) -> Result<CreateSummary>
where
    I: IntoIterator<Item = P>,
    P: AsRef<Path>,
{
    if !wrapper.can_write() {
        return Err(wrapper.unsupported());
    }
    refuse_options_a_tarball_cannot_keep(options)?;

    let mut sources = Vec::new();
    let mut specials = 0u64;
    for input in inputs {
        collect(input.as_ref(), input.as_ref(), &mut sources, options, &mut specials)?;
    }
    sources.sort_by(|a, b| a.name.cmp(&b.name));

    let links = find_hard_links(&sources);

    let total: u64 = sources.iter().enumerate().filter(|(i, s)| s.meta.kind == EntryKind::File && !links.contains_key(i)).map(|(_, s)| s.size).sum();
    reporter.set_totals(total, sources.len() as u64);

    let mut summary = CreateSummary { volumes: 1, specials, ..CreateSummary::default() };

    {
        let sink = Sink::open(archive, wrapper, options.level, options.threads)?;
        let mut tar = TarWriter::with_format(sink, Format::Pax);

        for (index, source) in sources.iter().enumerate() {
            let (name, path, meta, size) = (&source.name, &source.path, &source.meta, &source.size);
            reporter.start_entry(name);

            if let Some(target) = links.get(&index) {
                tar.add_hard_link(name, meta, target)?;
                summary.hardlinks += 1;
                reporter.finish_entry();
                continue;
            }

            match meta.kind {
                EntryKind::Directory => {
                    tar.start_entry(name, meta, 0, "")?;
                    summary.directories += 1;
                }
                EntryKind::Symlink => {
                    let target = sys::read_symlink_target(path)?;
                    tar.start_entry(name, meta, 0, &String::from_utf8_lossy(&target))?;
                    summary.symlinks += 1;
                }
                EntryKind::File if options.sparse && *size >= SPARSE_MINIMUM => {
                    let data = fs::read(path)?;
                    tar.add_sparse(name, meta, &data)?;
                    reporter.add_bytes(data.len() as u64);
                    summary.files += 1;
                    summary.bytes += data.len() as u64;
                }
                EntryKind::File => {
                    tar.start_entry(name, meta, *size, "")?;

                    let mut file = File::open(path)?;
                    let mut buffer = vec![0u8; COPY_BUF];
                    let mut written = 0u64;

                    while written < *size {
                        let want = ((*size - written) as usize).min(buffer.len());
                        let n = file.read(&mut buffer[..want])?;
                        if n == 0 {
                            break;
                        }
                        tar.write_data(&buffer[..n])?;
                        written += n as u64;
                        reporter.add_bytes(n as u64);
                    }

                    if written < *size {
                        tar.write_data(&vec![0u8; (*size - written) as usize])?;
                    }

                    tar.finish_entry(*size)?;
                    summary.files += 1;
                    summary.bytes += *size;
                }
            }

            reporter.finish_entry();
        }

        tar.finish()?.finish()?;
    }

    summary.archive_size = fs::metadata(archive)?.len();
    reporter.finish();
    Ok(summary)
}

fn refuse_options_a_tarball_cannot_keep(options: &CreateOptions) -> Result<()> {
    if options.password.is_some() {
        return Err(Error::Unsupported(Unsupported::Other(
            "encrypting a tarball; tar has no entry encryption, and writing it unencrypted would not be what was asked for",
        )));
    }
    if options.volume_size.is_some() {
        return Err(Error::Unsupported(Unsupported::Other("splitting a tarball across volumes")));
    }
    if options.method.is_some() {
        return Err(Error::Unsupported(Unsupported::Other("choosing a per-entry compression method for a tarball; the wrapper compresses the whole stream")));
    }
    if !options.comment.is_empty() {
        return Err(Error::Unsupported(Unsupported::Other("an archive comment on a tarball; tar has nowhere to store one")));
    }

    Ok(())
}

fn collect(base: &Path, path: &Path, out: &mut Vec<crate::pipeline::create::Source>, options: &CreateOptions, specials: &mut u64) -> Result<()> {
    crate::pipeline::create::collect(path, base, out, options, specials)
}

fn find_hard_links(sources: &[crate::pipeline::create::Source]) -> HashMap<usize, String> {
    let mut first: HashMap<(u64, u64), &str> = HashMap::new();
    let mut links = HashMap::new();

    for (index, source) in sources.iter().enumerate() {
        if source.meta.kind != EntryKind::File {
            continue;
        }
        let Some(identity) = sys::link_identity(&source.path) else { continue };

        match first.get(&identity) {
            Some(target) => {
                links.insert(index, (*target).to_owned());
            }
            None => {
                first.insert(identity, &source.name);
            }
        }
    }

    links
}

struct Body {
    index: usize,
    target: std::path::PathBuf,
}

fn write_bodies(source: &Source, found: &[Scanned], bodies: &[Body], options: &ExtractOptions, reporter: &Reporter) -> Result<u64> {
    if bodies.is_empty() {
        return Ok(0);
    }

    let Some(access) = source.random_access() else {
        return write_bodies_in_order(source, found, bodies, options, reporter);
    };

    let written = std::sync::atomic::AtomicU64::new(0);
    let threads = thread_count(options.threads, bodies.len());
    let open = || Ok((access.open()?, vec![0u8; COPY_BUF]));

    pool::for_each(bodies.len(), threads, open, |(input, buffer), slot| {
        let body = &bodies[slot];
        let entry = &found[body.index];

        reporter.start_entry(&entry.name);
        input.seek(std::io::SeekFrom::Start(entry.offset))?;

        let produced = if let Some(map) = &entry.sparse {
            let mut stored = vec![0u8; entry.stored as usize];
            input.read_exact(&mut stored)?;

            let whole = if map.in_data {
                let (map, used) = crate::tar::sparse::from_data(&stored, map.real_size)?;
                crate::tar::sparse::expand(&map, &stored[used..])?
            } else {
                crate::tar::sparse::expand(map, &stored)?
            };

            fs::write(&body.target, &whole)?;
            reporter.add_bytes(whole.len() as u64);
            whole.len() as u64
        } else {
            let mut out = BufWriter::with_capacity(COPY_BUF, File::create(&body.target)?);
            let mut left = entry.stored;
            while left > 0 {
                let want = left.min(buffer.len() as u64) as usize;
                input.read_exact(&mut buffer[..want])?;
                out.write_all(&buffer[..want])?;
                left -= want as u64;
                reporter.add_bytes(want as u64);
            }
            out.flush()?;
            entry.stored
        };

        if options.preserve_permissions {
            sys::apply_permissions(&body.target, &entry.meta)?;
            sys::apply_times(&body.target, &entry.meta)?;
        }

        written.fetch_add(produced, std::sync::atomic::Ordering::Relaxed);
        reporter.finish_entry();
        Ok(())
    })?;

    Ok(written.load(std::sync::atomic::Ordering::Relaxed))
}

fn write_bodies_in_order(source: &Source, found: &[Scanned], bodies: &[Body], options: &ExtractOptions, reporter: &Reporter) -> Result<u64> {
    let wanted: HashMap<usize, &Body> = bodies.iter().map(|body| (body.index, body)).collect();

    let mut reader = TarReader::new(source.reader()?);
    let mut buffer = vec![0u8; COPY_BUF];
    let mut written = 0u64;
    let mut index = 0usize;

    while let Some(entry) = reader.next_entry()? {
        let slot = index;
        index += 1;

        let Some(body) = wanted.get(&slot) else {
            reader.skip_data(&entry)?;
            continue;
        };

        reporter.start_entry(&entry.name);

        if entry.is_sparse() {
            let whole = reader.read_data(&entry)?;
            fs::write(&body.target, &whole)?;
            reporter.add_bytes(whole.len() as u64);
            written += whole.len() as u64;
        } else {
            let mut out = BufWriter::with_capacity(COPY_BUF, File::create(&body.target)?);
            written += reader.copy_data(&entry, &mut out, &mut buffer, |n| reporter.add_bytes(n))?;
            out.flush()?;
        }

        if options.preserve_permissions {
            sys::apply_permissions(&body.target, &found[slot].meta)?;
            sys::apply_times(&body.target, &found[slot].meta)?;
        }

        reporter.finish_entry();
    }

    Ok(written)
}

pub fn extract(archive: &Path, dest: &Path, wrapper: Wrapper, options: &ExtractOptions, reporter: &Reporter) -> Result<ExtractSummary> {
    layout::check_destination(dest)?;
    fs::create_dir_all(dest)?;
    let root = fs::canonicalize(dest)?;

    let source = Source::repeatable(archive, wrapper, &root)?;

    let found = scan(source.reader()?)?;
    let chosen: Vec<&Scanned> = found.iter().filter(|e| layout::selected(&e.name, &options.selection)).collect();
    let strip = layout::strip_depth(chosen.iter().map(|e| e.name.as_str()), options);

    let mut rejected = Rejected::default();
    let mut claims = Claims::new();
    let mut planned: Vec<(std::path::PathBuf, usize)> = Vec::new();

    for (index, entry) in found.iter().enumerate() {
        if is_archive_root(&entry.name) || !layout::selected(&entry.name, &options.selection) {
            continue;
        }
        let Some(relative) = layout::place(&entry.name, strip, options, &mut rejected)? else { continue };
        let kind = if entry.name.ends_with('/') { EntryKind::Directory } else { EntryKind::File };

        match claims.claim(&relative, planned.len(), kind, options.overwrite, &mut rejected)? {
            Claim::Fresh => planned.push((relative, index)),
            Claim::Replaces(slot) => planned[slot] = (relative, index),
            Claim::Drop => {}
        }
    }

    let planned_bytes: u64 = planned.iter().map(|(_, index)| found[*index].size).sum();
    reporter.set_totals(planned_bytes, planned.len() as u64);

    let mut summary = ExtractSummary { refused: rejected.refused, ..ExtractSummary::default() };
    let mut bodies: Vec<Body> = Vec::new();
    let mut pending_links: Vec<(std::path::PathBuf, std::path::PathBuf)> = Vec::new();
    let mut directories: Vec<(std::path::PathBuf, usize)> = Vec::new();

    for (relative, index) in &planned {
        let entry = &found[*index];
        let target = root.join(relative);

        if entry.kind == Kind::HardLink {
            reporter.finish_entry();
            let mut ignored = Rejected::default();
            match layout::place(&entry.linkname, strip, options, &mut ignored)? {
                Some(link_target) => pending_links.push((relative.clone(), link_target)),
                None => summary.refused += 1,
            }
            continue;
        }

        if matches!(entry.kind, Kind::CharDevice | Kind::BlockDevice | Kind::Fifo) {
            reporter.finish_entry();
            summary.specials += 1;
            continue;
        }

        match entry.meta.kind {
            EntryKind::Directory => {
                layout::create_directory(&root, relative)?;
                directories.push((relative.clone(), *index));
                summary.directories += 1;
                reporter.finish_entry();
            }

            EntryKind::Symlink => {
                reporter.finish_entry();
                if !options.restore_symlinks {
                    summary.skipped += 1;
                    continue;
                }
                if let Some(parent) = relative.parent().filter(|p| !p.as_os_str().is_empty()) {
                    layout::create_directory(&root, parent)?;
                }
                if crate::platform::policy::symlink_target_escapes(&root, &target, &entry.linkname) {
                    if options.unsafe_entries == crate::pipeline::UnsafeEntries::Skip {
                        summary.refused += 1;
                        continue;
                    }
                    return Err(Error::UnsafeEntryPath { name: entry.name.clone(), reason: crate::utils::error::PathRejection::SymlinkEscape });
                }
                if !layout::should_write(&target, options.overwrite)? {
                    summary.skipped += 1;
                    continue;
                }
                sys::create_symlink(&entry.linkname, &target)?;
                summary.symlinks += 1;
            }

            EntryKind::File => {
                if let Some(parent) = relative.parent().filter(|p| !p.as_os_str().is_empty()) {
                    layout::create_directory(&root, parent)?;
                }
                if !layout::should_write(&target, options.overwrite)? {
                    summary.skipped += 1;
                    reporter.finish_entry();
                    continue;
                }
                bodies.push(Body { index: *index, target });
            }
        }
    }

    summary.files += bodies.len() as u64;
    summary.bytes += write_bodies(&source, &found, &bodies, options, reporter)?;

    for (relative, link_target) in pending_links {
        let path = root.join(&relative);
        let source = root.join(&link_target);

        if let Some(parent) = relative.parent().filter(|p| !p.as_os_str().is_empty()) {
            layout::create_directory(&root, parent)?;
        }
        if !layout::should_write(&path, options.overwrite)? {
            summary.skipped += 1;
            continue;
        }

        if !source.exists() {
            summary.skipped += 1;
            continue;
        }

        sys::create_hard_link(&source, &path)?;
        summary.hardlinks += 1;
    }

    if options.preserve_permissions {
        directories.sort_by_key(|(relative, _)| std::cmp::Reverse(relative.components().count()));

        for (relative, index) in &directories {
            let target = root.join(relative);
            if target.exists() {
                sys::apply_permissions(&target, &found[*index].meta)?;
                sys::apply_times(&target, &found[*index].meta)?;
            }
        }
    }

    summary.skipped += rejected.skipped();
    reporter.finish();
    Ok(summary)
}

pub fn entries(archive: &Path, wrapper: Wrapper) -> Result<Vec<crate::pipeline::entry::Entry>> {
    let source = Source::once(archive, wrapper)?;

    let mut reader = TarReader::new(source.reader()?);
    let mut out = Vec::new();

    while let Some(entry) = reader.next_entry()? {
        out.push(crate::pipeline::entry::Entry {
            name: entry.name.clone(),
            size: entry.size,
            meta: entry.meta.clone(),
            detail: crate::pipeline::entry::EntryDetail::Tar(crate::pipeline::entry::TarDetail {
                typeflag: entry.kind.to_byte(),
                linkname: entry.linkname.clone(),
                uname: entry.uname.clone(),
                gname: entry.gname.clone(),
                devmajor: entry.devmajor,
                devminor: entry.devminor,
                sparse: entry.sparse.is_some(),
            }),
        });
        reader.skip_data(&entry)?;
    }

    Ok(out)
}

fn is_archive_root(name: &str) -> bool {
    matches!(name.trim_end_matches('/'), "" | ".")
}

struct Scanned {
    name: String,
    size: u64,
    stored: u64,
    offset: u64,
    kind: Kind,
    linkname: String,
    meta: crate::platform::EntryMeta,
    sparse: Option<crate::tar::sparse::Map>,
}

fn scan<R: Read>(plain: R) -> Result<Vec<Scanned>> {
    let mut reader = TarReader::new(plain);
    let mut found = Vec::new();

    while let Some(entry) = reader.next_entry()? {
        let mut name = entry.name.clone();
        if entry.entry_kind() == EntryKind::Directory && !name.ends_with('/') {
            name.push('/');
        }

        found.push(Scanned {
            name,
            size: if entry.kind.carries_data() { entry.size } else { 0 },
            stored: if entry.kind.carries_data() { entry.stored_size() } else { 0 },
            offset: entry.data_offset,
            kind: entry.kind,
            linkname: entry.linkname.clone(),
            meta: entry.meta.clone(),
            sparse: entry.sparse.clone(),
        });

        reader.skip_data(&entry)?;
    }

    Ok(found)
}

pub fn label(wrapper: Wrapper) -> &'static str {
    wrapper.name()
}
