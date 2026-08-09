const PRIME1: u64 = 0x9E37_79B1_85EB_CA87;
const PRIME2: u64 = 0xC2B2_AE3D_27D4_EB4F;
const PRIME3: u64 = 0x1656_67B1_9E37_79F9;
const PRIME4: u64 = 0x85EB_CA77_C2B2_AE63;
const PRIME5: u64 = 0x27D4_EB2F_1656_67C5;

pub struct XxHash64 {
    lanes: [u64; 4],
    buffer: [u8; 32],
    buffered: usize,
    total: u64,
    seed: u64,
}

impl Default for XxHash64 {
    fn default() -> Self {
        Self::new(0)
    }
}

impl XxHash64 {
    pub fn new(seed: u64) -> Self {
        XxHash64 {
            lanes: [seed.wrapping_add(PRIME1).wrapping_add(PRIME2), seed.wrapping_add(PRIME2), seed, seed.wrapping_sub(PRIME1)],
            buffer: [0u8; 32],
            buffered: 0,
            total: 0,
            seed,
        }
    }

    pub fn hash(data: &[u8]) -> u64 {
        let mut state = XxHash64::new(0);
        state.update(data);
        state.finish()
    }

    pub fn update(&mut self, mut data: &[u8]) {
        self.total = self.total.wrapping_add(data.len() as u64);

        if self.buffered > 0 {
            let want = (32 - self.buffered).min(data.len());
            self.buffer[self.buffered..self.buffered + want].copy_from_slice(&data[..want]);
            self.buffered += want;
            data = &data[want..];
            if self.buffered < 32 {
                return;
            }
            let block = self.buffer;
            self.absorb(&block);
            self.buffered = 0;
        }

        while data.len() >= 32 {
            let (block, rest) = data.split_at(32);
            self.absorb(block.try_into().expect("split at 32"));
            data = rest;
        }

        self.buffer[..data.len()].copy_from_slice(data);
        self.buffered = data.len();
    }

    fn absorb(&mut self, block: &[u8; 32]) {
        for (lane, chunk) in self.lanes.iter_mut().zip(block.chunks_exact(8)) {
            let value = u64::from_le_bytes(chunk.try_into().expect("eight bytes"));
            *lane = round(*lane, value);
        }
    }

    pub fn finish(&self) -> u64 {
        let mut hash = if self.total >= 32 {
            let [a, b, c, d] = self.lanes;
            let mut hash = a.rotate_left(1).wrapping_add(b.rotate_left(7)).wrapping_add(c.rotate_left(12)).wrapping_add(d.rotate_left(18));
            for lane in self.lanes {
                hash = merge(hash, lane);
            }
            hash
        } else {
            self.seed.wrapping_add(PRIME5)
        };

        hash = hash.wrapping_add(self.total);

        let mut tail = &self.buffer[..self.buffered];
        while tail.len() >= 8 {
            let value = u64::from_le_bytes(tail[..8].try_into().expect("eight bytes"));
            hash ^= round(0, value);
            hash = hash.rotate_left(27).wrapping_mul(PRIME1).wrapping_add(PRIME4);
            tail = &tail[8..];
        }
        if tail.len() >= 4 {
            let value = u32::from_le_bytes(tail[..4].try_into().expect("four bytes")) as u64;
            hash ^= value.wrapping_mul(PRIME1);
            hash = hash.rotate_left(23).wrapping_mul(PRIME2).wrapping_add(PRIME3);
            tail = &tail[4..];
        }
        for &byte in tail {
            hash ^= (byte as u64).wrapping_mul(PRIME5);
            hash = hash.rotate_left(11).wrapping_mul(PRIME1);
        }

        hash ^= hash >> 33;
        hash = hash.wrapping_mul(PRIME2);
        hash ^= hash >> 29;
        hash = hash.wrapping_mul(PRIME3);
        hash ^= hash >> 32;
        hash
    }
}

fn round(lane: u64, value: u64) -> u64 {
    lane.wrapping_add(value.wrapping_mul(PRIME2)).rotate_left(31).wrapping_mul(PRIME1)
}

fn merge(hash: u64, lane: u64) -> u64 {
    (hash ^ round(0, lane)).wrapping_mul(PRIME1).wrapping_add(PRIME4)
}
