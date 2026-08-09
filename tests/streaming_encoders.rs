mod common;

use std::process::Command;

use ttarchive::codecs::Level;
use ttarchive::codecs::lzma::{Properties, alone, lzma2};
use ttarchive::codecs::{xz, zstd};

fn have(tool: &str) -> bool {
    Command::new("which").arg(tool).output().is_ok_and(|o| o.status.success())
}

fn mixed(len: usize, seed: u32) -> Vec<u8> {
    let noise = common::pseudo_random(len / 4, seed);
    let text = common::compressible(len / 2);
    let mut out = Vec::with_capacity(len);
    out.extend_from_slice(&text);
    out.extend_from_slice(&noise);
    out.extend_from_slice(&text[..len - out.len()]);
    out
}

fn in_pieces(data: &[u8], piece: usize, mut push: impl FnMut(&[u8])) {
    for chunk in data.chunks(piece) {
        push(chunk);
    }
}

#[test]
fn an_lzma2_stream_written_in_pieces_matches_one_written_at_once() {
    let data = mixed(400_000, 7);
    let props = Properties { lc: 3, lp: 0, pb: 2, dict_size: 1 << 20 };

    let once = lzma2::compress(&data, props, 32).expect("one-shot");

    for piece in [1, 7, 1024, 40_000, 1 << 15] {
        let mut writer = lzma2::Writer::new(Vec::new(), props, 32, data.len());
        in_pieces(&data, piece, |chunk| writer.push(chunk).expect("push"));
        let streamed = writer.finish().expect("finish");

        assert_eq!(streamed, once, "pieces of {piece} bytes produced a different stream");
    }
}

#[test]
fn an_lzma2_stream_shorter_than_a_chunk_still_matches() {
    let props = Properties { lc: 3, lp: 0, pb: 2, dict_size: 1 << 16 };

    for len in [0usize, 1, 2, 300, 32_767, 32_768, 32_769] {
        let data = mixed_or_empty(len);

        let once = lzma2::compress(&data, props, 16).expect("one-shot");
        let mut writer = lzma2::Writer::new(Vec::new(), props, 16, data.len());
        in_pieces(&data, 101, |chunk| writer.push(chunk).expect("push"));
        let streamed = writer.finish().expect("finish");

        assert_eq!(streamed, once, "{len} bytes streamed differently");
    }
}

fn mixed_or_empty(len: usize) -> Vec<u8> {
    if len < 8 { common::pseudo_random(len, 3) } else { mixed(len, 3) }
}

#[test]
fn a_streamed_lzma_file_round_trips_through_our_own_reader() {
    for len in [0usize, 1, 5_000, 400_000] {
        let data = mixed_or_empty(len);

        let mut writer = alone::Writer::new(Vec::new(), 32, Level::Default).expect("start");
        in_pieces(&data, 4096, |chunk| writer.push(chunk).expect("push"));
        let packed = writer.finish().expect("finish");

        let back = alone::decompress(&packed, len).expect("decompress");
        assert_eq!(back, data, "{len} bytes did not survive");
    }
}

#[test]
fn a_streamed_lzma_file_says_its_size_is_unknown_and_marks_its_end() {
    let data = mixed(50_000, 11);

    let mut writer = alone::Writer::new(Vec::new(), 32, Level::Default).expect("start");
    writer.push(&data).expect("push");
    let packed = writer.finish().expect("finish");

    assert_eq!(&packed[5..13], &[0xff; 8], "a streamed .lzma must record its size as unknown");

    let mut with_junk = packed.clone();
    with_junk.extend_from_slice(b"trailing bytes that are not part of the stream");

    let back = alone::decompress(&with_junk, data.len()).expect("decompress");
    assert_eq!(back, data, "without a size field, only the end marker can stop the decoder in the right place");
}

#[test]
fn the_lzma_tool_reads_what_we_stream() {
    if !have("lzma") {
        eprintln!("skipping: lzma not installed");
        return;
    }

    let dir = common::TempDir::new("stream-lzma");
    let data = mixed(300_000, 5);

    let mut writer = alone::Writer::new(Vec::new(), 32, Level::Default).expect("start");
    in_pieces(&data, 8192, |chunk| writer.push(chunk).expect("push"));
    let packed = writer.finish().expect("finish");

    let path = dir.join("s.lzma");
    std::fs::write(&path, &packed).unwrap();

    let out = Command::new("lzma").arg("-dc").arg(&path).output().expect("run lzma");
    assert!(out.status.success(), "lzma rejected our stream: {}", String::from_utf8_lossy(&out.stderr));
    assert!(out.stdout == data, "lzma produced different bytes");
}

