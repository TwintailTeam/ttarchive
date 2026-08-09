mod common;

use std::process::Command;

use ttarchive::codecs::{compress, lzip, lzma};

fn have(tool: &str) -> bool {
    Command::new("which").arg(tool).output().is_ok_and(|o| o.status.success())
}

fn run(tool: &str, args: &[&str], input: &[u8], dir: &common::TempDir) -> Vec<u8> {
    let raw = dir.join("in.bin");
    std::fs::write(&raw, input).unwrap();

    let out = Command::new(tool).args(args).arg(&raw).output().unwrap_or_else(|e| panic!("run {tool}: {e}"));
    assert!(out.status.success(), "{tool} failed: {}", String::from_utf8_lossy(&out.stderr));
    out.stdout
}

fn payloads() -> Vec<(&'static str, Vec<u8>)> {
    vec![
        ("empty", Vec::new()),
        ("one byte", vec![7]),
        ("runs", vec![b'q'; 300_000]),
        ("text", common::compressible(200_000)),
        ("noise", common::pseudo_random(150_000, 17)),
        ("all bytes", (0..=255u8).cycle().take(100_000).collect()),
    ]
}

#[test]
fn lzma_alone_reads_what_the_lzma_tool_writes() {
    if !have("lzma") {
        eprintln!("skipping: lzma not installed");
        return;
    }

    for (name, data) in payloads() {
        let dir = common::TempDir::new("alone");
        let packed = run("lzma", &["-c", "-q"], &data, &dir);

        assert!(!packed.is_empty(), "{name}: lzma produced nothing");
        let back = lzma::alone::decompress(&packed, data.len()).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert!(back == data, "{name}: contents differ");
    }
}

#[test]
fn lzma_alone_handles_every_preset() {
    if !have("lzma") {
        eprintln!("skipping: lzma not installed");
        return;
    }

    let data = common::compressible(300_000);
    for level in ["-1", "-3", "-6", "-9"] {
        let dir = common::TempDir::new("alone-level");
        let packed = run("lzma", &["-c", "-q", level], &data, &dir);
        let back = lzma::alone::decompress(&packed, data.len()).unwrap_or_else(|e| panic!("{level}: {e}"));
        assert!(back == data, "{level}: contents differ");
    }
}

#[test]
fn an_alone_header_is_recognised_but_a_zip_lzma_header_is_not() {
    if !have("lzma") {
        eprintln!("skipping: lzma not installed");
        return;
    }

    let dir = common::TempDir::new("alone-magic");
    let packed = run("lzma", &["-c", "-q"], b"detect me", &dir);
    assert!(lzma::alone::is_alone(&packed), "a real .lzma stream should be recognised");

    assert!(!lzma::alone::is_alone(&[0xff, 0xff, 0xff, 0xff, 0xff]), "nonsense should not look like a header");
    assert!(!lzma::alone::is_alone(&[]), "an empty prefix should not look like a header");
}

#[test]
fn a_truncated_alone_stream_is_reported() {
    if !have("lzma") {
        eprintln!("skipping: lzma not installed");
        return;
    }

    let dir = common::TempDir::new("alone-truncated");
    let data = common::compressible(120_000);
    let packed = run("lzma", &["-c", "-q"], &data, &dir);

    let cut = &packed[..packed.len() / 2];
    match lzma::alone::decompress(cut, data.len()) {
        Err(_) => {}
        Ok(out) => assert!(out != data, "a truncated stream must not decode to the whole payload"),
    }
}

#[test]
fn unix_compress_reads_what_the_compress_tool_writes() {
    if !have("compress") {
        eprintln!("skipping: compress not installed");
        return;
    }

    for (name, data) in payloads() {
        if data.is_empty() {
            continue;
        }
        let dir = common::TempDir::new("compress");
        let packed = run("compress", &["-c"], &data, &dir);

        assert!(compress::is_compress(&packed), "{name}: missing 1f 9d magic");
        let back = compress::decompress(&packed, data.len()).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert!(back == data, "{name}: contents differ ({} vs {} bytes)", back.len(), data.len());
    }
}

