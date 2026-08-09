pub const BLOCK_SIZE: usize = 16;

static SBOX: [u8; 256] = [
    0x63, 0x7c, 0x77, 0x7b, 0xf2, 0x6b, 0x6f, 0xc5, 0x30, 0x01, 0x67, 0x2b, 0xfe, 0xd7, 0xab, 0x76, 0xca, 0x82, 0xc9, 0x7d, 0xfa, 0x59, 0x47, 0xf0, 0xad, 0xd4,
    0xa2, 0xaf, 0x9c, 0xa4, 0x72, 0xc0, 0xb7, 0xfd, 0x93, 0x26, 0x36, 0x3f, 0xf7, 0xcc, 0x34, 0xa5, 0xe5, 0xf1, 0x71, 0xd8, 0x31, 0x15, 0x04, 0xc7, 0x23, 0xc3,
    0x18, 0x96, 0x05, 0x9a, 0x07, 0x12, 0x80, 0xe2, 0xeb, 0x27, 0xb2, 0x75, 0x09, 0x83, 0x2c, 0x1a, 0x1b, 0x6e, 0x5a, 0xa0, 0x52, 0x3b, 0xd6, 0xb3, 0x29, 0xe3,
    0x2f, 0x84, 0x53, 0xd1, 0x00, 0xed, 0x20, 0xfc, 0xb1, 0x5b, 0x6a, 0xcb, 0xbe, 0x39, 0x4a, 0x4c, 0x58, 0xcf, 0xd0, 0xef, 0xaa, 0xfb, 0x43, 0x4d, 0x33, 0x85,
    0x45, 0xf9, 0x02, 0x7f, 0x50, 0x3c, 0x9f, 0xa8, 0x51, 0xa3, 0x40, 0x8f, 0x92, 0x9d, 0x38, 0xf5, 0xbc, 0xb6, 0xda, 0x21, 0x10, 0xff, 0xf3, 0xd2, 0xcd, 0x0c,
    0x13, 0xec, 0x5f, 0x97, 0x44, 0x17, 0xc4, 0xa7, 0x7e, 0x3d, 0x64, 0x5d, 0x19, 0x73, 0x60, 0x81, 0x4f, 0xdc, 0x22, 0x2a, 0x90, 0x88, 0x46, 0xee, 0xb8, 0x14,
    0xde, 0x5e, 0x0b, 0xdb, 0xe0, 0x32, 0x3a, 0x0a, 0x49, 0x06, 0x24, 0x5c, 0xc2, 0xd3, 0xac, 0x62, 0x91, 0x95, 0xe4, 0x79, 0xe7, 0xc8, 0x37, 0x6d, 0x8d, 0xd5,
    0x4e, 0xa9, 0x6c, 0x56, 0xf4, 0xea, 0x65, 0x7a, 0xae, 0x08, 0xba, 0x78, 0x25, 0x2e, 0x1c, 0xa6, 0xb4, 0xc6, 0xe8, 0xdd, 0x74, 0x1f, 0x4b, 0xbd, 0x8b, 0x8a,
    0x70, 0x3e, 0xb5, 0x66, 0x48, 0x03, 0xf6, 0x0e, 0x61, 0x35, 0x57, 0xb9, 0x86, 0xc1, 0x1d, 0x9e, 0xe1, 0xf8, 0x98, 0x11, 0x69, 0xd9, 0x8e, 0x94, 0x9b, 0x1e,
    0x87, 0xe9, 0xce, 0x55, 0x28, 0xdf, 0x8c, 0xa1, 0x89, 0x0d, 0xbf, 0xe6, 0x42, 0x68, 0x41, 0x99, 0x2d, 0x0f, 0xb0, 0x54, 0xbb, 0x16,
];

static RCON: [u8; 11] = [0x00, 0x01, 0x02, 0x04, 0x08, 0x10, 0x20, 0x40, 0x80, 0x1b, 0x36];

#[inline]
const fn xtime(b: u8) -> u8 {
    (b << 1) ^ (((b >> 7) & 1) * 0x1b)
}

#[inline]
const fn mul3(b: u8) -> u8 {
    xtime(b) ^ b
}

static T: [[u32; 256]; 4] = build_tables();

