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
