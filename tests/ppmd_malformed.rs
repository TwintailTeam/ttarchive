mod common;

use std::io::Read;
use std::process::Command;

use common::{TempDir, compressible, have, pseudo_random, skip};
use ttarchive::codecs::ppmd;
use ttarchive::sevenz::SevenZReader;

fn started(bytes: &[u8]) -> Vec<u8> {
    let mut out = vec![0u8];
    out.extend_from_slice(bytes);
    out
}

fn decode_h(bytes: &[u8], order: u32, mem_size: u32, expected: u64) -> (Vec<u8>, Option<String>) {
    let mut reader = match ppmd::h::Reader::new(bytes, expected, order, mem_size) {
        Ok(reader) => reader,
        Err(e) => return (Vec::new(), Some(e.to_string())),
    };

    let mut out = Vec::new();
    match reader.read_to_end(&mut out) {
        Ok(_) => (out, None),
        Err(e) => (out, Some(e.to_string())),
    }
}

fn decode_i(bytes: &[u8], expected: u64) -> Result<Vec<u8>, String> {
    ppmd::i::decompress(bytes, expected).map_err(|e| e.to_string())
}

fn packed_ppmd_folder(dir: &TempDir) -> (Vec<u8>, u64, Vec<u8>) {
    dir.write("src/prose.txt", compressible(20_000));
    dir.write("src/noise.bin", pseudo_random(6_000, 5));

    let args = ["a", "-mhc=off", "-m0=PPMd", "ppmd.7z", "src"];
    let output = Command::new(common::resolve("7z")).args(args).current_dir(dir.path()).output().expect("7z");
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));

    let path = dir.join("ppmd.7z");
    let reader = SevenZReader::open(&path).unwrap();
    let folder = &reader.header().streams.folders[0];
    let properties = folder.coders[0].properties.clone();
    let size = folder.unpack_size().unwrap();
    let packed = reader.header().streams.pack.sizes[0] as usize;
    let start = 32 + reader.header().streams.pack.pos as usize;

    (properties, size, std::fs::read(&path).unwrap()[start..start + packed].to_vec())
}

fn decode_with(properties: &[u8], packed: &[u8], expected: u64) -> (Vec<u8>, Option<String>) {
    let mut reader = match ppmd::h::from_properties(packed, properties, expected) {
        Ok(reader) => reader,
        Err(e) => return (Vec::new(), Some(e.to_string())),
    };

    let mut out = Vec::new();
    match reader.read_to_end(&mut out) {
        Ok(_) => (out, None),
        Err(e) => (out, Some(e.to_string())),
    }
}

#[test]
fn a_stream_that_opens_correctly_and_then_turns_to_noise_never_panics() {
    let mut symbols = 0usize;

    for seed in 0..64u32 {
        let bytes = started(&pseudo_random(4_096, seed));

        for order in [2u32, 3, 6, 16, 32, 64] {
            for mem_size in [2_048u32, 1 << 16, 1 << 20] {
                symbols += decode_h(&bytes, order, mem_size, 100_000).0.len();
            }
        }
    }

    assert!(symbols > 100_000, "only {symbols} symbols came out of the model, so this tested the door rather than the model");
}

#[test]
fn a_stream_of_one_repeated_byte_never_panics_at_any_order() {
    for filler in [0x00u8, 0x01, 0x7F, 0x80, 0xFE, 0xFF] {
        let bytes = started(&vec![filler; 8_192]);

        for order in [2u32, 4, 16, 64] {
            for mem_size in [2_048u32, 1 << 16] {
                let _ = decode_h(&bytes, order, mem_size, 200_000);
            }
        }
    }
}

#[test]
fn the_smallest_sub_allocator_the_format_allows_survives_a_stream_that_outgrows_it() {
    for seed in 0..16u32 {
        let bytes = started(&pseudo_random(16_384, seed));
        let _ = decode_h(&bytes, 64, 2_048, 500_000);
    }
}

#[test]
fn an_order_or_memory_size_outside_the_format_is_refused_rather_than_attempted() {
    let bytes = started(&pseudo_random(256, 1));

    for order in [0u32, 1, 65, 100, u32::MAX] {
        assert!(decode_h(&bytes, order, 1 << 16, 1_000).1.is_some(), "order {order} should be refused");
    }
    for mem_size in [0u32, 1, 2_047] {
        assert!(decode_h(&bytes, 8, mem_size, 1_000).1.is_some(), "a {mem_size} byte sub-allocator should be refused");
    }
}

#[test]
fn arbitrary_bytes_never_panic_the_variant_i_decoder() {
    for seed in 0..40u32 {
        let bytes = pseudo_random(2_048, seed);
        for expected in [1_000u64, 50_000] {
            let _ = decode_i(&bytes, expected);
        }
    }
}

#[test]
fn a_real_folder_decodes_to_exactly_what_went_into_it() {
    if !have("7z") {
        return skip("7z");
    }

    let dir = TempDir::new("ppmd-good");
    let (properties, expected, packed) = packed_ppmd_folder(&dir);

    let (decoded, error) = decode_with(&properties, &packed, expected);
    assert!(error.is_none(), "a real folder should decode: {error:?}");

    let mut whole = pseudo_random(6_000, 5);
    whole.extend_from_slice(&compressible(20_000));
    assert_eq!(decoded.len() as u64, expected);
    assert_eq!(decoded, whole);
}

#[test]
fn every_truncation_of_a_real_folder_fails_without_panicking() {
    if !have("7z") {
        return skip("7z");
    }

    let dir = TempDir::new("ppmd-truncate");
    let (properties, expected, packed) = packed_ppmd_folder(&dir);

    for len in (0..packed.len()).step_by(211) {
        let (_, error) = decode_with(&properties, &packed[..len], expected);
        assert!(error.is_some(), "a folder cut to {len} of {} bytes decoded anyway", packed.len());
    }
}

#[test]
fn every_bit_flip_in_a_real_folder_is_an_error_or_wrong_bytes_but_never_a_panic() {
    if !have("7z") {
        return skip("7z");
    }

    let dir = TempDir::new("ppmd-flip");
    let (properties, expected, packed) = packed_ppmd_folder(&dir);

    for at in (0..packed.len()).step_by(53) {
        for bit in [0u8, 7] {
            let mut bytes = packed.clone();
            bytes[at] ^= 1 << bit;
            let _ = decode_with(&properties, &bytes, expected);
        }
    }
}

#[test]
fn a_folder_whose_declared_size_was_raised_does_not_read_past_its_own_model() {
    if !have("7z") {
        return skip("7z");
    }

    let dir = TempDir::new("ppmd-size");
    let (properties, expected, packed) = packed_ppmd_folder(&dir);

    for factor in [2u64, 17, 1_000] {
        let (_, error) = decode_with(&properties, &packed, expected * factor);
        assert!(error.is_some(), "claiming {factor} times the real size should not succeed");
    }
}

#[test]
fn properties_that_name_a_different_model_than_the_stream_was_written_for_fail_cleanly() {
    if !have("7z") {
        return skip("7z");
    }

    let dir = TempDir::new("ppmd-props");
    let (properties, expected, packed) = packed_ppmd_folder(&dir);

    for order in [2u8, 5, 33, 64] {
        let mut altered = properties.clone();
        altered[0] = order;
        let _ = decode_with(&altered, &packed, expected);
    }

    for mem_size in [2_048u32, 1 << 14, 1 << 26] {
        let mut altered = properties.clone();
        altered[1..5].copy_from_slice(&mem_size.to_le_bytes());
        let _ = decode_with(&altered, &packed, expected);
    }
}
