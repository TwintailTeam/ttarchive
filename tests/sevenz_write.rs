mod common;

use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;

use common::{TempDir, compressible, have, pseudo_random, skip};
use ttarchive::crypto::Password;
use ttarchive::sevenz::SevenZReader;
use ttarchive::sevenz::spec::Branch;
use ttarchive::sevenz::spec::attribute;
use ttarchive::sevenz::writer::{Filter, Item, Options, SevenZWriter};

fn file(name: &str) -> Item {
    Item { name: name.into(), mtime: Some(1_700_000_000), attributes: Some(0x20) }
}

fn directory(name: &str) -> Item {
    Item { name: name.into(), mtime: Some(1_700_000_000), attributes: Some(attribute::DIRECTORY) }
}

fn sample() -> Vec<(Item, Vec<u8>)> {
    vec![
        (file("readme.txt"), b"hello world".to_vec()),
        (file("prose.txt"), compressible(40_000)),
        (file("nested/noise.bin"), pseudo_random(30_000, 9)),
        (file("empty.txt"), Vec::new()),
        (file("unicode-\u{00e9}\u{672c}.txt"), b"non-ascii name".to_vec()),
    ]
}

fn write(path: &Path, options: Options) -> Vec<(Item, Vec<u8>)> {
    let items = sample();
    let mut writer = SevenZWriter::new(File::create(path).unwrap(), options).unwrap();

    writer.add_directory(&directory("nested"));
    for (item, data) in &items {
        writer.add_file(item, data).unwrap();
    }
    writer.finish().unwrap();

    items
}

fn read_back(path: &Path) -> Vec<(String, Vec<u8>)> {
    read_back_with(path, None)
}

fn read_back_with(path: &Path, password: Option<&str>) -> Vec<(String, Vec<u8>)> {
    let reader = SevenZReader::open_with(path, password.map(Password::from)).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let files = &reader.header().files;

    let mut out = Vec::new();
    for (index, entry) in files.iter().enumerate() {
        if entry.is_directory() {
            out.push((format!("{}/", entry.name), Vec::new()));
            continue;
        }
        let location = reader.locations().unwrap().into_iter().find(|l| l.file == index);
        let mut bytes = Vec::new();
        if let Some(location) = location {
            reader.entry_reader(&location).unwrap().read_to_end(&mut bytes).unwrap();
            assert_eq!(bytes.len() as u64, location.size, "{}", entry.name);
            assert_eq!(Some(ttarchive::utils::crc32::checksum(&bytes)), location.crc, "{}", entry.name);
        }
        out.push((entry.name.clone(), bytes));
    }
    out
}

#[test]
fn what_the_writer_emits_our_own_reader_reads_back() {
    let dir = TempDir::new("sz-w-round");

    for block in [None, Some(8_192), Some(1 << 30)] {
        for compress_header in [false, true] {
            let path = dir.join(format!("round-{}-{compress_header}.7z", block.unwrap_or(0)));
            let items = write(&path, Options { block_size: block, compress_header, ..Options::default() });

            let mut expected: Vec<(String, Vec<u8>)> = vec![("nested/".into(), Vec::new())];
            expected.extend(items.into_iter().map(|(item, data)| (item.name, data)));

            assert_eq!(read_back(&path), expected, "block {block:?} header {compress_header}");
        }
    }
}

#[test]
fn a_solid_block_limit_splits_the_archive_into_several_folders() {
    let dir = TempDir::new("sz-w-folders");

    let one = dir.join("one.7z");
    write(&one, Options { block_size: Some(1 << 30), ..Options::default() });
    assert_eq!(SevenZReader::open(&one).unwrap().header().streams.folders.len(), 1);

    let many = dir.join("many.7z");
    write(&many, Options { block_size: Some(8_192), ..Options::default() });
    assert!(SevenZReader::open(&many).unwrap().header().streams.folders.len() > 1);

    let each = dir.join("each.7z");
    write(&each, Options { block_size: None, ..Options::default() });
    assert_eq!(SevenZReader::open(&each).unwrap().header().streams.folders.len(), 4);
}

