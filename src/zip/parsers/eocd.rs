use std::io::{Read, Seek, SeekFrom};

use crate::utils::bytes::{Cursor, rfind};
use crate::utils::error::{Error, Result};
use crate::zip::spec::{EOCD_LEN, EOCD_SIG, MAX_COMMENT_LEN, U16_MAX, U32_MAX, ZIP64_EOCD_SIG, ZIP64_LOCATOR_LEN, ZIP64_LOCATOR_SIG};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Directory {
    pub offset: u64,
    pub size: u64,
    pub entries: u64,
    pub comment: Vec<u8>,
    pub zip64: bool,
    pub eocd_offset: u64,
    pub this_disk: u32,
    pub directory_disk: u32,
    pub total_disks: u32,
}

#[derive(Debug, Clone, Copy)]
struct Eocd {
    disk: u16,
    directory_disk: u16,
    entries_this_disk: u16,
    entries_total: u16,
    size: u32,
    offset: u32,
    comment_len: u16,
}

pub fn find<R: Read + Seek>(reader: &mut R) -> Result<Directory> {
    let file_len = reader.seek(SeekFrom::End(0))?;
    if file_len < EOCD_LEN as u64 {
        return Err(Error::malformed(format!("file is {file_len} bytes, too short to contain a ZIP end of central directory")));
    }

    let search_len = (EOCD_LEN + MAX_COMMENT_LEN).min(file_len as usize);
    let search_start = file_len - search_len as u64;
    let mut tail = vec![0u8; search_len];
    reader.seek(SeekFrom::Start(search_start))?;
    reader.read_exact(&mut tail)?;

    let (eocd, eocd_offset) = locate_eocd(&tail, search_start, file_len)?;

    let needs_zip64 = eocd.entries_total == U16_MAX
        || eocd.entries_this_disk == U16_MAX
        || eocd.size == U32_MAX
        || eocd.offset == U32_MAX
        || eocd.disk == U16_MAX
        || eocd.directory_disk == U16_MAX;

    let comment_start = (eocd_offset - search_start) as usize + EOCD_LEN;
    let comment = tail.get(comment_start..comment_start + eocd.comment_len as usize).unwrap_or(&[]).to_vec();

    if needs_zip64 && let Some(dir) = read_zip64(reader, eocd_offset, comment.clone(), file_len)? {
        return Ok(dir);
    }

    Ok(Directory {
        offset: eocd.offset as u64,
        size: eocd.size as u64,
        entries: eocd.entries_total as u64,
        comment,
        zip64: false,
        eocd_offset,
        this_disk: eocd.disk as u32,
        directory_disk: eocd.directory_disk as u32,
        total_disks: eocd.disk as u32 + 1,
    })
}

fn locate_eocd(tail: &[u8], tail_base: u64, file_len: u64) -> Result<(Eocd, u64)> {
    let sig = EOCD_SIG.to_le_bytes();
    let mut search = tail;

    loop {
        let Some(pos) = rfind(search, &sig) else {
            return Err(Error::malformed(
                "no end of central directory signature found; not a ZIP archive \
                 (or the archive is truncated)",
            ));
        };

        if search.len() - pos >= EOCD_LEN
            && let Ok(eocd) = parse_eocd(&search[pos..])
        {
            let absolute = tail_base + pos as u64;
            let expected_end = absolute + EOCD_LEN as u64 + eocd.comment_len as u64;

            if expected_end == file_len {
                return Ok((eocd, absolute));
            }
        }

        search = &search[..pos];
    }
}

fn parse_eocd(buf: &[u8]) -> Result<Eocd> {
    let mut c = Cursor::new(buf, 0);
    let sig = c.u32("eocd signature")?;
    if sig != EOCD_SIG {
        return Err(Error::malformed("bad eocd signature"));
    }
    Ok(Eocd {
        disk: c.u16("number of this disk")?,
        directory_disk: c.u16("disk with central directory")?,
        entries_this_disk: c.u16("entries on this disk")?,
        entries_total: c.u16("total entries")?,
        size: c.u32("central directory size")?,
        offset: c.u32("central directory offset")?,
        comment_len: c.u16("comment length")?,
    })
}

fn read_zip64<R: Read + Seek>(reader: &mut R, eocd_offset: u64, comment: Vec<u8>, file_len: u64) -> Result<Option<Directory>> {
    if eocd_offset < ZIP64_LOCATOR_LEN as u64 {
        return Ok(None);
    }

    let locator_offset = eocd_offset - ZIP64_LOCATOR_LEN as u64;
    let mut buf = [0u8; ZIP64_LOCATOR_LEN];
    reader.seek(SeekFrom::Start(locator_offset))?;
    reader.read_exact(&mut buf)?;

    let mut c = Cursor::new(&buf, locator_offset);
    if c.u32("zip64 locator signature")? != ZIP64_LOCATOR_SIG {
        return Ok(None);
    }
    let locator_disk = c.u32("zip64 locator disk")?;
    let zip64_eocd_offset = c.u64("zip64 eocd offset")?;
    let total_disks = c.u32("total disks")?;

    let _ = locator_disk;
    if zip64_eocd_offset >= file_len {
        return Err(Error::malformed_at("zip64 end of central directory offset points past end of file", locator_offset));
    }

    let mut buf = [0u8; 56];
    reader.seek(SeekFrom::Start(zip64_eocd_offset))?;
    reader.read_exact(&mut buf)?;

    let mut c = Cursor::new(&buf, zip64_eocd_offset);
    if c.u32("zip64 eocd signature")? != ZIP64_EOCD_SIG {
        return Err(Error::malformed_at("zip64 end of central directory signature missing", zip64_eocd_offset));
    }
    let _record_size = c.u64("zip64 eocd size")?;
    let _version_made_by = c.u16("version made by")?;
    let _version_needed = c.u16("version needed")?;
    let disk = c.u32("number of this disk")?;
    let directory_disk = c.u32("disk with central directory")?;
    let _entries_this_disk = c.u64("entries on this disk")?;
    let entries = c.u64("total entries")?;
    let size = c.u64("central directory size")?;
    let offset = c.u64("central directory offset")?;

    Ok(Some(Directory { offset, size, entries, comment, zip64: true, eocd_offset, this_disk: disk, directory_disk, total_disks: total_disks.max(1) }))
}

pub fn validate_range(offset: u64, size: u64, file_len: u64) -> Result<()> {
    let end = offset.checked_add(size).ok_or_else(|| Error::malformed("central directory offset plus size overflows"))?;
    if end > file_len {
        return Err(Error::malformed(format!("central directory runs from {offset} to {end}, past the {file_len} byte file")));
    }
    Ok(())
}