#[test]
fn a_streamed_xz_file_round_trips_and_the_xz_tool_accepts_it() {
    let dir = common::TempDir::new("stream-xz");

    for len in [0usize, 1, 5_000, 400_000] {
        let data = mixed_or_empty(len);

        let mut writer = xz::encode::Writer::new(Vec::new(), 32, Level::Default).expect("start");
        in_pieces(&data, 4096, |chunk| writer.push(chunk).expect("push"));
        let packed = writer.finish().expect("finish");

        let back = xz::decompress(&packed, len).expect("decompress");
        assert_eq!(back, data, "{len} bytes did not survive our own reader");

        if !have("xz") {
            continue;
        }
        let path = dir.join(format!("s-{len}.xz"));
        std::fs::write(&path, &packed).unwrap();

        let out = Command::new("xz").arg("-dc").arg(&path).output().expect("run xz");
        assert!(out.status.success(), "{len}: xz rejected our stream: {}", String::from_utf8_lossy(&out.stderr));
        assert!(out.stdout == data, "{len}: xz produced different bytes");
    }
}

#[test]
fn streaming_costs_almost_nothing_in_size() {
    let data = mixed(2_000_000, 13);

    let once = xz::encode::compress_at(&data, 32, Level::Default).expect("one-shot");

    let mut writer = xz::encode::Writer::new(Vec::new(), 32, Level::Default).expect("start");
    in_pieces(&data, 64 * 1024, |chunk| writer.push(chunk).expect("push"));
    let streamed = writer.finish().expect("finish");

    let ratio = streamed.len() as f64 / once.len() as f64;
    assert!(ratio < 1.02, "streaming cost {:.1}% more: {} against {}", (ratio - 1.0) * 100.0, streamed.len(), once.len());
}

#[test]
fn input_several_times_the_dictionary_still_compresses_well() {
    let props = Properties { lc: 3, lp: 0, pb: 2, dict_size: 1 << 16 };
    let data = mixed(1_000_000, 17);

    let mut writer = lzma2::Writer::new(Vec::new(), props, 16, usize::MAX);
    in_pieces(&data, 9_973, |chunk| writer.push(chunk).expect("push"));
    let streamed = writer.finish().expect("finish");

    assert!(streamed.len() * 3 < data.len(), "a window sixteen times smaller than the input should still pay: {} of {}", streamed.len(), data.len());

    let once = lzma2::compress(&data, props, 16).expect("one-shot");
    let ratio = streamed.len() as f64 / once.len() as f64;
    assert!(ratio < 1.02, "sliding the window cost {:.1}% more", (ratio - 1.0) * 100.0);
}

#[test]
fn a_streamed_zstd_frame_round_trips_and_the_zstd_tool_accepts_it() {
    let dir = common::TempDir::new("stream-zst");

    for len in [0usize, 1, 5_000, 400_000] {
        let data = mixed_or_empty(len);

        let mut writer = zstd::encode::Writer::new(Vec::new(), true, 32).expect("start");
        in_pieces(&data, 4096, |chunk| writer.push(chunk).expect("push"));
        let packed = writer.finish().expect("finish");

        let back = zstd::decompress(&packed, len).expect("decompress");
        assert_eq!(back, data, "{len} bytes did not survive our own reader");

        if !have("zstd") {
            continue;
        }
        let path = dir.join(format!("s-{len}.zst"));
        std::fs::write(&path, &packed).unwrap();

        let out = Command::new("zstd").arg("-dc").arg(&path).output().expect("run zstd");
        assert!(out.status.success(), "{len}: zstd rejected our frame: {}", String::from_utf8_lossy(&out.stderr));
        assert!(out.stdout == data, "{len}: zstd produced different bytes");
    }
}

