use crate::codecs::ppmd::range::RangeDecoder;
use std::io::Read;

const UNIT_SIZE: u32 = 12;
const INDEXES: usize = 4 + 4 + 4 + (128 + 3 - 4 - 8 - 12) / 4;
const MAX_FREQ: u32 = 124;
const EMPTY_NODE: u32 = 0xFFFF_FFFF;

const INT_BITS: u32 = 7;
const PERIOD_BITS: u32 = 7;
const BIN_SCALE: u32 = 1 << (INT_BITS + PERIOD_BITS);

const FLAG_RESCALED: u8 = 1 << 2;
const FLAG_PREV_HIGH: u8 = 1 << 4;

const EXP_ESCAPE: [u8; 16] = [25, 14, 9, 7, 5, 5, 4, 4, 4, 3, 3, 3, 2, 2, 2, 2];
const INIT_BIN_ESC: [u16; 8] = [0x3CDD, 0x1F3F, 0x59BF, 0x48F3, 0x64A1, 0x5ABC, 0x6632, 0x6051];

pub const MAX_ORDER: u32 = 16;

pub const RESTORE_RESTART: u32 = 0;
pub const RESTORE_CUT_OFF: u32 = 1;

pub const SYM_END: i32 = -1;
pub const SYM_ERROR: i32 = -2;

#[derive(Clone, Copy, Default)]
struct See {
    summ: u16,
    shift: u8,
    count: u8,
}

impl See {
    fn update(&mut self) {
        if self.shift < PERIOD_BITS as u8 {
            self.count -= 1;
            if self.count == 0 {
                self.summ = self.summ.wrapping_shl(1);
                self.count = 3u8.wrapping_shl(self.shift as u32);
                self.shift += 1;
            }
        }
    }

    fn mean(&mut self) -> u32 {
        let summ = self.summ as u32;
        let r = summ >> self.shift;
        self.summ = (summ - r) as u16;
        r + u32::from(r == 0)
    }
}

pub struct Ppmd8<R> {
    rc: RangeDecoder<R>,

    base: Vec<u8>,
    size: u32,
    align_offset: u32,

    min_context: u32,
    max_context: u32,
    found_state: u32,

    order_fall: u32,
    init_esc: u32,
    prev_success: u32,
    max_order: u32,
    restore_method: u32,
    run_length: i32,
    init_rl: i32,

    glue_count: u32,
    lo_unit: u32,
    hi_unit: u32,
    text: u32,
    units_start: u32,

    indx2units: [u8; INDEXES],
    units2indx: [u8; 128],
    free_list: [u32; INDEXES],
    stamps: [u32; INDEXES],

    ns2bs_indx: [u8; 256],
    ns2indx: [u8; 260],

    dummy_see: See,
    see: [[See; 32]; 24],
    bin_summ: [[u16; 64]; 25],
}

impl<R: Read> Ppmd8<R> {
    pub fn new(inner: R, mem_size: u32, max_order: u32, restore_method: u32) -> crate::Result<Self> {
        let align_offset = (4u32.wrapping_sub(mem_size)) & 3;

        let mut indx2units = [0u8; INDEXES];
        let mut units2indx = [0u8; 128];
        let mut k = 0usize;
        for (i, slot) in indx2units.iter_mut().enumerate() {
            let step = if i >= 12 { 4 } else { (i >> 2) + 1 };
            for _ in 0..step {
                units2indx[k] = i as u8;
                k += 1;
            }
            *slot = k as u8;
        }

        let mut ns2bs_indx = [0u8; 256];
        ns2bs_indx[0] = 0;
        ns2bs_indx[1] = 2;
        ns2bs_indx[2..11].fill(4);
        ns2bs_indx[11..].fill(6);

        let mut ns2indx = [0u8; 260];
        for (i, slot) in ns2indx.iter_mut().enumerate().take(5) {
            *slot = i as u8;
        }
        let mut m = 5usize;
        let mut k = 1usize;
        for slot in ns2indx.iter_mut().skip(5) {
            *slot = m as u8;
            k -= 1;
            if k == 0 {
                m += 1;
                k = m - 4;
            }
        }

        let mut model = Ppmd8 {
            rc: RangeDecoder::new(inner)?,
            base: vec![0u8; (align_offset + mem_size) as usize + UNIT_SIZE as usize],
            size: mem_size,
            align_offset,
            min_context: 0,
            max_context: 0,
            found_state: 0,
            order_fall: 0,
            init_esc: 0,
            prev_success: 0,
            max_order,
            restore_method,
            run_length: 0,
            init_rl: 0,
            glue_count: 0,
            lo_unit: 0,
            hi_unit: 0,
            text: 0,
            units_start: 0,
            indx2units,
            units2indx,
            free_list: [0; INDEXES],
            stamps: [0; INDEXES],
            ns2bs_indx,
            ns2indx,
            dummy_see: See::default(),
            see: [[See::default(); 32]; 24],
            bin_summ: [[0u16; 64]; 25],
        };

        model.restart();
        Ok(model)
    }

    #[inline]
    fn u8_at(&self, at: u32) -> u8 {
        self.base[at as usize]
    }
    #[inline]
    fn set_u8(&mut self, at: u32, v: u8) {
        self.base[at as usize] = v;
    }
    #[inline]
    fn u16_at(&self, at: u32) -> u16 {
        u16::from_le_bytes([self.base[at as usize], self.base[at as usize + 1]])
    }
    #[inline]
    fn set_u16(&mut self, at: u32, v: u16) {
        self.base[at as usize..at as usize + 2].copy_from_slice(&v.to_le_bytes());
    }
    #[inline]
    fn u32_at(&self, at: u32) -> u32 {
        u32::from_le_bytes(self.base[at as usize..at as usize + 4].try_into().expect("four bytes"))
    }
    #[inline]
    fn set_u32(&mut self, at: u32, v: u32) {
        self.base[at as usize..at as usize + 4].copy_from_slice(&v.to_le_bytes());
    }

