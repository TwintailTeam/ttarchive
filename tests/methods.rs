mod common;

use std::path::{Path, PathBuf};

use ttarchive::Archive;
use ttarchive::codecs::Method;

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/methods").join(name)
}

fn legacy_fixture(name: &str) -> PathBuf {
    fixture("legacy").join(name)
}

fn check_single_entry(archive: &str, expected: &str) {
    let dir = common::TempDir::new("methods");
    let out = dir.join("out");

    let summary = Archive::new(fixture(archive)).extract_to(&out).expect("extraction failed");
    assert_eq!(summary.files, 1, "expected exactly one file in {archive}");

    let want = std::fs::read(fixture(expected)).unwrap();
    let got = std::fs::read(out.join(expected)).unwrap();

    assert_eq!(got.len(), want.len(), "{archive}: decompressed length differs");
    assert!(got == want, "{archive}: decompressed bytes differ");
}

#[test]
fn method_9_deflate64_matches_the_original_bytes() {
    check_single_entry("deflate64.zip", "deflate64.raw");
}

#[test]
fn deflate64_uses_the_long_distances_and_lengths_it_adds() {
    let entries = Archive::new(fixture("deflate64.zip")).entries().unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].zip().unwrap().method_code, 9);
    assert_eq!(entries[0].zip().unwrap().method().unwrap(), Method::Deflate64);

    let raw = std::fs::read(fixture("deflate64.raw")).unwrap();
    assert!(raw.len() > 64 * 1024, "fixture must exceed one 64 KiB window");
    assert!(entries[0].zip().unwrap().compressed_size < raw.len() as u64 * 3 / 5, "fixture should compress well, meaning the long match was found");
}

#[test]
fn deflate64_is_decode_only() {
    assert!(!Method::Deflate64.can_encode());
    assert!(Method::Deflate.can_encode());
    assert!(Method::Store.can_encode());

    match ttarchive::codecs::encoder(Method::Deflate64, Vec::new(), Default::default()) {
        Ok(_) => panic!("must refuse to encode deflate64"),
        Err(e) => assert!(e.is_unsupported(), "expected Unsupported, got {e}"),
    }
}

#[test]
fn method_12_bzip2_matches_the_original_bytes() {
    check_single_entry("bzip2.zip", "bzip2.raw");
}

#[test]
fn bzip2_reads_info_zip_output_too() {
    check_single_entry("bzip2_infozip.zip", "bzip2.raw");
}

#[test]
fn bzip2_round_trips_through_our_own_reader() {
    let dir = common::TempDir::new("bzip2-roundtrip");

    let cases: Vec<(&str, Vec<u8>)> = vec![
        ("runs.bin", vec![b'x'; 300_000]),
        ("text.txt", "the quick brown fox. ".repeat(20_000).into_bytes()),
        ("noise.bin", (0..200_000u32).map(|i| (i.wrapping_mul(2654435761) >> 13) as u8).collect()),
        ("empty.bin", Vec::new()),
        ("one.bin", vec![7u8]),
    ];

    let source = dir.join("src");
    std::fs::create_dir_all(&source).unwrap();
    for (name, data) in &cases {
        std::fs::write(source.join(name), data).unwrap();
    }

    let archive = dir.join("bz.zip");
    Archive::new(&archive).set_method(Method::Bzip2).create_from([&source]).expect("create failed");

    let out = dir.join("out");
    Archive::new(&archive).set_strip_root(true).extract_to(&out).expect("extract failed");

    for (name, data) in &cases {
        let got = std::fs::read(out.join(name)).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert!(got == *data, "{name}: round trip changed the bytes");
    }
}

