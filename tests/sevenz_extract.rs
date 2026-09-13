mod common;

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use common::{TempDir, compressible, have, pseudo_random, skip};
use ttarchive::crypto::Password;
use ttarchive::pipeline::sevenz;
use ttarchive::pipeline::{ExtractOptions, Overwrite};
use ttarchive::platform::EntryKind;
use ttarchive::utils::progress::Reporter;

fn build(dir: &TempDir, name: &str, switches: &[&str]) -> PathBuf {
    let mut args = vec!["a"];
    args.extend_from_slice(switches);
    args.push(name);
    args.push("src");

    let output = Command::new(common::resolve("7z")).args(&args).current_dir(dir.path()).output().unwrap_or_else(|e| panic!("failed to run 7z: {e}"));
    assert!(output.status.success(), "7z {args:?} failed:\n{}", String::from_utf8_lossy(&output.stderr));
    dir.join(name)
}

fn snapshot(root: &Path) -> BTreeMap<String, Vec<u8>> {
    let mut out = BTreeMap::new();
    let mut stack = vec![root.to_path_buf()];

    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir).expect("read_dir") {
            let entry = entry.expect("dir entry");
            let path = entry.path();
            let rel = path.strip_prefix(root).unwrap().to_string_lossy().replace('\\', "/");
            let md = fs::symlink_metadata(&path).expect("metadata");

            if md.is_symlink() {
                out.insert(format!("{rel} -> "), fs::read_link(&path).expect("read_link").to_string_lossy().into_owned().into_bytes());
            } else if md.is_dir() {
                out.insert(format!("{rel}/"), Vec::new());
                stack.push(path);
            } else {
                out.insert(rel, fs::read(&path).expect("read file"));
            }
        }
    }
    out
}

fn tree(dir: &TempDir) -> PathBuf {
    dir.write("src/readme.txt", "hello world");
    dir.write("src/prose.txt", compressible(40_000));
    dir.write("src/nested/noise.bin", pseudo_random(30_000, 3));
    dir.write("src/nested/deep/leaf.txt", b"leaf contents");
    dir.write("src/empty.txt", b"");
    dir.write("src/unicode-\u{00e9}\u{672c}.txt", "non-ascii name");
    fs::create_dir_all(dir.join("src/empty-dir")).unwrap();
    dir.join("src")
}

fn extract_to(archive: &Path, dest: &Path, options: &ExtractOptions) -> ttarchive::ExtractSummary {
    sevenz::extract(archive, dest, options, &Reporter::disabled()).unwrap_or_else(|e| panic!("{}: {e}", archive.display()))
}

#[test]
fn a_seven_zip_archive_extracts_to_the_tree_it_was_made_from() {
    if !have("7z") {
        return skip("7z");
    }

    for switches in [&["-mhc=off", "-ms=off"][..], &["-mhc=on", "-ms=on"][..], &["-m0=PPMd", "-ms=on"][..]] {
        let dir = TempDir::new("sz-x-round");
        let source = tree(&dir);
        let archive = build(&dir, "a.7z", switches);

        let out = dir.join("out");
        let summary = extract_to(&archive, &out, &ExtractOptions::default());

        assert_eq!(snapshot(&out.join("src")), snapshot(&source), "{switches:?}");
        assert_eq!(summary.files, 6, "{switches:?}");
        assert_eq!(summary.directories, 4, "{switches:?}");
        assert_eq!(summary.skipped, 0, "{switches:?}");
    }
}

#[test]
fn one_file_from_a_solid_folder_comes_out_without_the_rest() {
    if !have("7z") {
        return skip("7z");
    }

    let dir = TempDir::new("sz-x-select");
    let source = tree(&dir);
    let archive = build(&dir, "solid.7z", &["-mhc=off", "-ms=on"]);

    let options = ExtractOptions { selection: vec!["src/nested/deep/leaf.txt".into()], ..ExtractOptions::default() };
    let out = dir.join("out");
    let summary = extract_to(&archive, &out, &options);

    assert_eq!(summary.files, 1);
    assert_eq!(fs::read(out.join("src/nested/deep/leaf.txt")).unwrap(), fs::read(source.join("nested/deep/leaf.txt")).unwrap());
    assert!(!out.join("src/prose.txt").exists());
}

