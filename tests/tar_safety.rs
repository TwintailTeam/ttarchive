mod common;

use std::path::Path;

use ttarchive::platform::EntryMeta;
use ttarchive::tar::TarWriter;
use ttarchive::{Archive, ArchiveType, Overwrite, UnsafeEntries};

fn hostile(dir: &common::TempDir, name: &str, entries: &[(&str, &[u8])]) -> std::path::PathBuf {
    let archive = dir.join(name);
    let file = std::fs::File::create(&archive).unwrap();

    let mut tar = TarWriter::new(std::io::BufWriter::new(file));
    for (entry, body) in entries {
        tar.add_entry(entry, &EntryMeta::file(), body, "").unwrap();
    }
    tar.finish().unwrap();

    archive
}

fn hostile_link(dir: &common::TempDir, name: &str, entry: &str, target: &str) -> std::path::PathBuf {
    let archive = dir.join(name);
    let file = std::fs::File::create(&archive).unwrap();

    let mut tar = TarWriter::new(std::io::BufWriter::new(file));
    tar.add_entry(entry, &EntryMeta::symlink(), &[], target).unwrap();
    tar.finish().unwrap();

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
fn a_tar_entry_climbing_out_with_dotdot_is_refused() {
    let dir = common::TempDir::new("tar-traversal");
    let archive = hostile(&dir, "evil.tar", &[("../escaped.txt", b"should not land")]);

    let out = dir.join("out");
    let result = Archive::new(&archive).extract_to(&out);
    assert!(result.is_err(), "a `..` entry must be refused by default");

    assert!(!dir.join("escaped.txt").exists(), "the entry escaped the destination");
    assert!(tree(&out).is_empty(), "nothing should have been written");
}

#[test]
fn a_deep_traversal_is_refused_too() {
    let dir = common::TempDir::new("tar-traversal-deep");
    let archive = hostile(&dir, "evil.tar", &[("a/b/../../../../escaped.txt", b"nope")]);

    assert!(Archive::new(&archive).extract_to(dir.join("out")).is_err(), "a buried `..` chain must be refused");
    assert!(!dir.join("escaped.txt").exists());
}

#[test]
fn an_absolute_tar_entry_is_refused() {
    let dir = common::TempDir::new("tar-absolute");
    let archive = hostile(&dir, "evil.tar", &[("/tmp/ttarchive-should-not-exist.txt", b"nope")]);

    let result = Archive::new(&archive).extract_to(dir.join("out"));
    assert!(result.is_err(), "an absolute entry name must be refused");
    assert!(!Path::new("/tmp/ttarchive-should-not-exist.txt").exists(), "an absolute name escaped");
}

#[test]
fn skipping_unsafe_entries_keeps_the_safe_ones() {
    let dir = common::TempDir::new("tar-skip-unsafe");
    let archive = hostile(&dir, "mixed.tar", &[("../escaped.txt", b"nope"), ("kept.txt", b"kept contents")]);

    let out = dir.join("out");
    let summary = Archive::new(&archive).set_unsafe_entries(UnsafeEntries::Skip).extract_to(&out).expect("skipping should succeed");

    assert!(summary.refused >= 1, "the hostile entry should be counted as refused");
    assert_eq!(tree(&out), vec!["kept.txt"]);
    assert_eq!(std::fs::read(out.join("kept.txt")).unwrap(), b"kept contents");
    assert!(!dir.join("escaped.txt").exists());
}

#[test]
fn a_symlink_pointing_outside_the_destination_is_refused() {
    let dir = common::TempDir::new("tar-symlink-escape");
    let archive = hostile_link(&dir, "evil.tar", "link.txt", "../../../../etc/passwd");

    let out = dir.join("out");
    let result = Archive::new(&archive).extract_to(&out);

    match result {
        Err(_) => {}
        Ok(summary) => assert!(summary.symlinks == 0, "an escaping symlink must not be created"),
    }

    let planted = out.join("link.txt");
    if let Ok(meta) = std::fs::symlink_metadata(&planted) {
        assert!(!meta.is_symlink(), "an escaping symlink was created anyway");
    }
}

#[test]
fn an_escaping_symlink_can_be_skipped_instead() {
    let dir = common::TempDir::new("tar-symlink-skip");
    let archive = hostile_link(&dir, "evil.tar", "link.txt", "../../../etc/passwd");

    let out = dir.join("out");
    let summary = Archive::new(&archive).set_unsafe_entries(UnsafeEntries::Skip).extract_to(&out).expect("skipping should succeed");
    assert!(summary.refused >= 1 || summary.symlinks == 0, "the escaping link should be refused, not created");
}

#[test]
fn a_symlink_inside_the_destination_round_trips() {
    if !cfg!(unix) {
        eprintln!("skipping: symlinks need unix");
        return;
    }

    let dir = common::TempDir::new("tar-symlink-ok");
    dir.write("src/target.txt", b"the real contents");

    #[cfg(unix)]
    std::os::unix::fs::symlink("target.txt", dir.join("src/link.txt")).unwrap();

    for name in ["s.tar", "s.tar.gz"] {
        let archive = dir.join(name);
        Archive::new(&archive).create_from([dir.join("src")]).unwrap_or_else(|e| panic!("{name}: {e}"));

        let out = dir.join(format!("{name}-out"));
        let summary = Archive::new(&archive).extract_to(&out).unwrap_or_else(|e| panic!("{name}: {e}"));

        assert_eq!(summary.symlinks, 1, "{name}: the link should be restored as a link");

        let restored = out.join("src/link.txt");
        let meta = std::fs::symlink_metadata(&restored).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert!(meta.is_symlink(), "{name}: restored as a plain file, not a link");
        assert_eq!(std::fs::read(&restored).unwrap(), b"the real contents", "{name}: link resolves to the wrong data");
    }
}

#[test]
fn symlinks_can_be_left_out_of_the_extraction() {
    if !cfg!(unix) {
        eprintln!("skipping: symlinks need unix");
        return;
    }

    let dir = common::TempDir::new("tar-symlink-off");
    dir.write("src/target.txt", b"data");
    #[cfg(unix)]
    std::os::unix::fs::symlink("target.txt", dir.join("src/link.txt")).unwrap();

    let archive = dir.join("s.tar");
    Archive::new(&archive).create_from([dir.join("src")]).unwrap();

    let mut options = ttarchive::ExtractOptions { restore_symlinks: false, ..Default::default() };
    options.preserve_permissions = true;

    let out = dir.join("out");
    let summary = Archive::new(&archive).with_extract_options(options).extract_to(&out).unwrap();

    assert_eq!(summary.symlinks, 0);
    assert!(!out.join("src/link.txt").exists(), "the link should have been left out");
    assert!(out.join("src/target.txt").exists(), "the real file should still be there");
}

#[test]
fn tar_preserves_unix_permissions() {
    if !cfg!(unix) {
        eprintln!("skipping: modes need unix");
        return;
    }

    let dir = common::TempDir::new("tar-modes");
    dir.write("src/exec.sh", b"#!/bin/sh\necho hi\n");
    dir.write("src/plain.txt", b"plain");

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir.join("src/exec.sh"), std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::set_permissions(dir.join("src/plain.txt"), std::fs::Permissions::from_mode(0o600)).unwrap();
    }

    for name in ["m.tar", "m.tar.gz", "m.tar.lzma"] {
        let archive = dir.join(name);
        Archive::new(&archive).create_from([dir.join("src")]).unwrap_or_else(|e| panic!("{name}: {e}"));

        let out = dir.join(format!("{name}-out"));
        Archive::new(&archive).extract_to(&out).unwrap_or_else(|e| panic!("{name}: {e}"));

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let exec = std::fs::metadata(out.join("src/exec.sh")).unwrap().permissions().mode() & 0o777;
            let plain = std::fs::metadata(out.join("src/plain.txt")).unwrap().permissions().mode() & 0o777;
            assert_eq!(exec, 0o755, "{name}: executable bit lost");
            assert_eq!(plain, 0o600, "{name}: restrictive mode lost");
        }
    }
}

