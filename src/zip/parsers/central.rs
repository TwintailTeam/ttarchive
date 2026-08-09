use crate::pipeline::entry::{Entry, EntryDetail, ZipDetail};
use crate::platform::policy;
use crate::utils::bytes::Cursor;
use crate::utils::error::{Error, Result};
use crate::utils::{cp437, datetime};
use crate::zip::attributes;
use crate::zip::parsers::extra::{self, Zip64Need};
use crate::zip::spec::{CENTRAL_HEADER_SIG, U32_MAX, flags};

pub fn parse_all(buf: &[u8], base: u64, expected: u64) -> Result<Vec<Entry>> {
    let mut entries = Vec::with_capacity(expected.min(64 * 1024) as usize);
    let mut cursor = Cursor::new(buf, base);

    while cursor.remaining() >= 4 {
        let peek = cursor.clone();
        if peek.clone().u32("signature")? != CENTRAL_HEADER_SIG {
            break;
        }

        entries.push(parse_one(&mut cursor)?);
    }

    Ok(entries)
}

pub fn parse_one(cursor: &mut Cursor<'_>) -> Result<Entry> {
    let header_offset = cursor.offset();

    let signature = cursor.u32("central header signature")?;
    if signature != CENTRAL_HEADER_SIG {
        return Err(Error::malformed_at(format!("expected central directory header signature, found {signature:#010x}"), header_offset));
    }

    let version_made_by = cursor.u16("version made by")?;
    let version_needed = cursor.u16("version needed to extract")?;
    let flag_bits = cursor.u16("general purpose bit flag")?;
    let method_code = cursor.u16("compression method")?;
    let dos_time = cursor.u16("last mod file time")?;
    let dos_date = cursor.u16("last mod file date")?;
    let crc32 = cursor.u32("crc-32")?;
    let compressed_size = cursor.u32("compressed size")?;
    let uncompressed_size = cursor.u32("uncompressed size")?;
    let name_len = cursor.u16("file name length")?;
    let extra_len = cursor.u16("extra field length")?;
    let comment_len = cursor.u16("file comment length")?;
    let disk_start = cursor.u16("disk number start")?;
    let internal_attributes = cursor.u16("internal file attributes")?;
    let external_attributes = cursor.u32("external file attributes")?;
    let local_header_offset = cursor.u32("relative offset of local header")?;

    let raw_name = cursor.slice(name_len as usize, "file name")?.to_vec();
    let extra_bytes = cursor.slice(extra_len as usize, "extra field")?;
    let comment_bytes = cursor.slice(comment_len as usize, "file comment")?;

    let need = Zip64Need::from_central(uncompressed_size, compressed_size, local_header_offset, disk_start);
    let extra_fields = extra::parse(extra_bytes, need, header_offset);

    let disk_start = extra_fields.disk_start.unwrap_or(disk_start as u32);

    let utf8 = flag_bits & flags::UTF8 != 0;
    let name = cp437::decode_name(&raw_name, utf8);
    let comment = cp437::decode_name(comment_bytes, utf8);

    let uncompressed_size = resolve(uncompressed_size, extra_fields.uncompressed_size);
    let compressed_size = resolve(compressed_size, extra_fields.compressed_size);
    let local_header_offset = resolve(local_header_offset, extra_fields.local_header_offset);

    let dos_mtime = datetime::from_dos(datetime::DosDateTime { time: dos_time, date: dos_date });
    let name_is_dir = policy::has_directory_suffix(&name);
    let meta = attributes::decode(external_attributes, version_made_by, name_is_dir, &extra_fields, dos_mtime);

    Ok(Entry {
        name,
        size: uncompressed_size,
        meta,
        detail: EntryDetail::Zip(ZipDetail {
            raw_name,
            comment,
            method_code,
            crc32,
            compressed_size,
            local_header_offset,
            disk_start,
            flags: flag_bits,
            version_made_by,
            version_needed,
            external_attributes,
            internal_attributes,
            dos_mtime,
            aes: extra_fields.aes,
        }),
    })
}

fn resolve(base: u32, zip64: Option<u64>) -> u64 {
    match zip64 {
        Some(v) => v,
        None if base == U32_MAX => U32_MAX as u64,
        None => base as u64,
    }
}
