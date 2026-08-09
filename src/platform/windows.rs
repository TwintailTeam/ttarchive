use std::fs;
use std::io;
use std::os::windows::fs::{MetadataExt, symlink_dir, symlink_file};
use std::path::Path;

use crate::platform::{EntryKind, EntryMeta, dos, mode};
use crate::utils::error::Result;

pub const HOST_SYSTEM: u8 = 10;

const FILE_ATTRIBUTE_READONLY: u32 = 0x0000_0001;
const FILE_ATTRIBUTE_HIDDEN: u32 = 0x0000_0002;
const FILE_ATTRIBUTE_SYSTEM: u32 = 0x0000_0004;
const FILE_ATTRIBUTE_DIRECTORY: u32 = 0x0000_0010;
const FILE_ATTRIBUTE_ARCHIVE: u32 = 0x0000_0020;
const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0000_0400;

fn unix_time(ticks: u64) -> Option<i64> {
    if ticks == 0 {
        return None;
    }
    Some(crate::utils::datetime::unix_from_filetime(ticks))
}

pub fn read_meta(path: &Path) -> Result<EntryMeta> {
    let md = fs::symlink_metadata(path)?;
    let attrs = md.file_attributes();

    let kind = if attrs & FILE_ATTRIBUTE_REPARSE_POINT != 0 && md.is_symlink() {
        EntryKind::Symlink
    } else if attrs & FILE_ATTRIBUTE_DIRECTORY != 0 {
        EntryKind::Directory
    } else {
        EntryKind::File
    };

    let mut dos_attrs = 0u8;
    if attrs & FILE_ATTRIBUTE_READONLY != 0 {
        dos_attrs |= dos::READONLY;
    }
    if attrs & FILE_ATTRIBUTE_HIDDEN != 0 {
        dos_attrs |= dos::HIDDEN;
    }
    if attrs & FILE_ATTRIBUTE_SYSTEM != 0 {
        dos_attrs |= dos::SYSTEM;
    }
    if attrs & FILE_ATTRIBUTE_ARCHIVE != 0 {
        dos_attrs |= dos::ARCHIVE;
    }
    if kind == EntryKind::Directory {
        dos_attrs |= dos::DIRECTORY;
    }

    let readonly = attrs & FILE_ATTRIBUTE_READONLY != 0;
    let unix_mode = match kind {
        EntryKind::Directory => mode::S_IFDIR | mode::DEFAULT_DIR,
        EntryKind::Symlink => mode::S_IFLNK | 0o777,
        EntryKind::File => mode::S_IFREG | if readonly { 0o444 } else { mode::DEFAULT_FILE },
    };

    Ok(EntryMeta {
        kind,
        unix_mode: Some(unix_mode),
        dos_attrs: Some(dos_attrs),
        mtime: unix_time(md.last_write_time()),
        atime: unix_time(md.last_access_time()),
        ctime: unix_time(md.creation_time()),
        uid: None,
        gid: None,
    })
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

pub fn read_symlink_target(path: &Path) -> Result<Vec<u8>> {
    let target = fs::read_link(path)?;
    let s = target.to_str().ok_or_else(|| crate::error::Error::malformed(format!("symlink target of {} is not valid UTF-8", path.display())))?;
    Ok(s.replace('\\', "/").into_bytes())
}

pub fn create_symlink(target: &str, path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.into()),
    }

    let native = target.replace('/', "\\");

    let resolved = path.parent().map(|p| p.join(&native));
    let target_is_dir = resolved.as_deref().is_some_and(|p| p.is_dir());

    let result = if target_is_dir { symlink_dir(&native, path) } else { symlink_file(&native, path) };

    result.map_err(|e| {
        if e.kind() == io::ErrorKind::PermissionDenied {
            io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!(
                    "creating symlink {} requires Developer Mode or \
                     SeCreateSymbolicLinkPrivilege",
                    path.display()
                ),
            )
            .into()
        } else {
            crate::error::Error::Io(e)
        }
    })
}

pub fn apply_permissions(path: &Path, meta: &EntryMeta) -> Result<()> {
    if meta.kind == EntryKind::Symlink {
        return Ok(());
    }
    let mut perms = fs::metadata(path)?.permissions();
    perms.set_readonly(meta.is_readonly());
    fs::set_permissions(path, perms)?;
    Ok(())
}

pub fn apply_times(_path: &Path, _meta: &EntryMeta) -> Result<()> {
    Ok(())
}

pub fn scratch_dir_mode() -> u32 {
    0o755
}