    #[inline]
    fn num_stats(&self, c: u32) -> u32 {
        self.u8_at(c) as u32
    }
    #[inline]
    fn set_num_stats(&mut self, c: u32, v: u32) {
        self.set_u8(c, v as u8);
    }
    #[inline]
    fn flags(&self, c: u32) -> u8 {
        self.u8_at(c + 1)
    }
    #[inline]
    fn set_flags(&mut self, c: u32, v: u8) {
        self.set_u8(c + 1, v);
    }
    #[inline]
    fn summ_freq(&self, c: u32) -> u32 {
        self.u16_at(c + 2) as u32
    }
    #[inline]
    fn set_summ_freq(&mut self, c: u32, v: u32) {
        self.set_u16(c + 2, v as u16);
    }
    #[inline]
    fn stats(&self, c: u32) -> u32 {
        self.u32_at(c + 4)
    }
    #[inline]
    fn set_stats(&mut self, c: u32, v: u32) {
        self.set_u32(c + 4, v);
    }
    #[inline]
    fn suffix(&self, c: u32) -> u32 {
        self.u32_at(c + 8)
    }
    #[inline]
    fn set_suffix(&mut self, c: u32, v: u32) {
        self.set_u32(c + 8, v);
    }
    #[inline]
    fn one_state(&self, c: u32) -> u32 {
        c + 2
    }

    #[inline]
    fn sym(&self, s: u32) -> u8 {
        self.u8_at(s)
    }
    #[inline]
    fn set_sym(&mut self, s: u32, v: u8) {
        self.set_u8(s, v);
    }
    #[inline]
    fn freq(&self, s: u32) -> u32 {
        self.u8_at(s + 1) as u32
    }
    #[inline]
    fn set_freq(&mut self, s: u32, v: u32) {
        self.set_u8(s + 1, v as u8);
    }
    #[inline]
    fn successor(&self, s: u32) -> u32 {
        self.u16_at(s + 2) as u32 | ((self.u16_at(s + 4) as u32) << 16)
    }
    #[inline]
    fn set_successor(&mut self, s: u32, v: u32) {
        self.set_u16(s + 2, v as u16);
        self.set_u16(s + 4, (v >> 16) as u16);
    }

    #[inline]
    fn copy_state(&mut self, dest: u32, src: u32) {
        let bytes: [u8; 6] = self.base[src as usize..src as usize + 6].try_into().expect("six bytes");
        self.base[dest as usize..dest as usize + 6].copy_from_slice(&bytes);
    }

    #[inline]
    fn swap_states(&mut self, a: u32, b: u32) {
        for i in 0..6 {
            self.base.swap((a + i) as usize, (b + i) as usize);
        }
    }

    fn copy_units(&mut self, dest: u32, src: u32, nu: u32) {
        let bytes = (nu * UNIT_SIZE) as usize;
        self.base.copy_within(src as usize..src as usize + bytes, dest as usize);
    }

    #[inline]
    fn units_of(&self, index: usize) -> u32 {
        self.indx2units[index] as u32
    }
    #[inline]
    fn index_of(&self, nu: u32) -> usize {
        self.units2indx[nu as usize - 1] as usize
    }

    fn insert_node(&mut self, node: u32, index: usize) {
        self.set_u32(node, EMPTY_NODE);
        self.set_u32(node + 4, self.free_list[index]);
        let nu = self.units_of(index);
        self.set_u32(node + 8, nu);
        self.free_list[index] = node;
        self.stamps[index] += 1;
    }

    fn remove_node(&mut self, index: usize) -> u32 {
        let node = self.free_list[index];
        self.free_list[index] = self.u32_at(node + 4);
        self.stamps[index] -= 1;
        node
    }

    fn split_block(&mut self, mut ptr: u32, old_index: usize, new_index: usize) {
        let nu = self.units_of(old_index) - self.units_of(new_index);
        ptr += self.units_of(new_index) * UNIT_SIZE;
        let mut i = self.index_of(nu);
        if self.units_of(i) != nu {
            i -= 1;
            let k = self.units_of(i);
            self.insert_node(ptr + k * UNIT_SIZE, (nu - k - 1) as usize);
        }
        self.insert_node(ptr, i);
    }

    fn glue_free_blocks(&mut self) {
        self.glue_count = 1 << 13;
        self.stamps = [0; INDEXES];

        if self.lo_unit != self.hi_unit {
            self.set_u32(self.lo_unit, 0);
        }

        let mut head = 0u32;
        let mut prev_slot: Option<u32> = None;

        for i in 0..INDEXES {
            let mut next = self.free_list[i];
            self.free_list[i] = 0;
            while next != 0 {
                let node = next;
                let mut nu = self.u32_at(node + 8);
                match prev_slot {
                    None => head = node,
                    Some(slot) => self.set_u32(slot, node),
                }
                next = self.u32_at(node + 4);
                if nu != 0 {
                    prev_slot = Some(node + 4);
                    loop {
                        let node2 = node + nu * UNIT_SIZE;
                        if self.u32_at(node2) != EMPTY_NODE {
                            break;
                        }
                        nu += self.u32_at(node2 + 8);
                        self.set_u32(node2 + 8, 0);
                        self.set_u32(node + 8, nu);
                    }
                }
            }
        }
        match prev_slot {
            None => head = 0,
            Some(slot) => self.set_u32(slot, 0),
        }

        let mut n = head;
        while n != 0 {
            let node = n;
            let mut nu = self.u32_at(node + 8);
            n = self.u32_at(node + 4);
            if nu == 0 {
                continue;
            }
            let mut at = node;
            while nu > 128 {
                self.insert_node(at, INDEXES - 1);
                nu -= 128;
                at += 128 * UNIT_SIZE;
            }
            let mut i = self.index_of(nu);
            if self.units_of(i) != nu {
                i -= 1;
                let k = self.units_of(i);
                self.insert_node(at + k * UNIT_SIZE, (nu - k - 1) as usize);
            }
            self.insert_node(at, i);
        }
    }

