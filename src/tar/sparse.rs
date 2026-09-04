use crate::tar::pax::Attributes;
use crate::utils::error::{Error, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Segment {
    pub offset: u64,
    pub length: u64,
}

#[derive(Debug, Clone, Default)]
pub struct Map {
    pub segments: Vec<Segment>,
    pub real_size: u64,
    pub in_data: bool,
    pub stored: u64,
}

impl Map {
    pub fn is_empty(&self) -> bool {
        self.segments.is_empty()
    }

    pub fn stored_size(&self) -> u64 {
        self.segments.iter().map(|s| s.length).sum()
    }

    pub fn validate(&self) -> Result<()> {
        let mut reach = 0u64;
        for segment in &self.segments {
            if segment.offset < reach {
                return Err(Error::malformed("tar sparse map segments overlap or move backwards"));
            }
            reach = segment.offset.checked_add(segment.length).ok_or_else(|| Error::malformed("tar sparse segment overflows"))?;
            if reach > self.real_size {
                return Err(Error::malformed("tar sparse segment reaches past the declared size"));
            }
        }
        Ok(())
    }
}

pub fn from_pax(attributes: &Attributes) -> Result<Option<Map>> {
    if attributes.number("GNU.sparse.major") == Some(1) {
        let real_size = attributes.number("GNU.sparse.realsize").ok_or_else(|| Error::malformed("a PAX 1.0 sparse entry gives no real size"))?;
        return Ok(Some(Map { segments: Vec::new(), real_size, in_data: true, stored: 0 }));
    }

    let Some(real_size) = attributes.number("GNU.sparse.realsize").or_else(|| attributes.number("GNU.sparse.size")) else {
        return Ok(None);
    };

    if let Some(raw) = attributes.text("GNU.sparse.map") {
        let mut numbers = Vec::new();
        for part in raw.split(',') {
            if part.is_empty() {
                continue;
            }
            numbers.push(part.parse::<u64>().map_err(|_| Error::malformed("tar sparse map holds a non-numeric field"))?);
        }

        if numbers.len() % 2 != 0 {
            return Err(Error::malformed("tar sparse map has an odd number of fields"));
        }

        let segments = numbers.chunks(2).map(|pair| Segment { offset: pair[0], length: pair[1] }).collect();
        let map = Map { segments, real_size, in_data: false, stored: 0 };
        map.validate()?;
        return Ok(Some(map));
    }

    let offsets: Vec<u64> = numbers_of(attributes, "GNU.sparse.offset")?;
    let lengths: Vec<u64> = numbers_of(attributes, "GNU.sparse.numbytes")?;

    if offsets.len() != lengths.len() {
        return Err(Error::malformed("tar sparse records pair an offset with no length"));
    }

    let segments = offsets.into_iter().zip(lengths).map(|(offset, length)| Segment { offset, length }).collect();
    let map = Map { segments, real_size, in_data: false, stored: 0 };
    map.validate()?;
    Ok(Some(map))
}

fn numbers_of(attributes: &Attributes, key: &str) -> Result<Vec<u64>> {
    attributes
        .all(key)
        .map(|raw| {
            std::str::from_utf8(raw)
                .ok()
                .and_then(|text| text.trim().parse::<u64>().ok())
                .ok_or_else(|| Error::malformed(format!("tar sparse record {key} is not a number")))
        })
        .collect()
}

pub fn from_data(data: &[u8], real_size: u64) -> Result<(Map, usize)> {
    let mut at = 0usize;
    let mut next = || -> Result<u64> {
        let end = data[at..].iter().position(|&b| b == b'\n').ok_or_else(|| Error::malformed("a PAX 1.0 sparse map ends mid-number"))?;
        let text = std::str::from_utf8(&data[at..at + end]).map_err(|_| Error::malformed("a PAX 1.0 sparse map is not ascii"))?;
        let value = text.trim().parse::<u64>().map_err(|_| Error::malformed("a PAX 1.0 sparse map holds a non-numeric field"))?;
        at += end + 1;
        Ok(value)
    };

    let count = next()?;
    if count > (data.len() / 2) as u64 {
        return Err(Error::malformed(format!("a PAX 1.0 sparse map claims {count} segments, more than its own length allows")));
    }

    let mut segments = Vec::with_capacity(count as usize);
    for _ in 0..count {
        let offset = next()?;
        let length = next()?;
        segments.push(Segment { offset, length });
    }

    let map = Map { segments, real_size, in_data: false, stored: 0 };
    map.validate()?;

    let used = at.div_ceil(BLOCK) * BLOCK;
    Ok((map, used))
}

const BLOCK: usize = 512;

const MIN_HOLE: usize = BLOCK;

pub fn scan(data: &[u8]) -> Map {
    let mut segments: Vec<Segment> = Vec::new();
    let mut at = 0usize;

    while at < data.len() {
        while at < data.len() && data[at] == 0 && zeros_from(data, at) >= MIN_HOLE {
            at += zeros_from(data, at);
        }
        if at >= data.len() {
            break;
        }

        let start = at;
        while at < data.len() && !(data[at] == 0 && zeros_from(data, at) >= MIN_HOLE) {
            at += 1;
        }

        let from = start / BLOCK * BLOCK;
        let to = (at.div_ceil(BLOCK) * BLOCK).min(data.len());

        match segments.last_mut() {
            Some(last) if (last.offset + last.length) as usize >= from => *last = Segment { offset: last.offset, length: to as u64 - last.offset },
            _ => segments.push(Segment { offset: from as u64, length: (to - from) as u64 }),
        }
    }

    Map { segments, real_size: data.len() as u64, in_data: false, stored: 0 }
}

fn zeros_from(data: &[u8], at: usize) -> usize {
    data[at..].iter().take_while(|&&b| b == 0).count()
}

pub fn to_data(map: &Map) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(map.segments.len().to_string().as_bytes());
    out.push(b'\n');

    for segment in &map.segments {
        out.extend_from_slice(segment.offset.to_string().as_bytes());
        out.push(b'\n');
        out.extend_from_slice(segment.length.to_string().as_bytes());
        out.push(b'\n');
    }

    out.resize(out.len().div_ceil(BLOCK) * BLOCK, 0);
    out
}

