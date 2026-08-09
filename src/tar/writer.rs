use std::io::Write;

use crate::platform::{EntryKind, EntryMeta, mode};
use crate::tar::header::{self, BLOCK, Format, Header, Kind};
use crate::tar::pax::{self, Attributes};
use crate::tar::sparse;
use crate::utils::error::Result;

const PAX_NAME: &str = "PaxHeaders/pax";

const SPARSE_NAME: &str = "GNUSparseFile/data";

pub struct TarWriter<W: Write> {
    inner: W,
    written: u64,
    format: Format,
}

impl<W: Write> TarWriter<W> {
    pub fn new(inner: W) -> Self {
        TarWriter { inner, written: 0, format: Format::Pax }
    }

    pub fn with_format(inner: W, format: Format) -> Self {
        TarWriter { inner, written: 0, format }
    }

    pub fn written(&self) -> u64 {
        self.written
    }

    fn emit(&mut self, bytes: &[u8]) -> Result<()> {
        self.inner.write_all(bytes)?;
        self.written += bytes.len() as u64;
        Ok(())
    }

    fn pad_to_block(&mut self, size: u64) -> Result<()> {
        let pad = header::padding(size);
        if pad > 0 {
            self.emit(&vec![0u8; pad])?;
        }
        Ok(())
    }

    fn emit_metadata(&mut self, kind: Kind, name: &str, payload: &[u8]) -> Result<()> {
        let head = Header { name: name.as_bytes().to_vec(), mode: 0o644, size: payload.len() as u64, kind, format: self.format, ..Header::default() };

        self.emit(&header::write(&head))?;
        self.emit(payload)?;
        self.pad_to_block(payload.len() as u64)
    }

    /// Write one entry header, followed by `size` bytes the caller then streams.
    pub fn start_entry(&mut self, name: &str, meta: &EntryMeta, size: u64, linkname: &str) -> Result<()> {
        let kind = match meta.kind {
            EntryKind::Directory => Kind::Directory,
            EntryKind::Symlink => Kind::Symlink,
            EntryKind::File => Kind::Regular,
        };

        self.write_header(name, meta, size, linkname, kind)
    }

    /// Write a hard link to `linkname`, which names another entry in this
    /// archive rather than a path on disk. Hard links carry no data.
    pub fn add_hard_link(&mut self, name: &str, meta: &EntryMeta, linkname: &str) -> Result<()> {
        self.write_header(name, meta, 0, linkname, Kind::HardLink)
    }

    fn write_header(&mut self, name: &str, meta: &EntryMeta, size: u64, linkname: &str, kind: Kind) -> Result<()> {
        let mut stored = name.to_owned();
        if kind == Kind::Directory && !stored.ends_with('/') {
            stored.push('/');
        }

        let mut extended = Attributes::default();

        let name_fits = header::split_ustar_name(stored.as_bytes()).is_some();
        if !name_fits {
            match self.format {
                Format::Gnu => self.emit_metadata(Kind::GnuLongName, "././@LongLink", &with_nul(stored.as_bytes()))?,
                _ => extended.set("path", stored.as_bytes().to_vec()),
            }
        }

        if linkname.len() > header::LINKNAME.1 {
            match self.format {
                Format::Gnu => self.emit_metadata(Kind::GnuLongLink, "././@LongLink", &with_nul(linkname.as_bytes()))?,
                _ => extended.set("linkpath", linkname.as_bytes().to_vec()),
            }
        }

        let uid = meta.uid.unwrap_or(0) as u64;
        let gid = meta.gid.unwrap_or(0) as u64;

        if !extended.is_empty() {
            let payload = pax::encode(&extended);
            self.emit_metadata(Kind::PaxNext, PAX_NAME, &payload)?;
        }

        let head = Header {
            name: stored.as_bytes().to_vec(),
            mode: meta.effective_mode() & mode::PERM_MASK,
            uid,
            gid,
            size: if kind == Kind::Regular { size } else { 0 },
            mtime: meta.mtime.unwrap_or(0),
            kind,
            linkname: linkname.as_bytes().to_vec(),
            uname: Vec::new(),
            gname: Vec::new(),
            devmajor: 0,
            devminor: 0,
            format: self.format,
        };

        self.emit(&header::write(&head))
    }

    /// Write a file whose long runs of zeros are recorded as holes rather than
    /// stored, using the PAX 1.0 layout: the map sits at the front of the
    /// entry's data and the real name and size travel as attributes.
    ///
    /// Falls back to an ordinary entry when the holes would not pay for the map.
    pub fn add_sparse(&mut self, name: &str, meta: &EntryMeta, data: &[u8]) -> Result<bool> {
        let map = sparse::scan(data);
        if !sparse::worth_it(&map, data) {
            self.add_entry(name, meta, data, "")?;
            return Ok(false);
        }

        let head = sparse::to_data(&map);
        let body = sparse::gather(&map, data);
        let stored = (head.len() + body.len()) as u64;

        let mut extended = Attributes::default();
        extended.set("GNU.sparse.major", b"1".to_vec());
        extended.set("GNU.sparse.minor", b"0".to_vec());
        extended.set("GNU.sparse.name", name.as_bytes().to_vec());
        extended.set("GNU.sparse.realsize", data.len().to_string().into_bytes());
        extended.set("size", stored.to_string().into_bytes());

        self.emit_metadata(Kind::PaxNext, PAX_NAME, &pax::encode(&extended))?;

        let header = Header {
            name: SPARSE_NAME.as_bytes().to_vec(),
            mode: meta.effective_mode() & mode::PERM_MASK,
            uid: meta.uid.unwrap_or(0) as u64,
            gid: meta.gid.unwrap_or(0) as u64,
            size: stored,
            mtime: meta.mtime.unwrap_or(0),
            kind: Kind::Regular,
            format: self.format,
            ..Header::default()
        };

        self.emit(&header::write(&header))?;
        self.emit(&head)?;
        self.emit(&body)?;
        self.pad_to_block(stored)?;
        Ok(true)
    }

    /// Write entry data. Must total the `size` given to [`TarWriter::start_entry`].
    pub fn write_data(&mut self, data: &[u8]) -> Result<()> {
        self.emit(data)
    }

    /// Pad the entry just written out to a block boundary.
    pub fn finish_entry(&mut self, size: u64) -> Result<()> {
        self.pad_to_block(size)
    }

    pub fn add_entry(&mut self, name: &str, meta: &EntryMeta, data: &[u8], linkname: &str) -> Result<()> {
        self.start_entry(name, meta, data.len() as u64, linkname)?;
        if meta.kind == EntryKind::File {
            self.write_data(data)?;
            self.finish_entry(data.len() as u64)?;
        }
        Ok(())
    }

    /// Write the two zero blocks that end a tar stream.
    pub fn finish(mut self) -> Result<W> {
        self.emit(&[0u8; BLOCK * 2])?;

        let tail = self.written % (BLOCK as u64 * 20);
        if tail != 0 {
            self.emit(&vec![0u8; (BLOCK as u64 * 20 - tail) as usize])?;
        }

        self.inner.flush()?;
        Ok(self.inner)
    }
}

fn with_nul(bytes: &[u8]) -> Vec<u8> {
    let mut out = bytes.to_vec();
    out.push(0);
    out
}
