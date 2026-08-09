#![cfg(unix)]

mod common;

use std::os::unix::fs::MetadataExt;
use std::path::Path;
use std::process::Command;

use ttarchive::{Archive, ArchiveType, Overwrite};

fn have(tool: &str) -> bool {
    Command::new("which").arg(tool).output().is_ok_and(|o| o.status.success())
}

fn linked_tree(dir: &common::TempDir) -> std::path::PathBuf {
    let first = dir.write("src/a-first.txt", b"shared body, stored exactly once");
    std::fs::hard_link(&first, dir.join("src/b-second.txt")).unwrap();
    std::fs::hard_link(&first, dir.join("src/c-third.txt")).unwrap();
    dir.write("src/lonely.txt", b"only one name points here");
    dir.join("src")
}

fn link_counts(root: &Path) -> std::collections::BTreeMap<String, u64> {
    let mut out = std::collections::BTreeMap::new();
    let mut stack = vec![root.to_path_buf()];

    while let Some(at) = stack.pop() {
        for entry in std::fs::read_dir(&at).unwrap().flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            out.insert(name, std::fs::metadata(&path).unwrap().nlink());
        }
    }

    out
}

fn inodes(root: &Path) -> std::collections::BTreeMap<String, u64> {
    let mut out = std::collections::BTreeMap::new();
    let mut stack = vec![root.to_path_buf()];

    while let Some(at) = stack.pop() {
        for entry in std::fs::read_dir(&at).unwrap().flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            out.insert(name, std::fs::metadata(&path).unwrap().ino());
        }
    }

    out
}

#[test]
fn hard_linked_files_are_stored_once_and_restored_as_links() {
    let dir = common::TempDir::new("hardlink-roundtrip");
    let source = linked_tree(&dir);

    let archive = dir.join("linked.tar");
    let made = Archive::new(&archive).create_from([&source]).unwrap();

    assert_eq!(made.hardlinks, 2, "two of the three names should have been stored as links");
    assert_eq!(made.files, 2, "only the first name and the unrelated file hold data");

    let out = dir.join("out");
    let got = Archive::new(&archive).extract_to(&out).unwrap();
    assert_eq!(got.hardlinks, 2, "both links should have been recreated");

    let counts = link_counts(&out.join("src"));
    assert_eq!(counts.get("a-first.txt"), Some(&3), "the linked file should carry three names, got {counts:?}");
    assert_eq!(counts.get("b-second.txt"), Some(&3));
    assert_eq!(counts.get("c-third.txt"), Some(&3));
    assert_eq!(counts.get("lonely.txt"), Some(&1), "an unlinked file must not be joined to anything");

    let ids = inodes(&out.join("src"));
    assert_eq!(ids["a-first.txt"], ids["b-second.txt"], "the links should share one inode");
    assert_eq!(ids["a-first.txt"], ids["c-third.txt"]);
    assert_ne!(ids["a-first.txt"], ids["lonely.txt"]);
}

#[test]
fn a_hard_link_stores_the_body_only_once() {
    let dir = common::TempDir::new("hardlink-size");
    let body = common::compressible(200_000);

    let first = dir.write("src/one.bin", &body);
    std::fs::hard_link(&first, dir.join("src/two.bin")).unwrap();

    let archive = dir.join("linked.tar");
    Archive::new(&archive).create_from([dir.join("src")]).unwrap();

    let size = std::fs::metadata(&archive).unwrap().len();
    assert!(size < body.len() as u64 * 3 / 2, "the archive is {size} bytes, so the body was stored twice");
}

#[test]
fn hard_links_survive_stripping_the_root() {
    let dir = common::TempDir::new("hardlink-strip");
    let source = linked_tree(&dir);

    let archive = dir.join("linked.tar");
    Archive::new(&archive).create_from([&source]).unwrap();

    let out = dir.join("stripped");
    let got = Archive::new(&archive).set_strip_root(true).extract_to(&out).unwrap();

    assert_eq!(got.hardlinks, 2, "the links should still be found after stripping");
    let ids = inodes(&out);
    assert_eq!(ids["a-first.txt"], ids["b-second.txt"], "stripping should not have broken the link");
}

