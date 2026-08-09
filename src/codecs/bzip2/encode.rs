use std::io::Write;

use crate::codecs::Encoder;
use crate::codecs::bzip2::crc::{self, Crc};
use crate::codecs::lengths::assign_lengths;
use crate::utils::error::{Error, Result};

const GROUP_SIZE: usize = 50;
const MIN_TABLES: usize = 2;
const MAX_TABLES: usize = 6;
const REFINE_ROUNDS: usize = 4;
const MAX_CODE_LEN: usize = 17;
const SEED_HIGH_COST: u8 = 15;

const RUN_A: u16 = 0;
const RUN_B: u16 = 1;

const MAX_RLE1_RUN: usize = 4 + 255;

const OUT_BUF: usize = 64 * 1024;

struct BitWriter<W> {
    inner: W,
    out: Vec<u8>,
    buf: u64,
    count: u32,
    written: u64,
}

impl<W: Write> BitWriter<W> {
    fn new(inner: W) -> Self {
        BitWriter { inner, out: Vec::with_capacity(OUT_BUF + 16), buf: 0, count: 0, written: 0 }
    }

    #[inline]
    fn bits(&mut self, value: u32, n: u32) -> Result<()> {
        if n == 0 {
            return Ok(());
        }
        self.buf = (self.buf << n) | (value as u64 & ((1u64 << n) - 1));
        self.count += n;
        while self.count >= 8 {
            self.count -= 8;
            self.out.push(((self.buf >> self.count) & 0xff) as u8);
            self.written += 1;
        }
        if self.out.len() >= OUT_BUF {
            self.spill()?;
        }
        Ok(())
    }

    #[cold]
    fn spill(&mut self) -> Result<()> {
        self.inner.write_all(&self.out)?;
        self.out.clear();
        Ok(())
    }

    #[inline]
    fn bit(&mut self, set: bool) -> Result<()> {
        self.bits(u32::from(set), 1)
    }

    fn magic(&mut self, value: u64) -> Result<()> {
        self.bits((value >> 24) as u32, 24)?;
        self.bits((value & 0xff_ffff) as u32, 24)
    }

    fn finish(&mut self) -> Result<u64> {
        if self.count > 0 {
            let pad = 8 - self.count;
            self.bits(0, pad)?;
        }
        self.spill()?;
        self.inner.flush()?;
        Ok(self.written)
    }
}

pub struct Bzip2Encoder<W: Write> {
    out: BitWriter<W>,
    level: u8,
    block_max: usize,
    block: Vec<u8>,
    run_byte: u8,
    run_len: usize,
    block_crc: Crc,
    stream_crc: u32,
    header_written: bool,
}

impl<W: Write> Bzip2Encoder<W> {
    pub fn new(out: W, level: u8) -> Self {
        let level = level.clamp(1, 9);
        let block_max = level as usize * 100_000 - 19;
        Bzip2Encoder {
            out: BitWriter::new(out),
            level,
            block_max,
            block: Vec::with_capacity(block_max),
            run_byte: 0,
            run_len: 0,
            block_crc: Crc::new(),
            stream_crc: 0,
            header_written: false,
        }
    }

    fn flush_run(&mut self) -> Result<()> {
        if self.run_len == 0 {
            return Ok(());
        }

        if self.block.len() + 5 > self.block_max {
            self.write_block()?;
        }

        self.block_crc.update_run(self.run_byte, self.run_len);

        let literals = self.run_len.min(4);
        for _ in 0..literals {
            self.block.push(self.run_byte);
        }
        if self.run_len >= 4 {
            self.block.push((self.run_len - 4) as u8);
        }
        self.run_len = 0;
        Ok(())
    }

    fn write_block(&mut self) -> Result<()> {
        if self.block.is_empty() {
            return Ok(());
        }

        if !self.header_written {
            self.out.bits(b'B' as u32, 8)?;
            self.out.bits(b'Z' as u32, 8)?;
            self.out.bits(b'h' as u32, 8)?;
            self.out.bits((b'0' + self.level) as u32, 8)?;
            self.header_written = true;
        }

        let block = std::mem::take(&mut self.block);

        let block_crc = std::mem::take(&mut self.block_crc).finish();
        self.stream_crc = crc::combine(self.stream_crc, block_crc);

        let (last_column, origin) = burrows_wheeler(&block);
        let (symbols, symbol_map) = move_to_front(&last_column);
        let alpha_size = symbol_map.len() + 2;

        self.out.magic(super::decode::BLOCK_MAGIC)?;
        self.out.bits(block_crc, 32)?;
        self.out.bit(false)?;
        self.out.bits(origin as u32, 24)?;
        self.write_symbol_map(&symbol_map)?;

        let (tables, selectors) = build_tables(&symbols, alpha_size);
        self.write_tables(&tables, &selectors, alpha_size)?;
        self.write_symbols(&symbols, &tables, &selectors)?;

        self.block = Vec::with_capacity(self.block_max);
        Ok(())
    }

