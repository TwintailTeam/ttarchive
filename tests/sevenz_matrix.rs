mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use common::{TempDir, compressible, have, pseudo_random};
use ttarchive::{Archive, Level, Method};

const READERS: [&str; 4] = ["7z", "7za", "7zr", "bsdtar"];

const LZMA_ONLY: [&str; 3] = ["7z", "7za", "bsdtar"];

fn run(dir: &Path, tool: &str, args: &[&str]) -> (bool, String) {
    let output = Command::new(common::resolve(tool)).args(args).current_dir(dir).output().unwrap_or_else(|e| panic!("failed to run {tool}: {e}"));
    let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&output.stderr));
    (output.status.success(), text)
}

fn volume_tree(dir: &TempDir) -> PathBuf {
    tree(dir);
    dir.write("src/incompressible.bin", pseudo_random(300_000, 13));
    dir.join("src")
}

fn tree(dir: &TempDir) -> PathBuf {
    dir.write("src/prose.txt", compressible(60_000));
    dir.write("src/noise.bin", pseudo_random(40_000, 12));
    dir.write("src/nested/deep/small.txt", b"a few bytes");
    dir.write("src/empty.txt", b"");
    dir.write("src/unicode-\u{00e9}\u{672c}.txt", "non-ascii name".as_bytes());
    dir.join("src")
}

fn check(out: &Path) {
    assert_eq!(fs::read(out.join("src/prose.txt")).unwrap(), compressible(60_000));
    assert_eq!(fs::read(out.join("src/noise.bin")).unwrap(), pseudo_random(40_000, 12));
    assert_eq!(fs::read(out.join("src/nested/deep/small.txt")).unwrap(), b"a few bytes");
    assert_eq!(fs::read(out.join("src/empty.txt")).unwrap(), b"");
    assert_eq!(fs::read(out.join("src/unicode-\u{00e9}\u{672c}.txt")).unwrap(), b"non-ascii name");
}

fn extract_with(dir: &TempDir, tool: &str, archive: &str, out: &str) -> bool {
    let (ok, text) = match tool {
        "bsdtar" => run(dir.path(), "bsdtar", &["-xf", archive, "-C", out]),
        _ => run(dir.path(), tool, &["x", "-y", &format!("-o{out}"), archive]),
    };
    assert!(ok, "{tool} failed to extract {archive}:\n{text}");
    ok
}

#[test]
fn every_reader_installed_accepts_what_we_write() {
    let dir = TempDir::new("mx-read-ours");
    let source = tree(&dir);

    let cases = [
        (Level::Default, None, &READERS[..]),
        (Level::None, None, &READERS[..]),
        (Level::Best, None, &READERS[..]),
        (Level::Default, Some(Method::Bzip2), &LZMA_ONLY[..]),
        (Level::Default, Some(Method::Deflate), &LZMA_ONLY[..]),
    ];

    for (level, method, readers) in cases {
        let name = format!("ours-{level:?}-{}.7z", method.map(|m| m.code()).unwrap_or(0));
        let mut archive = Archive::new(dir.join(&name)).set_level(level);
        if let Some(method) = method {
            archive = archive.set_method(method);
        }
        archive.create_from([&source]).unwrap_or_else(|e| panic!("{name}: {e}"));

        for tool in readers.iter().copied() {
            if !have(tool) {
                continue;
            }

            let out = format!("out-{tool}-{level:?}-{}", method.map(|m| m.code()).unwrap_or(0));
            fs::create_dir_all(dir.join(&out)).unwrap();
            extract_with(&dir, tool, &name, &out);
            check(&dir.join(&out));
        }
    }
}

#[test]
fn we_accept_what_every_writer_installed_produces() {
    let dir = TempDir::new("mx-write-theirs");
    tree(&dir);

    let mut made = 0usize;

    for (tool, args) in [
        ("7z", vec!["a", "theirs-7z.7z", "src"]),
        ("7za", vec!["a", "theirs-7za.7z", "src"]),
        ("7zr", vec!["a", "theirs-7zr.7z", "src"]),
        ("7z", vec!["a", "-m0=PPMd", "theirs-ppmd.7z", "src"]),
        ("7z", vec!["a", "-m0=BCJ2", "theirs-bcj2.7z", "src"]),
        ("7z", vec!["a", "-m0=BZip2", "theirs-bzip2.7z", "src"]),
        ("7z", vec!["a", "-m0=Deflate", "theirs-deflate.7z", "src"]),
        ("7z", vec!["a", "-mhe=on", "-psecret", "theirs-hidden.7z", "src"]),
        ("bsdtar", vec!["-a", "-cf", "theirs-bsdtar.7z", "src"]),
    ] {
        if !have(tool) {
            continue;
        }

        let (ok, text) = run(dir.path(), tool, &args);
        assert!(ok, "{tool} {args:?} failed:\n{text}");

        let name = args.iter().find(|a| a.ends_with(".7z")).expect("an archive name");
        let out = dir.join(format!("out-{name}"));

        let mut archive = Archive::new(dir.join(name));
        if args.iter().any(|a| a.starts_with("-p")) {
            archive = archive.set_password("secret");
        }
        archive.extract_to(&out).unwrap_or_else(|e| panic!("{name}: {e}"));

        check(&out);
        made += 1;
    }

    if made == 0 {
        eprintln!("skipping: no 7z writer is installed");
    }
}

