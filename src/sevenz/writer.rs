use std::io::{Read, Seek, SeekFrom, Write};

use crate::codecs::Level;
use crate::codecs::bcj;
use crate::codecs::lzma::Properties;
use crate::codecs::lzma::encode::dictionary_at;
use crate::codecs::lzma::lzma2::{self, dictionary_code};
use crate::crypto::Password;
use crate::crypto::sevenz_aes;
use crate::sevenz::number::{put_bits, put_defined_bits, put_number};
use crate::sevenz::spec;
use crate::utils::crc32::Crc32;
use crate::utils::datetime::filetime_from_unix;
use crate::utils::error::{Error, Result, Unsupported};
use crate::utils::io::COPY_BUF;

pub const DEFAULT_BLOCK_SIZE: u64 = 64 * 1024 * 1024;

const MIN_COMPRESSED_HEADER: usize = 128;

const DEFAULT_CYCLES: u8 = 19;

const AES_ID: &[u8] = &[0x06, 0xF1, 0x07, 0x01];

#[derive(Debug, Clone)]
pub struct Item {
    pub name: String,
    pub mtime: Option<i64>,
    pub attributes: Option<u32>,
}

#[derive(Debug, Clone)]
pub struct Options {
    pub level: Level,

    pub packing: Packing,

    pub filter: Option<Filter>,

    pub block_size: Option<u64>,
    pub compress_header: bool,

    pub password: Option<Password>,

    pub encrypt_header: bool,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            level: Level::default(),
            packing: Packing::default(),
            filter: None,
            block_size: Some(DEFAULT_BLOCK_SIZE),
            compress_header: true,
            password: None,
            encrypt_header: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Packing {
    Copy,
    #[default]
    Lzma2,
    Deflate,
    Bzip2,
}

impl Packing {
    fn id(self) -> &'static [u8] {
        match self {
            Packing::Copy => &[0x00],
            Packing::Lzma2 => &[0x21],
            Packing::Deflate => &[0x04, 0x01, 0x08],
            Packing::Bzip2 => &[0x04, 0x02, 0x02],
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Filter {
    Delta(u16),
    Branch(spec::Branch),
}

impl Filter {
    fn id(self) -> &'static [u8] {
        match self {
            Filter::Delta(_) => &[0x03],
            Filter::Branch(spec::Branch::X86) => &[0x03, 0x03, 0x01, 0x03],
            Filter::Branch(spec::Branch::Ppc) => &[0x03, 0x03, 0x02, 0x05],
            Filter::Branch(spec::Branch::Ia64) => &[0x03, 0x03, 0x04, 0x01],
            Filter::Branch(spec::Branch::Arm) => &[0x03, 0x03, 0x05, 0x01],
            Filter::Branch(spec::Branch::ArmThumb) => &[0x03, 0x03, 0x07, 0x01],
            Filter::Branch(spec::Branch::Sparc) => &[0x03, 0x03, 0x08, 0x05],
            Filter::Branch(spec::Branch::Arm64) => &[0x0A],
            Filter::Branch(spec::Branch::RiscV) => &[0x0B],
        }
    }

    fn properties(self) -> Vec<u8> {
        match self {
            Filter::Delta(distance) => vec![(distance.saturating_sub(1)) as u8],
            Filter::Branch(_) => Vec::new(),
        }
    }

    fn is_valid(self) -> bool {
        match self {
            Filter::Delta(distance) => (1..=256).contains(&distance),
            Filter::Branch(_) => true,
        }
    }

    fn apply(self, data: &mut [u8]) -> Result<()> {
        match self {
            Filter::Delta(_) => bcj::encode(bcj::DELTA, &self.properties(), data),
            Filter::Branch(branch) => bcj::encode(branch.xz_filter_id(), &[], data),
        }
    }
}

struct CoderSpec {
    id: &'static [u8],
    properties: Vec<u8>,
}

enum Packer {
    Lzma2(Box<lzma2::Writer<Vec<u8>>>),
    Held { held: Vec<u8>, packing: Packing, level: Level, filter: Option<Filter>, hint: usize },
}

impl Packer {
    fn push(&mut self, bytes: &[u8]) -> Result<()> {
        match self {
            Packer::Lzma2(writer) => writer.push(bytes),
            Packer::Held { held, .. } => {
                held.extend_from_slice(bytes);
                Ok(())
            }
        }
    }

