mod common;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use common::{TempDir, compressible, pseudo_random};
use ttarchive::{Archive, ArchiveType};

const WRITABLE: [(ArchiveType, &str); 7] = [
    (ArchiveType::Tar, "w.tar"),
    (ArchiveType::TarGz, "w.tar.gz"),
    (ArchiveType::TarBz2, "w.tar.bz2"),
    (ArchiveType::TarXz, "w.tar.xz"),
    (ArchiveType::TarZst, "w.tar.zst"),
    (ArchiveType::TarLzma, "w.tar.lzma"),
    (ArchiveType::TarLz, "w.tar.lz"),
];

fn without_lzip() -> impl Iterator<Item = (ArchiveType, &'static str)> {
    WRITABLE.into_iter().filter(|(kind, _)| *kind != ArchiveType::TarLz)
}

fn have(tool: &str) -> bool {
    Command::new("sh").arg("-c").arg(format!("command -v {tool}")).stdout(Stdio::null()).stderr(Stdio::null()).status().map(|s| s.success()).unwrap_or(false)
}

fn skip(tool: &str) {
    eprintln!("skipping: {tool} is not installed");
}

fn run(dir: &Path, program: &str, args: &[&str]) -> (bool, String) {
    let output = Command::new(program).args(args).current_dir(dir).output().unwrap_or_else(|e| panic!("failed to run {program}: {e}"));

    let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&output.stderr));
    (output.status.success(), text)
}

fn must_run(dir: &Path, program: &str, args: &[&str]) -> String {
    let (ok, text) = run(dir, program, args);
    assert!(ok, "{program} {args:?} failed:\n{text}");
    text
}

fn snapshot(root: &Path) -> BTreeMap<String, (u64, u32)> {
    let mut out = BTreeMap::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            let path = entry.path();
            let rel = path.strip_prefix(root).unwrap().to_string_lossy().replace('\\', "/");
            let Ok(md) = std::fs::symlink_metadata(&path) else { continue };
            if md.is_dir() {
                stack.push(path);
            } else if !md.is_symlink() {
                let bytes = std::fs::read(&path).unwrap_or_default();
                out.insert(rel, (bytes.len() as u64, ttarchive::utils::crc32::checksum(&bytes)));
            }
        }
    }
    out
}

fn source(dir: &TempDir) -> PathBuf {
    dir.write("src/a.txt", compressible(30_000));
    dir.write("src/nested/b.bin", pseudo_random(20_000, 11));
    dir.write("src/nested/deep/leaf.txt", b"leaf contents");
    dir.write("src/empty.txt", b"");
    dir.write("src/unicode-\u{00e9}\u{00fc}.txt", "non-ascii name");
    dir.join("src")
}

fn only_file(dir: &Path) -> PathBuf {
    let mut found: Vec<PathBuf> = std::fs::read_dir(dir).expect("read output dir").flatten().map(|e| e.path()).collect();
    assert_eq!(found.len(), 1, "expected one file in {}, found {found:?}", dir.display());
    found.pop().unwrap()
}

#[test]
fn bsdtar_lists_every_wrapper_we_write() {
    if !have("bsdtar") {
        return skip("bsdtar");
    }

    let dir = TempDir::new("ti-bsd-list");
    let src = source(&dir);

    for (kind, name) in WRITABLE {
        Archive::new(dir.join(name)).set_type(kind).create_from([&src]).unwrap_or_else(|e| panic!("{name}: {e}"));

        let listing = must_run(dir.path(), "bsdtar", &["-tf", name]);
        assert!(listing.contains("src/nested/deep/leaf.txt"), "{name}: {listing}");
        assert!(listing.contains("src/unicode-\u{00e9}\u{00fc}.txt"), "{name}: {listing}");
    }
}