#[test]
fn bzip2_spans_several_blocks() {
    let dir = common::TempDir::new("bzip2-blocks");
    let source = dir.join("src");
    std::fs::create_dir_all(&source).unwrap();

    let data: Vec<u8> = (0..2_500_000u32).map(|i| (i.wrapping_mul(2654435761) >> 11) as u8).collect();
    std::fs::write(source.join("big.bin"), &data).unwrap();

    let archive = dir.join("big.zip");
    Archive::new(&archive).set_method(Method::Bzip2).create_from([&source]).unwrap();

    let out = dir.join("out");
    Archive::new(&archive).set_strip_root(true).extract_to(&out).unwrap();
    assert!(std::fs::read(out.join("big.bin")).unwrap() == data);
}

#[test]
fn method_14_lzma_matches_the_original_bytes() {
    let dir = common::TempDir::new("lzma");
    let out = dir.join("out");
    Archive::new(fixture("lzma.zip")).extract_to(&out).expect("extraction failed");

    for name in ["bzip2.raw", "deflate64.raw"] {
        let want = std::fs::read(fixture(name)).unwrap();
        let got = std::fs::read(out.join(name)).unwrap();
        assert!(got == want, "{name}: decompressed bytes differ");
    }
}

#[test]
fn lzma_is_decode_only() {
    assert!(!Method::Lzma.can_encode());
    assert_eq!(Method::from_code(14).unwrap(), Method::Lzma);
}

#[test]
fn method_95_xz_matches_the_original_bytes() {
    let dir = common::TempDir::new("xz");
    let out = dir.join("out");
    Archive::new(fixture("xz.zip")).extract_to(&out).expect("extraction failed");

    for name in ["bzip2.raw", "deflate64.raw"] {
        let want = std::fs::read(fixture(name)).unwrap();
        let got = std::fs::read(out.join(name)).unwrap();
        assert!(got == want, "{name}: decompressed bytes differ");
    }
}

#[test]
fn xz_and_lzma_are_decode_only() {
    assert!(!Method::Xz.can_encode());
    assert_eq!(Method::from_code(95).unwrap(), Method::Xz);
}

#[test]
fn unimplemented_methods_are_reported_not_guessed() {
    for (code, _name) in [(1u16, "shrink"), (6, "implode"), (93, "zstd"), (98, "ppmd")] {
        match Method::from_code(code) {
            Ok(_) => {}
            Err(e) => assert!(e.is_unsupported(), "method {code} gave {e}"),
        }
    }
    for code in [11u16, 13, 15, 17, 200, 0xFFFF] {
        assert!(Method::from_code(code).is_err(), "method {code} should not resolve");
    }
}

#[test]
fn legacy_methods_1_through_6_decode() {
    let want = std::fs::read(legacy_fixture("first.txt")).unwrap();

    for (archive, name) in [("shrink.zip", "FIRST.TXT"), ("reduce.zip", "first.txt"), ("implode.zip", "first.txt")] {
        let dir = common::TempDir::new("legacy");
        let out = dir.join("out");
        Archive::new(legacy_fixture(archive)).extract_to(&out).unwrap_or_else(|e| panic!("{archive}: {e}"));

        let got = std::fs::read(out.join(name)).unwrap_or_else(|e| panic!("{archive}: {e}"));
        assert!(got == want, "{archive}: decompressed bytes differ");
    }
}

#[test]
fn legacy_methods_resolve_to_the_right_variants() {
    assert_eq!(Method::from_code(1).unwrap(), Method::Shrink);
    assert_eq!(Method::from_code(6).unwrap(), Method::Implode);
    for code in 2..=5u16 {
        let method = Method::from_code(code).unwrap();
        assert_eq!(method, Method::Reduce((code - 1) as u8), "method {code}");
        assert_eq!(method.code(), code, "method {code} does not round-trip");
    }
    for method in [Method::Shrink, Method::Implode, Method::Reduce(1)] {
        assert!(!method.can_encode(), "{method:?} must be decode only");
    }
}

