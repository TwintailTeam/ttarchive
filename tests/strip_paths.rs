mod common;

use std::path::Path;

use ttarchive::{Archive, Overwrite};

fn archive_of(dir: &common::TempDir, names: &[&str]) -> std::path::PathBuf {
    let source = dir.join("src");
    for name in names {
        dir.write(Path::new("src").join(name), name.as_bytes());
    }

    let archive = dir.join("in.zip");
    Archive::new(&archive).create_from([&source]).expect("create failed");
    archive
}

fn tree(root: &Path) -> Vec<String> {
    fn walk(base: &Path, at: &Path, out: &mut Vec<String>) {
        let Ok(entries) = std::fs::read_dir(at) else { return };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(base, &path, out);
            } else {
                out.push(path.strip_prefix(base).unwrap().to_string_lossy().replace('\\', "/"));
            }
        }
    }
    let mut out = Vec::new();
    walk(root, root, &mut out);
    out.sort();
    out
}

#[test]
fn strip_root_drops_a_shared_wrapper_directory() {
    let dir = common::TempDir::new("strip-root");
    let archive = archive_of(&dir, &["file.txt", "subfolder/some.exe", "subfolder/deep/x.bin"]);

    let out = dir.join("out");
    Archive::new(&archive).set_strip_root(true).extract_to(&out).expect("extract failed");

    assert_eq!(tree(&out), vec!["file.txt", "subfolder/deep/x.bin", "subfolder/some.exe"]);
}

#[test]
fn without_stripping_the_wrapper_is_kept() {
    let dir = common::TempDir::new("strip-none");
    let archive = archive_of(&dir, &["file.txt", "subfolder/some.exe"]);

    let out = dir.join("out");
    Archive::new(&archive).extract_to(&out).expect("extract failed");

    assert_eq!(tree(&out), vec!["src/file.txt", "src/subfolder/some.exe"]);
}

#[test]
fn strip_root_is_a_no_op_without_a_common_root() {
    let dir = common::TempDir::new("strip-noroot");

    let a = dir.write("a/one.txt", b"one");
    let b = dir.write("b/two.txt", b"two");
    let archive = dir.join("in.zip");
    Archive::new(&archive).create_from([a.parent().unwrap(), b.parent().unwrap()]).unwrap();

    let out = dir.join("out");
    Archive::new(&archive).set_strip_root(true).extract_to(&out).expect("extract failed");

    assert_eq!(tree(&out), vec!["a/one.txt", "b/two.txt"]);
}

#[test]
fn strip_root_never_empties_a_flat_archive() {
    let dir = common::TempDir::new("strip-flat");
    let file = dir.write("report.pdf", b"pdf");

    let archive = dir.join("in.zip");
    Archive::new(&archive).create_from([&file]).unwrap();

    let out = dir.join("out");
    let summary = Archive::new(&archive).set_strip_root(true).extract_to(&out).expect("extract failed");

    assert_eq!(summary.files, 1);
    assert_eq!(tree(&out), vec!["report.pdf"]);
}

#[test]
fn strip_components_drops_a_fixed_number_of_levels() {
    let dir = common::TempDir::new("strip-n");
    let archive = archive_of(&dir, &["a/b/deep.txt", "a/shallow.txt"]);

    let out = dir.join("out");
    Archive::new(&archive).set_strip_components(2).extract_to(&out).expect("extract failed");

    assert_eq!(tree(&out), vec!["b/deep.txt", "shallow.txt"]);
}

#[test]
fn entries_consumed_entirely_by_stripping_are_skipped() {
    let dir = common::TempDir::new("strip-empty");
    let archive = archive_of(&dir, &["kept.txt"]);

    let out = dir.join("out");
    let summary = Archive::new(&archive).set_strip_components(2).extract_to(&out).expect("extract failed");

    assert!(out.is_dir(), "destination must still be a directory");
    assert_eq!(summary.files, 0);
    assert!(summary.skipped >= 1, "the emptied entry should be counted as skipped");
    assert!(tree(&out).is_empty());
}

