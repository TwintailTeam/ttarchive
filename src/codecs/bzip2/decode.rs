use std::io::{self, Read};

use crate::codecs::bzip2::crc::{self, Crc};
use crate::utils::error::{Error, Result, Unsupported};

pub(crate) const BLOCK_MAGIC: u64 = 0x3141_5926_5359;
pub(crate) const STREAM_MAGIC: u64 = 0x1772_4538_5090;

const MAX_CODE_LEN: usize = 23;
const GROUP_SIZE: usize = 50;
const MAX_BLOCK: usize = 9 * 100_000;

const RUN_A: u16 = 0;
const RUN_B: u16 = 1;

const IN_BUF: usize = 64 * 1024;

struct BitReader<R> {
    inner: R,
    data: Box<[u8]>,
    pos: usize,
    filled: usize,
    buf: u64,
    count: u32,
}

impl<R: Read> BitReader<R> {
    fn new(inner: R) -> Self {
        BitReader { inner, data: vec![0u8; IN_BUF].into_boxed_slice(), pos: 0, filled: 0, buf: 0, count: 0 }
    }

    #[cold]
    fn refill(&mut self) -> Result<bool> {
        loop {
            match self.inner.read(&mut self.data) {
                Ok(0) => return Ok(false),
                Ok(n) => {
                    self.pos = 0;
                    self.filled = n;
                    return Ok(true);
                }
                Err(ref e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(Error::from(e)),
            }
        }
    }

    #[inline]
    fn bits(&mut self, n: u32) -> Result<u32> {
        while self.count < n {
            if self.pos == self.filled && !self.refill()? {
                return Err(Error::malformed("truncated bzip2 stream"));
            }
            self.buf = (self.buf << 8) | self.data[self.pos] as u64;
            self.pos += 1;
            self.count += 8;
        }
        self.count -= n;
        let value = (self.buf >> self.count) & ((1u64 << n) - 1);
        Ok(value as u32)
    }

    #[inline]
    fn bit(&mut self) -> Result<bool> {
        Ok(self.bits(1)? == 1)
    }

    fn magic(&mut self) -> Result<u64> {
        let high = self.bits(24)? as u64;
        let low = self.bits(24)? as u64;
        Ok((high << 24) | low)
    }

    fn align(&mut self) {
        self.count -= self.count % 8;
    }

    fn next_byte(&mut self) -> Result<Option<u8>> {
        if self.count >= 8 {
            self.count -= 8;
            return Ok(Some((self.buf >> self.count) as u8));
        }
        if self.pos == self.filled && !self.refill()? {
            return Ok(None);
        }
        let byte = self.data[self.pos];
        self.pos += 1;
        Ok(Some(byte))
    }
}

struct Table {
    min_len: u32,
    max_len: u32,
    limit: [i32; MAX_CODE_LEN + 2],
    base: [i32; MAX_CODE_LEN + 2],
    perm: Vec<u16>,
}

impl Table {
    fn new(lengths: &[u8]) -> Result<Self> {
        let min_len = lengths.iter().copied().min().unwrap_or(1) as u32;
        let max_len = lengths.iter().copied().max().unwrap_or(1) as u32;
        if min_len == 0 || max_len as usize > MAX_CODE_LEN {
            return Err(Error::malformed(format!("bzip2 huffman code lengths out of range: {min_len}..={max_len}")));
        }

        let mut perm = Vec::with_capacity(lengths.len());
        for len in min_len..=max_len {
            for (symbol, &l) in lengths.iter().enumerate() {
                if l as u32 == len {
                    perm.push(symbol as u16);
                }
            }
        }

        let mut base = [0i32; MAX_CODE_LEN + 2];
        for &l in lengths {
            base[l as usize + 1] += 1;
        }
        for i in 1..base.len() {
            base[i] += base[i - 1];
        }

        let mut limit = [0i32; MAX_CODE_LEN + 2];
        let mut vec = 0i32;
        for len in min_len as usize..=max_len as usize {
            vec += base[len + 1] - base[len];
            limit[len] = vec - 1;
            vec <<= 1;
        }
        for len in min_len as usize + 1..=max_len as usize {
            base[len] = ((limit[len - 1] + 1) << 1) - base[len];
        }

        Ok(Table { min_len, max_len, limit, base, perm })
    }