    fn write_symbol_map(&mut self, symbol_map: &[u8]) -> Result<()> {
        let mut present = [false; 256];
        for &byte in symbol_map {
            present[byte as usize] = true;
        }

        let mut groups = 0u32;
        for group in 0..16 {
            if present[group * 16..group * 16 + 16].iter().any(|&p| p) {
                groups |= 0x8000 >> group;
            }
        }
        self.out.bits(groups, 16)?;

        for group in 0..16 {
            if groups & (0x8000 >> group) == 0 {
                continue;
            }
            let mut bits = 0u32;
            for bit in 0..16 {
                if present[group * 16 + bit] {
                    bits |= 0x8000 >> bit;
                }
            }
            self.out.bits(bits, 16)?;
        }

        Ok(())
    }

    fn write_tables(&mut self, tables: &[Vec<u8>], selectors: &[u8], alpha_size: usize) -> Result<()> {
        self.out.bits(tables.len() as u32, 3)?;
        self.out.bits(selectors.len() as u32, 15)?;

        let mut order: Vec<u8> = (0..tables.len() as u8).collect();
        for &selector in selectors {
            let index = order.iter().position(|&t| t == selector).expect("selector names a table");
            for _ in 0..index {
                self.out.bit(true)?;
            }
            self.out.bit(false)?;
            let picked = order.remove(index);
            order.insert(0, picked);
        }

        for table in tables {
            let mut current = table[0] as i32;
            self.out.bits(current as u32, 5)?;
            for &length in table.iter().take(alpha_size) {
                let want = length as i32;
                while current < want {
                    self.out.bit(true)?;
                    self.out.bit(false)?;
                    current += 1;
                }
                while current > want {
                    self.out.bit(true)?;
                    self.out.bit(true)?;
                    current -= 1;
                }
                self.out.bit(false)?;
            }
        }

        Ok(())
    }

    fn write_symbols(&mut self, symbols: &[u16], tables: &[Vec<u8>], selectors: &[u8]) -> Result<()> {
        let codes: Vec<Vec<u32>> = tables.iter().map(|t| canonical_codes(t)).collect();

        for (group, chunk) in symbols.chunks(GROUP_SIZE).enumerate() {
            let table = selectors[group] as usize;
            for &symbol in chunk {
                let length = tables[table][symbol as usize] as u32;
                self.out.bits(codes[table][symbol as usize], length)?;
            }
        }

        Ok(())
    }
}

