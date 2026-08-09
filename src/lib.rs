//! Reading and writing ZIP archives and tarballs, with no dependencies.
//!
//! Point [`Archive`] at a path, set whatever options you want, and call
//! [`create_from`](Archive::create_from) or [`extract_to`](Archive::extract_to).
//! The format comes from the file's magic bytes when reading and from its name
//! when writing, unless [`set_type`](Archive::set_type) says otherwise.
//!
//! ```no_run
//! use ttarchive::Archive;
//!
//! Archive::new("photos.tar.gz").create_from(["holiday"])?;
//! Archive::new("photos.tar.gz").extract_to("out")?;
//! # Ok::<(), ttarchive::Error>(())
//! ```
//!
//! Entry names are checked before anything is written, so an archive cannot
//! place files outside the destination directory. See [`ExtractOptions`] for
//! the knobs, and [`ArchiveType`] for the formats.

#![warn(missing_docs)]
#[doc(hidden)]
pub mod codecs;
#[doc(hidden)]
pub mod crypto;
#[doc(hidden)]
pub mod pipeline;
#[doc(hidden)]
pub mod platform;
#[doc(hidden)]
pub mod tar;
#[doc(hidden)]
pub mod utils;
#[doc(hidden)]
pub mod zip;

use std::path::{Path, PathBuf};
use std::sync::Arc;

pub use codecs::{Level, Method};
pub use crypto::{Encryption as EncryptionMethod, Password};
pub use pipeline::entry::{Entry, EntryDetail, ZipDetail};
pub use pipeline::{CreateOptions, CreateSummary, ExtractOptions, ExtractSummary, Overwrite, UnsafeEntries};
pub use platform::policy::NamePolicy;
pub use utils::error::{Error, Result};
pub use utils::progress::{Operation, ProgressCallback, ProgressUpdate};

use utils::progress::Reporter;

/// An archive format.
///
/// [`ArchiveType::ALL`] lists every one this build knows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ArchiveType {
    /// PKWARE ZIP.
    Zip,
    /// Uncompressed tar.
    Tar,
    /// tar wrapped in gzip.
    TarGz,
    /// tar wrapped in bzip2.
    TarBz2,
    /// tar wrapped in xz.
    TarXz,
    /// tar wrapped in Zstandard.
    TarZst,
    /// tar wrapped in a bare LZMA stream.
    TarLzma,
    /// tar wrapped in Unix compress.
    TarZ,
    /// tar wrapped in lzip.
    TarLz,
}

const ZIP_SUFFIXES: [&str; 17] =
    [".zip", ".zipx", ".krzip", ".jar", ".war", ".ear", ".apk", ".epub", ".odt", ".ods", ".odp", ".docx", ".xlsx", ".pptx", ".whl", ".crx", ".xpi"];

const TARBALL_SUFFIXES: [(&str, ArchiveType); 21] = [
    (".tar.gz", ArchiveType::TarGz),
    (".tar.gzip", ArchiveType::TarGz),
    (".tar.bz2", ArchiveType::TarBz2),
    (".tar.bz", ArchiveType::TarBz2),
    (".tar.xz", ArchiveType::TarXz),
    (".tar.zst", ArchiveType::TarZst),
    (".tar.zstd", ArchiveType::TarZst),
    (".tar.lzma", ArchiveType::TarLzma),
    (".tar.lz", ArchiveType::TarLz),
    (".tar.z", ArchiveType::TarZ),
    (".tgz", ArchiveType::TarGz),
    (".taz", ArchiveType::TarGz),
    (".tbz2", ArchiveType::TarBz2),
    (".tbz", ArchiveType::TarBz2),
    (".tb2", ArchiveType::TarBz2),
    (".txz", ArchiveType::TarXz),
    (".tzst", ArchiveType::TarZst),
    (".tlz", ArchiveType::TarLzma),
    (".tz", ArchiveType::TarZ),
    (".tarz", ArchiveType::TarZ),
    (".tar", ArchiveType::Tar),
];

