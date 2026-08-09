use crate::utils::error::{Error, Result};

pub const BLOCK: usize = 512;

pub const NAME: (usize, usize) = (0, 100);
pub const MODE: (usize, usize) = (100, 8);
pub const UID: (usize, usize) = (108, 8);
pub const GID: (usize, usize) = (116, 8);
pub const SIZE: (usize, usize) = (124, 12);
pub const MTIME: (usize, usize) = (136, 12);
pub const CHKSUM: (usize, usize) = (148, 8);
pub const TYPEFLAG: usize = 156;
pub const LINKNAME: (usize, usize) = (157, 100);
pub const MAGIC: (usize, usize) = (257, 6);
pub const VERSION: (usize, usize) = (263, 2);
pub const UNAME: (usize, usize) = (265, 32);
pub const GNAME: (usize, usize) = (297, 32);
pub const DEVMAJOR: (usize, usize) = (329, 8);
pub const DEVMINOR: (usize, usize) = (337, 8);
pub const PREFIX: (usize, usize) = (345, 155);

pub const USTAR_MAGIC: &[u8; 6] = b"ustar\0";
pub const USTAR_VERSION: &[u8; 2] = b"00";
pub const GNU_MAGIC: &[u8; 6] = b"ustar ";
pub const GNU_VERSION: &[u8; 2] = b" \0";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    V7,
    Ustar,
    Gnu,
    Pax,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Regular,
    HardLink,
    Symlink,
    CharDevice,
    BlockDevice,
    Directory,
    Fifo,
    Contiguous,
    PaxNext,
    PaxGlobal,
    GnuLongName,
    GnuLongLink,
    GnuSparse,
    GnuDumpDir,
    GnuVolume,
    GnuMultiVolume,
    Other(u8),
}

impl Kind {
    pub fn from_byte(byte: u8) -> Self {
        match byte {
            0 | b'0' => Kind::Regular,
            b'1' => Kind::HardLink,
            b'2' => Kind::Symlink,
            b'3' => Kind::CharDevice,
            b'4' => Kind::BlockDevice,
            b'5' => Kind::Directory,
            b'6' => Kind::Fifo,
            b'7' => Kind::Contiguous,
            b'x' | b'X' => Kind::PaxNext,
            b'g' => Kind::PaxGlobal,
            b'L' => Kind::GnuLongName,
            b'K' => Kind::GnuLongLink,
            b'S' => Kind::GnuSparse,
            b'D' => Kind::GnuDumpDir,
            b'V' => Kind::GnuVolume,
            b'M' => Kind::GnuMultiVolume,
            other => Kind::Other(other),
        }
    }

    pub fn to_byte(self) -> u8 {
        match self {
            Kind::Regular => b'0',
            Kind::HardLink => b'1',
            Kind::Symlink => b'2',
            Kind::CharDevice => b'3',
            Kind::BlockDevice => b'4',
            Kind::Directory => b'5',
            Kind::Fifo => b'6',
            Kind::Contiguous => b'7',
            Kind::PaxNext => b'x',
            Kind::PaxGlobal => b'g',
            Kind::GnuLongName => b'L',
            Kind::GnuLongLink => b'K',
            Kind::GnuSparse => b'S',
            Kind::GnuDumpDir => b'D',
            Kind::GnuVolume => b'V',
            Kind::GnuMultiVolume => b'M',
            Kind::Other(byte) => byte,
        }
    }

    pub fn carries_data(self) -> bool {
        !matches!(self, Kind::Directory | Kind::Symlink | Kind::HardLink | Kind::CharDevice | Kind::BlockDevice | Kind::Fifo)
    }

    pub fn is_metadata(self) -> bool {
        matches!(self, Kind::PaxNext | Kind::PaxGlobal | Kind::GnuLongName | Kind::GnuLongLink | Kind::GnuVolume)
    }
}

#[derive(Debug, Clone)]
pub struct Header {
    pub name: Vec<u8>,
    pub mode: u32,
    pub uid: u64,
    pub gid: u64,
    pub size: u64,
    pub mtime: i64,
    pub kind: Kind,
    pub linkname: Vec<u8>,
    pub uname: Vec<u8>,
    pub gname: Vec<u8>,
    pub devmajor: u32,
    pub devminor: u32,
    pub format: Format,
}

