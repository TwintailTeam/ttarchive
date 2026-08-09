use std::io::{Read, Seek, SeekFrom};

use crate::utils::bytes::Cursor;
use crate::utils::error::{Error, Result};
use crate::zip::parsers::extra::{self, Zip64Need};
use crate::zip::spec::{DATA_DESCRIPTOR_SIG, LOCAL_HEADER_LEN, LOCAL_HEADER_SIG};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LocalHeader {
    pub data_offset: u64,
    pub method_code: u16,
    pub flags: u16,
    pub compressed_size: u64,
    pub uncompressed_size: u64,
    pub crc32: u32,
    pub mod_time: u16,
}

pub fn read_at<R: Read + Seek>(reader: &mut R, offset: u64) -> Result<LocalHeader> {
    let mut fixed = [0u8; LOCAL_HEADER_LEN];
    reader.seek(SeekFrom::Start(offset))?;
    reader.read_exact(&mut fixed).map_err(|e| {
        if e.kind() == std::io::ErrorKind::UnexpectedEof { Error::malformed_at("local header runs past end of file", offset) } else { Error::Io(e) }
    })?;

    let mut c = Cursor::new(&fixed, offset);
    let signature = c.u32("local header signature")?;
    if signature != LOCAL_HEADER_SIG {
        return Err(Error::malformed_at(
            format!(
                "expected local file header signature 0x04034b50, found {signature:#010x}; \
                 the central directory offset for this entry is wrong"
            ),
            offset,
        ));
    }

    let _version_needed = c.u16("version needed to extract")?;
    let flags = c.u16("general purpose bit flag")?;
    let method_code = c.u16("compression method")?;
    let mod_time = c.u16("last mod file time")?;
    let _date = c.u16("last mod file date")?;
    let crc32 = c.u32("crc-32")?;
    let compressed_size = c.u32("compressed size")?;
    let uncompressed_size = c.u32("uncompressed size")?;
    let name_len = c.u16("file name length")?;
    let extra_len = c.u16("extra field length")?;

    let (compressed_size, uncompressed_size) = if extra_len > 0 {
        let mut extra_buf = vec![0u8; extra_len as usize];
        reader.seek(SeekFrom::Start(offset + LOCAL_HEADER_LEN as u64 + name_len as u64))?;
        reader.read_exact(&mut extra_buf)?;

        let need = Zip64Need::from_local(uncompressed_size, compressed_size);
        let fields = extra::parse(&extra_buf, need, offset);
        (fields.compressed_size.unwrap_or(compressed_size as u64), fields.uncompressed_size.unwrap_or(uncompressed_size as u64))
    } else {
        (compressed_size as u64, uncompressed_size as u64)
    };

    let data_offset = offset + LOCAL_HEADER_LEN as u64 + name_len as u64 + extra_len as u64;

    Ok(LocalHeader { data_offset, method_code, flags, compressed_size, uncompressed_size, crc32, mod_time })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DataDescriptor {
    pub crc32: u32,
    pub compressed_size: u64,
    pub uncompressed_size: u64,
}

pub fn read_data_descriptor<R: Read>(reader: &mut R, zip64: bool) -> Result<DataDescriptor> {
    let size_width = if zip64 { 8 } else { 4 };
    let mut buf = vec![0u8; 4 + 2 * size_width];
    reader.read_exact(&mut buf)?;

    let mut c = Cursor::new(&buf, 0);
    let first = c.u32("data descriptor field")?;

    if first == DATA_DESCRIPTOR_SIG {
        let mut rest = vec![0u8; 4];
        reader.read_exact(&mut rest)?;
        let mut all = buf[4..].to_vec();
        all.extend_from_slice(&rest);

        let mut c = Cursor::new(&all, 0);
        let crc32 = c.u32("crc-32")?;
        let (compressed, uncompressed) = read_sizes(&mut c, zip64)?;
        return Ok(DataDescriptor { crc32, compressed_size: compressed, uncompressed_size: uncompressed });
    }

    let (compressed, uncompressed) = read_sizes(&mut c, zip64)?;
    Ok(DataDescriptor { crc32: first, compressed_size: compressed, uncompressed_size: uncompressed })
}

fn read_sizes(c: &mut Cursor<'_>, zip64: bool) -> Result<(u64, u64)> {
    if zip64 {
        Ok((c.u64("compressed size")?, c.u64("uncompressed size")?))
    } else {
        Ok((c.u32("compressed size")? as u64, c.u32("uncompressed size")? as u64))
    }
}
