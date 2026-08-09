use std::fs::{self, File};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

use crate::pipeline::layout::{self, Claim, Claims, Rejected, create_directory, should_write};
use crate::pipeline::{ExtractOptions, ExtractSummary, UnsafeEntries, pool, thread_count};
use crate::platform::{EntryKind, EntryMeta, policy, sys};
use crate::utils::error::{Error, PathRejection, Result};
use crate::utils::io::COPY_BUF;
use crate::utils::progress::Reporter;
use crate::zip::ZipReader;
use crate::zip::reader::EntryRange;
use crate::zip::volumes::VolumeSet;

struct Planned {
    path: PathBuf,
    name: String,
    kind: EntryKind,
    meta: EntryMeta,
    range: EntryRange,
}

pub fn extract(archive: &Path, dest: &Path, options: &ExtractOptions, reporter: &Reporter) -> Result<ExtractSummary> {
    let volumes = VolumeSet::discover(archive)?;
    let mut reader = ZipReader::with_layout(volumes.open()?, volumes.layout().clone())?;

    let (plan, rejected) = build_plan(&mut reader, dest, options)?;
    drop(reader);

    reporter.set_totals(plan.iter().map(|p| p.range.uncompressed_size).sum(), plan.len() as u64);

    fs::create_dir_all(dest)?;
    let root = fs::canonicalize(dest)?;

    let mut summary = ExtractSummary { refused: rejected.refused, ..ExtractSummary::default() };

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

    let files: Vec<&Planned> = plan.iter().filter(|p| p.kind == EntryKind::File).collect();
    let threads = thread_count(options.threads, files.len());

    let written = std::sync::atomic::AtomicU64::new(0);
    let skipped = std::sync::atomic::AtomicU64::new(0);

    let open = || Ok((volumes.open()?, vec![0u8; COPY_BUF]));

    pool::for_each(files.len(), threads, open, |(source, buffer), index| {
        let item = files[index];
        let target = root.join(&item.path);

        if !should_write(&target, options.overwrite)? {
            skipped.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            reporter.finish_entry();
            return Ok(());
        }

        reporter.start_entry(&item.name);

        let out = File::create(&target)?;
        let mut out = BufWriter::with_capacity(COPY_BUF, out);
        crate::zip::reader::read_range(&mut *source, &item.range, &item.name, &mut out, reporter, buffer, options.password.as_ref())?;
        out.flush()?;
        let out = out.into_inner().map_err(|e| Error::Io(e.into_error()))?;
        drop(out);

        if options.preserve_permissions {
            sys::apply_permissions(&target, &item.meta)?;
            sys::apply_times(&target, &item.meta)?;
        }

        written.fetch_add(item.range.uncompressed_size, std::sync::atomic::Ordering::Relaxed);
        reporter.finish_entry();
        Ok(())
    })?;

    summary.files = files.len() as u64 - skipped.load(std::sync::atomic::Ordering::Relaxed);
    summary.skipped = skipped.load(std::sync::atomic::Ordering::Relaxed) + rejected.skipped();
    summary.bytes = written.load(std::sync::atomic::Ordering::Relaxed);

    if options.restore_symlinks {
        let mut source = volumes.open()?;
        let mut buffer = vec![0u8; COPY_BUF];

        for item in plan.iter().filter(|p| p.kind == EntryKind::Symlink) {
            let target_path = root.join(&item.path);

            let mut link_bytes = Vec::new();
            crate::zip::reader::read_range(&mut source, &item.range, &item.name, &mut link_bytes, reporter, &mut buffer, options.password.as_ref())?;

            let link_target = String::from_utf8_lossy(&link_bytes).into_owned();

            reporter.finish_entry();

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
            summary.symlinks += 1;
        }
    }

    if options.preserve_permissions {
        let mut dirs: Vec<&Planned> = plan.iter().filter(|p| p.kind == EntryKind::Directory).collect();
        dirs.sort_by_key(|d| std::cmp::Reverse(d.path.components().count()));

        for item in dirs {
            let target = root.join(&item.path);
            if target.exists() {
                sys::apply_permissions(&target, &item.meta)?;
                sys::apply_times(&target, &item.meta)?;
            }
        }
    }

    reporter.finish();
    Ok(summary)
}

fn build_plan<R: std::io::Read + std::io::Seek>(reader: &mut ZipReader<R>, dest: &Path, options: &ExtractOptions) -> Result<(Vec<Planned>, Rejected)> {
    let count = reader.len();
    let mut plan = Vec::with_capacity(count);
    let mut rejected = Rejected::default();
    let mut claims = Claims::new();

    let strip = layout::strip_depth(reader.entries().iter().map(|e| e.name.as_str()).filter(|n| layout::selected(n, &options.selection)), options);

    for index in 0..count {
        let entry = reader.entries()[index].clone();

        if !layout::selected(&entry.name, &options.selection) {
            continue;
        }
        ZipReader::<R>::check_readable(&entry, options.password.as_ref())?;

        let Some(relative) = layout::place(&entry.name, strip, options, &mut rejected)? else { continue };

        let range = reader.entry_range(index)?;
        let planned = Planned { path: relative, name: entry.name.clone(), kind: entry.meta.kind, meta: entry.meta.clone(), range };

        match claims.claim(&planned.path, plan.len(), planned.kind, options.overwrite, &mut rejected)? {
            Claim::Fresh => plan.push(planned),
            Claim::Replaces(slot) => plan[slot] = planned,
            Claim::Drop => {}
        }
    }

    layout::check_destination(dest)?;
    Ok((plan, rejected))
}