#[test]
fn unix_compress_handles_every_code_width() {
    if !have("compress") {
        eprintln!("skipping: compress not installed");
        return;
    }

    let data = common::compressible(400_000);
    for bits in ["9", "12", "14", "16"] {
        let dir = common::TempDir::new("compress-bits");
        let packed = run("compress", &["-c", "-b", bits], &data, &dir);
        let back = compress::decompress(&packed, data.len()).unwrap_or_else(|e| panic!("{bits} bits: {e}"));
        assert!(back == data, "{bits} bits: contents differ");
    }
}

#[test]
fn a_compress_stream_with_a_bad_header_is_refused() {
    assert!(compress::decompress(&[0x1f, 0x9d, 0x00], 0).is_err(), "9 bits is the minimum, 0 must be refused");
    assert!(compress::decompress(&[0x00, 0x00, 0x8b], 0).is_err(), "the magic must be checked");
    assert!(compress::decompress(&[0x1f], 0).is_err(), "a short stream must be refused");
}

#[test]
fn lzip_rejects_a_stream_that_is_not_lzip() {
    assert!(!lzip::is_lzip(b"NOPE"));
    assert!(lzip::is_lzip(b"LZIP\x01\x0c"));
    assert!(lzip::decompress(b"NOPEnope", 0).is_err(), "a bad magic must be reported");
}

#[test]
fn lzip_reads_a_stream_we_assemble_by_hand() {
    if !have("lzma") {
        eprintln!("skipping: lzma not installed");
        return;
    }

    let data = common::compressible(80_000);

    let dir = common::TempDir::new("lzip-build");
    let alone = run("lzma", &["-c", "-q", "--format=alone", "--lzma1=lc=3,lp=0,pb=2"], &data, &dir);

    let dict = u32::from_le_bytes([alone[1], alone[2], alone[3], alone[4]]);
    let exponent = (32 - dict.leading_zeros()).clamp(12, 29) as u8;

    let mut stream = Vec::new();
    stream.extend_from_slice(b"LZIP");
    stream.push(1);
    stream.push(exponent);
    stream.extend_from_slice(&alone[13..]);
    stream.extend_from_slice(&ttarchive::utils::crc32::checksum(&data).to_le_bytes());
    stream.extend_from_slice(&(data.len() as u64).to_le_bytes());
    stream.extend_from_slice(&((stream.len() + 8) as u64).to_le_bytes());

    let back = lzip::decompress(&stream, data.len()).expect("hand-built lzip member should decode");
    assert!(back == data, "lzip contents differ");
}

#[test]
fn lzip_catches_a_corrupt_trailer() {
    if !have("lzma") {
        eprintln!("skipping: lzma not installed");
        return;
    }

    let data = common::compressible(40_000);
    let dir = common::TempDir::new("lzip-corrupt");
    let alone = run("lzma", &["-c", "-q", "--format=alone", "--lzma1=lc=3,lp=0,pb=2"], &data, &dir);

    let dict = u32::from_le_bytes([alone[1], alone[2], alone[3], alone[4]]);
    let exponent = (32 - dict.leading_zeros()).clamp(12, 29) as u8;

    let mut stream = Vec::new();
    stream.extend_from_slice(b"LZIP");
    stream.push(1);
    stream.push(exponent);
    stream.extend_from_slice(&alone[13..]);
    stream.extend_from_slice(&(ttarchive::utils::crc32::checksum(&data) ^ 0xff).to_le_bytes());
    stream.extend_from_slice(&(data.len() as u64).to_le_bytes());
    stream.extend_from_slice(&((stream.len() + 8) as u64).to_le_bytes());

    assert!(lzip::decompress(&stream, data.len()).is_err(), "a wrong CRC in the trailer must be reported");
}

