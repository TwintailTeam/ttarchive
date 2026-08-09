use std::fs;
use std::io::Read;
use std::path::PathBuf;

use ttarchive::codecs::Level;
use ttarchive::codecs::deflate::{InflateReader, compress, decompress};

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/deflate")
}

const CASES: [&str; 9] = ["empty", "single_byte", "text", "repeat_one", "incompressible", "binary_struct", "long_matches", "max_distance", "mixed"];

fn read_case(name: &str, suffix: &str) -> (Vec<u8>, Vec<u8>) {
    let dir = fixture_dir();
    let raw = fs::read(dir.join(format!("{name}.raw"))).expect("raw fixture");
    let deflated = fs::read(dir.join(format!("{name}{suffix}.deflate"))).expect("deflate fixture");
    (raw, deflated)
}

#[test]
fn inflates_zlib_output() {
    for name in CASES {
        for suffix in ["", ".l0", ".l9"] {
            let (raw, deflated) = read_case(name, suffix);
            let got = decompress(&deflated, raw.len()).unwrap_or_else(|e| panic!("{name}{suffix}: {e}"));
            assert_eq!(got.len(), raw.len(), "{name}{suffix}: length");
            assert_eq!(got, raw, "{name}{suffix}: contents");
        }
    }
}

#[test]
fn inflates_with_tiny_reads() {
    struct Trickle<'a> {
        data: &'a [u8],
        chunk: usize,
    }
    impl Read for Trickle<'_> {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            let n = self.data.len().min(buf.len()).min(self.chunk);
            buf[..n].copy_from_slice(&self.data[..n]);
            self.data = &self.data[n..];
            Ok(n)
        }
    }

    for name in ["text", "repeat_one", "mixed", "max_distance"] {
        let (raw, deflated) = read_case(name, "");
        for chunk in [1usize, 3, 7, 64] {
            let mut out = Vec::new();
            InflateReader::new(Trickle { data: &deflated, chunk }).read_to_end(&mut out).unwrap_or_else(|e| panic!("{name} chunk {chunk}: {e}"));
            assert_eq!(out, raw, "{name} chunk {chunk}");
        }
    }
}

#[test]
fn inflates_with_tiny_output_reads() {
    let (raw, deflated) = read_case("binary_struct", "");
    let mut reader = InflateReader::new(&deflated[..]);
    let mut out = Vec::new();
    let mut buf = [0u8; 13];
    loop {
        let n = reader.read(&mut buf).unwrap();
        if n == 0 {
            break;
        }
        out.extend_from_slice(&buf[..n]);
    }
    assert_eq!(out, raw);
}

#[test]
fn round_trips_own_output() {
    for name in CASES {
        let (raw, _) = read_case(name, "");
        for level in [Level::None, Level::Fast, Level::Default, Level::Best] {
            let packed = compress(&raw, level);
            let got = decompress(&packed, raw.len()).unwrap_or_else(|e| panic!("{name} {level:?}: {e}"));
            assert_eq!(got, raw, "{name} {level:?}");
        }
    }
}

#[test]
fn achieves_reasonable_ratios() {
    let (text, _) = read_case("text", "");
    let packed = compress(&text, Level::Default);
    assert!(packed.len() < text.len() / 20, "highly repetitive text should compress >20x, got {} -> {}", text.len(), packed.len());

    let (rle, _) = read_case("repeat_one", "");
    let packed = compress(&rle, Level::Default);
    assert!(packed.len() < 1000, "100 KB of one repeated byte should compress to well under 1 KB, got {}", packed.len());

    let (noise, _) = read_case("incompressible", "");
    let packed = compress(&noise, Level::Default);
    assert!(packed.len() <= noise.len() + noise.len() / 1000 + 64, "random data grew too much: {} -> {}", noise.len(), packed.len());
}

#[test]
fn higher_levels_compress_at_least_as_well() {
    let (raw, _) = read_case("long_matches", "");
    let fast = compress(&raw, Level::Fast).len();
    let default = compress(&raw, Level::Default).len();
    let best = compress(&raw, Level::Best).len();

    assert!(default <= fast, "default {default} should not exceed fast {fast}");
    assert!(best <= default, "best {best} should not exceed default {default}");
}

#[test]
fn round_trips_multi_block_input() {
    let mut data = Vec::with_capacity(1_500_000);
    let mut x: u32 = 0x1234_5678;
    while data.len() < 1_500_000 {
        x = x.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        data.extend_from_slice(&x.to_le_bytes());
        if x.is_multiple_of(5) {
            data.extend_from_slice(b"a common repeated phrase ");
        }
    }

    let packed = compress(&data, Level::Default);
    assert_eq!(decompress(&packed, data.len()).unwrap(), data);
}

