#![cfg(any(target_arch = "x86", target_arch = "x86_64", target_arch = "aarch64"))]

use std::sync::OnceLock;

use crate::crypto::{sha1, sha256};

pub fn sha256_available() -> bool {
    static READY: OnceLock<bool> = OnceLock::new();
    *READY.get_or_init(|| detected() && sha256_agrees())
}

pub fn sha1_available() -> bool {
    static READY: OnceLock<bool> = OnceLock::new();
    *READY.get_or_init(|| detected() && sha1_agrees())
}

#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
fn detected() -> bool {
    is_x86_feature_detected!("sha") && is_x86_feature_detected!("sse2") && is_x86_feature_detected!("ssse3") && is_x86_feature_detected!("sse4.1")
}

#[cfg(target_arch = "aarch64")]
fn detected() -> bool {
    std::arch::is_aarch64_feature_detected!("sha2") && std::arch::is_aarch64_feature_detected!("neon")
}

fn shapes() -> [[u8; 64]; 4] {
    let mut counted = [0u8; 64];
    for (index, slot) in counted.iter_mut().enumerate() {
        *slot = (index as u8).wrapping_mul(31).wrapping_add(7);
    }

    let mut padded = [0u8; 64];
    padded[0] = b'a';
    padded[1] = 0x80;
    padded[63] = 8;

    [[0u8; 64], [0xffu8; 64], counted, padded]
}

fn sha256_agrees() -> bool {
    let start = [0x6a09_e667u32, 0xbb67_ae85, 0x3c6e_f372, 0xa54f_f53a, 0x510e_527f, 0x9b05_688c, 0x1f83_d9ab, 0x5be0_cd19];

    shapes().into_iter().all(|block| {
        let mut hardware = start;
        sha256_block(&mut hardware, &block);

        let mut software = start;
        sha256::software_block(&mut software, &block);

        hardware == software
    })
}

fn sha1_agrees() -> bool {
    let start = [0x6745_2301u32, 0xefcd_ab89, 0x98ba_dcfe, 0x1032_5476, 0xc3d2_e1f0];

    shapes().into_iter().all(|block| {
        let mut hardware = start;
        sha1_block(&mut hardware, &block);

        let mut software = start;
        sha1::software_block(&mut software, &block);

        hardware == software
    })
}

pub fn sha256_block(state: &mut [u32; 8], block: &[u8; 64]) {
    let w = sha256::schedule(block);
    unsafe { sha256_rounds(state, &w) };
}

pub fn sha1_block(state: &mut [u32; 5], block: &[u8; 64]) {
    let w = sha1::schedule(block);
    unsafe { sha1_rounds(state, &w) };
}

#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
#[target_feature(enable = "sha,sse2,ssse3,sse4.1")]
unsafe fn sha256_rounds(state: &mut [u32; 8], w: &[u32; 64]) {
    #[cfg(target_arch = "x86")]
    use std::arch::x86::*;
    #[cfg(target_arch = "x86_64")]
    use std::arch::x86_64::*;

    unsafe {
        let k = sha256::constants();

        let abcd = _mm_loadu_si128(state.as_ptr() as *const __m128i);
        let efgh = _mm_loadu_si128(state.as_ptr().add(4) as *const __m128i);

        let shuffled = _mm_shuffle_epi32(abcd, 0xb1);
        let reversed = _mm_shuffle_epi32(efgh, 0x1b);
        let mut abef = _mm_alignr_epi8(shuffled, reversed, 8);
        let mut cdgh = _mm_blend_epi16(reversed, shuffled, 0xf0);

        let kept_abef = abef;
        let kept_cdgh = cdgh;

        for quartet in 0..16 {
            let at = quartet * 4;
            let packed = _mm_set_epi32(
                w[at + 3].wrapping_add(k[at + 3]) as i32,
                w[at + 2].wrapping_add(k[at + 2]) as i32,
                w[at + 1].wrapping_add(k[at + 1]) as i32,
                w[at].wrapping_add(k[at]) as i32,
            );

            cdgh = _mm_sha256rnds2_epu32(cdgh, abef, packed);
            abef = _mm_sha256rnds2_epu32(abef, cdgh, _mm_shuffle_epi32(packed, 0x0e));
        }

        abef = _mm_add_epi32(abef, kept_abef);
        cdgh = _mm_add_epi32(cdgh, kept_cdgh);

        let folded = _mm_shuffle_epi32(abef, 0x1b);
        let other = _mm_shuffle_epi32(cdgh, 0xb1);

        _mm_storeu_si128(state.as_mut_ptr() as *mut __m128i, _mm_blend_epi16(folded, other, 0xf0));
        _mm_storeu_si128(state.as_mut_ptr().add(4) as *mut __m128i, _mm_alignr_epi8(other, folded, 8));
    }
}