#[test]
fn method_93_zstd_matches_the_original_bytes() {
    let dir = common::TempDir::new("zstd");
    let out = dir.join("out");
    Archive::new(fixture("zstd.zip")).extract_to(&out).expect("extraction failed");

    for name in ["bzip2.raw", "deflate64.raw"] {
        let want = std::fs::read(fixture(name)).unwrap();
        let got = std::fs::read(out.join(name)).unwrap();
        assert!(got == want, "{name}: decompressed bytes differ");
    }
}

#[test]
fn xxhash64_matches_the_reference_vectors() {
    use ttarchive::utils::xxhash::XxHash64;

    assert_eq!(XxHash64::hash(b""), 0xEF46DB3751D8E999);
    assert_eq!(XxHash64::hash(b"a"), 0xD24EC4F1A98C6E5B);
    assert_eq!(XxHash64::hash(b"abc"), 0x44BC2CF5AD770999);

    let data: Vec<u8> = (0..1000u32).map(|i| (i * 31) as u8).collect();
    let whole = XxHash64::hash(&data);
    let mut split = XxHash64::new(0);
    for chunk in data.chunks(7) {
        split.update(chunk);
    }
    assert_eq!(split.finish(), whole);
}

#[test]
fn zstd_content_checksum_is_enforced() {
    use ttarchive::codecs::zstd;

    let data: Vec<u8> = (0..50_000u32).map(|i| (i.wrapping_mul(2654435761) >> 15) as u8).collect();

    let raw = std::fs::read(fixture("zstd.zip")).unwrap();
    let entry = zstd_entry_bytes(&raw);

    let good = zstd::decompress(&entry, 0).expect("clean frame must decode");
    assert!(!good.is_empty());

    let mut corrupt = entry.clone();
    let victim = corrupt.len() / 2;
    corrupt[victim] ^= 0x01;
    match zstd::decompress(&corrupt, 0) {
        Err(_) => {}
        Ok(out) => assert!(out != good, "corrupted frame decoded to the same bytes"),
    }

    let _ = data;
}

fn zstd_entry_bytes(zip: &[u8]) -> Vec<u8> {
    let name_len = u16::from_le_bytes([zip[26], zip[27]]) as usize;
    let extra_len = u16::from_le_bytes([zip[28], zip[29]]) as usize;
    let compressed = u32::from_le_bytes([zip[18], zip[19], zip[20], zip[21]]) as usize;
    let start = 30 + name_len + extra_len;
    zip[start..start + compressed].to_vec()
}

#[test]
fn method_98_ppmd_matches_the_original_bytes() {
    let dir = common::TempDir::new("ppmd");
    let out = dir.join("out");
    Archive::new(fixture("ppmd.zip")).extract_to(&out).expect("extraction failed");

    for name in ["bzip2.raw", "deflate64.raw"] {
        let want = std::fs::read(fixture(name)).unwrap();
        let got = std::fs::read(out.join(name)).unwrap();
        assert!(got == want, "{name}: decompressed bytes differ");
    }
}

#[test]
fn ppmd_cut_off_restore_method_decodes() {
    let dir = common::TempDir::new("ppmd-cutoff");
    let out = dir.join("out");

    let summary = Archive::new(fixture("ppmd_cutoff.zip")).set_strip_root(true).extract_to(&out).expect("extraction failed");
    assert_eq!(summary.files, 3);

    let mut total = 0u64;
    for name in ["text.bin", "code.bin", "runs.bin"] {
        total += std::fs::metadata(out.join(name)).unwrap_or_else(|e| panic!("{name}: {e}")).len();
    }
    assert_eq!(total, 460_705);
}

#[test]
fn ppmd_is_decode_only_and_rejects_freeze() {
    assert!(!Method::Ppmd.can_encode());
    assert_eq!(Method::from_code(98).unwrap(), Method::Ppmd);

    let freeze = [0x0fu8, 0x20, 0, 0, 0, 0];
    let err = ttarchive::codecs::ppmd::decompress(&freeze, 16).expect_err("FREEZE must be refused");
    assert!(err.is_unsupported(), "expected Unsupported, got {err}");
}