#[test]
fn round_trips_skewed_frequency_distributions() {
    for alphabet in [24usize, 40, 64, 96, 140, 200, 255] {
        let mut data: Vec<u8> = Vec::new();
        let (mut a, mut b) = (1u64, 1u64);
        for symbol in 0..alphabet {
            let count = a.min(4096);
            for _ in 0..count {
                data.push(symbol as u8);
            }
            let next = a.saturating_add(b);
            b = a;
            a = next;
        }

        let mut shuffled = Vec::with_capacity(data.len());
        let mut index = 0usize;
        while shuffled.len() < data.len() {
            shuffled.push(data[index % data.len()]);
            index = index.wrapping_add(7919);
        }

        for level in [Level::Fast, Level::Default, Level::Best] {
            let packed = compress(&shuffled, level);
            let got = decompress(&packed, shuffled.len()).unwrap_or_else(|e| panic!("alphabet {alphabet}, {level:?}: {e}"));
            assert_eq!(got, shuffled, "alphabet {alphabet}, {level:?}");
        }
    }
}

#[test]
fn round_trips_varied_random_inputs() {
    let mut seed = 0x2545_F491_4F6C_DD1Du64;
    let mut next = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };

    for case in 0..120 {
        let len = 1_000 + (next() % 90_000) as usize;
        let alphabet = 1 + (next() % 256) as usize;
        let skew = 1 + (next() % 6) as u32;

        let mut data = Vec::with_capacity(len);
        while data.len() < len {
            let mut value = (next() % alphabet as u64) as u32;
            for _ in 0..skew {
                value = value.min((next() % alphabet as u64) as u32);
            }
            data.push(value as u8);
        }

        for level in [Level::Fast, Level::Default, Level::Best] {
            let packed = compress(&data, level);
            let got = decompress(&packed, data.len()).unwrap_or_else(|e| panic!("case {case} (len {len}, alphabet {alphabet}, skew {skew}), {level:?}: {e}"));
            assert_eq!(got, data, "case {case}, {level:?}");
        }
    }
}

#[test]
fn rejects_truncated_streams() {
    let (_, deflated) = read_case("text", "");
    for cut in [1usize, 5, 20, deflated.len() - 1] {
        let err = decompress(&deflated[..cut], 0);
        assert!(err.is_err(), "truncating to {cut} bytes should fail");
    }
}

#[test]
fn rejects_reserved_block_type() {
    assert!(decompress(&[0b0000_0111], 0).is_err());
}

#[test]
fn rejects_bad_stored_length_check() {
    let data = [0b0000_0001, 0x04, 0x00, 0xFF, 0xFF, b'a', b'b', b'c', b'd'];
    assert!(decompress(&data, 0).is_err());
}

#[test]
fn rejects_distance_past_start_of_stream() {
    let mut bits: Vec<bool> = Vec::new();

    fn lsb(bits: &mut Vec<bool>, v: u32, n: u32) {
        for i in 0..n {
            bits.push((v >> i) & 1 == 1);
        }
    }
    fn msb(bits: &mut Vec<bool>, v: u32, n: u32) {
        for i in (0..n).rev() {
            bits.push((v >> i) & 1 == 1);
        }
    }

    lsb(&mut bits, 1, 1);
    lsb(&mut bits, 1, 2);

    msb(&mut bits, 0x30 + 0x61, 8);
    msb(&mut bits, 1, 7);
    msb(&mut bits, 29, 5);
    lsb(&mut bits, 32768 - 24577, 13);

    let mut bytes = vec![0u8; bits.len().div_ceil(8)];
    for (i, b) in bits.iter().enumerate() {
        if *b {
            bytes[i / 8] |= 1 << (i % 8);
        }
    }

    let err = decompress(&bytes, 0);
    assert!(err.is_err(), "distance beyond available history must be rejected");
}

#[test]
fn independently_compressed_chunks_concatenate() {
    use ttarchive::codecs::deflate::compress_chunk;

    for name in ["text", "binary_struct", "long_matches", "mixed", "incompressible"] {
        let (raw, _) = read_case(name, "");

        for chunk_size in [1024usize, 32 * 1024, 100_000] {
            if raw.is_empty() {
                continue;
            }
            let chunks: Vec<&[u8]> = raw.chunks(chunk_size).collect();

            let mut stream = Vec::new();
            for (i, chunk) in chunks.iter().enumerate() {
                let last = i + 1 == chunks.len();
                stream.extend_from_slice(&compress_chunk(chunk, Level::Default, last));
            }

            let got = decompress(&stream, raw.len()).unwrap_or_else(|e| panic!("{name} @ {chunk_size}: {e}"));
            assert_eq!(got, raw, "{name} @ {chunk_size}");
        }
    }
}

#[test]
fn single_final_chunk_matches_whole_stream() {
    use ttarchive::codecs::deflate::compress_chunk;

    for name in ["empty", "single_byte", "text"] {
        let (raw, _) = read_case(name, "");
        let chunked = compress_chunk(&raw, Level::Default, true);
        assert_eq!(decompress(&chunked, raw.len()).unwrap(), raw, "{name}");
    }
}
