pub mod create;
pub mod entry;
pub mod extract;
pub mod layout;
pub mod pool;
pub mod tarball;

use crate::codecs::Level;
use crate::crypto::{Encryption, Password};
use crate::platform::policy::NamePolicy;

/// What to do when an archive contains an entry that is unsafe to extract.
///
/// Unsafe means the entry would write outside the destination: a name with a
/// `..` component or a drive letter, or a symlink whose target escapes it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum UnsafeEntries {
    /// Abort the whole extraction.
    #[default]
    Refuse,

    /// Skip the offending entries and extract the rest, counting them in
    /// [`ExtractSummary::refused`].
    Skip,
}

/// What to do when an extracted path already exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Overwrite {
    /// Replace the existing file.
    #[default]
    Always,
    /// Leave the existing file alone and skip the entry.
    Never,
    /// Fail if anything would be replaced.
    Error,
}

/// Options controlling extraction.
#[derive(Debug, Clone)]
pub struct ExtractOptions {
    /// How strict to be about entry names.
    pub name_policy: NamePolicy,
    /// What to do about existing files.
    pub overwrite: Overwrite,
    /// Restore permissions and ownership bits recorded in the archive.
    pub preserve_permissions: bool,
    /// Recreate symbolic links. When false, symlink entries are skipped.
    ///
    /// Links are confined to the destination directory either way.
    pub restore_symlinks: bool,
    /// Worker threads. `None` uses the available parallelism; `Some(1)` is sequential.
    pub threads: Option<usize>,

    /// How to handle entries that would escape the destination directory.
    pub unsafe_entries: UnsafeEntries,

    /// Drop this many leading path components from every entry name.
    ///
    /// An archive whose entries are all `myfolder/...` extracts into
    /// `dest/myfolder/...` by default; with `1` it extracts into `dest/...`.
    /// Entries left with an empty path are skipped and counted in
    /// [`ExtractSummary::skipped`].
    ///
    /// Applied after the name is validated, so it cannot turn a safe name into
    /// an escaping one.
    pub strip_components: usize,

    /// Drop a leading component only when every entry shares the same one.
    ///
    /// Does nothing when the entries have no common root. Adds to
    /// [`ExtractOptions::strip_components`] rather than replacing it.
    pub strip_root: bool,

    /// Extract only these entries, by name.
    ///
    /// Empty, the default, extracts everything. A directory name takes
    /// everything beneath it. Names that match nothing are simply absent from
    /// the result rather than an error.
    pub selection: Vec<String>,

    /// Password for encrypted entries.
    ///
    /// Encrypted entries fail with [`crate::Error::PasswordRequired`] when this
    /// is `None`.
    pub password: Option<Password>,
}

impl Default for ExtractOptions {
    fn default() -> Self {
        ExtractOptions {
            name_policy: NamePolicy::default(),
            overwrite: Overwrite::default(),
            preserve_permissions: true,
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

/// Options controlling archive creation.
#[derive(Debug, Clone)]
pub struct CreateOptions {
    /// Compression level.
    pub level: Level,

    /// Compression method, when something other than the default is wanted.
    ///
    /// `None` deflates, or stores at [`Level::None`]. A method for which
    /// [`crate::codecs::Method::can_encode`] is false fails the creation with
    /// [`crate::Error::Unsupported`].
    pub method: Option<crate::codecs::Method>,
    /// Store symbolic links as links rather than following them.
    pub store_symlinks: bool,
    /// Descend into directories.
    pub recursive: bool,
    /// Worker threads. `None` uses the available parallelism.
    pub threads: Option<usize>,
    /// Archive comment.
    pub comment: Vec<u8>,

    /// Store long runs of zeros as holes rather than as data.
    ///
    /// Tar only, and only for files large enough that the map pays for itself.
    /// Finding the holes means reading each file's bytes, so this is off by
    /// default; ZIP has no sparse entry and refuses the option.
    pub sparse: bool,

    /// Split the output across volumes of at most this many bytes.
    ///
    /// `None` writes one file. `Some(n)` writes `name.z01`, `name.z02`, … with
    /// the final segment named `name.zip`, clamped to 64 KiB … 4 GiB.
    pub volume_size: Option<u64>,

    /// Encrypt entries with this password.
    ///
    /// `None` writes an unencrypted archive.
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
    /// Regular files written.
    pub files: u64,
    /// Directories created.
    pub directories: u64,
    /// Symbolic links created.
    pub symlinks: u64,
    /// Hard links created. Tar only; ZIP has no hard link entry.
    pub hardlinks: u64,
    /// Device nodes, fifos and sockets in the archive that were passed over.
    ///
    /// `std` offers no way to create one, and writing an empty regular file in
    /// its place would misrepresent the archive.
    pub specials: u64,
    /// Entries skipped: already present under [`Overwrite::Never`], or left with
    /// an empty path by [`ExtractOptions::strip_components`].
    pub skipped: u64,
    /// Entries refused as unsafe and skipped, under [`UnsafeEntries::Skip`].
    pub refused: u64,
    /// Total uncompressed bytes written.
    pub bytes: u64,
}

/// What an archive creation did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CreateSummary {
    /// Regular files stored.
    pub files: u64,
    /// Directory entries stored.
    pub directories: u64,
    /// Symbolic links stored.
    pub symlinks: u64,
    /// Files stored as a hard link to an earlier entry instead of a second copy.
    ///
    /// Always zero on Windows, where `std` exposes no inode to match names by.
    pub hardlinks: u64,
    /// Device nodes, fifos and sockets passed over.
    ///
    /// No archive format here stores them and `std` cannot recreate them, so
    /// they are counted rather than dropped silently.
    pub specials: u64,
    /// Total uncompressed bytes read.
    pub bytes: u64,
    /// Total size of the finished archive across all volumes.
    pub archive_size: u64,

    /// Number of volumes written. `1` for an ordinary single-file archive.
    pub volumes: u32,
}

/// How much of an archive either handler will hold in memory before it changes
/// tactics: the tarball reader parks the decoded stream on disk past this, and
/// zip creation stops taking more entries into a compression wave.
///
/// Shared so the two sides cannot drift apart.
pub(crate) const MEMORY_BUDGET: u64 = 64 * 1024 * 1024;

pub(crate) fn thread_count(requested: Option<usize>, work_items: usize) -> usize {
    let available = std::thread::available_parallelism().map_or(1, |n| n.get());
    let want = requested.unwrap_or(available).max(1);
    want.min(available.max(1)).min(work_items.max(1))
}
