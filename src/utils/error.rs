use std::fmt;
use std::io;
use std::path::PathBuf;

/// A `Result` whose error is this crate's [`Error`].
pub type Result<T> = std::result::Result<T, Error>;

/// Everything that can go wrong reading or writing an archive.
#[derive(Debug)]
pub enum Error {
    /// The file system said no.
    Io(io::Error),

    /// The archive does not say what the format says it should.
    Malformed {
        /// What was wrong.
        detail: String,
        /// Where in the file, when that is known.
        at: Option<u64>,
    },

    /// The archive uses something this build cannot handle.
    Unsupported(Unsupported),

    /// An entry's contents do not match the checksum stored with them.
    ChecksumMismatch {
        /// The entry's name.
        entry: String,
        /// The checksum the archive recorded.
        expected: u32,
        /// The checksum its bytes actually have.
        found: u32,
    },

    /// An entry's contents are not the length stored with them.
    SizeMismatch {
        /// The entry's name.
        entry: String,
        /// The length the archive recorded.
        expected: u64,
        /// The length it actually produced.
        found: u64,
    },

    /// An entry would have been written outside the destination.
    UnsafeEntryPath {
        /// The name as the archive stored it.
        name: String,
        /// Why it was refused.
        reason: PathRejection,
    },

    /// Neither the name nor the leading bytes identify a format this build
    /// knows.
    UnknownFormat {
        /// The path that could not be identified.
        path: Option<PathBuf>,
    },

    /// A worker thread panicked, so the result cannot be trusted.
    WorkerPanic,

    /// An entry is encrypted and no password was given.
    PasswordRequired {
        /// The entry that needs one.
        entry: String,
    },

    /// The password is wrong.
    WrongPassword,

    /// The password was right but the data has been tampered with.
    AuthenticationFailed,
}

/// What exactly this build cannot handle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Unsupported {
    /// A compression method, by its number.
    CompressionMethod(u16),
    /// An encryption scheme this build does not implement.
    Encryption,
    /// PKWARE's proprietary strong encryption.
    StrongEncryption,
    /// A split archive in a shape this build cannot follow.
    SplitArchive,
    /// Something else, described in place.
    Other(&'static str),
}

/// Why an entry's name was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathRejection {
    /// The name is an absolute path, or names a drive.
    Absolute,
    /// The name climbs out of the destination with `..`.
    ParentTraversal,
    /// The name holds a character the platform will not accept.
    IllegalCharacter,
    /// The name is empty.
    Empty,
    /// A symbolic link points outside the destination.
    SymlinkEscape,
}

impl Error {
    pub(crate) fn malformed(detail: impl Into<String>) -> Self {
        Error::Malformed { detail: detail.into(), at: None }
    }

    pub(crate) fn malformed_at(detail: impl Into<String>, at: u64) -> Self {
        Error::Malformed { detail: detail.into(), at: Some(at) }
    }

    /// Whether this is a missing feature rather than a broken archive.
    pub fn is_unsupported(&self) -> bool {
        matches!(self, Error::Unsupported(_))
    }

    /// Whether the right password is all that was missing.
    pub fn needs_password(&self) -> bool {
        matches!(self, Error::PasswordRequired { .. } | Error::WrongPassword)
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io(e) => write!(f, "i/o error: {e}"),
            Error::Malformed { detail, at: Some(at) } => {
                write!(f, "malformed archive at offset {at}: {detail}")
            }
            Error::Malformed { detail, at: _None } => write!(f, "malformed archive: {detail}"),
            Error::Unsupported(u) => write!(f, "unsupported archive feature: {u}"),
            Error::ChecksumMismatch { entry, expected, found } => {
                write!(f, "checksum mismatch for {entry:?}: expected {expected:#010x}, computed {found:#010x}")
            }
            Error::SizeMismatch { entry, expected, found } => write!(f, "size mismatch for {entry:?}: expected {expected} bytes, produced {found}"),
            Error::UnsafeEntryPath { name, reason } => {
                write!(f, "refusing unsafe entry name {name:?}: {reason}")
            }
            Error::UnknownFormat { path: Some(p) } => {
                write!(f, "could not determine archive format of {}", p.display())
            }
            Error::UnknownFormat { path: None } => {
                write!(f, "could not determine archive format")
            }
            Error::WorkerPanic => write!(f, "a worker thread panicked"),
            Error::PasswordRequired { entry } => {
                write!(f, "entry {entry:?} is encrypted; a password is required")
            }
            Error::WrongPassword => write!(f, "incorrect password"),
            Error::AuthenticationFailed => write!(f, "encrypted data failed its authentication check; it was modified or truncated"),
        }
    }
}

impl fmt::Display for Unsupported {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Unsupported::CompressionMethod(m) => {
                write!(f, "compression method {m} ({})", method_name(*m))
            }
            Unsupported::Encryption => write!(f, "encrypted entries"),
            Unsupported::StrongEncryption => write!(f, "strong / central directory encryption"),
            Unsupported::SplitArchive => write!(f, "split or spanned archives"),
            Unsupported::Other(s) => f.write_str(s),
        }
    }
}

impl fmt::Display for PathRejection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            PathRejection::Absolute => "name is absolute",
            PathRejection::ParentTraversal => "name escapes the destination via `..`",
            PathRejection::IllegalCharacter => "name contains an illegal character",
            PathRejection::Empty => "name is empty",
            PathRejection::SymlinkEscape => "symlink target escapes the destination",
        })
    }
}

fn method_name(method: u16) -> &'static str {
    match method {
        0 => "stored",
        1 => "shrunk",
        2..=5 => "reduced",
        6 => "imploded",
        8 => "deflate",
        9 => "deflate64",
        10 => "PKWARE DCL imploded",
        12 => "bzip2",
        14 => "lzma",
        16 => "IBM z/OS CMPSC",
        18 => "IBM TERSE",
        19 => "IBM LZ77",
        93 => "zstd",
        94 => "mp3",
        95 => "xz",
        96 => "JPEG variant",
        97 => "WavPack",
        98 => "PPMd",
        99 => "AE-x encryption marker",
        _ => "unknown",
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<io::Error> for Error {
    fn from(e: io::Error) -> Self {
        if e.get_ref().is_some_and(|inner| inner.is::<Error>()) {
            if let Some(boxed) = e.into_inner() {
                match boxed.downcast::<Error>() {
                    Ok(inner) => return *inner,
                    Err(other) => return Error::Io(io::Error::other(other)),
                }
            }
            unreachable!("get_ref reported an inner error");
        }
        Error::Io(e)
    }
}

impl From<Error> for io::Error {
    fn from(e: Error) -> Self {
        match e {
            Error::Io(e) => e,
            other => io::Error::new(io::ErrorKind::InvalidData, other),
        }
    }
}