    fn decode<R: Read>(&self, reader: &mut BitReader<R>) -> Result<u16> {
        let mut len = self.min_len;
        let mut code = reader.bits(len)? as i32;
        while len <= self.max_len && code > self.limit[len as usize] {
            len += 1;
            code = (code << 1) | reader.bits(1)? as i32;
        }
        if len > self.max_len {
            return Err(Error::malformed("invalid bzip2 huffman code"));
        }
        let index = code - self.base[len as usize];
        self.perm.get(index as usize).copied().ok_or_else(|| Error::malformed("bzip2 huffman code decodes to no symbol"))
    }
}

struct GroupCursor {
    next_selector: usize,
    left: usize,
    table: usize,
}

impl GroupCursor {
    fn next<R: Read>(&mut self, reader: &mut BitReader<R>, tables: &[Table], selectors: &[u8]) -> Result<u16> {
        if self.left == 0 {
            let selector = *selectors.get(self.next_selector).ok_or_else(|| Error::malformed("bzip2 block ran out of huffman selectors"))?;
            if selector as usize >= tables.len() {
                return Err(Error::malformed("bzip2 selector names no table"));
            }
            self.table = selector as usize;
            self.next_selector += 1;
            self.left = GROUP_SIZE;
        }
        self.left -= 1;
        tables[self.table].decode(reader)
    }
}

pub struct Bzip2Reader<R> {
    reader: BitReader<R>,
    block_limit: usize,
    out: Vec<u8>,
    out_read: usize,
    stream_crc: u32,
    started: bool,
    done: bool,
    tt: Vec<u32>,
}

impl<R: Read> Bzip2Reader<R> {
    pub fn new(inner: R) -> Self {
        Bzip2Reader {
            reader: BitReader::new(inner),
            block_limit: MAX_BLOCK,
            out: Vec::new(),
            out_read: 0,
            stream_crc: 0,
            started: false,
            done: false,
            tt: Vec::new(),
        }
    }

    fn read_stream_header(&mut self, first: u8) -> Result<()> {
        let z = self.reader.bits(8)?;
        let h = self.reader.bits(8)?;
        if (first as u32, z, h) != (b'B' as u32, b'Z' as u32, b'h' as u32) {
            return Err(Error::malformed("not a bzip2 stream: missing BZh signature"));
        }

        let level = self.reader.bits(8)?;
        if !(b'1' as u32..=b'9' as u32).contains(&level) {
            return Err(Error::malformed(format!("bzip2 block size digit must be 1-9, found {:?}", char::from_u32(level).unwrap_or('?'))));
        }
        self.block_limit = (level - b'0' as u32) as usize * 100_000;
        self.tt = vec![0u32; self.block_limit];
        self.started = true;
        Ok(())
    }

    fn start_next_stream(&mut self) -> Result<bool> {
        self.reader.align();

        let first = loop {
            match self.reader.next_byte()? {
                None => return Ok(false),
                Some(0) => continue,
                Some(byte) => break byte,
            }
        };

        self.stream_crc = 0;
        self.read_stream_header(first)?;
        Ok(true)
    }

