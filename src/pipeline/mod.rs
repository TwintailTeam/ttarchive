pub mod create;
pub mod entry;
pub mod extract;
pub mod layout;
pub mod pool;
pub mod sevenz;
pub mod tarball;

use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::codecs::Level;
use crate::crypto::{Encryption, Password};
use crate::platform::accounts::Accounts;
use crate::platform::policy::NamePolicy;
use crate::platform::{EntryMeta, sys};
use crate::utils::error::Result;

/// What to do about an entry that would write outside the destination.
///
/// That means a name containing `..`, an absolute path or a drive letter, or a
/// symbolic link pointing somewhere outside.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum UnsafeEntries {
    /// Stop the extraction. This is the default.
    #[default]
    Refuse,

    /// Pass over the entry and extract the rest, counting it in
    /// [`ExtractSummary::refused`].
    Skip,
}

/// What to do when an extracted file is already there.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Overwrite {
    /// Replace it. This is the default.
    #[default]
    Always,
    /// Leave it alone and count the entry in [`ExtractSummary::skipped`].
    Never,
    /// Stop the extraction.
    Error,
}

/// Whose files the extracted ones become.
///
/// Tar records an owner and group for every entry, as numbers and usually as
/// names too. ZIP records the numbers when the archive was written on Unix. 7z
/// records neither, so its entries always belong to whoever extracts them.
///
/// Only Unix can hand a file to someone else, and only a process running as
/// root may give it away to another user. Anything refused, including every
/// entry on Windows, is extracted anyway, left belonging to whoever
/// extracted it, and counted in [`ExtractSummary::owners_not_restored`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Ownership {
    /// Leave every file belonging to whoever runs the extraction. This is the
    /// default, and what `tar` does for anyone but root.
    #[default]
    Ignore,

    /// Give each file the user and group numbers the archive recorded, as
    /// `tar --numeric-owner` does.
    ///
    /// Numbers mean different people on different machines, so this suits
    /// restoring onto the system the archive came from.
    Ids,

    /// Look up the user and group names the archive recorded, and give each
    /// file the numbers they have on this system, as `tar` does when run as
    /// root.
    ///
    /// A name that is not recorded, or that this system does not know, falls
    /// back to the recorded number. Names are looked up in `/etc/passwd` and
    /// `/etc/group`, so accounts that live only in a directory service such as
    /// LDAP are not found and fall back the same way.
    Names,
}

/// Everything that can change how an archive is extracted.
///
/// [`crate::Archive`] sets these for you one at a time. Build one of these
/// directly when you would rather set them all at once.
#[derive(Debug, Clone)]
pub struct ExtractOptions {
    /// How strict to be about the names an archive holds.
    pub name_policy: NamePolicy,

    /// What to do when an extracted file is already there.
    pub overwrite: Overwrite,

    /// Restore the permissions the archive recorded. On by default.
    ///
    /// On Unix that means the mode bits, setuid, setgid and sticky included; on
    /// Windows, the read-only attribute. Directories are done last, once
    /// everything inside them is written, so a directory recorded read-only
    /// can still be filled. Symbolic links keep whatever the system gives
    /// them.
    ///
    /// When this is off, extracted files get the system's default permissions.
    pub preserve_permissions: bool,

    /// Restore the modified and last-accessed times the archive recorded. On
    /// by default.
    ///
    /// Directories are done last, so writing their contents cannot change
    /// their times afterwards. Symbolic links are left alone, as setting a
    /// time through one would change what it points at.
    ///
    /// When this is off, extracted files carry the time they were written.
    pub preserve_timestamps: bool,

    /// Whose files the extracted ones become. See [`Ownership`].
    pub ownership: Ownership,

    /// Recreate symbolic links. When false, they are skipped instead.
    ///
    /// Either way, a link can never point outside the destination.
    pub restore_symlinks: bool,

    /// How many worker threads to use. `None` uses every core, `Some(1)` one.
    pub threads: Option<usize>,

