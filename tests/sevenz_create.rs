mod common;

use std::path::{Path, PathBuf};
use std::process::Command;

use common::{TempDir, compressible, have, pseudo_random};
use ttarchive::{Archive, ArchiveType, Level, Method};

fn tree(dir: &TempDir) -> PathBuf {
    dir.write("src/readme.txt", b"hello world");
    dir.write("src/prose.txt", compressible(40_000));
    dir.write("src/nested/noise.bin", pseudo_random(30_000, 4));
    dir.write("src/nested/empty.txt", b"");
    dir.join("src")
}

fn run_7z(dir: &Path, args: &[&str]) -> (bool, String) {
    let output = Command::new(common::resolve("7z")).args(args).current_dir(dir).output().unwrap_or_else(|e| panic!("failed to run 7z: {e}"));
    let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&output.stderr));
    (output.status.success(), text)
}

fn check(out: &Path) {
    assert_eq!(std::fs::read(out.join("src/readme.txt")).unwrap(), b"hello world");
    assert_eq!(std::fs::read(out.join("src/prose.txt")).unwrap(), compressible(40_000));
    assert_eq!(std::fs::read(out.join("src/nested/noise.bin")).unwrap(), pseudo_random(30_000, 4));
    assert_eq!(std::fs::read(out.join("src/nested/empty.txt")).unwrap(), b"");
}

#[test]
fn a_7z_is_created_and_extracted_through_the_public_api() {
    let dir = TempDir::new("sz-api-round");
    let source = tree(&dir);

    let archive = dir.join("made.7z");
    let made = Archive::new(&archive).create_from([&source]).expect("create");

    assert_eq!(made.files, 4);
    assert_eq!(made.directories, 2);
    assert_eq!(made.bytes, 70_011);
    assert_eq!(made.volumes, 1);
    assert_eq!(made.archive_size, std::fs::metadata(&archive).unwrap().len());

    let out = dir.join("out");
    let got = Archive::new(&archive).extract_to(&out).expect("extract");
    assert_eq!(got.files, 4);
    assert_eq!(got.directories, 2);
    check(&out);
}

#[test]
fn the_extension_alone_decides_the_format() {
    let dir = TempDir::new("sz-api-ext");
    let source = tree(&dir);

    let archive = dir.join("bare.7z");
    Archive::new(&archive).create_from([&source]).unwrap();

    let head = std::fs::read(&archive).unwrap();
    assert_eq!(&head[..6], &[b'7', b'z', 0xBC, 0xAF, 0x27, 0x1C]);
    assert_eq!(ArchiveType::from_extension(&archive), Some(ArchiveType::SevenZ));
    assert_eq!(ArchiveType::from_magic(&head[..512.min(head.len())]), Some(ArchiveType::SevenZ));
    assert!(ArchiveType::SevenZ.can_write());
}

#[test]
fn entries_lists_what_was_stored() {
    let dir = TempDir::new("sz-api-entries");
    let source = tree(&dir);

    let archive = dir.join("listed.7z");
    Archive::new(&archive).create_from([&source]).unwrap();

    let names: Vec<String> = Archive::new(&archive).entries().unwrap().into_iter().map(|e| e.name).collect();
    for wanted in ["src", "src/readme.txt", "src/prose.txt", "src/nested", "src/nested/noise.bin", "src/nested/empty.txt"] {
        assert!(names.contains(&wanted.to_string()), "{wanted} missing from {names:?}");
    }
}

#[test]
fn every_writable_method_becomes_a_real_7z_coder() {
    let dir = TempDir::new("sz-api-methods");
    let source = tree(&dir);

    for method in [Method::Store, Method::Deflate, Method::Bzip2] {
        let archive = dir.join(format!("m{}.7z", method.code()));
        Archive::new(&archive).set_method(method).create_from([&source]).unwrap_or_else(|e| panic!("{method:?}: {e}"));

        let out = dir.join(format!("out{}", method.code()));
        Archive::new(&archive).extract_to(&out).unwrap_or_else(|e| panic!("{method:?}: {e}"));
        check(&out);
    }
}