#[test]
fn the_lzma_tool_decodes_what_our_encoder_writes() {
    if !have("lzma") {
        eprintln!("skipping: lzma not installed");
        return;
    }

    for (name, data) in payloads() {
        let dir = common::TempDir::new("alone-write");
        let packed = lzma::alone::compress(&data, 32).unwrap_or_else(|e| panic!("{name}: compress failed: {e}"));

        let path = dir.join("ours.lzma");
        std::fs::write(&path, &packed).unwrap();

        let out = Command::new("lzma").arg("-d").arg("-c").arg(&path).output().expect("run lzma");
        assert!(out.status.success(), "{name}: lzma -d rejected our stream: {}", String::from_utf8_lossy(&out.stderr));
        assert!(out.stdout == data, "{name}: lzma -d produced different bytes");
    }
}

#[test]
fn xz_also_decodes_our_lzma_streams() {
    if !have("xz") {
        eprintln!("skipping: xz not installed");
        return;
    }

    for (name, data) in payloads() {
        let dir = common::TempDir::new("alone-xz");
        let packed = lzma::alone::compress(&data, 32).unwrap();

        let path = dir.join("ours.lzma");
        std::fs::write(&path, &packed).unwrap();

        let out = Command::new("xz").args(["--format=lzma", "-d", "-c"]).arg(&path).output().expect("run xz");
        assert!(out.status.success(), "{name}: xz rejected our stream: {}", String::from_utf8_lossy(&out.stderr));
        assert!(out.stdout == data, "{name}: xz produced different bytes");
    }
}

#[test]
fn our_lzma_round_trips_through_our_own_decoder_at_every_depth() {
    let data = common::compressible(250_000);
    let mut noisy = data.clone();
    noisy.extend(common::pseudo_random(80_000, 23));

    for depth in [1usize, 4, 32, 128] {
        for payload in [&data, &noisy] {
            let packed = lzma::alone::compress(payload, depth).unwrap_or_else(|e| panic!("depth {depth}: {e}"));
            let back = lzma::alone::decompress(&packed, payload.len()).unwrap_or_else(|e| panic!("depth {depth}: {e}"));
            assert!(back == *payload, "depth {depth}: round trip changed the bytes");
        }
    }
}

#[test]
fn our_lzma_writes_the_dictionary_size_it_actually_used() {
    let data = common::compressible(100_000);
    let packed = lzma::alone::compress(&data, 32).unwrap();

    let dict = u32::from_le_bytes([packed[1], packed[2], packed[3], packed[4]]);
    assert!(dict >= 1 << 12, "the header must record a dictionary the decoder will accept, got {dict}");
    assert!(lzma::alone::is_alone(&packed), "our own header should be recognised");
}

#[test]
fn the_xz_tool_validates_and_decodes_what_our_encoder_writes() {
    if !have("xz") {
        eprintln!("skipping: xz not installed");
        return;
    }

    for (name, data) in payloads() {
        let dir = common::TempDir::new("xz-write");
        let packed = ttarchive::codecs::xz::encode::compress_default(&data, 32).unwrap_or_else(|e| panic!("{name}: {e}"));

        let path = dir.join("ours.xz");
        std::fs::write(&path, &packed).unwrap();

        let checked = Command::new("xz").arg("-t").arg(&path).output().expect("run xz -t");
        assert!(checked.status.success(), "{name}: xz -t rejected our stream: {}", String::from_utf8_lossy(&checked.stderr));

        let out = Command::new("xz").args(["-d", "-c"]).arg(&path).output().expect("run xz -d");
        assert!(out.status.success(), "{name}: xz -d failed: {}", String::from_utf8_lossy(&out.stderr));
        assert!(out.stdout == data, "{name}: xz produced different bytes");
    }
}

#[test]
fn our_xz_survives_data_that_forces_stored_lzma2_chunks() {
    if !have("xz") {
        eprintln!("skipping: xz not installed");
        return;
    }

    let data = common::pseudo_random(400_000, 91);
    let packed = ttarchive::codecs::xz::encode::compress_default(&data, 32).unwrap();
    assert!(packed.len() > data.len(), "incompressible input should not shrink, got {} from {}", packed.len(), data.len());

    let dir = common::TempDir::new("xz-stored");
    let path = dir.join("stored.xz");
    std::fs::write(&path, &packed).unwrap();

    let out = Command::new("xz").args(["-d", "-c"]).arg(&path).output().expect("run xz");
    assert!(out.status.success(), "xz rejected our stored chunks: {}", String::from_utf8_lossy(&out.stderr));
    assert!(out.stdout == data, "stored chunk contents differ");

    let back = ttarchive::codecs::xz::decompress(&packed, 0).unwrap();
    assert!(back == data, "our own decoder disagrees about stored chunks");
}