    fn finish(self) -> Result<Vec<u8>> {
        match self {
            Packer::Lzma2(writer) => writer.finish(),
            Packer::Held { mut held, packing, level, filter, hint } => {
                if let Some(filter) = filter {
                    filter.apply(&mut held)?;
                }

                match packing {
                    Packing::Deflate => Ok(crate::codecs::deflate::compress(&held, level)),
                    Packing::Bzip2 => crate::codecs::bzip2::compress(&held, level.bzip2_block_size()),
                    Packing::Lzma2 => {
                        let props = Properties { lc: 3, lp: 0, pb: 2, dict_size: dictionary_at(hint, level) };
                        lzma2::compress(&held, props, level.search_depth())
                    }
                    Packing::Copy => Ok(held),
                }
            }
        }
    }
}

struct FolderRecord {
    coders: Vec<CoderSpec>,
    sizes: Vec<u64>,
    packed_size: u64,
    substreams: Vec<(u64, u32)>,
}

struct FileRecord {
    name: String,
    has_stream: bool,
    is_empty_file: bool,
    is_anti: bool,
    mtime: Option<i64>,
    attributes: Option<u32>,
}

struct OpenFolder {
    writer: Packer,
    coders: Vec<CoderSpec>,
    unpack_size: u64,
    substreams: Vec<(u64, u32)>,
}

pub struct SevenZWriter<W: Write + Seek> {
    out: W,
    options: Options,
    position: u64,
    folders: Vec<FolderRecord>,
    files: Vec<FileRecord>,
    open: Option<OpenFolder>,
}

impl<W: Write + Seek> SevenZWriter<W> {
    pub fn new(mut out: W, options: Options) -> Result<Self> {
        if options.encrypt_header && options.password.is_none() {
            return Err(Error::Unsupported(Unsupported::Other("encrypting a 7z header without a password")));
        }
        if options.filter.is_some_and(|filter| !filter.is_valid()) {
            return Err(Error::Unsupported(Unsupported::Other("a delta filter whose distance is not between 1 and 256, which is all the coder can record")));
        }

        out.write_all(&[0u8; spec::SIGNATURE_HEADER_LEN])?;
        Ok(SevenZWriter { out, options, position: 0, folders: Vec::new(), files: Vec::new(), open: None })
    }

    pub fn add_directory(&mut self, item: &Item) {
        self.files.push(FileRecord {
            name: item.name.clone(),
            has_stream: false,
            is_empty_file: false,
            is_anti: false,
            mtime: item.mtime,
            attributes: item.attributes,
        });
    }

    pub fn add_file(&mut self, item: &Item, data: &[u8]) -> Result<()> {
        if data.is_empty() {
            self.record(item, false, true);
            return Ok(());
        }

        let folder = self.begin(data.len() as u64)?;
        folder.writer.push(data)?;
        folder.unpack_size += data.len() as u64;
        folder.substreams.push((data.len() as u64, crate::utils::crc32::checksum(data)));

        self.record(item, true, false);
        Ok(())
    }

    pub fn add_stream<R: Read>(&mut self, item: &Item, source: &mut R, size: u64) -> Result<()> {
        if size == 0 {
            self.record(item, false, true);
            return Ok(());
        }

        let folder = self.begin(size)?;
        let mut buffer = vec![0u8; COPY_BUF];
        let mut crc = Crc32::new();
        let mut done = 0u64;

        while done < size {
            let want = (size - done).min(buffer.len() as u64) as usize;
            let got = source.read(&mut buffer[..want])?;
            if got == 0 {
                break;
            }
            folder.writer.push(&buffer[..got])?;
            crc.update(&buffer[..got]);
            done += got as u64;
        }

        if done == 0 {
            self.record(item, false, true);
            return Ok(());
        }

        folder.unpack_size += done;
        folder.substreams.push((done, crc.finish()));
        self.record(item, true, false);
        Ok(())
    }

