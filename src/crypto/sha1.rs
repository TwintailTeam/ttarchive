pub const DIGEST_LEN: usize = 20;
pub const BLOCK_LEN: usize = 64;

#[derive(Clone)]
pub struct Sha1 {
    state: [u32; 5],
    buffer: [u8; BLOCK_LEN],
    buffered: usize,
    length: u64,
}

impl Default for Sha1 {
    fn default() -> Self {
        Self::new()
    }
}

impl Sha1 {
    pub const fn new() -> Self {
        Sha1 { state: [0x6745_2301, 0xEFCD_AB89, 0x98BA_DCFE, 0x1032_5476, 0xC3D2_E1F0], buffer: [0u8; BLOCK_LEN], buffered: 0, length: 0 }
    }

    pub fn update(&mut self, mut data: &[u8]) {
        self.length = self.length.wrapping_add(data.len() as u64);

        if self.buffered > 0 {
            let take = (BLOCK_LEN - self.buffered).min(data.len());
            self.buffer[self.buffered..self.buffered + take].copy_from_slice(&data[..take]);
            self.buffered += take;
            data = &data[take..];

            if self.buffered < BLOCK_LEN {
                return;
            }

            let block = self.buffer;
            self.compress(&block);
            self.buffered = 0;
        }

        let mut chunks = data.chunks_exact(BLOCK_LEN);
        for chunk in &mut chunks {
            let mut block = [0u8; BLOCK_LEN];
            block.copy_from_slice(chunk);
            self.compress(&block);
        }

        let rest = chunks.remainder();
        self.buffer[..rest.len()].copy_from_slice(rest);
        self.buffered = rest.len();
    }

    pub fn finish(mut self) -> [u8; DIGEST_LEN] {
        let bit_length = self.length.wrapping_mul(8);

        self.update_raw(&[0x80]);
        while self.buffered != BLOCK_LEN - 8 {
            self.update_raw(&[0x00]);
        }
        self.update_raw(&bit_length.to_be_bytes());

        let mut out = [0u8; DIGEST_LEN];
        for (i, word) in self.state.iter().enumerate() {
            out[i * 4..i * 4 + 4].copy_from_slice(&word.to_be_bytes());
        }
        out
    }

    fn update_raw(&mut self, data: &[u8]) {
        for &b in data {
            self.buffer[self.buffered] = b;
            self.buffered += 1;
            if self.buffered == BLOCK_LEN {
                let block = self.buffer;
                self.compress(&block);
                self.buffered = 0;
            }
        }
    }

    fn compress(&mut self, block: &[u8; BLOCK_LEN]) {
        let mut w = [0u32; 16];
        for (i, slot) in w.iter_mut().enumerate() {
            *slot = u32::from_be_bytes([block[i * 4], block[i * 4 + 1], block[i * 4 + 2], block[i * 4 + 3]]);
        }

        let [mut a, mut b, mut c, mut d, mut e] = self.state;

        #[inline(always)]
        fn extend(w: &mut [u32; 16], i: usize) -> u32 {
            let value = (w[i & 15] ^ w[(i + 2) & 15] ^ w[(i + 8) & 15] ^ w[(i + 13) & 15]).rotate_left(1);
            w[i & 15] = value;
            value
        }

        macro_rules! round {
            ($f:expr, $k:expr, $word:expr) => {{
                let temp = a.rotate_left(5).wrapping_add($f).wrapping_add(e).wrapping_add($k).wrapping_add($word);
                e = d;
                d = c;
                c = b.rotate_left(30);
                b = a;
                a = temp;
            }};
        }

        for &word in &w[..16] {
            round!((b & c) | (!b & d), 0x5A82_7999, word);
        }
        for i in 16..20 {
            let word = extend(&mut w, i);
            round!((b & c) | (!b & d), 0x5A82_7999, word);
        }
        for i in 20..40 {
            let word = extend(&mut w, i);
            round!(b ^ c ^ d, 0x6ED9_EBA1, word);
        }
        for i in 40..60 {
            let word = extend(&mut w, i);
            round!((b & c) | (b & d) | (c & d), 0x8F1B_BCDC, word);
        }
        for i in 60..80 {
            let word = extend(&mut w, i);
            round!(b ^ c ^ d, 0xCA62_C1D6, word);
        }

        self.state[0] = self.state[0].wrapping_add(a);
        self.state[1] = self.state[1].wrapping_add(b);
        self.state[2] = self.state[2].wrapping_add(c);
        self.state[3] = self.state[3].wrapping_add(d);
        self.state[4] = self.state[4].wrapping_add(e);
    }
}

pub fn digest(data: &[u8]) -> [u8; DIGEST_LEN] {
    let mut h = Sha1::new();
    h.update(data);
    h.finish()
}