#[test]
fn our_xz_spans_many_lzma2_chunks_without_losing_context() {
    if !have("xz") {
        eprintln!("skipping: xz not installed");
        return;
    }

    let data = common::compressible(2_000_000);
    let packed = ttarchive::codecs::xz::encode::compress_default(&data, 32).unwrap();
    assert!(packed.len() * 20 < data.len(), "cross-chunk matching looks broken: {} from {}", packed.len(), data.len());

    let dir = common::TempDir::new("xz-chunks");
    let path = dir.join("many.xz");
    std::fs::write(&path, &packed).unwrap();

    let out = Command::new("xz").args(["-d", "-c"]).arg(&path).output().expect("run xz");
    assert!(out.status.success(), "xz rejected a multi-chunk stream: {}", String::from_utf8_lossy(&out.stderr));
    assert!(out.stdout == data, "multi-chunk contents differ");
}

#[test]
fn our_xz_round_trips_through_our_own_decoder() {
    for depth in [1usize, 8, 64] {
        for (name, data) in payloads() {
            let packed = ttarchive::codecs::xz::encode::compress_default(&data, depth).unwrap_or_else(|e| panic!("{name}/{depth}: {e}"));
            let back = ttarchive::codecs::xz::decompress(&packed, 0).unwrap_or_else(|e| panic!("{name}/{depth}: {e}"));
            assert!(back == data, "{name}/{depth}: round trip changed the bytes");
        }
    }
}

#[test]
fn a_corrupt_xz_check_is_caught() {
    let data = common::compressible(80_000);
    let mut packed = ttarchive::codecs::xz::encode::compress_default(&data, 32).unwrap();

    let victim = packed.len() / 2;
    packed[victim] ^= 0x01;

    match ttarchive::codecs::xz::decompress(&packed, 0) {
        Err(_) => {}
        Ok(out) => assert!(out != data, "a corrupted xz stream decoded to the original bytes"),
    }
}

#[test]
fn the_zstd_tool_validates_and_decodes_what_our_encoder_writes() {
    if !have("zstd") {
        eprintln!("skipping: zstd not installed");
        return;
    }

    for (name, data) in payloads() {
        let dir = common::TempDir::new("zstd-write");
        let packed = ttarchive::codecs::zstd::encode::compress(&data, true).unwrap_or_else(|e| panic!("{name}: {e}"));

        let path = dir.join("ours.zst");
        std::fs::write(&path, &packed).unwrap();

        let checked = Command::new("zstd").arg("-t").arg(&path).output().expect("run zstd -t");
        assert!(checked.status.success(), "{name}: zstd -t rejected our frame: {}", String::from_utf8_lossy(&checked.stderr));

        let out = Command::new("zstd").args(["-d", "-c"]).arg(&path).output().expect("run zstd -d");
        assert!(out.status.success(), "{name}: zstd -d failed: {}", String::from_utf8_lossy(&out.stderr));
        assert!(out.stdout == data, "{name}: zstd produced different bytes");
    }
}

#[test]
fn our_zstd_frames_carry_a_content_checksum_that_is_checked() {
    let data = common::compressible(90_000);
    let mut packed = ttarchive::codecs::zstd::encode::compress(&data, true).unwrap();

    let last = packed.len() - 1;
    packed[last] ^= 0xff;

    assert!(ttarchive::codecs::zstd::decompress(&packed, 0).is_err(), "a corrupted content checksum must be reported");
}

