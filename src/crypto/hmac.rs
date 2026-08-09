use crate::crypto::sha1::{BLOCK_LEN, DIGEST_LEN, Sha1};

#[derive(Clone)]
pub struct HmacSha1 {
    inner: Sha1,
    outer_key: [u8; BLOCK_LEN],
}

impl HmacSha1 {
    pub fn new(key: &[u8]) -> Self {
        let mut padded = [0u8; BLOCK_LEN];
        if key.len() > BLOCK_LEN {
            padded[..DIGEST_LEN].copy_from_slice(&crate::crypto::sha1::digest(key));
        } else {
            padded[..key.len()].copy_from_slice(key);
        }

        let mut inner_key = [0u8; BLOCK_LEN];
        let mut outer_key = [0u8; BLOCK_LEN];
        for i in 0..BLOCK_LEN {
            inner_key[i] = padded[i] ^ 0x36;
            outer_key[i] = padded[i] ^ 0x5c;
        }

        let mut inner = Sha1::new();
        inner.update(&inner_key);

        HmacSha1 { inner, outer_key }
    }

    pub fn update(&mut self, data: &[u8]) {
        self.inner.update(data);
    }

    pub fn finish(self) -> [u8; DIGEST_LEN] {
        let inner = self.inner.finish();
        let mut outer = Sha1::new();
        outer.update(&self.outer_key);
        outer.update(&inner);
        outer.finish()
    }
}

pub fn hmac_sha1(key: &[u8], message: &[u8]) -> [u8; DIGEST_LEN] {
    let mut h = HmacSha1::new(key);
    h.update(message);
    h.finish()
}

pub fn pbkdf2_sha1(password: &[u8], salt: &[u8], iterations: u32, out: &mut [u8]) {
    let base = HmacSha1::new(password);

    for (block_index, chunk) in out.chunks_mut(DIGEST_LEN).enumerate() {
        let mut mac = base.clone();
        mac.update(salt);
        mac.update(&((block_index as u32) + 1).to_be_bytes());
        let mut u = mac.finish();

        let mut accumulator = u;

        for _ in 1..iterations {
            let mut mac = base.clone();
            mac.update(&u);
            u = mac.finish();
            for (a, b) in accumulator.iter_mut().zip(u.iter()) {
                *a ^= b;
            }
        }

        let take = chunk.len();
        chunk.copy_from_slice(&accumulator[..take]);
    }
}

pub fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}
