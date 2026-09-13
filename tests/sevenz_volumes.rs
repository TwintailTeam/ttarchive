mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use common::{TempDir, compressible, have, pseudo_random, skip};
use ttarchive::pipeline::sevenz;
use ttarchive::pipeline::{ExtractOptions, ExtractSummary};
use ttarchive::sevenz::SevenZReader;
use ttarchive::sevenz::volumes;
use ttarchive::utils::progress::Reporter;
use ttarchive::{Archive, ArchiveType};

const VOLUME: u64 = 64 * 1024;

fn tree(dir: &TempDir) -> PathBuf {
    dir.write("src/prose.txt", compressible(200_000));
    dir.write("src/noise.bin", pseudo_random(250_000, 6));
    dir.write("src/nested/small.txt", b"a few bytes");
    dir.join("src")
}

fn check(out: &Path) {
    assert_eq!(fs::read(out.join("src/prose.txt")).unwrap(), compressible(200_000));
    assert_eq!(fs::read(out.join("src/noise.bin")).unwrap(), pseudo_random(250_000, 6));
    assert_eq!(fs::read(out.join("src/nested/small.txt")).unwrap(), b"a few bytes");
}

fn extract_to(archive: &Path, dest: &Path, options: &ExtractOptions) -> ExtractSummary {
    sevenz::extract(archive, dest, options, &Reporter::disabled()).unwrap_or_else(|e| panic!("{}: {e}", archive.display()))
}

#[test]
fn a_volume_set_is_written_as_numbered_pieces_with_no_file_under_the_plain_name() {
    let dir = TempDir::new("sz-v-write");
    let source = tree(&dir);
    let archive = dir.join("split.7z");

    let made = Archive::new(&archive).set_volume_size(VOLUME).create_from([&source]).expect("create");

    assert!(made.volumes > 1, "the sample should not fit in one {VOLUME} byte volume");
    assert!(!archive.exists(), "7z leaves no file under the plain name; the pieces are the archive");

    let pieces: Vec<PathBuf> = (1..=made.volumes).map(|n| dir.join(format!("split.7z.{n:03}"))).collect();
    for piece in &pieces {
        assert!(piece.exists(), "{} is missing", piece.display());
    }

    let sizes: Vec<u64> = pieces.iter().map(|p| fs::metadata(p).unwrap().len()).collect();
    let (last, full) = sizes.split_last().unwrap();
    for size in full {
        assert_eq!(*size, VOLUME, "every volume but the last is exactly the volume size: {sizes:?}");
    }
    assert!(*last > 0 && *last <= VOLUME, "the last volume holds the remainder: {sizes:?}");
    assert_eq!(made.archive_size, sizes.iter().sum::<u64>());
}

#[test]
fn the_pieces_concatenated_are_the_archive() {
    let dir = TempDir::new("sz-v-join");
    let source = tree(&dir);

    let made = Archive::new(dir.join("split.7z")).set_volume_size(VOLUME).create_from([&source]).unwrap();

    let mut joined = Vec::new();
    for n in 1..=made.volumes {
        joined.extend_from_slice(&fs::read(dir.join(format!("split.7z.{n:03}"))).unwrap());
    }

    let whole = dir.join("whole.7z");
    fs::write(&whole, &joined).unwrap();

    let out = dir.join("out");
    Archive::new(&whole).extract_to(&out).expect("the concatenation is an ordinary archive");
    check(&out);
}

#[test]
fn a_volume_set_reads_back_from_any_of_its_names() {
    let dir = TempDir::new("sz-v-read");
    let source = tree(&dir);
    let archive = dir.join("split.7z");
    Archive::new(&archive).set_volume_size(VOLUME).create_from([&source]).unwrap();

    for named in [archive.clone(), dir.join("split.7z.001")] {
        let reader = SevenZReader::open(&named).unwrap_or_else(|e| panic!("{}: {e}", named.display()));
        assert!(reader.volumes().len() > 1, "{} should open the whole set", named.display());

        let names: Vec<String> = reader.header().files.iter().map(|f| f.name.clone()).collect();
        assert!(names.iter().any(|n| n == "src/noise.bin"), "{names:?}");

        let out = dir.join(format!("out-{}", named.file_name().unwrap().to_string_lossy()));
        extract_to(&named, &out, &ExtractOptions::default());
        check(&out);
    }
}