#[cfg(unix)]
#[test]
fn symbolic_links_come_back_as_links_and_are_skipped_when_asked() {
    if !have("7z") {
        return skip("7z");
    }

    let dir = TempDir::new("sz-x-links");
    dir.write("src/real.txt", "the target contents");
    std::os::unix::fs::symlink("real.txt", dir.join("src/link.txt")).unwrap();
    let archive = build(&dir, "links.7z", &["-mhc=off", "-snl"]);

    let listed = sevenz::entries(&archive, None).unwrap();
    let link = listed.iter().find(|e| e.name.ends_with("link.txt")).expect("the link is listed");
    assert_eq!(link.kind(), EntryKind::Symlink);

    let out = dir.join("out");
    let summary = extract_to(&archive, &out, &ExtractOptions::default());
    assert_eq!(summary.symlinks, 1);
    assert_eq!(fs::read_link(out.join("src/link.txt")).unwrap(), Path::new("real.txt"));

    let bare = dir.join("bare");
    let options = ExtractOptions { restore_symlinks: false, ..ExtractOptions::default() };
    let summary = extract_to(&archive, &bare, &options);
    assert_eq!(summary.symlinks, 0);
    assert!(!bare.join("src/link.txt").exists());
}

#[cfg(unix)]
#[test]
fn a_link_pointing_out_of_the_destination_is_refused() {
    if !have("7z") {
        return skip("7z");
    }

    let dir = TempDir::new("sz-x-escape");
    dir.write("src/keep.txt", "kept");
    std::os::unix::fs::symlink("../../../etc/passwd", dir.join("src/escape.txt")).unwrap();
    let archive = build(&dir, "escape.7z", &["-mhc=off", "-snl"]);

    let error = sevenz::extract(&archive, &dir.join("out"), &ExtractOptions::default(), &Reporter::disabled()).unwrap_err();
    assert!(matches!(error, ttarchive::Error::UnsafeEntryPath { .. }), "{error}");

    let options = ExtractOptions { unsafe_entries: ttarchive::UnsafeEntries::Skip, ..ExtractOptions::default() };
    let summary = extract_to(&archive, &dir.join("skipped"), &options);
    assert_eq!(summary.refused, 1);
    assert_eq!(summary.symlinks, 0);
    assert!(dir.join("skipped/src/keep.txt").exists());
}

#[test]
fn an_encrypted_archive_extracts_with_its_password_and_writes_nothing_without_it() {
    if !have("7z") {
        return skip("7z");
    }

    let dir = TempDir::new("sz-x-aes");
    let source = tree(&dir);
    let archive = build(&dir, "aes.7z", &["-mhe=on", "-ms=on", "-pcorrect horse battery"]);

    let out = dir.join("out");
    let options = ExtractOptions { password: Some(Password::from("correct horse battery")), ..ExtractOptions::default() };
    extract_to(&archive, &out, &options);
    assert_eq!(snapshot(&out.join("src")), snapshot(&source));

    let error = sevenz::extract(&archive, &dir.join("none"), &ExtractOptions::default(), &Reporter::disabled()).unwrap_err();
    assert!(error.needs_password(), "{error}");

    let options = ExtractOptions { password: Some(Password::from("wrong")), ..ExtractOptions::default() };
    assert!(sevenz::extract(&archive, &dir.join("wrong"), &options, &Reporter::disabled()).is_err());
}