pub fn gather(map: &Map, data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(map.stored_size() as usize);
    for segment in &map.segments {
        let start = segment.offset as usize;
        out.extend_from_slice(&data[start..start + segment.length as usize]);
    }
    out
}

pub fn worth_it(map: &Map, data: &[u8]) -> bool {
    let stored = map.stored_size() as usize + to_data(map).len();
    stored + BLOCK < data.len()
}

pub fn from_gnu_header(header: &[u8; BLOCK]) -> Result<(Map, bool)> {
    let real_size = crate::tar::header::parse_numeric(&header[483..495], "gnu sparse real size").unwrap_or(0);

    let mut segments = Vec::new();
    take_segments(&header[386..482], &mut segments)?;

    let map = Map { segments, real_size, in_data: false, stored: 0 };
    Ok((map, header[482] != 0))
}

pub fn from_gnu_extension(block: &[u8; BLOCK], map: &mut Map) -> Result<bool> {
    take_segments(&block[..504], &mut map.segments)?;
    Ok(block[504] != 0)
}

fn take_segments(area: &[u8], into: &mut Vec<Segment>) -> Result<()> {
    for pair in area.chunks_exact(24) {
        let offset = crate::tar::header::parse_numeric(&pair[..12], "gnu sparse offset").unwrap_or(0);
        let length = crate::tar::header::parse_numeric(&pair[12..24], "gnu sparse length").unwrap_or(0);
        if offset == 0 && length == 0 {
            break;
        }
        into.push(Segment { offset, length });
    }
    Ok(())
}

pub fn expand(map: &Map, stored: &[u8]) -> Result<Vec<u8>> {
    map.validate()?;

    let mut out = vec![0u8; map.real_size as usize];
    let mut at = 0usize;

    for segment in &map.segments {
        let length = segment.length as usize;
        if at + length > stored.len() {
            return Err(Error::malformed("tar sparse data is shorter than its map declares"));
        }
        let start = segment.offset as usize;
        out[start..start + length].copy_from_slice(&stored[at..at + length]);
        at += length;
    }

    Ok(out)
}
