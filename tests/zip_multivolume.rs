mod common;

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use common::{TempDir, compressible, pseudo_random};
use ttarchive::zip::volumes::{MIN_VOLUME_SIZE, Scheme, VolumeSet};
use ttarchive::{Archive, ArchiveType};

fn snapshot(root: &Path) -> BTreeMap<String, Vec<u8>> {
    let mut out = BTreeMap::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            let path = entry.path();
            let rel = path.strip_prefix(root).unwrap().to_string_lossy().replace('\\', "/");
            if path.is_dir() {
                stack.push(path);
            } else {
                out.insert(rel, fs::read(&path).unwrap_or_default());
            }
        }
    }
    out
}

fn build_source(tag: &str) -> TempDir {
    let src = TempDir::new(tag);
    for i in 0..12 {
        if i % 2 == 0 {
            src.write(format!("text/file{i:02}.txt"), compressible(80_000 + i * 111));
        } else {
            src.write(format!("bin/file{i:02}.bin"), pseudo_random(70_000 + i * 97, i as u32 + 1));
        }
    }
    src
}

fn volume_files(archive: &Path) -> Vec<PathBuf> {
    VolumeSet::discover(archive).expect("discover").paths().to_vec()
}

#[test]
fn creates_named_split_volumes() {
    let src = build_source("mv-src");
    let work = TempDir::new("mv-work");
    let archive = work.join("photos.zip");

    let summary = Archive::new(&archive).set_type(ArchiveType::Zip).set_volume_size(MIN_VOLUME_SIZE).create_from([src.path()]).expect("create split");

    assert!(summary.volumes > 1, "expected several volumes, got {}", summary.volumes);

    for n in 1..summary.volumes {
        let seg = work.join(format!("photos.z{n:02}"));
        assert!(seg.exists(), "missing segment {}", seg.display());
    }
    assert!(archive.exists(), "final segment must be named .zip");

    assert!(!work.join(format!("photos.z{:02}", summary.volumes)).exists());
}

#[test]
fn split_volumes_respect_the_size_limit() {
    let src = build_source("mv-size-src");
    let work = TempDir::new("mv-size-work");
    let archive = work.join("a.zip");

    Archive::new(&archive).set_type(ArchiveType::Zip).set_volume_size(MIN_VOLUME_SIZE).create_from([src.path()]).unwrap();

    let files = volume_files(&archive);
    assert!(files.len() > 1);

    for path in &files[..files.len() - 1] {
        let len = fs::metadata(path).unwrap().len();
        assert!(len <= MIN_VOLUME_SIZE, "{} is {len} bytes, over the {MIN_VOLUME_SIZE} limit", path.display());
    }
}

#[test]
fn first_segment_carries_the_spanning_signature() {
    let src = build_source("mv-sig-src");
    let work = TempDir::new("mv-sig-work");
    let archive = work.join("a.zip");

    Archive::new(&archive).set_type(ArchiveType::Zip).set_volume_size(MIN_VOLUME_SIZE).create_from([src.path()]).unwrap();

    let first = fs::read(work.join("a.z01")).unwrap();
    assert_eq!(&first[..4], &[0x50, 0x4b, 0x07, 0x08], "expected PK\\x07\\x08");
}

#[test]
fn single_segment_split_uses_the_temporary_marker() {
    let src = TempDir::new("mv-one-src");
    src.write("small.txt", "not much here");

    let work = TempDir::new("mv-one-work");
    let archive = work.join("a.zip");

    let summary = Archive::new(&archive).set_type(ArchiveType::Zip).set_volume_size(MIN_VOLUME_SIZE).create_from([src.join("small.txt")]).unwrap();

    assert_eq!(summary.volumes, 1);
    assert!(!work.join("a.z01").exists(), "the lone segment should be renamed to .zip");

    let bytes = fs::read(&archive).unwrap();
    assert_eq!(&bytes[..4], &[0x50, 0x4b, 0x30, 0x30], "expected PK00");

    let dest = TempDir::new("mv-one-dest");
    Archive::new(&archive).extract_to(dest.path()).unwrap();
    assert_eq!(fs::read(dest.join("small.txt")).unwrap(), b"not much here");
}

