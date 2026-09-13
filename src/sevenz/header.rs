use crate::sevenz::folder::{Folder, read_unpack_sizes};
use crate::sevenz::number::{bits, count, defined_bits, number};
use crate::sevenz::spec::{self, property};
use crate::utils::bytes::Cursor;
use crate::utils::crc32;
use crate::utils::error::{Error, Result, Unsupported};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SignatureHeader {
    pub next_header_offset: u64,
    pub next_header_size: u64,
    pub next_header_crc: u32,
    pub version: (u8, u8),
}

impl SignatureHeader {
    pub fn read(bytes: &[u8]) -> Result<Self> {
        if bytes.len() < spec::SIGNATURE_HEADER_LEN {
            return Err(Error::malformed("file is too short to hold a 7z signature header"));
        }
        if bytes[..spec::SIGNATURE.len()] != spec::SIGNATURE {
            return Err(Error::malformed("file does not start with the 7z signature"));
        }

        let version = (bytes[6], bytes[7]);
        if version.0 != spec::VERSION_MAJOR {
            return Err(Error::Unsupported(Unsupported::Other("a 7z archive written to a future major format version")));
        }

        let start = &bytes[12..spec::SIGNATURE_HEADER_LEN];
        let stored = u32::from_le_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]);
        let computed = crc32::checksum(start);
        if stored != computed {
            return Err(Error::ChecksumMismatch { entry: "7z start header".into(), expected: stored, found: computed });
        }

        let mut cursor = Cursor::new(start, 12);
        Ok(SignatureHeader {
            next_header_offset: cursor.u64("the header offset")?,
            next_header_size: cursor.u64("the header size")?,
            next_header_crc: cursor.u32("the header checksum")?,
            version,
        })
    }

    pub fn is_empty(&self) -> bool {
        self.next_header_size == 0
    }

    pub fn header_at(&self) -> Result<u64> {
        self.next_header_offset.checked_add(spec::SIGNATURE_HEADER_LEN as u64).ok_or_else(|| Error::malformed("7z header offset overflows the file"))
    }
}

#[derive(Debug, Clone, Default)]
pub struct PackInfo {
    pub pos: u64,
    pub sizes: Vec<u64>,
}

impl PackInfo {
    pub fn offset_of(&self, index: usize) -> Result<u64> {
        if index >= self.sizes.len() {
            return Err(Error::malformed("7z folder references a packed stream the archive does not have"));
        }
        let mut at = self.pos;
        for size in &self.sizes[..index] {
            at = at.checked_add(*size).ok_or_else(|| Error::malformed("7z packed stream offsets overflow"))?;
        }
        Ok(at)
    }
}

#[derive(Debug, Clone, Default)]
pub struct SubStreams {
    pub counts: Vec<usize>,
    pub sizes: Vec<u64>,
    pub crcs: Vec<Option<u32>>,
}

impl SubStreams {
    pub fn range_of(&self, index: usize) -> std::ops::Range<usize> {
        let start: usize = self.counts[..index].iter().sum();
        start..start + self.counts[index]
    }
}

#[derive(Debug, Clone, Default)]
pub struct StreamsInfo {
    pub pack: PackInfo,
    pub folders: Vec<Folder>,
    pub substreams: SubStreams,
    pub folder_first_pack: Vec<usize>,
}

#[derive(Debug, Clone, Default)]
pub struct FileEntry {
    pub name: String,
    pub has_stream: bool,
    pub is_empty_file: bool,
    pub is_anti: bool,
    pub ctime: Option<u64>,
    pub atime: Option<u64>,
    pub mtime: Option<u64>,
    pub attributes: Option<u32>,
}

impl FileEntry {
    pub fn is_directory(&self) -> bool {
        !self.has_stream && !self.is_empty_file && !self.is_anti
    }
}

#[derive(Debug, Clone, Default)]
pub struct ArchiveHeader {
    pub streams: StreamsInfo,
    pub files: Vec<FileEntry>,
}

#[derive(Debug, Clone)]
pub enum RawHeader {
    Plain(Box<ArchiveHeader>),
    Encoded(Box<StreamsInfo>),
}

pub fn read(bytes: &[u8], at: u64) -> Result<RawHeader> {
    let mut cursor = Cursor::new(bytes, at);
    match read_id(&mut cursor)? {
        id if id == property::HEADER as u64 => Ok(RawHeader::Plain(Box::new(read_archive_header(&mut cursor)?))),
        id if id == property::ENCODED_HEADER as u64 => Ok(RawHeader::Encoded(Box::new(read_streams_info(&mut cursor)?))),
        other => Err(Error::malformed_at(format!("7z header starts with property {other:#x}, not a header or an encoded header"), at)),
    }
}

