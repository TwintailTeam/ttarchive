use crate::codecs::lzma::window::Window;
use crate::codecs::zstd::bits::BackwardBits;
use crate::codecs::zstd::fse::{self, State, Table};
use crate::utils::error::{Error, Result};

pub(crate) const LITERAL_LENGTH_EXTRA: [u8; 36] =
    [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 3, 3, 4, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16];
pub(crate) const LITERAL_LENGTH_BASE: [u32; 36] =
    [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 18, 20, 22, 24, 28, 32, 40, 48, 64, 128, 256, 512, 1024, 2048, 4096, 8192, 16384, 32768, 65536];

pub(crate) const MATCH_LENGTH_EXTRA: [u8; 53] = [
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 3, 3, 4, 4, 5, 7, 8, 9, 10, 11, 12, 13,
    14, 15, 16,
];
pub(crate) const MATCH_LENGTH_BASE: [u32; 53] = [
    3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 28, 29, 30, 31, 32, 33, 34, 35, 37, 39, 41, 43, 47, 51, 59,
    67, 83, 99, 131, 259, 515, 1027, 2051, 4099, 8195, 16387, 32771, 65539,
];

pub(crate) const DEFAULT_LITERAL_LENGTH: [i32; 36] =
    [4, 3, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 1, 1, 1, 2, 2, 2, 2, 2, 2, 2, 2, 2, 3, 2, 1, 1, 1, 1, 1, -1, -1, -1, -1];
pub(crate) const DEFAULT_MATCH_LENGTH: [i32; 53] = [
    1, 4, 3, 2, 2, 2, 2, 2, 2, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, -1, -1, -1, -1,
    -1, -1, -1,
];
pub(crate) const DEFAULT_OFFSET: [i32; 29] = [1, 1, 1, 1, 1, 1, 2, 2, 2, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, -1, -1, -1, -1, -1];

pub(crate) const DEFAULT_LITERAL_LENGTH_LOG: u32 = 6;
pub(crate) const DEFAULT_MATCH_LENGTH_LOG: u32 = 6;
pub(crate) const DEFAULT_OFFSET_LOG: u32 = 5;

const MAX_LITERAL_LENGTH_SYMBOL: usize = 35;
const MAX_MATCH_LENGTH_SYMBOL: usize = 52;
pub(crate) const MAX_OFFSET_SYMBOL: usize = 31;

#[derive(Default)]
pub struct Tables {
    literal_length: Option<Table>,
    offset: Option<Table>,
    match_length: Option<Table>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Predefined,
    Rle,
    Compressed,
    Repeat,
}

impl Mode {
    fn from_bits(value: u8) -> Self {
        match value {
            0 => Mode::Predefined,
            1 => Mode::Rle,
            2 => Mode::Compressed,
            _ => Mode::Repeat,
        }
    }
}

struct TableSpec {
    default: &'static [i32],
    default_log: u32,
    max_symbol: usize,
    what: &'static str,
}

const LITERAL_LENGTH_SPEC: TableSpec =
    TableSpec { default: &DEFAULT_LITERAL_LENGTH, default_log: DEFAULT_LITERAL_LENGTH_LOG, max_symbol: MAX_LITERAL_LENGTH_SYMBOL, what: "literal length" };
const OFFSET_SPEC: TableSpec = TableSpec { default: &DEFAULT_OFFSET, default_log: DEFAULT_OFFSET_LOG, max_symbol: MAX_OFFSET_SYMBOL, what: "offset" };
const MATCH_LENGTH_SPEC: TableSpec =
    TableSpec { default: &DEFAULT_MATCH_LENGTH, default_log: DEFAULT_MATCH_LENGTH_LOG, max_symbol: MAX_MATCH_LENGTH_SYMBOL, what: "match length" };

fn take_table(mode: Mode, data: &[u8], offset: &mut usize, kept: &mut Option<Table>, spec: &TableSpec) -> Result<()> {
    let TableSpec { default, default_log, max_symbol, what } = *spec;
    match mode {
        Mode::Predefined => *kept = Some(Table::from_counts(default, default_log)?),
        Mode::Rle => {
            let symbol = *data.get(*offset).ok_or_else(|| Error::malformed(format!("zstd {what} RLE table has no symbol")))?;
            *offset += 1;
            *kept = Some(Table::single(symbol));
        }
        Mode::Compressed => {
            let (table, used) = Table::parse(&data[*offset..], fse::MAX_TABLE_LOG, max_symbol)?;
            *offset += used;
            *kept = Some(table);
        }
        Mode::Repeat => {
            if kept.is_none() {
                return Err(Error::malformed(format!("zstd block reuses a {what} table that was never sent")));
            }
        }
    }
    Ok(())
}

/// Where a block's output goes and how far back it may reach.
pub struct Target<'a> {
    /// The frame's output, appended to. Its history starts where the frame did,
    /// which is what bounds how far a match may reach.
    pub out: &'a mut Window,
    /// The declared window, or zero when the frame set none.
    pub window_size: u64,
}