#[test]
fn bsdtar_extracts_our_archives_byte_for_byte() {
    if !have("bsdtar") {
        return skip("bsdtar");
    }

    let dir = TempDir::new("ti-bsd-extract");
    let src = source(&dir);
    let expected = snapshot(&src);

    for (kind, name) in WRITABLE {
        Archive::new(dir.join(name)).set_type(kind).create_from([&src]).unwrap_or_else(|e| panic!("{name}: {e}"));

        let dest = dir.join(format!("{name}-out"));
        std::fs::create_dir_all(&dest).unwrap();
        must_run(dir.path(), "bsdtar", &["-xf", name, "-C", dest.to_str().unwrap()]);

        assert_eq!(snapshot(&dest.join("src")), expected, "{name}: bsdtar produced different contents");
    }
}

#[test]
fn gnu_tar_extracts_every_wrapper_we_write_byte_for_byte() {
    if !have("tar") {
        return skip("tar");
    }

    let dir = TempDir::new("ti-gnu-extract");
    let src = source(&dir);
    let expected = snapshot(&src);

    for (kind, name) in WRITABLE {
        Archive::new(dir.join(name)).set_type(kind).create_from([&src]).unwrap_or_else(|e| panic!("{name}: {e}"));

        let dest = dir.join(format!("{name}-out"));
        std::fs::create_dir_all(&dest).unwrap();
        must_run(dir.path(), "tar", &["-xf", name, "-C", dest.to_str().unwrap()]);

        assert_eq!(snapshot(&dest.join("src")), expected, "{name}: GNU tar produced different contents");
    }
}

#[test]
fn we_extract_every_wrapper_bsdtar_writes() {
    if !have("bsdtar") {
        return skip("bsdtar");
    }

    let dir = TempDir::new("ti-read-bsd");
    let src = source(&dir);
    let expected = snapshot(&src);

    let matrix: [(ArchiveType, &str, &str); 7] = [
        (ArchiveType::Tar, "b.tar", ""),
        (ArchiveType::TarGz, "b.tar.gz", "--gzip"),
        (ArchiveType::TarBz2, "b.tar.bz2", "--bzip2"),
        (ArchiveType::TarXz, "b.tar.xz", "--xz"),
        (ArchiveType::TarZst, "b.tar.zst", "--zstd"),
        (ArchiveType::TarLzma, "b.tar.lzma", "--lzma"),
        (ArchiveType::TarLz, "b.tar.lz", "--lzip"),
    ];

    for (kind, name, flag) in matrix {
        let mut args = vec!["-cf", name];
        if !flag.is_empty() {
            args.push(flag);
        }
        args.extend(["-C", src.to_str().unwrap(), "."]);
        must_run(dir.path(), "bsdtar", &args);

        let dest = dir.join(format!("{name}-out"));
        Archive::new(dir.join(name)).set_type(kind).extract_to(&dest).unwrap_or_else(|e| panic!("{name}: {e}"));

        assert_eq!(snapshot(&dest), expected, "{name}: we read bsdtar's bytes differently");
    }
}

#[test]
fn we_extract_every_header_format_gnu_tar_writes() {
    if !have("tar") {
        return skip("tar");
    }

    let dir = TempDir::new("ti-gnu-formats");
    let src = source(&dir);
    let expected = snapshot(&src);

    for format in ["v7", "oldgnu", "gnu", "ustar", "pax", "posix"] {
        let name = format!("g-{format}.tar");
        must_run(dir.path(), "tar", &["-cf", &name, &format!("--format={format}"), "-C", src.to_str().unwrap(), "."]);

        let dest = dir.join(format!("{name}-out"));
        Archive::new(dir.join(&name)).extract_to(&dest).unwrap_or_else(|e| panic!("{format}: {e}"));

        assert_eq!(snapshot(&dest), expected, "{format}: we read this header format differently");
    }
}