#[test]
fn a_wrong_password_on_encrypted_data_fails_the_checksum_rather_than_writing_rubbish() {
    if !have("7z") {
        return skip("7z");
    }

    let dir = TempDir::new("sz-x-aes-data");
    tree(&dir);
    let archive = build(&dir, "data.7z", &["-mhc=off", "-ms=on", "-pcorrect horse battery"]);

    let options = ExtractOptions { password: Some(Password::from("wrong")), ..ExtractOptions::default() };
    let error = sevenz::extract(&archive, &dir.join("out"), &options, &Reporter::disabled()).unwrap_err();
    assert!(matches!(error, ttarchive::Error::ChecksumMismatch { .. } | ttarchive::Error::Malformed { .. }), "{error}");
}

#[test]
fn stripping_components_drops_the_leading_directory() {
    if !have("7z") {
        return skip("7z");
    }

    let dir = TempDir::new("sz-x-strip");
    let source = tree(&dir);
    let archive = build(&dir, "strip.7z", &["-mhc=off"]);

    let out = dir.join("out");
    let options = ExtractOptions { strip_components: 1, ..ExtractOptions::default() };
    extract_to(&archive, &out, &options);

    assert_eq!(snapshot(&out), snapshot(&source));
}

#[test]
fn an_existing_file_is_left_alone_when_overwriting_is_refused() {
    if !have("7z") {
        return skip("7z");
    }

    let dir = TempDir::new("sz-x-overwrite");
    tree(&dir);
    let archive = build(&dir, "over.7z", &["-mhc=off", "-ms=on"]);

    let out = dir.join("out");
    fs::create_dir_all(out.join("src")).unwrap();
    fs::write(out.join("src/readme.txt"), "already here").unwrap();

    let options = ExtractOptions { overwrite: Overwrite::Never, ..ExtractOptions::default() };
    let summary = extract_to(&archive, &out, &options);

    assert_eq!(fs::read(out.join("src/readme.txt")).unwrap(), b"already here");
    assert_eq!(summary.skipped, 1);
    assert_eq!(summary.files, 5);
}

fn with_deletion_marker(dir: &TempDir, name: &str, gone: &str) -> PathBuf {
    build(dir, name, &["-mhc=off"]);
    fs::remove_file(dir.join(gone)).unwrap();

    let args = ["u", "-mhc=off", "-up0q3r2x2y2z1w2", name, "src"];
    let output = Command::new(common::resolve("7z")).args(args).current_dir(dir.path()).output().unwrap_or_else(|e| panic!("failed to run 7z: {e}"));
    assert!(output.status.success(), "7z {args:?} failed:\n{}", String::from_utf8_lossy(&output.stderr));

    dir.join(name)
}

#[test]
fn a_deletion_marker_removes_the_file_it_names() {
    if !have("7z") {
        return skip("7z");
    }

    let dir = TempDir::new("sz-x-anti");
    dir.write("src/keep.txt", "still here");
    dir.write("src/gone.txt", "not for long");
    let archive = with_deletion_marker(&dir, "diff.7z", "src/gone.txt");

    let listed = sevenz::entries(&archive, None).unwrap();
    let marker = listed.iter().find(|e| e.name.ends_with("gone.txt")).expect("the marker is listed");
    assert!(marker.sevenz().unwrap().is_anti);
    assert!(listed.iter().any(|e| e.name.ends_with("keep.txt")));

    let out = dir.join("out");
    fs::create_dir_all(out.join("src")).unwrap();
    fs::write(out.join("src/gone.txt"), "left over from an earlier restore").unwrap();

    let summary = extract_to(&archive, &out, &ExtractOptions::default());

    assert_eq!(summary.deleted, 1);
    assert!(!out.join("src/gone.txt").exists());
    assert_eq!(fs::read(out.join("src/keep.txt")).unwrap(), b"still here");
}

#[test]
fn a_deletion_marker_for_a_path_that_is_absent_deletes_nothing() {
    if !have("7z") {
        return skip("7z");
    }

    let dir = TempDir::new("sz-x-anti-absent");
    dir.write("src/keep.txt", "still here");
    dir.write("src/gone.txt", "not for long");
    let archive = with_deletion_marker(&dir, "diff.7z", "src/gone.txt");

    let summary = extract_to(&archive, &dir.join("out"), &ExtractOptions::default());
    assert_eq!(summary.deleted, 0);
    assert_eq!(summary.files, 1);
}

