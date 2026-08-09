use std::fs::{self, File};
use std::io::{BufWriter, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use crate::codecs::{Level, Method};
use crate::pipeline::{CreateOptions, CreateSummary, MEMORY_BUDGET, pool, thread_count};
use crate::platform::{EntryKind, EntryMeta, policy, sys};
use crate::utils::crc32::Crc32;
use crate::utils::error::{Error, Result, Unsupported};
use crate::utils::io::COPY_BUF;
use crate::utils::progress::Reporter;
use crate::zip::ZipWriter;
use crate::zip::volumes::{SingleSink, Sink, VolumeSink};
use crate::zip::writer::PreparedEntry;

const STREAM_THRESHOLD: u64 = 8 * 1024 * 1024;

const PARALLEL_CHUNK: u64 = 2 * 1024 * 1024;

pub struct Source {
    pub path: PathBuf,
    pub name: String,
    pub meta: EntryMeta,
    pub size: u64,
}

pub fn create<I: IntoIterator<Item = P>, P: AsRef<Path>>(archive: &Path, inputs: I, options: &CreateOptions, reporter: &Reporter) -> Result<CreateSummary> {
    let mut sources = Vec::new();
    let mut specials = 0u64;
    for input in inputs {
        collect(input.as_ref(), input.as_ref(), &mut sources, options, &mut specials)?;
    }

    sources.sort_by(|a, b| a.name.cmp(&b.name));

    let total_bytes: u64 = sources.iter().map(|s| s.size).sum();
    reporter.set_totals(total_bytes, sources.len() as u64);

    let file_indices: Vec<usize> =
        sources.iter().enumerate().filter(|(_, s)| s.meta.kind == EntryKind::File && s.size <= STREAM_THRESHOLD).map(|(i, _)| i).collect();

    if options.sparse {
        return Err(Error::Unsupported(Unsupported::Other("storing holes in a ZIP entry, which the format has no way to record")));
    }

    let method = chosen_method(options);
    if !method.can_encode() {
        return Err(Error::Unsupported(Unsupported::CompressionMethod(method.code())));
    }

    let batch_threads = thread_count(options.threads, file_indices.len());
    let mut compressed = Batch::new(&sources, &file_indices, method, options.level, batch_threads, reporter);

    let large = Streaming { method, level: options.level, threads: thread_count(options.threads, usize::MAX) };

    let mut summary = CreateSummary { specials, ..CreateSummary::default() };

    match options.volume_size {
        Some(size) => {
            let sink = VolumeSink::create(archive, size)?;
            let mut writer = ZipWriter::new(sink)?;
            configure(&mut writer, options);
            write_entries(&mut writer, &sources, &mut compressed, &mut summary, reporter, &large)?;

            let paths = writer.finish()?.finish()?;
            summary.volumes = paths.len() as u32;
            summary.archive_size = paths.iter().filter_map(|p| fs::metadata(p).ok()).map(|m| m.len()).sum();
        }

        None => {
            let out = File::create(archive)?;
            let sink = SingleSink(BufWriter::with_capacity(COPY_BUF, out));
            let mut writer = ZipWriter::new(sink)?;
            configure(&mut writer, options);
            write_entries(&mut writer, &sources, &mut compressed, &mut summary, reporter, &large)?;

            let file = writer.finish()?.0.into_inner().map_err(|e| Error::Io(e.into_error()))?;
            summary.volumes = 1;
            summary.archive_size = file.metadata()?.len();
        }
    }

    reporter.finish();
    Ok(summary)
}

type Slot = std::sync::Mutex<Option<(Vec<u8>, u32, u64)>>;

fn chosen_method(options: &CreateOptions) -> Method {
    match options.method {
        _ if options.level == Level::None => Method::Store,
        Some(method) => method,
        None => Method::Deflate,
    }
}

fn configure<S: Sink>(writer: &mut ZipWriter<S>, options: &CreateOptions) {
    writer.set_level(options.level);
    writer.set_method(options.method);
    if !options.comment.is_empty() {
        writer.set_comment(options.comment.clone());
    }
    if let Some(password) = &options.password {
        writer.set_encryption(password.clone(), options.encryption);
    }
}

struct Streaming {
    method: Method,
    level: Level,
    threads: usize,
}

fn write_entries<S: Sink>(
    writer: &mut ZipWriter<S>,
    sources: &[Source],
    compressed: &mut Batch<'_>,
    summary: &mut CreateSummary,
    reporter: &Reporter,
    large: &Streaming,
) -> Result<()> {
    let method = large.method;
    for source in sources {
        reporter.start_entry(&source.name);

        match source.meta.kind {
            EntryKind::Directory => {
                writer.add_directory(&source.name, &source.meta)?;
                summary.directories += 1;
            }
            EntryKind::Symlink => {
                let target = sys::read_symlink_target(&source.path)?;
                writer.add_symlink(&source.name, &target, &source.meta)?;
                summary.symlinks += 1;
            }
            EntryKind::File => {
                if source.size <= STREAM_THRESHOLD {
                    let prepared = compressed.take()?;
                    summary.bytes += prepared.uncompressed_size;
                    writer.add_prepared(prepared)?;
                } else if matches!(method, Method::Store | Method::Deflate) {
                    write_large_entry(writer, source, large, reporter)?;
                    summary.bytes += source.size;
                } else {
                    let mut file = File::open(&source.path)?;
                    writer.add_file(&source.name, &source.meta, &mut file, source.size)?;
                    reporter.add_bytes(source.size);
                    summary.bytes += source.size;
                }

                summary.files += 1;
            }
        }

        reporter.finish_entry();
    }

    Ok(())
}

fn write_large_entry<S: Sink>(writer: &mut ZipWriter<S>, source: &Source, large: &Streaming, reporter: &Reporter) -> Result<()> {
    let Streaming { method, level, threads } = *large;
    let path = source.path.clone();
    let total = source.size;
    let chunk_count = total.div_ceil(PARALLEL_CHUNK).max(1) as usize;
    let workers = threads.min(chunk_count).max(1);

    writer.add_produced(&source.name, &source.meta, method, total, |sink| {
        let mut crc_total: u32 = 0;
        let mut bytes_total: u64 = 0;
        let mut next_chunk = 0usize;

        while next_chunk < chunk_count {
            let wave = (chunk_count - next_chunk).min(workers);
            let slots: Vec<Slot> = (0..wave).map(|_| std::sync::Mutex::new(None)).collect();

            let open = || Ok((File::open(&path)?, vec![0u8; PARALLEL_CHUNK as usize]));

            pool::for_each(wave, workers, open, |(file, buffer), slot| {
                let index = next_chunk + slot;
                let offset = index as u64 * PARALLEL_CHUNK;
                let len = PARALLEL_CHUNK.min(total - offset) as usize;

                file.seek(SeekFrom::Start(offset))?;
                file.read_exact(&mut buffer[..len])?;
                let data = &buffer[..len];

                let crc = crate::utils::crc32::checksum(data);
                let is_final = index + 1 == chunk_count;
                let packed = match method {
                    Method::Store => data.to_vec(),
                    _ => crate::codecs::deflate::compress_chunk(data, level, is_final),
                };

                reporter.add_bytes(len as u64);
                *slots[slot].lock().unwrap_or_else(|p| p.into_inner()) = Some((packed, crc, len as u64));
                Ok(())
            })?;

            for slot in slots {
                let (packed, crc, len) =
                    slot.into_inner().unwrap_or_else(|p| p.into_inner()).ok_or_else(|| Error::malformed("internal: chunk was not compressed"))?;

                sink.write_all(&packed)?;
                crc_total = if bytes_total == 0 { crc } else { crate::utils::crc32::combine(crc_total, crc, len) };
                bytes_total += len;
            }

            next_chunk += wave;
        }

        Ok((crc_total, bytes_total))
    })
}

struct Batch<'a> {
    sources: &'a [Source],
    indices: &'a [usize],
    method: Method,
    level: Level,
    threads: usize,
    reporter: &'a Reporter,
    next: usize,
    ready: std::collections::VecDeque<PreparedEntry>,
}