#[test]
fn long_names_and_long_link_targets_survive_both_tools() {
    if !have("tar") || !have("bsdtar") || !cfg!(unix) {
        return skip("GNU tar and bsdtar on unix");
    }

    let dir = TempDir::new("ti-longnames");
    let deep = format!("{}/{}", "a".repeat(120), "b".repeat(120));
    dir.write(format!("src/{deep}"), b"a name no ustar header can hold");
    let src = dir.join("src");

    #[cfg(unix)]
    std::os::unix::fs::symlink(&deep, dir.join("src/link.txt")).expect("symlink");

    let cases: [(&str, &str, &[&str]); 3] = [("gnu.tar", "tar", &["--format=gnu"]), ("pax.tar", "tar", &["--format=pax"]), ("bsd.tar", "bsdtar", &[])];

    for (name, tool, extra) in cases {
        let mut args = vec!["-cf", name];
        args.extend_from_slice(extra);
        args.extend(["-C", src.to_str().unwrap(), "."]);
        must_run(dir.path(), tool, &args);

        let dest = dir.join(format!("{name}-out"));
        Archive::new(dir.join(name)).extract_to(&dest).unwrap_or_else(|e| panic!("{name}: {e}"));

        let body = std::fs::read(dest.join(&deep)).unwrap_or_else(|e| panic!("{name}: long name lost: {e}"));
        assert_eq!(body, b"a name no ustar header can hold", "{name}: long-named file has the wrong contents");

        let target = std::fs::read_link(dest.join("link.txt")).unwrap_or_else(|e| panic!("{name}: long link target lost: {e}"));
        assert_eq!(target.to_string_lossy(), deep, "{name}: long link target truncated");
    }
}

#[test]
fn p7zip_accepts_every_wrapper_we_write() {
    if !have("7z") {
        return skip("7z");
    }

    let dir = TempDir::new("ti-7z-test");
    let src = source(&dir);

    for (kind, name) in without_lzip() {
        Archive::new(dir.join(name)).set_type(kind).create_from([&src]).unwrap_or_else(|e| panic!("{name}: {e}"));

        let out = must_run(dir.path(), "7z", &["t", name]);
        assert!(out.contains("Everything is Ok"), "{name}: 7z t said:\n{out}");
    }
}

#[test]
fn p7zip_extracts_our_tarballs_in_two_passes() {
    if !have("7z") {
        return skip("7z");
    }

    let dir = TempDir::new("ti-7z-extract");
    let src = source(&dir);
    let expected = snapshot(&src);

    for (kind, name) in without_lzip() {
        Archive::new(dir.join(name)).set_type(kind).create_from([&src]).unwrap_or_else(|e| panic!("{name}: {e}"));

        let tar = if kind == ArchiveType::Tar {
            dir.join(name)
        } else {
            let unwrapped = dir.join(format!("{name}-unwrapped"));
            std::fs::create_dir_all(&unwrapped).unwrap();
            must_run(dir.path(), "7z", &["x", name, &format!("-o{}", unwrapped.display())]);
            only_file(&unwrapped)
        };

        let dest = dir.join(format!("{name}-out"));
        must_run(dir.path(), "7z", &["x", tar.to_str().unwrap(), &format!("-o{}", dest.display())]);

        assert_eq!(snapshot(&dest.join("src")), expected, "{name}: 7-Zip produced different contents");
    }
}

#[test]
fn we_extract_what_p7zip_writes() {
    if !have("7z") {
        return skip("7z");
    }

    let dir = TempDir::new("ti-read-7z");
    let src = source(&dir);
    let expected = snapshot(&src);

    must_run(dir.path(), "7z", &["a", "-ttar", "s.tar", src.to_str().unwrap()]);

    for (kind, wrap, name) in
        [(ArchiveType::TarGz, "-tgzip", "s.tar.gz"), (ArchiveType::TarBz2, "-tbzip2", "s.tar.bz2"), (ArchiveType::TarXz, "-txz", "s.tar.xz")]
    {
        must_run(dir.path(), "7z", &["a", wrap, name, "s.tar"]);

        let dest = dir.join(format!("{name}-out"));
        Archive::new(dir.join(name)).set_type(kind).extract_to(&dest).unwrap_or_else(|e| panic!("{name}: {e}"));

        assert_eq!(snapshot(&dest.join("src")), expected, "{name}: we read 7-Zip's bytes differently");
    }

    let dest = dir.join("s.tar-out");
    Archive::new(dir.join("s.tar")).extract_to(&dest).expect("extract 7-Zip's tar");
    assert_eq!(snapshot(&dest.join("src")), expected, "we read 7-Zip's tar differently");
}