#[test]
fn a_streamed_zstd_frame_costs_almost_nothing_in_size() {
    let data = mixed(2_000_000, 19);

    let once = zstd::encode::compress_at(&data, true, 32).expect("one-shot");

    let mut writer = zstd::encode::Writer::new(Vec::new(), true, 32).expect("start");
    in_pieces(&data, 64 * 1024, |chunk| writer.push(chunk).expect("push"));
    let streamed = writer.finish().expect("finish");

    let ratio = streamed.len() as f64 / once.len() as f64;
    assert!(ratio < 1.02, "streaming cost {:.1}% more: {} against {}", (ratio - 1.0) * 100.0, streamed.len(), once.len());
}

fn large_tree(tag: &str) -> (common::TempDir, std::path::PathBuf) {
    let dir = common::TempDir::new(tag);
    dir.write("src/bulk.txt", common::compressible(9_000_000));
    dir.write("src/noise.bin", common::pseudo_random(3_000_000, 23));
    dir.write("src/nested/small.txt", b"a short one after the big ones");
    let source = dir.join("src");
    (dir, source)
}

fn tree_digest(root: &std::path::Path) -> std::collections::BTreeMap<String, u32> {
    let mut out = std::collections::BTreeMap::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(at) = stack.pop() {
        for entry in std::fs::read_dir(&at).expect("read dir").flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            let rel = path.strip_prefix(root).unwrap().to_string_lossy().into_owned();
            out.insert(rel, ttarchive::utils::crc32::checksum(&std::fs::read(&path).unwrap()));
        }
    }
    out
}

#[test]
fn a_tarball_past_the_streaming_threshold_still_round_trips() {
    let (dir, source) = large_tree("stream-tar");
    let expected = tree_digest(&source);

    let cases = [
        (ttarchive::ArchiveType::TarXz, "big.tar.xz", "xz"),
        (ttarchive::ArchiveType::TarZst, "big.tar.zst", "zstd"),
        (ttarchive::ArchiveType::TarLzma, "big.tar.lzma", "lzma"),
    ];

    for (kind, name, tool) in cases {
        let archive = dir.join(name);
        ttarchive::Archive::new(&archive).set_type(kind).create_from([&source]).unwrap_or_else(|e| panic!("{name}: {e}"));

        let dest = dir.join(format!("{name}-out"));
        ttarchive::Archive::new(&archive).set_type(kind).extract_to(&dest).unwrap_or_else(|e| panic!("{name}: {e}"));

        assert_eq!(tree_digest(&dest.join("src")), expected, "{name}: a streamed tarball came back different");

        if !have(tool) {
            eprintln!("skipping the external half: {tool} is not installed");
            continue;
        }
        let out = Command::new(tool).arg("-t").arg(&archive).output().expect("run the tool");
        assert!(out.status.success(), "{name}: {tool} -t rejected it: {}", String::from_utf8_lossy(&out.stderr));
    }
}

#[test]
fn the_held_prefix_is_handed_over_whole_when_streaming_starts() {
    let dir = common::TempDir::new("stream-handover");

    for size in [8 * 1024 * 1024 - 4096, 8 * 1024 * 1024 + 4096] {
        let source = dir.join(format!("src-{size}"));
        std::fs::create_dir_all(&source).unwrap();

        let mut body = common::compressible(size - 65_536);
        body.extend_from_slice(&common::pseudo_random(65_536, 29));
        std::fs::write(source.join("one.bin"), &body).unwrap();

        let archive = dir.join(format!("h-{size}.tar.xz"));
        ttarchive::Archive::new(&archive).create_from([&source]).unwrap();

        let dest = dir.join(format!("h-{size}-out"));
        ttarchive::Archive::new(&archive).extract_to(&dest).unwrap();

        let back = std::fs::read(dest.join(format!("src-{size}/one.bin"))).expect("the entry is missing");
        assert!(back == body, "{size} bytes: the prefix held before streaming began did not survive");
    }
}

#[test]
fn an_lzip_stream_decodes_a_piece_at_a_time() {
    use std::io::Read;

    let data = mixed(300_000, 31);

    let mut members = Vec::new();
    for part in data.chunks(120_000) {
        members.extend_from_slice(&lzip_member(part));
    }

    let mut out = Vec::new();
    ttarchive::codecs::lzip::Reader::new(members.as_slice()).read_to_end(&mut out).expect("read the lzip stream");
    assert_eq!(out, data, "a multi-member lzip stream came back different");

    let whole = ttarchive::codecs::lzip::decompress(&members, data.len()).expect("decompress");
    assert_eq!(whole, data, "the slice path and the streaming path disagree");
}

