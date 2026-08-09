mod common;

use std::io::Read;
use std::path::{Path, PathBuf};

use ttarchive::{Archive, ArchiveType};

fn fixture(group: &str, name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(group).join(name)
}

fn holes() -> Vec<u8> {
    let mut data = vec![0u8; 1024 * 1024];
    data[..16].copy_from_slice(b"HEADHEADHEADHEAD");
    data[512 * 1024..512 * 1024 + 24].copy_from_slice(b"MIDDLEMIDDLEMIDDLEMIDDLE");
    let end = data.len() - 16;
    data[end..].copy_from_slice(b"TAILTAILTAILTAIL");
    data
}

fn plain() -> Vec<u8> {
    let phrase = b"the quick brown fox jumps over the lazy dog ";
    phrase.iter().copied().cycle().take(80_000).collect()
}

#[test]
fn every_sparse_layout_reads_from_a_fixture() {
    let expected = holes();

    for layout in ["oldgnu", "gnu", "pax00", "pax01", "pax10"] {
        let archive = fixture("tar", &format!("sparse-{layout}.tar"));
        let dir = common::TempDir::new(&format!("fx-sparse-{layout}"));

        Archive::new(&archive).extract_to(dir.path()).unwrap_or_else(|e| panic!("{layout}: {e}"));

        let back = std::fs::read(dir.join("holes.bin")).unwrap_or_else(|e| panic!("{layout}: missing entry: {e}"));
        assert_eq!(back.len(), expected.len(), "{layout}: wrong length");
        assert!(back == expected, "{layout}: the holes landed in the wrong places");
    }
}

#[test]
fn a_sparse_fixture_is_far_smaller_than_what_it_expands_to() {
    for layout in ["oldgnu", "gnu", "pax00", "pax01", "pax10"] {
        let archive = fixture("tar", &format!("sparse-{layout}.tar"));
        let stored = std::fs::metadata(&archive).unwrap().len();
        assert!(stored < 64 * 1024, "{layout}: the fixture is not sparse ({stored} bytes)");
    }
}

#[test]
fn an_lzip_fixture_decodes() {
    let packed = std::fs::read(fixture("containers", "sample.lz")).expect("read the fixture");
    let expected = plain();

    let whole = ttarchive::codecs::lzip::decompress(&packed, expected.len()).expect("decompress");
    assert!(whole == expected, "the slice path decoded it differently");

    let mut streamed = Vec::new();
    ttarchive::codecs::lzip::Reader::new(packed.as_slice()).read_to_end(&mut streamed).expect("read");
    assert!(streamed == expected, "the streaming path decoded it differently");
}

#[test]
fn a_compress_fixture_decodes() {
    let packed = std::fs::read(fixture("containers", "sample.Z")).expect("read the fixture");
    let expected = plain();

    let whole = ttarchive::codecs::compress::decompress(&packed, expected.len()).expect("decompress");
    assert!(whole == expected, "the slice path decoded it differently");

    let mut streamed = Vec::new();
    ttarchive::codecs::compress::Reader::new(packed.as_slice()).read_to_end(&mut streamed).expect("read");
    assert!(streamed == expected, "the streaming path decoded it differently");
}

#[test]
fn an_lzip_tarball_fixture_extracts() {
    let archive = fixture("containers", "sample.tar.lz");
    let dir = common::TempDir::new("fx-tar-lz");

    Archive::new(&archive).set_type(ArchiveType::TarLz).extract_to(dir.path()).expect("extract");

    let back = std::fs::read(dir.join("holes.bin")).expect("missing entry");
    assert_eq!(back.len(), holes().len(), "wrong length through the lzip wrapper");
}
