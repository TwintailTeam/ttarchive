use std::fmt;
use std::io;
use std::path::PathBuf;

pub type Result<T> = std::result::Result<T, Error>;

/// Everything that can go wrong while reading or writing an archive.
#[derive(Debug)]
pub enum Error {
    /// An underlying I/O failure.
    Io(io::Error),

    /// The byte stream is not a well-formed archive of the expected format.
    Malformed { detail: String, at: Option<u64> },

    /// The archive is well formed but uses a feature this crate does not implement.
    Unsupported(Unsupported),

    /// A stored checksum did not match the data that was actually read.
    ChecksumMismatch { entry: String, expected: u32, found: u32 },

    /// A decompressed entry did not have the length its metadata promised.
    SizeMismatch { entry: String, expected: u64, found: u64 },

    /// An entry name would escape the extraction directory, or is otherwise unsafe.
    UnsafeEntryPath { name: String, reason: PathRejection },

    /// The archive format could not be determined from magic bytes or extension.
    UnknownFormat { path: Option<PathBuf> },

    /// A worker thread panicked during parallel processing.
    WorkerPanic,

    /// The entry is encrypted and no password was supplied.
    PasswordRequired { entry: String },

    /// The supplied password is wrong.
    WrongPassword,

    /// An encrypted entry failed its authentication check, having been modified
    /// or truncated after it was written.
    AuthenticationFailed,
}

/// The specific archive feature that is not implemented.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Unsupported {
    /// Compression method number that this build cannot decode.
    CompressionMethod(u16),
    /// Entry is encrypted and no decryption is implemented.
    Encryption,
    /// Strong or central directory encryption.
    StrongEncryption,
    /// Multi-disk split or spanned archive.
    SplitArchive,
    /// A named feature that is recognised but not handled.
    Other(&'static str),
}

/// Why an entry name was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathRejection {
    /// Name is absolute (leading `/` or a `C:` style drive prefix).
    Absolute,
    /// Name contains a `..` component that walks above the destination.
    ParentTraversal,
    /// Name contains a NUL byte or another character illegal in a path.
    IllegalCharacter,
    /// Name is empty.
    Empty,
    /// Entry is a symlink whose target resolves outside the destination.
    SymlinkEscape,
}

impl Error {
    pub(crate) fn malformed(detail: impl Into<String>) -> Self {
        Error::Malformed { detail: detail.into(), at: None }
    }

    pub(crate) fn malformed_at(detail: impl Into<String>, at: u64) -> Self {
        Error::Malformed { detail: detail.into(), at: Some(at) }
    }

    /// True when the failure is an unimplemented feature rather than bad data.
    pub fn is_unsupported(&self) -> bool {
        matches!(self, Error::Unsupported(_))
    }

    /// True when the operation failed only for want of a correct password.
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
