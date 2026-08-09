use std::collections::HashMap;
use std::fs;
use std::path::{Component, Path, PathBuf};

use crate::pipeline::{ExtractOptions, Overwrite, UnsafeEntries};
use crate::platform::{EntryKind, policy};
use crate::utils::error::{Error, PathRejection, Result};

/// Where an archive entry will land, once its name has been validated and any
/// leading components stripped.
#[derive(Debug, Clone)]
pub struct Placement {
    pub path: PathBuf,
    pub index: usize,
}

/// Counters for entries that never reach the filesystem.
#[derive(Debug, Clone, Copy, Default)]
pub struct Rejected {
    pub refused: u64,
    pub stripped: u64,
    pub collided: u64,
}

impl Rejected {
    pub fn skipped(&self) -> u64 {
        self.stripped + self.collided
    }
}

/// Whether an entry is one the caller asked for.
///
/// An empty selection means everything. A name selects that entry, and a name
/// that is a directory selects everything beneath it.
pub fn selected(name: &str, selection: &[String]) -> bool {
    if selection.is_empty() {
        return true;
    }
    let name = name.trim_end_matches('/');
    selection.iter().any(|want| {
        let want = want.trim_end_matches('/');
        name == want || (name.len() > want.len() && name.starts_with(want) && name.as_bytes()[want.len()] == b'/')
    })
}

/// Decide how many leading components to drop for this archive.
pub fn strip_depth<'a>(names: impl Iterator<Item = &'a str>, options: &ExtractOptions) -> usize {
    options.strip_components + usize::from(options.strip_root && has_common_root(names))
}

pub fn has_common_root<'a>(names: impl Iterator<Item = &'a str>) -> bool {
    let mut root: Option<&str> = None;
    let mut nested = false;

    for name in names {
        let mut parts = policy::components(name);
        let Some(first) = parts.next() else { return false };
        if parts.next().is_some() {
            nested = true;
        }

        match root {
            None => root = Some(first),
            Some(seen) if seen == first => {}
            Some(_) => return false,
        }
    }

    nested
}

pub fn strip_leading(path: &Path, count: usize) -> PathBuf {
    if count == 0 {
        return path.to_path_buf();
    }
    path.components().skip(count).collect()
}

/// Validate one entry name and place it, honouring the strip settings.
///
/// `Ok(None)` means the entry was rejected or emptied and the counters say why.
pub fn place(name: &str, strip: usize, options: &ExtractOptions, rejected: &mut Rejected) -> Result<Option<PathBuf>> {
    let relative = match policy::to_relative_path(name, options.name_policy) {
        Ok(path) => path,
        Err(reason) => {
            if options.unsafe_entries == UnsafeEntries::Skip {
                rejected.refused += 1;
                return Ok(None);
            }
            return Err(Error::UnsafeEntryPath { name: name.to_owned(), reason });
        }
    };

    let relative = strip_leading(&relative, strip);
    if relative.as_os_str().is_empty() {
        rejected.stripped += 1;
        return Ok(None);
    }

    Ok(Some(relative))
}

/// Resolve two entries that strip down to the same destination path.
///
/// Extraction runs in parallel, so a collision left to the filesystem would race:
/// both workers would see the path missing and both would write. Collisions are
/// settled here, before any worker starts.
pub struct Claims {
    seen: HashMap<PathBuf, usize>,
}

pub enum Claim {
    Fresh,
    Replaces(usize),
    Drop,
}

impl Claims {
    pub fn new() -> Self {
        Claims { seen: HashMap::new() }
    }

    pub fn claim(&mut self, path: &Path, slot: usize, kind: EntryKind, overwrite: Overwrite, rejected: &mut Rejected) -> Result<Claim> {
        if kind == EntryKind::Directory {
            return Ok(Claim::Fresh);
        }

        if let Some(&earlier) = self.seen.get(path) {
            rejected.collided += 1;
            return match overwrite {
                Overwrite::Always => Ok(Claim::Replaces(earlier)),
                Overwrite::Never => Ok(Claim::Drop),
                Overwrite::Error => {
                    Err(Error::Io(std::io::Error::new(std::io::ErrorKind::AlreadyExists, format!("{} appears twice after stripping", path.display()))))
                }
            };
        }

        self.seen.insert(path.to_path_buf(), slot);
        Ok(Claim::Fresh)
    }
}

impl Default for Claims {
    fn default() -> Self {
        Self::new()
    }
}

/// Refuse to extract into a symlinked destination.
pub fn check_destination(dest: &Path) -> Result<()> {
    if let Ok(md) = fs::symlink_metadata(dest)
        && md.is_symlink()
    {
        return Err(Error::UnsafeEntryPath { name: dest.display().to_string(), reason: PathRejection::SymlinkEscape });
    }
    Ok(())
}

/// Create `relative` under `root`, refusing to walk through a symlink.
pub fn create_directory(root: &Path, relative: &Path) -> Result<()> {
    let mut current = root.to_path_buf();

    for component in relative.components() {
        let Component::Normal(name) = component else {
            return Err(Error::UnsafeEntryPath { name: relative.display().to_string(), reason: PathRejection::ParentTraversal });
        };
        current.push(name);

        match fs::symlink_metadata(&current) {
            Ok(md) if md.is_dir() => {}
            Ok(md) if md.is_symlink() => {
                return Err(Error::UnsafeEntryPath { name: current.display().to_string(), reason: PathRejection::SymlinkEscape });
            }
            Ok(_) => {
                fs::remove_file(&current)?;
                fs::create_dir(&current)?;
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                fs::create_dir(&current)?;
            }
            Err(e) => return Err(Error::from(e)),
        }
    }

    Ok(())
}

pub fn should_write(path: &Path, overwrite: Overwrite) -> Result<bool> {
    match fs::symlink_metadata(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(true),
        Err(e) => Err(Error::from(e)),
        Ok(_) => match overwrite {
            Overwrite::Always => {
                let _ = fs::remove_file(path);
                Ok(true)
            }
            Overwrite::Never => Ok(false),
            Overwrite::Error => Err(Error::Io(std::io::Error::new(std::io::ErrorKind::AlreadyExists, format!("{} already exists", path.display())))),
        },
    }
}