impl<'a> Batch<'a> {
    fn new(sources: &'a [Source], indices: &'a [usize], method: Method, level: Level, threads: usize, reporter: &'a Reporter) -> Self {
        Batch { sources, indices, method, level, threads, reporter, next: 0, ready: std::collections::VecDeque::new() }
    }

    fn take(&mut self) -> Result<PreparedEntry> {
        if self.ready.is_empty() {
            self.fill()?;
        }
        self.ready.pop_front().ok_or_else(|| Error::malformed("internal: missing compressed entry"))
    }

    fn fill(&mut self) -> Result<()> {
        let mut wave = 0usize;
        let mut bytes = 0u64;
        while self.next + wave < self.indices.len() && wave < self.threads {
            let size = self.sources[self.indices[self.next + wave]].size;
            if wave > 0 && bytes + size > MEMORY_BUDGET {
                break;
            }
            bytes += size;
            wave += 1;
        }
        if wave == 0 {
            return Ok(());
        }

        let (sources, indices, method, level, reporter) = (self.sources, self.indices, self.method, self.level, self.reporter);
        let start = self.next;

        let slots: Vec<std::sync::Mutex<Option<PreparedEntry>>> = (0..wave).map(|_| std::sync::Mutex::new(None)).collect();
        let buffer = || Ok(Vec::<u8>::with_capacity(COPY_BUF));

        pool::for_each(wave, self.threads.min(wave), buffer, |scratch, slot| {
            let source = &sources[indices[start + slot]];

            scratch.clear();
            scratch.reserve(source.size as usize);
            let mut input = File::open(&source.path)?;
            let mut chunk = [0u8; COPY_BUF];
            loop {
                let n = input.read(&mut chunk)?;
                if n == 0 {
                    break;
                }
                scratch.extend_from_slice(&chunk[..n]);
                reporter.add_bytes(n as u64);
            }

            let crc = {
                let mut c = Crc32::new();
                c.update(scratch);
                c.finish()
            };

            let (method, data) = compress_best(scratch, method, level)?;

            *slots[slot].lock().unwrap_or_else(|p| p.into_inner()) =
                Some(PreparedEntry { name: source.name.clone(), meta: source.meta.clone(), method, data, crc32: crc, uncompressed_size: scratch.len() as u64 });
            Ok(())
        })?;

        for slot in slots {
            let entry = slot.into_inner().unwrap_or_else(|p| p.into_inner()).ok_or_else(|| Error::malformed("internal: entry was not compressed"))?;
            self.ready.push_back(entry);
        }

        self.next += wave;
        Ok(())
    }
}