    pub fn add_anti(&mut self, item: &Item) {
        self.files.push(FileRecord {
            name: item.name.clone(),
            has_stream: false,
            is_empty_file: true,
            is_anti: true,
            mtime: item.mtime,
            attributes: item.attributes,
        });
    }

    fn record(&mut self, item: &Item, has_stream: bool, is_empty_file: bool) {
        self.files.push(FileRecord { name: item.name.clone(), has_stream, is_empty_file, is_anti: false, mtime: item.mtime, attributes: item.attributes });
    }

    fn begin(&mut self, size: u64) -> Result<&mut OpenFolder> {
        let block = self.options.block_size.unwrap_or(0);
        if self.open.as_ref().is_some_and(|open| open.unpack_size >= block) {
            self.close_folder()?;
        }

        if self.open.is_none() {
            let (writer, coders) = self.packer(self.folder_hint(size));
            self.open = Some(OpenFolder { writer, coders, unpack_size: 0, substreams: Vec::new() });
        }

        self.open.as_mut().ok_or_else(|| Error::malformed("internal: no folder to write into").into())
    }

    fn packer(&self, hint: usize) -> (Packer, Vec<CoderSpec>) {
        let packing = if self.options.level == Level::None { Packing::Copy } else { self.options.packing };
        let filter = self.options.filter;

        let (writer, packer_coder) = match packing {
            Packing::Lzma2 if filter.is_none() => {
                let dictionary = dictionary_at(hint, self.options.level);
                let props = Properties { lc: 3, lp: 0, pb: 2, dict_size: dictionary };
                let writer = lzma2::Writer::new(Vec::new(), props, self.options.level.search_depth(), hint);
                (Packer::Lzma2(Box::new(writer)), CoderSpec { id: packing.id(), properties: vec![dictionary_code(dictionary)] })
            }
            Packing::Lzma2 => {
                let properties = vec![dictionary_code(dictionary_at(hint, self.options.level))];
                (Packer::Held { held: Vec::new(), packing, level: self.options.level, filter, hint }, CoderSpec { id: packing.id(), properties })
            }
            _ => (Packer::Held { held: Vec::new(), packing, level: self.options.level, filter, hint }, CoderSpec { id: packing.id(), properties: Vec::new() }),
        };

        let mut coders = vec![packer_coder];
        if let Some(filter) = filter {
            coders.push(CoderSpec { id: filter.id(), properties: filter.properties() });
        }
        (writer, coders)
    }

    fn folder_hint(&self, first: u64) -> usize {
        let ceiling = usize::MAX as u64;
        first.max(self.options.block_size.unwrap_or(0)).min(ceiling) as usize
    }

    fn close_folder(&mut self) -> Result<()> {
        let Some(open) = self.open.take() else { return Ok(()) };
        if open.substreams.is_empty() {
            return Ok(());
        }

        let compressed = open.writer.finish()?;
        let (stored, aes) = self.seal(compressed)?;

        self.out.write_all(&stored)?;
        self.position += stored.len() as u64;

        let mut coders = open.coders;
        let mut sizes = vec![open.unpack_size; coders.len()];
        if let Some((properties, plain)) = aes {
            coders.insert(0, CoderSpec { id: AES_ID, properties });
            sizes.insert(0, plain);
        }

        self.folders.push(FolderRecord { coders, sizes, packed_size: stored.len() as u64, substreams: open.substreams });
        Ok(())
    }

    fn seal(&self, compressed: Vec<u8>) -> Result<(Vec<u8>, Option<(Vec<u8>, u64)>)> {
        let Some(password) = &self.options.password else { return Ok((compressed, None)) };

        let properties = sevenz_aes::Properties::generate(DEFAULT_CYCLES);
        let key = properties.key(password);
        let plain_len = compressed.len() as u64;
        let sealed = sevenz_aes::cbc_encrypt(&compressed, &key, properties.iv)?;

        Ok((sealed, Some((properties.to_bytes(), plain_len))))
    }