#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
#[target_feature(enable = "sha,sse2,ssse3,sse4.1")]
unsafe fn sha1_rounds(state: &mut [u32; 5], w: &[u32; 80]) {
    #[cfg(target_arch = "x86")]
    use std::arch::x86::*;
    #[cfg(target_arch = "x86_64")]
    use std::arch::x86_64::*;

    unsafe {
        let mut abcd = _mm_shuffle_epi32(_mm_loadu_si128(state.as_ptr() as *const __m128i), 0x1b);
        let mut carry = _mm_set_epi32(state[4] as i32, 0, 0, 0);

        let kept_abcd = abcd;
        let kept_carry = carry;

        let group = |at: usize| _mm_set_epi32(w[at] as i32, w[at + 1] as i32, w[at + 2] as i32, w[at + 3] as i32);

        carry = _mm_add_epi32(carry, group(0));
        let mut held = abcd;
        abcd = _mm_sha1rnds4_epu32(abcd, carry, 0);
        carry = held;

        for quartet in 1..5 {
            carry = _mm_sha1nexte_epu32(carry, group(quartet * 4));
            held = abcd;
            abcd = _mm_sha1rnds4_epu32(abcd, carry, 0);
            carry = held;
        }
        for quartet in 5..10 {
            carry = _mm_sha1nexte_epu32(carry, group(quartet * 4));
            held = abcd;
            abcd = _mm_sha1rnds4_epu32(abcd, carry, 1);
            carry = held;
        }
        for quartet in 10..15 {
            carry = _mm_sha1nexte_epu32(carry, group(quartet * 4));
            held = abcd;
            abcd = _mm_sha1rnds4_epu32(abcd, carry, 2);
            carry = held;
        }
        for quartet in 15..20 {
            carry = _mm_sha1nexte_epu32(carry, group(quartet * 4));
            held = abcd;
            abcd = _mm_sha1rnds4_epu32(abcd, carry, 3);
            carry = held;
        }

        carry = _mm_sha1nexte_epu32(carry, kept_carry);
        abcd = _mm_add_epi32(abcd, kept_abcd);

        _mm_storeu_si128(state.as_mut_ptr() as *mut __m128i, _mm_shuffle_epi32(abcd, 0x1b));
        state[4] = _mm_extract_epi32(carry, 3) as u32;
    }
}

#[cfg(target_arch = "aarch64")]
#[target_feature(enable = "sha2,neon")]
unsafe fn sha256_rounds(state: &mut [u32; 8], w: &[u32; 64]) {
    use std::arch::aarch64::*;

    unsafe {
        let k = sha256::constants();

        let mut abcd = vld1q_u32(state.as_ptr());
        let mut efgh = vld1q_u32(state.as_ptr().add(4));
        let kept_abcd = abcd;
        let kept_efgh = efgh;

        for quartet in 0..16 {
            let at = quartet * 4;
            let scheduled = vaddq_u32(vld1q_u32(w.as_ptr().add(at)), vld1q_u32(k.as_ptr().add(at)));

            let carried = abcd;
            abcd = vsha256hq_u32(abcd, efgh, scheduled);
            efgh = vsha256h2q_u32(efgh, carried, scheduled);
        }

        vst1q_u32(state.as_mut_ptr(), vaddq_u32(abcd, kept_abcd));
        vst1q_u32(state.as_mut_ptr().add(4), vaddq_u32(efgh, kept_efgh));
    }
}

#[cfg(target_arch = "aarch64")]
#[target_feature(enable = "sha2,neon")]
unsafe fn sha1_rounds(state: &mut [u32; 5], w: &[u32; 80]) {
    use std::arch::aarch64::*;

    unsafe {
        let mut abcd = vld1q_u32(state.as_ptr());
        let mut carry = state[4];
        let kept_abcd = abcd;
        let kept_carry = carry;

        for quartet in 0..20 {
            let at = quartet * 4;
            let scheduled = vld1q_u32(w.as_ptr().add(at));
            let next = vsha1h_u32(vgetq_lane_u32(abcd, 0));

            abcd = match quartet / 5 {
                0 => vsha1cq_u32(abcd, carry, scheduled),
                1 => vsha1pq_u32(abcd, carry, scheduled),
                2 => vsha1mq_u32(abcd, carry, scheduled),
                _ => vsha1pq_u32(abcd, carry, scheduled),
            };
            carry = next;
        }

        vst1q_u32(state.as_mut_ptr(), vaddq_u32(abcd, kept_abcd));
        state[4] = carry.wrapping_add(kept_carry);
    }
}
