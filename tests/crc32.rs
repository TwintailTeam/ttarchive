use ttarchive::utils::crc32::{Crc32, checksum};

#[test]
fn known_vectors() {
    assert_eq!(checksum(b"123456789"), 0xCBF4_3926);
    assert_eq!(checksum(b""), 0x0000_0000);
    assert_eq!(checksum(b"a"), 0xE8B7_BE43);
    assert_eq!(checksum(b"The quick brown fox jumps over the lazy dog"), 0x414F_A339);
}

#[test]
fn streaming_matches_one_shot_at_every_split() {
    let data: Vec<u8> = (0..1000u32).map(|i| (i.wrapping_mul(31) & 0xff) as u8).collect();
    let expected = checksum(&data);

    for split in 0..data.len() {
        let mut c = Crc32::new();
        c.update(&data[..split]);
        c.update(&data[split..]);
        assert_eq!(c.finish(), expected, "split at {split}");
    }
}

#[test]
fn resume_continues_a_finished_checksum() {
    let mut a = Crc32::new();
    a.update(b"hello ");

    let mut b = Crc32::resume(a.finish());
    b.update(b"world");

    assert_eq!(b.finish(), checksum(b"hello world"));
}

#[test]
fn handles_large_input() {
    let data = vec![0xA5u8; 1 << 20];
    let mut c = Crc32::new();
    c.update(&data);
    let mut d = Crc32::new();
    for chunk in data.chunks(7919) {
        d.update(chunk);
    }
    assert_eq!(c.finish(), d.finish());
}

#[test]
fn combine_matches_sequential_checksum() {
    let data: Vec<u8> = (0..50_000u32).map(|i| (i.wrapping_mul(2654435761) >> 24) as u8).collect();
    let whole = checksum(&data);

    for split in [0usize, 1, 2, 15, 16, 17, 1000, 4096, 25_000, 49_999, 50_000] {
        let (a, b) = data.split_at(split);
        let combined = ttarchive::utils::crc32::combine(checksum(a), checksum(b), b.len() as u64);
        assert_eq!(combined, whole, "split at {split}");
    }
}

#[test]
fn combine_across_many_chunks() {
    let data: Vec<u8> = (0..100_000u32).map(|i| (i % 251) as u8).collect();
    let whole = checksum(&data);

    for chunk in [1usize, 7, 1024, 9973] {
        let mut acc = 0u32;
        let mut first = true;
        for piece in data.chunks(chunk) {
            let c = checksum(piece);
            acc = if first {
                first = false;
                c
            } else {
                ttarchive::utils::crc32::combine(acc, c, piece.len() as u64)
            };
        }
        assert_eq!(acc, whole, "chunk size {chunk}");
    }
}