#[test]
fn a_compressed_header_is_smaller_and_says_the_same_thing() {
    let dir = TempDir::new("sz-w-hc");

    let plain = dir.join("plain.7z");
    let encoded = dir.join("encoded.7z");
    write(&plain, Options { compress_header: false, ..Options::default() });
    write(&encoded, Options { compress_header: true, ..Options::default() });

    assert_eq!(read_back(&plain), read_back(&encoded));
    assert!(std::fs::metadata(&encoded).unwrap().len() < std::fs::metadata(&plain).unwrap().len());
}

#[test]
fn an_archive_with_no_entries_at_all_is_still_a_valid_container() {
    let dir = TempDir::new("sz-w-empty");
    let path = dir.join("empty.7z");

    SevenZWriter::new(File::create(&path).unwrap(), Options::default()).unwrap().finish().unwrap();

    let reader = SevenZReader::open(&path).unwrap();
    assert!(reader.header().files.is_empty());
    assert!(reader.locations().unwrap().is_empty());
}

#[test]
fn an_encrypted_archive_round_trips_through_our_own_reader() {
    let dir = TempDir::new("sz-w-aes");

    for encrypt_header in [false, true] {
        let path = dir.join(format!("aes-{encrypt_header}.7z"));
        let options = Options { password: Some(Password::from("correct horse battery")), encrypt_header, ..Options::default() };
        let items = write(&path, options);

        let mut expected: Vec<(String, Vec<u8>)> = vec![("nested/".into(), Vec::new())];
        expected.extend(items.into_iter().map(|(item, data)| (item.name, data)));

        assert_eq!(read_back_with(&path, Some("correct horse battery")), expected, "header {encrypt_header}");
    }
}

#[test]
fn an_encrypted_archive_is_useless_without_the_password() {
    let dir = TempDir::new("sz-w-aes-none");

    let data = dir.join("data.7z");
    write(&data, Options { password: Some(Password::from("secret")), ..Options::default() });

    let reader = SevenZReader::open(&data).unwrap();
    let location = reader.locations().unwrap().remove(0);
    assert!(reader.entry_reader(&location).is_err());

    let hidden = dir.join("hidden.7z");
    write(&hidden, Options { password: Some(Password::from("secret")), encrypt_header: true, ..Options::default() });
    assert!(SevenZReader::open(&hidden).is_err());
}

#[test]
fn encrypting_a_header_without_a_password_is_refused() {
    let dir = TempDir::new("sz-w-aes-bad");
    let options = Options { encrypt_header: true, ..Options::default() };
    assert!(SevenZWriter::new(File::create(dir.join("bad.7z")).unwrap(), options).is_err());
}

fn run_7z(dir: &Path, args: &[&str]) -> (bool, String) {
    let output = Command::new(common::resolve("7z")).args(args).current_dir(dir).output().unwrap_or_else(|e| panic!("failed to run 7z: {e}"));
    let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&output.stderr));
    (output.status.success(), text)
}

#[test]
fn the_cli_tests_and_extracts_what_we_wrote() {
    if !have("7z") {
        return skip("7z");
    }

    let dir = TempDir::new("sz-w-cli");

    for (block, compress_header) in [(None, false), (Some(8_192), true), (Some(1 << 30), true), (Some(1 << 30), false)] {
        let name = format!("cli-{}-{compress_header}.7z", block.unwrap_or(0));
        let items = write(&dir.join(&name), Options { block_size: block, compress_header, ..Options::default() });

        let (ok, text) = run_7z(dir.path(), &["t", &name]);
        assert!(ok, "7z t rejected our archive ({block:?}):\n{text}");

        let out = format!("out-{}-{compress_header}", block.unwrap_or(0));
        let (ok, text) = run_7z(dir.path(), &["x", "-y", &format!("-o{out}"), &name]);
        assert!(ok, "7z x failed ({block:?}):\n{text}");

        for (item, data) in &items {
            let path = dir.join(&out).join(&item.name);
            assert_eq!(&std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display())), data, "{}", item.name);
        }
        assert!(dir.join(&out).join("nested").is_dir());
    }
}