fn read_archive_header(cursor: &mut Cursor<'_>) -> Result<ArchiveHeader> {
    let mut header = ArchiveHeader::default();
    let mut id = read_id(cursor)?;

    if id == property::ARCHIVE_PROPERTIES as u64 {
        skip_archive_properties(cursor)?;
        id = read_id(cursor)?;
    }
    if id == property::ADDITIONAL_STREAMS_INFO as u64 {
        return Err(Error::Unsupported(Unsupported::Other("a 7z archive with additional streams, which no released 7-Zip writes")));
    }
    if id == property::MAIN_STREAMS_INFO as u64 {
        header.streams = read_streams_info(cursor)?;
        id = read_id(cursor)?;
    }
    if id == property::FILES_INFO as u64 {
        header.files = read_files_info(cursor)?;
        id = read_id(cursor)?;
    }
    if id != property::END as u64 {
        return Err(Error::malformed_at(format!("7z header carries an unexpected property {id:#x}"), cursor.offset()));
    }

    Ok(header)
}

fn skip_archive_properties(cursor: &mut Cursor<'_>) -> Result<()> {
    loop {
        if read_id(cursor)? == property::END as u64 {
            return Ok(());
        }
        skip_property(cursor)?;
    }
}

pub fn read_streams_info(cursor: &mut Cursor<'_>) -> Result<StreamsInfo> {
    let mut pack = PackInfo::default();
    let mut folders = Vec::new();
    let mut substreams = None;

    loop {
        let id = read_id(cursor)?;
        if id == property::END as u64 {
            break;
        } else if id == property::PACK_INFO as u64 {
            pack = read_pack_info(cursor)?;
        } else if id == property::UNPACK_INFO as u64 {
            folders = read_unpack_info(cursor)?;
        } else if id == property::SUB_STREAMS_INFO as u64 {
            substreams = Some(read_sub_streams_info(cursor, &folders)?);
        } else {
            skip_property(cursor)?;
        }
    }

    let substreams = match substreams {
        Some(substreams) => substreams,
        None => implicit_substreams(&folders)?,
    };
    let folder_first_pack = first_pack_streams(&folders, &pack)?;

    Ok(StreamsInfo { pack, folders, substreams, folder_first_pack })
}

fn first_pack_streams(folders: &[Folder], pack: &PackInfo) -> Result<Vec<usize>> {
    let mut first = Vec::with_capacity(folders.len());
    let mut next = 0usize;
    for folder in folders {
        first.push(next);
        next += folder.packed_indices.len();
    }
    if next > pack.sizes.len() {
        return Err(Error::malformed(format!("7z folders need {next} packed streams but the archive declares {}", pack.sizes.len())));
    }
    Ok(first)
}

fn read_pack_info(cursor: &mut Cursor<'_>) -> Result<PackInfo> {
    let pos = number(cursor, "the packed stream base offset")?;
    let streams = count(cursor, "packed streams")?;
    let mut sizes = Vec::new();

    loop {
        let id = read_id(cursor)?;
        if id == property::END as u64 {
            break;
        } else if id == property::SIZE as u64 {
            for _ in 0..streams {
                sizes.push(number(cursor, "a packed stream size")?);
            }
        } else if id == property::CRC as u64 {
            read_digests(cursor, streams, "a packed stream checksum")?;
        } else {
            skip_property(cursor)?;
        }
    }

    if sizes.len() != streams {
        return Err(Error::malformed(format!("7z PackInfo declares {streams} packed streams but lists {} sizes", sizes.len())));
    }
    Ok(PackInfo { pos, sizes })
}

fn read_unpack_info(cursor: &mut Cursor<'_>) -> Result<Vec<Folder>> {
    if read_id(cursor)? != property::FOLDER as u64 {
        return Err(Error::malformed("7z UnpackInfo does not start with its folder list"));
    }

    let num_folders = count(cursor, "folders")?;
    if cursor.u8("the folder list's external flag")? != 0 {
        return Err(Error::Unsupported(Unsupported::Other("a 7z archive holding its folder list in a separate stream, which no released 7-Zip writes")));
    }

    let mut folders = Vec::new();
    for _ in 0..num_folders {
        folders.push(Folder::read(cursor)?);
    }

    if read_id(cursor)? != property::CODERS_UNPACK_SIZE as u64 {
        return Err(Error::malformed("7z UnpackInfo does not list its unpacked sizes"));
    }
    read_unpack_sizes(cursor, &mut folders)?;

    loop {
        let id = read_id(cursor)?;
        if id == property::END as u64 {
            break;
        } else if id == property::CRC as u64 {
            for (folder, crc) in folders.iter_mut().zip(read_digests(cursor, num_folders, "a folder checksum")?) {
                folder.crc = crc;
            }
        } else {
            skip_property(cursor)?;
        }
    }

    Ok(folders)
}

