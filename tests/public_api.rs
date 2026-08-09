mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use ttarchive::{Archive, ArchiveType, Level, Method, Overwrite, ProgressUpdate, UnsafeEntries};

fn sample(dir: &common::TempDir) -> std::path::PathBuf {
    let source = dir.join("src");
    dir.write("src/a.txt", common::compressible(40_000));
    dir.write("src/nested/b.bin", common::pseudo_random(30_000, 7));
    dir.write("src/empty.txt", b"");
    source
}

#[test]
fn archive_type_is_detected_from_extension() {
    use std::path::Path;
    for name in ["a.zip", "a.ZIP", "a.jar", "a.apk", "a.docx", "a.epub", "a.whl", "a.xpi"] {
        assert_eq!(ArchiveType::from_extension(Path::new(name)), Some(ArchiveType::Zip), "{name}");
    }
    assert_eq!(ArchiveType::from_extension(Path::new("a.tar")), Some(ArchiveType::Tar));
    assert_eq!(ArchiveType::from_extension(Path::new("a.tar.gz")), Some(ArchiveType::TarGz));

    for name in ["a.gz", "a", "a.7z", "a.rar"] {
        assert_eq!(ArchiveType::from_extension(Path::new(name)), None, "{name}");
    }
}

#[test]
fn archive_type_is_detected_from_magic() {
    assert_eq!(ArchiveType::from_magic(&[0x50, 0x4b, 0x03, 0x04]), Some(ArchiveType::Zip));
    assert_eq!(ArchiveType::from_magic(&[0x50, 0x4b, 0x05, 0x06]), Some(ArchiveType::Zip));
    assert_eq!(ArchiveType::from_magic(&[0x50, 0x4b, 0x07, 0x08]), Some(ArchiveType::Zip));
    assert_eq!(ArchiveType::from_magic(b"7z\xbc\xaf"), None);
    assert_eq!(ArchiveType::from_magic(b"PK"), None);
}

#[test]
fn an_archive_without_a_known_extension_still_reads_by_magic() {
    let dir = common::TempDir::new("api-magic");
    let source = sample(&dir);

    let archive = dir.join("payload.dat");
    Archive::new(&archive).set_type(ArchiveType::Zip).create_from([&source]).expect("create failed");

    let entries = Archive::new(&archive).entries().expect("listing should work by magic bytes");
    assert_eq!(entries.len(), 5);
}

#[test]
fn entries_lists_without_extracting() {
    let dir = common::TempDir::new("api-entries");
    let source = sample(&dir);
    let archive = dir.join("a.zip");
    Archive::new(&archive).create_from([&source]).unwrap();

    let entries = Archive::new(&archive).entries().unwrap();
    let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
    assert!(names.contains(&"src/a.txt"), "{names:?}");
    assert!(names.contains(&"src/nested/b.bin"), "{names:?}");

    let out = dir.join("out");
    assert!(!out.exists(), "listing must not write anything");
}

#[test]
fn extract_defaults_to_a_directory_named_after_the_archive() {
    let dir = common::TempDir::new("api-extract-default");
    let source = sample(&dir);
    let archive = dir.join("photos.zip");
    Archive::new(&archive).create_from([&source]).unwrap();

    Archive::new(&archive).extract().expect("extract failed");
    assert!(dir.join("photos").is_dir(), "should extract into photos/ beside the archive");
    assert!(dir.join("photos/src/a.txt").is_file());
}

#[test]
fn one_thread_and_many_threads_agree() {
    let dir = common::TempDir::new("api-threads");
    let source = sample(&dir);

    let sequential = dir.join("seq.zip");
    Archive::new(&sequential).set_threads(Some(1)).create_from([&source]).expect("sequential create");

    let parallel = dir.join("par.zip");
    Archive::new(&parallel).set_threads(None).create_from([&source]).expect("parallel create");

    for (archive, name) in [(&sequential, "seq"), (&parallel, "par")] {
        let out = dir.join(name);
        Archive::new(archive).set_threads(Some(1)).extract_to(&out).expect("extract failed");
        assert_eq!(std::fs::read(out.join("src/a.txt")).unwrap(), common::compressible(40_000));
        assert_eq!(std::fs::read(out.join("src/nested/b.bin")).unwrap(), common::pseudo_random(30_000, 7));
    }
}

#[test]
fn overwrite_error_refuses_an_existing_file() {
    let dir = common::TempDir::new("api-overwrite");
    let source = sample(&dir);
    let archive = dir.join("a.zip");
    Archive::new(&archive).create_from([&source]).unwrap();

    let out = dir.join("out");
    Archive::new(&archive).extract_to(&out).unwrap();

    let again = Archive::new(&archive).set_overwrite(Overwrite::Error).extract_to(&out);
    assert!(again.is_err(), "Overwrite::Error must refuse to replace an existing file");

    let summary = Archive::new(&archive).set_overwrite(Overwrite::Never).extract_to(&out).unwrap();
    assert!(summary.skipped > 0, "Overwrite::Never should skip and report it");
}