#[test]
fn the_cli_tests_and_extracts_what_we_encrypted() {
    if !have("7z") {
        return skip("7z");
    }

    let dir = TempDir::new("sz-w-aes-cli");

    for encrypt_header in [false, true] {
        let name = format!("aes-{encrypt_header}.7z");
        let options = Options { password: Some(Password::from("correct horse battery")), encrypt_header, ..Options::default() };
        let items = write(&dir.join(&name), options);

        let (ok, text) = run_7z(dir.path(), &["t", "-pcorrect horse battery", &name]);
        assert!(ok, "7z t rejected our encrypted archive (header {encrypt_header}):\n{text}");

        let out = format!("out-{encrypt_header}");
        let (ok, text) = run_7z(dir.path(), &["x", "-y", "-pcorrect horse battery", &format!("-o{out}"), &name]);
        assert!(ok, "7z x failed (header {encrypt_header}):\n{text}");

        for (item, data) in &items {
            let path = dir.join(&out).join(&item.name);
            assert_eq!(&std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display())), data, "{}", item.name);
        }
    }
}

#[test]
fn the_cli_lists_the_names_and_sizes_we_recorded() {
    if !have("7z") {
        return skip("7z");
    }

    let dir = TempDir::new("sz-w-list");
    let items = write(&dir.join("list.7z"), Options::default());

    let (ok, text) = run_7z(dir.path(), &["l", "-slt", "list.7z"]);
    assert!(ok, "{text}");

    let listed: Vec<PathBuf> = text.lines().filter_map(|l| l.strip_prefix("Path = ")).map(|p| PathBuf::from(p.trim().replace('\\', "/"))).skip(1).collect();

    for (item, _) in &items {
        assert!(listed.contains(&PathBuf::from(&item.name)), "{} missing from {listed:?}", item.name);
    }
    assert!(listed.contains(&PathBuf::from("nested")));
}

fn write_one(path: &Path, name: &str, data: &[u8], options: Options) {
    let mut writer = SevenZWriter::new(File::create(path).unwrap(), options).unwrap();
    writer.add_file(&file(name), data).unwrap();
    writer.finish().unwrap();
}

#[test]
fn a_filtered_folder_round_trips_through_our_own_reader() {
    let dir = TempDir::new("sz-w-filter");
    let code = common::machine_code();

    let filters = [
        Filter::Branch(Branch::X86),
        Filter::Branch(Branch::Ppc),
        Filter::Branch(Branch::Ia64),
        Filter::Branch(Branch::Arm),
        Filter::Branch(Branch::ArmThumb),
        Filter::Branch(Branch::Sparc),
        Filter::Branch(Branch::Arm64),
        Filter::Branch(Branch::RiscV),
        Filter::Delta(1),
        Filter::Delta(4),
        Filter::Delta(256),
    ];

    for (index, filter) in filters.into_iter().enumerate() {
        let path = dir.join(format!("filtered-{index}.7z"));
        write_one(&path, "code.bin", &code, Options { filter: Some(filter), ..Options::default() });

        let reader = SevenZReader::open(&path).unwrap();
        assert_eq!(reader.header().streams.folders[0].coders.len(), 2, "{filter:?}");

        let location = reader.locations().unwrap().remove(0);
        let mut back = Vec::new();
        reader.entry_reader(&location).unwrap().read_to_end(&mut back).unwrap();
        assert_eq!(back, code, "{filter:?}");
    }
}

#[test]
fn a_filter_changes_what_is_stored_and_an_encrypted_one_still_does() {
    let dir = TempDir::new("sz-w-filter-aes");
    let code = common::machine_code();

    let plain = dir.join("plain.7z");
    let filtered = dir.join("filtered.7z");
    write_one(&plain, "code.bin", &code, Options::default());
    write_one(&filtered, "code.bin", &code, Options { filter: Some(Filter::Branch(Branch::X86)), ..Options::default() });

    let size = |path: &Path| std::fs::metadata(path).unwrap().len();
    assert!(size(&filtered) < size(&plain), "x86 machine code should pack smaller once its calls are converted: {} vs {}", size(&filtered), size(&plain));

    let secret = dir.join("secret.7z");
    let options = Options { filter: Some(Filter::Branch(Branch::X86)), password: Some(Password::from("pw")), encrypt_header: true, ..Options::default() };
    write_one(&secret, "code.bin", &code, options);

    let reader = SevenZReader::open_with(&secret, Some(Password::from("pw"))).unwrap();
    assert_eq!(reader.header().streams.folders[0].coders.len(), 3);

    let location = reader.locations().unwrap().remove(0);
    let mut back = Vec::new();
    reader.entry_reader(&location).unwrap().read_to_end(&mut back).unwrap();
    assert_eq!(back, code);
}

