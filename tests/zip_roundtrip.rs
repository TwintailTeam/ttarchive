mod common;

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use common::{TempDir, compressible, pseudo_random};
use ttarchive::codecs::Level;
use ttarchive::{Archive, ArchiveType};

fn snapshot(root: &Path) -> BTreeMap<String, Vec<u8>> {
    let mut out = BTreeMap::new();
    let mut stack = vec![root.to_path_buf()];

    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir).expect("read_dir") {
            let entry = entry.expect("dir entry");
            let path = entry.path();
            let rel = path.strip_prefix(root).unwrap().to_string_lossy().replace('\\', "/");
            let md = fs::symlink_metadata(&path).expect("metadata");

            if md.is_dir() {
                out.insert(format!("{rel}/"), Vec::new());
                stack.push(path);
            } else if md.is_symlink() {
                let target = fs::read_link(&path).expect("read_link");
                out.insert(format!("{rel} -> "), target.to_string_lossy().into_owned().into_bytes());
            } else {
                out.insert(rel, fs::read(&path).expect("read file"));
            }
        }
    }
    out
}

#[test]
fn round_trips_a_directory_tree() {
    let src = TempDir::new("src");
    src.write("readme.txt", "hello world");
    src.write("nested/deep/data.bin", pseudo_random(50_000, 7));
    src.write("nested/text.log", compressible(200_000));
    src.write("empty.txt", "");
    fs::create_dir_all(src.join("empty_dir")).unwrap();

    let work = TempDir::new("work");
    let archive = work.join("out.zip");

    let created = Archive::new(&archive).set_type(ArchiveType::Zip).create_from([src.path()]).expect("create");
    assert!(created.files >= 4, "expected at least 4 files, got {created:?}");
    assert!(archive.exists());

    let dest = TempDir::new("dest");
    Archive::new(&archive).extract_to(dest.path()).expect("extract");

    let stem = src.path().file_name().unwrap().to_string_lossy().into_owned();
    let before = snapshot(src.path());
    let after = snapshot(&dest.join(&stem));

    assert_eq!(before, after, "extracted tree differs from the original");
}

#[test]
fn round_trips_at_every_compression_level() {
    for level in [Level::None, Level::Fast, Level::Default, Level::Best] {
        let src = TempDir::new("lvl-src");
        src.write("text.txt", compressible(100_000));
        src.write("noise.bin", pseudo_random(40_000, 3));

        let work = TempDir::new("lvl-work");
        let archive = work.join("a.zip");

        Archive::new(&archive)
            .set_type(ArchiveType::Zip)
            .set_level(level)
            .create_from([src.path()])
            .unwrap_or_else(|e| panic!("{level:?}: create failed: {e}"));

        let dest = TempDir::new("lvl-dest");
        Archive::new(&archive).extract_to(dest.path()).unwrap_or_else(|e| panic!("{level:?}: extract failed: {e}"));

        let stem = src.path().file_name().unwrap().to_string_lossy().into_owned();
        assert_eq!(snapshot(src.path()), snapshot(&dest.join(&stem)), "{level:?}");
    }
}

#[test]
fn stores_incompressible_data_rather_than_growing_it() {
    let src = TempDir::new("inc-src");
    let noise = pseudo_random(500_000, 11);
    src.write("noise.bin", &noise);

    let work = TempDir::new("inc-work");
    let archive = work.join("a.zip");
    let summary = Archive::new(&archive).set_type(ArchiveType::Zip).create_from([src.join("noise.bin")]).unwrap();

    assert!(summary.archive_size < noise.len() as u64 + 4096, "archive {} vs data {}", summary.archive_size, noise.len());
}

#[test]
fn empty_archive_round_trips() {
    let empty = TempDir::new("empty-src");
    let work = TempDir::new("empty-work");
    let archive = work.join("empty.zip");

    Archive::new(&archive).set_type(ArchiveType::Zip).create_from([empty.path()]).unwrap();

    let entries = Archive::new(&archive).entries().unwrap();
    assert!(entries.len() <= 1, "got {entries:?}");

    let dest = TempDir::new("empty-dest");
    Archive::new(&archive).extract_to(dest.path()).unwrap();
}

#[test]
fn zero_byte_file_round_trips() {
    let src = TempDir::new("zero-src");
    src.write("zero.txt", "");

    let work = TempDir::new("zero-work");
    let archive = work.join("a.zip");
    Archive::new(&archive).set_type(ArchiveType::Zip).create_from([src.join("zero.txt")]).unwrap();

    let entries = Archive::new(&archive).entries().unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].size, 0);
    assert_eq!(entries[0].zip().unwrap().crc32, 0);

    let dest = TempDir::new("zero-dest");
    Archive::new(&archive).extract_to(dest.path()).unwrap();
    assert_eq!(fs::read(dest.join("zero.txt")).unwrap(), Vec::<u8>::new());
}

