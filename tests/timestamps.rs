mod common;

use std::fs::{self, File, FileTimes};
use std::path::Path;
use std::process::Command;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use common::{TempDir, have};
use ttarchive::{Archive, ArchiveType, ExtractOptions};

const FILE_TIME: u64 = 1_000_000_000;
const OTHER_TIME: u64 = 1_300_000_000;
const DIR_TIME: u64 = 1_450_000_000;

fn at(seconds: u64) -> SystemTime {
    UNIX_EPOCH + Duration::from_secs(seconds)
}

fn set_mtime(path: &Path, seconds: u64) {
    let handle = if path.is_dir() { open_dir(path) } else { File::options().write(true).open(path).unwrap() };
    handle.set_times(FileTimes::new().set_modified(at(seconds)).set_accessed(at(seconds))).unwrap();
}

#[cfg(windows)]
fn open_dir(path: &Path) -> File {
    use std::os::windows::fs::OpenOptionsExt;
    File::options().write(true).custom_flags(0x0200_0000).open(path).unwrap()
}

#[cfg(not(windows))]
fn open_dir(path: &Path) -> File {
    File::open(path).unwrap()
}

fn mtime(path: &Path) -> u64 {
    fs::metadata(path).unwrap().modified().unwrap().duration_since(UNIX_EPOCH).unwrap().as_secs()
}

fn tree(dir: &TempDir) -> std::path::PathBuf {
    let first = dir.write("src/first.txt", b"first");
    let second = dir.write("src/nested/second.bin", common::pseudo_random(20_000, 4));
    set_mtime(&first, FILE_TIME);
    set_mtime(&second, OTHER_TIME);
    set_mtime(&dir.join("src/nested"), DIR_TIME);
    dir.join("src")
}

fn tolerance(kind: ArchiveType) -> u64 {
    if kind == ArchiveType::Zip { 2 } else { 0 }
}

fn assert_close(label: &str, found: u64, wanted: u64, slack: u64) {
    assert!(found.abs_diff(wanted) <= slack, "{label}: modified at {found}, expected {wanted} (within {slack}s)");
}

#[test]
fn every_writable_format_restores_file_and_directory_times() {
    for kind in ArchiveType::ALL.into_iter().filter(|k| k.can_write()) {
        let dir = TempDir::new(&format!("times-{}", kind.extension().trim_start_matches('.')));
        let source = tree(&dir);
        let archive = dir.join(format!("archive{}", kind.extension()));
        let label = kind.extension();

        Archive::new(&archive).set_type(kind).create_from([&source]).unwrap_or_else(|e| panic!("{label}: {e}"));

        let out = dir.join("out");
        Archive::new(&archive).set_type(kind).extract_to(&out).unwrap_or_else(|e| panic!("{label}: {e}"));

        let slack = tolerance(kind);
        assert_close(&format!("{label} first.txt"), mtime(&out.join("src/first.txt")), FILE_TIME, slack);
        assert_close(&format!("{label} nested/second.bin"), mtime(&out.join("src/nested/second.bin")), OTHER_TIME, slack);
        assert_close(&format!("{label} nested/"), mtime(&out.join("src/nested")), DIR_TIME, slack);
    }
}

#[test]
fn an_extracted_file_is_not_left_with_the_time_it_was_extracted() {
    let dir = TempDir::new("times-not-now");
    let source = tree(&dir);
    let archive = dir.join("archive.7z");
    Archive::new(&archive).create_from([&source]).unwrap();

    let before = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
    let out = dir.join("out");
    Archive::new(&archive).extract_to(&out).unwrap();

    let restored = mtime(&out.join("src/first.txt"));
    assert!(restored < before - 86_400, "first.txt was modified at {restored}, which is extraction time, not the archive's");
}