pub fn execute(data: &[u8], literals: &[u8], target: &mut Target<'_>, tables: &mut Tables, repeats: &mut [u32; 3]) -> Result<()> {
    let Target { out, window_size } = target;
    let window_size = *window_size;
    let (count, mut offset) = sequence_count(data)?;

    if count == 0 {
        out.extend(literals);
        return Ok(());
    }

    let modes = *data.get(offset).ok_or_else(|| Error::malformed("zstd sequences section has no mode byte"))?;
    offset += 1;
    if modes & 0x3 != 0 {
        return Err(Error::malformed("zstd sequence mode byte uses its reserved bits"));
    }

    let literal_mode = Mode::from_bits(modes >> 6);
    let offset_mode = Mode::from_bits((modes >> 4) & 0x3);
    let match_mode = Mode::from_bits((modes >> 2) & 0x3);

    take_table(literal_mode, data, &mut offset, &mut tables.literal_length, &LITERAL_LENGTH_SPEC)?;
    take_table(offset_mode, data, &mut offset, &mut tables.offset, &OFFSET_SPEC)?;
    take_table(match_mode, data, &mut offset, &mut tables.match_length, &MATCH_LENGTH_SPEC)?;

    let literal_table = tables.literal_length.as_ref().expect("literal length table was just set");
    let offset_table = tables.offset.as_ref().expect("offset table was just set");
    let match_table = tables.match_length.as_ref().expect("match length table was just set");

    let stream = data.get(offset..).filter(|s| !s.is_empty()).ok_or_else(|| Error::malformed("zstd sequences have no bitstream"))?;
    let mut bits = BackwardBits::new(stream)?;

    let mut literal_state = State::new(literal_table, &mut bits);
    let mut offset_state = State::new(offset_table, &mut bits);
    let mut match_state = State::new(match_table, &mut bits);

    let mut literal_pos = 0usize;

    for index in 0..count {
        let offset_code = offset_state.symbol(offset_table) as u32;
        let match_code = match_state.symbol(match_table) as usize;
        let literal_code = literal_state.symbol(literal_table) as usize;

        if offset_code > 31 || match_code >= MATCH_LENGTH_BASE.len() || literal_code >= LITERAL_LENGTH_BASE.len() {
            return Err(Error::malformed("zstd sequence names a symbol outside its alphabet"));
        }

        let raw_offset = (1u64 << offset_code) + bits.bits(offset_code);
        let match_length = MATCH_LENGTH_BASE[match_code] as u64 + bits.bits(MATCH_LENGTH_EXTRA[match_code] as u32);
        let literal_length = LITERAL_LENGTH_BASE[literal_code] as u64 + bits.bits(LITERAL_LENGTH_EXTRA[literal_code] as u32);

        let distance = resolve_offset(raw_offset, literal_length, repeats)?;

        let piece = Piece { literal_length: literal_length as usize, distance, match_length: match_length as usize };
        emit(out, literals, &mut literal_pos, &piece, window_size)?;

        if index + 1 < count {
            literal_state.advance(literal_table, &mut bits);
            match_state.advance(match_table, &mut bits);
            offset_state.advance(offset_table, &mut bits);
        }
    }

    out.extend(&literals[literal_pos..]);
    Ok(())
}

fn sequence_count(data: &[u8]) -> Result<(usize, usize)> {
    let first = *data.first().ok_or_else(|| Error::malformed("zstd sequences section is empty"))? as usize;

    if first < 128 {
        return Ok((first, 1));
    }
    if first < 255 {
        let second = *data.get(1).ok_or_else(|| Error::malformed("zstd sequence count is truncated"))? as usize;
        return Ok((((first - 128) << 8) + second, 2));
    }

    let second = *data.get(1).ok_or_else(|| Error::malformed("zstd sequence count is truncated"))? as usize;
    let third = *data.get(2).ok_or_else(|| Error::malformed("zstd sequence count is truncated"))? as usize;
    Ok((second + (third << 8) + 0x7F00, 3))
}

fn resolve_offset(raw: u64, literal_length: u64, repeats: &mut [u32; 3]) -> Result<usize> {
    if raw > 3 {
        let distance = (raw - 3) as u32;
        repeats[2] = repeats[1];
        repeats[1] = repeats[0];
        repeats[0] = distance;
        return Ok(distance as usize);
    }

    let index = raw as usize - 1 + usize::from(literal_length == 0);

    let distance = if index == 3 { repeats[0].saturating_sub(1) } else { repeats[index] };
    if distance == 0 {
        return Err(Error::malformed("zstd sequence repeats an offset of zero"));
    }

    match index {
        0 => {}
        1 => repeats.swap(0, 1),
        _ => {
            let moved = distance;
            repeats[2] = repeats[1];
            repeats[1] = repeats[0];
            repeats[0] = moved;
        }
    }

    Ok(distance as usize)
}

struct Piece {
    literal_length: usize,
    distance: usize,
    match_length: usize,
}

fn emit(out: &mut Window, literals: &[u8], literal_pos: &mut usize, piece: &Piece, window_size: u64) -> Result<()> {
    let Piece { literal_length, distance, match_length } = *piece;
    let end = literal_pos.checked_add(literal_length).filter(|&e| e <= literals.len());
    let end = end.ok_or_else(|| Error::malformed("zstd sequence wants more literals than the block holds"))?;
    out.extend(&literals[*literal_pos..end]);
    *literal_pos = end;

    let produced = out.history();
    if distance > produced {
        return Err(Error::malformed(format!("zstd match reaches {distance} bytes back, past the {produced} produced in this frame")));
    }
    if window_size > 0 && distance as u64 > window_size {
        return Err(Error::malformed(format!("zstd match distance {distance} exceeds the declared window")));
    }

    out.copy_match(distance as u32, match_length as u32)
}