#[test]
fn a_volume_set_we_wrote_is_read_by_every_reader_that_supports_one() {
    let dir = TempDir::new("mx-volumes");
    let source = volume_tree(&dir);

    let made = Archive::new(dir.join("split.7z")).set_volume_size(64 * 1024).create_from([&source]).unwrap();
    assert!(made.volumes > 1);

    for tool in ["7z", "7za", "7zr"] {
        if !have(tool) {
            continue;
        }

        let (ok, text) = run(dir.path(), tool, &["t", "split.7z.001"]);
        assert!(ok, "{tool} rejected our volume set:\n{text}");

        let out = format!("out-{tool}");
        let (ok, text) = run(dir.path(), tool, &["x", "-y", &format!("-o{out}"), "split.7z.001"]);
        assert!(ok, "{tool} failed to extract our volume set:\n{text}");
        check(&dir.join(&out));
    }
}

#[test]
fn a_volume_set_the_cli_wrote_reads_back_through_us() {
    if !have("7z") {
        return;
    }

    let dir = TempDir::new("mx-volumes-theirs");
    volume_tree(&dir);

    let (ok, text) = run(dir.path(), "7z", &["a", "-v64k", "theirs.7z", "src"]);
    assert!(ok, "{text}");
    assert!(dir.join("theirs.7z.002").exists(), "the corpus should need more than one volume");

    let out = dir.join("out");
    Archive::new(dir.join("theirs.7z.001")).extract_to(&out).expect("extract their set");
    check(&out);
}

#[test]
fn an_encrypted_archive_we_wrote_opens_in_every_reader_that_does_encryption() {
    let dir = TempDir::new("mx-aes");
    let source = tree(&dir);

    Archive::new(dir.join("secret.7z")).set_password("across tools").create_from([&source]).unwrap();

    for tool in ["7z", "7za"] {
        if !have(tool) {
            continue;
        }

        let (ok, text) = run(dir.path(), tool, &["t", "-pacross tools", "secret.7z"]);
        assert!(ok, "{tool} rejected our encrypted archive:\n{text}");

        let out = format!("out-{tool}");
        let (ok, text) = run(dir.path(), tool, &["x", "-y", "-pacross tools", &format!("-o{out}"), "secret.7z"]);
        assert!(ok, "{tool} failed to extract it:\n{text}");
        check(&dir.join(&out));
    }
}

#[test]
fn every_filter_we_can_write_is_read_back_by_the_cli() {
    if !have("7z") {
        return;
    }

    use ttarchive::sevenz::spec::Branch;
    use ttarchive::sevenz::writer::{Filter, Item, Options, SevenZWriter};

    let dir = TempDir::new("mx-filters");
    let code = common::machine_code();

    let filters = [
        Filter::Branch(Branch::X86),
        Filter::Branch(Branch::Ppc),
        Filter::Branch(Branch::Ia64),
        Filter::Branch(Branch::Arm),
        Filter::Branch(Branch::ArmThumb),
        Filter::Branch(Branch::Sparc),
        Filter::Branch(Branch::Arm64),
        Filter::Branch(Branch::RiscV),
        Filter::Delta(1),
        Filter::Delta(16),
    ];

    for (index, filter) in filters.into_iter().enumerate() {
        let name = format!("filter-{index}.7z");
        let options = Options { filter: Some(filter), ..Options::default() };

        let mut writer = SevenZWriter::new(fs::File::create(dir.join(&name)).unwrap(), options).unwrap();
        writer.add_file(&Item { name: "code.bin".into(), mtime: None, attributes: Some(0x20) }, &code).unwrap();
        writer.finish().unwrap();

        let (ok, text) = run(dir.path(), "7z", &["t", &name]);
        assert!(ok, "7z rejected {filter:?}:\n{text}");

        let out = format!("out-{index}");
        let (ok, text) = run(dir.path(), "7z", &["x", "-y", &format!("-o{out}"), &name]);
        assert!(ok, "7z failed to extract {filter:?}:\n{text}");
        assert_eq!(fs::read(dir.join(&out).join("code.bin")).unwrap(), code, "{filter:?}");
    }
}