    fn next_block(&mut self) -> Result<()> {
        if !self.started {
            let first = self.reader.bits(8)? as u8;
            self.read_stream_header(first)?;
        }

        let magic = self.reader.magic()?;
        if magic == STREAM_MAGIC {
            let declared = self.reader.bits(32)?;
            if declared != self.stream_crc {
                return Err(Error::malformed(format!(
                    "bzip2 stream checksum mismatch: stream declares {declared:#010x}, \
                     blocks total {:#010x}",
                    self.stream_crc
                )));
            }
            self.done = !self.start_next_stream()?;
            return Ok(());
        }
        if magic != BLOCK_MAGIC {
            return Err(Error::malformed(format!("bad bzip2 block marker {magic:#014x}")));
        }

        let block_crc = self.reader.bits(32)?;

        if self.reader.bit()? {
            return Err(Error::Unsupported(Unsupported::Other("a bzip2 block using the deprecated randomisation of bzip2 0.9.0")));
        }

        let orig_ptr = self.reader.bits(24)? as usize;

        let (symbol_map, alpha_size) = self.read_symbol_map()?;
        let tables = self.read_tables(alpha_size)?;
        let selectors = tables.1;
        let tables = tables.0;

        let block_len = self.read_mtf_rle2(&tables, &selectors, &symbol_map, alpha_size)?;

        if orig_ptr >= block_len {
            return Err(Error::malformed(format!("bzip2 BWT origin pointer {orig_ptr} is outside the {block_len} byte block")));
        }

        let bwt = self.inverse_bwt(block_len, orig_ptr);
        self.out = decode_rle1(&bwt)?;
        self.out_read = 0;

        let mut crc = Crc::new();
        crc.update(&self.out);
        let found = crc.finish();
        if found != block_crc {
            return Err(Error::malformed(format!("bzip2 block checksum mismatch: expected {block_crc:#010x}, computed {found:#010x}")));
        }
        self.stream_crc = crc::combine(self.stream_crc, block_crc);

        Ok(())
    }

    fn read_symbol_map(&mut self) -> Result<(Vec<u8>, usize)> {
        let used_groups = self.reader.bits(16)?;
        let mut symbols = Vec::with_capacity(256);
        for group in 0..16 {
            if used_groups & (0x8000 >> group) == 0 {
                continue;
            }
            let bits = self.reader.bits(16)?;
            for bit in 0..16 {
                if bits & (0x8000 >> bit) != 0 {
                    symbols.push((group * 16 + bit) as u8);
                }
            }
        }

        if symbols.is_empty() {
            return Err(Error::malformed("bzip2 block uses no symbols at all"));
        }
        let alpha_size = symbols.len() + 2;
        Ok((symbols, alpha_size))
    }

    fn read_tables(&mut self, alpha_size: usize) -> Result<(Vec<Table>, Vec<u8>)> {
        let group_count = self.reader.bits(3)? as usize;
        if !(2..=6).contains(&group_count) {
            return Err(Error::malformed(format!("bzip2 block declares {group_count} huffman tables; 2 to 6 are legal")));
        }

        let selector_count = self.reader.bits(15)? as usize;
        if selector_count == 0 {
            return Err(Error::malformed("bzip2 block declares no huffman selectors"));
        }

        let mut order: Vec<u8> = (0..group_count as u8).collect();
        let mut selectors = Vec::with_capacity(selector_count);
        for _ in 0..selector_count {
            let mut j = 0usize;
            while self.reader.bit()? {
                j += 1;
                if j >= group_count {
                    return Err(Error::malformed("bzip2 selector index out of range"));
                }
            }
            let picked = order.remove(j);
            order.insert(0, picked);
            selectors.push(picked);
        }

        let mut tables = Vec::with_capacity(group_count);
        for _ in 0..group_count {
            let mut length = self.reader.bits(5)? as i32;
            let mut lengths = vec![0u8; alpha_size];
            for slot in lengths.iter_mut() {
                loop {
                    if !(1..=20).contains(&length) {
                        return Err(Error::malformed(format!("bzip2 huffman code length {length} outside 1..=20")));
                    }
                    if !self.reader.bit()? {
                        break;
                    }
                    length += if self.reader.bit()? { -1 } else { 1 };
                }
                *slot = length as u8;
            }
            tables.push(Table::new(&lengths)?);
        }

        Ok((tables, selectors))
    }