    fn alloc_units_rare(&mut self, index: usize) -> u32 {
        if self.glue_count == 0 {
            self.glue_free_blocks();
            if self.free_list[index] != 0 {
                return self.remove_node(index);
            }
        }

        let mut i = index;
        loop {
            i += 1;
            if i == INDEXES {
                let num_bytes = self.units_of(index) * UNIT_SIZE;
                self.glue_count -= 1;
                return if self.units_start - self.text > num_bytes {
                    self.units_start -= num_bytes;
                    self.units_start
                } else {
                    0
                };
            }
            if self.free_list[i] != 0 {
                break;
            }
        }

        let block = self.remove_node(i);
        self.split_block(block, i, index);
        block
    }

    fn alloc_units(&mut self, index: usize) -> u32 {
        if self.free_list[index] != 0 {
            return self.remove_node(index);
        }
        let num_bytes = self.units_of(index) * UNIT_SIZE;
        if self.hi_unit - self.lo_unit >= num_bytes {
            let at = self.lo_unit;
            self.lo_unit += num_bytes;
            return at;
        }
        self.alloc_units_rare(index)
    }

    fn shrink_units(&mut self, old: u32, old_nu: u32, new_nu: u32) -> u32 {
        let i0 = self.index_of(old_nu);
        let i1 = self.index_of(new_nu);
        if i0 == i1 {
            return old;
        }
        if self.free_list[i1] != 0 {
            let ptr = self.remove_node(i1);
            self.copy_units(ptr, old, new_nu);
            self.insert_node(old, i0);
            ptr
        } else {
            self.split_block(old, i0, i1);
            old
        }
    }

    fn free_units(&mut self, ptr: u32, nu: u32) {
        let index = self.index_of(nu);
        self.insert_node(ptr, index);
    }

    fn special_free_unit(&mut self, ptr: u32) {
        if ptr != self.units_start {
            self.insert_node(ptr, 0);
        } else {
            self.units_start += UNIT_SIZE;
        }
    }

    fn expand_text_area(&mut self) {
        let mut count = [0u32; INDEXES];
        if self.lo_unit != self.hi_unit {
            self.set_u32(self.lo_unit, 0);
        }

        let mut node = self.units_start;
        while self.u32_at(node) == EMPTY_NODE {
            let nu = self.u32_at(node + 8);
            self.set_u32(node, 0);
            count[self.index_of(nu)] += 1;
            node += nu * UNIT_SIZE;
        }
        self.units_start = node;

        for (i, &total) in count.iter().enumerate() {
            let mut cnt = total;
            if cnt == 0 {
                continue;
            }
            self.stamps[i] -= cnt;
            let mut prev_is_head = true;
            let mut prev = 0u32;
            let mut n = self.free_list[i];
            loop {
                let node = n;
                n = self.u32_at(node + 4);
                if self.u32_at(node) != 0 {
                    prev_is_head = false;
                    prev = node + 4;
                    continue;
                }
                if prev_is_head {
                    self.free_list[i] = n;
                } else {
                    self.set_u32(prev, n);
                }
                cnt -= 1;
                if cnt == 0 {
                    break;
                }
            }
        }
    }

    fn used_memory(&self) -> u32 {
        let mut v = 0u32;
        for i in 0..INDEXES {
            v += self.stamps[i] * self.units_of(i);
        }
        self.size - (self.hi_unit - self.lo_unit) - (self.units_start - self.text) - v * UNIT_SIZE
    }

    fn restart(&mut self) {
        self.free_list = [0; INDEXES];
        self.stamps = [0; INDEXES];
        self.text = self.align_offset;
        self.hi_unit = self.text + self.size;
        self.units_start = self.hi_unit - self.size / 8 / UNIT_SIZE * 7 * UNIT_SIZE;
        self.lo_unit = self.units_start;
        self.glue_count = 0;

        self.order_fall = self.max_order;
        self.init_rl = -((if self.max_order < 12 { self.max_order } else { 12 }) as i32) - 1;
        self.run_length = self.init_rl;
        self.prev_success = 0;

        self.hi_unit -= UNIT_SIZE;
        let mc = self.hi_unit;
        let s = self.lo_unit;
        self.lo_unit += (256 / 2) * UNIT_SIZE;

        self.max_context = mc;
        self.min_context = mc;
        self.found_state = s;

        self.set_flags(mc, 0);
        self.set_num_stats(mc, 255);
        self.set_summ_freq(mc, 256 + 1);
        self.set_stats(mc, s);
        self.set_suffix(mc, 0);

        for i in 0..256u32 {
            let state = s + i * 6;
            self.set_sym(state, i as u8);
            self.set_freq(state, 1);
            self.set_successor(state, 0);
        }

        let mut i = 0usize;
        for m in 0..25usize {
            while self.ns2indx[i] as usize == m {
                i += 1;
            }
            for (k, &esc) in INIT_BIN_ESC.iter().enumerate() {
                let val = (BIN_SCALE - esc as u32 / (i as u32 + 1)) as u16;
                let mut r = 0usize;
                while r < 64 {
                    self.bin_summ[m][k + r] = val;
                    r += 8;
                }
            }
        }

        let mut i = 0usize;
        for m in 0..24usize {
            while self.ns2indx[i + 3] as usize == m + 3 {
                i += 1;
            }
            let summ = ((2 * i as u32 + 5) << (PERIOD_BITS - 4)) as u16;
            for k in 0..32usize {
                self.see[m][k] = See { summ, shift: (PERIOD_BITS - 4) as u8, count: 7 };
            }
        }

        self.dummy_see = See { summ: 0, shift: PERIOD_BITS as u8, count: 64 };
    }

