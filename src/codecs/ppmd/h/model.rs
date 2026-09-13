use std::io::Read;

use crate::codecs::ppmd::h::range::RangeDecoder;

const UNIT_SIZE: u32 = 12;
const INDEXES: usize = 4 + 4 + 4 + (128 + 3 - 4 - 8 - 12) / 4;
const MAX_FREQ: u32 = 124;

const INT_BITS: u32 = 7;
const PERIOD_BITS: u32 = 7;
const BIN_SCALE: u32 = 1 << (INT_BITS + PERIOD_BITS);

const EXP_ESCAPE: [u8; 16] = [25, 14, 9, 7, 5, 5, 4, 4, 4, 3, 3, 3, 2, 2, 2, 2];
const INIT_BIN_ESC: [u16; 8] = [0x3CDD, 0x1F3F, 0x59BF, 0x48F3, 0x64A1, 0x5ABC, 0x6632, 0x6051];

pub const MAX_ORDER: u32 = 64;
pub const MIN_ORDER: u32 = 2;

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

pub struct Ppmd7<R> {
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
    hi_bits_flag: u32,
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

    ns2bs_indx: [u8; 256],
    ns2indx: [u8; 256],
    hb2flag: [u8; 256],

    dummy_see: See,
    see: [[See; 16]; 25],
    bin_summ: [[u16; 64]; 128],
}

