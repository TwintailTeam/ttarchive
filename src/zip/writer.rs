use std::io::{SeekFrom, Write};

use crate::codecs::{Level, Method};
use crate::crypto::stream::{EncryptWriter, encrypt_buffer};
use crate::crypto::winzip_aes::AesExtra;
use crate::crypto::{Encryption, Password};
use crate::platform::{EntryKind, EntryMeta};
use crate::utils::bytes::{put_u16, put_u32, put_u64};
use crate::utils::crc32::Crc32;
use crate::utils::datetime;
use crate::utils::error::{Error, Result};
use crate::utils::io::CountingWriter;
use crate::zip::attributes;
use crate::zip::parsers::extra::{self, Zip64Need};
use crate::zip::spec::{
    CENTRAL_HEADER_SIG, EOCD_SIG, LOCAL_HEADER_SIG, U16_MAX, U32_MAX, ZIP64_EOCD_SIG, ZIP64_LOCATOR_SIG, flags, host, make_version_made_by, version,
};
use crate::zip::volumes::Sink;

const AES_METHOD_MARKER: u16 = 99;
const AE_VERSION: u16 = 2;

#[derive(Debug, Clone)]
struct CentralRecord {
    raw_name: Vec<u8>,
    method: Method,
    flags: u16,
    dos_time: u16,
    dos_date: u16,
    crc32: u32,
    compressed_size: u64,
    uncompressed_size: u64,
    local_header_offset: u64,
    external_attributes: u32,
    mtime: i64,
    uid: Option<u32>,
    gid: Option<u32>,
    disk_start: u32,
    aes: Option<AesExtra>,
}

#[derive(Debug)]
pub struct PreparedEntry {
    pub name: String,
    pub meta: EntryMeta,
    pub method: Method,
    pub data: Vec<u8>,
    pub crc32: u32,
    pub uncompressed_size: u64,
}

pub struct ZipWriter<S: Sink> {
    sink: CountingWriter<S>,
    records: Vec<CentralRecord>,
    comment: Vec<u8>,
    level: Level,
    method: Option<Method>,
    encryption: Option<(Password, Encryption)>,
}

struct Directory {
    offset: u64,
    size: u64,
    count: u64,
    count_this_disk: u64,
    directory_disk: u32,
    last_disk: u32,
}

impl<S: Sink> ZipWriter<S> {
    pub fn new(mut sink: S) -> Result<Self> {
        let start = sink.stream_position()?;
        Ok(ZipWriter {
            sink: CountingWriter::new(sink, start),
            records: Vec::new(),
            comment: Vec::new(),
            level: Level::default(),
            method: None,
            encryption: None,
        })
    }

    pub fn set_level(&mut self, level: Level) {
        self.level = level;
    }

    pub fn set_method(&mut self, method: Option<Method>) {
        self.method = method;
    }

    pub fn set_encryption(&mut self, password: Password, encryption: Encryption) {
        self.encryption = Some((password, encryption));
    }

    pub fn set_comment(&mut self, comment: impl Into<Vec<u8>>) {
        let mut c = comment.into();
        c.truncate(crate::zip::spec::MAX_COMMENT_LEN);
        self.comment = c;
    }

    pub fn len(&self) -> usize {
        self.records.len()
    }

    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    pub fn add_directory(&mut self, name: &str, meta: &EntryMeta) -> Result<()> {
        let mut name = name.replace('\\', "/");
        if !name.ends_with('/') {
            name.push('/');
        }
        self.write_entry(&name, meta, Method::Store, &[], 0, 0)
    }

    pub fn add_symlink(&mut self, name: &str, target: &[u8], meta: &EntryMeta) -> Result<()> {
        let crc = crate::utils::crc32::checksum(target);
        self.write_entry(name, meta, Method::Store, target, crc, target.len() as u64)
    }

