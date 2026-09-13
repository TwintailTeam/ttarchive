mod common;

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;

use common::{TempDir, compressible, have, pseudo_random, skip};
use ttarchive::crypto::Password;
use ttarchive::sevenz::header::FileEntry;
use ttarchive::sevenz::spec::Codec;
use ttarchive::sevenz::{ArchiveSource, SevenZReader};

fn run(dir: &Path, program: &str, args: &[&str]) -> (bool, String) {
    let output = Command::new(common::resolve(program)).args(args).current_dir(dir).output().unwrap_or_else(|e| panic!("failed to run {program}: {e}"));
    let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&output.stderr));
    (output.status.success(), text)
}

fn must_run(dir: &Path, program: &str, args: &[&str]) -> String {
    let (ok, text) = run(dir, program, args);
    assert!(ok, "{program} {args:?} failed:\n{text}");
    text
}

fn source(dir: &TempDir) -> PathBuf {
    dir.write("src/prose.txt", compressible(30_000));
    dir.write("src/nested/noise.bin", pseudo_random(20_000, 11));
    dir.write("src/nested/deep/leaf.txt", b"leaf contents");
    dir.write("src/empty.txt", b"");
    dir.write("src/unicode-\u{00e9}\u{00fc}-\u{65e5}\u{672c}.txt", "non-ascii name");
    std::fs::create_dir_all(dir.join("src/empty-dir")).unwrap();
    dir.join("src")
}

fn pseudo_x86(len: usize) -> Vec<u8> {
    let mut out: Vec<u8> = Vec::with_capacity(len + 8);
    let mut state = 0x1234_5678u32;
    let mut next = move || {
        state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        state
    };

    while out.len() < len {
        let roll = next();
        if roll % 100 < 8 {
            out.push(if roll & 0x100 == 0 { 0xE8 } else { 0xE9 });
            let target = next() % (out.len() as u32 + 4096);
            out.extend_from_slice(&target.to_le_bytes());
        } else {
            out.push((0x40 + (roll >> 16) % 0x50) as u8);
        }
    }
    out.truncate(len);
    out
}

fn pseudo_riscv(len: usize) -> Vec<u8> {
    let mut out: Vec<u8> = Vec::with_capacity(len + 8);
    let mut state = 0x2468_ACE1u32;
    let mut next = move || {
        state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        state
    };

    while out.len() < len {
        match next() % 10 {
            0..=2 => {
                let imm = next() % 0x0010_0000;
                let jal = ((imm >> 20) & 1) << 31 | ((imm >> 1) & 0x3FF) << 21 | ((imm >> 11) & 1) << 20 | ((imm >> 12) & 0xFF) << 12 | 1 << 7 | 0x6F;
                out.extend_from_slice(&jal.to_le_bytes());
            }
            3..=5 => {
                let reg = 10 + next() % 5;
                let auipc = (next() % 0x0010_0000) << 12 | reg << 7 | 0x17;
                let addi = (next() % 0x1000) << 20 | reg << 15 | reg << 7 | 0x13;
                out.extend_from_slice(&auipc.to_le_bytes());
                out.extend_from_slice(&addi.to_le_bytes());
            }
            _ => out.extend_from_slice(&[0x01, 0x00]),
        }
    }
    out.truncate(len);
    out
}

fn build(dir: &TempDir, name: &str, switches: &[&str]) -> PathBuf {
    let mut args = vec!["a"];
    args.extend_from_slice(switches);
    args.push(name);
    args.push("src");
    must_run(dir.path(), "7z", &args);
    dir.join(name)
}