fn read_sub_streams_info(cursor: &mut Cursor<'_>, folders: &[Folder]) -> Result<SubStreams> {
    let mut counts = vec![1usize; folders.len()];
    let mut id = read_id(cursor)?;

    if id == property::NUM_UNPACK_STREAM as u64 {
        for slot in counts.iter_mut() {
            *slot = count(cursor, "a folder's substreams")?;
        }
        id = read_id(cursor)?;
    }

    let mut sizes = Vec::new();
    for (index, folder) in folders.iter().enumerate() {
        if counts[index] == 0 {
            continue;
        }
        let mut sum = 0u64;
        for _ in 1..counts[index] {
            if id != property::SIZE as u64 {
                return Err(Error::malformed("7z SubStreamsInfo splits a folder without listing the substream sizes"));
            }
            let size = number(cursor, "a substream size")?;
            sum = sum.checked_add(size).ok_or_else(|| Error::malformed("7z substream sizes overflow their folder"))?;
            sizes.push(size);
        }
        let total = folder.unpack_size()?;
        sizes.push(total.checked_sub(sum).ok_or_else(|| Error::malformed("7z substream sizes exceed the folder they divide"))?);
    }
    if id == property::SIZE as u64 {
        id = read_id(cursor)?;
    }

    let stored: usize = folders.iter().enumerate().filter(|(i, f)| counts[*i] != 1 || f.crc.is_none()).map(|(i, _)| counts[i]).sum();
    let mut crcs = Vec::new();

    loop {
        if id == property::END as u64 {
            break;
        } else if id == property::CRC as u64 {
            let digests = read_digests(cursor, stored, "a substream checksum")?;
            let mut taken = 0usize;
            for (index, folder) in folders.iter().enumerate() {
                if counts[index] == 1 && folder.crc.is_some() {
                    crcs.push(folder.crc);
                } else {
                    crcs.extend_from_slice(&digests[taken..taken + counts[index]]);
                    taken += counts[index];
                }
            }
        } else {
            skip_property(cursor)?;
        }
        id = read_id(cursor)?;
    }

    if crcs.is_empty() {
        crcs = vec![None; sizes.len()];
    }
    Ok(SubStreams { counts, sizes, crcs })
}

fn implicit_substreams(folders: &[Folder]) -> Result<SubStreams> {
    let mut substreams = SubStreams { counts: vec![1; folders.len()], sizes: Vec::new(), crcs: Vec::new() };
    for folder in folders {
        substreams.sizes.push(folder.unpack_size()?);
        substreams.crcs.push(folder.crc);
    }
    Ok(substreams)
}