    pub fn add_file(&mut self, name: &str, meta: &EntryMeta, source: &mut impl std::io::Read, expected_size: u64) -> Result<()> {
        let method = self.level_method();
        let (raw_name, utf8) = encode_name(name, false);
        let dos = datetime::to_dos(meta.mtime.unwrap_or(0));
        let encrypt = self.encryption.clone();

        let aes =
            encrypt.as_ref().and_then(|(_, scheme)| scheme.strength().map(|strength| AesExtra { version: AE_VERSION, strength, actual_method: method.code() }));

        let mut entry_flags = base_flags(utf8, self.level);
        if encrypt.is_some() {
            entry_flags |= flags::ENCRYPTED;
            entry_flags |= flags::DATA_DESCRIPTOR;
        }
        let use_descriptor = entry_flags & flags::DATA_DESCRIPTOR != 0;
        let check_byte = (dos.time >> 8) as u8;

        self.reserve_header(&raw_name)?;
        let offset = self.sink.offset();
        let (disk, local_offset) = self.sink.get_mut().locate(offset);

        let zip64 = needs_zip64(expected_size, expected_size, local_offset);

        let values = LocalHeaderValues {
            method,
            method_code: if aes.is_some() { AES_METHOD_MARKER } else { method.code() },
            flags: entry_flags,
            dos,
            crc32: 0,
            compressed_size: 0,
            uncompressed_size: 0,
            zip64,
            aes,
        };
        self.write_local_header(&raw_name, &values, meta)?;

        let data_start = self.sink.offset();

        let mut crc = Crc32::new();
        let mut uncompressed = 0u64;
        let mut buf = vec![0u8; crate::utils::io::COPY_BUF];

        match &encrypt {
            None => {
                let mut encoder = crate::codecs::encoder(method, &mut self.sink, self.level)?;
                copy_compressing(source, &mut *encoder, &mut buf, &mut crc, &mut uncompressed)?;
                encoder.finish()?;
            }
            Some((password, scheme)) => {
                let mut sealed = EncryptWriter::new(&mut self.sink, password, *scheme, check_byte)?;
                {
                    let mut encoder = crate::codecs::encoder(method, &mut sealed, self.level)?;
                    copy_compressing(source, &mut *encoder, &mut buf, &mut crc, &mut uncompressed)?;
                    encoder.finish()?;
                }
                sealed.finish()?;
            }
        }

        let end = self.sink.offset();
        let compressed = end - data_start;
        let crc32 = crc.finish();

        if !zip64 && needs_zip64(compressed, uncompressed, local_offset) {
            return Err(Error::malformed(format!(
                "{name:?} grew past the 4 GiB Zip64 threshold while being archived \
                 (expected {expected_size} bytes, read {uncompressed})"
            )));
        }

        let stored_crc = if values.aes.is_some() { 0 } else { crc32 };

        if use_descriptor {
            self.write_data_descriptor(stored_crc, compressed, uncompressed, zip64)?;
        } else {
            self.patch_local_header(offset, raw_name.len() as u64, zip64, stored_crc, compressed, uncompressed)?;
            self.sink.get_mut().seek(SeekFrom::Start(end))?;
        }

        self.records.push(CentralRecord {
            raw_name,
            method,
            flags: entry_flags,
            dos_time: dos.time,
            dos_date: dos.date,
            crc32: stored_crc,
            compressed_size: compressed,
            uncompressed_size: uncompressed,
            local_header_offset: local_offset,
            external_attributes: attributes::encode(meta, host_system()),
            mtime: meta.mtime.unwrap_or(0),
            uid: meta.uid,
            gid: meta.gid,
            disk_start: disk,
            aes: values.aes,
        });

        Ok(())
    }