#[test]
fn a_volume_of_a_set_resolves_to_the_format_by_name_and_by_magic() {
    let dir = TempDir::new("sz-v-detect");
    let source = tree(&dir);
    Archive::new(dir.join("split.7z")).set_volume_size(VOLUME).create_from([&source]).unwrap();

    let first = dir.join("split.7z.001");
    assert_eq!(ArchiveType::from_extension(&first), Some(ArchiveType::SevenZ));
    assert_eq!(ArchiveType::from_extension(Path::new("a.7z.017")), Some(ArchiveType::SevenZ));
    assert_eq!(ArchiveType::from_extension(Path::new("a.zip.001")), None);

    let head = fs::read(&first).unwrap();
    assert_eq!(ArchiveType::from_magic(&head[..512]), Some(ArchiveType::SevenZ));

    let out = dir.join("out");
    Archive::new(&first).extract_to(&out).expect("extracting the set through the public API");
    check(&out);
}

#[test]
fn a_missing_volume_is_named_rather_than_read_as_a_short_archive() {
    let dir = TempDir::new("sz-v-gap");
    let source = tree(&dir);
    let made = Archive::new(dir.join("split.7z")).set_volume_size(VOLUME).create_from([&source]).unwrap();
    assert!(made.volumes >= 3, "this test needs a middle volume to remove");

    fs::remove_file(dir.join("split.7z.002")).unwrap();

    let error = SevenZReader::open(dir.join("split.7z.001")).err().expect("a set with a hole is not an archive");
    let text = error.to_string();
    assert!(text.contains("split.7z.002"), "the error should name the missing volume: {text}");
}

#[test]
fn a_truncated_volume_fails_the_extraction_rather_than_writing_what_it_holds() {
    let dir = TempDir::new("sz-v-truncated");
    let source = tree(&dir);
    let made = Archive::new(dir.join("split.7z")).set_volume_size(VOLUME).create_from([&source]).unwrap();
    assert!(made.volumes >= 3);

    let second = dir.join("split.7z.002");
    let kept = fs::read(&second).unwrap();
    fs::write(&second, &kept[..1_000]).unwrap();

    let out = dir.join("out");
    let result = sevenz::extract(&dir.join("split.7z.001"), &out, &ExtractOptions::default(), &Reporter::disabled());
    assert!(result.is_err(), "a set one volume short of its own bytes must not extract");
}

#[test]
fn volumes_past_the_nine_hundred_and_ninety_ninth_keep_being_named_in_order() {
    let base = Path::new("/tmp/archive.7z");

    assert_eq!(volumes::volume_name(base, 1), Path::new("/tmp/archive.7z.001"));
    assert_eq!(volumes::volume_name(base, 999), Path::new("/tmp/archive.7z.999"));
    assert_eq!(volumes::volume_name(base, 1_000), Path::new("/tmp/archive.7z.1000"));

    for index in [1usize, 999, 1_000, 12_345] {
        assert_eq!(volumes::base_name(&volumes::volume_name(base, index)), base, "volume {index}");
    }
    assert_eq!(volumes::base_name(base), base);
}

#[test]
fn a_volume_size_too_small_to_hold_the_signature_header_is_raised_to_one_that_can() {
    let dir = TempDir::new("sz-v-tiny");
    let source = tree(&dir);

    Archive::new(dir.join("tiny.7z")).set_volume_size(16).create_from([&source]).expect("create");

    let first = fs::metadata(dir.join("tiny.7z.001")).unwrap().len();
    assert!(first >= 32, "the signature header must fit in the first volume, not {first} bytes");

    let out = dir.join("out");
    extract_to(&dir.join("tiny.7z.001"), &out, &ExtractOptions::default());
    check(&out);
}

#[test]
fn an_archive_small_enough_for_one_volume_is_still_written_as_one() {
    let dir = TempDir::new("sz-v-one");
    dir.write("src/small.txt", b"barely anything");

    let made = Archive::new(dir.join("one.7z")).set_volume_size(VOLUME).create_from([dir.join("src")]).unwrap();

    assert_eq!(made.volumes, 1);
    assert!(dir.join("one.7z.001").exists());
    assert!(!dir.join("one.7z").exists());

    let out = dir.join("out");
    extract_to(&dir.join("one.7z.001"), &out, &ExtractOptions::default());
    assert_eq!(fs::read(out.join("src/small.txt")).unwrap(), b"barely anything");
}