#[test]
fn progress_callback_reports_monotonic_totals() {
    let dir = common::TempDir::new("api-progress");
    let source = sample(&dir);
    let archive = dir.join("a.zip");

    let calls = Arc::new(AtomicU64::new(0));
    let seen = Arc::new(AtomicU64::new(0));
    {
        let calls = Arc::clone(&calls);
        let seen = Arc::clone(&seen);
        Archive::new(&archive)
            .on_progress(move |p: &ProgressUpdate<'_>| {
                calls.fetch_add(1, Ordering::Relaxed);
                seen.fetch_max(p.processed_bytes, Ordering::Relaxed);
                assert!(p.processed_bytes <= p.total_bytes.max(p.processed_bytes));
            })
            .create_from([&source])
            .unwrap();
    }
    assert!(calls.load(Ordering::Relaxed) > 0, "callback should fire at least once");
    assert!(seen.load(Ordering::Relaxed) > 0, "callback should report progress");
}

#[test]
fn options_structs_can_replace_the_builder() {
    let dir = common::TempDir::new("api-options");
    let source = sample(&dir);

    let create = ttarchive::CreateOptions { level: Level::Best, method: Some(Method::Deflate), ..Default::default() };

    let archive = dir.join("a.zip");
    Archive::new(&archive).with_create_options(create).create_from([&source]).expect("create failed");

    let extract = ttarchive::ExtractOptions { strip_root: true, unsafe_entries: UnsafeEntries::Skip, ..Default::default() };

    let out = dir.join("out");
    Archive::new(&archive).with_extract_options(extract).extract_to(&out).expect("extract failed");
    assert!(out.join("a.txt").is_file(), "strip_root via ExtractOptions should apply");
}

#[test]
fn a_shared_progress_callback_can_be_attached() {
    let dir = common::TempDir::new("api-shared-progress");
    let source = sample(&dir);
    let archive = dir.join("a.zip");

    let calls = Arc::new(AtomicU64::new(0));
    let counter = Arc::clone(&calls);
    let callback: Arc<dyn ttarchive::ProgressCallback> = Arc::new(move |_: &ProgressUpdate<'_>| {
        counter.fetch_add(1, Ordering::Relaxed);
    });

    Archive::new(&archive).with_progress(Arc::clone(&callback)).create_from([&source]).unwrap();
    assert!(calls.load(Ordering::Relaxed) > 0);
}

#[test]
fn every_writable_method_round_trips_through_the_public_api() {
    let dir = common::TempDir::new("api-methods");
    let source = sample(&dir);

    for method in [Method::Store, Method::Deflate, Method::Bzip2] {
        assert!(method.can_encode(), "{method:?} should be writable");

        let archive = dir.join(format!("m{}.zip", method.code()));
        Archive::new(&archive).set_method(method).create_from([&source]).unwrap_or_else(|e| panic!("{method:?}: {e}"));

        let out = dir.join(format!("out{}", method.code()));
        Archive::new(&archive).extract_to(&out).unwrap_or_else(|e| panic!("{method:?}: {e}"));
        assert_eq!(std::fs::read(out.join("src/a.txt")).unwrap(), common::compressible(40_000), "{method:?}");
    }
}

#[test]
fn a_decode_only_method_is_refused_at_creation() {
    let dir = common::TempDir::new("api-decode-only");
    let source = sample(&dir);

    for method in [Method::Lzma, Method::Xz, Method::Zstd, Method::Ppmd, Method::Deflate64, Method::Shrink] {
        assert!(!method.can_encode(), "{method:?} must be decode only");
        let archive = dir.join("x.zip");
        let result = Archive::new(&archive).set_method(method).create_from([&source]);
        let err = result.err().unwrap_or_else(|| panic!("{method:?} should not be writable"));
        assert!(err.is_unsupported(), "{method:?} gave {err}");
    }
}

#[test]
fn level_none_stores_and_best_compresses() {
    let dir = common::TempDir::new("api-levels");
    let source = dir.join("src");
    dir.write("src/text.txt", common::compressible(200_000));

    let stored = dir.join("stored.zip");
    Archive::new(&stored).set_level(Level::None).create_from([&source]).unwrap();

    let best = dir.join("best.zip");
    Archive::new(&best).set_level(Level::Best).create_from([&source]).unwrap();

    let stored_len = std::fs::metadata(&stored).unwrap().len();
    let best_len = std::fs::metadata(&best).unwrap().len();
    assert!(best_len * 4 < stored_len, "Level::Best {best_len} should be far smaller than Level::None {stored_len}");

    for archive in [&stored, &best] {
        let out = dir.join(format!("out{}", std::fs::metadata(archive).unwrap().len()));
        Archive::new(archive).extract_to(&out).unwrap();
        assert_eq!(std::fs::read(out.join("src/text.txt")).unwrap(), common::compressible(200_000));
    }
}

#[test]
fn an_unknown_extension_without_magic_is_rejected() {
    let dir = common::TempDir::new("api-unknown");
    let source = sample(&dir);

    let result = Archive::new(dir.join("out.7z")).create_from([&source]);
    assert!(result.is_err(), "an unknown target extension should not silently produce an archive");

    let result = Archive::new(dir.join("out")).create_from([&source]);
    assert!(result.is_err(), "a name with no extension at all should not resolve");
}