    pub fn add_produced<F>(&mut self, name: &str, meta: &EntryMeta, method: Method, expected_size: u64, produce: F) -> Result<()>
    where
        F: FnOnce(&mut dyn Write) -> Result<(u32, u64)>,
    {
        let (raw_name, utf8) = encode_name(name, false);
        let dos = datetime::to_dos(meta.mtime.unwrap_or(0));
        let encrypt = self.encryption.clone();

        let aes =
            encrypt.as_ref().and_then(|(_, scheme)| scheme.strength().map(|strength| AesExtra { version: AE_VERSION, strength, actual_method: method.code() }));

        let mut entry_flags = base_flags(utf8, self.level);
        if encrypt.is_some() {
            entry_flags |= flags::ENCRYPTED | flags::DATA_DESCRIPTOR;
        }
        let use_descriptor = entry_flags & flags::DATA_DESCRIPTOR != 0;
        let check_byte = (dos.time >> 8) as u8;

        self.reserve_header(&raw_name)?;
        let offset = self.sink.offset();
        let (disk, local_offset) = self.sink.get_mut().locate(offset);
        let zip64 = needs_zip64(expected_size, expected_size, local_offset);

        let values = LocalHeaderValues {
            method,
            method_code: if aes.is_some() { AES_METHOD_MARKER } else { method.code() },
            flags: entry_flags,
            dos,
            crc32: 0,
            compressed_size: 0,
            uncompressed_size: 0,
            zip64,
            aes,
        };
        self.write_local_header(&raw_name, &values, meta)?;

        let data_start = self.sink.offset();

        let (crc32, uncompressed) = match &encrypt {
            None => produce(&mut self.sink)?,
            Some((password, scheme)) => {
                let mut sealed = EncryptWriter::new(&mut self.sink, password, *scheme, check_byte)?;
                let result = produce(&mut sealed)?;
                sealed.finish()?;
                result
            }
        };

        let end = self.sink.offset();
        let compressed = end - data_start;

        if !zip64 && needs_zip64(compressed, uncompressed, local_offset) {
            return Err(Error::malformed(format!(
                "{name:?} grew past the 4 GiB Zip64 threshold while being archived \
                 (expected {expected_size} bytes, produced {uncompressed})"
            )));
        }

        let stored_crc = if values.aes.is_some() { 0 } else { crc32 };

        if use_descriptor {
            self.write_data_descriptor(stored_crc, compressed, uncompressed, zip64)?;
        } else {
            self.patch_local_header(offset, raw_name.len() as u64, zip64, stored_crc, compressed, uncompressed)?;
            self.sink.get_mut().seek(SeekFrom::Start(end))?;
        }

        self.records.push(CentralRecord {
            raw_name,
            method,
            flags: entry_flags,
            dos_time: dos.time,
            dos_date: dos.date,
            crc32: stored_crc,
            compressed_size: compressed,
            uncompressed_size: uncompressed,
            local_header_offset: local_offset,
            external_attributes: attributes::encode(meta, host_system()),
            mtime: meta.mtime.unwrap_or(0),
            uid: meta.uid,
            gid: meta.gid,
            disk_start: disk,
            aes: values.aes,
        });

        Ok(())
    }

    fn write_data_descriptor(&mut self, crc32: u32, compressed: u64, uncompressed: u64, zip64: bool) -> Result<()> {
        let mut buf = Vec::with_capacity(24);
        put_u32(&mut buf, crate::zip::spec::DATA_DESCRIPTOR_SIG);
        put_u32(&mut buf, crc32);
        if zip64 {
            put_u64(&mut buf, compressed);
            put_u64(&mut buf, uncompressed);
        } else {
            put_u32(&mut buf, compressed as u32);
            put_u32(&mut buf, uncompressed as u32);
        }
        self.sink.get_mut().begin_record(buf.len() as u64)?;
        self.sink.write_all(&buf)?;
        Ok(())
    }

    pub fn add_prepared(&mut self, prepared: PreparedEntry) -> Result<()> {
        self.write_entry(&prepared.name, &prepared.meta, prepared.method, &prepared.data, prepared.crc32, prepared.uncompressed_size)
    }

