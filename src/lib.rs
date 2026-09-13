//! Read and write ZIP, 7z and tar archives. No dependencies.
//!
//! Point [`Archive`] at a path, set any options you want, then call
//! [`create_from`](Archive::create_from) or [`extract_to`](Archive::extract_to).
//!
//! ```no_run
//! use ttarchive::Archive;
//!
//! Archive::new("photos.tar.gz").create_from(["holiday"])?;
//! Archive::new("photos.tar.gz").extract_to("out")?;
//! # Ok::<(), ttarchive::Error>(())
//! ```
//!
//! The format is detected for you. Reading uses the file's magic bytes, writing
//! uses its name. [`set_type`](Archive::set_type) overrides both.
//!
//! Extraction is safe by default. Every entry name is checked before a single
//! byte is written, so no archive can drop files outside the directory you gave
//! it.
//!
//! [`ArchiveType`] lists the formats. [`ExtractOptions`] and [`CreateOptions`]
//! list the settings.

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
pub mod sevenz;
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
pub use pipeline::{CreateOptions, CreateSummary, ExtractOptions, ExtractSummary, Overwrite, Ownership, UnsafeEntries, tarball::Wrapper};
pub use platform::policy::NamePolicy;
pub use utils::error::{Error, Result};
pub use utils::progress::{Operation, ProgressCallback, ProgressUpdate};

use utils::progress::Reporter;

/// A format this crate can read, and usually write.
///
/// [`ArchiveType::ALL`] lists them all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ArchiveType {
    /// PKWARE ZIP.
    Zip,
    /// A plain, uncompressed tar.
    Tar,
    /// A tar compressed with gzip.
    TarGz,
    /// A tar compressed with bzip2.
    TarBz2,
    /// A tar compressed with xz.
    TarXz,
    /// A tar compressed with Zstandard.
    TarZst,
    /// A tar compressed with raw LZMA.
    TarLzma,
    /// A tar compressed with Unix compress. Read only.
    TarZ,
    /// A tar compressed with lzip.
    TarLz,
    /// 7-Zip.
    SevenZ,
}

