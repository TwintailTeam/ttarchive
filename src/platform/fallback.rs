use std::fs;
use std::path::Path;

use crate::platform::{EntryKind, EntryMeta};
use crate::utils::error::{Error, Result, Unsupported};

pub const HOST_SYSTEM: u8 = 0;

pub fn read_meta(path: &Path) -> Result<EntryMeta> {
    let md = fs::symlink_metadata(path)?;
    let kind = if md.is_dir() { EntryKind::Directory } else { EntryKind::File };
    Ok(EntryMeta { kind, ..EntryMeta::file() })
}

/// Windows exposes no inode through `std`, so hard links cannot be detected
/// when creating an archive; each name is stored as its own copy.
pub fn link_identity(_path: &Path) -> Option<(u64, u64)> {
    None
}

pub fn create_hard_link(target: &Path, path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.into()),
    }
    fs::hard_link(target, path)?;
    Ok(())
}

pub fn read_symlink_target(_path: &Path) -> Result<Vec<u8>> {
    Err(Error::Unsupported(Unsupported::Other("symlinks on this platform")))
}

pub fn create_symlink(_target: &str, _path: &Path) -> Result<()> {
    Err(Error::Unsupported(Unsupported::Other("symlinks on this platform")))
}

pub fn apply_permissions(_path: &Path, _meta: &EntryMeta) -> Result<()> {
    Ok(())
}

pub fn apply_times(_path: &Path, _meta: &EntryMeta) -> Result<()> {
    Ok(())
}

pub fn scratch_dir_mode() -> u32 {
    0o755
}
