use ttarchive::codecs::bzip2;
use ttarchive::codecs::bzip2::encode::burrows_wheeler;

fn brute_force(block: &[u8]) -> Vec<u8> {
    let n = block.len();
    let mut rows: Vec<usize> = (0..n).collect();
    rows.sort_by(|&a, &b| {
        for k in 0..n {
            let left = block[(a + k) % n];
            let right = block[(b + k) % n];
            if left != right {
                return left.cmp(&right);
            }
        }
        std::cmp::Ordering::Equal
    });
    rows.iter().map(|&index| block[(index + n - 1) % n]).collect()
}

fn pseudo_random(len: usize, seed: u32, alphabet: u8) -> Vec<u8> {
    let mut state = seed | 1;
    (0..len)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            (state % alphabet as u32) as u8
        })
        .collect()
}

#[test]
fn the_last_column_matches_a_brute_force_rotation_sort() {
    let mut cases: Vec<Vec<u8>> = vec![
        Vec::new(),
        vec![0],
        vec![7, 7, 7, 7, 7, 7, 7, 7],
        b"banana".to_vec(),
        b"mississippi".to_vec(),
        b"abababababababab".to_vec(),
        b"the quick brown fox".to_vec(),
    ];

    for seed in 1..40u32 {
        for alphabet in [2u8, 3, 17, 255] {
            cases.push(pseudo_random((seed as usize * 7) % 300 + 1, seed, alphabet));
        }
    }

    for period in [2usize, 3, 5, 65] {
        let unit: Vec<u8> = (0..period).map(|i| b'a' + (i % 26) as u8).collect();
        for total in [period * 4, period * 40 + 1, period * 41 - 1] {
            cases.push(unit.iter().copied().cycle().take(total).collect());
        }
    }

    for block in &cases {
        let (last, origin) = burrows_wheeler(block);
        assert_eq!(last, brute_force(block), "last column differs for {block:?}");
        if !block.is_empty() {
            assert!(origin < block.len(), "origin {origin} outside a {} byte block", block.len());
        }
    }
}

#[test]
fn the_origin_row_reconstructs_the_block() {
    for seed in 1..60u32 {
        for alphabet in [2u8, 4, 200] {
            let block = pseudo_random((seed as usize * 13) % 500 + 1, seed, alphabet);
            let (last, origin) = burrows_wheeler(&block);
            assert_eq!(inverse(&last, origin), block, "inverse failed for seed {seed}/{alphabet}");
        }
    }

    let periodic: Vec<u8> = b"the quick brown fox jumps over the lazy dog\n".iter().copied().cycle().take(9_997).collect();
    let (last, origin) = burrows_wheeler(&periodic);
    assert_eq!(inverse(&last, origin), periodic);
}

fn inverse(last: &[u8], origin: usize) -> Vec<u8> {
    let n = last.len();
    if n == 0 {
        return Vec::new();
    }

    let mut counts = [0u32; 256];
    for &byte in last {
        counts[byte as usize] += 1;
    }
    let mut running = 0u32;
    let mut starts = [0u32; 256];
    for byte in 0..256 {
        starts[byte] = running;
        running += counts[byte];
    }

    let mut next = vec![0u32; n];
    for (index, &byte) in last.iter().enumerate() {
        next[starts[byte as usize] as usize] = index as u32;
        starts[byte as usize] += 1;
    }

    let mut out = Vec::with_capacity(n);
    let mut position = next[origin];
    for _ in 0..n {
        out.push(last[position as usize]);
        position = next[position as usize];
    }
    out
}

#[test]
fn adversarial_shapes_round_trip_through_bzip2() {
    let block = 900_000usize;
    let cases: Vec<(&str, Vec<u8>)> = vec![
        ("single byte repeated past a block", vec![b'q'; block + 5_000]),
        ("two symbols alternating", (0..block + 1_111).map(|i| if i % 2 == 0 { b'a' } else { b'b' }).collect()),
        ("period 65 text", b"the quick brown fox jumps over the lazy dog and keeps on running\n".iter().copied().cycle().take(block + 313).collect()),
        ("exactly one block", vec![b'z'; block - 19]),
        ("incompressible", pseudo_random(400_000, 99, 255)),
        ("long runs then noise", {
            let mut data = vec![b'r'; 300_000];
            data.extend(pseudo_random(200_000, 5, 255));
            data
        }),
        ("all 256 symbols cycling", (0..300_000u32).map(|i| (i % 256) as u8).collect()),
    ];

    for (name, data) in &cases {
        let packed = bzip2::compress(data, 9).unwrap_or_else(|e| panic!("{name}: compress failed: {e}"));
        let restored = bzip2::decompress(&packed, data.len()).unwrap_or_else(|e| panic!("{name}: decompress failed: {e}"));
        assert!(restored == *data, "{name}: round trip changed the bytes");
    }
}

#[test]
fn every_block_size_round_trips() {
    let data: Vec<u8> = (0..750_000u32).map(|i| (i.wrapping_mul(2654435761) >> 19) as u8).collect();

    for level in 1..=9u8 {
        let packed = bzip2::compress(&data, level).unwrap_or_else(|e| panic!("level {level}: {e}"));
        let restored = bzip2::decompress(&packed, data.len()).unwrap_or_else(|e| panic!("level {level}: {e}"));
        assert!(restored == data, "level {level}: round trip changed the bytes");
    }
}