    fn refresh(&mut self, ctx: u32, old_nu: u32, mut scale: u32) {
        let mut i = self.num_stats(ctx);
        let stats = self.stats(ctx);
        let s0 = self.shrink_units(stats, old_nu, (i + 2) >> 1);
        self.set_stats(ctx, s0);

        scale |= u32::from(self.summ_freq(ctx) >= (1 << 15));

        let mut s = s0;
        let mut flags = hi_bits_prepare(self.sym(s));
        let mut freq = self.freq(s);
        let mut esc_freq = self.summ_freq(ctx) - freq;
        freq = (freq + scale) >> scale;
        let mut sum_freq = freq;
        self.set_freq(s, freq);

        while i > 0 {
            s += 6;
            let mut freq = self.freq(s);
            esc_freq -= freq;
            freq = (freq + scale) >> scale;
            sum_freq += freq;
            self.set_freq(s, freq);
            flags |= hi_bits_prepare(self.sym(s));
            i -= 1;
        }

        self.set_summ_freq(ctx, sum_freq + ((esc_freq + scale) >> scale));
        let kept = self.flags(ctx) & (FLAG_PREV_HIGH + FLAG_RESCALED * scale as u8);
        self.set_flags(ctx, kept + hi_bits_convert_3(flags));
    }

    fn cut_off(&mut self, ctx: u32, order: u32) -> u32 {
        let mut ns = self.num_stats(ctx) as i32;

        if ns == 0 {
            let s = self.one_state(ctx);
            let mut successor = self.successor(s);
            if successor >= self.units_start {
                successor = if order < self.max_order { self.cut_off(successor, order + 1) } else { 0 };
                self.set_successor(s, successor);
                if successor != 0 || order <= 9 {
                    return ctx;
                }
            }
            self.special_free_unit(ctx);
            return 0;
        }

        let nu = (ns as u32 + 2) >> 1;
        let mut stats = self.stats(ctx);

        let indx = self.index_of(nu);
        if stats - self.units_start <= (1 << 14) && self.stats(ctx) <= self.free_list[indx] {
            let ptr = self.remove_node(indx);
            self.set_stats(ctx, ptr);
            self.copy_units(ptr, stats, nu);
            if stats != self.units_start {
                self.insert_node(stats, indx);
            } else {
                self.units_start += self.units_of(indx) * UNIT_SIZE;
            }
            stats = ptr;
        }

        let mut s = stats + ns as u32 * 6;
        loop {
            let successor = self.successor(s);
            if successor < self.units_start {
                let s2 = stats + ns as u32 * 6;
                ns -= 1;
                if order != 0 {
                    if s != s2 {
                        self.copy_state(s, s2);
                    }
                } else {
                    self.swap_states(s, s2);
                    self.set_successor(s2, 0);
                }
            } else if order < self.max_order {
                let cut = self.cut_off(successor, order + 1);
                self.set_successor(s, cut);
            } else {
                self.set_successor(s, 0);
            }

            if s == stats {
                break;
            }
            s -= 6;
        }

        if ns != self.num_stats(ctx) as i32 && order != 0 {
            if ns < 0 {
                self.free_units(stats, nu);
                self.special_free_unit(ctx);
                return 0;
            }
            self.set_num_stats(ctx, ns as u32);
            if ns == 0 {
                let symbol = self.sym(stats);
                let flags = (self.flags(ctx) & FLAG_PREV_HIGH) + hi_bits_flag_3(symbol);
                self.set_flags(ctx, flags);
                let freq = (self.freq(stats) + 11) >> 3;
                self.set_u8(ctx + 2, symbol);
                self.set_u8(ctx + 3, freq as u8);
                let low = self.u16_at(stats + 2);
                let high = self.u16_at(stats + 4);
                self.set_u16(ctx + 4, low);
                self.set_u16(ctx + 6, high);
                self.free_units(stats, nu);
            } else {
                let scale = u32::from(self.summ_freq(ctx) > 16 * ns as u32);
                self.refresh(ctx, nu, scale);
            }
        }

        ctx
    }