#[test]
fn split_archive_round_trips() {
    let src = build_source("mv-rt-src");
    let work = TempDir::new("mv-rt-work");
    let archive = work.join("a.zip");

    Archive::new(&archive).set_type(ArchiveType::Zip).set_volume_size(MIN_VOLUME_SIZE).create_from([src.path()]).unwrap();

    let dest = TempDir::new("mv-rt-dest");
    Archive::new(&archive).extract_to(dest.path()).expect("extract split");

    let stem = src.path().file_name().unwrap().to_string_lossy().into_owned();
    assert_eq!(snapshot(src.path()), snapshot(&dest.join(&stem)));
}

#[test]
fn extracting_from_the_first_segment_chains_the_set() {
    let src = build_source("mv-chain-src");
    let work = TempDir::new("mv-chain-work");
    let archive = work.join("a.zip");

    Archive::new(&archive).set_type(ArchiveType::Zip).set_volume_size(MIN_VOLUME_SIZE).create_from([src.path()]).unwrap();

    let first = work.join("a.z01");
    assert!(first.exists());

    let dest = TempDir::new("mv-chain-dest");
    Archive::new(&first).set_type(ArchiveType::Zip).extract_to(dest.path()).expect("first segment should chain to the whole set");

    let stem = src.path().file_name().unwrap().to_string_lossy().into_owned();
    assert_eq!(snapshot(src.path()), snapshot(&dest.join(&stem)));
}

#[test]
fn extracting_from_a_middle_segment_chains_the_set() {
    let src = build_source("mv-mid-src");
    let work = TempDir::new("mv-mid-work");
    let archive = work.join("a.zip");

    let summary = Archive::new(&archive).set_type(ArchiveType::Zip).set_volume_size(MIN_VOLUME_SIZE).create_from([src.path()]).unwrap();
    assert!(summary.volumes >= 3, "need at least 3 volumes for this test");

    let middle = work.join("a.z02");
    let dest = TempDir::new("mv-mid-dest");
    Archive::new(&middle).set_type(ArchiveType::Zip).extract_to(dest.path()).unwrap();

    let stem = src.path().file_name().unwrap().to_string_lossy().into_owned();
    assert_eq!(snapshot(src.path()), snapshot(&dest.join(&stem)));
}

#[test]
fn listing_a_split_archive_reports_every_entry() {
    let src = build_source("mv-list-src");
    let work = TempDir::new("mv-list-work");
    let archive = work.join("a.zip");

    Archive::new(&archive).set_type(ArchiveType::Zip).set_volume_size(MIN_VOLUME_SIZE).create_from([src.path()]).unwrap();

    let entries = Archive::new(&archive).entries().expect("list split archive");
    let files = entries.iter().filter(|e| e.is_file()).count();
    assert_eq!(files, 12, "expected all 12 files listed");

    let disks: std::collections::BTreeSet<u32> = entries.iter().map(|e| e.zip().unwrap().disk_start).collect();
    assert!(disks.len() > 1, "entries should span segments, saw disks {disks:?}");
}

#[test]
fn entry_data_spanning_a_boundary_round_trips() {
    let src = TempDir::new("mv-span-src");
    let big = pseudo_random(400_000, 42);
    src.write("big.bin", &big);

    let work = TempDir::new("mv-span-work");
    let archive = work.join("a.zip");

    let summary = Archive::new(&archive).set_type(ArchiveType::Zip).set_volume_size(MIN_VOLUME_SIZE).create_from([src.join("big.bin")]).unwrap();
    assert!(summary.volumes > 1, "one file should have forced several segments");

    let dest = TempDir::new("mv-span-dest");
    Archive::new(&archive).extract_to(dest.path()).unwrap();
    assert_eq!(fs::read(dest.join("big.bin")).unwrap(), big);
}

fn raw_split(path: &Path, piece_size: usize) -> usize {
    let data = fs::read(path).expect("read archive");
    let mut count = 0;
    for (i, chunk) in data.chunks(piece_size).enumerate() {
        let name = format!("{}.{:03}", path.file_name().unwrap().to_string_lossy(), i + 1);
        fs::write(path.with_file_name(name), chunk).expect("write piece");
        count = i + 1;
    }
    fs::remove_file(path).expect("remove original");
    count
}