impl<R: Read> Ppmd7<R> {
    pub fn new(inner: R, mem_size: u32, max_order: u32) -> crate::Result<Self> {
        let align_offset = 4 - (mem_size & 3);

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

        let mut ns2indx = [0u8; 256];
        for (i, slot) in ns2indx.iter_mut().enumerate().take(3) {
            *slot = i as u8;
        }
        let mut m = 3usize;
        let mut k = 1usize;
        for slot in ns2indx.iter_mut().skip(3) {
            *slot = m as u8;
            k -= 1;
            if k == 0 {
                m += 1;
                k = m - 2;
            }
        }

        let mut hb2flag = [0u8; 256];
        hb2flag[0x40..].fill(8);

        let mut model = Ppmd7 {
            rc: RangeDecoder::new(inner)?,
            base: crate::utils::limits::zeroed((align_offset + mem_size) as usize + UNIT_SIZE as usize)?,
            size: mem_size,
            align_offset,
            min_context: 0,
            max_context: 0,
            found_state: 0,
            order_fall: 0,
            init_esc: 0,
            prev_success: 0,
            max_order,
            hi_bits_flag: 0,
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
            ns2bs_indx,
            ns2indx,
            hb2flag,
            dummy_see: See::default(),
            see: [[See::default(); 16]; 25],
            bin_summ: [[0u16; 64]; 128],
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
        self.u16_at(c) as u32
    }
    #[inline]
    fn set_num_stats(&mut self, c: u32, v: u32) {
        self.set_u16(c, v as u16);
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
        self.set_u32(node, self.free_list[index]);
        self.free_list[index] = node;
    }

    fn remove_node(&mut self, index: usize) -> u32 {
        let node = self.free_list[index];
        self.free_list[index] = self.u32_at(node);
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

    #[inline]
    fn node_stamp(&self, n: u32) -> u16 {
        self.u16_at(n)
    }
    #[inline]
    fn set_node_stamp(&mut self, n: u32, v: u16) {
        self.set_u16(n, v);
    }
    #[inline]
    fn node_nu(&self, n: u32) -> u32 {
        self.u16_at(n + 2) as u32
    }
    #[inline]
    fn set_node_nu(&mut self, n: u32, v: u32) {
        self.set_u16(n + 2, v as u16);
    }
    #[inline]
    fn node_next(&self, n: u32) -> u32 {
        self.u32_at(n + 4)
    }
    #[inline]
    fn set_node_next(&mut self, n: u32, v: u32) {
        self.set_u32(n + 4, v);
    }
    #[inline]
    fn node_prev(&self, n: u32) -> u32 {
        self.u32_at(n + 8)
    }
    #[inline]
    fn set_node_prev(&mut self, n: u32, v: u32) {
        self.set_u32(n + 8, v);
    }

    fn glue_free_blocks(&mut self) {
        let head = self.align_offset + self.size;
        let mut n = head;

        self.glue_count = 255;

        for i in 0..INDEXES {
            let nu = self.units_of(i);
            let mut next = self.free_list[i];
            self.free_list[i] = 0;
            while next != 0 {
                let node = next;
                self.set_node_next(node, n);
                self.set_node_prev(n, node);
                n = node;
                next = self.u32_at(node);
                self.set_node_stamp(node, 0);
                self.set_node_nu(node, nu);
            }
        }

        self.set_node_stamp(head, 1);
        self.set_node_next(head, n);
        self.set_node_prev(n, head);
        if self.lo_unit != self.hi_unit {
            self.set_node_stamp(self.lo_unit, 1);
        }

        while n != head {
            let mut nu = self.node_nu(n);
            loop {
                let node2 = n + nu * UNIT_SIZE;
                nu += self.node_nu(node2);
                if self.node_stamp(node2) != 0 || nu >= 0x10000 {
                    break;
                }
                let prev = self.node_prev(node2);
                let next = self.node_next(node2);
                self.set_node_next(prev, next);
                self.set_node_prev(next, prev);
                self.set_node_nu(n, nu);
            }
            n = self.node_next(n);
        }

        n = self.node_next(head);
        while n != head {
            let next = self.node_next(n);
            let mut nu = self.node_nu(n);
            let mut at = n;
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
            n = next;
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

    fn restart(&mut self) {
        self.free_list = [0; INDEXES];
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

        self.set_num_stats(mc, 256);
        self.set_summ_freq(mc, 256 + 1);
        self.set_stats(mc, s);
        self.set_suffix(mc, 0);

        for i in 0..256u32 {
            let state = s + i * 6;
            self.set_sym(state, i as u8);
            self.set_freq(state, 1);
            self.set_successor(state, 0);
        }

        for i in 0..128usize {
            for (k, &esc) in INIT_BIN_ESC.iter().enumerate() {
                let val = (BIN_SCALE - esc as u32 / (i as u32 + 2)) as u16;
                let mut m = 0usize;
                while m < 64 {
                    self.bin_summ[i][k + m] = val;
                    m += 8;
                }
            }
        }

        for i in 0..25usize {
            for k in 0..16usize {
                self.see[i][k] = See { summ: (((5 * i as u32 + 10) << (PERIOD_BITS - 4)) as u16), shift: (PERIOD_BITS - 4) as u8, count: 4 };
            }
        }

        self.dummy_see = See { summ: 0, shift: PERIOD_BITS as u8, count: 64 };
    }

    fn create_successors(&mut self, skip: bool) -> u32 {
        let mut c = self.min_context;
        let up_branch = self.successor(self.found_state);
        let mut ps = [0u32; MAX_ORDER as usize];
        let mut num_ps = 0usize;

        if !skip {
            ps[num_ps] = self.found_state;
            num_ps += 1;
        }

        while self.suffix(c) != 0 {
            c = self.suffix(c);
            let s = if self.num_stats(c) != 1 {
                let sym = self.sym(self.found_state);
                let mut t = self.stats(c);
                let end = t + self.num_stats(c) * 6;
                while t != end && self.sym(t) != sym {
                    t += 6;
                }
                if t == end {
                    return 0;
                }
                t
            } else {
                self.one_state(c)
            };

            let successor = self.successor(s);
            if successor != up_branch {
                c = successor;
                if num_ps == 0 {
                    return c;
                }
                break;
            }
            if num_ps >= ps.len() {
                return 0;
            }
            ps[num_ps] = s;
            num_ps += 1;
        }

        let up_symbol = self.u8_at(up_branch);
        let up_successor = up_branch + 1;
        let up_freq = if self.num_stats(c) == 1 {
            self.freq(self.one_state(c))
        } else {
            let mut s = self.stats(c);
            let end = s + self.num_stats(c) * 6;
            while s != end && self.sym(s) != up_symbol {
                s += 6;
            }
            if s == end {
                return 0;
            }

            let Some(cf) = self.freq(s).checked_sub(1) else { return 0 };
            let Some(s0) = self.summ_freq(c).checked_sub(self.num_stats(c)).and_then(|rest| rest.checked_sub(cf)) else { return 0 };
            if s0 == 0 && 2 * cf > s0 {
                return 0;
            }

            1 + if 2 * cf <= s0 { u32::from(5 * cf > s0) } else { (2 * cf + 3 * s0 - 1) / (2 * s0) }
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

            self.set_num_stats(c1, 1);
            let one = self.one_state(c1);
            self.set_sym(one, up_symbol);
            self.set_freq(one, up_freq);
            self.set_successor(one, up_successor);
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

    fn update_model(&mut self) {
        let mut f_successor = self.successor(self.found_state);
        let f_symbol = self.sym(self.found_state);
        let f_freq = self.freq(self.found_state);

        if f_freq < MAX_FREQ / 4 && self.suffix(self.min_context) != 0 {
            let c = self.suffix(self.min_context);
            if self.num_stats(c) == 1 {
                let s = self.one_state(c);
                if self.freq(s) < 32 {
                    self.set_freq(s, self.freq(s) + 1);
                }
            } else {
                let mut s = self.stats(c);
                if self.sym(s) != f_symbol {
                    while self.sym(s) != f_symbol {
                        s += 6;
                    }
                    if self.freq(s) >= self.freq(s - 6) {
                        self.swap_states(s, s - 6);
                        s -= 6;
                    }
                }
                if self.freq(s) < MAX_FREQ - 9 {
                    self.set_freq(s, self.freq(s) + 2);
                    self.set_summ_freq(c, self.summ_freq(c) + 2);
                }
            }
        }

        if self.order_fall == 0 {
            let cs = self.create_successors(true);
            if cs == 0 {
                self.restart();
                return;
            }
            self.min_context = cs;
            self.max_context = cs;
            self.set_successor(self.found_state, cs);
            return;
        }

        self.set_u8(self.text, f_symbol);
        self.text += 1;
        let mut successor = self.text;
        if self.text >= self.units_start {
            self.restart();
            return;
        }

        if f_successor != 0 {
            if f_successor <= successor {
                let cs = self.create_successors(false);
                if cs == 0 {
                    self.restart();
                    return;
                }
                f_successor = cs;
            }
            self.order_fall -= 1;
            if self.order_fall == 0 {
                successor = f_successor;
                self.text -= u32::from(self.max_context != self.min_context);
            }
        } else {
            self.set_successor(self.found_state, successor);
            f_successor = self.min_context;
        }

        let ns = self.num_stats(self.min_context);
        let Some(s0) = self.summ_freq(self.min_context).checked_sub(ns).and_then(|rest| rest.checked_sub(f_freq.saturating_sub(1))) else {
            self.restart();
            return;
        };

        let mut c = self.max_context;
        while c != self.min_context {
            let ns1 = self.num_stats(c);

            if ns1 != 1 {
                if ns1 & 1 == 0 {
                    let old_nu = ns1 >> 1;
                    let i = self.index_of(old_nu);
                    if i != self.index_of(old_nu + 1) {
                        let ptr = self.alloc_units(i + 1);
                        if ptr == 0 {
                            self.restart();
                            return;
                        }
                        let old_ptr = self.stats(c);
                        self.copy_units(ptr, old_ptr, old_nu);
                        self.insert_node(old_ptr, i);
                        self.set_stats(c, ptr);
                    }
                }
                let bump = u32::from(2 * ns1 < ns) + 2 * u32::from(4 * ns1 <= ns && self.summ_freq(c) <= 8 * ns1);
                self.set_summ_freq(c, self.summ_freq(c) + bump);
            } else {
                let ptr = self.alloc_units(0);
                if ptr == 0 {
                    self.restart();
                    return;
                }
                self.copy_state(ptr, self.one_state(c));
                self.set_stats(c, ptr);

                let mut freq = self.freq(ptr);
                if freq < MAX_FREQ / 4 - 1 {
                    freq += freq;
                } else {
                    freq = MAX_FREQ - 4;
                }
                self.set_freq(ptr, freq);
                self.set_summ_freq(c, freq + self.init_esc + u32::from(ns > 3));
            }

            let cf = 2 * f_freq * (self.summ_freq(c) + 6);
            let sf = s0 + self.summ_freq(c);
            let freq = if cf < 6 * sf {
                let freq = 1 + u32::from(cf > sf) + u32::from(cf >= 4 * sf);
                self.set_summ_freq(c, self.summ_freq(c) + 3);
                freq
            } else {
                let freq = 4 + u32::from(cf >= 9 * sf) + u32::from(cf >= 12 * sf) + u32::from(cf >= 15 * sf);
                self.set_summ_freq(c, self.summ_freq(c) + freq);
                freq
            };

            let s = self.stats(c) + ns1 * 6;
            self.set_successor(s, successor);
            self.set_sym(s, f_symbol);
            self.set_freq(s, freq);
            self.set_num_stats(c, ns1 + 1);

            c = self.suffix(c);
        }

        self.max_context = f_successor;
        self.min_context = f_successor;
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
        self.set_freq(s, self.freq(s) + 4);
        let adder = u32::from(self.order_fall != 0);
        self.set_freq(s, (self.freq(s) + adder) >> 1);
        let mut sum_freq = self.freq(s);

        let mut i = self.num_stats(self.min_context) - 1;
        while i > 0 {
            s += 6;
            esc_freq -= self.freq(s);
            self.set_freq(s, (self.freq(s) + adder) >> 1);
            sum_freq += self.freq(s);

            if self.freq(s) > self.freq(s - 6) {
                let mut tmp = [0u8; 6];
                tmp.copy_from_slice(&self.base[s as usize..s as usize + 6]);
                let freq = tmp[1] as u32;
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

            if num_stats_new == 1 {
                let mut freq = self.freq(stats);
                loop {
                    freq -= freq >> 1;
                    esc_freq >>= 1;
                    if esc_freq <= 1 {
                        break;
                    }
                }
                let index = self.index_of((num_stats + 1) >> 1);
                let one = self.one_state(mc);
                self.copy_state(one, stats);
                self.set_freq(one, freq);
                self.found_state = one;
                self.insert_node(stats, index);
                return;
            }

            let n0 = (num_stats + 1) >> 1;
            let n1 = (num_stats_new + 1) >> 1;
            if n0 != n1 {
                let shrunk = self.shrink_units(stats, n0, n1);
                self.set_stats(mc, shrunk);
            }
        }

        let mc = self.min_context;
        self.set_summ_freq(mc, sum_freq + esc_freq - (esc_freq >> 1));
        self.found_state = self.stats(mc);
    }

    fn make_esc_freq(&mut self, num_masked: u32) -> (usize, usize, u32) {
        let mc = self.min_context;
        let num_stats = self.num_stats(mc);
        let non_masked = num_stats.saturating_sub(num_masked).max(1);

        if num_stats != 256 {
            let row = self.ns2indx[(non_masked as usize - 1).min(self.ns2indx.len() - 1)] as usize;
            let column = u32::from(non_masked < self.num_stats(self.suffix(mc)).wrapping_sub(num_stats))
                + 2 * u32::from(self.summ_freq(mc) < 11 * num_stats)
                + 4 * u32::from(num_masked > non_masked)
                + self.hi_bits_flag;
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
        if self.order_fall == 0 && c > self.text {
            self.min_context = c;
            self.max_context = c;
        } else {
            self.update_model();
        }
    }

    fn update1(&mut self) {
        let s = self.found_state;
        self.set_freq(s, self.freq(s) + 4);
        self.set_summ_freq(self.min_context, self.summ_freq(self.min_context) + 4);
        if self.freq(s) > self.freq(s - 6) {
            self.swap_states(s, s - 6);
            self.found_state = s - 6;
            if self.freq(s - 6) > MAX_FREQ {
                self.rescale();
            }
        }
        self.next_context();
    }

    fn update1_0(&mut self) {
        let s = self.found_state;
        let mc = self.min_context;
        self.prev_success = u32::from(2 * self.freq(s) > self.summ_freq(mc));
        self.run_length += self.prev_success as i32;
        self.set_summ_freq(mc, self.summ_freq(mc) + 4);
        self.set_freq(s, self.freq(s) + 4);
        if self.freq(s) > MAX_FREQ {
            self.rescale();
        }
        self.next_context();
    }

    fn update_bin(&mut self) {
        let s = self.found_state;
        let freq = self.freq(s);
        self.set_freq(s, freq + u32::from(freq < 128));
        self.prev_success = 1;
        self.run_length += 1;
        self.next_context();
    }

    fn update2(&mut self) {
        let s = self.found_state;
        self.set_freq(s, self.freq(s) + 4);
        self.set_summ_freq(self.min_context, self.summ_freq(self.min_context) + 4);
        if self.freq(s) > MAX_FREQ {
            self.rescale();
        }
        self.run_length = self.init_rl;
        self.update_model();
    }

    fn bin_summ_index(&mut self) -> (usize, usize) {
        let mc = self.min_context;
        let one = self.one_state(mc);
        self.hi_bits_flag = self.hb2flag[self.sym(self.found_state) as usize] as u32;

        let row = self.freq(one) as usize - 1;
        let column = self.prev_success as usize
            + self.ns2bs_indx[self.num_stats(self.suffix(mc)) as usize - 1] as usize
            + self.hi_bits_flag as usize
            + 2 * self.hb2flag[self.sym(one) as usize] as usize
            + ((self.run_length >> 26) & 0x20) as usize;
        (row, column)
    }

    pub fn is_finished(&self) -> bool {
        self.rc.is_finished()
    }

    pub fn ran_dry(&self) -> bool {
        self.rc.ran_dry()
    }

    pub fn decode_symbol(&mut self) -> i32 {
        let mut char_mask = [0u8; 256];

        if self.num_stats(self.min_context) != 1 {
            let mut s = self.stats(self.min_context);
            let summ_freq = self.summ_freq(self.min_context);

            let count = self.rc.threshold(summ_freq);
            let hi_cnt = count;

            let mut count = count.wrapping_sub(self.freq(s));
            if (count as i32) < 0 {
                let freq = self.freq(s);
                self.rc.decode(0, freq);
                self.found_state = s;
                let sym = self.sym(s);
                self.update1_0();
                return sym as i32;
            }

            self.prev_success = 0;
            let mut i = self.num_stats(self.min_context) - 1;
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
                self.found_state = s;
                let sym = self.sym(s);
                self.update1();
                return sym as i32;
            }

            if hi_cnt >= summ_freq {
                return SYM_ERROR;
            }
            let total = hi_cnt.wrapping_sub(count);
            self.hi_bits_flag = self.hb2flag[self.sym(self.found_state) as usize] as u32;
            self.rc.decode(total, summ_freq - total);

            char_mask.fill(0xFF);
            let mut s2 = self.stats(self.min_context);
            let end = s2 + self.num_stats(self.min_context) * 6;
            while s2 != end {
                char_mask[self.sym(s2) as usize] = 0;
                s2 += 6;
            }
        } else {
            let one = self.one_state(self.min_context);
            let (row, column) = self.bin_summ_index();
            let pr = self.bin_summ[row][column] as u32;
            let mean = mean(pr);

            if self.rc.decode_bit(pr) == 0 {
                self.bin_summ[row][column] = (pr + (1 << INT_BITS) - mean) as u16;
                let sym = self.sym(one);
                self.found_state = one;
                self.update_bin();
                return sym as i32;
            }

            let updated = pr - mean;
            self.bin_summ[row][column] = updated as u16;
            self.init_esc = EXP_ESCAPE[(updated >> 10) as usize] as u32;

            char_mask.fill(0xFF);
            char_mask[self.sym(one) as usize] = 0;
            self.prev_success = 0;
        }

        loop {
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

            let Some(wanted) = self.num_stats(mc).checked_sub(num_masked).filter(|wanted| *wanted > 0) else {
                return SYM_ERROR;
            };

            let mut ps = [0u32; 256];
            let mut found = 0usize;
            let mut hi_cnt = 0u32;
            let mut s = self.stats(mc);
            let end = s + self.num_stats(mc) * 6;
            while found != wanted as usize {
                if s == end {
                    return SYM_ERROR;
                }
                if char_mask[self.sym(s) as usize] != 0 {
                    hi_cnt += self.freq(s);
                    ps[found] = s;
                    found += 1;
                }
                s += 6;
            }

            let (row, column, esc) = self.make_esc_freq(num_masked);
            let freq_sum = esc + hi_cnt;

            let count = self.rc.threshold(freq_sum);

            if count < hi_cnt {
                let mut acc = 0u32;
                let mut at = 0usize;
                loop {
                    acc += self.freq(ps[at]);
                    if acc > count {
                        break;
                    }
                    at += 1;
                }
                let s = ps[at];
                let freq = self.freq(s);
                self.rc.decode(acc - freq, freq);

                self.see_update(row, column);
                self.found_state = s;
                let sym = self.sym(s);
                self.update2();
                return sym as i32;
            }

            if count >= freq_sum {
                return SYM_ERROR;
            }
            self.rc.decode(hi_cnt, freq_sum - hi_cnt);
            self.see_add(row, column, freq_sum);

            for &state in ps.iter().take(found) {
                char_mask[self.sym(state) as usize] = 0;
            }
        }
    }
}

#[inline]
fn mean(summ: u32) -> u32 {
    (summ + (1 << (PERIOD_BITS - 2))) >> PERIOD_BITS
}