fn open(path: &Path) -> SevenZReader<ArchiveSource> {
    SevenZReader::open(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

fn open_with(path: &Path, password: &str) -> SevenZReader<ArchiveSource> {
    SevenZReader::open_with(path, Some(Password::from(password))).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

#[derive(Debug, Default, PartialEq, Eq)]
struct Listed {
    size: u64,
    crc: Option<u32>,
    directory: bool,
}

fn listing(dir: &Path, name: &str) -> Vec<(String, Listed)> {
    let text = must_run(dir, "7z", &["l", "-slt", name]);
    let body = text.split("----------").nth(1).unwrap_or("");

    let mut out = Vec::new();
    let mut path = None;
    let mut current = Listed::default();

    for line in body.lines() {
        let Some((key, value)) = line.split_once(" = ") else { continue };
        match key.trim() {
            "Path" => {
                if let Some(previous) = path.take() {
                    out.push((previous, std::mem::take(&mut current)));
                }
                path = Some(value.trim().replace('\\', "/"));
            }
            "Size" => current.size = value.trim().parse().unwrap_or(0),
            "CRC" => current.crc = u32::from_str_radix(value.trim(), 16).ok(),
            "Attributes" => current.directory = value.trim().starts_with('D'),
            _ => {}
        }
    }
    if let Some(previous) = path {
        out.push((previous, current));
    }
    out
}

fn ours(entry: &FileEntry, size: u64, crc: Option<u32>) -> (String, Listed) {
    (entry.name.replace('\\', "/"), Listed { size, crc, directory: entry.is_directory() })
}

fn parsed(reader: &SevenZReader<ArchiveSource>) -> Vec<(String, Listed)> {
    let header = reader.header();
    let mut streamed = header.streams.substreams.sizes.iter().zip(&header.streams.substreams.crcs);

    header
        .files
        .iter()
        .map(|entry| {
            if entry.has_stream {
                let (size, crc) = streamed.next().expect("a substream for every file with data");
                ours(entry, *size, *crc)
            } else {
                ours(entry, 0, None)
            }
        })
        .collect()
}

#[test]
fn a_plain_header_parses_to_what_the_cli_lists() {
    if !have("7z") {
        return skip("7z");
    }

    let dir = TempDir::new("sz-plain");
    source(&dir);
    build(&dir, "plain.7z", &["-mhc=off", "-m0=LZMA"]);

    assert_eq!(parsed(&open(&dir.join("plain.7z"))), listing(dir.path(), "plain.7z"));
}

#[test]
fn every_coder_the_cli_writes_produces_a_header_we_can_parse() {
    if !have("7z") {
        return skip("7z");
    }

    let dir = TempDir::new("sz-coders");
    source(&dir);

    let cases: [(&str, &[&str]); 8] = [
        ("copy.7z", &["-m0=Copy"]),
        ("lzma.7z", &["-m0=LZMA"]),
        ("lzma2.7z", &["-m0=LZMA2"]),
        ("ppmd.7z", &["-m0=PPMd"]),
        ("bzip2.7z", &["-m0=BZip2"]),
        ("deflate.7z", &["-m0=Deflate"]),
        ("delta.7z", &["-m0=Delta:4", "-m1=LZMA"]),
        ("bcj.7z", &["-m0=BCJ", "-m1=LZMA"]),
    ];

    for (name, coder) in cases {
        let mut switches = vec!["-mhc=off"];
        switches.extend_from_slice(coder);
        build(&dir, name, &switches);

        assert_eq!(parsed(&open(&dir.join(name))), listing(dir.path(), name), "{name}");
    }
}

#[test]
fn a_compressed_header_describes_the_same_archive_as_a_plain_one() {
    if !have("7z") {
        return skip("7z");
    }

    let dir = TempDir::new("sz-encoded");
    source(&dir);
    build(&dir, "plain.7z", &["-mhc=off", "-m0=LZMA2"]);
    build(&dir, "encoded.7z", &["-mhc=on", "-m0=LZMA2"]);

    let encoded = open(&dir.join("encoded.7z"));
    assert_eq!(parsed(&encoded), parsed(&open(&dir.join("plain.7z"))));
    assert_eq!(parsed(&encoded), listing(dir.path(), "encoded.7z"));

    check_every_file_decodes(&dir, "encoded.7z");
}

#[test]
fn the_default_switches_produce_an_archive_we_read_without_being_told_anything() {
    if !have("7z") {
        return skip("7z");
    }

    let dir = TempDir::new("sz-default");
    source(&dir);
    build(&dir, "default.7z", &[]);

    assert_eq!(parsed(&open(&dir.join("default.7z"))), listing(dir.path(), "default.7z"));
    check_every_file_decodes(&dir, "default.7z");
}

#[test]
fn a_solid_archive_and_a_non_solid_one_describe_the_same_files() {
    if !have("7z") {
        return skip("7z");
    }

    let dir = TempDir::new("sz-solid");
    source(&dir);
    build(&dir, "solid.7z", &["-mhc=off", "-ms=on", "-m0=LZMA2"]);
    build(&dir, "split.7z", &["-mhc=off", "-ms=off", "-m0=LZMA2"]);

    let solid = open(&dir.join("solid.7z"));
    let split = open(&dir.join("split.7z"));

    assert_eq!(parsed(&solid), parsed(&split));
    assert_eq!(parsed(&solid), listing(dir.path(), "solid.7z"));

    assert_eq!(solid.header().streams.folders.len(), 1, "a solid archive should pack every file into one folder");
    assert!(split.header().streams.folders.len() > 1, "a non-solid archive should give each file its own folder");
}

#[test]
fn a_folders_substream_sizes_add_up_to_the_folder_itself() {
    if !have("7z") {
        return skip("7z");
    }

    let dir = TempDir::new("sz-substreams");
    source(&dir);
    build(&dir, "solid.7z", &["-mhc=off", "-ms=on", "-m0=LZMA2"]);

    let reader = open(&dir.join("solid.7z"));
    let streams = &reader.header().streams;

    for (index, folder) in streams.folders.iter().enumerate() {
        let range = streams.substreams.range_of(index);
        let sum: u64 = streams.substreams.sizes[range].iter().sum();
        assert_eq!(sum, folder.unpack_size().unwrap(), "folder {index}");
    }
}

#[test]
fn a_directory_is_told_apart_from_an_empty_file() {
    if !have("7z") {
        return skip("7z");
    }

    let dir = TempDir::new("sz-empties");
    source(&dir);
    build(&dir, "empties.7z", &["-mhc=off", "-m0=LZMA"]);

    let reader = open(&dir.join("empties.7z"));
    let by_name = |name: &str| reader.header().files.iter().find(|f| f.name.replace('\\', "/") == name).unwrap_or_else(|| panic!("no entry {name}"));

    let empty_dir = by_name("src/empty-dir");
    assert!(empty_dir.is_directory());
    assert!(!empty_dir.has_stream);
    assert!(!empty_dir.is_empty_file);

    let empty_file = by_name("src/empty.txt");
    assert!(!empty_file.is_directory());
    assert!(!empty_file.has_stream);
    assert!(empty_file.is_empty_file);

    let with_data = by_name("src/prose.txt");
    assert!(with_data.has_stream);
    assert!(!with_data.is_directory());
}

#[test]
fn names_are_stored_with_a_forward_slash_and_survive_a_round_trip_through_utf16() {
    if !have("7z") {
        return skip("7z");
    }

    let dir = TempDir::new("sz-names");
    source(&dir);
    build(&dir, "names.7z", &["-mhc=off", "-m0=LZMA"]);

    let reader = open(&dir.join("names.7z"));
    let names: Vec<&str> = reader.header().files.iter().map(|f| f.name.as_str()).collect();

    assert!(names.contains(&"src/nested/deep/leaf.txt"), "{names:?}");
    assert!(names.contains(&"src/unicode-\u{00e9}\u{00fc}-\u{65e5}\u{672c}.txt"), "{names:?}");
    assert!(!names.iter().any(|name| name.contains('\\')), "{names:?}");
}

#[test]
fn an_archive_with_no_files_parses_as_empty() {
    if !have("7z") {
        return skip("7z");
    }

    let dir = TempDir::new("sz-empty");
    dir.write("src/only.txt", b"only");
    must_run(dir.path(), "7z", &["a", "empty.7z", "src"]);
    must_run(dir.path(), "7z", &["d", "-r", "empty.7z", "src"]);

    let reader = open(&dir.join("empty.7z"));
    assert!(reader.signature().is_empty());
    assert!(reader.header().files.is_empty());
    assert!(reader.header().streams.folders.is_empty());
}

#[test]
fn timestamps_and_attributes_come_back_for_every_entry() {
    if !have("7z") {
        return skip("7z");
    }

    let dir = TempDir::new("sz-attrs");
    source(&dir);
    build(&dir, "attrs.7z", &["-mhc=off", "-m0=LZMA"]);

    let reader = open(&dir.join("attrs.7z"));
    for entry in &reader.header().files {
        assert!(entry.mtime.is_some(), "{}: no modification time", entry.name);
        assert!(entry.attributes.is_some(), "{}: no attributes", entry.name);
    }
}

#[test]
fn a_header_whose_checksum_does_not_match_is_refused() {
    if !have("7z") {
        return skip("7z");
    }

    let dir = TempDir::new("sz-corrupt");
    source(&dir);
    let path = build(&dir, "corrupt.7z", &["-mhc=off", "-m0=LZMA"]);

    let mut bytes = std::fs::read(&path).unwrap();
    let last = bytes.len() - 1;
    bytes[last] ^= 0xFF;
    std::fs::write(&path, &bytes).unwrap();

    let error = SevenZReader::open(&path).unwrap_err();
    assert!(matches!(error, ttarchive::Error::ChecksumMismatch { .. }), "{error}");
}

#[test]
fn an_encrypted_folder_decodes_with_the_password_the_cli_took() {
    if !have("7z") {
        return skip("7z");
    }

    let dir = TempDir::new("sz-aes");
    source(&dir);

    for coder in ["LZMA2", "PPMd", "BZip2"] {
        let name = format!("aes-{}.7z", coder.to_lowercase());
        build(&dir, &name, &["-mhc=off", &format!("-m0={coder}"), "-pcorrect horse battery"]);
        check_folders_decode(&open_with(&dir.join(&name), "correct horse battery"), &dir, &name);
    }
}

#[test]
fn an_encrypted_header_is_decoded_before_the_archive_is_listed() {
    if !have("7z") {
        return skip("7z");
    }

    let dir = TempDir::new("sz-aes-header");
    source(&dir);
    build(&dir, "plain.7z", &["-mhc=off", "-m0=LZMA2"]);
    build(&dir, "hidden.7z", &["-mhe=on", "-m0=LZMA2", "-pcorrect horse battery"]);

    let reader = open_with(&dir.join("hidden.7z"), "correct horse battery");
    assert_eq!(parsed(&reader), parsed(&open(&dir.join("plain.7z"))));
    check_folders_decode(&reader, &dir, "hidden.7z");
}

#[test]
fn an_encrypted_archive_opened_without_a_password_asks_for_one() {
    if !have("7z") {
        return skip("7z");
    }

    let dir = TempDir::new("sz-aes-missing");
    source(&dir);
    build(&dir, "data.7z", &["-mhc=off", "-m0=LZMA2", "-pcorrect horse battery"]);
    build(&dir, "hidden.7z", &["-mhe=on", "-m0=LZMA2", "-pcorrect horse battery"]);

    let reader = open(&dir.join("data.7z"));
    let location = reader.locations().unwrap().remove(0);
    let error = reader.entry_reader(&location).err().expect("an encrypted folder without a password");
    assert!(error.needs_password(), "{error}");

    let error = SevenZReader::open(&dir.join("hidden.7z")).unwrap_err();
    assert!(error.needs_password(), "{error}");
}

#[test]
fn a_wrong_password_never_passes_for_the_right_one() {
    if !have("7z") {
        return skip("7z");
    }

    let dir = TempDir::new("sz-aes-wrong");
    source(&dir);
    build(&dir, "data.7z", &["-mhc=off", "-m0=LZMA2", "-pcorrect horse battery"]);
    build(&dir, "hidden.7z", &["-mhe=on", "-m0=LZMA2", "-pcorrect horse battery"]);

    assert!(SevenZReader::open_with(&dir.join("hidden.7z"), Some(Password::from("wrong"))).is_err());

    let reader = open_with(&dir.join("data.7z"), "wrong");
    let location = reader.locations().unwrap().remove(0);
    let expected = std::fs::read(dir.join(reader.header().files[location.file].name.replace('\\', "/"))).unwrap();

    let mut got = Vec::new();
    let outcome = reader.entry_reader(&location).unwrap().read_to_end(&mut got);
    assert!(outcome.is_err() || got != expected, "the wrong password produced the right bytes");
}

fn check_every_file_decodes(dir: &TempDir, name: &str) {
    check_folders_decode(&open(&dir.join(name)), dir, name)
}

fn check_folders_decode(reader: &SevenZReader<ArchiveSource>, dir: &TempDir, name: &str) {
    let files = &reader.header().files;

    for location in reader.locations().unwrap() {
        let entry = &files[location.file];
        let expected = std::fs::read(dir.join(entry.name.replace('\\', "/"))).unwrap_or_else(|e| panic!("{name}: {}: {e}", entry.name));

        let mut got = Vec::new();
        reader.entry_reader(&location).unwrap().read_to_end(&mut got).unwrap_or_else(|e| panic!("{name}: {}: {e}", entry.name));

        assert_eq!(got.len() as u64, location.size, "{name}: {} decoded to the wrong length", entry.name);
        assert!(got == expected, "{name}: {} decoded to the wrong bytes", entry.name);

        if let Some(crc) = location.crc {
            assert_eq!(ttarchive::utils::crc32::checksum(&got), crc, "{name}: {} failed its checksum", entry.name);
        }
    }
}

#[test]
fn every_file_of_a_solid_folder_decodes_to_the_bytes_it_was_made_from() {
    if !have("7z") {
        return skip("7z");
    }

    let dir = TempDir::new("sz-decode-solid");
    source(&dir);
    build(&dir, "solid.7z", &["-mhc=off", "-ms=on", "-m0=LZMA2"]);

    let reader = open(&dir.join("solid.7z"));
    assert_eq!(reader.header().streams.folders.len(), 1);
    assert!(reader.locations().unwrap().len() > 1, "a solid folder should hold several files");

    check_every_file_decodes(&dir, "solid.7z");
}

#[test]
fn every_file_of_a_non_solid_archive_decodes_to_the_bytes_it_was_made_from() {
    if !have("7z") {
        return skip("7z");
    }

    let dir = TempDir::new("sz-decode-split");
    source(&dir);

    for (name, coder) in [("copy.7z", "Copy"), ("lzma.7z", "LZMA"), ("lzma2.7z", "LZMA2")] {
        build(&dir, name, &["-mhc=off", "-ms=off", &format!("-m0={coder}")]);
        check_every_file_decodes(&dir, name);
    }
}

#[test]
fn every_thin_coder_decodes_to_the_bytes_it_was_made_from() {
    if !have("7z") {
        return skip("7z");
    }

    let dir = TempDir::new("sz-thin");
    source(&dir);

    for coder in ["BZip2", "Deflate", "Delta:4", "BCJ", "ARM", "ARMT", "PPC", "SPARC", "ARM64", "IA64", "RISCV"] {
        let name = format!("{}.7z", coder.replace(':', "-").to_lowercase());
        build(&dir, &name, &["-mhc=off", &format!("-m0={coder}"), "-m1=LZMA"]);
        check_every_file_decodes(&dir, &name);
    }
}

#[test]
fn a_filter_chain_puts_the_branch_converter_after_the_compressor() {
    if !have("7z") {
        return skip("7z");
    }

    let dir = TempDir::new("sz-chain");
    source(&dir);
    build(&dir, "bcj.7z", &["-mhc=off", "-m0=BCJ", "-m1=LZMA"]);

    let reader = open(&dir.join("bcj.7z"));
    let folder = &reader.header().streams.folders[0];

    assert_eq!(folder.coders.len(), 2, "a filtered folder chains two coders");
    assert_eq!(folder.bind_pairs.len(), 1);
    assert!(folder.is_linear());
    assert_eq!(folder.packed_indices.len(), 1);
}

#[test]
fn a_bcj2_folder_decodes_from_all_four_of_its_streams() {
    if !have("7z") {
        return skip("7z");
    }

    let dir = TempDir::new("sz-bcj2");
    source(&dir);
    dir.write("src/code.bin", pseudo_x86(160_000));
    build(&dir, "bcj2.7z", &["-mhc=off", "-m0=BCJ2", "-m1=LZMA", "-m2=LZMA", "-m3=LZMA"]);

    let reader = open(&dir.join("bcj2.7z"));
    let folder = reader.header().streams.folders.iter().find(|f| f.coders.len() == 4).expect("a four coder folder");
    let converter = folder.coders.iter().find(|c| c.codec == Codec::Bcj2).expect("a BCJ2 coder");

    assert_eq!(converter.in_streams, 4, "BCJ2 takes four inputs");
    assert_eq!(folder.bind_pairs.len(), 3, "three of those inputs come from the other coders");
    assert_eq!(folder.packed_indices.len(), 4);
    assert!(!folder.is_linear());

    check_every_file_decodes(&dir, "bcj2.7z");
}

#[test]
fn bcj2_and_plain_bcj_decode_the_same_binary_to_the_same_bytes() {
    if !have("7z") {
        return skip("7z");
    }

    let dir = TempDir::new("sz-bcj2-vs-bcj");
    dir.write("src/code.bin", pseudo_x86(160_000));
    build(&dir, "two.7z", &["-mhc=off", "-m0=BCJ2", "-m1=LZMA", "-m2=LZMA", "-m3=LZMA"]);
    build(&dir, "one.7z", &["-mhc=off", "-m0=BCJ", "-m1=LZMA"]);

    let expected = std::fs::read(dir.join("src/code.bin")).unwrap();
    for name in ["two.7z", "one.7z"] {
        let reader = open(&dir.join(name));
        let location = reader.locations().unwrap().remove(0);
        let mut got = Vec::new();
        reader.entry_reader(&location).unwrap().read_to_end(&mut got).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert!(got == expected, "{name}: decoded to the wrong bytes");
    }
}

#[test]
fn ppmd_variant_h_decodes_at_every_order_the_cli_offers() {
    if !have("7z") {
        return skip("7z");
    }

    let dir = TempDir::new("sz-ppmd");
    dir.write("src/repetitive.txt", compressible(40_000));
    dir.write("src/noise.bin", pseudo_random(30_000, 5));

    for order in ["2", "3", "4", "6", "16", "32"] {
        let name = format!("ppmd-o{order}.7z");
        build(&dir, &name, &["-mhc=off", "-m0=PPMd", &format!("-mo={order}")]);
        check_every_file_decodes(&dir, &name);
    }
}

#[test]
fn a_ppmd_folder_whose_packed_bytes_were_rewritten_is_refused_rather_than_returned() {
    if !have("7z") {
        return skip("7z");
    }

    let dir = TempDir::new("sz-ppmd-cut");
    dir.write("src/noise.bin", pseudo_random(60_000, 7));
    let path = build(&dir, "cut.7z", &["-mhc=off", "-m0=PPMd"]);

    let mut bytes = std::fs::read(&path).unwrap();
    let packed_end = 32 + u64::from_le_bytes(bytes[12..20].try_into().unwrap()) as usize;
    bytes[packed_end - 400..packed_end].fill(0);
    std::fs::write(&path, &bytes).unwrap();

    let reader = open(&path);
    let location = reader.locations().unwrap().remove(0);

    let mut sink = Vec::new();
    let error = reader.entry_reader(&location).unwrap().read_to_end(&mut sink).unwrap_err();
    assert!(error.to_string().contains("ppmd"), "{error}");
}

#[test]
fn a_ppmd_folder_that_outgrows_its_sub_allocator_decodes_across_the_restarts() {
    if !have("7z") {
        return skip("7z");
    }

    let dir = TempDir::new("sz-ppmd-mem");
    dir.write("src/repetitive.txt", compressible(400_000));
    dir.write("src/noise.bin", pseudo_random(300_000, 5));

    for order in ["4", "32"] {
        let name = format!("ppmd-small-o{order}.7z");
        build(&dir, &name, &["-mhc=off", "-m0=PPMd", &format!("-mo={order}"), "-mmem=16"]);
        check_every_file_decodes(&dir, &name);
    }
}

#[test]
fn a_riscv_folder_decodes_to_the_bytes_it_was_made_from() {
    if !have("7z") {
        return skip("7z");
    }

    let dir = TempDir::new("sz-riscv");
    let code = pseudo_riscv(160_000);
    dir.write("src/code.bin", &code);
    build(&dir, "riscv.7z", &["-mhc=off", "-m0=RISCV", "-m1=LZMA"]);
    check_every_file_decodes(&dir, "riscv.7z");

    let mut converted = code.clone();
    ttarchive::codecs::bcj::decode(ttarchive::codecs::bcj::RISCV, &[], &mut converted).unwrap();
    assert_ne!(converted, code, "the converter left this stream untouched, so the archive proves nothing");
}

#[test]
fn a_file_in_a_solid_folder_is_found_at_the_offset_the_cli_extracts_it_from() {
    if !have("7z") {
        return skip("7z");
    }

    let dir = TempDir::new("sz-offsets");
    source(&dir);
    build(&dir, "solid.7z", &["-mhc=off", "-ms=on", "-m0=LZMA2"]);

    let reader = open(&dir.join("solid.7z"));
    let mut whole = Vec::new();
    reader.folder_reader(0).unwrap().read_to_end(&mut whole).unwrap();

    let extracted = dir.join("cli-out");
    must_run(dir.path(), "7z", &["x", "-y", &format!("-o{}", extracted.display()), "solid.7z"]);

    for location in reader.locations().unwrap() {
        let name = reader.header().files[location.file].name.replace('\\', "/");
        let expected = std::fs::read(extracted.join(&name)).unwrap_or_else(|e| panic!("{name}: {e}"));

        let start = location.offset as usize;
        let end = start + location.size as usize;
        assert!(end <= whole.len(), "{name}: lies past the folder");
        assert!(whole[start..end] == expected[..], "{name}: our offset {start} does not hold what the cli extracted");
    }
}

#[test]
fn a_folder_whose_stored_chunks_interrupt_its_compressed_ones_keeps_the_state_across_them() {
    if !have("7z") {
        return skip("7z");
    }

    let dir = TempDir::new("sz-lzma2-mixed");
    dir.write("src/prose.txt", compressible(2_000_000));
    dir.write("src/noise.bin", pseudo_random(6_000_000, 21));
    dir.write("src/more-prose.txt", compressible(1_500_000));

    for dictionary in ["1m", "4m", "8m", "12m", "64m"] {
        let name = format!("mixed-{dictionary}.7z");
        build(&dir, &name, &["-mhc=off", &format!("-md={dictionary}")]);

        let reader = open(&dir.join(&name));
        let mut whole = Vec::new();
        reader.folder_reader(0).unwrap().read_to_end(&mut whole).unwrap_or_else(|e| panic!("{name}: {e}"));

        let mut expected = compressible(1_500_000);
        expected.extend_from_slice(&pseudo_random(6_000_000, 21));
        expected.extend_from_slice(&compressible(2_000_000));
        assert_eq!(whole.len(), expected.len(), "{name}");
        assert!(whole == expected, "{name}: the folder decoded to the wrong bytes");
    }
}
