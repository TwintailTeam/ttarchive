pub mod policy;

#[cfg(unix)]
#[path = "unix.rs"]
pub mod imp;

#[cfg(windows)]
#[path = "windows.rs"]
pub mod imp;

#[cfg(not(any(unix, windows)))]
#[path = "fallback.rs"]
pub mod imp;

pub use imp as sys;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    File,
    Directory,
    Symlink,
}

pub mod mode {
    pub const S_IFMT: u32 = 0o170_000;
    pub const S_IFREG: u32 = 0o100_000;
    pub const S_IFDIR: u32 = 0o040_000;
    pub const S_IFLNK: u32 = 0o120_000;
    pub const S_IFIFO: u32 = 0o010_000;
    pub const S_IFCHR: u32 = 0o020_000;
    pub const S_IFBLK: u32 = 0o060_000;
    pub const S_IFSOCK: u32 = 0o140_000;
    pub const PERM_MASK: u32 = 0o7777;

    pub const DEFAULT_FILE: u32 = 0o644;
    pub const DEFAULT_DIR: u32 = 0o755;
}

pub mod dos {
    pub const READONLY: u8 = 0x01;
    pub const HIDDEN: u8 = 0x02;
    pub const SYSTEM: u8 = 0x04;
    pub const VOLUME: u8 = 0x08;
    pub const DIRECTORY: u8 = 0x10;
    pub const ARCHIVE: u8 = 0x20;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntryMeta {
    pub kind: EntryKind,
    pub unix_mode: Option<u32>,
    pub dos_attrs: Option<u8>,
    pub mtime: Option<i64>,
    pub atime: Option<i64>,
    pub ctime: Option<i64>,
    pub uid: Option<u32>,
    pub gid: Option<u32>,
}

impl EntryMeta {
    pub fn file() -> Self {
        EntryMeta { kind: EntryKind::File, unix_mode: None, dos_attrs: None, mtime: None, atime: None, ctime: None, uid: None, gid: None }
    }

    pub fn directory() -> Self {
        EntryMeta { kind: EntryKind::Directory, ..EntryMeta::file() }
    }

    pub fn symlink() -> Self {
        EntryMeta { kind: EntryKind::Symlink, ..EntryMeta::file() }
    }

    pub fn effective_mode(&self) -> u32 {
        match self.unix_mode.map(|m| m & mode::PERM_MASK) {
            Some(m) if m != 0 => m,
            _ => match self.kind {
                EntryKind::Directory => mode::DEFAULT_DIR,
                EntryKind::Symlink => 0o777,
                EntryKind::File => {
                    if self.dos_attrs.is_some_and(|a| a & dos::READONLY != 0) {
                        0o444
                    } else {
                        mode::DEFAULT_FILE
                    }
                }
            },
        }
    }

    pub fn is_readonly(&self) -> bool {
        if let Some(a) = self.dos_attrs {
            return a & dos::READONLY != 0;
        }
        self.unix_mode.is_some_and(|m| m & 0o200 == 0)
    }
}
