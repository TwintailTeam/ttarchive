use crate::platform::{EntryKind, EntryMeta, dos, mode};
use crate::zip::parsers::extra::ExtraFields;
use crate::zip::spec::host;

pub fn decode(external: u32, version_made_by: u16, name_is_dir: bool, extra: &ExtraFields, dos_mtime: i64) -> EntryMeta {
    let (host_system, _) = crate::zip::spec::split_version_made_by(version_made_by);
    let dos_attrs = (external & 0xff) as u8;
    let unix_mode = if host::has_unix_mode(host_system) { external >> 16 } else { 0 };

    let kind = if unix_mode & mode::S_IFMT == mode::S_IFLNK {
        EntryKind::Symlink
    } else if name_is_dir || unix_mode & mode::S_IFMT == mode::S_IFDIR || dos_attrs & dos::DIRECTORY != 0 {
        EntryKind::Directory
    } else {
        EntryKind::File
    };

    let permissions = unix_mode & mode::PERM_MASK;

    EntryMeta {
        kind,
        unix_mode: if unix_mode != 0 { Some(permissions) } else { None },
        dos_attrs: if dos_attrs != 0 { Some(dos_attrs) } else { None },
        mtime: extra.mtime.or(Some(dos_mtime)),
        atime: extra.atime,
        ctime: extra.ctime,
        uid: extra.uid,
        gid: extra.gid,
    }
}

pub fn encode(meta: &EntryMeta, host_system: u8) -> u32 {
    let mut dos_attrs = meta.dos_attrs.unwrap_or(0);

    if meta.kind == EntryKind::Directory {
        dos_attrs |= dos::DIRECTORY;
    }
    if meta.is_readonly() {
        dos_attrs |= dos::READONLY;
    }

    let mut external = dos_attrs as u32;

    if host::has_unix_mode(host_system) {
        let type_bits = match meta.kind {
            EntryKind::File => mode::S_IFREG,
            EntryKind::Directory => mode::S_IFDIR,
            EntryKind::Symlink => mode::S_IFLNK,
        };
        external |= (type_bits | meta.effective_mode()) << 16;
    }

    external
}