#[test]
fn preserves_non_ascii_names() {
    let src = TempDir::new("utf8-src");
    src.write("Grüße.txt", "latin");
    src.write("日本語.txt", "japanese");
    src.write("emoji-😀.txt", "emoji");

    let work = TempDir::new("utf8-work");
    let archive = work.join("a.zip");
    Archive::new(&archive).set_type(ArchiveType::Zip).create_from([src.path()]).unwrap();

    let names: Vec<String> = Archive::new(&archive).entries().unwrap().into_iter().map(|e| e.name).collect();

    for expected in ["Grüße.txt", "日本語.txt", "emoji-😀.txt"] {
        assert!(names.iter().any(|n| n.ends_with(expected)), "missing {expected} in {names:?}");
    }

    let dest = TempDir::new("utf8-dest");
    Archive::new(&archive).extract_to(dest.path()).unwrap();
    let stem = src.path().file_name().unwrap().to_string_lossy().into_owned();
    assert_eq!(snapshot(src.path()), snapshot(&dest.join(&stem)));
}

#[test]
fn progress_callback_reports_monotonic_totals() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

    let src = TempDir::new("prog-src");
    src.write("a.txt", compressible(400_000));
    src.write("b.bin", pseudo_random(400_000, 5));

    let work = TempDir::new("prog-work");
    let archive = work.join("a.zip");

    let calls = Arc::new(AtomicU64::new(0));
    let high_water = Arc::new(AtomicU64::new(0));
    let regressed = Arc::new(AtomicBool::new(false));

    {
        let (calls, high_water, regressed) = (Arc::clone(&calls), Arc::clone(&high_water), Arc::clone(&regressed));

        Archive::new(&archive)
            .set_type(ArchiveType::Zip)
            .on_progress(move |p| {
                calls.fetch_add(1, Ordering::Relaxed);
                let previous = high_water.fetch_max(p.processed_bytes, Ordering::Relaxed);
                if p.processed_bytes < previous {
                    regressed.store(true, Ordering::Relaxed);
                }
            })
            .create_from([src.path()])
            .unwrap();
    }

    assert!(calls.load(Ordering::Relaxed) > 0, "callback was never invoked");
    assert!(!regressed.load(Ordering::Relaxed), "processed_bytes went backwards");

    let dest = TempDir::new("prog-dest");
    let seen_total = Arc::new(AtomicU64::new(0));
    {
        let seen_total = Arc::clone(&seen_total);
        Archive::new(&archive)
            .on_progress(move |p| {
                seen_total.fetch_max(p.total_bytes, Ordering::Relaxed);
            })
            .extract_to(dest.path())
            .unwrap();
    }

    assert_eq!(seen_total.load(Ordering::Relaxed), 800_000, "total_bytes should be the sum of uncompressed sizes");
}

#[test]
fn sequential_and_parallel_produce_identical_output() {
    let src = TempDir::new("par-src");
    for i in 0..30 {
        src.write(format!("f{i:02}.txt"), compressible(20_000 + i * 37));
    }

    let work = TempDir::new("par-work");

    let seq = work.join("seq.zip");
    Archive::new(&seq).set_type(ArchiveType::Zip).set_threads(Some(1)).create_from([src.path()]).unwrap();

    let par = work.join("par.zip");
    Archive::new(&par).set_type(ArchiveType::Zip).set_threads(Some(8)).create_from([src.path()]).unwrap();

    assert_eq!(fs::read(&seq).unwrap(), fs::read(&par).unwrap());

    let d1 = TempDir::new("par-d1");
    let d2 = TempDir::new("par-d2");
    Archive::new(&seq).set_threads(Some(1)).extract_to(d1.path()).unwrap();
    Archive::new(&par).set_threads(Some(8)).extract_to(d2.path()).unwrap();
    assert_eq!(snapshot(d1.path()), snapshot(d2.path()));
}

#[test]
fn detects_format_without_set_type() {
    let src = TempDir::new("sniff-src");
    src.write("a.txt", "hi");

    let work = TempDir::new("sniff-work");
    let archive = work.join("mystery.dat");
    Archive::new(&archive).set_type(ArchiveType::Zip).create_from([src.join("a.txt")]).unwrap();

    let dest = TempDir::new("sniff-dest");
    Archive::new(&archive).extract_to(dest.path()).expect("should sniff ZIP magic");
    assert_eq!(fs::read(dest.join("a.txt")).unwrap(), b"hi");
}

#[test]
fn extract_defaults_to_directory_named_after_archive() {
    let src = TempDir::new("def-src");
    src.write("a.txt", "content");

    let work = TempDir::new("def-work");
    let archive = work.join("photos.zip");
    Archive::new(&archive).set_type(ArchiveType::Zip).create_from([src.join("a.txt")]).unwrap();

    Archive::new(&archive).extract().unwrap();

    assert_eq!(fs::read(work.join("photos/a.txt")).unwrap(), b"content");
}