#[test]
fn raw_split_pieces_extract_when_given_the_first() {
    let src = build_source("raw-src");
    let work = TempDir::new("raw-work");
    let archive = work.join("a.zip");

    Archive::new(&archive).set_type(ArchiveType::Zip).create_from([src.path()]).unwrap();

    let pieces = raw_split(&archive, 100_000);
    assert!(pieces > 1, "expected several pieces, got {pieces}");
    assert!(!archive.exists(), "original must be gone for this to be a real test");

    let first = work.join("a.zip.001");
    let dest = TempDir::new("raw-dest");
    Archive::new(&first).set_type(ArchiveType::Zip).extract_to(dest.path()).expect("raw split should chain from .001");

    let stem = src.path().file_name().unwrap().to_string_lossy().into_owned();
    assert_eq!(snapshot(src.path()), snapshot(&dest.join(&stem)));
}

#[test]
fn raw_split_is_detected_as_one_logical_disk() {
    let src = build_source("raw-scheme-src");
    let work = TempDir::new("raw-scheme-work");
    let archive = work.join("a.zip");

    Archive::new(&archive).set_type(ArchiveType::Zip).create_from([src.path()]).unwrap();
    raw_split(&archive, 100_000);

    let set = VolumeSet::discover(&work.join("a.zip.001")).unwrap();
    assert_eq!(set.layout().scheme(), Scheme::RawSplit);
    assert!(set.is_multi_volume());
}

#[test]
fn single_file_archive_is_not_multi_volume() {
    let src = TempDir::new("single-src");
    src.write("a.txt", "hi");
    let work = TempDir::new("single-work");
    let archive = work.join("a.zip");

    Archive::new(&archive).set_type(ArchiveType::Zip).create_from([src.join("a.txt")]).unwrap();

    let set = VolumeSet::discover(&archive).unwrap();
    assert_eq!(set.layout().scheme(), Scheme::Single);
    assert!(!set.is_multi_volume());
    assert_eq!(set.paths().len(), 1);
}

#[test]
fn missing_segment_is_reported_clearly() {
    let src = build_source("miss-src");
    let work = TempDir::new("miss-work");
    let archive = work.join("a.zip");

    let summary = Archive::new(&archive).set_type(ArchiveType::Zip).set_volume_size(MIN_VOLUME_SIZE).create_from([src.path()]).unwrap();
    assert!(summary.volumes >= 3);

    fs::remove_file(work.join("a.z02")).unwrap();

    let dest = TempDir::new("miss-dest");
    let err = Archive::new(&archive).extract_to(dest.path()).unwrap_err();

    let message = err.to_string();
    assert!(message.contains("segment") || message.contains("disk") || message.contains("volume"), "error should mention the missing segment, got: {message}");
}

#[test]
fn missing_final_segment_is_reported_clearly() {
    let src = build_source("missfinal-src");
    let work = TempDir::new("missfinal-work");
    let archive = work.join("a.zip");

    Archive::new(&archive).set_type(ArchiveType::Zip).set_volume_size(MIN_VOLUME_SIZE).create_from([src.path()]).unwrap();

    fs::remove_file(&archive).unwrap();

    let err = VolumeSet::discover(&work.join("a.z01")).unwrap_err();
    let message = err.to_string();
    assert!(message.contains("final segment") || message.contains("missing"), "got: {message}");
}

#[test]
fn volume_size_below_the_minimum_is_clamped() {
    let src = build_source("clamp-src");
    let work = TempDir::new("clamp-work");
    let archive = work.join("a.zip");

    let summary = Archive::new(&archive).set_type(ArchiveType::Zip).set_volume_size(1024).create_from([src.path()]).unwrap();

    let files = volume_files(&archive);
    for path in &files[..files.len() - 1] {
        assert!(fs::metadata(path).unwrap().len() <= MIN_VOLUME_SIZE);
    }
    assert!(summary.volumes < 100, "clamping should keep the count sane");
}

#[test]
fn split_and_single_extract_to_identical_trees() {
    let src = build_source("cmp-src");
    let work = TempDir::new("cmp-work");

    let single = work.join("single.zip");
    Archive::new(&single).set_type(ArchiveType::Zip).create_from([src.path()]).unwrap();

    let split = work.join("split.zip");
    Archive::new(&split).set_type(ArchiveType::Zip).set_volume_size(MIN_VOLUME_SIZE).create_from([src.path()]).unwrap();

    let d1 = TempDir::new("cmp-d1");
    let d2 = TempDir::new("cmp-d2");
    Archive::new(&single).extract_to(d1.path()).unwrap();
    Archive::new(&split).extract_to(d2.path()).unwrap();

    assert_eq!(snapshot(d1.path()), snapshot(d2.path()));
}