#[test]
fn our_zstd_uses_rle_blocks_for_runs() {
    let data = vec![b'q'; 300_000];
    let packed = ttarchive::codecs::zstd::encode::compress(&data, true).unwrap();
    assert!(packed.len() < 100, "a single repeated byte should collapse to RLE blocks, got {} bytes", packed.len());

    let back = ttarchive::codecs::zstd::decompress(&packed, 0).unwrap();
    assert!(back == data, "RLE round trip changed the bytes");
}

#[test]
fn our_zstd_round_trips_through_our_own_decoder() {
    for (name, data) in payloads() {
        for checksum in [true, false] {
            let packed = ttarchive::codecs::zstd::encode::compress(&data, checksum).unwrap_or_else(|e| panic!("{name}: {e}"));
            let back = ttarchive::codecs::zstd::decompress(&packed, 0).unwrap_or_else(|e| panic!("{name} checksum={checksum}: {e}"));
            assert!(back == data, "{name} checksum={checksum}: round trip changed the bytes");
        }
    }
}

#[test]
fn our_zstd_spans_more_than_one_block() {
    if !have("zstd") {
        eprintln!("skipping: zstd not installed");
        return;
    }

    let data = common::pseudo_random(600_000, 41);
    let packed = ttarchive::codecs::zstd::encode::compress(&data, true).unwrap();

    let dir = common::TempDir::new("zstd-blocks");
    let path = dir.join("many.zst");
    std::fs::write(&path, &packed).unwrap();

    let out = Command::new("zstd").args(["-d", "-c"]).arg(&path).output().expect("run zstd");
    assert!(out.status.success(), "zstd rejected a multi-block frame: {}", String::from_utf8_lossy(&out.stderr));
    assert!(out.stdout == data, "multi-block contents differ");
}

fn concatenate(tool: &str, args: &[&str], separator: &[u8]) -> Option<(Vec<u8>, Vec<u8>)> {
    if !have(tool) {
        eprintln!("skipping: {tool} not installed");
        return None;
    }

    let dir = common::TempDir::new("concat");
    let first = common::compressible(120_000);
    let second = common::pseudo_random(90_000, 23);

    let mut packed = run(tool, args, &first, &dir);
    packed.extend_from_slice(separator);
    packed.extend_from_slice(&run(tool, args, &second, &dir));

    let mut plain = first;
    plain.extend_from_slice(&second);
    Some((packed, plain))
}

#[test]
fn concatenated_gzip_members_decode_in_full() {
    let Some((packed, plain)) = concatenate("gzip", &["-c"], &[]) else { return };

    let out = ttarchive::codecs::gzip::decompress(&packed, plain.len()).unwrap();
    assert!(out == plain, "concatenated gzip members decoded {} of {} bytes", out.len(), plain.len());
}

#[test]
fn concatenated_bzip2_streams_decode_in_full() {
    let Some((packed, plain)) = concatenate("bzip2", &["-c"], &[]) else { return };

    let out = ttarchive::codecs::bzip2::decompress(&packed, plain.len()).unwrap();
    assert!(out == plain, "concatenated bzip2 streams decoded {} of {} bytes; the trailing stream was dropped", out.len(), plain.len());
}

#[test]
fn concatenated_xz_streams_decode_in_full() {
    let Some((packed, plain)) = concatenate("xz", &["-c"], &[]) else { return };

    let out = ttarchive::codecs::xz::decompress(&packed, plain.len()).unwrap();
    assert!(out == plain, "concatenated xz streams decoded {} of {} bytes; the trailing stream was dropped", out.len(), plain.len());
}

#[test]
fn xz_stream_padding_between_streams_is_skipped() {
    let Some((packed, plain)) = concatenate("xz", &["-c"], &[0; 8]) else { return };

    let out = ttarchive::codecs::xz::decompress(&packed, plain.len()).unwrap();
    assert!(out == plain, "padded concatenation decoded {} of {} bytes", out.len(), plain.len());
}

#[test]
fn xz_stream_padding_that_is_not_a_multiple_of_four_is_rejected() {
    let Some((packed, _)) = concatenate("xz", &["-c"], &[0; 3]) else { return };

    assert!(ttarchive::codecs::xz::decompress(&packed, 0).is_err(), "misaligned stream padding must be reported");
}