fn read_files_info(cursor: &mut Cursor<'_>) -> Result<Vec<FileEntry>> {
    let num_files = count(cursor, "files")?;
    if num_files > cursor.remaining() {
        return Err(Error::malformed_at(format!("7z FilesInfo declares {num_files} files in {} bytes", cursor.remaining()), cursor.offset()));
    }
    let mut files = vec![FileEntry::default(); num_files];

    let mut empty_streams = vec![false; num_files];
    let mut names = None;

    let mut empty_file_body = None;
    let mut anti_body = None;

    loop {
        let id = read_id(cursor)?;
        if id == property::END as u64 {
            break;
        }

        let len = count(cursor, "a file property's bytes")?;
        let at = cursor.offset();
        let body = cursor.slice(len, "a file property")?;
        let mut property = Cursor::new(body, at);

        if id == property::EMPTY_STREAM as u64 {
            empty_streams = bits(&mut property, num_files, "the empty stream vector")?;
        } else if id == property::EMPTY_FILE as u64 {
            empty_file_body = Some((body, at));
        } else if id == property::ANTI as u64 {
            anti_body = Some((body, at));
        } else if id == property::NAME as u64 {
            names = Some(read_names(&mut property, num_files)?);
        } else if id == property::CTIME as u64 || id == property::ATIME as u64 || id == property::MTIME as u64 {
            let times = read_times(&mut property, num_files)?;
            for (file, time) in files.iter_mut().zip(times) {
                match id as u8 {
                    property::CTIME => file.ctime = time,
                    property::ATIME => file.atime = time,
                    _ => file.mtime = time,
                }
            }
        } else if id == property::WIN_ATTRIBUTES as u64 {
            for (file, value) in files.iter_mut().zip(read_attributes(&mut property, num_files)?) {
                file.attributes = value;
            }
        }
    }

    let names = names.ok_or_else(|| Error::malformed("7z FilesInfo carries no names"))?;
    if names.len() != num_files {
        return Err(Error::malformed(format!("7z FilesInfo declares {num_files} files but names {}", names.len())));
    }

    let streamless = empty_streams.iter().filter(|&&empty| empty).count();
    let empty_files = match empty_file_body {
        Some((body, at)) => bits(&mut Cursor::new(body, at), streamless, "the empty file vector")?,
        None => Vec::new(),
    };
    let anti = match anti_body {
        Some((body, at)) => bits(&mut Cursor::new(body, at), streamless, "the anti file vector")?,
        None => Vec::new(),
    };

    let mut empty = 0usize;
    for (index, file) in files.iter_mut().enumerate() {
        file.name = names[index].clone();
        file.has_stream = !empty_streams[index];
        if !file.has_stream {
            file.is_empty_file = empty_files.get(empty).copied().unwrap_or(false);
            file.is_anti = anti.get(empty).copied().unwrap_or(false);
            empty += 1;
        }
    }

    Ok(files)
}

fn read_names(cursor: &mut Cursor<'_>, num_files: usize) -> Result<Vec<String>> {
    if cursor.u8("the name list's external flag")? != 0 {
        return Err(Error::Unsupported(Unsupported::Other("a 7z archive holding its file names in a separate stream, which no released 7-Zip writes")));
    }

    let mut names = Vec::with_capacity(num_files.min(1024));
    let mut units = Vec::new();

    while !cursor.is_empty() {
        let unit = cursor.u16("a name character")?;
        if unit == 0 {
            names.push(char::decode_utf16(units.drain(..)).map(|c| c.unwrap_or(char::REPLACEMENT_CHARACTER)).collect());
        } else {
            units.push(unit);
        }
    }

    if !units.is_empty() {
        return Err(Error::malformed("7z name list ends without terminating its last name"));
    }
    Ok(names)
}

fn read_times(cursor: &mut Cursor<'_>, num_files: usize) -> Result<Vec<Option<u64>>> {
    let defined = defined_bits(cursor, num_files, "a time vector")?;
    if cursor.u8("a time list's external flag")? != 0 {
        return Err(Error::Unsupported(Unsupported::Other("a 7z archive holding its timestamps in a separate stream, which no released 7-Zip writes")));
    }

    let mut times = Vec::with_capacity(num_files.min(1024));
    for &is_defined in &defined {
        times.push(if is_defined { Some(cursor.u64("a timestamp")?) } else { None });
    }
    Ok(times)
}

fn read_attributes(cursor: &mut Cursor<'_>, num_files: usize) -> Result<Vec<Option<u32>>> {
    let defined = defined_bits(cursor, num_files, "the attribute vector")?;
    if cursor.u8("the attribute list's external flag")? != 0 {
        return Err(Error::Unsupported(Unsupported::Other("a 7z archive holding its attributes in a separate stream, which no released 7-Zip writes")));
    }

    let mut attributes = Vec::with_capacity(num_files.min(1024));
    for &is_defined in &defined {
        attributes.push(if is_defined { Some(cursor.u32("an attribute word")?) } else { None });
    }
    Ok(attributes)
}

fn read_digests(cursor: &mut Cursor<'_>, len: usize, what: &str) -> Result<Vec<Option<u32>>> {
    let defined = defined_bits(cursor, len, what)?;
    let mut digests = Vec::with_capacity(len.min(1024));
    for &is_defined in &defined {
        digests.push(if is_defined { Some(cursor.u32(what)?) } else { None });
    }
    Ok(digests)
}

fn read_id(cursor: &mut Cursor<'_>) -> Result<u64> {
    number(cursor, "a property id")
}

fn skip_property(cursor: &mut Cursor<'_>) -> Result<()> {
    let len = count(cursor, "an unrecognised property's bytes")?;
    cursor.skip(len, "an unrecognised property")
}