impl<W: Write> Write for Bzip2Encoder<W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let mut at = 0usize;
        while at < buf.len() {
            let byte = buf[at];
            let mut run = 1usize;
            while at + run < buf.len() && buf[at + run] == byte {
                run += 1;
            }

            if self.run_len > 0 && self.run_byte == byte {
                let room = MAX_RLE1_RUN - self.run_len;
                let merged = run.min(room);
                self.run_len += merged;
                at += merged;
                if merged == run {
                    continue;
                }
                run -= merged;
            }

            while run > 0 {
                self.flush_run()?;
                let taken = run.min(MAX_RLE1_RUN);
                self.run_byte = byte;
                self.run_len = taken;
                at += taken;
                run -= taken;
            }
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<W: Write> Encoder for Bzip2Encoder<W> {
    fn finish(mut self: Box<Self>) -> Result<u64> {
        self.flush_run()?;
        self.write_block()?;

        if !self.header_written {
            self.out.bits(b'B' as u32, 8)?;
            self.out.bits(b'Z' as u32, 8)?;
            self.out.bits(b'h' as u32, 8)?;
            self.out.bits((b'0' + self.level) as u32, 8)?;
        }

        self.out.magic(super::decode::STREAM_MAGIC)?;
        let stream_crc = self.stream_crc;
        self.out.bits(stream_crc, 32)?;
        self.out.finish()
    }
}

const RADIX_BUCKETS: usize = 1 << 16;
const RADIX_MIN: usize = 1 << 16;

fn sort_by_key(items: &mut Vec<u64>, spare: &mut Vec<u64>, histogram: &mut [u32]) {
    if items.len() < RADIX_MIN {
        items.sort_unstable();
        return;
    }

    spare.clear();
    spare.resize(items.len(), 0);

    for shift in [32u32, 48] {
        histogram.fill(0);
        for &packed in items.iter() {
            histogram[((packed >> shift) & 0xffff) as usize] += 1;
        }

        let mut running = 0u32;
        for slot in histogram.iter_mut() {
            let count = *slot;
            *slot = running;
            running += count;
        }

        for &packed in items.iter() {
            let bucket = ((packed >> shift) & 0xffff) as usize;
            spare[histogram[bucket] as usize] = packed;
            histogram[bucket] += 1;
        }

        std::mem::swap(items, spare);
    }
}

pub fn burrows_wheeler(block: &[u8]) -> (Vec<u8>, usize) {
    let n = block.len();
    if n == 0 {
        return (Vec::new(), 0);
    }

    let mut order = vec![0u32; n];
    let mut counts = [0u32; 257];
    for &byte in block {
        counts[byte as usize + 1] += 1;
    }
    for i in 1..257 {
        counts[i] += counts[i - 1];
    }

    let starts = counts;
    for (index, &byte) in block.iter().enumerate() {
        order[counts[byte as usize] as usize] = index as u32;
        counts[byte as usize] += 1;
    }

    let mut rank = vec![0u32; n];
    for (index, &byte) in block.iter().enumerate() {
        rank[index] = starts[byte as usize];
    }

    let mut groups: Vec<(u32, u32)> = Vec::new();
    for byte in 0..256 {
        let (start, end) = (starts[byte], starts[byte + 1]);
        if end - start > 1 {
            groups.push((start, end));
        }
    }

    let mut next_rank = rank.clone();
    let mut next_groups: Vec<(u32, u32)> = Vec::new();
    let mut scratch: Vec<u64> = Vec::new();
    let mut spare: Vec<u64> = Vec::new();
    let mut histogram = vec![0u32; RADIX_BUCKETS];
    let mut shift = 1usize;

    while shift < n && !groups.is_empty() {
        next_rank.copy_from_slice(&rank);
        next_groups.clear();

        for &(start, end) in &groups {
            let (start, end) = (start as usize, end as usize);

            scratch.clear();
            for &index in &order[start..end] {
                let moved = index as usize + shift;
                let key = rank[if moved >= n { moved - n } else { moved }];
                scratch.push(((key as u64) << 32) | index as u64);
            }
            sort_by_key(&mut scratch, &mut spare, &mut histogram);

            for (slot, &packed) in order[start..end].iter_mut().zip(scratch.iter()) {
                *slot = packed as u32;
            }

            let mut run = start;
            while run < end {
                let key = scratch[run - start] >> 32;
                let mut past = run + 1;
                while past < end && scratch[past - start] >> 32 == key {
                    past += 1;
                }
                for &index in &order[run..past] {
                    next_rank[index as usize] = run as u32;
                }
                if past - run > 1 {
                    next_groups.push((run as u32, past as u32));
                }
                run = past;
            }
        }

        std::mem::swap(&mut rank, &mut next_rank);
        std::mem::swap(&mut groups, &mut next_groups);
        shift *= 2;
    }

    let mut last = vec![0u8; n];
    let mut origin = 0usize;
    for (row, &index) in order.iter().enumerate() {
        if index == 0 {
            origin = row;
        }
        last[row] = block[if index == 0 { n - 1 } else { index as usize - 1 }];
    }

    (last, origin)
}

fn move_to_front(last_column: &[u8]) -> (Vec<u16>, Vec<u8>) {
    let mut present = [false; 256];
    for &byte in last_column {
        present[byte as usize] = true;
    }
    let symbol_map: Vec<u8> = (0..256u16).filter(|&b| present[b as usize]).map(|b| b as u8).collect();

    let mut mtf = symbol_map.clone();
    let mut out = Vec::with_capacity(last_column.len() / 2 + 8);
    let mut zeros = 0usize;

    for &byte in last_column {
        let index = mtf.iter().position(|&m| m == byte).expect("byte is in the symbol map");
        if index == 0 {
            zeros += 1;
            continue;
        }
        flush_zero_run(&mut out, &mut zeros);

        mtf[..=index].rotate_right(1);
        out.push(index as u16 + 1);
    }

    flush_zero_run(&mut out, &mut zeros);
    out.push((symbol_map.len() + 1) as u16);

    (out, symbol_map)
}

fn flush_zero_run(out: &mut Vec<u16>, zeros: &mut usize) {
    if *zeros == 0 {
        return;
    }
    let mut remaining = *zeros - 1;
    loop {
        out.push(if remaining & 1 == 1 { RUN_B } else { RUN_A });
        if remaining < 2 {
            break;
        }
        remaining = (remaining - 2) / 2;
    }
    *zeros = 0;
}

fn build_tables(symbols: &[u16], alpha_size: usize) -> (Vec<Vec<u8>>, Vec<u8>) {
    let group_count = table_count(symbols.len());
    let groups = symbols.len().div_ceil(GROUP_SIZE);

    let mut frequencies = vec![0u32; alpha_size];
    for &symbol in symbols {
        frequencies[symbol as usize] += 1;
    }

    let mut tables = seed_tables(&frequencies, alpha_size, group_count);
    let mut selectors = vec![0u8; groups];

    for _ in 0..REFINE_ROUNDS {
        let mut per_table = vec![vec![0u32; alpha_size]; group_count];
        for (group, chunk) in symbols.chunks(GROUP_SIZE).enumerate() {
            let best = (0..group_count).min_by_key(|&t| chunk.iter().map(|&s| tables[t][s as usize] as u32).sum::<u32>()).unwrap_or(0);
            selectors[group] = best as u8;
            for &symbol in chunk {
                per_table[best][symbol as usize] += 1;
            }
        }

        for (table, counts) in tables.iter_mut().zip(per_table) {
            *table = complete_lengths(&counts);
        }
    }

    (tables, selectors)
}

fn table_count(symbols: usize) -> usize {
    let count = match symbols {
        0..200 => 2,
        200..600 => 3,
        600..1200 => 4,
        1200..2400 => 5,
        _ => 6,
    };
    count.clamp(MIN_TABLES, MAX_TABLES)
}

fn seed_tables(frequencies: &[u32], alpha_size: usize, group_count: usize) -> Vec<Vec<u8>> {
    let total: u64 = frequencies.iter().map(|&f| f as u64).sum();
    let mut tables = Vec::with_capacity(group_count);

    let mut start = 0usize;
    let mut remaining = total;
    for table in 0..group_count {
        let target = remaining / (group_count - table) as u64;
        let mut end = start;
        let mut accumulated = 0u64;
        while end < alpha_size && (accumulated < target || end == start) {
            accumulated += frequencies[end] as u64;
            end += 1;
        }

        let mut lengths = vec![SEED_HIGH_COST; alpha_size];
        for slot in lengths.iter_mut().take(end).skip(start) {
            *slot = 1;
        }
        tables.push(lengths);

        remaining = remaining.saturating_sub(accumulated);
        start = end.min(alpha_size);
    }

    tables
}

fn complete_lengths(frequencies: &[u32]) -> Vec<u8> {
    let padded: Vec<u32> = frequencies.iter().map(|&f| f.max(1)).collect();
    assign_lengths(&padded, MAX_CODE_LEN)
}

fn canonical_codes(lengths: &[u8]) -> Vec<u32> {
    let max = lengths.iter().copied().max().unwrap_or(0) as usize;
    let min = lengths.iter().copied().filter(|&l| l > 0).min().unwrap_or(0) as usize;

    let mut codes = vec![0u32; lengths.len()];
    let mut next = 0u32;
    for length in min..=max {
        for (symbol, &l) in lengths.iter().enumerate() {
            if l as usize == length {
                codes[symbol] = next;
                next += 1;
            }
        }
        next <<= 1;
    }
    codes
}

pub fn compress(data: &[u8], level: u8) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    let mut encoder = Box::new(Bzip2Encoder::new(&mut out, level));
    encoder.write_all(data).map_err(Error::from)?;
    encoder.finish()?;
    Ok(out)
}