#[test]
fn stripping_past_every_entry_produces_nothing() {
    let dir = common::TempDir::new("strip-past");
    let archive = archive_of(&dir, &["a/b/c.txt"]);

    let out = dir.join("out");
    let summary = Archive::new(&archive).set_strip_components(99).extract_to(&out).expect("extract failed");

    assert_eq!(summary.files, 0);
    assert!(tree(&out).is_empty());
    assert!(out.is_dir());
}

#[test]
fn colliding_entries_follow_the_overwrite_policy() {
    let dir = common::TempDir::new("strip-collide");
    let archive = archive_of(&dir, &["a/dup.txt", "b/dup.txt"]);

    let last = dir.join("last");
    Archive::new(&archive).set_strip_components(2).set_overwrite(Overwrite::Always).extract_to(&last).unwrap();
    assert_eq!(tree(&last), vec!["dup.txt"]);

    let first = dir.join("first");
    let summary = Archive::new(&archive).set_strip_components(2).set_overwrite(Overwrite::Never).extract_to(&first).unwrap();
    assert_eq!(tree(&first), vec!["dup.txt"]);
    assert!(summary.skipped >= 1, "the second entry should have been skipped");
}

#[test]
fn stripping_does_not_weaken_the_path_checks() {
    let dir = common::TempDir::new("strip-safety");
    let archive = archive_of(&dir, &["ok.txt"]);

    let out = dir.join("out");
    Archive::new(&archive).set_strip_components(1).extract_to(&out).expect("extract failed");
    for path in tree(&out) {
        assert!(!path.contains(".."), "{path} escaped the destination");
    }
}

#[test]
fn strip_root_adds_to_strip_components() {
    let dir = common::TempDir::new("strip-both");
    let archive = archive_of(&dir, &["a/b/c.txt"]);

    let out = dir.join("out");
    Archive::new(&archive).set_strip_root(true).set_strip_components(1).extract_to(&out).expect("extract failed");

    assert_eq!(tree(&out), vec!["b/c.txt"]);
}

#[test]
fn colliding_entries_resolve_deterministically_across_many_runs() {
    let dir = common::TempDir::new("strip-collide-repeat");
    let archive = archive_of(&dir, &["a/dup.txt", "b/dup.txt", "c/dup.txt"]);

    let mut baseline: Option<u64> = None;

    for run in 0..40 {
        let never = dir.join(format!("never{run}"));
        let summary = Archive::new(&archive).set_strip_components(2).set_overwrite(Overwrite::Never).extract_to(&never).unwrap();
        assert_eq!(tree(&never), vec!["dup.txt"], "run {run}");
        assert_eq!(summary.files, 1, "run {run}: exactly one entry should be written");

        assert!(summary.skipped >= 2, "run {run}: both losing entries must be reported as skipped, got {}", summary.skipped);
        match baseline {
            None => baseline = Some(summary.skipped),
            Some(first) => assert_eq!(summary.skipped, first, "run {run}: the skipped count must not vary between runs"),
        }

        let always = dir.join(format!("always{run}"));
        let summary = Archive::new(&archive).set_strip_components(2).set_overwrite(Overwrite::Always).extract_to(&always).unwrap();
        assert_eq!(tree(&always), vec!["dup.txt"], "run {run}");
        assert_eq!(summary.files, 1, "run {run}: collisions must collapse to one write");
    }
}

#[test]
fn overwrite_error_rejects_entries_that_collide_after_stripping() {
    let dir = common::TempDir::new("strip-collide-error");
    let archive = archive_of(&dir, &["a/dup.txt", "b/dup.txt"]);

    let out = dir.join("out");
    let result = Archive::new(&archive).set_strip_components(2).set_overwrite(Overwrite::Error).extract_to(&out);
    assert!(result.is_err(), "Overwrite::Error must refuse a collision created by stripping");

    let apart = dir.join("apart");
    Archive::new(&archive).set_overwrite(Overwrite::Error).extract_to(&apart).expect("without stripping the names do not collide");
}