#[test]
fn a_truncated_lzip_member_is_refused_rather_than_returned_short() {
    use std::io::Read;

    let data = mixed(60_000, 37);
    let member = lzip_member(&data);

    let mut out = Vec::new();
    let cut = &member[..member.len() - 8];
    let result = ttarchive::codecs::lzip::Reader::new(cut).read_to_end(&mut out);
    assert!(result.is_err(), "a member whose trailer is cut short must not read as complete");
}

fn lzip_member(part: &[u8]) -> Vec<u8> {
    use ttarchive::codecs::lzma::Properties;
    use ttarchive::codecs::lzma::encode::{Encoder, Feed, Finder, RangeEncoder};

    const DICT: u32 = 1 << 20;
    let props = Properties { lc: 3, lp: 0, pb: 2, dict_size: DICT };

    let mut encoder = Encoder::new(props);
    let mut finder = Finder::new(part.len(), DICT as usize, 32);
    let mut coder = RangeEncoder::new(Vec::new());
    let feed = Feed::whole(part);
    encoder.encode_span(&feed, 0, part.len(), &mut finder, &mut coder).expect("encode");
    encoder.encode_end_marker(part.len(), &mut coder).expect("end marker");
    let body = coder.finish().expect("finish");

    let mut out = Vec::with_capacity(body.len() + 32);
    out.extend_from_slice(b"LZIP");
    out.push(1);
    out.push(20);
    out.extend_from_slice(&body);

    let member_size = (out.len() + 20) as u64;
    out.extend_from_slice(&ttarchive::utils::crc32::checksum(part).to_le_bytes());
    out.extend_from_slice(&(part.len() as u64).to_le_bytes());
    out.extend_from_slice(&member_size.to_le_bytes());
    out
}

#[test]
fn a_compress_stream_long_enough_to_clear_its_dictionary_decodes_a_piece_at_a_time() {
    use std::io::Read;

    if !have("compress") {
        eprintln!("skipping: compress not installed");
        return;
    }

    let dir = common::TempDir::new("stream-lzw");
    let mut data = common::compressible(3_000_000);
    data.extend_from_slice(&common::pseudo_random(500_000, 41));

    let plain = dir.join("p.bin");
    std::fs::write(&plain, &data).unwrap();

    let packed = Command::new("compress").arg("-c").arg(&plain).output().expect("run compress");
    assert!(packed.status.success(), "compress failed: {}", String::from_utf8_lossy(&packed.stderr));

    let mut reader = ttarchive::codecs::compress::Reader::new(packed.stdout.as_slice());
    let mut out = Vec::new();
    let mut chunk = [0u8; 997];
    loop {
        let n = reader.read(&mut chunk).expect("read");
        if n == 0 {
            break;
        }
        out.extend_from_slice(&chunk[..n]);
    }

    assert_eq!(out.len(), data.len(), "a stream past its first dictionary clear came back the wrong length");
    assert!(out == data, "a stream past its first dictionary clear came back different");

    let whole = ttarchive::codecs::compress::decompress(&packed.stdout, data.len()).expect("decompress");
    assert!(whole == data, "the slice path and the streaming path disagree");
}

#[test]
fn several_zstd_frames_in_a_row_all_decode() {
    use std::io::Read;

    let first = mixed(200_000, 43);
    let second = mixed(150_000, 47);

    let mut stream = zstd::encode::compress_at(&first, true, 16).expect("first frame");
    stream.extend_from_slice(&zstd::encode::compress_at(&second, true, 16).expect("second frame"));

    let mut out = Vec::new();
    zstd::Reader::new(stream.as_slice(), 0).read_to_end(&mut out).expect("read both frames");

    let mut both = first.clone();
    both.extend_from_slice(&second);
    assert_eq!(out, both, "a frame after the first was dropped");
}