#[test]
fn a_stream_the_tools_wrote_in_several_members_is_read_whole() {
    if !have("tar") {
        return skip("tar");
    }

    let dir = TempDir::new("ti-multimember");
    let src = source(&dir);
    let expected = snapshot(&src);

    must_run(dir.path(), "tar", &["-cf", "plain.tar", "-C", src.to_str().unwrap(), "."]);

    let whole = std::fs::read(dir.join("plain.tar")).unwrap();
    let split = (whole.len() / 2) / 512 * 512;
    assert!(split > 0 && split < whole.len(), "the sample tar is too small to split");
    std::fs::write(dir.join("p1"), &whole[..split]).unwrap();
    std::fs::write(dir.join("p2"), &whole[split..]).unwrap();

    for (kind, tool, ext) in
        [(ArchiveType::TarGz, "gzip", "gz"), (ArchiveType::TarBz2, "bzip2", "bz2"), (ArchiveType::TarXz, "xz", "xz"), (ArchiveType::TarZst, "zstd", "zst")]
    {
        if !have(tool) {
            skip(tool);
            continue;
        }

        let name = format!("multi.tar.{ext}");
        must_run(dir.path(), "sh", &["-c", &format!("{tool} -c -q p1 > m1 && {tool} -c -q p2 > m2 && cat m1 m2 > {name}")]);

        let dest = dir.join(format!("{name}-out"));
        Archive::new(dir.join(&name)).set_type(kind).extract_to(&dest).unwrap_or_else(|e| panic!("{name}: {e}"));

        assert_eq!(snapshot(&dest), expected, "{name}: a member after the first was dropped");
    }
}

#[test]
fn we_extract_the_wrapper_we_cannot_write_yet() {
    if !have("tar") {
        return skip("tar");
    }

    let dir = TempDir::new("ti-readonly");
    let src = source(&dir);
    let expected = snapshot(&src);

    for (kind, tool, flag, name) in [(ArchiveType::TarZ, "compress", "-Z", "r.tar.Z")] {
        if !have(tool) {
            skip(tool);
            continue;
        }

        must_run(dir.path(), "tar", &["-cf", name, flag, "-C", src.to_str().unwrap(), "."]);

        let dest = dir.join(format!("{name}-out"));
        Archive::new(dir.join(name)).set_type(kind).extract_to(&dest).unwrap_or_else(|e| panic!("{name}: {e}"));

        assert_eq!(snapshot(&dest), expected, "{name}: we read this wrapper differently");
    }
}

#[cfg(unix)]
#[test]
fn permission_bits_survive_a_round_trip_through_each_tool() {
    use std::os::unix::fs::PermissionsExt;

    if !have("tar") || !have("bsdtar") {
        return skip("GNU tar and bsdtar");
    }

    let dir = TempDir::new("ti-modes");
    let src = source(&dir);
    std::fs::set_permissions(src.join("a.txt"), std::fs::Permissions::from_mode(0o600)).unwrap();
    std::fs::set_permissions(src.join("nested"), std::fs::Permissions::from_mode(0o750)).unwrap();

    for tool in ["tar", "bsdtar"] {
        let name = format!("{tool}.tar");
        Archive::new(dir.join(&name)).create_from([&src]).unwrap();

        let dest = dir.join(format!("{tool}-out"));
        std::fs::create_dir_all(&dest).unwrap();
        must_run(dir.path(), tool, &["-xpf", &name, "-C", dest.to_str().unwrap()]);

        let file = std::fs::metadata(dest.join("src/a.txt")).unwrap().permissions().mode() & 0o777;
        let nested = std::fs::metadata(dest.join("src/nested")).unwrap().permissions().mode() & 0o777;
        assert_eq!(file, 0o600, "{tool}: file mode changed");
        assert_eq!(nested, 0o750, "{tool}: directory mode changed");
    }
}