#[test]
fn the_cli_accepts_a_filtered_folder() {
    if !have("7z") {
        return skip("7z");
    }

    let dir = TempDir::new("sz-w-filter-cli");
    let code = common::machine_code();

    for (index, (filter, expected)) in [
        (Filter::Branch(Branch::X86), "BCJ"),
        (Filter::Branch(Branch::Arm64), "ARM64"),
        (Filter::Branch(Branch::RiscV), "RISCV"),
        (Filter::Branch(Branch::Sparc), "SPARC"),
        (Filter::Delta(4), "Delta"),
    ]
    .into_iter()
    .enumerate()
    {
        let name = format!("filtered-{index}.7z");
        write_one(&dir.join(&name), "code.bin", &code, Options { filter: Some(filter), ..Options::default() });

        let (ok, text) = run_7z(dir.path(), &["l", "-slt", &name]);
        assert!(ok, "{text}");
        assert!(text.contains(expected), "7z does not name the {expected} coder in:\n{text}");

        let (ok, text) = run_7z(dir.path(), &["t", &name]);
        assert!(ok, "7z t rejected our {expected} folder:\n{text}");

        let out = format!("out-{index}");
        let (ok, text) = run_7z(dir.path(), &["x", "-y", &format!("-o{out}"), &name]);
        assert!(ok, "7z x failed for {expected}:\n{text}");
        assert_eq!(std::fs::read(dir.join(&out).join("code.bin")).unwrap(), code, "{expected}");
    }
}

#[test]
fn the_cli_accepts_a_filtered_and_encrypted_folder() {
    if !have("7z") {
        return skip("7z");
    }

    let dir = TempDir::new("sz-w-filter-aes-cli");
    let code = common::machine_code();

    let options =
        Options { filter: Some(Filter::Branch(Branch::X86)), password: Some(Password::from("correct horse")), encrypt_header: true, ..Options::default() };
    write_one(&dir.join("secret.7z"), "code.bin", &code, options);

    let (ok, text) = run_7z(dir.path(), &["t", "-pcorrect horse", "secret.7z"]);
    assert!(ok, "7z t rejected our filtered, encrypted folder:\n{text}");

    let (ok, text) = run_7z(dir.path(), &["x", "-y", "-pcorrect horse", "-oout", "secret.7z"]);
    assert!(ok, "7z x failed:\n{text}");
    assert_eq!(std::fs::read(dir.join("out").join("code.bin")).unwrap(), code);
}

#[test]
fn the_cli_accepts_an_archive_holding_a_deletion_marker() {
    if !have("7z") {
        return skip("7z");
    }

    let dir = TempDir::new("sz-w-anti-cli");
    let path = dir.join("anti.7z");

    let mut writer = SevenZWriter::new(File::create(&path).unwrap(), Options::default()).unwrap();
    writer.add_file(&file("keep.txt"), b"still here").unwrap();
    writer.add_anti(&file("gone.txt"));
    writer.finish().unwrap();

    let (ok, text) = run_7z(dir.path(), &["t", "anti.7z"]);
    assert!(ok, "7z t rejected an archive with a deletion marker:\n{text}");

    let (ok, text) = run_7z(dir.path(), &["l", "-slt", "anti.7z"]);
    assert!(ok, "{text}");
    assert!(text.contains("gone.txt"), "7z does not list the marker:\n{text}");
    assert!(text.contains("Anti = +"), "7z does not report the entry as an anti item:\n{text}");
}

#[test]
fn a_delta_distance_the_coder_cannot_record_is_refused_before_anything_is_written() {
    let dir = TempDir::new("sz-w-delta-range");

    for distance in [0u16, 257, 1000] {
        let path = dir.join(format!("delta-{distance}.7z"));
        let options = Options { filter: Some(Filter::Delta(distance)), ..Options::default() };
        assert!(SevenZWriter::new(File::create(&path).unwrap(), options).is_err(), "distance {distance} should be refused");
    }

    for distance in [1u16, 256] {
        let path = dir.join(format!("ok-{distance}.7z"));
        let options = Options { filter: Some(Filter::Delta(distance)), ..Options::default() };
        assert!(SevenZWriter::new(File::create(&path).unwrap(), options).is_ok(), "distance {distance} is legal");
    }
}