    fn restore_model(&mut self, ctx_error: u32) {
        self.text = self.align_offset;

        let mut c = self.max_context;
        while c != ctx_error {
            let ns = self.num_stats(c);
            self.set_num_stats(c, ns - 1);
            if ns - 1 == 0 {
                let s = self.stats(c);
                let symbol = self.sym(s);
                let flags = (self.flags(c) & FLAG_PREV_HIGH) + hi_bits_flag_3(symbol);
                self.set_flags(c, flags);
                let freq = (self.freq(s) + 11) >> 3;
                self.set_u8(c + 2, symbol);
                self.set_u8(c + 3, freq as u8);
                let low = self.u16_at(s + 2);
                let high = self.u16_at(s + 4);
                self.set_u16(c + 4, low);
                self.set_u16(c + 6, high);
                self.special_free_unit(s);
            } else {
                let nu = (self.num_stats(c) + 3) >> 1;
                self.refresh(c, nu, 0);
            }
            c = self.suffix(c);
        }

        while c != self.min_context {
            if self.num_stats(c) == 0 {
                let freq = (self.u8_at(c + 3) as u32 + 1) >> 1;
                self.set_u8(c + 3, freq as u8);
            } else {
                let sum = self.summ_freq(c) + 4;
                self.set_summ_freq(c, sum);
                if sum > 128 + 4 * self.num_stats(c) {
                    let nu = (self.num_stats(c) + 2) >> 1;
                    self.refresh(c, nu, 1);
                }
            }
            c = self.suffix(c);
        }

        if self.restore_method == RESTORE_RESTART || self.used_memory() < (self.size >> 1) {
            self.restart();
        } else {
            while self.suffix(self.max_context) != 0 {
                self.max_context = self.suffix(self.max_context);
            }
            loop {
                self.cut_off(self.max_context, 0);
                self.expand_text_area();
                if self.used_memory() <= 3 * (self.size >> 2) {
                    break;
                }
            }
            self.glue_count = 0;
            self.order_fall = self.max_order;
        }

        self.min_context = self.max_context;
    }

    fn create_successors(&mut self, skip: bool, mut s1: u32, mut c: u32) -> u32 {
        let mut up_branch = self.successor(self.found_state);
        let mut ps = [0u32; MAX_ORDER as usize + 1];
        let mut num_ps = 0usize;

        if !skip {
            ps[num_ps] = self.found_state;
            num_ps += 1;
        }

        while self.suffix(c) != 0 {
            c = self.suffix(c);
            let s;
            if s1 != 0 {
                s = s1;
                s1 = 0;
            } else if self.num_stats(c) != 0 {
                let sym = self.sym(self.found_state);
                let mut t = self.stats(c);
                while self.sym(t) != sym {
                    t += 6;
                }
                if self.freq(t) < MAX_FREQ - 9 {
                    self.set_freq(t, self.freq(t) + 1);
                    self.set_summ_freq(c, self.summ_freq(c) + 1);
                }
                s = t;
            } else {
                let t = self.one_state(c);
                let bump = u32::from(self.num_stats(self.suffix(c)) == 0 && self.freq(t) < 24);
                self.set_freq(t, self.freq(t) + bump);
                s = t;
            }

            let successor = self.successor(s);
            if successor != up_branch {
                c = successor;
                if num_ps == 0 {
                    return c;
                }
                break;
            }
            ps[num_ps] = s;
            num_ps += 1;
            if num_ps >= ps.len() {
                return 0;
            }
        }

        let new_sym = self.u8_at(up_branch);
        up_branch += 1;
        let flags = hi_bits_flag_4(self.sym(self.found_state)) + hi_bits_flag_3(new_sym);

        let new_freq = if self.num_stats(c) == 0 {
            self.u8_at(c + 3) as u32
        } else {
            let mut s = self.stats(c);
            while self.sym(s) != new_sym {
                s += 6;
            }
            let cf = self.freq(s) - 1;
            let s0 = self.summ_freq(c) - self.num_stats(c) - cf;
            1 + if 2 * cf <= s0 { u32::from(5 * cf > s0) } else { (cf + 2 * s0 - 3) / s0 }
        };

        loop {
            let c1 = if self.hi_unit != self.lo_unit {
                self.hi_unit -= UNIT_SIZE;
                self.hi_unit
            } else if self.free_list[0] != 0 {
                self.remove_node(0)
            } else {
                let got = self.alloc_units_rare(0);
                if got == 0 {
                    return 0;
                }
                got
            };

            self.set_flags(c1, flags);
            self.set_num_stats(c1, 0);
            self.set_u8(c1 + 2, new_sym);
            self.set_u8(c1 + 3, new_freq as u8);
            let one = self.one_state(c1);
            self.set_successor(one, up_branch);
            self.set_suffix(c1, c);
            num_ps -= 1;
            self.set_successor(ps[num_ps], c1);
            c = c1;
            if num_ps == 0 {
                break;
            }
        }

        c
    }

    fn reduce_order(&mut self, mut s1: u32, c: u32) -> u32 {
        let up_branch = self.text;
        let c1 = c;
        let mut c = c;
        let mut s;

        self.set_successor(self.found_state, up_branch);
        self.order_fall += 1;

        loop {
            if s1 != 0 {
                c = self.suffix(c);
                s = s1;
                s1 = 0;
            } else {
                if self.suffix(c) == 0 {
                    return c;
                }
                c = self.suffix(c);
                if self.num_stats(c) != 0 {
                    let sym = self.sym(self.found_state);
                    let mut t = self.stats(c);
                    while self.sym(t) != sym {
                        t += 6;
                    }
                    if self.freq(t) < MAX_FREQ - 9 {
                        self.set_freq(t, self.freq(t) + 2);
                        self.set_summ_freq(c, self.summ_freq(c) + 2);
                    }
                    s = t;
                } else {
                    let t = self.one_state(c);
                    let bump = u32::from(self.freq(t) < 32);
                    self.set_freq(t, self.freq(t) + bump);
                    s = t;
                }
            }
            if self.successor(s) != 0 {
                break;
            }
            self.set_successor(s, up_branch);
            self.order_fall += 1;
        }

        if self.successor(s) <= up_branch {
            let s2 = self.found_state;
            self.found_state = s;
            let successor = self.create_successors(false, 0, c);
            self.set_successor(s, successor);
            self.found_state = s2;
        }

        let successor = self.successor(s);
        if self.order_fall == 1 && c1 == self.max_context {
            self.set_successor(self.found_state, successor);
            self.text -= 1;
        }
        successor
    }