#[test]
fn a_read_only_file_still_gets_its_time() {
    let dir = TempDir::new("times-readonly");
    let file = dir.write("src/locked.txt", b"cannot write me");
    set_mtime(&file, FILE_TIME);
    let mut permissions = fs::metadata(&file).unwrap().permissions();
    permissions.set_readonly(true);
    fs::set_permissions(&file, permissions).unwrap();

    for name in ["locked.tar", "locked.zip", "locked.7z"] {
        let archive = dir.join(name);
        Archive::new(&archive).create_from([dir.join("src")]).unwrap();

        let out = dir.join(format!("out-{name}"));
        Archive::new(&archive).extract_to(&out).unwrap_or_else(|e| panic!("{name}: {e}"));

        let restored = out.join("src/locked.txt");
        assert!(fs::metadata(&restored).unwrap().permissions().readonly(), "{name}: the file should come back read-only");
        assert_close(name, mtime(&restored), FILE_TIME, if name.ends_with(".zip") { 2 } else { 0 });

        let mut permissions = fs::metadata(&restored).unwrap().permissions();
        permissions.set_readonly(false);
        fs::set_permissions(&restored, permissions).unwrap();
    }

    let mut permissions = fs::metadata(&file).unwrap().permissions();
    permissions.set_readonly(false);
    fs::set_permissions(&file, permissions).unwrap();
}

#[test]
fn times_are_left_alone_when_timestamps_are_not_being_restored() {
    let dir = TempDir::new("times-off");
    let source = tree(&dir);
    let archive = dir.join("archive.tar");
    Archive::new(&archive).create_from([&source]).unwrap();

    let before = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
    let out = dir.join("out");
    let options = ExtractOptions { preserve_timestamps: false, ..ExtractOptions::default() };
    Archive::new(&archive).with_extract_options(options).extract_to(&out).unwrap();

    assert!(mtime(&out.join("src/first.txt")) + 5 >= before, "with timestamps off, the file keeps the time it was written");
}

#[cfg(unix)]
#[test]
fn a_symbolic_link_does_not_pass_its_time_on_to_what_it_points_at() {
    let dir = TempDir::new("times-link");
    let target = dir.write("src/target.txt", b"pointed at");
    set_mtime(&target, FILE_TIME);
    std::os::unix::fs::symlink("target.txt", dir.join("src/link.txt")).unwrap();

    for name in ["links.tar", "links.zip", "links.7z"] {
        let archive = dir.join(name);
        Archive::new(&archive).create_from([dir.join("src")]).unwrap();

        let out = dir.join(format!("out-{name}"));
        Archive::new(&archive).extract_to(&out).unwrap();

        assert!(fs::symlink_metadata(out.join("src/link.txt")).unwrap().file_type().is_symlink(), "{name}");
        assert_close(name, mtime(&out.join("src/target.txt")), FILE_TIME, 0);
    }
}

#[test]
fn archives_written_by_other_tools_have_their_times_restored() {
    let dir = TempDir::new("times-tools");
    tree(&dir);

    let mut checked = 0;
    for (tool, name, args) in [
        ("tar", "theirs.tar", vec!["-cf", "theirs.tar", "src"]),
        ("zip", "theirs.zip", vec!["-q", "-r", "theirs.zip", "src"]),
        ("7z", "theirs.7z", vec!["a", "-bso0", "-bsp0", "theirs.7z", "src"]),
    ] {
        if !have(tool) {
            continue;
        }

        let status = Command::new(common::resolve(tool)).args(&args).current_dir(dir.path()).status().unwrap();
        assert!(status.success(), "{tool} failed");

        let out = dir.join(format!("out-{name}"));
        Archive::new(dir.join(name)).extract_to(&out).unwrap_or_else(|e| panic!("{name}: {e}"));

        let slack = if name.ends_with(".zip") { 2 } else { 0 };
        assert_close(&format!("{name} file"), mtime(&out.join("src/first.txt")), FILE_TIME, slack);
        assert_close(&format!("{name} directory"), mtime(&out.join("src/nested")), DIR_TIME, slack);
        checked += 1;
    }

    if checked == 0 {
        eprintln!("skipping: none of tar, zip or 7z is installed");
    }
}