    pub fn finish(mut self) -> Result<W> {
        self.close_folder()?;

        let mut header = self.header();
        let wants_encoding = self.options.encrypt_header || (self.options.compress_header && header.len() > MIN_COMPRESSED_HEADER);
        if wants_encoding && !header.is_empty() {
            header = self.encode_header(&header)?;
        }

        let header_offset = self.position;
        self.out.write_all(&header)?;

        let mut signature = [0u8; spec::SIGNATURE_HEADER_LEN];
        signature[..spec::SIGNATURE.len()].copy_from_slice(&spec::SIGNATURE);
        signature[6] = 0;
        signature[7] = 4;
        signature[12..20].copy_from_slice(&header_offset.to_le_bytes());
        signature[20..28].copy_from_slice(&(header.len() as u64).to_le_bytes());
        signature[28..32].copy_from_slice(&crate::utils::crc32::checksum(&header).to_le_bytes());

        let start = crate::utils::crc32::checksum(&signature[12..32]);
        signature[8..12].copy_from_slice(&start.to_le_bytes());

        self.out.seek(SeekFrom::Start(0))?;
        self.out.write_all(&signature)?;
        self.out.flush()?;
        Ok(self.out)
    }

    fn encode_header(&mut self, plain: &[u8]) -> Result<Vec<u8>> {
        let dictionary = dictionary_at(plain.len(), self.options.level);
        let props = Properties { lc: 3, lp: 0, pb: 2, dict_size: dictionary };
        let compressed = lzma2::compress(plain, props, self.options.level.search_depth())?;
        let (packed, sealed) = if self.options.encrypt_header { self.seal(compressed)? } else { (compressed, None) };

        let mut coders = vec![CoderSpec { id: Packing::Lzma2.id(), properties: vec![dictionary_code(dictionary)] }];
        let mut sizes = vec![plain.len() as u64];
        if let Some((properties, compressed_len)) = &sealed {
            coders.insert(0, CoderSpec { id: AES_ID, properties: properties.clone() });
            sizes.insert(0, *compressed_len);
        }

        let pack_pos = self.position;
        self.out.write_all(&packed)?;
        self.position += packed.len() as u64;

        let mut out = Vec::with_capacity(64);
        out.push(spec::property::ENCODED_HEADER);

        out.push(spec::property::PACK_INFO);
        put_number(&mut out, pack_pos);
        put_number(&mut out, 1);
        out.push(spec::property::SIZE);
        put_number(&mut out, packed.len() as u64);
        out.push(spec::property::END);

        out.push(spec::property::UNPACK_INFO);
        out.push(spec::property::FOLDER);
        put_number(&mut out, 1);
        out.push(0);
        put_folder(&mut out, &coders);
        out.push(spec::property::CODERS_UNPACK_SIZE);
        for size in &sizes {
            put_number(&mut out, *size);
        }
        out.push(spec::property::CRC);
        out.push(1);
        out.extend_from_slice(&crate::utils::crc32::checksum(plain).to_le_bytes());
        out.push(spec::property::END);

        out.push(spec::property::END);
        Ok(out)
    }

    fn header(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(256 + self.files.len() * 32);
        out.push(spec::property::HEADER);

        if !self.folders.is_empty() {
            out.push(spec::property::MAIN_STREAMS_INFO);
            self.put_streams_info(&mut out);
        }

        if !self.files.is_empty() {
            out.push(spec::property::FILES_INFO);
            self.put_files_info(&mut out);
        }

        out.push(spec::property::END);
        out
    }

    fn put_streams_info(&self, out: &mut Vec<u8>) {
        out.push(spec::property::PACK_INFO);
        put_number(out, 0);
        put_number(out, self.folders.len() as u64);
        out.push(spec::property::SIZE);
        for folder in &self.folders {
            put_number(out, folder.packed_size);
        }
        out.push(spec::property::END);

        out.push(spec::property::UNPACK_INFO);
        out.push(spec::property::FOLDER);
        put_number(out, self.folders.len() as u64);
        out.push(0);
        for folder in &self.folders {
            put_folder(out, &folder.coders);
        }

        out.push(spec::property::CODERS_UNPACK_SIZE);
        for folder in &self.folders {
            for size in &folder.sizes {
                put_number(out, *size);
            }
        }
        out.push(spec::property::END);

        out.push(spec::property::SUB_STREAMS_INFO);
        out.push(spec::property::NUM_UNPACK_STREAM);
        for folder in &self.folders {
            put_number(out, folder.substreams.len() as u64);
        }

        out.push(spec::property::SIZE);
        for folder in &self.folders {
            for (size, _) in folder.substreams.iter().take(folder.substreams.len() - 1) {
                put_number(out, *size);
            }
        }

        out.push(spec::property::CRC);
        out.push(1);
        for folder in &self.folders {
            for (_, crc) in &folder.substreams {
                out.extend_from_slice(&crc.to_le_bytes());
            }
        }
        out.push(spec::property::END);

        out.push(spec::property::END);
    }

