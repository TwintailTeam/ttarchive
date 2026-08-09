mod common;

use ttarchive::{Archive, ArchiveType};

fn tree(dir: &common::TempDir) -> std::path::PathBuf {
    dir.write("src/docs/guide.md", b"the guide");
    dir.write("src/docs/deep/notes.txt", b"some notes");
    dir.write("src/code/main.rs", b"fn main() {}");
    dir.write("src/README.md", b"read me");
    dir.write("src/docs-extra/other.txt", b"not under docs");
    dir.join("src")
}

fn names(root: &std::path::Path) -> Vec<String> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(at) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&at) else { continue };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else {
                out.push(path.strip_prefix(root).unwrap().to_string_lossy().replace('\\', "/"));
            }
        }
    }
    out.sort();
    out
}

const EVERY: [(&str, ArchiveType); 7] = [
    ("s.zip", ArchiveType::Zip),
    ("s.tar", ArchiveType::Tar),
    ("s.tar.gz", ArchiveType::TarGz),
    ("s.tar.bz2", ArchiveType::TarBz2),
    ("s.tar.xz", ArchiveType::TarXz),
    ("s.tar.lzma", ArchiveType::TarLzma),
    ("s.tar.zst", ArchiveType::TarZst),
];

#[test]
fn a_directory_takes_everything_beneath_it() {
    for (name, kind) in EVERY {
        let dir = common::TempDir::new("select-dir");
        let source = tree(&dir);
        let archive = dir.join(name);
        Archive::new(&archive).set_type(kind).create_from([&source]).unwrap_or_else(|e| panic!("{name}: {e}"));

        let out = dir.join("out");
        let got = Archive::new(&archive).set_type(kind).set_selection(["src/docs"]).extract_to(&out).unwrap_or_else(|e| panic!("{name}: {e}"));

        assert_eq!(names(&out), vec!["src/docs/deep/notes.txt", "src/docs/guide.md"], "{name}: wrong files");
        assert_eq!(got.files, 2, "{name}: summary counts more than was selected");
        assert_eq!(got.bytes, 19, "{name}: byte total covers more than the selection");
    }
}

#[test]
fn a_single_name_takes_only_that_entry() {
    let dir = common::TempDir::new("select-one");
    let source = tree(&dir);
    let archive = dir.join("one.tar.gz");
    Archive::new(&archive).create_from([&source]).unwrap();

    let out = dir.join("out");
    Archive::new(&archive).set_selection(["src/README.md"]).extract_to(&out).unwrap();
    assert_eq!(names(&out), vec!["src/README.md"]);
}

#[test]
fn a_prefix_does_not_leak_into_a_sibling() {
    let dir = common::TempDir::new("select-prefix");
    let source = tree(&dir);
    let archive = dir.join("p.zip");
    Archive::new(&archive).set_type(ArchiveType::Zip).create_from([&source]).unwrap();

    let out = dir.join("out");
    Archive::new(&archive).set_selection(["src/docs"]).extract_to(&out).unwrap();

    let found = names(&out);
    assert!(!found.iter().any(|n| n.contains("docs-extra")), "a sibling sharing the prefix was taken: {found:?}");
}

#[test]
fn several_names_can_be_selected_at_once() {
    let dir = common::TempDir::new("select-many");
    let source = tree(&dir);
    let archive = dir.join("m.tar");
    Archive::new(&archive).create_from([&source]).unwrap();

    let out = dir.join("out");
    Archive::new(&archive).set_selection(["src/code", "src/README.md"]).extract_to(&out).unwrap();
    assert_eq!(names(&out), vec!["src/README.md", "src/code/main.rs"]);
}

#[test]
fn an_empty_selection_takes_the_whole_archive() {
    let dir = common::TempDir::new("select-none");
    let source = tree(&dir);
    let archive = dir.join("a.tar.gz");
    Archive::new(&archive).create_from([&source]).unwrap();

    let out = dir.join("out");
    Archive::new(&archive).set_selection(Vec::<String>::new()).extract_to(&out).unwrap();
    assert_eq!(names(&out).len(), 5, "an empty selection should not filter anything");
}

#[test]
fn a_name_that_matches_nothing_extracts_nothing() {
    let dir = common::TempDir::new("select-miss");
    let source = tree(&dir);
    let archive = dir.join("a.tar.gz");
    Archive::new(&archive).create_from([&source]).unwrap();

    let out = dir.join("out");
    let got = Archive::new(&archive).set_selection(["src/nowhere"]).extract_to(&out).unwrap();
    assert_eq!(got.files, 0);
    assert!(names(&out).is_empty());
}

#[test]
fn stripping_applies_to_the_selection_not_the_whole_archive() {
    let dir = common::TempDir::new("select-strip");
    let source = tree(&dir);
    let archive = dir.join("a.tar.gz");
    Archive::new(&archive).create_from([&source]).unwrap();

    let out = dir.join("out");
    Archive::new(&archive).set_selection(["src/docs"]).set_strip_components(2).extract_to(&out).unwrap();
    assert_eq!(names(&out), vec!["deep/notes.txt", "guide.md"]);
}