#[test]
fn a_decode_only_method_is_refused_for_7z_too() {
    let dir = TempDir::new("sz-api-decode-only");
    let source = tree(&dir);

    for method in [Method::Lzma, Method::Xz, Method::Zstd, Method::Ppmd] {
        let archive = dir.join("x.7z");
        let err = Archive::new(&archive).set_method(method).create_from([&source]).err().unwrap_or_else(|| panic!("{method:?} should be refused"));
        assert!(err.is_unsupported(), "{method:?} gave {err}");
    }
}

#[test]
fn level_none_stores_and_best_compresses() {
    let dir = TempDir::new("sz-api-levels");
    dir.write("src/text.txt", compressible(200_000));
    let source = dir.join("src");

    let stored = dir.join("stored.7z");
    Archive::new(&stored).set_level(Level::None).create_from([&source]).unwrap();

    let best = dir.join("best.7z");
    Archive::new(&best).set_level(Level::Best).create_from([&source]).unwrap();

    let stored_len = std::fs::metadata(&stored).unwrap().len();
    let best_len = std::fs::metadata(&best).unwrap().len();
    assert!(stored_len > 200_000, "Level::None should store the bytes, not compress them: {stored_len}");
    assert!(best_len * 4 < stored_len, "Level::Best {best_len} should be far smaller than Level::None {stored_len}");

    for archive in [&stored, &best] {
        let out = dir.join(format!("out{}", std::fs::metadata(archive).unwrap().len()));
        Archive::new(archive).extract_to(&out).unwrap();
        assert_eq!(std::fs::read(out.join("src/text.txt")).unwrap(), compressible(200_000));
    }
}

#[test]
fn a_password_encrypts_the_names_as_well_as_the_data() {
    let dir = TempDir::new("sz-api-aes");
    let source = tree(&dir);

    let archive = dir.join("secret.7z");
    Archive::new(&archive).set_password("correct horse").create_from([&source]).unwrap();

    assert!(Archive::new(&archive).entries().is_err(), "the header must not be readable without the password");
    assert!(Archive::new(&archive).extract_to(dir.join("nope")).is_err());

    let out = dir.join("out");
    Archive::new(&archive).set_password("correct horse").extract_to(&out).expect("extract with the password");
    check(&out);
}

#[test]
fn zipcrypto_is_refused_for_7z() {
    let dir = TempDir::new("sz-api-zipcrypto");
    let source = tree(&dir);

    let err = Archive::new(dir.join("bad.7z"))
        .set_password("pw")
        .set_encryption(ttarchive::EncryptionMethod::ZipCrypto)
        .create_from([&source])
        .err()
        .expect("7z has no ZipCrypto");
    assert!(err.is_unsupported(), "{err}");
}

#[test]
fn sparse_is_refused_with_a_reason_and_volumes_are_written() {
    let dir = TempDir::new("sz-api-refusals");
    let source = tree(&dir);

    let sparse = Archive::new(dir.join("s.7z")).set_sparse(true).create_from([&source]).err().expect("7z has no sparse entry");
    assert!(sparse.is_unsupported(), "{sparse}");

    let split = Archive::new(dir.join("v.7z")).set_volume_size(64 * 1024).create_from([&source]).expect("7z writes volumes");
    assert_eq!(split.volumes, 1, "this sample fits in one volume");
    assert!(dir.join("v.7z.001").exists());
}