    fn put_files_info(&self, out: &mut Vec<u8>) {
        put_number(out, self.files.len() as u64);

        let streamless: Vec<&FileRecord> = self.files.iter().filter(|f| !f.has_stream).collect();
        if !streamless.is_empty() {
            let bits: Vec<bool> = self.files.iter().map(|f| !f.has_stream).collect();
            let mut body = Vec::new();
            put_bits(&mut body, &bits);
            put_property(out, spec::property::EMPTY_STREAM, &body);

            if streamless.iter().any(|f| f.is_empty_file) {
                let bits: Vec<bool> = streamless.iter().map(|f| f.is_empty_file).collect();
                let mut body = Vec::new();
                put_bits(&mut body, &bits);
                put_property(out, spec::property::EMPTY_FILE, &body);
            }

            if streamless.iter().any(|f| f.is_anti) {
                let bits: Vec<bool> = streamless.iter().map(|f| f.is_anti).collect();
                let mut body = Vec::new();
                put_bits(&mut body, &bits);
                put_property(out, spec::property::ANTI, &body);
            }
        }

        let mut body = vec![0u8];
        for file in &self.files {
            for unit in file.name.encode_utf16() {
                body.extend_from_slice(&unit.to_le_bytes());
            }
            body.extend_from_slice(&[0, 0]);
        }
        put_property(out, spec::property::NAME, &body);

        if self.files.iter().any(|f| f.mtime.is_some()) {
            let defined: Vec<bool> = self.files.iter().map(|f| f.mtime.is_some()).collect();
            let mut body = Vec::new();
            put_defined_bits(&mut body, &defined);
            body.push(0);
            for file in self.files.iter().filter(|f| f.mtime.is_some()) {
                body.extend_from_slice(&filetime_from_unix(file.mtime.expect("a defined time")).to_le_bytes());
            }
            put_property(out, spec::property::MTIME, &body);
        }

        if self.files.iter().any(|f| f.attributes.is_some()) {
            let defined: Vec<bool> = self.files.iter().map(|f| f.attributes.is_some()).collect();
            let mut body = Vec::new();
            put_defined_bits(&mut body, &defined);
            body.push(0);
            for file in self.files.iter().filter(|f| f.attributes.is_some()) {
                body.extend_from_slice(&file.attributes.expect("a defined attribute word").to_le_bytes());
            }
            put_property(out, spec::property::WIN_ATTRIBUTES, &body);
        }

        out.push(spec::property::END);
    }
}

fn put_property(out: &mut Vec<u8>, id: u8, body: &[u8]) {
    out.push(id);
    put_number(out, body.len() as u64);
    out.extend_from_slice(body);
}

fn put_folder(out: &mut Vec<u8>, coders: &[CoderSpec]) {
    put_number(out, coders.len() as u64);
    for coder in coders {
        put_coder(out, coder.id, &coder.properties);
    }

    for index in 1..coders.len() as u64 {
        put_number(out, index);
        put_number(out, index - 1);
    }
}

fn put_coder(out: &mut Vec<u8>, id: &[u8], properties: &[u8]) {
    let attributes = if properties.is_empty() { 0 } else { spec::coder_flags::HAS_ATTRIBUTES };
    out.push(id.len() as u8 | attributes);
    out.extend_from_slice(id);

    if !properties.is_empty() {
        put_number(out, properties.len() as u64);
        out.extend_from_slice(properties);
    }
}
