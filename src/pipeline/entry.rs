use crate::crypto::winzip_aes::AesExtra;
use crate::platform::{EntryKind, EntryMeta};
use crate::zip::spec::flags;

/// One thing inside an archive, whatever the format.
#[derive(Debug, Clone)]
pub struct Entry {
    /// The name, always with `/` separators.
    pub name: String,

    /// How big the contents are once decompressed.
    pub size: u64,

    /// What it is, plus its permissions, owner and timestamps.
    pub meta: EntryMeta,

    /// Whatever else its own format recorded.
    pub detail: EntryDetail,
}

/// The fields only one format has, hanging off an [`Entry`].
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum EntryDetail {
    /// The entry came from a ZIP.
    Zip(ZipDetail),
    /// The entry came from a tar.
    Tar(TarDetail),
    /// The entry came from a 7z.
    SevenZ(SevenZDetail),
}

impl Entry {
    /// Whether the entry is a file, a directory or a symbolic link.
    pub fn kind(&self) -> EntryKind {
        self.meta.kind
    }

    /// Modification time as Unix epoch seconds.
    pub fn mtime(&self) -> i64 {
        self.meta.mtime.unwrap_or(0)
    }

    /// Unix permission bits, when the archive recorded them.
    pub fn mode(&self) -> Option<u32> {
        self.meta.unix_mode
    }

    /// Owning user id, when the archive recorded one.
    pub fn uid(&self) -> Option<u32> {
        self.meta.uid
    }

    /// Owning group id, when the archive recorded one.
    pub fn gid(&self) -> Option<u32> {
        self.meta.gid
    }

    /// Owning user name, when the archive recorded one. Tar only.
    pub fn user(&self) -> Option<&str> {
        self.meta.user.as_deref()
    }

    /// Owning group name, when the archive recorded one. Tar only.
    pub fn group(&self) -> Option<&str> {
        self.meta.group.as_deref()
    }

    /// True when this entry is a directory.
    pub fn is_dir(&self) -> bool {
        self.meta.kind == EntryKind::Directory
    }

    /// True when this entry is a symbolic link, whose data is the link target.
    pub fn is_symlink(&self) -> bool {
        self.meta.kind == EntryKind::Symlink
    }

    /// True when this entry is an ordinary file.
    pub fn is_file(&self) -> bool {
        self.meta.kind == EntryKind::File
    }

    /// The ZIP-specific fields, when this entry came from a ZIP archive.
    pub fn zip(&self) -> Option<&ZipDetail> {
        match &self.detail {
            EntryDetail::Zip(detail) => Some(detail),
            _ => None,
        }
    }

    /// The 7z-specific fields, when this entry came from a 7z archive.
    pub fn sevenz(&self) -> Option<&SevenZDetail> {
        match &self.detail {
            EntryDetail::SevenZ(detail) => Some(detail),
            _ => None,
        }
    }

    /// The tar-specific fields, when this entry came from a tar archive.
    pub fn tar(&self) -> Option<&TarDetail> {
        match &self.detail {
            EntryDetail::Tar(detail) => Some(detail),
            _ => None,
        }
    }
}

/// What a 7z entry carries that others do not.
#[derive(Debug, Clone)]
pub struct SevenZDetail {
    /// The checksum the header recorded for these bytes, if it recorded one.
    pub crc: Option<u32>,

    /// The Windows attribute word, whose high sixteen bits hold a Unix mode when
    /// [`crate::sevenz::spec::attribute::UNIX_EXTENSION`] is set in the low ones.
    pub attributes: Option<u32>,

    /// This is a deletion marker rather than a real entry.
    ///
    /// An incremental 7z uses one to say a path was removed. Extracting it
    /// deletes that path if it is there.
    pub is_anti: bool,
}

/// What a ZIP entry carries that others do not.
#[derive(Debug, Clone)]
pub struct ZipDetail {
    /// The name exactly as stored, before decoding.
    pub raw_name: Vec<u8>,

    /// Decoded per-entry comment.
    pub comment: String,

    /// The raw method number, as stored.
    ///
    /// Kept unresolved so that listing an archive never fails just because one
    /// entry uses a method this build cannot decode. [`ZipDetail::method`]
    /// resolves it.
    pub method_code: u16,

    /// CRC-32 of the uncompressed data.
    pub crc32: u32,

    /// Size of the stored, compressed data.
    pub compressed_size: u64,

    /// Offset of this entry's local file header within [`ZipDetail::disk_start`].
    ///
    /// A global file offset for a single-volume archive.
    pub local_header_offset: u64,

    /// Segment on which this entry's local header lives.
    ///
    /// Always 0 unless the archive is split across volumes.
    pub disk_start: u32,

    /// General purpose bit flags.
    pub flags: u16,

    /// `version made by`, whose high byte identifies the host system.
    pub version_made_by: u16,

    /// `version needed to extract`.
    pub version_needed: u16,

    /// Raw external file attributes.
    pub external_attributes: u32,

    /// Internal file attributes. Bit 0 marks the entry as text.
    pub internal_attributes: u16,

    /// Modification time from the MS-DOS fields, as Unix epoch seconds.
    pub dos_mtime: i64,

    /// WinZip AES parameters, when this entry is AE-x encrypted.
    pub aes: Option<AesExtra>,
}

impl ZipDetail {
    /// The compression method, or an error if this build does not know it.
    ///
    /// An AE-x encrypted entry stores 99 as its method and keeps the real one
    /// in an extra field. This returns the real one.
    pub fn method(&self) -> crate::Result<crate::codecs::Method> {
        crate::codecs::Method::from_code(self.effective_method_code())
    }

    /// The compression method number actually used for the data.
    pub fn effective_method_code(&self) -> u16 {
        match &self.aes {
            Some(aes) => aes.actual_method,
            None => self.method_code,
        }
    }

    /// True when the entry uses WinZip AES rather than traditional encryption.
    pub fn is_aes(&self) -> bool {
        self.aes.is_some()
    }

    /// True when the entry is encrypted.
    pub fn is_encrypted(&self) -> bool {
        self.flags & flags::ENCRYPTED != 0
    }

    /// True when sizes and CRC live in a data descriptor after the data.
    pub fn has_data_descriptor(&self) -> bool {
        self.flags & flags::DATA_DESCRIPTOR != 0
    }

    /// True when the name and comment are UTF-8.
    pub fn is_utf8(&self) -> bool {
        self.flags & flags::UTF8 != 0
    }
}

/// What a tar entry carries that others do not.
#[derive(Debug, Clone)]
pub struct TarDetail {
    /// Raw type flag byte from the header.
    pub typeflag: u8,

    /// Link target for a symlink or hard link, empty otherwise.
    pub linkname: String,

    /// Owning user name, when the archive recorded one.
    pub uname: String,

    /// Owning group name, when the archive recorded one.
    pub gname: String,

    /// Device major number for a device node.
    pub devmajor: u32,

    /// Device minor number for a device node.
    pub devminor: u32,

    /// True when the entry was stored sparsely and has been expanded on read.
    pub sparse: bool,
}
