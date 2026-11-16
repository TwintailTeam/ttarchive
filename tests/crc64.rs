use ttarchive::utils::crc64::{self, Crc64, checksum};

fn pattern(len: usize, seed: u64) -> Vec<u8> {
    let mut out = Vec::with_capacity(len);
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
fn known_vectors() {
    assert_eq!(checksum(b""), 0x0000_0000_0000_0000);
    assert_eq!(checksum(b"123456789"), 0x995D_C9BB_DF19_39FA);
    assert_eq!(checksum(b"a"), 0x3302_8477_2E65_2B05);
    assert_eq!(checksum(b"The quick brown fox jumps over the lazy dog"), 0x5B5E_B8C2_E54A_A1C4);
}

#[test]
fn streaming_matches_one_shot_at_every_split() {
    let data = pattern(2_000, 7);
    let expected = checksum(&data);

    for split in 0..data.len() {
        let mut c = Crc64::new();
        c.update(&data[..split]);
        c.update(&data[split..]);
        assert_eq!(c.finish(), expected, "split at {split}");
    }
}

#[test]
fn every_path_agrees_at_every_length() {
    let data = pattern(70_000, 0x5eed);

    for len in 0..=4_096 {
        let slice = &data[..len];
        let table = crc64::sliced(u64::MAX, slice);
        assert_eq!(crc64::striped(u64::MAX, slice), table, "striped disagrees at {len} bytes");
        assert_eq!(checksum(slice), !table, "checksum disagrees at {len} bytes");
    }

    for len in [4_097, 8_191, 8_192, 8_193, 16_384, 32_768, 65_535, 65_536, 69_999] {
        let slice = &data[..len];
        let table = crc64::sliced(u64::MAX, slice);
        assert_eq!(crc64::striped(u64::MAX, slice), table, "striped disagrees at {len} bytes");
        assert_eq!(checksum(slice), !table, "checksum disagrees at {len} bytes");
    }
}

#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
#[test]
fn the_carryless_path_agrees_with_the_table_at_every_length() {
    use ttarchive::utils::crc64_clmul;

    if !crc64_clmul::available() {
        eprintln!("skipping: this cpu has no pclmulqdq");
        return;
    }

    let data = pattern(70_000, 0xc0ffee);

    for len in 0..=4_096 {
        let slice = &data[..len];
        assert_eq!(crc64_clmul::update(u64::MAX, slice), crc64::sliced(u64::MAX, slice), "carryless disagrees at {len} bytes");
    }

    for len in [4_097, 8_191, 8_192, 8_193, 12_345, 16_384, 65_535, 65_536, 69_999] {
        let slice = &data[..len];
        assert_eq!(crc64_clmul::update(u64::MAX, slice), crc64::sliced(u64::MAX, slice), "carryless disagrees at {len} bytes");
    }

    for seed in [1u64, 0xdead_beef, u64::MAX] {
        let slice = &data[..40_000];
        assert_eq!(crc64_clmul::update(seed, slice), crc64::sliced(seed, slice), "carryless disagrees with seed {seed:#x}");
    }
}

#[test]
fn the_field_arithmetic_behaves_like_the_table() {
    assert_eq!(crc64::multiply(crc64::ONE, crc64::ONE), crc64::ONE);
    assert_eq!(crc64::multiply(crc64::power_of_x(64), crc64::inverse_power_of_x(64)), crc64::ONE);

    for bytes in [1usize, 2, 3, 7, 8, 9, 64] {
        let zeros = vec![0u8; bytes];
        let shifted = crc64::multiply(u64::MAX, crc64::power_of_x(8 * bytes as u64));
        assert_eq!(shifted, crc64::sliced(u64::MAX, &zeros), "multiplying by x^{} is not {bytes} zero bytes", 8 * bytes);
    }
}