    /// What to do about an entry that would write outside the destination.
    pub unsafe_entries: UnsafeEntries,

    /// Drop this many leading directories from every name.
    ///
    /// An archive of `myfolder/...` normally extracts into `dest/myfolder/...`.
    /// With `1` it extracts into `dest/...`. An entry left with nothing is
    /// skipped and counted in [`ExtractSummary::skipped`].
    ///
    /// Names are checked for safety first, so stripping can never turn a safe
    /// name into an escaping one.
    pub strip_components: usize,

    /// Drop the leading directory, but only if every entry shares it.
    ///
    /// Does nothing when they do not. Adds to
    /// [`ExtractOptions::strip_components`] rather than replacing it.
    pub strip_root: bool,

    /// Extract only these entries. Empty, the default, means all of them.
    ///
    /// Naming a directory takes everything inside it. Names that match nothing
    /// are ignored rather than reported.
    pub selection: Vec<String>,

    /// The password for an encrypted archive.
    ///
    /// Without it, encrypted entries fail with
    /// [`crate::Error::PasswordRequired`].
    pub password: Option<Password>,
}

impl Default for ExtractOptions {
    fn default() -> Self {
        ExtractOptions {
            name_policy: NamePolicy::default(),
            overwrite: Overwrite::default(),
            preserve_permissions: true,
            preserve_timestamps: true,
            ownership: Ownership::default(),
            restore_symlinks: true,
            threads: None,
            unsafe_entries: UnsafeEntries::default(),
            strip_components: 0,
            strip_root: false,
            selection: Vec::new(),
            password: None,
        }
    }
}

/// Everything that can change how an archive is written.
///
/// [`crate::Archive`] sets these for you one at a time. Build one of these
/// directly when you would rather set them all at once.
#[derive(Debug, Clone)]
pub struct CreateOptions {
    /// How hard to compress.
    pub level: Level,

    /// Which method to compress each entry with.
    ///
    /// `None` means deflate for a ZIP and LZMA2 for a 7z, or stored at
    /// [`Level::None`]. A method [`crate::codecs::Method::can_encode`] rejects
    /// fails with [`crate::Error::Unsupported`].
    pub method: Option<crate::codecs::Method>,

    /// Store symbolic links as links, rather than copying what they point at.
    pub store_symlinks: bool,

    /// Descend into directories instead of storing only what was named.
    pub recursive: bool,

    /// How many worker threads to use. `None` uses every core.
    pub threads: Option<usize>,

    /// A comment to store in the archive. ZIP only.
    pub comment: Vec<u8>,

    /// Store long runs of zeros as holes instead of as bytes.
    ///
    /// Tar only, and only for files big enough to gain from it. Finding the
    /// holes means reading every byte, so this is off unless you ask. ZIP and
    /// 7z have no hole to record and refuse it.
    pub sparse: bool,

    /// Split the output into volumes of at most this many bytes.
    ///
    /// `None` writes one file. See
    /// [`Archive::set_volume_size`](crate::Archive::set_volume_size) for how
    /// each format names and sizes its pieces.
    pub volume_size: Option<u64>,

    /// Encrypt with this password. `None` writes a plain archive.
    pub password: Option<Password>,

    /// Which encryption scheme to use when `password` is set.
    pub encryption: Encryption,
}

impl Default for CreateOptions {
    fn default() -> Self {
        CreateOptions {
            level: Level::default(),
            method: None,
            store_symlinks: true,
            recursive: true,
            threads: None,
            comment: Vec::new(),
            sparse: false,
            volume_size: None,
            password: None,
            encryption: Encryption::default(),
        }
    }
}