const ZIP_SUFFIXES: [&str; 18] =
    [".zip", ".zipx", ".krzip", ".gf", ".jar", ".war", ".ear", ".apk", ".epub", ".odt", ".ods", ".odp", ".docx", ".xlsx", ".pptx", ".whl", ".crx", ".xpi"];

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
    /// Every format this build knows, whether or not it can write it.
    pub const ALL: [ArchiveType; 10] = [
        ArchiveType::Zip,
        ArchiveType::Tar,
        ArchiveType::TarGz,
        ArchiveType::TarBz2,
        ArchiveType::TarXz,
        ArchiveType::TarZst,
        ArchiveType::TarLzma,
        ArchiveType::TarZ,
        ArchiveType::TarLz,
        ArchiveType::SevenZ,
    ];

    /// The usual extension for this format, dot included.
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
            ArchiveType::SevenZ => ".7z",
        }
    }

    /// Work out the format from a file name.
    ///
    /// Matches the whole name, not just the last extension, so `.tar.gz` is
    /// told apart from a bare `.gz`. Shorthands like `.tgz` work too, and so
    /// does one volume of a 7z set, `.7z.001`.
    pub fn from_extension(path: &Path) -> Option<Self> {
        let name = path.file_name()?.to_str()?.to_ascii_lowercase();

        if let Some((stem, digits)) = name.rsplit_once('.')
            && digits.len() >= 3
            && digits.bytes().all(|b| b.is_ascii_digit())
            && stem.ends_with(".7z")
        {
            return Some(ArchiveType::SevenZ);
        }

        for (suffix, kind) in TARBALL_SUFFIXES {
            if name.len() > suffix.len() && name.ends_with(suffix) {
                return Some(kind);
            }
        }

        let last = name.rsplit_once('.')?.1;
        if last == "7z" {
            return Some(ArchiveType::SevenZ);
        }
        ZIP_SUFFIXES.iter().any(|suffix| suffix[1..] == *last).then_some(ArchiveType::Zip)
    }

    /// Every suffix that means this format, usual spelling first.
    ///
    /// The rest are the shorthands people actually use, such as `.tgz` for
    /// `.tar.gz`. Case does not matter when matching.
    pub fn extensions(self) -> Vec<&'static str> {
        if self == ArchiveType::Zip {
            return ZIP_SUFFIXES.to_vec();
        }
        if self == ArchiveType::SevenZ {
            return vec![".7z"];
        }

        let mut found: Vec<&'static str> = TARBALL_SUFFIXES.iter().filter(|(_, kind)| *kind == self).map(|(suffix, _)| *suffix).collect();

        let canonical = self.extension();
        if let Some(at) = found.iter().position(|s| s.eq_ignore_ascii_case(canonical)) {
            found.swap(0, at);
        }
        found
    }

    /// Work out the format from the start of a file.
    ///
    /// A compressed tarball is recognised by its wrapper. A bare tar needs at
    /// least 265 bytes to recognise, since its magic sits that far in.
    pub fn from_magic(prefix: &[u8]) -> Option<Self> {
        if zip::is_zip(prefix) {
            return Some(ArchiveType::Zip);
        }
        if sevenz::is_sevenz(prefix) {
            return Some(ArchiveType::SevenZ);
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

    /// Whether this build can write this format, not just read it.
    pub fn can_write(self) -> bool {
        match self.dispatch() {
            Dispatch::Zip | Dispatch::SevenZ => true,
            Dispatch::Tarball(wrapper) => wrapper.can_write(),
        }
    }

    fn dispatch(self) -> Dispatch {
        Dispatch::Tarball(match self {
            ArchiveType::Zip => return Dispatch::Zip,
            ArchiveType::SevenZ => return Dispatch::SevenZ,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Dispatch {
    Zip,
    SevenZ,
    Tarball(pipeline::tarball::Wrapper),
}

/// An archive, named by its path.
///
/// Build one with [`Archive::new`], chain any settings you want, then finish
/// with [`extract_to`](Archive::extract_to), [`create_from`](Archive::create_from),
/// or one of their shorthands. Nothing touches the disk until then.
pub struct Archive {
    path: PathBuf,
    kind: Option<ArchiveType>,
    extract_options: ExtractOptions,
    create_options: CreateOptions,
    callback: Option<Arc<dyn ProgressCallback>>,
}

impl Archive {
    /// Name the archive at `path`. Nothing is opened yet.
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Archive { path: path.into(), kind: None, extract_options: ExtractOptions::default(), create_options: CreateOptions::default(), callback: None }
    }

    /// Use this format instead of detecting one.
    pub fn set_type(mut self, kind: ArchiveType) -> Self {
        self.kind = Some(kind);
        self
    }

    /// How hard to compress. Defaults to [`Level::Default`].
    pub fn set_level(mut self, level: Level) -> Self {
        self.create_options.level = level;
        self
    }

    /// Compress each entry with this method.
    ///
    /// ZIP defaults to [`Method::Deflate`], and stores any entry that does not
    /// come out smaller. 7z defaults to LZMA2, which has no method code of its
    /// own. Naming one of the others here picks the 7z coder that means the
    /// same thing.
    ///
    /// A tarball has no per-entry method, since its wrapper compresses the
    /// whole stream, so asking for one fails with [`Error::Unsupported`]. So
    /// does any method [`Method::can_encode`] rejects.
    pub fn set_method(mut self, method: Method) -> Self {
        self.create_options.method = Some(method);
        self
    }

    /// How many worker threads to use, for both reading and writing.
    ///
    /// `None` is the default and uses every core. `Some(1)` keeps everything on
    /// one thread.
    ///
    /// Threads compress ZIP entries side by side, split gzip and bzip2 tarballs
    /// into pieces, and write extracted files in parallel.
    pub fn set_threads(mut self, threads: Option<usize>) -> Self {
        self.extract_options.threads = threads;
        self.create_options.threads = threads;
        self
    }

    /// Split the new archive into volumes of at most `bytes` each.
    ///
    /// A ZIP is written as `name.z01`, `name.z02`, and so on, with the last
    /// piece keeping the plain `name.zip`. Volumes must be between 64 KiB and
    /// 4 GiB, which is what its disk numbers can count.
    ///
    /// A 7z is written as `name.7z.001`, `name.7z.002`, and so on. There is no
    /// last piece under the plain name, and no upper size limit. The lower
    /// limit is 64 KiB.
    ///
    /// To read a set back, name any piece of it, or the archive itself. The
    /// rest are found from there.
    ///
    /// Tar has no volumes, so this fails with [`Error::Unsupported`].
    pub fn set_volume_size(mut self, bytes: u64) -> Self {
        self.create_options.volume_size = Some(bytes);
        self
    }

    /// Extract only these entries rather than the whole archive.
    ///
    /// Naming a directory takes everything inside it. Names that match nothing
    /// are ignored rather than reported.
    ///
    /// Everything else follows the selection: progress totals, the returned
    /// summary, and both of the strip settings.
    pub fn set_selection(mut self, names: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.extract_options.selection = names.into_iter().map(Into::into).collect();
        self
    }

    /// Encrypt with this password when writing, decrypt with it when reading.
    ///
    /// When writing, [`Archive::set_encryption`] picks the scheme. When reading,
    /// the archive does. Extracting without a password you need fails with
    /// [`Error::PasswordRequired`]. The wrong one fails with
    /// [`Error::WrongPassword`].
    ///
    /// A 7z encrypts its header as well as its contents, so a password hides
    /// the file names too. Tar encrypts nothing, so a password fails with
    /// [`Error::Unsupported`].
    pub fn set_password(mut self, password: impl Into<Password>) -> Self {
        let password = password.into();
        self.extract_options.password = Some(password.clone());
        self.create_options.password = Some(password);
        self
    }

    /// Which encryption scheme to use when a password is set.
    ///
    /// The default, [`EncryptionMethod::Aes256`], means WinZip AES in a ZIP and
    /// 7zAES in a 7z. One name, and whichever scheme the format defines.
    ///
    /// [`EncryptionMethod::ZipCrypto`] is broken and only worth using for old
    /// readers that accept nothing else. 7z has no equivalent and refuses it.
    pub fn set_encryption(mut self, encryption: EncryptionMethod) -> Self {
        self.create_options.encryption = encryption;
        self
    }

    /// What to do about an entry that would write outside the destination.
    ///
    /// The default, [`UnsafeEntries::Refuse`], stops the whole extraction.
    /// [`UnsafeEntries::Skip`] passes over the entry, extracts the rest, and
    /// counts what it skipped in [`ExtractSummary::refused`].
    pub fn set_unsafe_entries(mut self, policy: UnsafeEntries) -> Self {
        self.extract_options.unsafe_entries = policy;
        self
    }

    /// Drop `count` leading directories from every name while extracting.
    ///
    /// An archive of `myfolder/file.txt` and `myfolder/sub/some.exe` normally
    /// extracts to `dest/myfolder/...`. With `1` it extracts to `dest/file.txt`
    /// and `dest/sub/some.exe`.
    ///
    /// An entry left with nothing but its stripped prefix is skipped and
    /// counted in [`ExtractSummary::skipped`]. Stripping can also make two
    /// entries land on one path, which [`Archive::set_overwrite`] settles.
    pub fn set_strip_components(mut self, count: usize) -> Self {
        self.extract_options.strip_components = count;
        self
    }

    /// Drop the leading directory, but only if every entry shares it.
    ///
    /// Does nothing when they do not. Adds to
    /// [`Archive::set_strip_components`] rather than replacing it.
    pub fn set_strip_root(mut self, strip: bool) -> Self {
        self.extract_options.strip_root = strip;
        self
    }

    /// Store long runs of zeros as holes instead of as bytes.
    ///
    /// Tar only. Neither ZIP nor 7z can record a hole, so both refuse this with
    /// [`Error::Unsupported`].
    ///
    /// Finding the holes means reading every byte of every file, so it is off
    /// unless you ask. Files too small to gain from a hole map are stored
    /// whole.
    pub fn set_sparse(mut self, sparse: bool) -> Self {
        self.create_options.sparse = sparse;
        self
    }

    /// Recreate symbolic links while extracting. On by default.
    ///
    /// When off, links are skipped. Either way, a link can never point outside
    /// the destination.
    pub fn set_restore_symlinks(mut self, restore: bool) -> Self {
        self.extract_options.restore_symlinks = restore;
        self
    }

    /// Restore the permissions each entry recorded while extracting. On by
    /// default.
    ///
    /// See [`ExtractOptions::preserve_permissions`] for what that covers on
    /// each platform.
    pub fn set_preserve_permissions(mut self, preserve: bool) -> Self {
        self.extract_options.preserve_permissions = preserve;
        self
    }

    /// Restore the modified and last-accessed times each entry recorded while
    /// extracting. On by default.
    pub fn set_preserve_timestamps(mut self, preserve: bool) -> Self {
        self.extract_options.preserve_timestamps = preserve;
        self
    }

    /// Whose files the extracted ones become.
    ///
    /// The default, [`Ownership::Ignore`], leaves them belonging to whoever
    /// extracts. [`Ownership::Names`] and [`Ownership::Ids`] give them the
    /// owner the archive recorded, which only root on Unix is allowed to do.
    /// Whatever the system refuses is still extracted, and counted in
    /// [`ExtractSummary::owners_not_restored`].
    pub fn set_ownership(mut self, ownership: Ownership) -> Self {
        self.extract_options.ownership = ownership;
        self
    }

    /// What to do when an extracted file is already there.
    pub fn set_overwrite(mut self, overwrite: Overwrite) -> Self {
        self.extract_options.overwrite = overwrite;
        self
    }

    /// Use these extraction options, replacing anything set so far.
    pub fn with_extract_options(mut self, options: ExtractOptions) -> Self {
        self.extract_options = options;
        self
    }

    /// Use these creation options, replacing anything set so far.
    pub fn with_create_options(mut self, options: CreateOptions) -> Self {
        self.create_options = options;
        self
    }

    /// Call this as work progresses.
    ///
    /// The callback runs on worker threads, so it must be `Send + Sync`. It is
    /// throttled by both bytes and time, so it will not be called once per
    /// file on a large archive.
    pub fn on_progress<F>(mut self, callback: F) -> Self
    where
        F: Fn(&ProgressUpdate<'_>) + Send + Sync + 'static,
    {
        self.callback = Some(Arc::new(callback));
        self
    }

    /// Like [`Archive::on_progress`], for a callback you already share.
    pub fn with_progress(mut self, callback: Arc<dyn ProgressCallback>) -> Self {
        self.callback = Some(callback);
        self
    }

    /// The path this archive was named with.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// List what the archive holds, without extracting it.
    ///
    /// Naming any piece of a multi-volume set lists the whole set.
    ///
    /// ZIP and 7z both keep an index, so listing them is cheap. Tar keeps none,
    /// so a compressed tarball has to be decompressed to be listed.
    pub fn entries(&self) -> Result<Vec<Entry>> {
        let kind = self.resolve_for_read()?;

        match kind.dispatch() {
            Dispatch::Tarball(wrapper) => pipeline::tarball::entries(&self.path, wrapper),
            Dispatch::SevenZ => pipeline::sevenz::entries(&self.path, self.extract_options.password.as_ref()),
            Dispatch::Zip => {
                let volumes = zip::VolumeSet::discover(&self.path)?;
                let reader = zip::ZipReader::with_layout(volumes.open()?, volumes.layout().clone())?;
                Ok(reader.entries().to_vec())
            }
        }
    }

    /// Extract into a new directory named after the archive.
    ///
    /// `photos.zip` extracts into `photos/`.
    pub fn extract(self) -> Result<ExtractSummary> {
        let stem = self.path.file_stem().map(PathBuf::from).unwrap_or_else(|| PathBuf::from("extracted"));
        let dest = self.path.parent().map_or(stem.clone(), |p| p.join(&stem));
        self.extract_to(dest)
    }

    /// Extract into `dest`, creating it if it is not there.
    ///
    /// Every name is resolved and checked first, so nothing can be written
    /// outside `dest` no matter what the archive claims. Files are written in
    /// parallel where the format allows it.
    ///
    /// Permissions and timestamps come back as the archive recorded them,
    /// unless [`Archive::set_preserve_permissions`] or
    /// [`Archive::set_preserve_timestamps`] turns them off. Files belong to
    /// whoever extracts them unless [`Archive::set_ownership`] asks for the
    /// recorded owner.
    ///
    /// A tarball can hold things this crate cannot create, such as device nodes
    /// and fifos. Those are counted in [`ExtractSummary::specials`] and passed
    /// over, rather than left behind as empty files.
    pub fn extract_to(self, dest: impl AsRef<Path>) -> Result<ExtractSummary> {
        let kind = self.resolve_for_read()?;
        let reporter = self.reporter(Operation::Extract);

        match kind.dispatch() {
            Dispatch::Zip => pipeline::extract::extract(&self.path, dest.as_ref(), &self.extract_options, &reporter),
            Dispatch::SevenZ => pipeline::sevenz::extract(&self.path, dest.as_ref(), &self.extract_options, &reporter),
            Dispatch::Tarball(wrapper) => pipeline::tarball::extract(&self.path, dest.as_ref(), wrapper, &self.extract_options, &reporter),
        }
    }

    /// Create the archive from everything in the current directory.
    pub fn create(self) -> Result<CreateSummary> {
        let entries: Vec<PathBuf> = std::fs::read_dir(".")?.filter_map(|e| e.ok()).map(|e| e.path()).collect();
        self.create_from(entries)
    }

    /// Create the archive from `inputs`.
    ///
    /// Directories are stored with everything inside them.
    ///
    /// On Unix, a tarball stores the second and later names of a hard-linked
    /// file as links rather than as another copy of its contents. Files no
    /// format here can store, such as device nodes and fifos, are counted in
    /// [`CreateSummary::specials`] and left out.
    pub fn create_from<I: IntoIterator<Item = P>, P: AsRef<Path>>(self, inputs: I) -> Result<CreateSummary> {
        let kind = self.resolve_for_write()?;
        let reporter = self.reporter(Operation::Create);

        match kind.dispatch() {
            Dispatch::Zip => pipeline::create::create(&self.path, inputs, &self.create_options, &reporter),
            Dispatch::SevenZ => pipeline::sevenz::create(&self.path, inputs, &self.create_options, &reporter),
            Dispatch::Tarball(wrapper) => pipeline::tarball::create(&self.path, inputs, wrapper, &self.create_options, &reporter),
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
