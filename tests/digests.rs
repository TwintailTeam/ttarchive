use ttarchive::crypto::{md5, sha512};

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn pattern(len: usize, seed: u64) -> Vec<u8> {
    let mut out = Vec::with_capacity(len + 8);
    let mut state = seed | 1;
    while out.len() < len {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        out.extend_from_slice(&state.to_le_bytes());
    }
    out.truncate(len);
    out
}

#[test]
fn md5_known_vectors() {
    assert_eq!(hex(&md5::digest(b"")), "d41d8cd98f00b204e9800998ecf8427e");
    assert_eq!(hex(&md5::digest(b"a")), "0cc175b9c0f1b6a831c399e269772661");
    assert_eq!(hex(&md5::digest(b"abc")), "900150983cd24fb0d6963f7d28e17f72");
    assert_eq!(hex(&md5::digest(b"message digest")), "f96b697d7cb7938d525a2f31aaf161d0");
    assert_eq!(hex(&md5::digest(b"abcdefghijklmnopqrstuvwxyz")), "c3fcd3d76192e4007dfb496cca67e13b");
    assert_eq!(hex(&md5::digest(b"12345678901234567890123456789012345678901234567890123456789012345678901234567890")), "57edf4a22be3c955ac49da2e2107b67a");
}

#[test]
fn sha512_known_vectors() {
    assert_eq!(
        hex(&sha512::digest(b"")),
        "cf83e1357eefb8bdf1542850d66d8007d620e4050b5715dc83f4a921d36ce9ce47d0d13c5d85f2b0ff8318d2877eec2f63b931bd47417a81a538327af927da3e"
    );
    assert_eq!(
        hex(&sha512::digest(b"abc")),
        "ddaf35a193617abacc417349ae20413112e6fa4e89a97ea20a9eeee64b55d39a2192992a274fc1a836ba3c23a3feebbd454d4423643ce80e2a9ac94fa54ca49f"
    );
    assert_eq!(
        hex(&sha512::digest(b"abcdefghbcdefghicdefghijdefghijkefghijklfghijklmghijklmnhijklmnoijklmnopjklmnopqklmnopqrlmnopqrsmnopqrstnopqrstu")),
        "8e959b75dae313da8cf4f72814fc143f8f7779c6eb9f7fa17299aeadb6889018501d289e4900f7e4331b99dec4b5433ac7d329eeb6dd26545e96e55b874be909"
    );
}

#[test]
fn streaming_matches_one_shot_at_every_split() {
    let data = pattern(600, 9);

    for split in 0..data.len() {
        let mut a = md5::Md5::new();
        a.update(&data[..split]);
        a.update(&data[split..]);
        assert_eq!(a.finish(), md5::digest(&data), "md5 split at {split}");

        let mut b = sha512::Sha512::new();
        b.update(&data[..split]);
        b.update(&data[split..]);
        assert_eq!(b.finish(), sha512::digest(&data), "sha512 split at {split}");
    }
}

#[test]
fn lengths_around_every_block_boundary_are_padded_correctly() {
    let data = pattern(600, 3);

    for len in 0..=520 {
        let slice = &data[..len];
        let mut md5_stream = md5::Md5::new();
        for chunk in slice.chunks(7) {
            md5_stream.update(chunk);
        }
        assert_eq!(md5_stream.finish(), md5::digest(slice), "md5 in sevens at {len} bytes");

        let mut sha_stream = sha512::Sha512::new();
        for chunk in slice.chunks(7) {
            sha_stream.update(chunk);
        }
        assert_eq!(sha_stream.finish(), sha512::digest(slice), "sha512 in sevens at {len} bytes");
    }
}

#[cfg(any(target_arch = "x86", target_arch = "x86_64", target_arch = "aarch64"))]
#[test]
fn the_hardware_sha_paths_agree_with_the_software_ones() {
    use ttarchive::crypto::{sha_ni, sha1, sha256};

    let sha256_hardware = sha_ni::sha256_available();
    let sha1_hardware = sha_ni::sha1_available();

    if !sha256_hardware && !sha1_hardware {
        eprintln!("skipping: this cpu offers no sha extensions, so only the software paths ran");
        return;
    }

    let data = pattern(100_000, 0xfeed);
    for len in [0usize, 1, 55, 56, 63, 64, 65, 127, 128, 129, 1_000, 65_536, 100_000] {
        let slice = &data[..len];
        assert_eq!(hex(&sha256::digest(slice)), hex(&software_sha256(slice)), "sha256 hardware disagrees at {len} bytes");
        assert_eq!(hex(&sha1::digest(slice)), hex(&software_sha1(slice)), "sha1 hardware disagrees at {len} bytes");
    }
}

#[cfg(any(target_arch = "x86", target_arch = "x86_64", target_arch = "aarch64"))]
fn software_sha256(data: &[u8]) -> [u8; 32] {
    let mut state = [0x6a09_e667u32, 0xbb67_ae85, 0x3c6e_f372, 0xa54f_f53a, 0x510e_527f, 0x9b05_688c, 0x1f83_d9ab, 0x5be0_cd19];
    let padded = pad_be(data, 64, 8);
    for block in padded.chunks_exact(64) {
        ttarchive::crypto::sha256::software_block(&mut state, block.try_into().unwrap());
    }
    let mut out = [0u8; 32];
    for (slot, word) in out.chunks_exact_mut(4).zip(state) {
        slot.copy_from_slice(&word.to_be_bytes());
    }
    out
}

#[cfg(any(target_arch = "x86", target_arch = "x86_64", target_arch = "aarch64"))]
fn software_sha1(data: &[u8]) -> [u8; 20] {
    let mut state = [0x6745_2301u32, 0xefcd_ab89, 0x98ba_dcfe, 0x1032_5476, 0xc3d2_e1f0];
    let padded = pad_be(data, 64, 8);
    for block in padded.chunks_exact(64) {
        ttarchive::crypto::sha1::software_block(&mut state, block.try_into().unwrap());
    }
    let mut out = [0u8; 20];
    for (slot, word) in out.chunks_exact_mut(4).zip(state) {
        slot.copy_from_slice(&word.to_be_bytes());
    }
    out
}

#[cfg(any(target_arch = "x86", target_arch = "x86_64", target_arch = "aarch64"))]
fn pad_be(data: &[u8], block: usize, length_bytes: usize) -> Vec<u8> {
    let mut out = data.to_vec();
    out.push(0x80);
    while (out.len() + length_bytes) % block != 0 {
        out.push(0);
    }
    let bits = (data.len() as u64).wrapping_mul(8);
    out.extend_from_slice(&bits.to_be_bytes()[8 - length_bytes.min(8)..]);
    out
}