#[test]
fn tar_preserves_modification_times() {
    let dir = common::TempDir::new("tar-mtime");
    dir.write("src/a.txt", b"timed");

    let archive = dir.join("t.tar");
    Archive::new(&archive).create_from([dir.join("src")]).unwrap();

    let listed = Archive::new(&archive).entries().unwrap();
    let entry = listed.iter().find(|e| e.name.ends_with("a.txt")).expect("a.txt missing");
    assert!(entry.mtime() > 1_600_000_000, "mtime looks unset: {}", entry.mtime());

    let out = dir.join("out");
    Archive::new(&archive).extract_to(&out).unwrap();

    let original = std::fs::metadata(dir.join("src/a.txt")).unwrap().modified().unwrap();
    let restored = std::fs::metadata(out.join("src/a.txt")).unwrap().modified().unwrap();

    let drift = original.duration_since(restored).or_else(|_| restored.duration_since(original)).unwrap();
    assert!(drift.as_secs() <= 2, "mtime drifted by {drift:?}");
}

#[test]
fn the_overwrite_policy_is_honoured_for_tarballs() {
    let dir = common::TempDir::new("tar-overwrite");
    dir.write("src/a.txt", b"first version");

    let archive = dir.join("o.tar.gz");
    Archive::new(&archive).create_from([dir.join("src")]).unwrap();

    let out = dir.join("out");
    Archive::new(&archive).extract_to(&out).unwrap();
    std::fs::write(out.join("src/a.txt"), b"locally edited").unwrap();

    let summary = Archive::new(&archive).set_overwrite(Overwrite::Never).extract_to(&out).unwrap();
    assert!(summary.skipped > 0, "Never should skip the existing file");
    assert_eq!(std::fs::read(out.join("src/a.txt")).unwrap(), b"locally edited", "Never must not replace it");

    assert!(Archive::new(&archive).set_overwrite(Overwrite::Error).extract_to(&out).is_err(), "Error should refuse to replace");

    Archive::new(&archive).set_overwrite(Overwrite::Always).extract_to(&out).unwrap();
    assert_eq!(std::fs::read(out.join("src/a.txt")).unwrap(), b"first version", "Always should replace it");
}

