mod common;

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use common::{TempDir, compressible, pseudo_random};
use ttarchive::pipeline::sevenz;
use ttarchive::pipeline::{ExtractOptions, ExtractSummary};
use ttarchive::sevenz::SevenZReader;
use ttarchive::utils::progress::Reporter;
use ttarchive::{Archive, Password};

const CODERS: [&str; 9] = ["plain", "header-compressed", "bcj2", "ppmd", "bzip2", "deflate", "copy", "arm64", "delta"];

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sevenz").join(name)
}

fn expected() -> BTreeMap<String, Vec<u8>> {
    BTreeMap::from([
        ("src/prose.txt".to_string(), compressible(12_000)),
        ("src/noise.bin".to_string(), pseudo_random(8_000, 9)),
        ("src/nested/small.txt".to_string(), b"a few bytes".to_vec()),
        ("src/empty.txt".to_string(), Vec::new()),
    ])
}

fn extract(archive: &Path, dest: &Path, options: &ExtractOptions) -> ExtractSummary {
    sevenz::extract(archive, dest, options, &Reporter::disabled()).unwrap_or_else(|e| panic!("{}: {e}", archive.display()))
}

fn check(out: &Path) {
    for (name, bytes) in expected() {
        let found = fs::read(out.join(&name)).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(found, bytes, "{name}");
    }
}

#[test]
fn every_coder_in_the_fixture_set_extracts() {
    for name in CODERS {
        let dir = TempDir::new(&format!("fx7-{name}"));
        let archive = fixture(&format!("{name}.7z"));

        let summary = extract(&archive, dir.path(), &ExtractOptions::default());
        assert_eq!(summary.files, 4, "{name}");
        check(dir.path());
    }
}

#[test]
fn a_raw_lzma_folder_extracts() {
    let dir = TempDir::new("fx7-lzma1");
    extract(&fixture("lzma1.7z"), dir.path(), &ExtractOptions::default());
    check(dir.path());
}

#[test]
fn a_non_solid_archive_gives_every_file_its_own_folder() {
    let reader = SevenZReader::open(fixture("nonsolid.7z")).unwrap();
    assert!(reader.header().streams.folders.len() > 1);

    let dir = TempDir::new("fx7-nonsolid");
    extract(&fixture("nonsolid.7z"), dir.path(), &ExtractOptions::default());
    check(dir.path());
}

#[test]
fn the_bcj2_fixture_really_uses_four_input_streams() {
    let reader = SevenZReader::open(fixture("bcj2.7z")).unwrap();
    let names: Vec<&str> = reader.header().streams.folders.iter().flat_map(|f| f.coders.iter().map(|c| c.codec.name())).collect();

    assert!(names.iter().any(|n| *n == "BCJ2"), "the fixture should hold a BCJ2 folder, not {names:?}");

    let dir = TempDir::new("fx7-bcj2");
    extract(&fixture("bcj2.7z"), dir.path(), &ExtractOptions::default());
    check(dir.path());
}

#[test]
fn an_encrypted_fixture_needs_its_password_and_no_more() {
    let dir = TempDir::new("fx7-aes");

    for name in ["encrypted-data.7z", "encrypted-header.7z"] {
        let archive = fixture(name);
        assert!(sevenz::extract(&archive, &dir.join("nope"), &ExtractOptions::default(), &Reporter::disabled()).is_err(), "{name} without a password");

        let options = ExtractOptions { password: Some(Password::from("fixture")), ..ExtractOptions::default() };
        let out = dir.join(name);
        extract(&archive, &out, &options);
        check(&out);
    }
}

#[test]
fn an_encrypted_header_hides_the_names_until_the_password_arrives() {
    assert!(sevenz::entries(&fixture("encrypted-header.7z"), None).is_err());

    let listed = sevenz::entries(&fixture("encrypted-header.7z"), Some(&Password::from("fixture"))).unwrap();
    assert!(listed.iter().any(|e| e.name == "src/noise.bin"));

    let listed = sevenz::entries(&fixture("encrypted-data.7z"), None).unwrap();
    assert!(listed.iter().any(|e| e.name == "src/noise.bin"), "a plain header lists its names without a password");
}