#[cfg(unix)]
#[test]
fn we_restore_the_modes_the_tools_recorded() {
    use std::os::unix::fs::PermissionsExt;

    if !have("tar") || !have("bsdtar") {
        return skip("GNU tar and bsdtar");
    }

    let dir = TempDir::new("ti-read-modes");
    let src = source(&dir);
    std::fs::set_permissions(src.join("a.txt"), std::fs::Permissions::from_mode(0o604)).unwrap();
    std::fs::set_permissions(src.join("nested"), std::fs::Permissions::from_mode(0o701)).unwrap();

    for tool in ["tar", "bsdtar"] {
        let name = format!("m-{tool}.tar");
        must_run(dir.path(), tool, &["-cf", &name, "-C", src.to_str().unwrap(), "."]);

        let dest = dir.join(format!("m-{tool}-out"));
        Archive::new(dir.join(&name)).extract_to(&dest).unwrap_or_else(|e| panic!("{tool}: {e}"));

        let file = std::fs::metadata(dest.join("a.txt")).unwrap().permissions().mode() & 0o777;
        let nested = std::fs::metadata(dest.join("nested")).unwrap().permissions().mode() & 0o777;
        assert_eq!(file, 0o604, "{tool}: we lost the file mode");
        assert_eq!(nested, 0o701, "{tool}: we lost the directory mode");
    }
}

#[cfg(unix)]
#[test]
fn directory_modes_follow_the_paths_stripping_the_root_produced() {
    use std::os::unix::fs::PermissionsExt;

    if !have("tar") {
        return skip("tar");
    }

    let dir = TempDir::new("ti-strip-modes");
    let src = source(&dir);
    std::fs::set_permissions(src.join("nested"), std::fs::Permissions::from_mode(0o701)).unwrap();
    std::fs::set_permissions(src.join("nested/deep"), std::fs::Permissions::from_mode(0o750)).unwrap();

    must_run(dir.path(), "tar", &["-cf", "s.tar", "-C", dir.path().to_str().unwrap(), "src"]);

    let dest = dir.join("stripped");
    Archive::new(dir.join("s.tar")).set_strip_root(true).extract_to(&dest).expect("extract with the root stripped");

    assert!(dest.join("nested/deep/leaf.txt").exists(), "the root was not stripped");
    assert_eq!(std::fs::metadata(dest.join("nested")).unwrap().permissions().mode() & 0o777, 0o701, "mode lost on the stripped path");
    assert_eq!(std::fs::metadata(dest.join("nested/deep")).unwrap().permissions().mode() & 0o777, 0o750, "mode lost on a nested stripped path");
}

#[test]
fn bsdtar_reads_the_lzip_tarballs_we_write() {
    if !have("bsdtar") {
        return skip("bsdtar");
    }

    let dir = TempDir::new("ti-lzip");
    let src = source(&dir);
    let expected = snapshot(&src);

    Archive::new(dir.join("l.tar.lz")).set_type(ArchiveType::TarLz).create_from([&src]).expect("write .tar.lz");

    let listing = must_run(dir.path(), "bsdtar", &["-tf", "l.tar.lz"]);
    assert!(listing.contains("src/nested/deep/leaf.txt"), "{listing}");

    let dest = dir.join("l-out");
    std::fs::create_dir_all(&dest).unwrap();
    must_run(dir.path(), "bsdtar", &["-xf", "l.tar.lz", "-C", dest.to_str().unwrap()]);

    assert_eq!(snapshot(&dest.join("src")), expected, "bsdtar read our lzip stream differently");
}

#[test]
fn both_tools_count_the_same_entries_we_do() {
    if !have("tar") || !have("bsdtar") {
        return skip("GNU tar and bsdtar");
    }

    let dir = TempDir::new("ti-counts");
    let src = source(&dir);

    let archive = dir.join("count.tar.gz");
    Archive::new(&archive).create_from([&src]).unwrap();

    let ours = Archive::new(&archive).entries().unwrap().len();

    for tool in ["tar", "bsdtar"] {
        let listing = must_run(dir.path(), tool, &["-tf", "count.tar.gz"]);
        let theirs = listing.lines().filter(|line| !line.trim().is_empty()).count();
        assert_eq!(theirs, ours, "{tool} sees {theirs} entries where we see {ours}:\n{listing}");
    }
}