#[test]
fn trailing_zeros_after_a_bzip2_stream_are_tolerated() {
    if !have("bzip2") {
        eprintln!("skipping: bzip2 not installed");
        return;
    }

    let dir = common::TempDir::new("bz-pad");
    let plain = common::compressible(60_000);
    let mut packed = run("bzip2", &["-c"], &plain, &dir);
    packed.extend_from_slice(&[0; 16]);

    let out = ttarchive::codecs::bzip2::decompress(&packed, plain.len()).unwrap();
    assert!(out == plain, "zero padding after a bzip2 stream should be ignored");
}

#[test]
fn garbage_after_a_bzip2_stream_is_reported() {
    if !have("bzip2") {
        eprintln!("skipping: bzip2 not installed");
        return;
    }

    let dir = common::TempDir::new("bz-junk");
    let plain = common::compressible(60_000);
    let mut packed = run("bzip2", &["-c"], &plain, &dir);
    packed.extend_from_slice(b"not another stream");

    assert!(ttarchive::codecs::bzip2::decompress(&packed, 0).is_err(), "trailing garbage must not be mistaken for a second stream");
}

#[test]
fn our_zstd_actually_compresses_repetitive_data() {
    let data: Vec<u8> = std::iter::repeat_n("the quick brown fox jumps over the lazy dog. ", 5_000).collect::<String>().into_bytes();

    let packed = ttarchive::codecs::zstd::encode::compress(&data, true).unwrap();
    assert!(packed.len() * 100 < data.len(), "expected better than 100:1 on a repeating body, got {} from {}", packed.len(), data.len());

    let back = ttarchive::codecs::zstd::decompress(&packed, 0).unwrap();
    assert!(back == data, "the compressed round trip changed the bytes");
}

#[test]
fn our_zstd_beats_storing_source_text() {
    let data = common::compressible(400_000);

    let packed = ttarchive::codecs::zstd::encode::compress(&data, true).unwrap();
    assert!(packed.len() < data.len() / 2, "expected at least 2:1 on compressible text, got {} from {}", packed.len(), data.len());

    assert!(ttarchive::codecs::zstd::decompress(&packed, 0).unwrap() == data);
}

#[test]
fn our_zstd_falls_back_to_raw_blocks_on_incompressible_data() {
    let data = common::pseudo_random(300_000, 61);

    let packed = ttarchive::codecs::zstd::encode::compress(&data, false).unwrap();
    assert!(packed.len() < data.len() + 512, "noise should not have grown by more than block headers, got {}", packed.len());

    assert!(ttarchive::codecs::zstd::decompress(&packed, 0).unwrap() == data);
}

#[test]
fn our_zstd_matches_across_block_boundaries() {
    if !have("zstd") {
        eprintln!("skipping: zstd not installed");
        return;
    }

    let unit = common::compressible(200_000);
    let mut data = unit.clone();
    data.extend_from_slice(&unit);

    let packed = ttarchive::codecs::zstd::encode::compress(&data, true).unwrap();
    let single = ttarchive::codecs::zstd::encode::compress(&unit, true).unwrap();

    assert!(
        packed.len() < single.len() * 3 / 2,
        "the repeat should have cost almost nothing: {} for two copies against {} for one",
        packed.len(),
        single.len()
    );

    let dir = common::TempDir::new("zstd-cross");
    let path = dir.join("cross.zst");
    std::fs::write(&path, &packed).unwrap();

    let out = Command::new("zstd").args(["-d", "-c"]).arg(&path).output().expect("run zstd");
    assert!(out.status.success(), "zstd rejected cross-block matches: {}", String::from_utf8_lossy(&out.stderr));
    assert!(out.stdout == data, "cross-block contents differ");
}

