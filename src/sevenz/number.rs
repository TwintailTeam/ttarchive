use crate::utils::bytes::Cursor;
use crate::utils::error::{Error, Result};

pub const MAX_COUNT: u64 = 1 << 28;

pub fn number(cursor: &mut Cursor<'_>, what: &str) -> Result<u64> {
    let first = cursor.u8(what)?;
    let mut mask = 0x80u8;
    let mut value = 0u64;

    for i in 0..8 {
        if first & mask == 0 {
            let high = (first & (mask - 1)) as u64;
            return Ok(value | (high << (i * 8)));
        }
        value |= (cursor.u8(what)? as u64) << (8 * i);
        mask >>= 1;
    }

    Ok(value)
}

pub fn count(cursor: &mut Cursor<'_>, what: &str) -> Result<usize> {
    let at = cursor.offset();
    let value = number(cursor, what)?;
    if value > MAX_COUNT {
        return Err(Error::malformed_at(format!("7z header declares {value} {what}, past the {MAX_COUNT} this build accepts"), at));
    }
    Ok(value as usize)
}

pub fn put_number(out: &mut Vec<u8>, value: u64) {
    let mut first = 0u8;
    let mut mask = 0x80u8;

    for i in 0..8u32 {
        if value < 1u64 << (7 * (i + 1)) {
            first |= (value >> (8 * i)) as u8;
            out.push(first);
            for byte in 0..i {
                out.push((value >> (8 * byte)) as u8);
            }
            return;
        }
        first |= mask;
        mask >>= 1;
    }

    out.push(0xFF);
    out.extend_from_slice(&value.to_le_bytes());
}

pub fn bits(cursor: &mut Cursor<'_>, len: usize, what: &str) -> Result<Vec<bool>> {
    let bytes = cursor.slice(len.div_ceil(8), what)?;
    Ok((0..len).map(|i| bytes[i / 8] >> (7 - (i % 8)) & 1 == 1).collect())
}

pub fn defined_bits(cursor: &mut Cursor<'_>, len: usize, what: &str) -> Result<Vec<bool>> {
    if cursor.u8(what)? != 0 {
        return Ok(vec![true; len]);
    }
    bits(cursor, len, what)
}

pub fn put_bits(out: &mut Vec<u8>, values: &[bool]) {
    let mut byte = 0u8;
    let mut mask = 0x80u8;

    for &value in values {
        if value {
            byte |= mask;
        }
        mask >>= 1;
        if mask == 0 {
            out.push(byte);
            byte = 0;
            mask = 0x80;
        }
    }

    if mask != 0x80 {
        out.push(byte);
    }
}

pub fn put_defined_bits(out: &mut Vec<u8>, values: &[bool]) {
    if values.iter().all(|&v| v) {
        out.push(1);
        return;
    }
    out.push(0);
    put_bits(out, values);
}