#[test]
fn writing_a_shorter_set_where_a_longer_one_stood_leaves_no_stale_volumes() {
    let dir = TempDir::new("sz-v-stale");
    let source = tree(&dir);
    let archive = dir.join("set.7z");

    let long = Archive::new(&archive).set_volume_size(VOLUME).create_from([&source]).unwrap();
    assert!(long.volumes >= 3);

    fs::remove_file(dir.join("src/noise.bin")).unwrap();
    fs::remove_file(dir.join("src/prose.txt")).unwrap();
    let short = Archive::new(&archive).set_volume_size(VOLUME).create_from([&source]).unwrap();

    assert!(short.volumes < long.volumes);
    for n in short.volumes + 1..=long.volumes {
        assert!(!dir.join(format!("set.7z.{n:03}")).exists(), "volume {n} is left over from the longer set");
    }

    let out = dir.join("out");
    extract_to(&dir.join("set.7z.001"), &out, &ExtractOptions::default());
    assert_eq!(fs::read(out.join("src/nested/small.txt")).unwrap(), b"a few bytes");
}

#[test]
fn a_set_written_where_a_single_archive_stood_is_not_shadowed_by_it() {
    let dir = TempDir::new("sz-v-shadow");
    let source = tree(&dir);
    let archive = dir.join("both.7z");

    Archive::new(&archive).create_from([&source]).unwrap();
    assert!(archive.is_file());

    let made = Archive::new(&archive).set_volume_size(VOLUME).create_from([&source]).unwrap();
    assert!(made.volumes > 1);
    assert!(!archive.exists(), "the single archive that stood here would shadow the set");

    let reader = SevenZReader::open(&archive).unwrap();
    assert_eq!(reader.volumes().len(), made.volumes as usize);
}

#[test]
fn an_encrypted_volume_set_round_trips() {
    let dir = TempDir::new("sz-v-aes");
    let source = tree(&dir);

    let made = Archive::new(dir.join("secret.7z")).set_volume_size(VOLUME).set_password("across volumes").create_from([&source]).unwrap();
    assert!(made.volumes > 1);

    assert!(sevenz::entries(&dir.join("secret.7z.001"), None).is_err(), "the header is encrypted too");

    let out = dir.join("out");
    let options = ExtractOptions { password: Some(ttarchive::Password::from("across volumes")), ..ExtractOptions::default() };
    extract_to(&dir.join("secret.7z.001"), &out, &options);
    check(&out);
}

#[test]
fn one_file_comes_out_of_a_set_without_reading_all_of_it() {
    let dir = TempDir::new("sz-v-one-file");
    let source = tree(&dir);
    Archive::new(dir.join("split.7z")).set_volume_size(VOLUME).create_from([&source]).unwrap();

    let out = dir.join("out");
    let options = ExtractOptions { selection: vec!["src/nested/small.txt".into()], ..ExtractOptions::default() };
    let summary = extract_to(&dir.join("split.7z.001"), &out, &options);

    assert_eq!(summary.files, 1);
    assert_eq!(fs::read(out.join("src/nested/small.txt")).unwrap(), b"a few bytes");
    assert!(!out.join("src/noise.bin").exists());
}

#[test]
fn the_cli_reads_a_set_we_wrote() {
    if !have("7z") {
        return skip("7z");
    }

    let dir = TempDir::new("sz-v-cli-read");
    let source = tree(&dir);
    Archive::new(dir.join("split.7z")).set_volume_size(VOLUME).create_from([&source]).unwrap();

    let run = |args: &[&str]| {
        let output = Command::new(common::resolve("7z")).args(args).current_dir(dir.path()).output().unwrap_or_else(|e| panic!("failed to run 7z: {e}"));
        let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
        text.push_str(&String::from_utf8_lossy(&output.stderr));
        (output.status.success(), text)
    };

    let (ok, text) = run(&["t", "split.7z.001"]);
    assert!(ok, "7z t rejected our volume set:\n{text}");

    let (ok, text) = run(&["x", "-y", "-oout", "split.7z.001"]);
    assert!(ok, "7z x failed:\n{text}");
    check(&dir.join("out"));
}

#[test]
fn we_read_a_set_the_cli_wrote() {
    if !have("7z") {
        return skip("7z");
    }

    let dir = TempDir::new("sz-v-cli-write");
    tree(&dir);

    let output = Command::new(common::resolve("7z")).args(["a", "-v100k", "made.7z", "src"]).current_dir(dir.path()).output().expect("7z");
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    assert!(dir.join("made.7z.002").exists(), "the sample should need more than one volume");

    let listed = sevenz::entries(&dir.join("made.7z.001"), None).unwrap();
    assert!(listed.iter().any(|e| e.name == "src/noise.bin"), "{:?}", listed.iter().map(|e| &e.name).collect::<Vec<_>>());

    let out = dir.join("out");
    extract_to(&dir.join("made.7z.001"), &out, &ExtractOptions::default());
    check(&out);

    let by_base = dir.join("out-base");
    extract_to(&dir.join("made.7z"), &by_base, &ExtractOptions::default());
    check(&by_base);
}
