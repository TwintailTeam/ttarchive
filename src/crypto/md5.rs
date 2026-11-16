const BLOCK_LEN: usize = 64;

pub const DIGEST_LEN: usize = 16;

const S: [u32; 64] = [
    7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 5, 9, 14, 20, 5, 9, 14, 20, 5, 9, 14, 20, 5, 9, 14, 20, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11,
    16, 23, 4, 11, 16, 23, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21,
];

const K: [u32; 64] = [
    0xd76aa478, 0xe8c7b756, 0x242070db, 0xc1bdceee, 0xf57c0faf, 0x4787c62a, 0xa8304613, 0xfd469501, 0x698098d8, 0x8b44f7af, 0xffff5bb1, 0x895cd7be, 0x6b901122,
    0xfd987193, 0xa679438e, 0x49b40821, 0xf61e2562, 0xc040b340, 0x265e5a51, 0xe9b6c7aa, 0xd62f105d, 0x02441453, 0xd8a1e681, 0xe7d3fbc8, 0x21e1cde6, 0xc33707d6,
    0xf4d50d87, 0x455a14ed, 0xa9e3e905, 0xfcefa3f8, 0x676f02d9, 0x8d2a4c8a, 0xfffa3942, 0x8771f681, 0x6d9d6122, 0xfde5380c, 0xa4beea44, 0x4bdecfa9, 0xf6bb4b60,
    0xbebfbc70, 0x289b7ec6, 0xeaa127fa, 0xd4ef3085, 0x04881d05, 0xd9d4d039, 0xe6db99e5, 0x1fa27cf8, 0xc4ac5665, 0xf4292244, 0x432aff97, 0xab9423a7, 0xfc93a039,
    0x655b59c3, 0x8f0ccc92, 0xffeff47d, 0x85845dd1, 0x6fa87e4f, 0xfe2ce6e0, 0xa3014314, 0x4e0811a1, 0xf7537e82, 0xbd3af235, 0x2ad7d2bb, 0xeb86d391,
];

#[derive(Debug, Clone)]
pub struct Md5 {
    state: [u32; 4],
    block: [u8; BLOCK_LEN],
    buffered: usize,
    length: u64,
}

impl Default for Md5 {
    fn default() -> Self {
        Self::new()
    }
}

impl Md5 {
    pub fn new() -> Self {
        Md5 { state: [0x6745_2301, 0xefcd_ab89, 0x98ba_dcfe, 0x1032_5476], block: [0u8; BLOCK_LEN], buffered: 0, length: 0 }
    }

    pub fn update(&mut self, mut data: &[u8]) {
        self.length = self.length.wrapping_add(data.len() as u64);

        if self.buffered > 0 {
            let want = (BLOCK_LEN - self.buffered).min(data.len());
            self.block[self.buffered..self.buffered + want].copy_from_slice(&data[..want]);
            self.buffered += want;
            data = &data[want..];

            if self.buffered < BLOCK_LEN {
                return;
            }
            let block = self.block;
            self.compress(&block);
            self.buffered = 0;
        }

        while data.len() >= BLOCK_LEN {
            let (block, rest) = data.split_at(BLOCK_LEN);
            self.compress(block.try_into().unwrap_or(&[0; BLOCK_LEN]));
            data = rest;
        }

        self.block[..data.len()].copy_from_slice(data);
        self.buffered = data.len();
    }

    pub fn finish(mut self) -> [u8; DIGEST_LEN] {
        let bits = self.length.wrapping_mul(8);

        self.update(&[0x80]);
        while self.buffered != BLOCK_LEN - 8 {
            self.update(&[0]);
        }
        self.block[BLOCK_LEN - 8..].copy_from_slice(&bits.to_le_bytes());
        let block = self.block;
        self.compress(&block);

        let mut out = [0u8; DIGEST_LEN];
        for (slot, word) in out.chunks_exact_mut(4).zip(self.state) {
            slot.copy_from_slice(&word.to_le_bytes());
        }
        out
    }

    fn compress(&mut self, block: &[u8; BLOCK_LEN]) {
        let mut words = [0u32; 16];
        for (word, chunk) in words.iter_mut().zip(block.chunks_exact(4)) {
            *word = u32::from_le_bytes(chunk.try_into().unwrap_or([0; 4]));
        }

        let [mut a, mut b, mut c, mut d] = self.state;

        for round in 0..16 {
            let mix = (b & c) | (!b & d);
            let moved = a.wrapping_add(mix).wrapping_add(K[round]).wrapping_add(words[round]);
            a = d;
            d = c;
            c = b;
            b = b.wrapping_add(moved.rotate_left(S[round]));
        }

        for round in 16..32 {
            let mix = (d & b) | (!d & c);
            let moved = a.wrapping_add(mix).wrapping_add(K[round]).wrapping_add(words[(5 * round + 1) % 16]);
            a = d;
            d = c;
            c = b;
            b = b.wrapping_add(moved.rotate_left(S[round]));
        }

        for round in 32..48 {
            let mix = b ^ c ^ d;
            let moved = a.wrapping_add(mix).wrapping_add(K[round]).wrapping_add(words[(3 * round + 5) % 16]);
            a = d;
            d = c;
            c = b;
            b = b.wrapping_add(moved.rotate_left(S[round]));
        }

        for round in 48..64 {
            let mix = c ^ (b | !d);
            let moved = a.wrapping_add(mix).wrapping_add(K[round]).wrapping_add(words[(7 * round) % 16]);
            a = d;
            d = c;
            c = b;
            b = b.wrapping_add(moved.rotate_left(S[round]));
        }

        self.state[0] = self.state[0].wrapping_add(a);
        self.state[1] = self.state[1].wrapping_add(b);
        self.state[2] = self.state[2].wrapping_add(c);
        self.state[3] = self.state[3].wrapping_add(d);
    }
}

pub fn digest(data: &[u8]) -> [u8; DIGEST_LEN] {
    let mut hash = Md5::new();
    hash.update(data);
    hash.finish()
}