impl Default for Header {
    fn default() -> Self {
        Header {
            name: Vec::new(),
            mode: 0o644,
            uid: 0,
            gid: 0,
            size: 0,
            mtime: 0,
            kind: Kind::Regular,
            linkname: Vec::new(),
            uname: Vec::new(),
            gname: Vec::new(),
            devmajor: 0,
            devminor: 0,
            format: Format::Pax,
        }
    }
}

pub fn is_zero_block(block: &[u8]) -> bool {
    block.iter().all(|&b| b == 0)
}

fn field(block: &[u8], (at, len): (usize, usize)) -> &[u8] {
    &block[at..at + len]
}

fn trimmed(bytes: &[u8]) -> &[u8] {
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    let mut slice = &bytes[..end];
    while let [rest @ .., last] = slice {
        if *last == b' ' {
            slice = rest;
        } else {
            break;
        }
    }
    slice
}

pub fn parse_numeric(bytes: &[u8], what: &str) -> Result<u64> {
    if bytes.first().is_some_and(|&b| b & 0x80 != 0) {
        return parse_base256(bytes, what).map(|v| v as u64);
    }

    let text = trimmed(bytes);
    let text: &[u8] = {
        let start = text.iter().position(|&b| b != b' ').unwrap_or(text.len());
        &text[start..]
    };

    if text.is_empty() {
        return Ok(0);
    }

    let mut value = 0u64;
    for &byte in text {
        if !(b'0'..b'8').contains(&byte) {
            return Err(Error::malformed(format!("tar {what} field is not octal")));
        }
        value =
            value.checked_mul(8).and_then(|v| v.checked_add((byte - b'0') as u64)).ok_or_else(|| Error::malformed(format!("tar {what} field overflows")))?;
    }
    Ok(value)
}

pub fn parse_signed(bytes: &[u8], what: &str) -> Result<i64> {
    if bytes.first().is_some_and(|&b| b & 0x80 != 0) {
        return parse_base256(bytes, what);
    }
    parse_numeric(bytes, what).map(|v| v as i64)
}

fn parse_base256(bytes: &[u8], what: &str) -> Result<i64> {
    let negative = bytes[0] & 0x40 != 0;
    let mut value: i64 = if negative { -1 } else { 0 };

    for (index, &byte) in bytes.iter().enumerate() {
        let byte = if index == 0 { byte & 0x3f } else { byte };
        value = value.checked_shl(8).ok_or_else(|| Error::malformed(format!("tar {what} field overflows")))? | byte as i64;
    }
    Ok(value)
}

pub fn checksum(block: &[u8]) -> (u32, i32) {
    let mut unsigned = 0u32;
    let mut signed = 0i32;

    for (index, &byte) in block.iter().enumerate() {
        let byte = if (CHKSUM.0..CHKSUM.0 + CHKSUM.1).contains(&index) { b' ' } else { byte };
        unsigned += byte as u32;
        signed += byte as i8 as i32;
    }
    (unsigned, signed)
}

pub fn parse(block: &[u8]) -> Result<Header> {
    if block.len() < BLOCK {
        return Err(Error::malformed("tar header block is short"));
    }

    let stored = parse_numeric(field(block, CHKSUM), "checksum")?;
    let (unsigned, signed) = checksum(block);
    if stored != unsigned as u64 && stored as i64 != signed as i64 {
        return Err(Error::malformed(format!("tar header checksum mismatch: stored {stored}, computed {unsigned}")));
    }

    let magic = field(block, MAGIC);
    let version = field(block, VERSION);
    let format = if magic == USTAR_MAGIC && version == USTAR_VERSION {
        Format::Ustar
    } else if magic == GNU_MAGIC {
        Format::Gnu
    } else if magic.starts_with(b"ustar") {
        Format::Ustar
    } else {
        Format::V7
    };

    let kind = Kind::from_byte(block[TYPEFLAG]);

    let mut name = trimmed(field(block, NAME)).to_vec();
    if format == Format::Ustar {
        let prefix = trimmed(field(block, PREFIX));
        if !prefix.is_empty() {
            let mut joined = prefix.to_vec();
            joined.push(b'/');
            joined.extend_from_slice(&name);
            name = joined;
        }
    }

    Ok(Header {
        name,
        mode: parse_numeric(field(block, MODE), "mode")? as u32,
        uid: parse_numeric(field(block, UID), "uid")?,
        gid: parse_numeric(field(block, GID), "gid")?,
        size: parse_numeric(field(block, SIZE), "size")?,
        mtime: parse_signed(field(block, MTIME), "mtime")?,
        kind,
        linkname: trimmed(field(block, LINKNAME)).to_vec(),
        uname: trimmed(field(block, UNAME)).to_vec(),
        gname: trimmed(field(block, GNAME)).to_vec(),
        devmajor: parse_numeric(field(block, DEVMAJOR), "devmajor").unwrap_or(0) as u32,
        devminor: parse_numeric(field(block, DEVMINOR), "devminor").unwrap_or(0) as u32,
        format,
    })
}