    fn update_model(&mut self) {
        let mut min_successor = self.successor(self.found_state);
        let f_freq = self.freq(self.found_state);
        let f_symbol = self.sym(self.found_state);
        let mut s = 0u32;

        if self.freq(self.found_state) < MAX_FREQ / 4 && self.suffix(self.min_context) != 0 {
            let c = self.suffix(self.min_context);
            if self.num_stats(c) == 0 {
                let t = self.one_state(c);
                if self.freq(t) < 32 {
                    self.set_freq(t, self.freq(t) + 1);
                }
                s = t;
            } else {
                let sym = self.sym(self.found_state);
                let mut t = self.stats(c);
                if self.sym(t) != sym {
                    while self.sym(t) != sym {
                        t += 6;
                    }
                    if self.freq(t) >= self.freq(t - 6) {
                        self.swap_states(t, t - 6);
                        t -= 6;
                    }
                }
                if self.freq(t) < MAX_FREQ - 9 {
                    self.set_freq(t, self.freq(t) + 2);
                    self.set_summ_freq(c, self.summ_freq(c) + 2);
                }
                s = t;
            }
        }

        let mut c = self.max_context;
        if self.order_fall == 0 && min_successor != 0 {
            let cs = self.create_successors(true, s, self.min_context);
            if cs == 0 {
                self.set_successor(self.found_state, 0);
                self.restore_model(c);
                return;
            }
            self.set_successor(self.found_state, cs);
            self.min_context = cs;
            self.max_context = cs;
            return;
        }

        self.set_u8(self.text, f_symbol);
        self.text += 1;
        if self.text >= self.units_start {
            self.restore_model(c);
            return;
        }
        let mut max_successor = self.text;

        if min_successor == 0 {
            let cs = self.reduce_order(s, self.min_context);
            if cs == 0 {
                self.restore_model(c);
                return;
            }
            min_successor = cs;
        } else if min_successor < self.units_start {
            let cs = self.create_successors(false, s, self.min_context);
            if cs == 0 {
                self.restore_model(c);
                return;
            }
            min_successor = cs;
        }

        self.order_fall -= 1;
        if self.order_fall == 0 {
            max_successor = min_successor;
            self.text -= u32::from(self.max_context != self.min_context);
        }

        let flag = hi_bits_flag_3(f_symbol);
        let ns = self.num_stats(self.min_context);
        let s0 = self.summ_freq(self.min_context) - ns - f_freq;

        while c != self.min_context {
            let ns1 = self.num_stats(c);
            let sum;

            if ns1 != 0 {
                if ns1 & 1 != 0 {
                    let old_nu = (ns1 + 1) >> 1;
                    let i = self.index_of(old_nu);
                    if i != self.index_of(old_nu + 1) {
                        let ptr = self.alloc_units(i + 1);
                        if ptr == 0 {
                            self.restore_model(c);
                            return;
                        }
                        let old_ptr = self.stats(c);
                        self.copy_units(ptr, old_ptr, old_nu);
                        self.insert_node(old_ptr, i);
                        self.set_stats(c, ptr);
                    }
                }
                sum = self.summ_freq(c) + u32::from(3 * ns1 + 1 < ns);
            } else {
                let ptr = self.alloc_units(0);
                if ptr == 0 {
                    self.restore_model(c);
                    return;
                }
                let symbol = self.u8_at(c + 2);
                let low = self.u16_at(c + 4);
                let high = self.u16_at(c + 6);
                self.set_sym(ptr, symbol);
                self.set_u16(ptr + 2, low);
                self.set_u16(ptr + 4, high);
                self.set_stats(c, ptr);

                let mut freq = self.u8_at(c + 3) as u32;
                if freq < MAX_FREQ / 4 - 1 {
                    freq <<= 1;
                } else {
                    freq = MAX_FREQ - 4;
                }
                self.set_freq(ptr, freq);
                sum = freq + self.init_esc + u32::from(ns > 2);
            }

            let s = self.stats(c) + (ns1 + 1) * 6;
            let cf = 2 * (sum + 6) * f_freq;
            let sf = s0 + sum;
            self.set_sym(s, f_symbol);
            self.set_num_stats(c, ns1 + 1);
            self.set_successor(s, max_successor);
            self.set_flags(c, self.flags(c) | flag);

            let (new_sum, freq) = if cf < 6 * sf {
                let cf = 1 + u32::from(cf > sf) + u32::from(cf >= 4 * sf);
                (sum + 4, cf)
            } else {
                let cf = 4 + u32::from(cf > 9 * sf) + u32::from(cf > 12 * sf) + u32::from(cf > 15 * sf);
                (sum + cf, cf)
            };
            self.set_summ_freq(c, new_sum);
            self.set_freq(s, freq);

            c = self.suffix(c);
        }

        self.max_context = min_successor;
        self.min_context = min_successor;
    }

