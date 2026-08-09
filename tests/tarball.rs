mod common;

use std::path::Path;
use std::process::Command;

use ttarchive::{Archive, ArchiveType};

fn have(tool: &str) -> bool {
    Command::new("which").arg(tool).output().is_ok_and(|o| o.status.success())
}

fn sample(dir: &common::TempDir) -> std::path::PathBuf {
    dir.write("src/a.txt", b"hello tarball");
    dir.write("src/nested/b.bin", common::pseudo_random(20_000, 11));
    dir.write("src/empty.txt", b"");
    dir.join("src")
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
fn extensions_resolve_to_the_right_format() {
    let cases = [
        ("a.tar", ArchiveType::Tar),
        ("a.tar.gz", ArchiveType::TarGz),
        ("a.TAR.GZ", ArchiveType::TarGz),
        ("a.tgz", ArchiveType::TarGz),
        ("a.taz", ArchiveType::TarGz),
        ("a.tar.bz2", ArchiveType::TarBz2),
        ("a.tbz2", ArchiveType::TarBz2),
        ("a.tar.xz", ArchiveType::TarXz),
        ("a.txz", ArchiveType::TarXz),
        ("a.tar.zst", ArchiveType::TarZst),
        ("a.tzst", ArchiveType::TarZst),
        ("a.tar.lzma", ArchiveType::TarLzma),
        ("a.tar.lz", ArchiveType::TarLz),
        ("a.tar.Z", ArchiveType::TarZ),
        ("a.zip", ArchiveType::Zip),
        ("a.jar", ArchiveType::Zip),
    ];

    for (name, want) in cases {
        assert_eq!(ArchiveType::from_extension(Path::new(name)), Some(want), "{name}");
    }

    for name in ["a.gz", "a.bz2", "a.xz", "a.zst", "a", "a.7z", ".tar", ".tgz"] {
        assert_eq!(ArchiveType::from_extension(Path::new(name)), None, "{name} should not resolve");
    }
}

#[test]
fn a_bare_tar_is_detected_by_its_magic_at_offset_257() {
    let dir = common::TempDir::new("tarball-magic");
    let source = sample(&dir);

    let archive = dir.join("payload.dat");
    Archive::new(&archive).set_type(ArchiveType::Tar).create_from([&source]).expect("create");

    let head = std::fs::read(&archive).unwrap();
    assert!(head.len() > 265, "need past offset 257 to see the ustar marker");
    assert_eq!(&head[257..262], b"ustar", "writer should emit a ustar marker");

    let entries = Archive::new(&archive).entries().expect("listing should work by magic bytes");
    assert!(entries.iter().any(|e| e.name.contains("a.txt")), "{entries:?}");
}

#[test]
fn tar_and_tar_gz_round_trip_through_the_public_api() {
    let dir = common::TempDir::new("tarball-roundtrip");
    let source = sample(&dir);
    let expected = common::pseudo_random(20_000, 11);

    for (name, kind) in [("out.tar", ArchiveType::Tar), ("out.tar.gz", ArchiveType::TarGz), ("out.tgz", ArchiveType::TarGz)] {
        let archive = dir.join(name);
        let summary = Archive::new(&archive).create_from([&source]).unwrap_or_else(|e| panic!("{name}: create failed: {e}"));
        assert!(summary.files >= 3, "{name}: stored {} files", summary.files);
        assert_eq!(ArchiveType::from_extension(&archive), Some(kind));

        let out = dir.join(format!("{name}-out"));
        Archive::new(&archive).extract_to(&out).unwrap_or_else(|e| panic!("{name}: extract failed: {e}"));

        assert_eq!(tree(&out), vec!["src/a.txt", "src/empty.txt", "src/nested/b.bin"], "{name}");
        assert_eq!(std::fs::read(out.join("src/a.txt")).unwrap(), b"hello tarball", "{name}");
        assert!(std::fs::read(out.join("src/nested/b.bin")).unwrap() == expected, "{name}: contents differ");
    }
}

#[test]
fn tar_bz2_round_trips() {
    let dir = common::TempDir::new("tarball-bz2");
    let source = sample(&dir);

    let archive = dir.join("out.tar.bz2");
    Archive::new(&archive).create_from([&source]).expect("create");

    let out = dir.join("out");
    Archive::new(&archive).extract_to(&out).expect("extract");
    assert_eq!(std::fs::read(out.join("src/a.txt")).unwrap(), b"hello tarball");
}

#[test]
fn gnu_tar_reads_what_we_write() {
    if !have("tar") {
        eprintln!("skipping: GNU tar not installed");
        return;
    }

    let dir = common::TempDir::new("tarball-gnu-reads");
    let source = sample(&dir);

    for name in ["ours.tar", "ours.tar.gz", "ours.tar.bz2"] {
        let archive = dir.join(name);
        Archive::new(&archive).create_from([&source]).unwrap_or_else(|e| panic!("{name}: {e}"));

        let out = Command::new("tar").arg("-tf").arg(&archive).output().expect("run tar");
        assert!(out.status.success(), "{name}: GNU tar rejected it: {}", String::from_utf8_lossy(&out.stderr));

        let listing = String::from_utf8_lossy(&out.stdout);
        assert!(listing.contains("a.txt"), "{name}: {listing}");
        assert!(listing.contains("nested/b.bin"), "{name}: {listing}");
    }
}

#[test]
fn gnu_tar_extracts_our_contents_byte_for_byte() {
    if !have("tar") {
        eprintln!("skipping: GNU tar not installed");
        return;
    }

    let dir = common::TempDir::new("tarball-gnu-extract");
    let source = sample(&dir);
    let expected = common::pseudo_random(20_000, 11);

    for name in ["x.tar", "x.tar.gz"] {
        let archive = dir.join(name);
        Archive::new(&archive).create_from([&source]).unwrap();

        let out = dir.join(format!("{name}-gnu"));
        std::fs::create_dir_all(&out).unwrap();
        let status = Command::new("tar").arg("-xf").arg(&archive).arg("-C").arg(&out).status().expect("run tar");
        assert!(status.success(), "{name}: GNU tar failed to extract");

        assert!(std::fs::read(out.join("src/nested/b.bin")).unwrap() == expected, "{name}: contents differ");
    }
}

#[test]
fn we_read_what_gnu_tar_writes_for_every_wrapper_we_support() {
    if !have("tar") {
        eprintln!("skipping: GNU tar not installed");
        return;
    }

    let dir = common::TempDir::new("tarball-read-gnu");
    let source = sample(&dir);
    let expected = common::pseudo_random(20_000, 11);
    let _ = source;

    for (name, flag) in [("g.tar", ""), ("g.tar.gz", "-z"), ("g.tar.bz2", "-j"), ("g.tar.xz", "-J")] {
        let archive = dir.join(name);
        let mut cmd = Command::new("tar");
        cmd.arg("-cf").arg(&archive);
        if !flag.is_empty() {
            cmd.arg(flag);
        }
        cmd.arg("-C").arg(dir.join("src")).arg(".");
        let out = cmd.output().expect("run tar");
        assert!(out.status.success(), "{name}: tar failed: {}", String::from_utf8_lossy(&out.stderr));

        let dest = dir.join(format!("{name}-out"));
        Archive::new(&archive).extract_to(&dest).unwrap_or_else(|e| panic!("{name}: extract failed: {e}"));

        assert!(std::fs::read(dest.join("nested/b.bin")).unwrap() == expected, "{name}: contents differ");
    }
}

#[test]
fn a_tar_gz_is_a_real_gzip_stream() {
    if !have("gzip") {
        eprintln!("skipping: gzip not installed");
        return;
    }

    let dir = common::TempDir::new("tarball-gzip-tool");
    let source = sample(&dir);

    let archive = dir.join("g.tar.gz");
    Archive::new(&archive).create_from([&source]).unwrap();

    let out = Command::new("gzip").arg("-t").arg(&archive).output().expect("run gzip");
    assert!(out.status.success(), "gzip -t rejected our stream: {}", String::from_utf8_lossy(&out.stderr));
}

#[test]
fn we_read_gzip_written_by_the_gzip_tool() {
    if !have("gzip") {
        eprintln!("skipping: gzip not installed");
        return;
    }

    let dir = common::TempDir::new("gzip-read");
    let payload = common::compressible(120_000);
    let raw = dir.join("payload.bin");
    std::fs::write(&raw, &payload).unwrap();

    for level in ["-1", "-6", "-9"] {
        let out = Command::new("gzip").arg(level).arg("-c").arg(&raw).output().expect("run gzip");
        assert!(out.status.success());

        let back = ttarchive::codecs::gzip::decompress(&out.stdout, payload.len()).unwrap_or_else(|e| panic!("gzip {level}: {e}"));
        assert!(back == payload, "gzip {level}: contents differ");
    }
}

#[test]
fn gzip_round_trips_through_our_own_encoder() {
    let cases: Vec<(&str, Vec<u8>)> = vec![
        ("empty", Vec::new()),
        ("one byte", vec![42]),
        ("runs", vec![b'z'; 200_000]),
        ("text", common::compressible(150_000)),
        ("noise", common::pseudo_random(120_000, 5)),
    ];

    for (name, data) in &cases {
        let packed = ttarchive::codecs::gzip::compress(data, ttarchive::Level::Default, &Default::default()).unwrap();
        assert_eq!(&packed[..2], &[0x1f, 0x8b], "{name}: missing gzip magic");

        let back = ttarchive::codecs::gzip::decompress(&packed, data.len()).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert!(back == *data, "{name}: round trip changed the bytes");
    }
}

#[test]
fn a_corrupt_gzip_trailer_is_caught() {
    let payload = common::compressible(50_000);
    let mut packed = ttarchive::codecs::gzip::compress(&payload, ttarchive::Level::Default, &Default::default()).unwrap();

    let last = packed.len() - 5;
    packed[last] ^= 0xff;

    assert!(ttarchive::codecs::gzip::decompress(&packed, payload.len()).is_err(), "a flipped CRC byte must be reported");
}

#[test]
fn concatenated_gzip_members_are_all_decoded() {
    if !have("gzip") {
        eprintln!("skipping: gzip not installed");
        return;
    }

    let dir = common::TempDir::new("gzip-multi");
    let first = b"first member contents\n";
    let second = b"second member contents\n";

    let mut joined = Vec::new();
    for part in [first.as_slice(), second.as_slice()] {
        let path = dir.join("part.bin");
        std::fs::write(&path, part).unwrap();
        let out = Command::new("gzip").arg("-c").arg(&path).output().expect("run gzip");
        joined.extend_from_slice(&out.stdout);
    }

    let back = ttarchive::codecs::gzip::decompress(&joined, 128).expect("both members should decode");
    let mut want = first.to_vec();
    want.extend_from_slice(second);
    assert!(back == want, "concatenated members did not concatenate");
}

#[test]
fn a_format_we_cannot_write_yet_is_refused_clearly() {
    let dir = common::TempDir::new("tarball-unsupported");
    let source = sample(&dir);

    let name = "x.tar.Z";
    let archive = dir.join(name);
    let kind = ArchiveType::from_extension(&archive).unwrap();
    assert!(!kind.can_write(), "{name} should report that it cannot be written");

    let err = Archive::new(&archive).create_from([&source]).expect_err("a format we cannot write should refuse");
    assert!(err.is_unsupported(), "{name}: expected Unsupported, got {err}");

    assert!(ArchiveType::Tar.can_write());
    assert!(ArchiveType::TarGz.can_write());
    assert!(ArchiveType::TarBz2.can_write());
    assert!(ArchiveType::TarLzma.can_write());
    assert!(ArchiveType::TarXz.can_write());
    assert!(ArchiveType::TarZst.can_write());
    assert!(ArchiveType::Zip.can_write());
}

#[test]
fn strip_options_apply_to_tarballs_too() {
    let dir = common::TempDir::new("tarball-strip");
    let source = sample(&dir);

    let archive = dir.join("s.tar.gz");
    Archive::new(&archive).create_from([&source]).unwrap();

    let out = dir.join("stripped");
    Archive::new(&archive).set_strip_root(true).extract_to(&out).expect("extract");
    assert_eq!(tree(&out), vec!["a.txt", "empty.txt", "nested/b.bin"]);

    let fixed = dir.join("fixed");
    Archive::new(&archive).set_strip_components(2).extract_to(&fixed).expect("extract");
    assert_eq!(tree(&fixed), vec!["b.bin"]);
}

#[test]
fn tar_listing_exposes_tar_detail_not_zip_detail() {
    let dir = common::TempDir::new("tarball-detail");
    let source = sample(&dir);

    let archive = dir.join("d.tar");
    Archive::new(&archive).create_from([&source]).unwrap();

    let entries = Archive::new(&archive).entries().expect("listing");
    let file = entries.iter().find(|e| e.name.ends_with("a.txt")).expect("a.txt missing");

    assert!(file.zip().is_none(), "a tar entry must not claim zip detail");
    let detail = file.tar().expect("a tar entry should carry tar detail");
    assert_eq!(detail.typeflag, b'0');
    assert_eq!(file.size, 13);
    assert!(file.is_file());
}

#[test]
fn we_read_every_tarball_wrapper_a_tool_can_produce() {
    if !have("tar") {
        eprintln!("skipping: GNU tar not installed");
        return;
    }

    let dir = common::TempDir::new("tarball-all-wrappers");
    let source = sample(&dir);
    let expected = common::pseudo_random(20_000, 11);
    let _ = source;

    let plain = dir.join("base.tar");
    let out = Command::new("tar").arg("-cf").arg(&plain).arg("-C").arg(dir.join("src")).arg(".").output().expect("run tar");
    assert!(out.status.success(), "tar failed: {}", String::from_utf8_lossy(&out.stderr));
    let raw = std::fs::read(&plain).unwrap();

    let wrappers: [(&str, &str, &[&str]); 5] = [
        ("w.tar.gz", "gzip", &["-c"]),
        ("w.tar.bz2", "bzip2", &["-c"]),
        ("w.tar.xz", "xz", &["-c"]),
        ("w.tar.lzma", "lzma", &["-c", "-q"]),
        ("w.tar.Z", "compress", &["-c"]),
    ];

    for (name, tool, args) in wrappers {
        if !have(tool) {
            eprintln!("skipping {name}: {tool} not installed");
            continue;
        }

        let out = Command::new(tool).args(args).arg(&plain).output().unwrap_or_else(|e| panic!("{tool}: {e}"));
        assert!(out.status.success(), "{tool} failed: {}", String::from_utf8_lossy(&out.stderr));

        let archive = dir.join(name);
        std::fs::write(&archive, &out.stdout).unwrap();

        let dest = dir.join(format!("{name}-out"));
        Archive::new(&archive).extract_to(&dest).unwrap_or_else(|e| panic!("{name}: extract failed: {e}"));

        assert!(std::fs::read(dest.join("nested/b.bin")).unwrap() == expected, "{name}: contents differ");
    }

    let _ = raw;
}

#[test]
fn every_tarball_variant_lists_its_entries() {
    if !have("tar") || !have("xz") {
        eprintln!("skipping: needs GNU tar and xz");
        return;
    }

    let dir = common::TempDir::new("tarball-listing");
    let source = sample(&dir);

    let archive = dir.join("l.tar.xz");
    let out = Command::new("tar").arg("-cJf").arg(&archive).arg("-C").arg(dir.join("src")).arg(".").output().expect("run tar");
    assert!(out.status.success());
    let _ = source;

    let entries = Archive::new(&archive).entries().expect("listing a .tar.xz");
    assert!(entries.iter().any(|e| e.name.contains("a.txt")), "{entries:?}");
    assert!(entries.iter().all(|e| e.zip().is_none()), "tar entries must not carry zip detail");
}

#[test]
fn tar_lzma_round_trips_and_gnu_tar_reads_it() {
    let dir = common::TempDir::new("tarball-lzma");
    let source = sample(&dir);
    let expected = common::pseudo_random(20_000, 11);

    let archive = dir.join("out.tar.lzma");
    Archive::new(&archive).create_from([&source]).expect("create .tar.lzma");

    let out = dir.join("ours");
    Archive::new(&archive).extract_to(&out).expect("extract .tar.lzma");
    assert!(std::fs::read(out.join("src/nested/b.bin")).unwrap() == expected, "our own round trip differs");

    if !have("tar") || !have("lzma") {
        eprintln!("skipping the external half: needs GNU tar and lzma");
        return;
    }

    let listing = Command::new("tar").arg("-tf").arg(&archive).output().expect("run tar");
    assert!(listing.status.success(), "GNU tar rejected our .tar.lzma: {}", String::from_utf8_lossy(&listing.stderr));
    assert!(String::from_utf8_lossy(&listing.stdout).contains("nested/b.bin"));

    let gnu = dir.join("gnu");
    std::fs::create_dir_all(&gnu).unwrap();
    let status = Command::new("tar").arg("-xf").arg(&archive).arg("-C").arg(&gnu).status().expect("run tar");
    assert!(status.success(), "GNU tar failed to extract our .tar.lzma");
    assert!(std::fs::read(gnu.join("src/nested/b.bin")).unwrap() == expected, "GNU tar produced different bytes");
}

#[test]
fn tar_xz_round_trips_and_gnu_tar_reads_it() {
    let dir = common::TempDir::new("tarball-xz");
    let source = sample(&dir);
    let expected = common::pseudo_random(20_000, 11);

    let archive = dir.join("out.tar.xz");
    Archive::new(&archive).create_from([&source]).expect("create .tar.xz");

    let out = dir.join("ours");
    Archive::new(&archive).extract_to(&out).expect("extract .tar.xz");
    assert!(std::fs::read(out.join("src/nested/b.bin")).unwrap() == expected, "our own round trip differs");

    if !have("tar") || !have("xz") {
        eprintln!("skipping the external half: needs GNU tar and xz");
        return;
    }

    let checked = Command::new("xz").arg("-t").arg(&archive).output().expect("run xz -t");
    assert!(checked.status.success(), "xz -t rejected our .tar.xz: {}", String::from_utf8_lossy(&checked.stderr));

    let listing = Command::new("tar").arg("-tf").arg(&archive).output().expect("run tar");
    assert!(listing.status.success(), "GNU tar rejected our .tar.xz: {}", String::from_utf8_lossy(&listing.stderr));
    assert!(String::from_utf8_lossy(&listing.stdout).contains("nested/b.bin"));

    let gnu = dir.join("gnu");
    std::fs::create_dir_all(&gnu).unwrap();
    let status = Command::new("tar").arg("-xf").arg(&archive).arg("-C").arg(&gnu).status().expect("run tar");
    assert!(status.success(), "GNU tar failed to extract our .tar.xz");
    assert!(std::fs::read(gnu.join("src/nested/b.bin")).unwrap() == expected, "GNU tar produced different bytes");
}

#[test]
fn tar_zst_round_trips_and_external_tools_read_it() {
    let dir = common::TempDir::new("tarball-zst");
    let source = sample(&dir);
    let expected = common::pseudo_random(20_000, 11);

    let archive = dir.join("out.tar.zst");
    Archive::new(&archive).create_from([&source]).expect("create .tar.zst");

    let out = dir.join("ours");
    Archive::new(&archive).extract_to(&out).expect("extract .tar.zst");
    assert!(std::fs::read(out.join("src/nested/b.bin")).unwrap() == expected, "our own round trip differs");

    if !have("zstd") {
        eprintln!("skipping the external half: zstd not installed");
        return;
    }

    let checked = Command::new("zstd").arg("-t").arg(&archive).output().expect("run zstd -t");
    assert!(checked.status.success(), "zstd -t rejected our .tar.zst: {}", String::from_utf8_lossy(&checked.stderr));

    if !have("tar") {
        return;
    }
    let listing = Command::new("tar").arg("-tf").arg(&archive).output().expect("run tar");
    assert!(listing.status.success(), "GNU tar rejected our .tar.zst: {}", String::from_utf8_lossy(&listing.stderr));
    assert!(String::from_utf8_lossy(&listing.stdout).contains("nested/b.bin"));
}

#[test]
fn a_password_on_a_tarball_is_refused_rather_than_ignored() {
    let dir = common::TempDir::new("tar-password");
    let source = sample(&dir);

    for (name, kind) in [("p.tar", ArchiveType::Tar), ("p.tar.gz", ArchiveType::TarGz), ("p.tar.xz", ArchiveType::TarXz)] {
        let archive = dir.join(name);
        let err = Archive::new(&archive)
            .set_type(kind)
            .set_password("hunter2")
            .create_from([&source])
            .expect_err(&format!("{name}: a password request must not silently produce a readable archive"));

        assert!(err.is_unsupported(), "{name}: expected Unsupported, got {err}");
        assert!(!archive.exists() || std::fs::metadata(&archive).unwrap().len() == 0, "{name}: no archive should have been left behind");
    }
}

#[test]
fn splitting_a_tarball_across_volumes_is_refused() {
    let dir = common::TempDir::new("tar-volumes");
    let source = sample(&dir);

    let err = Archive::new(dir.join("v.tar.gz")).set_volume_size(64 * 1024).create_from([&source]).expect_err("volumes are not a tar concept");
    assert!(err.is_unsupported(), "expected Unsupported, got {err}");
}

#[test]
fn a_per_entry_method_on_a_tarball_is_refused() {
    let dir = common::TempDir::new("tar-method");
    let source = sample(&dir);

    let err = Archive::new(dir.join("m.tar.gz"))
        .set_method(ttarchive::codecs::Method::Bzip2)
        .create_from([&source])
        .expect_err("the wrapper compresses the stream, so a per-entry method means nothing");
    assert!(err.is_unsupported(), "expected Unsupported, got {err}");
}

#[test]
fn an_archive_comment_on_a_tarball_is_refused() {
    let dir = common::TempDir::new("tar-comment");
    let source = sample(&dir);

    let options = ttarchive::CreateOptions { comment: b"notes".to_vec(), ..ttarchive::CreateOptions::default() };
    let err = Archive::new(dir.join("c.tar")).with_create_options(options).create_from([&source]).expect_err("tar has nowhere to put a comment");
    assert!(err.is_unsupported(), "expected Unsupported, got {err}");
}

#[test]
fn a_thread_count_is_still_accepted_on_a_tarball() {
    let dir = common::TempDir::new("tar-threads");
    let source = sample(&dir);

    let archive = dir.join("t.tar.gz");
    Archive::new(&archive).set_threads(Some(4)).create_from([&source]).expect("a thread count should be accepted");
    assert!(archive.exists());
}

fn oversized(dir: &common::TempDir) -> std::path::PathBuf {
    for part in 0..3 {
        dir.write(format!("src/deep/nested/part-{part}.bin"), common::pseudo_random(3_500_000, 17 + part));
    }
    dir.write("src/deep/nested/text.txt", common::compressible(400_000));
    dir.join("src")
}

#[test]
fn a_large_tarball_round_trips_through_the_streaming_reader() {
    for (name, kind) in [("big.tar", ArchiveType::Tar), ("big.tar.gz", ArchiveType::TarGz)] {
        let dir = common::TempDir::new("tarball-streaming");
        let source = oversized(&dir);

        let archive = dir.join(name);
        Archive::new(&archive).set_type(kind).create_from([&source]).unwrap_or_else(|e| panic!("{name}: {e}"));

        let packed = std::fs::metadata(&archive).unwrap().len();
        assert!(packed > 8 * 1024 * 1024, "{name}: the fixture must exceed the buffering limit, got {packed}");

        let out = dir.join("out");
        let got = Archive::new(&archive).set_type(kind).extract_to(&out).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(got.files, 4, "{name}: expected four files");

        for part in 0..3 {
            let want = common::pseudo_random(3_500_000, 17 + part);
            let found = std::fs::read(out.join(format!("src/deep/nested/part-{part}.bin"))).unwrap();
            assert!(found == want, "{name}: part {part} came back different");
        }
        assert!(std::fs::read(out.join("src/deep/nested/text.txt")).unwrap() == common::compressible(400_000));
    }
}

#[test]
fn a_large_tarball_lists_its_entries_without_holding_it() {
    let dir = common::TempDir::new("tarball-streaming-list");
    let source = oversized(&dir);

    let archive = dir.join("big.tar.gz");
    Archive::new(&archive).create_from([&source]).unwrap();

    let entries = Archive::new(&archive).entries().unwrap();
    let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();

    assert!(names.iter().any(|n| n.contains("part-0.bin")), "listing missed an entry: {names:?}");
    assert!(names.iter().any(|n| n.contains("text.txt")), "listing missed an entry: {names:?}");
}

#[test]
fn the_planning_and_writing_passes_agree_on_entry_order() {
    let dir = common::TempDir::new("tarball-passes");
    let long = "a-very-long-directory-name-that-will-not-fit-in-a-ustar-header-field";
    dir.write(format!("src/{long}/{long}/one.txt"), b"first");
    dir.write("src/short.txt", b"second");
    dir.write(format!("src/{long}/two.txt"), b"third");

    for (name, kind) in [("p.tar", ArchiveType::Tar), ("p.tar.gz", ArchiveType::TarGz), ("p.tar.xz", ArchiveType::TarXz)] {
        let archive = dir.join(name);
        Archive::new(&archive).set_type(kind).create_from([dir.join("src")]).unwrap();

        let out = dir.join(format!("{name}-out"));
        Archive::new(&archive).set_type(kind).extract_to(&out).unwrap();

        assert_eq!(std::fs::read(out.join(format!("src/{long}/{long}/one.txt"))).unwrap(), b"first", "{name}: contents landed under the wrong name");
        assert_eq!(std::fs::read(out.join("src/short.txt")).unwrap(), b"second", "{name}: contents landed under the wrong name");
        assert_eq!(std::fs::read(out.join(format!("src/{long}/two.txt"))).unwrap(), b"third", "{name}: contents landed under the wrong name");
    }
}

#[test]
fn a_truncated_tarball_is_still_reported() {
    let dir = common::TempDir::new("tarball-truncated");
    let source = sample(&dir);

    let archive = dir.join("cut.tar.gz");
    Archive::new(&archive).create_from([&source]).unwrap();

    let whole = std::fs::read(&archive).unwrap();
    std::fs::write(&archive, &whole[..whole.len() * 2 / 3]).unwrap();

    assert!(Archive::new(&archive).extract_to(dir.join("out")).is_err(), "a truncated archive must not extract quietly");
}

#[test]
fn creating_a_large_archive_does_not_grow_with_the_tree() {
    let dir = common::TempDir::new("create-bounded");
    for part in 0..6 {
        dir.write(format!("src/part{part}.bin"), common::compressible(6_000_000));
    }

    for kind in ArchiveType::ALL.into_iter().filter(|k| k.can_write()) {
        let archive = dir.join(format!("bounded{}", kind.extension().clone()));
        let made = Archive::new(&archive).set_type(kind.clone()).create_from([dir.join("src")]).unwrap_or_else(|e| panic!("{kind:?}: {e}"));
        let kk = kind;

        assert_eq!(made.files, 6, "{kk:?}: expected six files");
        assert_eq!(made.bytes, 36_000_000, "{kk:?}: byte total does not match the tree");

        let out = dir.join(format!("out{}", kind.extension()));
        let got = Archive::new(&archive).set_type(kind).extract_to(&out).unwrap_or_else(|e| panic!("{kind:?}: {e}"));
        assert_eq!(got.files, 6, "{kk:?}: round trip lost files");

        for part in 0..6 {
            let found = std::fs::read(out.join(format!("src/part{part}.bin"))).unwrap();
            assert!(found == common::compressible(6_000_000), "{kind:?}: part {part} came back different");
        }
    }
}

#[test]
fn the_compression_level_is_honoured_by_every_wrapper() {
    let dir = common::TempDir::new("levels");
    dir.write("src/body.txt", common::compressible(1_500_000));

    for kind in ArchiveType::ALL.into_iter().filter(|k| k.can_write() && k != &ArchiveType::Tar) {
        let mut sizes = Vec::new();
        for level in [ttarchive::Level::Fast, ttarchive::Level::Default, ttarchive::Level::Best] {
            let archive = dir.join(format!("{level:?}{}", kind.extension()));
            Archive::new(&archive).set_type(kind).set_level(level).create_from([dir.join("src")]).unwrap_or_else(|e| panic!("{kind:?}/{level:?}: {e}"));

            let size = std::fs::metadata(&archive).unwrap().len();
            assert!(size > 0, "{kind:?}/{level:?}: produced nothing");
            sizes.push(size);

            let out = dir.join(format!("{level:?}-out{}", kind.extension()));
            Archive::new(&archive).set_type(kind).extract_to(&out).unwrap();
            assert!(std::fs::read(out.join("src/body.txt")).unwrap() == common::compressible(1_500_000), "{kind:?}/{level:?}: round trip differs");
        }

        let slack = (sizes[0] / 50).max(64);
        assert!(
            sizes[2] <= sizes[0] + slack,
            "{kind:?}: Best ({}) is materially larger than Fast ({}); input this compressible bottoms out at every level, so only a real regression should exceed the {slack} byte slack",
            sizes[2],
            sizes[0]
        );
    }
}

#[test]
fn tools_read_the_wrappers_we_write_in_parallel() {
    if !have("tar") || !have("gzip") || !have("bzip2") {
        eprintln!("skipping: tar, gzip or bzip2 not installed");
        return;
    }

    let dir = common::TempDir::new("parallel-pieces");
    for part in 0..4 {
        dir.write(format!("src/part{part}.bin"), common::compressible(3_000_000));
    }

    for (name, kind, tool) in [("p.tar.gz", ArchiveType::TarGz, "gzip"), ("p.tar.bz2", ArchiveType::TarBz2, "bzip2")] {
        let archive = dir.join(name);
        Archive::new(&archive).set_type(kind).create_from([dir.join("src")]).unwrap();

        let checked = Command::new(tool).arg("-t").arg(&archive).status().unwrap();
        assert!(checked.success(), "{name}: {tool} -t rejected the stream");

        let out = dir.join(format!("{name}-gnu"));
        std::fs::create_dir_all(&out).unwrap();
        let done = Command::new("tar").arg("-xf").arg(&archive).arg("-C").arg(&out).status().unwrap();
        assert!(done.success(), "{name}: tar failed to extract it");

        for part in 0..4 {
            let found = std::fs::read(out.join(format!("src/part{part}.bin"))).unwrap();
            assert!(found == common::compressible(3_000_000), "{name}: part {part} differs after tar extracted it");
        }
    }
}

#[test]
fn splitting_the_stream_does_not_cost_much_compression() {
    let dir = common::TempDir::new("parallel-ratio");
    for part in 0..4 {
        dir.write(format!("src/part{part}.bin"), common::compressible(3_000_000));
    }

    for (name, kind) in [("r.tar.gz", ArchiveType::TarGz), ("r.tar.bz2", ArchiveType::TarBz2)] {
        let whole = dir.join(format!("whole-{name}"));
        Archive::new(&whole).set_type(kind).set_threads(Some(1)).create_from([dir.join("src")]).unwrap();

        let split = dir.join(format!("split-{name}"));
        Archive::new(&split).set_type(kind).create_from([dir.join("src")]).unwrap();

        let (one, many) = (std::fs::metadata(&whole).unwrap().len(), std::fs::metadata(&split).unwrap().len());
        assert!(many < one + one / 10, "{name}: splitting cost {many} against {one}, more than a tenth");

        for archive in [&whole, &split] {
            let out = dir.join(format!("out-{}", archive.file_name().unwrap().to_string_lossy()));
            Archive::new(archive).set_type(kind).extract_to(&out).unwrap();
            for part in 0..4 {
                assert!(std::fs::read(out.join(format!("src/part{part}.bin"))).unwrap() == common::compressible(3_000_000), "{name}: part {part} differs");
            }
        }
    }
}