#[test]
fn a_deletion_marker_leaves_the_file_alone_when_overwriting_is_refused() {
    if !have("7z") {
        return skip("7z");
    }

    let dir = TempDir::new("sz-x-anti-never");
    dir.write("src/keep.txt", "still here");
    dir.write("src/gone.txt", "not for long");
    let archive = with_deletion_marker(&dir, "diff.7z", "src/gone.txt");

    let out = dir.join("out");
    fs::create_dir_all(out.join("src")).unwrap();
    fs::write(out.join("src/gone.txt"), "protected").unwrap();

    let options = ExtractOptions { overwrite: Overwrite::Never, ..ExtractOptions::default() };
    let summary = extract_to(&archive, &out, &options);

    assert_eq!(summary.deleted, 0);
    assert_eq!(fs::read(out.join("src/gone.txt")).unwrap(), b"protected");
}

#[test]
fn listing_an_archive_gives_the_sizes_and_kinds_the_cli_reports() {
    if !have("7z") {
        return skip("7z");
    }

    let dir = TempDir::new("sz-x-list");
    let source = tree(&dir);
    let archive = build(&dir, "list.7z", &["-mhc=off", "-ms=on"]);

    let listed = sevenz::entries(&archive, None).unwrap();
    assert_eq!(listed.len(), 10);

    for entry in &listed {
        let on_disk = dir.join(&entry.name);
        assert!(on_disk.exists(), "{}", entry.name);

        if entry.is_dir() {
            assert_eq!(entry.size, 0, "{}", entry.name);
        } else {
            assert_eq!(entry.size, fs::metadata(&on_disk).unwrap().len(), "{}", entry.name);
            assert!(entry.sevenz().unwrap().crc.is_some() || entry.size == 0, "{}", entry.name);
        }
    }

    assert!(listed.iter().any(|e| e.name == source.file_name().unwrap().to_string_lossy()));
}

fn anti_archive(path: &Path, items: &[(&str, Option<&[u8]>)]) {
    use ttarchive::sevenz::writer::{Item, Options, SevenZWriter};

    let named = |name: &str| Item { name: name.into(), mtime: Some(1_700_000_000), attributes: Some(0x20) };
    let mut writer = SevenZWriter::new(fs::File::create(path).unwrap(), Options::default()).unwrap();

    for (name, data) in items {
        match data {
            Some(bytes) => writer.add_file(&named(name), bytes).unwrap(),
            None => writer.add_anti(&named(name)),
        }
    }
    writer.finish().unwrap();
}

#[test]
fn a_deletion_marker_we_wrote_reads_back_as_one() {
    let dir = TempDir::new("sz-x-anti-own");
    let archive = dir.join("own.7z");
    anti_archive(&archive, &[("src/keep.txt", Some(b"still here")), ("src/gone.txt", None), ("src/empty.txt", Some(b""))]);

    let listed = sevenz::entries(&archive, None).unwrap();
    let marker = listed.iter().find(|e| e.name == "src/gone.txt").expect("the marker is listed");
    let empty = listed.iter().find(|e| e.name == "src/empty.txt").expect("the empty file is listed");

    assert!(marker.sevenz().unwrap().is_anti);
    assert!(!empty.sevenz().unwrap().is_anti, "an empty file is not a deletion marker");
    assert_eq!(marker.meta.kind, EntryKind::File);

    let out = dir.join("out");
    fs::create_dir_all(out.join("src")).unwrap();
    fs::write(out.join("src/gone.txt"), "left over").unwrap();

    let summary = extract_to(&archive, &out, &ExtractOptions::default());
    assert_eq!(summary.deleted, 1);
    assert!(!out.join("src/gone.txt").exists());
    assert_eq!(fs::read(out.join("src/empty.txt")).unwrap(), b"");
    assert_eq!(fs::read(out.join("src/keep.txt")).unwrap(), b"still here");
}

