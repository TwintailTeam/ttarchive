#![cfg(any(target_arch = "x86", target_arch = "x86_64"))]

#[cfg(target_arch = "x86")]
use std::arch::x86::*;
#[cfg(target_arch = "x86_64")]
use std::arch::x86_64::*;

use super::crc64::{inverse_power_of_x, multiply, over_x, power_of_x, sliced};

const BLOCK: usize = 16;

const LANES: usize = 8;

const GROUP: usize = LANES * BLOCK;

pub const MIN_BYTES: usize = GROUP * 4;

const AHEAD: u64 = power_of_x(64);

const BEHIND: u64 = inverse_power_of_x(64);

const FOLD_LOW: u64 = over_x(power_of_x(8 * GROUP as u64 + 64));

const FOLD_HIGH: u64 = over_x(power_of_x(8 * GROUP as u64));

const LANE_SHIFT: [u64; LANES] = lane_shifts();

const fn lane_shifts() -> [u64; LANES] {
    let mut shifts = [0u64; LANES];
    let mut lane = 0;
    while lane < LANES {
        shifts[lane] = power_of_x(8 * BLOCK as u64 * (LANES - 1 - lane) as u64);
        lane += 1;
    }
    shifts
}

pub fn available() -> bool {
    is_x86_feature_detected!("pclmulqdq") && is_x86_feature_detected!("sse2")
}

pub fn update(crc: u64, data: &[u8]) -> u64 {
    let groups = data.len() / GROUP;
    if groups == 0 {
        return super::crc64::striped(crc, data);
    }

    let covered = groups * GROUP;
    let crc = unsafe { fold(crc, &data[..covered]) };
    sliced(crc, &data[covered..])
}

#[target_feature(enable = "pclmulqdq,sse2")]
unsafe fn fold(seed: u64, data: &[u8]) -> u64 {
    unsafe {
        let constants = _mm_set_epi64x(FOLD_HIGH as i64, FOLD_LOW as i64);

        let mut lanes = [_mm_setzero_si128(); LANES];
        lanes[LANES - 1] = _mm_set_epi64x(multiply(seed, BEHIND) as i64, 0);

        for group in data.chunks_exact(GROUP) {
            for (lane, state) in lanes.iter_mut().enumerate() {
                let block = group.as_ptr().add(lane * BLOCK) as *const __m128i;
                let low = _mm_clmulepi64_si128(*state, constants, 0x00);
                let high = _mm_clmulepi64_si128(*state, constants, 0x11);
                *state = _mm_xor_si128(_mm_xor_si128(low, high), _mm_loadu_si128(block));
            }
        }

        let mut total = 0u64;
        for (lane, state) in lanes.iter().enumerate() {
            let low = _mm_cvtsi128_si64(*state) as u64;
            let high = _mm_extract_epi64(*state, 1) as u64;
            total ^= multiply(multiply(low, AHEAD) ^ high, LANE_SHIFT[lane]);
        }

        multiply(total, AHEAD)
    }
}