impl ArchiveType {
    /// Every format this crate knows, readable and writable alike.
    pub const ALL: [ArchiveType; 9] = [
        ArchiveType::Zip,
        ArchiveType::Tar,
        ArchiveType::TarGz,
        ArchiveType::TarBz2,
        ArchiveType::TarXz,
        ArchiveType::TarZst,
        ArchiveType::TarLzma,
        ArchiveType::TarZ,
        ArchiveType::TarLz,
    ];

    /// The usual file extension for this format, including the leading dot.
    pub fn extension(self) -> &'static str {
        match self {
            ArchiveType::Zip => ".zip",
            ArchiveType::Tar => ".tar",
            ArchiveType::TarGz => ".tar.gz",
            ArchiveType::TarBz2 => ".tar.bz2",
            ArchiveType::TarXz => ".tar.xz",
            ArchiveType::TarZst => ".tar.zst",
            ArchiveType::TarLzma => ".tar.lzma",
            ArchiveType::TarZ => ".tar.Z",
            ArchiveType::TarLz => ".tar.lz",
        }
    }

    /// The format a file name implies.
    ///
    /// Matches the whole name rather than the last extension, so `.tar.gz` is
    /// distinguished from a bare `.gz`, and the `.tgz` family of shorthands
    /// resolves to the same formats.
    pub fn from_extension(path: &Path) -> Option<Self> {
        let name = path.file_name()?.to_str()?.to_ascii_lowercase();

        for (suffix, kind) in TARBALL_SUFFIXES {
            if name.len() > suffix.len() && name.ends_with(suffix) {
                return Some(kind);
            }
        }

        let last = name.rsplit_once('.')?.1;
        ZIP_SUFFIXES.iter().any(|suffix| suffix[1..] == *last).then_some(ArchiveType::Zip)
    }

    /// Every file suffix that resolves to this format.
    ///
    /// The first is the usual spelling; the rest are the shorthands in common
    /// use, such as `.tgz` beside `.tar.gz`. Matching ignores case.
    pub fn extensions(self) -> Vec<&'static str> {
        if self == ArchiveType::Zip {
            return ZIP_SUFFIXES.to_vec();
        }

        let mut found: Vec<&'static str> = TARBALL_SUFFIXES.iter().filter(|(_, kind)| *kind == self).map(|(suffix, _)| *suffix).collect();

        let canonical = self.extension();
        if let Some(at) = found.iter().position(|s| s.eq_ignore_ascii_case(canonical)) {
            found.swap(0, at);
        }
        found
    }

    /// The format the leading bytes of a file identify.
    ///
    /// Needs at least 265 bytes to recognise a bare tar, whose `ustar` marker
    /// sits at offset 257. A wrapped tarball is identified by its wrapper.
    pub fn from_magic(prefix: &[u8]) -> Option<Self> {
        if zip::is_zip(prefix) {
            return Some(ArchiveType::Zip);
        }
        if codecs::gzip::is_gzip(prefix) {
            return Some(ArchiveType::TarGz);
        }
        if prefix.starts_with(b"BZh") {
            return Some(ArchiveType::TarBz2);
        }
        if prefix.starts_with(&[0xfd, b'7', b'z', b'X', b'Z', 0x00]) {
            return Some(ArchiveType::TarXz);
        }
        if prefix.starts_with(&[0x28, 0xb5, 0x2f, 0xfd]) {
            return Some(ArchiveType::TarZst);
        }
        if prefix.starts_with(b"LZIP") {
            return Some(ArchiveType::TarLz);
        }
        if prefix.starts_with(&[0x1f, 0x9d]) {
            return Some(ArchiveType::TarZ);
        }
        if tar::is_tar(prefix) {
            return Some(ArchiveType::Tar);
        }
        None
    }

    /// True when this crate can create an archive of this format.
    pub fn can_write(self) -> bool {
        match self.wrapper() {
            Some(wrapper) => wrapper.can_write(),
            None => true,
        }
    }

    fn wrapper(self) -> Option<pipeline::tarball::Wrapper> {
        use pipeline::tarball::Wrapper;
        Some(match self {
            ArchiveType::Zip => return None,
            ArchiveType::Tar => Wrapper::None,
            ArchiveType::TarGz => Wrapper::Gzip,
            ArchiveType::TarBz2 => Wrapper::Bzip2,
            ArchiveType::TarXz => Wrapper::Xz,
            ArchiveType::TarZst => Wrapper::Zstd,
            ArchiveType::TarLzma => Wrapper::Lzma,
            ArchiveType::TarZ => Wrapper::Compress,
            ArchiveType::TarLz => Wrapper::Lzip,
        })
    }
}