    fn rescale(&mut self) {
        let stats = self.stats(self.min_context);
        let mut s = self.found_state;

        if s != stats {
            let mut tmp = [0u8; 6];
            tmp.copy_from_slice(&self.base[s as usize..s as usize + 6]);
            while s != stats {
                self.copy_state(s, s - 6);
                s -= 6;
            }
            self.base[s as usize..s as usize + 6].copy_from_slice(&tmp);
        }

        let mut esc_freq = self.summ_freq(self.min_context) - self.freq(s);
        let adder = u32::from(self.order_fall != 0);
        let mut sum_freq = (self.freq(s) + 4 + adder) >> 1;
        self.set_freq(s, sum_freq);

        let mut i = self.num_stats(self.min_context);
        while i > 0 {
            s += 6;
            let mut freq = self.freq(s);
            esc_freq -= freq;
            freq = (freq + adder) >> 1;
            sum_freq += freq;
            self.set_freq(s, freq);

            if freq > self.freq(s - 6) {
                let mut tmp = [0u8; 6];
                tmp.copy_from_slice(&self.base[s as usize..s as usize + 6]);
                let mut s1 = s;
                loop {
                    self.copy_state(s1, s1 - 6);
                    s1 -= 6;
                    if s1 == stats || freq <= self.freq(s1 - 6) {
                        break;
                    }
                }
                self.base[s1 as usize..s1 as usize + 6].copy_from_slice(&tmp);
            }
            i -= 1;
        }

        if self.freq(s) == 0 {
            let mut i = 0u32;
            loop {
                i += 1;
                s -= 6;
                if self.freq(s) != 0 {
                    break;
                }
            }

            esc_freq += i;
            let mc = self.min_context;
            let num_stats = self.num_stats(mc);
            let num_stats_new = num_stats - i;
            self.set_num_stats(mc, num_stats_new);
            let n0 = (num_stats + 2) >> 1;

            if num_stats_new == 0 {
                let mut freq = (2 * self.freq(stats)).div_ceil(esc_freq);
                if freq > MAX_FREQ / 3 {
                    freq = MAX_FREQ / 3;
                }
                let symbol = self.sym(stats);
                let flags = (self.flags(mc) & FLAG_PREV_HIGH) + hi_bits_flag_3(symbol);
                self.set_flags(mc, flags);

                let one = self.one_state(mc);
                self.copy_state(one, stats);
                self.set_freq(one, freq);
                self.found_state = one;
                let index = self.index_of(n0);
                self.insert_node(stats, index);
                return;
            }

            let n1 = (num_stats_new + 2) >> 1;
            if n0 != n1 {
                let shrunk = self.shrink_units(stats, n0, n1);
                self.set_stats(mc, shrunk);
            }
        }

        let mc = self.min_context;
        self.set_summ_freq(mc, sum_freq + esc_freq - (esc_freq >> 1));
        self.set_flags(mc, self.flags(mc) | FLAG_RESCALED);
        self.found_state = self.stats(mc);
    }

    fn make_esc_freq(&mut self, num_masked: u32) -> (usize, usize, u32) {
        let mc = self.min_context;
        let num_stats = self.num_stats(mc);

        if num_stats != 0xFF {
            let row = self.ns2indx[num_stats as usize + 2] as usize - 3;
            let column = u32::from(self.summ_freq(mc) > 11 * (num_stats + 1))
                + 2 * u32::from(2 * num_stats < self.num_stats(self.suffix(mc)) + num_masked)
                + self.flags(mc) as u32;
            let column = column as usize;
            let esc = self.see[row][column].mean();
            (row, column, esc)
        } else {
            (usize::MAX, 0, 1)
        }
    }

    fn see_update(&mut self, row: usize, column: usize) {
        if row != usize::MAX {
            self.see[row][column].update();
        }
    }

    fn see_add(&mut self, row: usize, column: usize, value: u32) {
        if row != usize::MAX {
            let see = &mut self.see[row][column];
            see.summ = see.summ.wrapping_add(value as u16);
        }
    }

    fn next_context(&mut self) {
        let c = self.successor(self.found_state);
        if self.order_fall == 0 && c >= self.units_start {
            self.min_context = c;
            self.max_context = c;
        } else {
            self.update_model();
        }
    }

    fn update1(&mut self) {
        let s = self.found_state;
        let freq = self.freq(s) + 4;
        self.set_summ_freq(self.min_context, self.summ_freq(self.min_context) + 4);
        self.set_freq(s, freq);
        if freq > self.freq(s - 6) {
            self.swap_states(s, s - 6);
            self.found_state = s - 6;
            if freq > MAX_FREQ {
                self.rescale();
            }
        }
        self.next_context();
    }

    fn update1_0(&mut self) {
        let s = self.found_state;
        let mc = self.min_context;
        let freq = self.freq(s);
        let summ_freq = self.summ_freq(mc);
        self.prev_success = u32::from(2 * freq >= summ_freq);
        self.run_length += self.prev_success as i32;
        self.set_summ_freq(mc, summ_freq + 4);
        let freq = freq + 4;
        self.set_freq(s, freq);
        if freq > MAX_FREQ {
            self.rescale();
        }
        self.next_context();
    }

    fn update2(&mut self) {
        let s = self.found_state;
        let freq = self.freq(s) + 4;
        self.run_length = self.init_rl;
        self.set_summ_freq(self.min_context, self.summ_freq(self.min_context) + 4);
        self.set_freq(s, freq);
        if freq > MAX_FREQ {
            self.rescale();
        }
        self.update_model();
    }

    fn bin_summ_index(&self) -> (usize, usize) {
        let one = self.one_state(self.min_context);
        let row = self.ns2indx[self.freq(one) as usize - 1] as usize;
        let column = self.prev_success as usize
            + ((self.run_length >> 26) & 0x20) as usize
            + self.ns2bs_indx[self.num_stats(self.suffix(self.min_context)) as usize] as usize
            + self.flags(self.min_context) as usize;
        (row, column)
    }