/// What an extraction did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExtractSummary {
    /// Files written.
    pub files: u64,

    /// Directories created.
    pub directories: u64,

    /// Symbolic links created.
    pub symlinks: u64,

    /// Hard links created. Tar only, as ZIP and 7z have no hard link entry.
    pub hardlinks: u64,

    /// Device nodes, fifos and sockets that were passed over.
    ///
    /// Rust cannot create them, and leaving an empty file in their place would
    /// misrepresent what the archive held.
    pub specials: u64,

    /// Entries skipped, either because the file was already there under
    /// [`Overwrite::Never`] or because
    /// [`ExtractOptions::strip_components`] left them with no name.
    pub skipped: u64,

    /// Entries passed over as unsafe under [`UnsafeEntries::Skip`].
    pub refused: u64,

    /// Files deleted because the archive said to. 7z only.
    ///
    /// An incremental 7z records a deletion as an anti item: an entry that
    /// names a path and carries nothing. Extracting one removes that path
    /// instead of writing it. Only paths that were really there are counted.
    pub deleted: u64,

    /// Entries whose recorded owner could not be given back.
    ///
    /// Always zero under [`Ownership::Ignore`]. Otherwise it counts every entry
    /// the system refused to hand over, which is all of them for anyone but
    /// root and everything on Windows. Entries that recorded no owner are not
    /// counted.
    pub owners_not_restored: u64,

    /// Uncompressed bytes written.
    pub bytes: u64,
}

/// What writing an archive did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CreateSummary {
    /// Files stored.
    pub files: u64,

    /// Directories stored.
    pub directories: u64,

    /// Symbolic links stored as links.
    pub symlinks: u64,

    /// Files stored as a link to an earlier entry rather than as a second copy.
    ///
    /// Always zero on Windows, where Rust exposes no inode to match them by.
    pub hardlinks: u64,

    /// Device nodes, fifos and sockets left out.
    ///
    /// No format here can store them, so they are counted rather than dropped
    /// without a word.
    pub specials: u64,

    /// Uncompressed bytes read.
    pub bytes: u64,

    /// How big the finished archive is, counting every volume.
    pub archive_size: u64,

    /// How many volumes were written. `1` for an ordinary single file.
    pub volumes: u32,
}

pub(crate) struct Restore {
    permissions: bool,
    timestamps: bool,
    ownership: Ownership,
    accounts: Accounts,
    refused: AtomicU64,
}

impl Restore {
    pub(crate) fn new(options: &ExtractOptions) -> Self {
        Restore {
            permissions: options.preserve_permissions,
            timestamps: options.preserve_timestamps,
            ownership: options.ownership,
            accounts: if options.ownership == Ownership::Names { Accounts::load() } else { Accounts::default() },
            refused: AtomicU64::new(0),
        }
    }

    pub(crate) fn anything(&self) -> bool {
        self.permissions || self.timestamps || self.ownership != Ownership::Ignore
    }

    pub(crate) fn apply(&self, path: &Path, meta: &EntryMeta) -> Result<()> {
        if self.ownership != Ownership::Ignore {
            let (uid, gid) = self.owner(meta);
            if !sys::apply_owner(path, meta, uid, gid)? {
                self.refused.fetch_add(1, Ordering::Relaxed);
            }
        }
        if self.timestamps {
            sys::apply_times(path, meta)?;
        }
        if self.permissions {
            sys::apply_permissions(path, meta)?;
        }
        Ok(())
    }

    pub(crate) fn refused(&self) -> u64 {
        self.refused.load(Ordering::Relaxed)
    }

    fn owner(&self, meta: &EntryMeta) -> (Option<u32>, Option<u32>) {
        match self.ownership {
            Ownership::Ignore => (None, None),
            Ownership::Ids => (meta.uid, meta.gid),
            Ownership::Names => (
                meta.user.as_deref().and_then(|name| self.accounts.user_id(name)).or(meta.uid),
                meta.group.as_deref().and_then(|name| self.accounts.group_id(name)).or(meta.gid),
            ),
        }
    }
}

pub(crate) const MEMORY_BUDGET: u64 = 64 * 1024 * 1024;

pub(crate) fn thread_count(requested: Option<usize>, work_items: usize) -> usize {
    let available = std::thread::available_parallelism().map_or(1, |n| n.get());
    let want = requested.unwrap_or(available).max(1);
    want.min(available.max(1)).min(work_items.max(1))
}