#[test]
fn an_archive_that_both_writes_and_deletes_a_path_keeps_the_contents() {
    let dir = TempDir::new("sz-x-anti-both");
    let archive = dir.join("both.7z");
    anti_archive(&archive, &[("src/both.txt", Some(b"written")), ("src/both.txt", None)]);

    let out = dir.join("out");
    let summary = extract_to(&archive, &out, &ExtractOptions::default());

    assert_eq!(summary.deleted, 0, "a path the same archive writes must not be deleted by its own marker");
    assert_eq!(fs::read(out.join("src/both.txt")).unwrap(), b"written");
}

#[test]
fn a_deletion_marker_only_empties_a_directory_it_was_given_the_contents_of() {
    let dir = TempDir::new("sz-x-anti-dir");
    let archive = dir.join("dirs.7z");
    anti_archive(&archive, &[("src/keep.txt", Some(b"stay")), ("src/empty-dir", None), ("src/full-dir", None)]);

    let out = dir.join("out");
    fs::create_dir_all(out.join("src/empty-dir")).unwrap();
    fs::create_dir_all(out.join("src/full-dir")).unwrap();
    fs::write(out.join("src/full-dir/held.txt"), "not named by any marker").unwrap();

    let summary = extract_to(&archive, &out, &ExtractOptions::default());

    assert_eq!(summary.deleted, 1);
    assert!(!out.join("src/empty-dir").exists());
    assert!(out.join("src/full-dir/held.txt").exists(), "a directory holding files nothing tombstoned stays");
}

#[test]
fn a_windows_reparse_point_is_passed_over_rather_than_written_as_its_own_buffer() {
    use ttarchive::sevenz::spec::attribute;
    use ttarchive::sevenz::writer::{Item, Options, SevenZWriter};

    let dir = TempDir::new("sz-x-reparse");
    let archive = dir.join("reparse.7z");

    let mut writer = SevenZWriter::new(fs::File::create(&archive).unwrap(), Options::default()).unwrap();
    writer.add_file(&Item { name: "src/ordinary.txt".into(), mtime: None, attributes: Some(0x20) }, b"plain bytes").unwrap();
    writer
        .add_file(
            &Item { name: "src/junction".into(), mtime: None, attributes: Some(attribute::REPARSE_POINT | 0x20) },
            &[0x0c, 0x00, 0x00, 0xa0, 0x10, 0x00, 0x00, 0x00],
        )
        .unwrap();
    writer.finish().unwrap();

    let out = dir.join("out");
    let summary = extract_to(&archive, &out, &ExtractOptions::default());

    assert_eq!(summary.specials, 1);
    assert_eq!(summary.files, 1);
    assert!(!out.join("src/junction").exists(), "the reparse buffer must not land on disk as a file");
    assert_eq!(fs::read(out.join("src/ordinary.txt")).unwrap(), b"plain bytes");
}

#[test]
fn a_reparse_point_with_a_unix_mode_is_still_read_as_the_link_it_describes() {
    use ttarchive::sevenz::spec::attribute;
    use ttarchive::sevenz::writer::{Item, Options, SevenZWriter};

    let dir = TempDir::new("sz-x-reparse-unix");
    let archive = dir.join("link.7z");

    let attributes = attribute::REPARSE_POINT | attribute::UNIX_EXTENSION | ((0o120_777u32) << 16);
    let mut writer = SevenZWriter::new(fs::File::create(&archive).unwrap(), Options::default()).unwrap();
    writer.add_file(&Item { name: "src/target.txt".into(), mtime: None, attributes: Some(0x20) }, b"pointed at").unwrap();
    writer.add_file(&Item { name: "src/link.txt".into(), mtime: None, attributes: Some(attributes) }, b"target.txt").unwrap();
    writer.finish().unwrap();

    let out = dir.join("out");
    let summary = extract_to(&archive, &out, &ExtractOptions::default());

    assert_eq!(summary.specials, 0);
    assert_eq!(summary.symlinks, 1);
    assert_eq!(fs::read(out.join("src/link.txt")).unwrap(), b"pointed at");
}
