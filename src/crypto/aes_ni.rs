#![cfg(any(target_arch = "x86", target_arch = "x86_64"))]

#[cfg(target_arch = "x86")]
use std::arch::x86::*;
#[cfg(target_arch = "x86_64")]
use std::arch::x86_64::*;

use crate::crypto::aes::{Aes, BLOCK_SIZE};

pub const LANES: usize = 8;

pub const STRIDE: usize = LANES * BLOCK_SIZE;

pub fn available() -> bool {
    is_x86_feature_detected!("aes") && is_x86_feature_detected!("sse2")
}

pub struct AesNi {
    round_keys: [__m128i; 15],
    rounds: usize,
}

unsafe impl Send for AesNi {}
unsafe impl Sync for AesNi {}

pub struct AesNiDecrypt {
    round_keys: [__m128i; 15],
    rounds: usize,
}

unsafe impl Send for AesNiDecrypt {}
unsafe impl Sync for AesNiDecrypt {}

impl AesNiDecrypt {
    pub fn new(key: &[u8]) -> Option<Self> {
        if !available() {
            return None;
        }

        let software = Aes::new(key)?;
        let rounds = software.rounds();
        let bytes = software.round_key_bytes();

        let round_keys = unsafe {
            let mut keys = [_mm_setzero_si128(); 15];
            for (i, slot) in keys.iter_mut().enumerate().take(rounds + 1) {
                let key = _mm_loadu_si128(bytes.as_ptr().add(i * BLOCK_SIZE).cast());
                *slot = if i == 0 || i == rounds { key } else { _mm_aesimc_si128(key) };
            }
            keys
        };

        Some(AesNiDecrypt { round_keys, rounds })
    }

    pub fn cbc(&self, chain: &mut [u8; BLOCK_SIZE], data: &mut [u8]) {
        debug_assert_eq!(data.len() % BLOCK_SIZE, 0);
        unsafe { self.cbc_impl(chain, data) }
    }

    #[target_feature(enable = "aes,sse2")]
    unsafe fn cbc_impl(&self, chain: &mut [u8; BLOCK_SIZE], data: &mut [u8]) {
        unsafe {
            let mut previous = _mm_loadu_si128(chain.as_ptr().cast());

            for group in data.chunks_mut(STRIDE) {
                let blocks = group.len() / BLOCK_SIZE;
                let mut state = [_mm_setzero_si128(); LANES];
                let mut cipher = [_mm_setzero_si128(); LANES];

                for i in 0..blocks {
                    let block = _mm_loadu_si128(group.as_ptr().add(i * BLOCK_SIZE).cast());
                    cipher[i] = block;
                    state[i] = _mm_xor_si128(block, self.round_keys[self.rounds]);
                }

                for round in (1..self.rounds).rev() {
                    let key = self.round_keys[round];
                    for slot in state.iter_mut().take(blocks) {
                        *slot = _mm_aesdec_si128(*slot, key);
                    }
                }

                let first = self.round_keys[0];
                for i in 0..blocks {
                    let plain = _mm_xor_si128(_mm_aesdeclast_si128(state[i], first), previous);
                    previous = cipher[i];
                    _mm_storeu_si128(group.as_mut_ptr().add(i * BLOCK_SIZE).cast(), plain);
                }
            }

            _mm_storeu_si128(chain.as_mut_ptr().cast(), previous);
        }
    }
}

impl AesNi {
    pub fn new(key: &[u8]) -> Option<Self> {
        if !available() {
            return None;
        }

        let software = Aes::new(key)?;
        let rounds = software.rounds();
        let bytes = software.round_key_bytes();

        let round_keys = unsafe {
            let mut keys = [_mm_setzero_si128(); 15];
            for (i, slot) in keys.iter_mut().enumerate().take(rounds + 1) {
                *slot = _mm_loadu_si128(bytes.as_ptr().add(i * BLOCK_SIZE).cast());
            }
            keys
        };

        Some(AesNi { round_keys, rounds })
    }

    pub fn keystream(&self, counter: u128, blocks: usize, out: &mut [u8]) {
        debug_assert!(blocks <= LANES);
        debug_assert!(out.len() >= blocks * BLOCK_SIZE);

        unsafe { self.keystream_impl(counter, blocks, out) }
    }

    #[target_feature(enable = "aes,sse2")]
    unsafe fn keystream_impl(&self, counter: u128, blocks: usize, out: &mut [u8]) {
        let mut state = [_mm_setzero_si128(); LANES];

        unsafe {
            for (i, slot) in state.iter_mut().enumerate().take(blocks) {
                let value = counter.wrapping_add(i as u128).to_le_bytes();
                *slot = _mm_xor_si128(_mm_loadu_si128(value.as_ptr().cast()), self.round_keys[0]);
            }

            for round in 1..self.rounds {
                let key = self.round_keys[round];
                for slot in state.iter_mut().take(blocks) {
                    *slot = _mm_aesenc_si128(*slot, key);
                }
            }

            let last = self.round_keys[self.rounds];
            for (i, slot) in state.iter_mut().enumerate().take(blocks) {
                *slot = _mm_aesenclast_si128(*slot, last);
                _mm_storeu_si128(out.as_mut_ptr().add(i * BLOCK_SIZE).cast(), *slot);
            }
        }
    }
}