#[cfg(unix)]
#[test]
fn a_symbolic_link_survives_the_round_trip() {
    let dir = TempDir::new("sz-api-link");
    dir.write("src/target.txt", b"pointed at");
    std::os::unix::fs::symlink("target.txt", dir.join("src/link.txt")).unwrap();

    let archive = dir.join("links.7z");
    let made = Archive::new(&archive).create_from([dir.join("src")]).unwrap();
    assert_eq!(made.symlinks, 1);

    let out = dir.join("out");
    let got = Archive::new(&archive).extract_to(&out).unwrap();
    assert_eq!(got.symlinks, 1);

    let link = out.join("src/link.txt");
    assert!(std::fs::symlink_metadata(&link).unwrap().file_type().is_symlink());
    assert_eq!(std::fs::read_link(&link).unwrap(), Path::new("target.txt"));
    assert_eq!(std::fs::read(&link).unwrap(), b"pointed at");
}

#[cfg(unix)]
#[test]
fn permissions_come_back_as_they_went_in() {
    use std::os::unix::fs::PermissionsExt;

    let dir = TempDir::new("sz-api-modes");
    let script = dir.write("src/run.sh", b"#!/bin/sh\necho hi\n");
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    dir.write("src/plain.txt", b"nothing special");

    let archive = dir.join("modes.7z");
    Archive::new(&archive).create_from([dir.join("src")]).unwrap();

    let out = dir.join("out");
    Archive::new(&archive).extract_to(&out).unwrap();

    assert_eq!(std::fs::metadata(out.join("src/run.sh")).unwrap().permissions().mode() & 0o777, 0o755);
    assert_eq!(std::fs::metadata(out.join("src/plain.txt")).unwrap().permissions().mode() & 0o777, 0o644);
}

#[test]
fn the_cli_lists_directories_as_directories() {
    if !have("7z") {
        eprintln!("skipping: 7z is not installed");
        return;
    }

    let dir = TempDir::new("sz-api-cli-list");
    let source = tree(&dir);
    Archive::new(dir.join("listed.7z")).create_from([&source]).unwrap();

    let (ok, text) = run_7z(dir.path(), &["l", "-slt", "listed.7z"]);
    assert!(ok, "{text}");

    let listed: Vec<String> = text.lines().filter_map(|l| l.strip_prefix("Path = ")).map(|p| p.trim().replace('\\', "/")).skip(1).collect();
    assert!(listed.contains(&"src".to_string()), "src missing from {listed:?}");
    assert!(listed.contains(&"src/nested".to_string()), "src/nested missing from {listed:?}");
    assert!(!listed.iter().any(|p| p.ends_with('/')), "7z entry names carry no trailing separator: {listed:?}");

    let blocks: Vec<&str> = text.split("Path = ").collect();
    let nested = blocks.iter().find(|b| b.starts_with("src/nested\n") || b.starts_with("src\\nested\n")).expect("a block for src/nested");
    let attributes = nested.lines().find_map(|l| l.strip_prefix("Attributes = ")).expect("an attribute line");
    assert!(attributes.starts_with('D'), "src/nested should list as a directory, not {attributes}");
}

#[test]
fn the_cli_accepts_what_the_pipeline_writes() {
    if !have("7z") {
        eprintln!("skipping: 7z is not installed");
        return;
    }

    let dir = TempDir::new("sz-api-cli");
    let source = tree(&dir);

    for (name, level, method) in [
        ("lzma2.7z", Level::Default, None),
        ("store.7z", Level::None, None),
        ("deflate.7z", Level::Default, Some(Method::Deflate)),
        ("bzip2.7z", Level::Default, Some(Method::Bzip2)),
    ] {
        let mut archive = Archive::new(dir.join(name)).set_level(level);
        if let Some(method) = method {
            archive = archive.set_method(method);
        }
        archive.create_from([&source]).unwrap_or_else(|e| panic!("{name}: {e}"));

        let (ok, text) = run_7z(dir.path(), &["t", name]);
        assert!(ok, "7z t rejected {name}:\n{text}");

        let out = format!("cli-{name}");
        let (ok, text) = run_7z(dir.path(), &["x", "-y", &format!("-o{out}"), name]);
        assert!(ok, "7z x failed for {name}:\n{text}");
        check(&dir.join(out));
    }
}