fn put(block: &mut [u8], (at, len): (usize, usize), value: &[u8]) {
    let n = value.len().min(len);
    block[at..at + n].copy_from_slice(&value[..n]);
}

pub fn put_octal(block: &mut [u8], span: (usize, usize), value: u64) {
    let (at, len) = span;
    if value >= 1 << (3 * (len - 1)) {
        put_base256(&mut block[at..at + len], value as i64);
        return;
    }
    let text = format!("{:0width$o}\0", value, width = len - 1);
    put(block, span, text.as_bytes());
}

pub fn put_signed(block: &mut [u8], span: (usize, usize), value: i64) {
    let (at, len) = span;
    if value < 0 || value >= 1 << (3 * (len - 1)) {
        put_base256(&mut block[at..at + len], value);
        return;
    }
    put_octal(block, span, value as u64);
}

fn put_base256(slot: &mut [u8], value: i64) {
    let mut value = value;
    for byte in slot.iter_mut().rev() {
        *byte = value as u8;
        value >>= 8;
    }
    slot[0] |= 0x80;
    if value < 0 {
        slot[0] |= 0x40;
    }
}

pub fn split_ustar_name(name: &[u8]) -> Option<(&[u8], &[u8])> {
    if name.len() <= NAME.1 {
        return Some((&[], name));
    }
    if name.len() > PREFIX.1 + 1 + NAME.1 {
        return None;
    }

    let split = name[..name.len().min(PREFIX.1 + 1)].iter().rposition(|&b| b == b'/')?;
    let (prefix, rest) = (&name[..split], &name[split + 1..]);
    if rest.len() > NAME.1 || prefix.len() > PREFIX.1 || rest.is_empty() {
        return None;
    }
    Some((prefix, rest))
}

pub fn write(header: &Header) -> [u8; BLOCK] {
    let mut block = [0u8; BLOCK];

    let (prefix, name) = match header.format {
        Format::Ustar | Format::Pax => split_ustar_name(&header.name).unwrap_or((&[], &header.name)),
        _ => (&[][..], &header.name[..]),
    };

    put(&mut block, NAME, name);
    put(&mut block, PREFIX, prefix);

    put_octal(&mut block, MODE, header.mode as u64);
    put_octal(&mut block, UID, header.uid);
    put_octal(&mut block, GID, header.gid);
    put_octal(&mut block, SIZE, header.size);
    put_signed(&mut block, MTIME, header.mtime);

    block[TYPEFLAG] = header.kind.to_byte();
    put(&mut block, LINKNAME, &header.linkname);

    match header.format {
        Format::Gnu => {
            put(&mut block, MAGIC, GNU_MAGIC);
            put(&mut block, VERSION, GNU_VERSION);
        }
        Format::V7 => {}
        _ => {
            put(&mut block, MAGIC, USTAR_MAGIC);
            put(&mut block, VERSION, USTAR_VERSION);
        }
    }

    put(&mut block, UNAME, &header.uname);
    put(&mut block, GNAME, &header.gname);
    put_octal(&mut block, DEVMAJOR, header.devmajor as u64);
    put_octal(&mut block, DEVMINOR, header.devminor as u64);

    let (unsigned, _) = checksum(&block);
    let text = format!("{unsigned:06o}\0 ");
    put(&mut block, CHKSUM, text.as_bytes());

    block
}

pub fn padding(size: u64) -> usize {
    let remainder = (size % BLOCK as u64) as usize;
    if remainder == 0 { 0 } else { BLOCK - remainder }
}