#[test]
fn progress_completes_over_the_selection_alone() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};

    for (name, kind) in EVERY {
        let dir = common::TempDir::new("select-progress");
        let source = tree(&dir);
        let archive = dir.join(name);
        Archive::new(&archive).set_type(kind).create_from([&source]).unwrap();

        let seen = Arc::new((AtomicU64::new(0), AtomicU64::new(0)));
        let track = Arc::clone(&seen);

        Archive::new(&archive)
            .set_type(kind)
            .set_selection(["src/docs"])
            .on_progress(move |u: &ttarchive::ProgressUpdate<'_>| {
                track.0.fetch_max(u.processed_entries, Ordering::Relaxed);
                track.1.fetch_max(u.total_entries, Ordering::Relaxed);
            })
            .extract_to(dir.join("out"))
            .unwrap();

        let (done, total) = (seen.0.load(Ordering::Relaxed), seen.1.load(Ordering::Relaxed));
        assert!(total > 0 && done == total, "{name}: finished {done} of {total} entries");
        assert!(total <= 4, "{name}: totals counted entries outside the selection ({total}); src/docs holds two files and two directories");
    }
}

#[test]
fn selection_and_progress_hold_for_read_only_formats() {
    use std::process::Command;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};

    let have = |tool: &str| Command::new("which").arg(tool).output().is_ok_and(|o| o.status.success());

    for (tool, args, suffix, kind) in [("compress", vec!["-c"], "tar.Z", ArchiveType::TarZ), ("lzip", vec!["-c"], "tar.lz", ArchiveType::TarLz)] {
        if !have(tool) {
            eprintln!("skipping {suffix}: {tool} not installed");
            continue;
        }

        let dir = common::TempDir::new("select-readonly");
        let source = tree(&dir);

        let plain = dir.join("plain.tar");
        Archive::new(&plain).set_type(ArchiveType::Tar).create_from([&source]).unwrap();

        let packed = Command::new(tool).args(&args).arg(&plain).output().unwrap();
        assert!(packed.status.success(), "{tool} failed");
        let archive = dir.join(format!("a.{suffix}"));
        std::fs::write(&archive, &packed.stdout).unwrap();

        let seen = Arc::new((AtomicU64::new(0), AtomicU64::new(0)));
        let track = Arc::clone(&seen);

        let out = dir.join("out");
        let got = Archive::new(&archive)
            .set_type(kind)
            .set_selection(["src/docs"])
            .on_progress(move |u: &ttarchive::ProgressUpdate<'_>| {
                track.0.fetch_max(u.processed_entries, Ordering::Relaxed);
                track.1.fetch_max(u.total_entries, Ordering::Relaxed);
            })
            .extract_to(&out)
            .unwrap_or_else(|e| panic!("{suffix}: {e}"));

        assert_eq!(names(&out), vec!["src/docs/deep/notes.txt", "src/docs/guide.md"], "{suffix}: wrong files");
        assert_eq!(got.files, 2, "{suffix}: summary counts more than was selected");

        let (done, total) = (seen.0.load(Ordering::Relaxed), seen.1.load(Ordering::Relaxed));
        assert!(total > 0 && done == total, "{suffix}: finished {done} of {total} entries");
        assert!(total <= 4, "{suffix}: totals counted entries outside the selection ({total})");
    }
}

#[test]
fn the_byte_total_covers_the_selection_and_is_reached() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};

    const EACH: usize = 2 * 1024 * 1024;

    for (name, kind) in EVERY {
        let dir = common::TempDir::new("select-bytes");
        dir.write("src/pick/one.bin", common::compressible(EACH));
        dir.write("src/pick/two.bin", common::pseudo_random(EACH, 5));
        dir.write("src/skip/three.bin", common::compressible(EACH));
        dir.write("src/skip/four.bin", common::compressible(EACH));

        let archive = dir.join(name);
        Archive::new(&archive).set_type(kind).create_from([dir.join("src")]).unwrap();

        let seen = Arc::new((AtomicU64::new(0), AtomicU64::new(0)));
        let track = Arc::clone(&seen);

        let got = Archive::new(&archive)
            .set_type(kind)
            .set_selection(["src/pick"])
            .on_progress(move |u: &ttarchive::ProgressUpdate<'_>| {
                track.0.fetch_max(u.processed_bytes, Ordering::Relaxed);
                track.1.fetch_max(u.total_bytes, Ordering::Relaxed);
            })
            .extract_to(dir.join("out"))
            .unwrap();

        let want = 2 * EACH as u64;
        let (done, total) = (seen.0.load(Ordering::Relaxed), seen.1.load(Ordering::Relaxed));

        assert_eq!(total, want, "{name}: declared {total} bytes for two 2 MiB files, not {want}");
        assert_eq!(done, want, "{name}: reported {done} of {total} bytes, so the bar stops short of full");
        assert_eq!(got.bytes, want, "{name}: summary says {} bytes", got.bytes);
    }
}