#[test]
fn an_empty_tarball_round_trips() {
    let dir = common::TempDir::new("tar-empty");
    std::fs::create_dir_all(dir.join("src")).unwrap();

    for name in ["e.tar", "e.tar.gz", "e.tar.bz2", "e.tar.lzma"] {
        let archive = dir.join(name);
        let summary = Archive::new(&archive).create_from([dir.join("src")]).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(summary.files, 0, "{name}");

        let entries = Archive::new(&archive).entries().unwrap_or_else(|e| panic!("{name}: listing failed: {e}"));
        assert!(entries.len() <= 1, "{name}: an empty tree should hold at most its own directory, got {entries:?}");

        let out = dir.join(format!("{name}-out"));
        Archive::new(&archive).extract_to(&out).unwrap_or_else(|e| panic!("{name}: extract failed: {e}"));
        assert!(out.is_dir(), "{name}: destination should still exist");
    }
}

#[test]
fn a_tarball_holding_many_files_round_trips() {
    let dir = common::TempDir::new("tar-many");
    for index in 0..500 {
        dir.write(format!("src/d{}/f{index}.txt", index % 20), format!("contents of {index}").as_bytes());
    }

    let archive = dir.join("many.tar.gz");
    let summary = Archive::new(&archive).create_from([dir.join("src")]).unwrap();
    assert_eq!(summary.files, 500);

    let out = dir.join("out");
    let back = Archive::new(&archive).extract_to(&out).unwrap();
    assert_eq!(back.files, 500);

    for index in [0usize, 1, 250, 499] {
        let path = out.join(format!("src/d{}/f{index}.txt", index % 20));
        assert_eq!(std::fs::read(&path).unwrap(), format!("contents of {index}").as_bytes(), "{path:?}");
    }
}

#[test]
fn a_truncated_tarball_is_reported_not_silently_accepted() {
    let dir = common::TempDir::new("tar-truncated");
    dir.write("src/a.txt", common::compressible(60_000));

    let archive = dir.join("t.tar");
    Archive::new(&archive).create_from([dir.join("src")]).unwrap();

    let full = std::fs::read(&archive).unwrap();
    let cut = dir.join("cut.tar");
    std::fs::write(&cut, &full[..full.len() / 2]).unwrap();

    let out = dir.join("out");
    match Archive::new(&cut).set_type(ArchiveType::Tar).extract_to(&out) {
        Err(_) => {}
        Ok(summary) => assert!(summary.bytes < 60_000, "a truncated archive should not report a complete extraction"),
    }
}

#[test]
fn a_corrupt_gzip_wrapper_is_reported() {
    let dir = common::TempDir::new("tar-corrupt-gz");
    dir.write("src/a.txt", common::compressible(50_000));

    let archive = dir.join("c.tar.gz");
    Archive::new(&archive).create_from([dir.join("src")]).unwrap();

    let mut bytes = std::fs::read(&archive).unwrap();
    let middle = bytes.len() / 2;
    bytes[middle] ^= 0xff;
    std::fs::write(&archive, &bytes).unwrap();

    assert!(Archive::new(&archive).extract_to(dir.join("out")).is_err(), "a flipped byte inside the gzip stream must be reported");
}

#[test]
fn extracting_into_a_symlinked_destination_is_refused() {
    if !cfg!(unix) {
        eprintln!("skipping: symlinks need unix");
        return;
    }

    let dir = common::TempDir::new("tar-dest-symlink");
    dir.write("src/a.txt", b"data");

    let archive = dir.join("d.tar");
    Archive::new(&archive).create_from([dir.join("src")]).unwrap();

    let real = dir.join("real");
    std::fs::create_dir_all(&real).unwrap();
    let link = dir.join("link");

    #[cfg(unix)]
    std::os::unix::fs::symlink(&real, &link).unwrap();

    assert!(Archive::new(&archive).extract_to(&link).is_err(), "a symlinked destination must be refused");
}
