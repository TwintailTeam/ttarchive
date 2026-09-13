use std::fs;
use std::io;
use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
use std::path::Path;

use crate::platform::{EntryKind, EntryMeta, mode};
use crate::utils::error::{Error, Result, Unsupported};

pub const HOST_SYSTEM: u8 = 3;

pub fn read_meta(path: &Path) -> Result<EntryMeta> {
    let md = fs::symlink_metadata(path)?;
    let raw = md.mode();

    let kind = match raw & mode::S_IFMT {
        mode::S_IFDIR => EntryKind::Directory,
        mode::S_IFLNK => EntryKind::Symlink,
        mode::S_IFREG => EntryKind::File,
        _ => {
            return Err(Error::Unsupported(Unsupported::Other("special file (fifo, device or socket)")));
        }
    };

    Ok(EntryMeta {
        kind,
        unix_mode: Some(raw),
        dos_attrs: None,
        mtime: Some(md.mtime()),
        atime: Some(md.atime()),
        ctime: Some(md.ctime()),
        uid: Some(md.uid()),
        gid: Some(md.gid()),
        user: None,
        group: None,
    })
}

pub fn link_identity(path: &Path) -> Option<(u64, u64)> {
    let md = fs::symlink_metadata(path).ok()?;
    if md.nlink() < 2 { None } else { Some((md.dev(), md.ino())) }
}

pub fn create_hard_link(target: &Path, path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.into()),
    }
    fs::hard_link(target, path)?;
    Ok(())
}

pub fn read_symlink_target(path: &Path) -> Result<Vec<u8>> {
    let target = fs::read_link(path)?;
    let s = target.to_str().ok_or_else(|| Error::malformed(format!("symlink target of {} is not valid UTF-8", path.display())))?;
    Ok(s.as_bytes().to_vec())
}

pub fn create_symlink(target: &str, path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.into()),
    }
    symlink(target, path)?;
    Ok(())
}

pub fn apply_permissions(path: &Path, meta: &EntryMeta) -> Result<()> {
    if meta.kind == EntryKind::Symlink {
        return Ok(());
    }
    let perms = fs::Permissions::from_mode(meta.effective_mode());
    fs::set_permissions(path, perms)?;
    Ok(())
}

pub fn apply_owner(path: &Path, meta: &EntryMeta, uid: Option<u32>, gid: Option<u32>) -> Result<bool> {
    if uid.is_none() && gid.is_none() {
        return Ok(true);
    }
    let changed = if meta.kind == EntryKind::Symlink { std::os::unix::fs::lchown(path, uid, gid) } else { std::os::unix::fs::chown(path, uid, gid) };
    match changed {
        Ok(()) => Ok(true),
        Err(e) if matches!(e.kind(), io::ErrorKind::PermissionDenied | io::ErrorKind::InvalidInput) => Ok(false),
        Err(e) => Err(e.into()),
    }
}

pub fn apply_times(path: &Path, meta: &EntryMeta) -> Result<()> {
    if meta.kind == EntryKind::Symlink {
        return Ok(());
    }
    let Some(times) = crate::platform::file_times(meta) else { return Ok(()) };

    match fs::File::open(path)?.set_times(times) {
        Err(e) if crate::platform::times_not_representable(&e) => Ok(()),
        other => Ok(other?),
    }
}

pub fn scratch_dir_mode() -> u32 {
    0o755
}