    fn write_entry(&mut self, name: &str, meta: &EntryMeta, method: Method, data: &[u8], crc32: u32, uncompressed_size: u64) -> Result<()> {
        let is_dir = meta.kind == EntryKind::Directory;
        let (raw_name, utf8) = encode_name(name, is_dir);
        let dos = datetime::to_dos(meta.mtime.unwrap_or(0));

        let encrypt = self.encryption.as_ref().filter(|_| !is_dir).cloned();

        let (payload, aes, stored_crc) = match &encrypt {
            None => (data.to_vec(), None, crc32),
            Some((password, scheme)) => {
                let sealed = encrypt_buffer(data, password, *scheme, (crc32 >> 24) as u8)?;
                match scheme.strength() {
                    Some(strength) => (sealed, Some(AesExtra { version: AE_VERSION, strength, actual_method: method.code() }), 0),
                    None => (sealed, None, crc32),
                }
            }
        };

        let stored_method_code = if aes.is_some() { AES_METHOD_MARKER } else { method.code() };

        let mut entry_flags = base_flags(utf8, self.level);
        if encrypt.is_some() {
            entry_flags |= flags::ENCRYPTED;
        }

        self.reserve_header(&raw_name)?;
        let offset = self.sink.offset();
        let (disk, local_offset) = self.sink.get_mut().locate(offset);

        let values = LocalHeaderValues {
            method,
            method_code: stored_method_code,
            flags: entry_flags,
            dos,
            crc32: stored_crc,
            compressed_size: payload.len() as u64,
            uncompressed_size,
            zip64: needs_zip64(payload.len() as u64, uncompressed_size, local_offset),
            aes,
        };

        self.write_local_header(&raw_name, &values, meta)?;
        self.sink.write_all(&payload)?;

        self.records.push(CentralRecord {
            raw_name,
            method,
            flags: values.flags,
            dos_time: dos.time,
            dos_date: dos.date,
            crc32: stored_crc,
            compressed_size: payload.len() as u64,
            uncompressed_size,
            local_header_offset: local_offset,
            external_attributes: attributes::encode(meta, host_system()),
            mtime: meta.mtime.unwrap_or(0),
            uid: meta.uid,
            gid: meta.gid,
            disk_start: disk,
            aes,
        });

        Ok(())
    }

    fn reserve_header(&mut self, raw_name: &[u8]) -> Result<()> {
        let bound = 30 + raw_name.len() as u64 + 64;
        self.sink.get_mut().begin_record(bound)?;
        Ok(())
    }

    fn level_method(&self) -> Method {
        match self.method {
            Some(_) | None if self.level == Level::None => Method::Store,
            Some(method) => method,
            None => Method::Deflate,
        }
    }

    fn write_local_header(&mut self, raw_name: &[u8], values: &LocalHeaderValues, meta: &EntryMeta) -> Result<()> {
        let mut extra_field = Vec::new();

        if values.zip64 {
            let need = Zip64Need { uncompressed_size: true, compressed_size: true, local_header_offset: false, disk_start: false };
            extra::write_zip64(&mut extra_field, need, values.uncompressed_size, values.compressed_size, 0, 0);
        }

        extra::write_extended_timestamp(&mut extra_field, meta.mtime.unwrap_or(0));
        if let (Some(uid), Some(gid)) = (meta.uid, meta.gid) {
            extra::write_infozip_unix2(&mut extra_field, uid, gid);
        }
        if let Some(aes) = &values.aes {
            extra::write_aes(&mut extra_field, aes);
        }

        let mut header = Vec::with_capacity(30 + raw_name.len() + extra_field.len());
        put_u32(&mut header, LOCAL_HEADER_SIG);
        put_u16(&mut header, version_needed(values.method, values.zip64, values.aes.is_some()));
        put_u16(&mut header, values.flags);
        put_u16(&mut header, values.method_code);
        put_u16(&mut header, values.dos.time);
        put_u16(&mut header, values.dos.date);
        put_u32(&mut header, values.crc32);
        put_u32(&mut header, clamp32(values.compressed_size, values.zip64));
        put_u32(&mut header, clamp32(values.uncompressed_size, values.zip64));
        put_u16(&mut header, raw_name.len() as u16);
        put_u16(&mut header, extra_field.len() as u16);
        header.extend_from_slice(raw_name);
        header.extend_from_slice(&extra_field);

        self.sink.write_all(&header)?;
        Ok(())
    }