    fn read_mtf_rle2(&mut self, tables: &[Table], selectors: &[u8], symbol_map: &[u8], alpha_size: usize) -> Result<usize> {
        let end_of_block = (alpha_size - 1) as u16;
        let mut mtf: Vec<u8> = symbol_map.to_vec();

        let mut cursor = GroupCursor { next_selector: 0, left: 0, table: 0 };

        let mut length = 0usize;
        let mut symbol = cursor.next(&mut self.reader, tables, selectors)?;

        while symbol != end_of_block {
            if symbol == RUN_A || symbol == RUN_B {
                let mut run = 0i64;
                let mut place = 1i64;
                loop {
                    run += if symbol == RUN_A { place } else { 2 * place };
                    place *= 2;
                    if place > MAX_BLOCK as i64 * 2 {
                        return Err(Error::malformed("bzip2 zero run length overflows the block"));
                    }
                    symbol = cursor.next(&mut self.reader, tables, selectors)?;
                    if symbol != RUN_A && symbol != RUN_B {
                        break;
                    }
                }

                let byte = mtf[0];
                if length + run as usize > self.block_limit {
                    return Err(Error::malformed("bzip2 block is longer than its header allows"));
                }
                for _ in 0..run {
                    self.tt[length] = byte as u32;
                    length += 1;
                }
                continue;
            }

            let index = symbol as usize - 1;
            if index >= mtf.len() {
                return Err(Error::malformed("bzip2 move-to-front index out of range"));
            }
            let byte = mtf[index];
            mtf[..=index].rotate_right(1);

            if length >= self.block_limit {
                return Err(Error::malformed("bzip2 block is longer than its header allows"));
            }
            self.tt[length] = byte as u32;
            length += 1;

            symbol = cursor.next(&mut self.reader, tables, selectors)?;
        }

        Ok(length)
    }

    fn inverse_bwt(&mut self, length: usize, orig_ptr: usize) -> Vec<u8> {
        let mut counts = [0u32; 256];
        for &value in &self.tt[..length] {
            counts[(value & 0xff) as usize] += 1;
        }

        let mut running = 0u32;
        let mut starts = [0u32; 256];
        for byte in 0..256 {
            starts[byte] = running;
            running += counts[byte];
        }

        for i in 0..length {
            let byte = (self.tt[i] & 0xff) as usize;
            let slot = starts[byte] as usize;
            starts[byte] += 1;
            self.tt[slot] |= (i as u32) << 8;
        }

        let mut out = Vec::with_capacity(length);
        let mut pos = self.tt[orig_ptr] >> 8;
        for _ in 0..length {
            let entry = self.tt[pos as usize];
            out.push((entry & 0xff) as u8);
            pos = entry >> 8;
        }
        out
    }
}

fn decode_rle1(input: &[u8]) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(input.len() * 2);
    let mut i = 0usize;

    while i < input.len() {
        let byte = input[i];
        let mut run = 1usize;
        while run < 4 && i + run < input.len() && input[i + run] == byte {
            run += 1;
        }

        if run < 4 {
            out.extend(std::iter::repeat_n(byte, run));
            i += run;
            continue;
        }

        let extra = *input.get(i + 4).ok_or_else(|| Error::malformed("bzip2 run-length escape has no count byte"))?;
        out.extend(std::iter::repeat_n(byte, 4 + extra as usize));
        i += 5;
    }

    Ok(out)
}

impl<R: Read> Read for Bzip2Reader<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }

        while self.out_read == self.out.len() {
            if self.done {
                return Ok(0);
            }
            self.next_block()?;
        }

        let n = (self.out.len() - self.out_read).min(buf.len());
        buf[..n].copy_from_slice(&self.out[self.out_read..self.out_read + n]);
        self.out_read += n;
        Ok(n)
    }
}

pub fn decompress(data: &[u8], size_hint: usize) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(size_hint);
    Bzip2Reader::new(data).read_to_end(&mut out)?;
    Ok(out)
}