    pub fn decode_symbol(&mut self) -> i32 {
        let mut char_mask = [0u8; 256];

        if self.num_stats(self.min_context) != 0 {
            let mut s = self.stats(self.min_context);
            let mut summ_freq = self.summ_freq(self.min_context);
            self.rc.correct_sum_range(&mut summ_freq);

            let mut count = self.rc.threshold(summ_freq);
            let hi_cnt = count;

            count = count.wrapping_sub(self.freq(s));
            if (count as i32) < 0 {
                let freq = self.freq(s);
                self.rc.decode(0, freq);
                self.rc.normalize_remote();
                self.found_state = s;
                let sym = self.sym(s);
                self.update1_0();
                return sym as i32;
            }

            self.prev_success = 0;
            let mut i = self.num_stats(self.min_context);
            let mut found = false;
            while i > 0 {
                s += 6;
                count = count.wrapping_sub(self.freq(s));
                if (count as i32) < 0 {
                    found = true;
                    break;
                }
                i -= 1;
            }

            if found {
                let freq = self.freq(s);
                self.rc.decode(hi_cnt.wrapping_sub(count) - freq, freq);
                self.rc.normalize_remote();
                self.found_state = s;
                let sym = self.sym(s);
                self.update1();
                return sym as i32;
            }

            if hi_cnt >= summ_freq {
                return SYM_ERROR;
            }
            let hi_cnt = hi_cnt.wrapping_sub(count);
            self.rc.decode(hi_cnt, summ_freq - hi_cnt);

            char_mask.fill(0xFF);
            let mut s2 = self.stats(self.min_context);
            char_mask[self.sym(s) as usize] = 0;
            while s2 < s {
                char_mask[self.sym(s2) as usize] = 0;
                s2 += 6;
            }
        } else {
            let s = self.one_state(self.min_context);
            let (row, column) = self.bin_summ_index();
            let pr = self.bin_summ[row][column] as u32;
            let size0 = (self.rc.range() >> 14) * pr;
            let pr_updated = pr - mean(pr);

            if self.rc.code() < size0 {
                self.bin_summ[row][column] = (pr_updated + (1 << INT_BITS)) as u16;
                self.rc.set_range(size0);
                self.rc.normalize();

                let freq = self.freq(s);
                let c = self.successor(s);
                let sym = self.sym(s);
                self.found_state = s;
                self.prev_success = 1;
                self.run_length += 1;
                self.set_freq(s, freq + u32::from(freq < 196));
                if self.order_fall == 0 && c >= self.units_start {
                    self.min_context = c;
                    self.max_context = c;
                } else {
                    self.update_model();
                }
                return sym as i32;
            }

            self.bin_summ[row][column] = pr_updated as u16;
            self.init_esc = EXP_ESCAPE[(pr_updated >> 10) as usize] as u32;
            self.rc.decode_bit1(size0);

            char_mask.fill(0xFF);
            char_mask[self.sym(self.one_state(self.min_context)) as usize] = 0;
            self.prev_success = 0;
        }

        loop {
            self.rc.normalize_remote();
            let mut mc = self.min_context;
            let num_masked = self.num_stats(mc);

            loop {
                self.order_fall += 1;
                if self.suffix(mc) == 0 {
                    return SYM_END;
                }
                mc = self.suffix(mc);
                if self.num_stats(mc) != num_masked {
                    break;
                }
            }

            self.min_context = mc;
            let mut s = self.stats(mc);
            let mut hi_cnt = 0u32;
            let total = self.num_stats(mc) + 1;
            for _ in 0..total {
                hi_cnt += self.freq(s) & char_mask[self.sym(s) as usize] as u32;
                s += 6;
            }

            let (row, column, esc) = self.make_esc_freq(num_masked);
            let freq_sum = esc + hi_cnt;
            let mut freq_sum2 = freq_sum;
            self.rc.correct_sum_range(&mut freq_sum2);

            let count = self.rc.threshold(freq_sum2);

            if count < hi_cnt {
                let mut s = self.stats(self.min_context);
                let mut acc = count;
                loop {
                    acc = acc.wrapping_sub(self.freq(s) & char_mask[self.sym(s) as usize] as u32);
                    s += 6;
                    if (acc as i32) < 0 {
                        break;
                    }
                }
                s -= 6;
                let freq = self.freq(s);
                self.rc.decode(count.wrapping_sub(acc) - freq, freq);
                self.rc.normalize_remote();

                self.see_update(row, column);
                self.found_state = s;
                let sym = self.sym(s);
                self.update2();
                return sym as i32;
            }

            if count >= freq_sum2 {
                return SYM_ERROR;
            }
            self.rc.decode(hi_cnt, freq_sum2 - hi_cnt);
            self.see_add(row, column, freq_sum);

            let mut s = self.stats(self.min_context);
            let end = s + (self.num_stats(self.min_context) + 1) * 6;
            while s != end {
                char_mask[self.sym(s) as usize] = 0;
                s += 6;
            }
        }
    }
}

#[inline]
fn hi_bits_prepare(sym: u8) -> u32 {
    sym as u32 + 0xC0
}

#[inline]
fn hi_bits_convert_3(flags: u32) -> u8 {
    ((flags >> 5) & (1 << 3)) as u8
}

#[inline]
fn hi_bits_flag_3(sym: u8) -> u8 {
    hi_bits_convert_3(hi_bits_prepare(sym))
}

#[inline]
fn hi_bits_flag_4(sym: u8) -> u8 {
    (((sym as u32 + 0xC0) >> 4) & (1 << 4)) as u8
}

#[inline]
fn mean(summ: u32) -> u32 {
    (summ + (1 << (PERIOD_BITS - 2))) >> PERIOD_BITS
}