fn compress_best(data: &[u8], method: Method, level: Level) -> Result<(Method, Vec<u8>)> {
    if method == Method::Store || data.is_empty() {
        return Ok((Method::Store, data.to_vec()));
    }

    let packed = match method {
        Method::Bzip2 => crate::codecs::bzip2::compress(data, level.bzip2_block_size())?,
        _ => crate::codecs::deflate::compress(data, level),
    };

    if packed.len() < data.len() { Ok((method, packed)) } else { Ok((Method::Store, data.to_vec())) }
}

pub fn collect(path: &Path, base: &Path, out: &mut Vec<Source>, options: &CreateOptions, specials: &mut u64) -> Result<()> {
    let meta = match sys::read_meta(path) {
        Ok(m) => m,
        Err(e) if e.is_unsupported() => {
            *specials += 1;
            return Ok(());
        }
        Err(e) => return Err(e),
    };

    let relative = match base.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => path.strip_prefix(parent).unwrap_or(path),
        _ => path,
    };

    let is_dir = meta.kind == EntryKind::Directory;
    let Some(name) = policy::to_entry_name(relative, is_dir) else {
        return Err(Error::malformed(format!("cannot store {} under a portable entry name", path.display())));
    };

    match meta.kind {
        EntryKind::Symlink if !options.store_symlinks => {
            let resolved = fs::metadata(path)?;
            if resolved.is_file() {
                out.push(Source { path: path.to_path_buf(), name, meta: EntryMeta::file(), size: resolved.len() });
            }
            return Ok(());
        }
        EntryKind::Symlink => {
            out.push(Source { path: path.to_path_buf(), name, meta, size: 0 });
            return Ok(());
        }
        EntryKind::Directory => {
            out.push(Source { path: path.to_path_buf(), name, meta, size: 0 });

            if options.recursive {
                let mut children: Vec<PathBuf> = fs::read_dir(path)?.filter_map(|e| e.ok()).map(|e| e.path()).collect();
                children.sort();
                for child in children {
                    collect(&child, base, out, options, specials)?;
                }
            }
        }
        EntryKind::File => {
            let size = fs::symlink_metadata(path)?.len();
            out.push(Source { path: path.to_path_buf(), name, meta, size });
        }
    }

    Ok(())
}