/// An archive, identified by path.
///
/// Set the format and options, then call [`Archive::extract`],
/// [`Archive::extract_to`], [`Archive::create`] or [`Archive::create_from`].
/// Without [`Archive::set_type`] the format comes from magic bytes when reading
/// and from the extension when writing.
pub struct Archive {
    path: PathBuf,
    kind: Option<ArchiveType>,
    extract_options: ExtractOptions,
    create_options: CreateOptions,
    callback: Option<Arc<dyn ProgressCallback>>,
}

impl Archive {
    /// Refer to the archive at `path`. Nothing is opened until an operation runs.
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Archive { path: path.into(), kind: None, extract_options: ExtractOptions::default(), create_options: CreateOptions::default(), callback: None }
    }

    /// Set the archive format explicitly, skipping detection.
    pub fn set_type(mut self, kind: ArchiveType) -> Self {
        self.kind = Some(kind);
        self
    }

    /// Set the compression level used when creating.
    pub fn set_level(mut self, level: Level) -> Self {
        self.create_options.level = level;
        self
    }

    /// Set the per-entry compression method used when creating.
    ///
    /// ZIP only. Defaults to [`Method::Deflate`]. Entries that do not get
    /// smaller are stored instead. Fails with [`Error::Unsupported`] for a
    /// method [`Method::can_encode`] rejects, and for a tarball, where the
    /// wrapper compresses the whole stream and a per-entry method means
    /// nothing.
    pub fn set_method(mut self, method: Method) -> Self {
        self.create_options.method = Some(method);
        self
    }

    /// Set the worker thread count for both directions.
    ///
    /// `None`, the default, uses the available parallelism; `Some(1)` is
    /// sequential. Threads compress entries of a ZIP and pieces of a gzip or
    /// bzip2 tarball side by side, and write extracted files in parallel.
    pub fn set_threads(mut self, threads: Option<usize>) -> Self {
        self.extract_options.threads = threads;
        self.create_options.threads = threads;
        self
    }

    /// Split the created archive into volumes of at most `bytes` each.
    ///
    /// ZIP only; fails with [`Error::Unsupported`] for a tarball. Writes
    /// `name.z01`, `name.z02`, … with the final segment named `name.zip`,
    /// clamped to 64 KiB … 4 GiB. Extraction takes any segment of the set and
    /// locates the rest.
    pub fn set_volume_size(mut self, bytes: u64) -> Self {
        self.create_options.volume_size = Some(bytes);
        self
    }

    /// Extract only the named entries, instead of the whole archive.
    ///
    /// Naming a directory takes everything beneath it. Names that match nothing
    /// are ignored. Progress totals and the returned summary cover only what
    /// was selected, and [`Archive::set_strip_components`] and
    /// [`Archive::set_strip_root`] apply to the selection rather than to the
    /// whole archive.
    pub fn set_selection(mut self, names: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.extract_options.selection = names.into_iter().map(Into::into).collect();
        self
    }

    /// Set the password used to encrypt when creating and decrypt when extracting.
    ///
    /// The scheme comes from [`Archive::set_encryption`] when creating and from
    /// the archive when extracting. A missing password fails with
    /// [`Error::PasswordRequired`], a wrong one with [`Error::WrongPassword`].
    ///
    /// Creating a tarball with a password fails with [`Error::Unsupported`]:
    /// tar has no entry encryption.
    pub fn set_password(mut self, password: impl Into<Password>) -> Self {
        let password = password.into();
        self.extract_options.password = Some(password.clone());
        self.create_options.password = Some(password);
        self
    }

    /// Set the encryption scheme used when creating with a password.
    ///
    /// Defaults to [`EncryptionMethod::Aes256`]. [`EncryptionMethod::ZipCrypto`]
    /// is cryptographically broken and exists for compatibility only.
    pub fn set_encryption(mut self, encryption: EncryptionMethod) -> Self {
        self.create_options.encryption = encryption;
        self
    }

    /// Set what happens when an entry would escape the destination directory.
    ///
    /// Defaults to [`UnsafeEntries::Refuse`], which aborts the extraction.
    /// [`UnsafeEntries::Skip`] extracts the rest and counts refusals in
    /// [`ExtractSummary::refused`].
    pub fn set_unsafe_entries(mut self, policy: UnsafeEntries) -> Self {
        self.extract_options.unsafe_entries = policy;
        self
    }

    /// Drop `count` leading path components from every entry when extracting.
    ///
    /// An archive holding `myfolder/file.txt` and `myfolder/sub/some.exe`
    /// extracts to `dest/myfolder/file.txt` by default; with `1` it extracts to
    /// `dest/file.txt` and `dest/sub/some.exe`. Entries left with an empty path
    /// are skipped and counted in [`ExtractSummary::skipped`]. Stripping can make
    /// two entries collide, which [`Archive::set_overwrite`] then resolves.
    pub fn set_strip_components(mut self, count: usize) -> Self {
        self.extract_options.strip_components = count;
        self
    }

    /// Drop one leading component when every entry shares the same one.
    ///
    /// Does nothing when the entries have no common root. Adds to
    /// [`Archive::set_strip_components`] rather than replacing it.
    pub fn set_strip_root(mut self, strip: bool) -> Self {
        self.extract_options.strip_root = strip;
        self
    }

    /// Store long runs of zeros as holes rather than as data.
    ///
    /// Tar only; a ZIP creation refuses it, since the format cannot record a
    /// hole. Finding the holes reads each file's bytes, so this is off unless
    /// asked for, and files too small to pay for the map are stored whole.
    pub fn set_sparse(mut self, sparse: bool) -> Self {
        self.create_options.sparse = sparse;
        self
    }

    /// Set what happens when an extracted path already exists.
    pub fn set_overwrite(mut self, overwrite: Overwrite) -> Self {
        self.extract_options.overwrite = overwrite;
        self
    }

    /// Replace the extraction options.
    pub fn with_extract_options(mut self, options: ExtractOptions) -> Self {
        self.extract_options = options;
        self
    }

    /// Replace the creation options.
    pub fn with_create_options(mut self, options: CreateOptions) -> Self {
        self.create_options = options;
        self
    }

    /// Receive progress updates.
    ///
    /// The callback runs on worker threads and is throttled by a byte and a time
    /// threshold.
    pub fn on_progress<F>(mut self, callback: F) -> Self
    where
        F: Fn(&ProgressUpdate<'_>) + Send + Sync + 'static,
    {
        self.callback = Some(Arc::new(callback));
        self
    }

    /// Attach a shared progress callback.
    pub fn with_progress(mut self, callback: Arc<dyn ProgressCallback>) -> Self {
        self.callback = Some(callback);
        self
    }

    /// The path this archive refers to.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// List the archive's entries without extracting.
    ///
    /// Accepts any segment of a multi-volume set. A compressed tarball has to
    /// be decoded to be listed, since tar keeps no index; a ZIP is read from
    /// its central directory.
    pub fn entries(&self) -> Result<Vec<Entry>> {
        let kind = self.resolve_for_read()?;

        match kind.wrapper() {
            Some(wrapper) => pipeline::tarball::entries(&self.path, wrapper),
            None => {
                let volumes = zip::VolumeSet::discover(&self.path)?;
                let reader = zip::ZipReader::with_layout(volumes.open()?, volumes.layout().clone())?;
                Ok(reader.entries().to_vec())
            }
        }
    }

    /// Extract into a directory named after the archive.
    ///
    /// `photos.zip` extracts into `photos/`.
    pub fn extract(self) -> Result<ExtractSummary> {
        let stem = self.path.file_stem().map(PathBuf::from).unwrap_or_else(|| PathBuf::from("extracted"));
        let dest = self.path.parent().map_or(stem.clone(), |p| p.join(&stem));
        self.extract_to(dest)
    }

    /// Extract into `dest`, creating it if needed.
    ///
    /// Entry names are resolved and checked before anything is written, so no
    /// entry can land outside `dest`. Files are written in parallel where the
    /// format allows it. Special files a tarball may hold, such as device nodes
    /// and fifos, cannot be created and are counted in
    /// [`ExtractSummary::specials`] rather than left as empty files.
    pub fn extract_to(self, dest: impl AsRef<Path>) -> Result<ExtractSummary> {
        let kind = self.resolve_for_read()?;
        let reporter = self.reporter(Operation::Extract);

        match kind.wrapper() {
            None => pipeline::extract::extract(&self.path, dest.as_ref(), &self.extract_options, &reporter),
            Some(wrapper) => pipeline::tarball::extract(&self.path, dest.as_ref(), wrapper, &self.extract_options, &reporter),
        }
    }

    /// Create the archive from the current directory's contents.
    pub fn create(self) -> Result<CreateSummary> {
        let entries: Vec<PathBuf> = std::fs::read_dir(".")?.filter_map(|e| e.ok()).map(|e| e.path()).collect();
        self.create_from(entries)
    }

    /// Create the archive containing `inputs`.
    ///
    /// Directories are stored recursively by default. On Unix, a tarball stores
    /// the second and later names of a hard-linked file as links rather than as
    /// copies. Files that cannot be stored at all, such as device nodes and
    /// fifos, are counted in [`CreateSummary::specials`].
    pub fn create_from<I: IntoIterator<Item = P>, P: AsRef<Path>>(self, inputs: I) -> Result<CreateSummary> {
        let kind = self.resolve_for_write()?;
        let reporter = self.reporter(Operation::Create);

        match kind.wrapper() {
            None => pipeline::create::create(&self.path, inputs, &self.create_options, &reporter),
            Some(wrapper) => pipeline::tarball::create(&self.path, inputs, wrapper, &self.create_options, &reporter),
        }
    }

    fn reporter(&self, operation: Operation) -> Reporter {
        match &self.callback {
            Some(cb) => Reporter::new(Arc::clone(cb), operation),
            None => Reporter::disabled(),
        }
    }

    fn resolve_for_read(&self) -> Result<ArchiveType> {
        if let Some(kind) = self.kind {
            return Ok(kind);
        }

        let mut prefix = [0u8; 512];
        if let Ok(mut file) = std::fs::File::open(&self.path) {
            use std::io::Read;
            let mut filled = 0usize;
            while filled < prefix.len() {
                match file.read(&mut prefix[filled..]) {
                    Ok(0) => break,
                    Ok(n) => filled += n,
                    Err(ref e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(_) => break,
                }
            }
            if let Some(kind) = ArchiveType::from_magic(&prefix[..filled]) {
                return Ok(kind);
            }
        }

        ArchiveType::from_extension(&self.path).ok_or_else(|| Error::UnknownFormat { path: Some(self.path.clone()) })
    }

    fn resolve_for_write(&self) -> Result<ArchiveType> {
        self.kind.or_else(|| ArchiveType::from_extension(&self.path)).ok_or_else(|| Error::UnknownFormat { path: Some(self.path.clone()) })
    }
}

impl std::fmt::Debug for Archive {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Archive").field("path", &self.path).field("kind", &self.kind).field("progress", &self.callback.is_some()).finish()
    }
}