#[test]
fn the_split_fixture_reads_from_its_first_volume_and_from_its_base_name() {
    let reader = SevenZReader::open(fixture("split.7z.001")).unwrap();
    assert!(reader.volumes().len() > 1);

    for named in ["split.7z.001", "split.7z"] {
        let dir = TempDir::new("fx7-split");
        extract(&fixture(named), dir.path(), &ExtractOptions::default());
        check(dir.path());
    }
}

#[test]
fn the_anti_fixture_carries_a_deletion_marker_the_cli_wrote() {
    let listed = sevenz::entries(&fixture("anti.7z"), None).unwrap();
    let marker = listed.iter().find(|e| e.name.ends_with("noise.bin")).expect("the marker is listed");

    assert!(marker.sevenz().unwrap().is_anti, "7-Zip's own update should have left an anti item");

    let dir = TempDir::new("fx7-anti");
    fs::create_dir_all(dir.join("src-anti")).unwrap();
    fs::write(dir.join("src-anti/noise.bin"), b"left over from an earlier restore").unwrap();

    let summary = extract(&fixture("anti.7z"), dir.path(), &ExtractOptions::default());
    assert_eq!(summary.deleted, 1);
    assert!(!dir.join("src-anti/noise.bin").exists());
}

#[test]
fn the_fixtures_list_the_same_entries_through_the_public_api() {
    for name in CODERS {
        let archive = fixture(&format!("{name}.7z"));
        let names: Vec<String> = Archive::new(&archive).entries().unwrap().into_iter().map(|e| e.name).collect();

        for wanted in expected().keys() {
            assert!(names.contains(wanted), "{name}: {wanted} missing from {names:?}");
        }
    }
}

#[test]
fn one_file_comes_out_of_every_fixture_without_the_others() {
    for name in CODERS {
        let dir = TempDir::new(&format!("fx7-one-{name}"));
        let options = ExtractOptions { selection: vec!["src/nested/small.txt".into()], ..ExtractOptions::default() };

        let summary = extract(&fixture(&format!("{name}.7z")), dir.path(), &options);
        assert_eq!(summary.files, 1, "{name}");
        assert_eq!(fs::read(dir.join("src/nested/small.txt")).unwrap(), b"a few bytes", "{name}");
        assert!(!dir.join("src/noise.bin").exists(), "{name}");
    }
}

#[test]
fn a_truncated_fixture_is_refused_rather_than_half_extracted() {
    let dir = TempDir::new("fx7-truncated");

    for name in CODERS {
        let whole = fs::read(fixture(&format!("{name}.7z"))).unwrap();
        let cut = dir.join(format!("{name}-cut.7z"));
        fs::write(&cut, &whole[..whole.len() * 2 / 3]).unwrap();

        let result = sevenz::extract(&cut, &dir.join(name), &ExtractOptions::default(), &Reporter::disabled());
        assert!(result.is_err(), "{name}: a truncated archive must not extract quietly");
    }
}

#[test]
fn a_fixture_with_a_rewritten_byte_fails_its_checksum() {
    let dir = TempDir::new("fx7-flipped");

    for name in CODERS {
        let mut bytes = fs::read(fixture(&format!("{name}.7z"))).unwrap();
        let at = bytes.len() / 2;
        bytes[at] ^= 0xFF;

        let path = dir.join(format!("{name}-flipped.7z"));
        fs::write(&path, &bytes).unwrap();

        let result = sevenz::extract(&path, &dir.join(name), &ExtractOptions::default(), &Reporter::disabled());
        assert!(result.is_err(), "{name}: a rewritten byte in the packed data must not pass");
    }
}
