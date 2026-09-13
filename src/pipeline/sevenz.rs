use std::fs::{self, File};
use std::io::{self, BufWriter, Read, Seek, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::codecs::Method;
use crate::crypto::{Encryption, Password};
use crate::pipeline::entry::{Entry, EntryDetail, SevenZDetail};
use crate::pipeline::layout::{self, Claim, Claims, Rejected, create_directory, should_write};
use crate::pipeline::{CreateOptions, CreateSummary, ExtractOptions, ExtractSummary, Overwrite, Restore, UnsafeEntries, create, pool, thread_count};
use crate::platform::{EntryKind, EntryMeta, mode, policy, sys};
use crate::sevenz::SevenZReader;
use crate::sevenz::header::FileEntry;
use crate::sevenz::reader::Location;
use crate::sevenz::spec::attribute;
use crate::sevenz::volumes::VolumeSink;
use crate::sevenz::writer::{DEFAULT_BLOCK_SIZE, Item, Options, Packing, SevenZWriter};
use crate::utils::crc32::Crc32;
use crate::utils::datetime::unix_from_filetime;
use crate::utils::error::{Error, PathRejection, Result, Unsupported};
use crate::utils::io::COPY_BUF;
use crate::utils::progress::Reporter;

pub fn entries(archive: &Path, password: Option<&Password>) -> Result<Vec<Entry>> {
    let reader = SevenZReader::open_with(archive, password.cloned())?;
    let header = reader.header();

    let mut sized = vec![(0u64, None); header.files.len()];
    for location in reader.locations()? {
        sized[location.file] = (location.size, location.crc);
    }

    Ok(header
        .files
        .iter()
        .zip(sized)
        .map(|(file, (size, crc))| Entry {
            name: file.name.clone(),
            size,
            meta: meta_of(file),
            detail: EntryDetail::SevenZ(SevenZDetail { crc, attributes: file.attributes, is_anti: file.is_anti }),
        })
        .collect())
}

fn meta_of(file: &FileEntry) -> EntryMeta {
    let attributes = file.attributes.unwrap_or(0);
    let unix_mode = (attributes & crate::sevenz::spec::attribute::UNIX_EXTENSION != 0).then(|| attributes >> 16);

    let kind = if file.is_directory() {
        EntryKind::Directory
    } else if unix_mode.is_some_and(|m| m & mode::S_IFMT == mode::S_IFLNK) {
        EntryKind::Symlink
    } else {
        EntryKind::File
    };

    EntryMeta {
        kind,
        unix_mode,
        dos_attrs: file.attributes.map(|a| a as u8),
        mtime: file.mtime.map(unix_from_filetime),
        atime: file.atime.map(unix_from_filetime),
        ctime: file.ctime.map(unix_from_filetime),
        uid: None,
        gid: None,
        user: None,
        group: None,
    }
}

struct Planned {
    path: PathBuf,
    name: String,
    kind: EntryKind,
    meta: EntryMeta,
    location: Option<Location>,
}

pub fn extract(archive: &Path, dest: &Path, options: &ExtractOptions, reporter: &Reporter) -> Result<ExtractSummary> {
    let restore = Restore::new(options);
    let reader = SevenZReader::open_with(archive, options.password.clone())?;

    let (plan, anti, rejected, specials) = build_plan(&reader, dest, options)?;
    reporter.set_totals(plan.iter().filter_map(|p| p.location.as_ref()).map(|l| l.size).sum(), plan.len() as u64);

    fs::create_dir_all(dest)?;
    let root = fs::canonicalize(dest)?;

    let mut summary = ExtractSummary { refused: rejected.refused, skipped: rejected.skipped(), specials, ..ExtractSummary::default() };

    for item in &plan {
        match item.kind {
            EntryKind::Directory => {
                create_directory(&root, &item.path)?;
                summary.directories += 1;
                reporter.finish_entry();
            }
            _ => {
                if let Some(parent) = item.path.parent().filter(|p| !p.as_os_str().is_empty()) {
                    create_directory(&root, parent)?;
                }
            }
        }
    }

    for item in plan.iter().filter(|p| p.kind == EntryKind::File && p.location.is_none()) {
        let target = root.join(&item.path);
        if should_write(&target, options.overwrite)? {
            File::create(&target)?;
            summary.files += 1;
            restore.apply(&target, &item.meta)?;
        } else {
            summary.skipped += 1;
        }
        reporter.finish_entry();
    }

    let groups = group_by_folder(&plan);
    let threads = thread_count(options.threads, groups.len());

    let written = AtomicU64::new(0);
    let skipped = AtomicU64::new(0);
    let links: Mutex<Vec<(usize, String)>> = Mutex::new(Vec::new());

    pool::for_each(
        groups.len(),
        threads,
        || Ok(vec![0u8; COPY_BUF]),
        |buffer, index| {
            let (folder, ref slots) = groups[index];
            let mut stream = reader.folder_reader(folder)?;
            let mut at = 0u64;

            for &slot in slots {
                let item = &plan[slot];
                let location = item.location.as_ref().expect("a grouped entry owns bytes");

                if location.offset > at {
                    discard(&mut stream, location.offset - at, buffer)?;
                    at = location.offset;
                }

                if item.kind == EntryKind::Symlink {
                    let mut target = Vec::with_capacity(location.size.min(1 << 16) as usize);
                    copy_entry(&mut stream, &mut target, location, &item.name, reporter, buffer)?;
                    links.lock().unwrap_or_else(|p| p.into_inner()).push((slot, String::from_utf8_lossy(&target).into_owned()));
                    at += location.size;
                    continue;
                }

                let target = root.join(&item.path);
                if !should_write(&target, options.overwrite)? {
                    discard(&mut stream, location.size, buffer)?;
                    skipped.fetch_add(1, Ordering::Relaxed);
                    reporter.finish_entry();
                    at += location.size;
                    continue;
                }

                reporter.start_entry(&item.name);
                let mut out = BufWriter::with_capacity(COPY_BUF, File::create(&target)?);
                copy_entry(&mut stream, &mut out, location, &item.name, reporter, buffer)?;
                out.flush()?;
                drop(out);

                restore.apply(&target, &item.meta)?;

                written.fetch_add(location.size, Ordering::Relaxed);
                reporter.finish_entry();
                at += location.size;
            }

            Ok(())
        },
    )?;

    let skipped_files = skipped.load(Ordering::Relaxed);
    summary.files += groups.iter().map(|(_, slots)| slots.iter().filter(|&&s| plan[s].kind == EntryKind::File).count() as u64).sum::<u64>() - skipped_files;
    summary.skipped += skipped_files;
    summary.bytes = written.load(Ordering::Relaxed);

    let mut links = links.into_inner().unwrap_or_else(|p| p.into_inner());
    links.sort_by_key(|(slot, _)| *slot);

    for (slot, link_target) in links {
        let item = &plan[slot];
        let target_path = root.join(&item.path);

        if !options.restore_symlinks {
            summary.skipped += 1;
            continue;
        }

        if policy::symlink_target_escapes(&root, &target_path, &link_target) {
            if options.unsafe_entries == UnsafeEntries::Skip {
                summary.refused += 1;
                continue;
            }
            return Err(Error::UnsafeEntryPath { name: item.name.clone(), reason: PathRejection::SymlinkEscape });
        }

        if !should_write(&target_path, options.overwrite)? {
            summary.skipped += 1;
            continue;
        }

        sys::create_symlink(&link_target, &target_path)?;
        restore.apply(&target_path, &item.meta)?;
        summary.symlinks += 1;
    }

    apply_deletions(&root, &plan, anti, options, &mut summary)?;

    if restore.anything() {
        let mut dirs: Vec<&Planned> = plan.iter().filter(|p| p.kind == EntryKind::Directory).collect();
        dirs.sort_by_key(|d| std::cmp::Reverse(d.path.components().count()));

        for item in dirs {
            let target = root.join(&item.path);
            if target.exists() {
                restore.apply(&target, &item.meta)?;
            }
        }
    }

    summary.owners_not_restored = restore.refused();
    reporter.finish();
    Ok(summary)
}

fn apply_deletions(root: &Path, plan: &[Planned], mut anti: Vec<Planned>, options: &ExtractOptions, summary: &mut ExtractSummary) -> Result<()> {
    anti.sort_by_key(|item| std::cmp::Reverse(item.path.components().count()));

    for item in anti {
        if plan.iter().any(|written| written.path == item.path) {
            continue;
        }

        let target = root.join(&item.path);
        let Some(existing) = symlink_metadata(&target)? else { continue };

        if !resolves_inside(root, &target)? {
            if options.unsafe_entries == UnsafeEntries::Skip {
                summary.refused += 1;
                continue;
            }
            return Err(Error::UnsafeEntryPath { name: item.name.clone(), reason: PathRejection::SymlinkEscape });
        }

        match options.overwrite {
            Overwrite::Never => {
                summary.skipped += 1;
                continue;
            }
            Overwrite::Error => {
                return Err(Error::Io(io::Error::new(io::ErrorKind::AlreadyExists, format!("{} already exists", target.display()))));
            }
            Overwrite::Always => {}
        }

        if existing.is_dir() {
            if fs::read_dir(&target)?.next().is_some() {
                summary.skipped += 1;
                continue;
            }
            fs::remove_dir(&target)?;
        } else {
            fs::remove_file(&target)?;
        }

        summary.deleted += 1;
    }

    Ok(())
}

fn symlink_metadata(path: &Path) -> Result<Option<fs::Metadata>> {
    match fs::symlink_metadata(path) {
        Ok(meta) => Ok(Some(meta)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(Error::from(e)),
    }
}

fn resolves_inside(root: &Path, target: &Path) -> Result<bool> {
    let Some(parent) = target.parent() else { return Ok(false) };
    match fs::canonicalize(parent) {
        Ok(real) => Ok(real == root || real.starts_with(root)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(Error::from(e)),
    }
}

fn group_by_folder(plan: &[Planned]) -> Vec<(usize, Vec<usize>)> {
    let mut groups: Vec<(usize, Vec<usize>)> = Vec::new();

    for (slot, item) in plan.iter().enumerate() {
        let Some(location) = &item.location else { continue };
        match groups.iter_mut().find(|(folder, _)| *folder == location.folder) {
            Some((_, slots)) => slots.push(slot),
            None => groups.push((location.folder, vec![slot])),
        }
    }

    for (_, slots) in &mut groups {
        slots.sort_by_key(|&slot| plan[slot].location.as_ref().expect("a grouped entry owns bytes").offset);
    }
    groups
}

fn discard<R: Read>(stream: &mut R, mut count: u64, buffer: &mut [u8]) -> Result<()> {
    while count > 0 {
        let want = count.min(buffer.len() as u64) as usize;
        match stream.read(&mut buffer[..want])? {
            0 => return Err(Error::malformed("7z folder ends before the entries it holds")),
            got => count -= got as u64,
        }
    }
    Ok(())
}

fn copy_entry<R: Read, W: Write>(stream: &mut R, out: &mut W, location: &Location, name: &str, reporter: &Reporter, buffer: &mut [u8]) -> Result<()> {
    let mut crc = Crc32::new();
    let mut left = location.size;

    while left > 0 {
        let want = left.min(buffer.len() as u64) as usize;
        let got = stream.read(&mut buffer[..want])?;
        if got == 0 {
            return Err(Error::malformed(format!("7z folder ends {left} bytes before {name} does")));
        }
        crc.update(&buffer[..got]);
        out.write_all(&buffer[..got])?;
        reporter.add_bytes(got as u64);
        left -= got as u64;
    }

    match location.crc {
        Some(expected) if crc.finish() != expected => Err(Error::ChecksumMismatch { entry: name.to_owned(), expected, found: crc.finish() }),
        _ => Ok(()),
    }
}

type Plan = (Vec<Planned>, Vec<Planned>, Rejected, u64);

fn build_plan<S: crate::sevenz::source::Source>(reader: &SevenZReader<S>, dest: &Path, options: &ExtractOptions) -> Result<Plan> {
    let files = &reader.header().files;
    let mut located = vec![None; files.len()];
    for location in reader.locations()? {
        located[location.file] = Some(location);
    }

    let mut plan = Vec::with_capacity(files.len());
    let mut anti = Vec::new();
    let mut rejected = Rejected::default();
    let mut claims = Claims::new();
    let mut specials = 0u64;

    let strip = layout::strip_depth(files.iter().map(|f| f.name.as_str()).filter(|n| layout::selected(n, &options.selection)), options);

    for (index, file) in files.iter().enumerate() {
        if !layout::selected(&file.name, &options.selection) {
            continue;
        }

        let Some(relative) = layout::place(&file.name, strip, options, &mut rejected)? else { continue };

        if is_opaque_reparse_point(file) {
            specials += 1;
            continue;
        }

        let meta = meta_of(file);
        let planned = Planned { path: relative, name: file.name.clone(), kind: meta.kind, meta, location: located[index].take() };

        if file.is_anti {
            anti.push(planned);
            continue;
        }

        match claims.claim(&planned.path, plan.len(), planned.kind, options.overwrite, &mut rejected)? {
            Claim::Fresh => plan.push(planned),
            Claim::Replaces(slot) => plan[slot] = planned,
            Claim::Drop => {}
        }
    }

    layout::check_destination(dest)?;
    Ok((plan, anti, rejected, specials))
}

fn is_opaque_reparse_point(file: &FileEntry) -> bool {
    let Some(attributes) = file.attributes else { return false };
    attributes & attribute::REPARSE_POINT != 0 && attributes & attribute::UNIX_EXTENSION == 0 && !file.is_directory() && !file.is_anti
}

pub fn create<I: IntoIterator<Item = P>, P: AsRef<Path>>(archive: &Path, inputs: I, options: &CreateOptions, reporter: &Reporter) -> Result<CreateSummary> {
    if options.sparse {
        return Err(Error::Unsupported(Unsupported::Other("storing holes in a 7z entry, which the format has no way to record")));
    }
    if options.password.is_some() && options.encryption != Encryption::Aes256 {
        return Err(Error::Unsupported(Unsupported::Other("encrypting a 7z archive with anything but AES-256")));
    }
    let packing = packing_for(options)?;

    let mut sources = Vec::new();
    let mut specials = 0u64;
    for input in inputs {
        create::collect(input.as_ref(), input.as_ref(), &mut sources, options, &mut specials)?;
    }
    sources.sort_by(|a, b| a.name.cmp(&b.name));

    let total_bytes: u64 = sources.iter().map(|s| s.size).sum();
    reporter.set_totals(total_bytes, sources.len() as u64);

    let mut summary = CreateSummary { specials, ..CreateSummary::default() };

    match options.volume_size {
        Some(size) => {
            let sink = BufWriter::with_capacity(COPY_BUF, VolumeSink::create(archive, size)?);
            let mut writer = SevenZWriter::new(sink, write_options(options, packing))?;
            fill(&mut writer, &sources, &mut summary, reporter)?;

            let paths = writer.finish()?.into_inner().map_err(|e| Error::Io(e.into_error()))?.finish()?;
            summary.volumes = paths.len() as u32;
            summary.archive_size = paths.iter().filter_map(|path| fs::metadata(path).ok()).map(|meta| meta.len()).sum();
        }

        None => {
            let out = BufWriter::with_capacity(COPY_BUF, File::create(archive)?);
            let mut writer = SevenZWriter::new(out, write_options(options, packing))?;
            fill(&mut writer, &sources, &mut summary, reporter)?;

            let finished = writer.finish()?.into_inner().map_err(|e| Error::Io(e.into_error()))?;
            summary.volumes = 1;
            summary.archive_size = finished.metadata()?.len();
        }
    }

    reporter.finish();
    Ok(summary)
}

fn fill<W: Write + Seek>(writer: &mut SevenZWriter<W>, sources: &[create::Source], summary: &mut CreateSummary, reporter: &Reporter) -> Result<()> {
    for source in sources {
        reporter.start_entry(&source.name);

        match source.meta.kind {
            EntryKind::Directory => {
                writer.add_directory(&item(source, attributes_of(&source.meta)));
                summary.directories += 1;
            }
            EntryKind::Symlink => {
                let target = sys::read_symlink_target(&source.path)?;
                writer.add_file(&item(source, attributes_of(&source.meta)), &target)?;
                summary.symlinks += 1;
            }
            EntryKind::File => {
                let mut file = File::open(&source.path)?;
                writer.add_stream(&item(source, attributes_of(&source.meta)), &mut file, source.size)?;
                reporter.add_bytes(source.size);
                summary.bytes += source.size;
                summary.files += 1;
            }
        }

        reporter.finish_entry();
    }

    Ok(())
}

fn packing_for(options: &CreateOptions) -> Result<Packing> {
    let Some(method) = options.method else { return Ok(Packing::Lzma2) };

    match method {
        Method::Store => Ok(Packing::Copy),
        Method::Deflate => Ok(Packing::Deflate),
        Method::Bzip2 => Ok(Packing::Bzip2),
        other => Err(Error::Unsupported(Unsupported::CompressionMethod(other.code()))),
    }
}

fn write_options(options: &CreateOptions, packing: Packing) -> Options {
    Options {
        level: options.level,
        packing,
        filter: None,
        block_size: Some(DEFAULT_BLOCK_SIZE),
        compress_header: true,
        encrypt_header: options.password.is_some(),
        password: options.password.clone(),
    }
}

fn item(source: &create::Source, attributes: u32) -> Item {
    Item { name: source.name.trim_end_matches('/').to_string(), mtime: source.meta.mtime, attributes: Some(attributes) }
}

fn attributes_of(meta: &EntryMeta) -> u32 {
    let kind = match meta.kind {
        EntryKind::Directory => mode::S_IFDIR,
        EntryKind::Symlink => mode::S_IFLNK,
        EntryKind::File => mode::S_IFREG,
    };

    let unix = kind | meta.effective_mode();
    let mut attributes = attribute::UNIX_EXTENSION | (unix << 16);

    if meta.kind == EntryKind::Directory {
        attributes |= attribute::DIRECTORY;
    }
    if meta.kind == EntryKind::Symlink {
        attributes |= attribute::REPARSE_POINT;
    }
    if meta.effective_mode() & 0o200 == 0 {
        attributes |= attribute::READONLY;
    }
    attributes
}
