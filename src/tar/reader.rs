use std::io::{Read, Write};

use crate::platform::{EntryKind, EntryMeta};
use crate::tar::header::{self, BLOCK, Format, Header, Kind};
use crate::tar::pax::{self, Attributes};
use crate::tar::sparse::{self, Map};
use crate::utils::error::{Error, Result};

const MAX_METADATA: u64 = 64 * 1024 * 1024;

#[derive(Debug, Clone)]
pub struct TarEntry {
    pub name: String,
    pub size: u64,
    pub meta: EntryMeta,
    pub kind: Kind,
    pub linkname: String,
    pub uname: String,
    pub gname: String,
    pub devmajor: u32,
    pub devminor: u32,
    pub format: Format,
    pub sparse: Option<Map>,
    pub data_offset: u64,
}

impl TarEntry {
    /// How many bytes of data follow the header, which for a sparse entry is
    /// less than the size the file will have once its map is expanded.
    pub fn stored_size(&self) -> u64 {
        match &self.sparse {
            Some(map) if map.in_data => map.stored,
            Some(map) if !map.is_empty() => map.stored_size(),
            _ => self.size,
        }
    }

    /// Whether the body has to be assembled from a sparse map rather than
    /// copied straight out.
    pub fn is_sparse(&self) -> bool {
        self.sparse.as_ref().is_some_and(|map| !map.is_empty() || map.in_data)
    }

    pub fn entry_kind(&self) -> EntryKind {
        match self.kind {
            Kind::Directory | Kind::GnuDumpDir => EntryKind::Directory,
            Kind::Symlink => EntryKind::Symlink,
            _ => EntryKind::File,
        }
    }
}

pub struct TarReader<R> {
    inner: R,
    position: u64,
    global: Attributes,
    finished: bool,
}

impl<R: Read> TarReader<R> {
    pub fn new(inner: R) -> Self {
        TarReader { inner, position: 0, global: Attributes::default(), finished: false }
    }

    pub fn into_inner(self) -> R {
        self.inner
    }

    pub fn position(&self) -> u64 {
        self.position
    }