#[test]
fn our_zstd_declares_a_bounded_window_for_large_content() {
    if !have("zstd") {
        eprintln!("skipping: zstd not installed");
        return;
    }

    let unit = common::compressible(600_000);
    let mut data = Vec::with_capacity(unit.len() * 16);
    for _ in 0..16 {
        data.extend_from_slice(&unit);
    }
    assert!(data.len() > 8 * 1024 * 1024, "the fixture needs to exceed the window");

    let packed = ttarchive::codecs::zstd::encode::compress(&data, true).unwrap();

    let descriptor = packed[4];
    assert_eq!(descriptor & 0x20, 0, "large content must not set the single segment flag");

    let dir = common::TempDir::new("zstd-window");
    let path = dir.join("big.zst");
    std::fs::write(&path, &packed).unwrap();

    let checked = Command::new("zstd").arg("-t").arg(&path).output().expect("run zstd -t");
    assert!(checked.status.success(), "zstd -t rejected a windowed frame: {}", String::from_utf8_lossy(&checked.stderr));

    let out = Command::new("zstd").args(["-d", "-c"]).arg(&path).output().expect("run zstd -d");
    assert!(out.stdout == data, "windowed frame decoded differently");
}

#[test]
fn small_zstd_content_still_uses_a_single_segment() {
    let data = common::compressible(50_000);
    let packed = ttarchive::codecs::zstd::encode::compress(&data, true).unwrap();

    assert_ne!(packed[4] & 0x20, 0, "content inside the window should be declared as a single segment");
    assert!(ttarchive::codecs::zstd::decompress(&packed, 0).unwrap() == data);
}

#[test]
fn our_zstd_round_trips_at_every_depth() {
    for depth in [1, 8, 32, 128] {
        for (name, data) in payloads() {
            let packed = ttarchive::codecs::zstd::encode::compress_at(&data, true, depth).unwrap_or_else(|e| panic!("{name}/{depth}: {e}"));
            let back = ttarchive::codecs::zstd::decompress(&packed, 0).unwrap_or_else(|e| panic!("{name}/{depth}: {e}"));
            assert!(back == data, "{name}/{depth}: round trip changed the bytes");
        }
    }
}

#[test]
fn the_gzip_reader_streams_rather_than_holding_the_whole_member() {
    use std::io::Read;

    if !have("gzip") {
        eprintln!("skipping: gzip not installed");
        return;
    }

    let dir = common::TempDir::new("gzip-stream");
    let plain = common::compressible(900_000);
    let packed = run("gzip", &["-c"], &plain, &dir);

    let path = dir.join("one.gz");
    std::fs::write(&path, &packed).unwrap();

    let mut reader = ttarchive::codecs::gzip::GzipReader::new(std::io::BufReader::new(std::fs::File::open(&path).unwrap()));
    let mut got = Vec::new();
    reader.read_to_end(&mut got).unwrap();

    assert!(got == plain, "the streaming reader returned {} bytes, wanted {}", got.len(), plain.len());
}

#[test]
fn the_gzip_reader_crosses_member_boundaries() {
    use std::io::Read;

    let Some((packed, plain)) = concatenate("gzip", &["-c"], &[]) else { return };

    let dir = common::TempDir::new("gzip-stream-members");
    let path = dir.join("many.gz");
    std::fs::write(&path, &packed).unwrap();

    let mut reader = ttarchive::codecs::gzip::GzipReader::new(std::fs::File::open(&path).unwrap());
    let mut got = Vec::new();
    let mut one = [0u8; 1];
    while reader.read(&mut one).unwrap() == 1 {
        got.push(one[0]);
    }

    assert!(got == plain, "streaming across members returned {} bytes, wanted {}", got.len(), plain.len());
}

#[test]
fn the_gzip_reader_reports_a_corrupt_member() {
    use std::io::Read;

    if !have("gzip") {
        eprintln!("skipping: gzip not installed");
        return;
    }

    let dir = common::TempDir::new("gzip-stream-bad");
    let plain = common::compressible(200_000);
    let mut packed = run("gzip", &["-c"], &plain, &dir);

    let last = packed.len() - 5;
    packed[last] ^= 0xff;

    let mut reader = ttarchive::codecs::gzip::GzipReader::new(packed.as_slice());
    let mut got = Vec::new();
    assert!(reader.read_to_end(&mut got).is_err(), "a corrupted trailer must be reported by the streaming reader");
}