    fn patch_local_header(&mut self, offset: u64, name_len: u64, zip64: bool, crc32: u32, compressed: u64, uncompressed: u64) -> Result<()> {
        let writer = self.sink.get_mut();

        writer.seek(SeekFrom::Start(offset + 14))?;
        let mut fields = Vec::with_capacity(12);
        put_u32(&mut fields, crc32);
        put_u32(&mut fields, clamp32(compressed, zip64));
        put_u32(&mut fields, clamp32(uncompressed, zip64));
        writer.write_all(&fields)?;

        if zip64 {
            writer.seek(SeekFrom::Start(offset + 30 + name_len + 4))?;
            let mut sizes = Vec::with_capacity(16);
            put_u64(&mut sizes, uncompressed);
            put_u64(&mut sizes, compressed);
            writer.write_all(&sizes)?;
        }

        Ok(())
    }

    pub fn finish(mut self) -> Result<S> {
        self.write_central_directory()?;
        let mut inner = self.sink.into_inner();
        inner.flush()?;
        Ok(inner)
    }

    fn write_central_directory(&mut self) -> Result<()> {
        let records = std::mem::take(&mut self.records);

        let directory_offset = self.sink.offset();
        let (directory_disk, directory_local) = self.sink.get_mut().locate(directory_offset);
        let mut entries_on_last_disk = 0u64;

        for record in &records {
            let need = Zip64Need {
                uncompressed_size: record.uncompressed_size >= U32_MAX as u64,
                compressed_size: record.compressed_size >= U32_MAX as u64,
                local_header_offset: record.local_header_offset >= U32_MAX as u64,
                disk_start: false,
            };

            let mut extra_field = Vec::new();
            extra::write_zip64(&mut extra_field, need, record.uncompressed_size, record.compressed_size, record.local_header_offset, 0);
            extra::write_extended_timestamp(&mut extra_field, record.mtime);
            if let (Some(uid), Some(gid)) = (record.uid, record.gid) {
                extra::write_infozip_unix2(&mut extra_field, uid, gid);
            }
            if let Some(aes) = &record.aes {
                extra::write_aes(&mut extra_field, aes);
            }

            let zip64 = !need.is_empty();

            let mut header = Vec::with_capacity(46 + record.raw_name.len() + extra_field.len());
            self.sink.get_mut().begin_record(header.capacity() as u64)?;
            put_u32(&mut header, CENTRAL_HEADER_SIG);
            put_u16(&mut header, make_version_made_by(host_system(), 63));
            put_u16(&mut header, version_needed(record.method, zip64, record.aes.is_some()));
            put_u16(&mut header, record.flags);
            put_u16(&mut header, if record.aes.is_some() { AES_METHOD_MARKER } else { record.method.code() });
            put_u16(&mut header, record.dos_time);
            put_u16(&mut header, record.dos_date);
            put_u32(&mut header, record.crc32);
            put_u32(&mut header, saturate32(record.compressed_size));
            put_u32(&mut header, saturate32(record.uncompressed_size));
            put_u16(&mut header, record.raw_name.len() as u16);
            put_u16(&mut header, extra_field.len() as u16);
            put_u16(&mut header, 0);
            put_u16(&mut header, saturate16(record.disk_start as u64));
            put_u16(&mut header, 0);
            put_u32(&mut header, record.external_attributes);
            put_u32(&mut header, saturate32(record.local_header_offset));
            header.extend_from_slice(&record.raw_name);
            header.extend_from_slice(&extra_field);

            let before = self.sink.offset();
            self.sink.write_all(&header)?;

            let (disk_here, _) = self.sink.get_mut().locate(before);
            let last_disk = self.sink.get_mut().disks().saturating_sub(1);
            if disk_here == last_disk {
                entries_on_last_disk += 1;
            } else {
                entries_on_last_disk = 0;
            }
        }

        let directory_size = self.sink.offset() - directory_offset;
        let count = records.len() as u64;
        self.records = records;

        let last_disk = self.sink.get_mut().disks().saturating_sub(1);

        let needs_zip64 = count > U16_MAX as u64 || directory_size >= U32_MAX as u64 || directory_local >= U32_MAX as u64 || last_disk >= U16_MAX as u32;

        if needs_zip64 {
            let directory =
                Directory { offset: directory_local, size: directory_size, count, count_this_disk: entries_on_last_disk, directory_disk, last_disk };
            self.write_zip64_end(&directory)?;
        }

        let mut eocd = Vec::with_capacity(22 + self.comment.len());
        self.sink.get_mut().begin_record(eocd.capacity() as u64)?;
        put_u32(&mut eocd, EOCD_SIG);
        put_u16(&mut eocd, saturate16(last_disk as u64));
        put_u16(&mut eocd, saturate16(directory_disk as u64));
        put_u16(&mut eocd, saturate16(entries_on_last_disk));
        put_u16(&mut eocd, saturate16(count));
        put_u32(&mut eocd, saturate32(directory_size));
        put_u32(&mut eocd, saturate32(directory_local));
        put_u16(&mut eocd, self.comment.len() as u16);
        eocd.extend_from_slice(&self.comment);

        self.sink.write_all(&eocd)?;
        Ok(())
    }