    fn read_exact_tracked(&mut self, buf: &mut [u8]) -> Result<bool> {
        let mut filled = 0usize;
        while filled < buf.len() {
            match self.inner.read(&mut buf[filled..]) {
                Ok(0) => break,
                Ok(n) => filled += n,
                Err(ref e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(Error::from(e)),
            }
        }
        self.position += filled as u64;

        if filled == 0 {
            return Ok(false);
        }
        if filled < buf.len() {
            return Err(Error::malformed("tar stream ends mid-block"));
        }
        Ok(true)
    }

    fn read_block(&mut self) -> Result<Option<[u8; BLOCK]>> {
        let mut block = [0u8; BLOCK];
        if !self.read_exact_tracked(&mut block)? {
            return Ok(None);
        }
        Ok(Some(block))
    }

    fn read_payload(&mut self, size: u64) -> Result<Vec<u8>> {
        if size > MAX_METADATA {
            return Err(Error::malformed(format!("tar metadata entry claims {size} bytes")));
        }
        let mut data = vec![0u8; size as usize];
        if size > 0 && !self.read_exact_tracked(&mut data)? {
            return Err(Error::malformed("tar stream ends inside an entry"));
        }
        self.skip(header::padding(size) as u64)?;
        Ok(data)
    }

    pub fn skip(&mut self, count: u64) -> Result<()> {
        let mut left = count;
        let mut scratch = [0u8; BLOCK];
        while left > 0 {
            let take = left.min(BLOCK as u64) as usize;
            if !self.read_exact_tracked(&mut scratch[..take])? {
                return Err(Error::malformed("tar stream ends while skipping"));
            }
            left -= take as u64;
        }
        Ok(())
    }

    /// Read the next entry header, leaving the reader positioned at its data.
    pub fn next_entry(&mut self) -> Result<Option<TarEntry>> {
        if self.finished {
            return Ok(None);
        }

        let mut pending = Attributes::default();
        let mut long_name: Option<Vec<u8>> = None;
        let mut long_link: Option<Vec<u8>> = None;

        loop {
            let Some(block) = self.read_block()? else {
                self.finished = true;
                return Ok(None);
            };

            if header::is_zero_block(&block) {
                match self.read_block()? {
                    Some(second) if header::is_zero_block(&second) => {}
                    _ => {}
                }
                self.finished = true;
                return Ok(None);
            }

            let head = header::parse(&block)?;

            match head.kind {
                Kind::GnuLongName => {
                    long_name = Some(strip_nul(self.read_payload(head.size)?));
                    continue;
                }
                Kind::GnuLongLink => {
                    long_link = Some(strip_nul(self.read_payload(head.size)?));
                    continue;
                }
                Kind::PaxGlobal => {
                    let data = self.read_payload(head.size)?;
                    self.global = pax::parse(&data)?;
                    continue;
                }
                Kind::PaxNext => {
                    let data = self.read_payload(head.size)?;
                    pending.merge(&pax::parse(&data)?);
                    continue;
                }
                Kind::GnuVolume => {
                    self.skip(head.size + header::padding(head.size) as u64)?;
                    continue;
                }
                _ => {}
            }

            let gnu_map = match head.kind {
                Kind::GnuSparse => Some(self.read_gnu_sparse_map(&block)?),
                _ => None,
            };

            return Ok(Some(self.assemble(head, pending, long_name, long_link, gnu_map)?));
        }
    }

    fn read_gnu_sparse_map(&mut self, header: &[u8; BLOCK]) -> Result<Map> {
        let (mut map, mut more) = sparse::from_gnu_header(header)?;

        while more {
            let block = self.read_block()?.ok_or_else(|| Error::malformed("tar sparse entry ends before its map does"))?;
            more = sparse::from_gnu_extension(&block, &mut map)?;
        }

        map.validate()?;
        Ok(map)
    }

    fn assemble(
        &mut self,
        head: Header,
        pending: Attributes,
        long_name: Option<Vec<u8>>,
        long_link: Option<Vec<u8>>,
        gnu_map: Option<Map>,
    ) -> Result<TarEntry> {
        let mut attributes = self.global.clone();
        attributes.merge(&pending);

        let name = attributes
            .text("GNU.sparse.name")
            .or_else(|| attributes.text("path"))
            .or_else(|| long_name.map(|raw| String::from_utf8_lossy(&raw).into_owned()))
            .unwrap_or_else(|| String::from_utf8_lossy(&head.name).into_owned());

        let linkname = attributes
            .text("linkpath")
            .or_else(|| long_link.map(|raw| String::from_utf8_lossy(&raw).into_owned()))
            .unwrap_or_else(|| String::from_utf8_lossy(&head.linkname).into_owned());

        let size = attributes.number("size").unwrap_or(head.size);
        let mtime = attributes.seconds("mtime").unwrap_or(head.mtime);
        let uid = attributes.number("uid").unwrap_or(head.uid) as u32;
        let gid = attributes.number("gid").unwrap_or(head.gid) as u32;

        let mut sparse = match gnu_map {
            Some(map) => Some(map),
            None => sparse::from_pax(&attributes)?,
        };

        let size = match &mut sparse {
            Some(map) if map.in_data => {
                map.stored = size;
                map.real_size
            }
            Some(map) if !map.is_empty() => map.real_size,
            _ => size,
        };

        let kind = head.kind;
        let entry_kind = match kind {
            Kind::Directory | Kind::GnuDumpDir => EntryKind::Directory,
            Kind::Symlink => EntryKind::Symlink,
            _ => EntryKind::File,
        };

        let meta = EntryMeta {
            kind: entry_kind,
            unix_mode: Some(head.mode),
            dos_attrs: None,
            mtime: Some(mtime),
            atime: attributes.seconds("atime"),
            ctime: attributes.seconds("ctime"),
            uid: Some(uid),
            gid: Some(gid),
        };

        Ok(TarEntry {
            name,
            size,
            meta,
            kind,
            linkname,
            uname: attributes.text("uname").unwrap_or_else(|| String::from_utf8_lossy(&head.uname).into_owned()),
            gname: attributes.text("gname").unwrap_or_else(|| String::from_utf8_lossy(&head.gname).into_owned()),
            devmajor: head.devmajor,
            devminor: head.devminor,
            format: head.format,
            sparse,
            data_offset: self.position,
        })
    }

    /// Copy the current entry's data into `out`, then advance past its padding.
    ///
    /// Nothing larger than `buffer` is held, so an entry of any size can be
    /// written straight to disk. `on_bytes` is called with each chunk's length
    /// as it is written. A sparse entry has to be assembled from its map, so
    /// use [`TarReader::read_data`] for those.
    pub fn copy_data<W: Write>(&mut self, entry: &TarEntry, out: &mut W, buffer: &mut [u8], mut on_bytes: impl FnMut(u64)) -> Result<u64> {
        if !entry.kind.carries_data() {
            return Ok(0);
        }

        let stored = entry.stored_size();
        let mut left = stored;

        while left > 0 {
            let want = left.min(buffer.len() as u64) as usize;
            if !self.read_exact_tracked(&mut buffer[..want])? {
                return Err(Error::malformed(format!("tar entry {:?} ends early", entry.name)));
            }
            out.write_all(&buffer[..want])?;
            left -= want as u64;
            on_bytes(want as u64);
        }

        self.skip(header::padding(stored) as u64)?;
        Ok(stored)
    }

    /// Read the current entry's data, then advance past its block padding.
    pub fn read_data(&mut self, entry: &TarEntry) -> Result<Vec<u8>> {
        let stored = entry.stored_size();

        if !entry.kind.carries_data() {
            return Ok(Vec::new());
        }

        let mut data = vec![0u8; stored as usize];
        if stored > 0 && !self.read_exact_tracked(&mut data)? {
            return Err(Error::malformed(format!("tar entry {:?} ends early", entry.name)));
        }
        self.skip(header::padding(stored) as u64)?;

        match &entry.sparse {
            Some(map) if map.in_data => {
                let (map, used) = sparse::from_data(&data, map.real_size)?;
                sparse::expand(&map, &data[used..])
            }
            Some(map) if !map.is_empty() => sparse::expand(map, &data),
            _ => Ok(data),
        }
    }

    /// Advance past the current entry's data without decoding it.
    pub fn skip_data(&mut self, entry: &TarEntry) -> Result<()> {
        if !entry.kind.carries_data() {
            return Ok(());
        }
        let stored = entry.stored_size();
        self.skip(stored + header::padding(stored) as u64)
    }
}

fn strip_nul(mut raw: Vec<u8>) -> Vec<u8> {
    while raw.last() == Some(&0) {
        raw.pop();
    }
    raw
}