fn xz_streamed(packed: &[u8]) -> ttarchive::Result<Vec<u8>> {
    use std::io::Read;
    let mut out = Vec::new();
    ttarchive::codecs::xz::Reader::new(packed, 0).read_to_end(&mut out)?;
    Ok(out)
}

#[test]
fn the_streaming_xz_reader_matches_the_whole_stream_decoder() {
    for (name, data) in payloads() {
        let packed = ttarchive::codecs::xz::encode::compress_default(&data, 16).unwrap_or_else(|e| panic!("{name}: {e}"));

        let whole = ttarchive::codecs::xz::decompress(&packed, 0).unwrap_or_else(|e| panic!("{name}: {e}"));
        let piecewise = xz_streamed(&packed).unwrap_or_else(|e| panic!("{name} streamed: {e}"));

        assert!(piecewise == whole, "{name}: streaming gave different bytes to decoding the whole stream");
        assert!(piecewise == data, "{name}: streaming did not round trip");
    }
}

#[test]
fn the_streaming_xz_reader_handles_what_the_xz_tool_writes() {
    if !have("xz") {
        eprintln!("skipping: xz not installed");
        return;
    }

    for preset in ["-0", "-6", "-9"] {
        let dir = common::TempDir::new("xz-stream");
        let plain = common::compressible(700_000);
        let packed = run("xz", &[preset, "-c"], &plain, &dir);

        let got = xz_streamed(&packed).unwrap_or_else(|e| panic!("xz {preset}: {e}"));
        assert!(got == plain, "xz {preset}: streaming returned {} bytes, wanted {}", got.len(), plain.len());
    }
}

#[test]
fn the_streaming_xz_reader_crosses_concatenated_streams() {
    let Some((packed, plain)) = concatenate("xz", &["-c"], &[]) else { return };

    let got = xz_streamed(&packed).unwrap();
    assert!(got == plain, "streaming stopped after the first stream: {} of {} bytes", got.len(), plain.len());
}

#[test]
fn the_streaming_xz_reader_skips_padding_between_streams() {
    let Some((packed, plain)) = concatenate("xz", &["-c"], &[0; 8]) else { return };

    let got = xz_streamed(&packed).unwrap();
    assert!(got == plain, "padded concatenation streamed {} of {} bytes", got.len(), plain.len());
}

#[test]
fn the_streaming_xz_reader_reports_a_corrupted_check() {
    let data = common::compressible(120_000);
    let mut packed = ttarchive::codecs::xz::encode::compress_default(&data, 16).unwrap();

    let victim = packed.len() / 2;
    packed[victim] ^= 0x01;

    match xz_streamed(&packed) {
        Err(_) => {}
        Ok(out) => assert!(out != data, "a corrupted xz stream streamed back as the original bytes"),
    }
}

#[test]
fn the_streaming_xz_reader_handles_every_check_type() {
    if !have("xz") {
        eprintln!("skipping: xz not installed");
        return;
    }

    for check in ["crc32", "crc64", "sha256", "none"] {
        let dir = common::TempDir::new("xz-check");
        let plain = common::compressible(200_000);
        let packed = run("xz", &[&format!("--check={check}"), "-c"], &plain, &dir);

        let got = xz_streamed(&packed).unwrap_or_else(|e| panic!("check={check}: {e}"));
        assert!(got == plain, "check={check}: streaming returned different bytes");
    }
}

#[test]
fn the_streaming_xz_reader_reads_a_multi_block_stream() {
    if !have("xz") {
        eprintln!("skipping: xz not installed");
        return;
    }

    let dir = common::TempDir::new("xz-blocks");
    let plain = common::compressible(4_000_000);
    let packed = run("xz", &["-T4", "--block-size=500000", "-c"], &plain, &dir);

    let got = xz_streamed(&packed).unwrap();
    assert!(got == plain, "multi-block streaming returned {} of {} bytes", got.len(), plain.len());
}