#[test]
fn hard_links_round_trip_through_every_writable_tarball() {
    const WRITABLE: [(&str, ArchiveType); 6] = [
        ("l.tar", ArchiveType::Tar),
        ("l.tar.gz", ArchiveType::TarGz),
        ("l.tar.bz2", ArchiveType::TarBz2),
        ("l.tar.lzma", ArchiveType::TarLzma),
        ("l.tar.xz", ArchiveType::TarXz),
        ("l.tar.zst", ArchiveType::TarZst),
    ];

    for (name, kind) in WRITABLE {
        let dir = common::TempDir::new("hardlink-wrappers");
        let source = linked_tree(&dir);

        let archive = dir.join(name);
        Archive::new(&archive).set_type(kind).create_from([&source]).unwrap_or_else(|e| panic!("{name}: {e}"));

        let out = dir.join("out");
        let got = Archive::new(&archive).set_type(kind).extract_to(&out).unwrap_or_else(|e| panic!("{name}: {e}"));

        assert_eq!(got.hardlinks, 2, "{name}: expected two links");
        let ids = inodes(&out.join("src"));
        assert_eq!(ids["a-first.txt"], ids["b-second.txt"], "{name}: the links do not share an inode");
    }
}

#[test]
fn a_hard_link_is_skipped_when_its_target_is_not_extracted() {
    let dir = common::TempDir::new("hardlink-orphan");
    let source = linked_tree(&dir);

    let archive = dir.join("linked.tar");
    Archive::new(&archive).create_from([&source]).unwrap();

    let out = dir.join("out");
    std::fs::create_dir_all(out.join("src")).unwrap();
    std::fs::write(out.join("src/a-first.txt"), b"already here").unwrap();

    let got = Archive::new(&archive).set_overwrite(Overwrite::Never).extract_to(&out).unwrap();
    assert!(got.skipped > 0, "the pre-existing target should have been skipped");
    assert_eq!(std::fs::read(out.join("src/a-first.txt")).unwrap(), b"already here", "the existing file was overwritten");
}

#[test]
fn a_fifo_is_skipped_rather_than_written_as_an_empty_file() {
    if !have("tar") || !have("mkfifo") {
        eprintln!("skipping: tar or mkfifo not installed");
        return;
    }

    let dir = common::TempDir::new("fifo");
    dir.write("src/real.txt", b"an ordinary file");

    let made = Command::new("mkfifo").arg(dir.join("src/pipe")).status().unwrap();
    assert!(made.success(), "mkfifo failed");

    let archive = dir.join("withfifo.tar");
    let packed = Command::new("tar").arg("-cf").arg(&archive).arg("-C").arg(dir.path()).arg("src").status().unwrap();
    assert!(packed.success(), "tar failed to archive the fifo");

    let out = dir.join("out");
    let got = Archive::new(&archive).extract_to(&out).unwrap();

    assert_eq!(got.files, 1, "only the ordinary file should have been written");
    assert_eq!(got.specials, 1, "the fifo should have been counted as a special file");
    assert!(!out.join("src/pipe").exists(), "a fifo must not be replaced by an empty regular file");
    assert_eq!(std::fs::read(out.join("src/real.txt")).unwrap(), b"an ordinary file");
}