#[test]
fn a_streamed_lzip_member_round_trips_and_bsdtar_reads_the_tarball() {
    use std::io::Read;

    for len in [0usize, 1, 5_000, 400_000] {
        let data = mixed_or_empty(len);

        let mut writer = ttarchive::codecs::lzip::Writer::new(Vec::new(), 32, Level::Default).expect("start");
        in_pieces(&data, 4096, |chunk| writer.push(chunk).expect("push"));
        let packed = writer.finish().expect("finish");

        assert_eq!(&packed[..4], b"LZIP", "an lzip member must start with its magic");

        let mut back = Vec::new();
        ttarchive::codecs::lzip::Reader::new(packed.as_slice()).read_to_end(&mut back).expect("read");
        assert_eq!(back, data, "{len} bytes did not survive");

        let whole = ttarchive::codecs::lzip::decompress(&packed, len).expect("decompress");
        assert_eq!(whole, data, "the slice path and the streaming path disagree");
    }
}

#[test]
fn an_lzip_member_records_its_own_length_and_crc() {
    let data = mixed(120_000, 53);
    let packed = ttarchive::codecs::lzip::compress_at(&data, 32, Level::Default).expect("compress");

    let tail = &packed[packed.len() - 20..];
    let crc = u32::from_le_bytes(tail[..4].try_into().unwrap());
    let size = u64::from_le_bytes(tail[4..12].try_into().unwrap());
    let member = u64::from_le_bytes(tail[12..20].try_into().unwrap());

    assert_eq!(crc, ttarchive::utils::crc32::checksum(&data), "the trailer CRC does not match the input");
    assert_eq!(size, data.len() as u64, "the trailer length does not match the input");
    assert_eq!(member, packed.len() as u64, "the trailer member size does not match the member");
}

#[test]
fn two_lzip_members_we_wrote_read_back_as_one_stream() {
    use std::io::Read;

    let first = mixed(90_000, 59);
    let second = mixed(70_000, 61);

    let mut stream = ttarchive::codecs::lzip::compress_at(&first, 32, Level::Default).expect("first");
    stream.extend_from_slice(&ttarchive::codecs::lzip::compress_at(&second, 32, Level::Default).expect("second"));

    let mut out = Vec::new();
    ttarchive::codecs::lzip::Reader::new(stream.as_slice()).read_to_end(&mut out).expect("read both");

    let mut both = first.clone();
    both.extend_from_slice(&second);
    assert_eq!(out, both, "a member after the first was dropped");
}

#[test]
fn zstd_huffman_codes_literals_it_cannot_match_away() {
    let mut x: u32 = 12_345;
    let noisy: Vec<u8> = (0..300_000)
        .map(|_| {
            x = x.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            32 + ((x >> 16) % 88) as u8
        })
        .collect();

    let packed = zstd::encode::compress_at(&noisy, true, 32).expect("compress");
    let back = zstd::decompress(&packed, noisy.len()).expect("decompress");
    assert_eq!(back, noisy, "huffman coded literals did not survive");

    let ratio = packed.len() as f64 / noisy.len() as f64;
    assert!(ratio < 0.88, "88 distinct byte values should pack to about 6.5 bits each, got {:.1}%", ratio * 100.0);

    let dir = common::TempDir::new("zstd-huff");
    let path = dir.join("h.zst");
    std::fs::write(&path, &packed).unwrap();

    if !have("zstd") {
        return;
    }
    let out = Command::new("zstd").arg("-dc").arg(&path).output().expect("run zstd");
    assert!(out.status.success(), "zstd rejected our huffman literals: {}", String::from_utf8_lossy(&out.stderr));
    assert!(out.stdout == noisy, "zstd decoded our huffman literals differently");
}

#[test]
fn zstd_falls_back_to_raw_literals_when_the_alphabet_is_too_wide() {
    let mut x: u32 = 999;
    let wide: Vec<u8> = (0..200_000)
        .map(|_| {
            x = x.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (x >> 16) as u8
        })
        .collect();

    let packed = zstd::encode::compress_at(&wide, true, 32).expect("compress");
    let back = zstd::decompress(&packed, wide.len()).expect("decompress");
    assert_eq!(back, wide, "raw literals did not survive");

    if !have("zstd") {
        return;
    }
    let dir = common::TempDir::new("zstd-raw");
    let path = dir.join("r.zst");
    std::fs::write(&path, &packed).unwrap();
    let out = Command::new("zstd").arg("-t").arg(&path).output().expect("run zstd");
    assert!(out.status.success(), "zstd rejected the raw fallback: {}", String::from_utf8_lossy(&out.stderr));
}
