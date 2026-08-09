use crate::crypto::winzip_aes::AesExtra;
use crate::utils::bytes::{Cursor, put_u16, put_u32, put_u64};
use crate::utils::datetime;
use crate::zip::spec::{U32_MAX, extra_id};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExtraFields {
    pub uncompressed_size: Option<u64>,
    pub compressed_size: Option<u64>,
    pub local_header_offset: Option<u64>,
    pub disk_start: Option<u32>,

    pub mtime: Option<i64>,
    pub atime: Option<i64>,
    pub ctime: Option<i64>,

    pub uid: Option<u32>,
    pub gid: Option<u32>,

    pub aes: Option<AesExtra>,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct Zip64Need {
    pub uncompressed_size: bool,
    pub compressed_size: bool,
    pub local_header_offset: bool,
    pub disk_start: bool,
}

impl Zip64Need {
    pub fn from_central(uncompressed: u32, compressed: u32, offset: u32, disk: u16) -> Self {
        Zip64Need {
            uncompressed_size: uncompressed == U32_MAX,
            compressed_size: compressed == U32_MAX,
            local_header_offset: offset == U32_MAX,
            disk_start: disk == crate::zip::spec::U16_MAX,
        }
    }

    pub fn from_local(uncompressed: u32, compressed: u32) -> Self {
        let saturated = uncompressed == U32_MAX || compressed == U32_MAX;
        Zip64Need { uncompressed_size: saturated, compressed_size: saturated, local_header_offset: false, disk_start: false }
    }

    pub fn encoded_len(&self) -> usize {
        8 * (self.uncompressed_size as usize + self.compressed_size as usize + self.local_header_offset as usize) + 4 * self.disk_start as usize
    }

    pub fn is_empty(&self) -> bool {
        !(self.uncompressed_size || self.compressed_size || self.local_header_offset || self.disk_start)
    }
}

pub fn parse(data: &[u8], need: Zip64Need, base: u64) -> ExtraFields {
    let mut out = ExtraFields::default();
    let mut cursor = Cursor::new(data, base);

    while cursor.remaining() >= 4 {
        let Ok(id) = cursor.u16("extra header id") else { break };
        let Ok(size) = cursor.u16("extra data size") else { break };
        let Ok(block) = cursor.slice(size as usize, "extra data") else { break };

        match id {
            extra_id::ZIP64 => parse_zip64(block, need, &mut out),
            extra_id::NTFS => parse_ntfs(block, &mut out),
            extra_id::EXTENDED_TIMESTAMP => parse_extended_timestamp(block, &mut out),
            extra_id::INFOZIP_UNIX2 => parse_infozip_unix2(block, &mut out),
            extra_id::INFOZIP_UNIX1 => parse_infozip_unix1(block, &mut out),
            extra_id::AES => out.aes = AesExtra::parse(block).ok(),
            _ => {}
        }
    }

    out
}

fn parse_zip64(block: &[u8], need: Zip64Need, out: &mut ExtraFields) {
    let mut c = Cursor::new(block, 0);

    if need.uncompressed_size && c.remaining() >= 8 {
        out.uncompressed_size = c.u64("zip64 uncompressed size").ok();
    }
    if need.compressed_size && c.remaining() >= 8 {
        out.compressed_size = c.u64("zip64 compressed size").ok();
    }
    if need.local_header_offset && c.remaining() >= 8 {
        out.local_header_offset = c.u64("zip64 local header offset").ok();
    }
    if need.disk_start && c.remaining() >= 4 {
        out.disk_start = c.u32("zip64 disk start").ok();
    }
}

fn parse_ntfs(block: &[u8], out: &mut ExtraFields) {
    let mut c = Cursor::new(block, 0);
    if c.skip(4, "ntfs reserved").is_err() {
        return;
    }

    while c.remaining() >= 4 {
        let Ok(tag) = c.u16("ntfs tag") else { return };
        let Ok(size) = c.u16("ntfs size") else { return };
        let Ok(data) = c.slice(size as usize, "ntfs data") else { return };

        if tag == 0x0001 && data.len() >= 24 {
            let mut t = Cursor::new(data, 0);
            if let (Ok(m), Ok(a), Ok(cr)) = (t.u64("mtime"), t.u64("atime"), t.u64("ctime")) {
                out.mtime = Some(datetime::unix_from_filetime(m));
                out.atime = Some(datetime::unix_from_filetime(a));
                out.ctime = Some(datetime::unix_from_filetime(cr));
            }
        }
    }
}

fn parse_extended_timestamp(block: &[u8], out: &mut ExtraFields) {
    let mut c = Cursor::new(block, 0);
    let Ok(flags) = c.u8("timestamp flags") else { return };

    if flags & 0x01 != 0 && c.remaining() >= 4 {
        out.mtime = c.u32("mtime").ok().map(|v| v as i32 as i64);
    }
    if flags & 0x02 != 0 && c.remaining() >= 4 {
        out.atime = c.u32("atime").ok().map(|v| v as i32 as i64);
    }
    if flags & 0x04 != 0 && c.remaining() >= 4 {
        out.ctime = c.u32("ctime").ok().map(|v| v as i32 as i64);
    }
}

fn parse_infozip_unix2(block: &[u8], out: &mut ExtraFields) {
    let mut c = Cursor::new(block, 0);
    let Ok(version) = c.u8("unix2 version") else { return };
    if version != 1 {
        return;
    }

    for slot in [0, 1] {
        let Ok(size) = c.u8("id size") else { return };
        let Ok(bytes) = c.slice(size as usize, "id value") else { return };
        let mut value: u64 = 0;
        for (i, &b) in bytes.iter().enumerate().take(8) {
            value |= (b as u64) << (8 * i);
        }
        let value = value as u32;
        if slot == 0 {
            out.uid = Some(value);
        } else {
            out.gid = Some(value);
        }
    }
}

fn parse_infozip_unix1(block: &[u8], out: &mut ExtraFields) {
    let mut c = Cursor::new(block, 0);
    if let Ok(atime) = c.u32("atime") {
        out.atime.get_or_insert(atime as i32 as i64);
    }
    if let Ok(mtime) = c.u32("mtime") {
        out.mtime.get_or_insert(mtime as i32 as i64);
    }
    if c.remaining() >= 4 {
        out.uid = c.u16("uid").ok().map(u32::from);
        out.gid = c.u16("gid").ok().map(u32::from);
    }
}

pub fn write_zip64(out: &mut Vec<u8>, need: Zip64Need, uncompressed: u64, compressed: u64, offset: u64, disk: u32) {
    if need.is_empty() {
        return;
    }

    put_u16(out, extra_id::ZIP64);
    put_u16(out, need.encoded_len() as u16);
    if need.uncompressed_size {
        put_u64(out, uncompressed);
    }
    if need.compressed_size {
        put_u64(out, compressed);
    }
    if need.local_header_offset {
        put_u64(out, offset);
    }
    if need.disk_start {
        put_u32(out, disk);
    }
}

pub fn write_extended_timestamp(out: &mut Vec<u8>, mtime: i64) {
    let flags = 0x01u8;

    put_u16(out, extra_id::EXTENDED_TIMESTAMP);
    put_u16(out, 5);
    out.push(flags);
    put_u32(out, mtime.clamp(0, u32::MAX as i64) as u32);
}

pub fn write_aes(out: &mut Vec<u8>, aes: &AesExtra) {
    put_u16(out, extra_id::AES);
    put_u16(out, 7);
    out.extend_from_slice(&aes.encode());
}

pub fn write_infozip_unix2(out: &mut Vec<u8>, uid: u32, gid: u32) {
    put_u16(out, extra_id::INFOZIP_UNIX2);
    put_u16(out, 11);
    out.push(1);
    out.push(4);
    put_u32(out, uid);
    out.push(4);
    put_u32(out, gid);
}