#[test]
fn gnu_tar_reads_the_hard_links_we_write() {
    if !have("tar") {
        eprintln!("skipping: tar not installed");
        return;
    }

    let dir = common::TempDir::new("hardlink-gnu-read");
    let source = linked_tree(&dir);

    let archive = dir.join("ours.tar");
    Archive::new(&archive).create_from([&source]).unwrap();

    let listed = Command::new("tar").arg("-tvf").arg(&archive).output().unwrap();
    assert!(listed.status.success(), "tar rejected our archive: {}", String::from_utf8_lossy(&listed.stderr));

    let text = String::from_utf8_lossy(&listed.stdout);
    let links = text.lines().filter(|line| line.contains("link to")).count();
    assert_eq!(links, 2, "tar should report two hard links, listing was:\n{text}");

    let out = dir.join("gnu");
    std::fs::create_dir_all(&out).unwrap();
    let done = Command::new("tar").arg("-xf").arg(&archive).arg("-C").arg(&out).status().unwrap();
    assert!(done.success(), "tar failed to extract our archive");

    let ids = inodes(&out.join("src"));
    assert_eq!(ids["a-first.txt"], ids["b-second.txt"], "tar did not restore our links as links");
}

#[test]
fn we_read_the_hard_links_gnu_tar_writes() {
    if !have("tar") {
        eprintln!("skipping: tar not installed");
        return;
    }

    let dir = common::TempDir::new("hardlink-gnu-write");
    linked_tree(&dir);

    let archive = dir.join("theirs.tar");
    let packed = Command::new("tar").arg("-cf").arg(&archive).arg("-C").arg(dir.path()).arg("src").status().unwrap();
    assert!(packed.success(), "tar failed to create the archive");

    let out = dir.join("out");
    let got = Archive::new(&archive).extract_to(&out).unwrap();
    assert_eq!(got.hardlinks, 2, "we should have found two hard links in tar's archive");

    let counts = link_counts(&out.join("src"));
    assert_eq!(counts.get("a-first.txt"), Some(&3), "all three names should share one file, got {counts:?}");
}

#[test]
fn listing_reports_a_hard_link_with_its_target() {
    let dir = common::TempDir::new("hardlink-listing");
    let source = linked_tree(&dir);

    let archive = dir.join("linked.tar");
    Archive::new(&archive).create_from([&source]).unwrap();

    let entries = Archive::new(&archive).entries().unwrap();
    let links: Vec<_> = entries.iter().filter_map(|e| e.tar()).filter(|d| d.typeflag == b'1').collect();

    assert_eq!(links.len(), 2, "listing should show two hard link entries");
    for link in links {
        assert_eq!(link.linkname, "src/a-first.txt", "a hard link should name the entry holding the data");
    }
}

#[test]
fn creating_an_archive_counts_the_special_files_it_could_not_store() {
    if !have("mkfifo") {
        eprintln!("skipping: mkfifo not installed");
        return;
    }

    let dir = common::TempDir::new("specials-create");
    dir.write("src/plain.txt", b"an ordinary file");
    dir.write("src/nested/also.txt", b"another");
    assert!(Command::new("mkfifo").arg(dir.join("src/pipe")).status().unwrap().success());
    assert!(Command::new("mkfifo").arg(dir.join("src/nested/pipe")).status().unwrap().success());

    for (name, kind) in [("s.tar", ArchiveType::Tar), ("s.tar.gz", ArchiveType::TarGz), ("s.zip", ArchiveType::Zip)] {
        let made = Archive::new(dir.join(name)).set_type(kind).create_from([dir.join("src")]).unwrap_or_else(|e| panic!("{name}: {e}"));

        assert_eq!(made.specials, 2, "{name}: both fifos should have been counted");
        assert_eq!(made.files, 2, "{name}: only the ordinary files should have been stored");
    }
}

#[test]
fn an_archive_without_special_files_reports_none() {
    let dir = common::TempDir::new("specials-none");
    dir.write("src/one.txt", b"just files");
    dir.write("src/two.txt", b"and more files");

    let made = Archive::new(dir.join("clean.tar")).create_from([dir.join("src")]).unwrap();
    assert_eq!(made.specials, 0, "nothing here is a special file");

    let got = Archive::new(dir.join("clean.tar")).extract_to(dir.join("out")).unwrap();
    assert_eq!(got.specials, 0);
}