    fn write_zip64_end(&mut self, directory: &Directory) -> Result<()> {
        let Directory { offset, size, count, count_this_disk, directory_disk, last_disk } = *directory;

        let record_offset = self.sink.offset();
        let (record_disk, record_local) = self.sink.get_mut().locate(record_offset);

        let mut buf = Vec::with_capacity(76);
        self.sink.get_mut().begin_record(76)?;
        put_u32(&mut buf, ZIP64_EOCD_SIG);
        put_u64(&mut buf, 44);
        put_u16(&mut buf, make_version_made_by(host_system(), 63));
        put_u16(&mut buf, version::ZIP64);
        put_u32(&mut buf, last_disk);
        put_u32(&mut buf, directory_disk);
        put_u64(&mut buf, count_this_disk);
        put_u64(&mut buf, count);
        put_u64(&mut buf, size);
        put_u64(&mut buf, offset);

        put_u32(&mut buf, ZIP64_LOCATOR_SIG);
        put_u32(&mut buf, record_disk);
        put_u64(&mut buf, record_local);
        put_u32(&mut buf, last_disk + 1);

        self.sink.write_all(&buf)?;
        Ok(())
    }
}

struct LocalHeaderValues {
    method: Method,
    method_code: u16,
    flags: u16,
    dos: datetime::DosDateTime,
    crc32: u32,
    compressed_size: u64,
    uncompressed_size: u64,
    zip64: bool,
    aes: Option<AesExtra>,
}

fn copy_compressing(source: &mut impl std::io::Read, out: &mut dyn Write, buf: &mut [u8], crc: &mut Crc32, uncompressed: &mut u64) -> Result<()> {
    loop {
        let n = match source.read(buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(ref e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(Error::from(e)),
        };
        crc.update(&buf[..n]);
        *uncompressed += n as u64;
        out.write_all(&buf[..n])?;
    }
    Ok(())
}

fn host_system() -> u8 {
    if cfg!(unix) {
        host::UNIX
    } else if cfg!(windows) {
        host::NTFS
    } else {
        host::MSDOS
    }
}

fn base_flags(utf8: bool, level: Level) -> u16 {
    let mut f = level.gp_flag_bits();
    if utf8 {
        f |= flags::UTF8;
    }
    f
}

fn encode_name(name: &str, is_dir: bool) -> (Vec<u8>, bool) {
    let mut name = name.replace('\\', "/");
    if is_dir && !name.ends_with('/') {
        name.push('/');
    }

    let needs_utf8 = !name.is_ascii();
    (name.into_bytes(), needs_utf8)
}

fn needs_zip64(compressed: u64, uncompressed: u64, offset: u64) -> bool {
    compressed >= U32_MAX as u64 || uncompressed >= U32_MAX as u64 || offset >= U32_MAX as u64
}

fn version_needed(method: Method, zip64: bool, aes: bool) -> u16 {
    let mut needed = method.version_needed();
    if zip64 {
        needed = needed.max(version::ZIP64);
    }
    if aes {
        needed = needed.max(51);
    }
    needed
}

fn clamp32(value: u64, zip64: bool) -> u32 {
    if zip64 { U32_MAX } else { value as u32 }
}

fn saturate32(value: u64) -> u32 {
    if value >= U32_MAX as u64 { U32_MAX } else { value as u32 }
}

fn saturate16(value: u64) -> u16 {
    if value >= U16_MAX as u64 { U16_MAX } else { value as u16 }
}
