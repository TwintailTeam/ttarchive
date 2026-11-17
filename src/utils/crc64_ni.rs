#![cfg(any(target_arch = "x86", target_arch = "x86_64", target_arch = "aarch64"))]

use std::sync::OnceLock;

use super::crc64::{inverse_power_of_x, multiply, over_x, power_of_x, sliced, striped};

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
    static READY: OnceLock<bool> = OnceLock::new();
    *READY.get_or_init(|| detected() && agrees())
}

#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
fn detected() -> bool {
    is_x86_feature_detected!("pclmulqdq") && is_x86_feature_detected!("sse2")
}

#[cfg(target_arch = "aarch64")]
fn detected() -> bool {
    std::arch::is_aarch64_feature_detected!("pmull") && std::arch::is_aarch64_feature_detected!("neon")
}

fn agrees() -> bool {
    let mut data = [0u8; MIN_BYTES * 3];
    for (index, slot) in data.iter_mut().enumerate() {
        *slot = (index as u8).wrapping_mul(37).wrapping_add(11);
    }

    [MIN_BYTES, MIN_BYTES + 1, MIN_BYTES * 2, MIN_BYTES * 3 - 7].into_iter().all(|len| {
        let slice = &data[..len];
        [u64::MAX, 0, 0xdead_beef_dead_beef].into_iter().all(|seed| fold_whole(seed, slice) == sliced(seed, slice))
    })
}

pub fn update(crc: u64, data: &[u8]) -> u64 {
    if data.len() < GROUP {
        return striped(crc, data);
    }
    fold_whole(crc, data)
}

fn fold_whole(crc: u64, data: &[u8]) -> u64 {
    let groups = data.len() / GROUP;
    let covered = groups * GROUP;
    let folded = unsafe { fold(crc, &data[..covered]) };
    sliced(folded, &data[covered..])
}

#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
#[target_feature(enable = "pclmulqdq,sse2")]
unsafe fn fold(seed: u64, data: &[u8]) -> u64 {
    #[cfg(target_arch = "x86")]
    use std::arch::x86::*;
    #[cfg(target_arch = "x86_64")]
    use std::arch::x86_64::*;

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

#[cfg(target_arch = "aarch64")]
#[target_feature(enable = "aes,neon")]
unsafe fn fold(seed: u64, data: &[u8]) -> u64 {
    use std::arch::aarch64::vmull_p64;

    {
        let mut lanes = [0u128; LANES];
        lanes[LANES - 1] = (multiply(seed, BEHIND) as u128) << 64;

        for group in data.chunks_exact(GROUP) {
            for (lane, state) in lanes.iter_mut().enumerate() {
                let at = lane * BLOCK;
                let block = u128::from_le_bytes(group[at..at + BLOCK].try_into().unwrap_or([0; BLOCK]));

                let low = vmull_p64(*state as u64, FOLD_LOW);
                let high = vmull_p64((*state >> 64) as u64, FOLD_HIGH);
                *state = low ^ high ^ block;
            }
        }

        let mut total = 0u64;
        for (lane, state) in lanes.iter().enumerate() {
            let low = *state as u64;
            let high = (*state >> 64) as u64;
            total ^= multiply(multiply(low, AHEAD) ^ high, LANE_SHIFT[lane]);
        }

        multiply(total, AHEAD)
    }
}