const fn build_tables() -> [[u32; 256]; 4] {
    let mut t = [[0u32; 256]; 4];
    let mut i = 0;
    while i < 256 {
        let s = SBOX[i];
        let s2 = xtime(s);
        let s3 = mul3(s);

        t[0][i] = (s2 as u32) | ((s as u32) << 8) | ((s as u32) << 16) | ((s3 as u32) << 24);
        t[1][i] = (s3 as u32) | ((s2 as u32) << 8) | ((s as u32) << 16) | ((s as u32) << 24);
        t[2][i] = (s as u32) | ((s3 as u32) << 8) | ((s2 as u32) << 16) | ((s as u32) << 24);
        t[3][i] = (s as u32) | ((s as u32) << 8) | ((s3 as u32) << 16) | ((s2 as u32) << 24);
        i += 1;
    }
    t
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeySize {
    Aes128,
    Aes192,
    Aes256,
}

impl KeySize {
    pub fn key_len(self) -> usize {
        match self {
            KeySize::Aes128 => 16,
            KeySize::Aes192 => 24,
            KeySize::Aes256 => 32,
        }
    }

    pub fn rounds(self) -> usize {
        match self {
            KeySize::Aes128 => 10,
            KeySize::Aes192 => 12,
            KeySize::Aes256 => 14,
        }
    }

    pub fn from_key_len(len: usize) -> Option<Self> {
        match len {
            16 => Some(KeySize::Aes128),
            24 => Some(KeySize::Aes192),
            32 => Some(KeySize::Aes256),
            _ => None,
        }
    }
}

pub struct Aes {
    round_keys: [u8; 240],
    rounds: usize,
}

impl Aes {
    pub fn new(key: &[u8]) -> Option<Self> {
        let size = KeySize::from_key_len(key.len())?;
        let rounds = size.rounds();
        let nk = key.len() / 4;
        let total_words = 4 * (rounds + 1);

        let mut round_keys = [0u8; 240];
        round_keys[..key.len()].copy_from_slice(key);

        for i in nk..total_words {
            let mut temp = [round_keys[(i - 1) * 4], round_keys[(i - 1) * 4 + 1], round_keys[(i - 1) * 4 + 2], round_keys[(i - 1) * 4 + 3]];

            if i % nk == 0 {
                temp.rotate_left(1);
                for b in &mut temp {
                    *b = SBOX[*b as usize];
                }
                temp[0] ^= RCON[i / nk];
            } else if nk > 6 && i % nk == 4 {
                for b in &mut temp {
                    *b = SBOX[*b as usize];
                }
            }

            for j in 0..4 {
                round_keys[i * 4 + j] = round_keys[(i - nk) * 4 + j] ^ temp[j];
            }
        }

        Some(Aes { round_keys, rounds })
    }

    pub fn rounds(&self) -> usize {
        self.rounds
    }

    pub fn round_key_bytes(&self) -> &[u8; 240] {
        &self.round_keys
    }

    #[inline]
    fn round_key(&self, index: usize) -> u32 {
        let b = index * 4;
        u32::from_le_bytes([self.round_keys[b], self.round_keys[b + 1], self.round_keys[b + 2], self.round_keys[b + 3]])
    }

    pub fn encrypt_block(&self, block: &mut [u8; BLOCK_SIZE]) {
        let mut s = [0u32; 4];
        for (c, slot) in s.iter_mut().enumerate() {
            *slot = u32::from_le_bytes([block[c * 4], block[c * 4 + 1], block[c * 4 + 2], block[c * 4 + 3]]) ^ self.round_key(c);
        }

        for round in 1..self.rounds {
            let k = round * 4;
            let next = [
                T[0][(s[0] & 0xff) as usize]
                    ^ T[1][((s[1] >> 8) & 0xff) as usize]
                    ^ T[2][((s[2] >> 16) & 0xff) as usize]
                    ^ T[3][((s[3] >> 24) & 0xff) as usize]
                    ^ self.round_key(k),
                T[0][(s[1] & 0xff) as usize]
                    ^ T[1][((s[2] >> 8) & 0xff) as usize]
                    ^ T[2][((s[3] >> 16) & 0xff) as usize]
                    ^ T[3][((s[0] >> 24) & 0xff) as usize]
                    ^ self.round_key(k + 1),
                T[0][(s[2] & 0xff) as usize]
                    ^ T[1][((s[3] >> 8) & 0xff) as usize]
                    ^ T[2][((s[0] >> 16) & 0xff) as usize]
                    ^ T[3][((s[1] >> 24) & 0xff) as usize]
                    ^ self.round_key(k + 2),
                T[0][(s[3] & 0xff) as usize]
                    ^ T[1][((s[0] >> 8) & 0xff) as usize]
                    ^ T[2][((s[1] >> 16) & 0xff) as usize]
                    ^ T[3][((s[2] >> 24) & 0xff) as usize]
                    ^ self.round_key(k + 3),
            ];
            s = next;
        }

        let k = self.rounds * 4;
        for c in 0..4 {
            let word = u32::from_le_bytes([
                SBOX[(s[c] & 0xff) as usize],
                SBOX[((s[(c + 1) % 4] >> 8) & 0xff) as usize],
                SBOX[((s[(c + 2) % 4] >> 16) & 0xff) as usize],
                SBOX[((s[(c + 3) % 4] >> 24) & 0xff) as usize],
            ]) ^ self.round_key(k + c);
            block[c * 4..c * 4 + 4].copy_from_slice(&word.to_le_bytes());
        }
    }
}

pub struct AesCtr {
    backend: Backend,
    counter: u128,
    keystream: [u8; BLOCK_SIZE],
    used: usize,
}

enum Backend {
    Software(Aes),
    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    Hardware(crate::crypto::aes_ni::AesNi),
}

impl AesCtr {
    pub fn new(key: &[u8]) -> Option<Self> {
        #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
        if let Some(hardware) = crate::crypto::aes_ni::AesNi::new(key) {
            return Some(AesCtr { backend: Backend::Hardware(hardware), counter: 1, keystream: [0u8; BLOCK_SIZE], used: BLOCK_SIZE });
        }

        Some(AesCtr { backend: Backend::Software(Aes::new(key)?), counter: 1, keystream: [0u8; BLOCK_SIZE], used: BLOCK_SIZE })
    }

    pub fn is_hardware_accelerated(&self) -> bool {
        match self.backend {
            #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
            Backend::Hardware(_) => true,
            Backend::Software(_) => false,
        }
    }

    fn fill_one(&mut self) {
        let value = self.counter.to_le_bytes();
        self.counter = self.counter.wrapping_add(1);

        match &self.backend {
            Backend::Software(cipher) => {
                self.keystream = value;
                cipher.encrypt_block(&mut self.keystream);
            }
            #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
            Backend::Hardware(cipher) => {
                cipher.keystream(self.counter.wrapping_sub(1), 1, &mut self.keystream);
            }
        }
        self.used = 0;
    }

    pub fn apply(&mut self, data: &mut [u8]) {
        let mut offset = 0;

        while self.used < BLOCK_SIZE && offset < data.len() {
            data[offset] ^= self.keystream[self.used];
            self.used += 1;
            offset += 1;
        }

        #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
        if let Backend::Hardware(cipher) = &self.backend {
            use crate::crypto::aes_ni::{LANES, STRIDE};

            let mut stream = [0u8; STRIDE];
            while data.len() - offset >= STRIDE {
                cipher.keystream(self.counter, LANES, &mut stream);
                self.counter = self.counter.wrapping_add(LANES as u128);

                let chunk = &mut data[offset..offset + STRIDE];
                for (byte, key) in chunk.iter_mut().zip(stream.iter()) {
                    *byte ^= key;
                }
                offset += STRIDE;
            }
        }

        if let Backend::Software(cipher) = &self.backend {
            while data.len() - offset >= BLOCK_SIZE {
                let mut block = self.counter.to_le_bytes();
                self.counter = self.counter.wrapping_add(1);
                cipher.encrypt_block(&mut block);

                let chunk = &mut data[offset..offset + BLOCK_SIZE];
                for (byte, key) in chunk.iter_mut().zip(block.iter()) {
                    *byte ^= key;
                }
                offset += BLOCK_SIZE;
            }
        }

        while offset < data.len() {
            if self.used == BLOCK_SIZE {
                self.fill_one();
            }
            data[offset] ^= self.keystream[self.used];
            self.used += 1;
            offset += 1;
        }
    }
}
