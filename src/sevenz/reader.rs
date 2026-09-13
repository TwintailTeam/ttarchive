use std::io::{self, Read};
use std::path::Path;

use crate::crypto::Password;
use crate::sevenz::chain::{self, Stream};
use crate::sevenz::header::{self, ArchiveHeader, RawHeader, SignatureHeader, StreamsInfo};
use crate::sevenz::source::{ArchiveSource, Source};
use crate::sevenz::spec;
use crate::utils::crc32;
use crate::utils::error::{Error, Result};
use crate::utils::io::Limited;

const MAX_HEADER_LEN: u64 = 1 << 31;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Location {
    pub file: usize,
    pub folder: usize,
    pub offset: u64,
    pub size: u64,
    pub crc: Option<u32>,
}

pub struct SevenZReader<S> {
    source: S,
    signature: SignatureHeader,
    header: ArchiveHeader,
    password: Option<Password>,
}

impl SevenZReader<ArchiveSource> {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        SevenZReader::new(ArchiveSource::open(path.as_ref())?)
    }

    pub fn open_with(path: impl AsRef<Path>, password: Option<Password>) -> Result<Self> {
        SevenZReader::with_password(ArchiveSource::open(path.as_ref())?, password)
    }

    pub fn volumes(&self) -> Vec<std::path::PathBuf> {
        self.source.paths()
    }
}

impl<S: Source> SevenZReader<S> {
    pub fn new(source: S) -> Result<Self> {
        SevenZReader::with_password(source, None)
    }

    pub fn with_password(source: S, password: Option<Password>) -> Result<Self> {
        let prefix = source.read_at(0, spec::SIGNATURE_HEADER_LEN as u64)?;
        let signature = SignatureHeader::read(&prefix)?;

        if signature.is_empty() {
            return Ok(SevenZReader { source, signature, header: ArchiveHeader::default(), password });
        }

        let bytes = read_header_bytes(&source, &signature)?;
        let header = match header::read(&bytes, signature.header_at()?)? {
            RawHeader::Plain(header) => *header,
            RawHeader::Encoded(streams) => {
                let decoded = decode_header(&source, &streams, password.as_ref())?;
                match header::read(&decoded, 0)? {
                    RawHeader::Plain(header) => *header,
                    RawHeader::Encoded(_) => return Err(Error::malformed("7z encoded header decodes to another encoded header")),
                }
            }
        };

        Ok(SevenZReader { source, signature, header, password })
    }

    pub fn header(&self) -> &ArchiveHeader {
        &self.header
    }

    pub fn signature(&self) -> &SignatureHeader {
        &self.signature
    }

    pub fn source(&self) -> &S {
        &self.source
    }

    pub fn packed_stream_at(&self, index: usize) -> Result<u64> {
        packed_offset(&self.header.streams, index)
    }

    pub fn folder_reader(&self, index: usize) -> Result<Stream<'_>> {
        folder_stream(&self.source, &self.header.streams, index, self.password.as_ref())
    }

    pub fn locations(&self) -> Result<Vec<Location>> {
        let streams = &self.header.streams;
        let mut locations = Vec::new();
        let (mut folder, mut within, mut offset, mut flat) = (0usize, 0usize, 0u64, 0usize);

        for (index, file) in self.header.files.iter().enumerate() {
            if !file.has_stream {
                continue;
            }

            while folder < streams.substreams.counts.len() && within >= streams.substreams.counts[folder] {
                folder += 1;
                within = 0;
                offset = 0;
            }
            if folder >= streams.folders.len() {
                return Err(Error::malformed("7z archive has more files with data than its folders hold substreams"));
            }

            let size = streams.substreams.sizes[flat];
            locations.push(Location { file: index, folder, offset, size, crc: streams.substreams.crcs[flat] });
            offset += size;
            within += 1;
            flat += 1;
        }

        Ok(locations)
    }

    pub fn entry_reader(&self, location: &Location) -> Result<Stream<'_>> {
        let mut folder = self.folder_reader(location.folder)?;
        if location.offset > 0 {
            let skipped = io::copy(&mut folder.by_ref().take(location.offset), &mut io::sink())?;
            if skipped != location.offset {
                return Err(Error::malformed("7z folder ends before the file it should hold"));
            }
        }
        Ok(Box::new(Limited::new(folder, location.size)))
    }
}

impl<S> std::fmt::Debug for SevenZReader<S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SevenZReader")
            .field("folders", &self.header.streams.folders.len())
            .field("files", &self.header.files.len())
            .field("version", &self.signature.version)
            .finish()
    }
}

fn packed_offset(streams: &StreamsInfo, index: usize) -> Result<u64> {
    streams
        .pack
        .offset_of(index)?
        .checked_add(spec::SIGNATURE_HEADER_LEN as u64)
        .ok_or_else(|| Error::malformed("7z packed stream lies past the end of the file"))
}

pub fn folder_stream<'a, S: Source>(source: &'a S, streams: &StreamsInfo, index: usize, password: Option<&Password>) -> Result<Stream<'a>> {
    let folder = streams.folders.get(index).ok_or_else(|| Error::malformed(format!("7z archive has no folder {index}")))?;
    let first = *streams.folder_first_pack.get(index).ok_or_else(|| Error::malformed("7z folder has no packed streams assigned to it"))?;

    let mut packed = Vec::with_capacity(folder.packed_indices.len());
    for offset in 0..folder.packed_indices.len() {
        let stream = first + offset;
        let size = *streams.pack.sizes.get(stream).ok_or_else(|| Error::malformed("7z folder references a packed stream the archive does not have"))?;
        packed.push(source.open(packed_offset(streams, stream)?, size)?);
    }

    chain::decoder(folder, packed, password)
}

fn decode_header<S: Source>(source: &S, streams: &StreamsInfo, password: Option<&Password>) -> Result<Vec<u8>> {
    if streams.folders.len() != 1 {
        return Err(Error::malformed(format!("7z encoded header spans {} folders; it has to be exactly one", streams.folders.len())));
    }

    let size = streams.folders[0].unpack_size()?;
    if size > MAX_HEADER_LEN {
        return Err(Error::malformed(format!("7z encoded header decodes to {size} bytes")));
    }

    let mut bytes = Vec::with_capacity(size.min(1 << 24) as usize);
    folder_stream(source, streams, 0, password)?.read_to_end(&mut bytes)?;
    if bytes.len() as u64 != size {
        return Err(Error::malformed(format!("7z encoded header decoded to {} bytes, not the {size} it declares", bytes.len())));
    }

    if let Some(expected) = streams.folders[0].crc {
        let found = crc32::checksum(&bytes);
        if found != expected {
            return Err(Error::ChecksumMismatch { entry: "7z encoded header".into(), expected, found });
        }
    }

    Ok(bytes)
}

fn read_header_bytes<S: Source>(source: &S, signature: &SignatureHeader) -> Result<Vec<u8>> {
    if signature.next_header_size > MAX_HEADER_LEN {
        return Err(Error::malformed(format!("7z header claims to be {} bytes", signature.next_header_size)));
    }

    let bytes = source.read_at(signature.header_at()?, signature.next_header_size)?;
    let computed = crc32::checksum(&bytes);
    if computed != signature.next_header_crc {
        return Err(Error::ChecksumMismatch { entry: "7z header".into(), expected: signature.next_header_crc, found: computed });
    }
    Ok(bytes)
}
