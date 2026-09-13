mod common;

use std::process::Command;

use common::have;
use ttarchive::tar::header::{self, Kind};
use ttarchive::tar::{TarReader, pax};

fn gnu_program() -> String {
    common::gnu_tar().unwrap_or_else(|| "tar".to_string())
}

fn gnu_tar(dir: &common::TempDir, format: &str, extra: &[&str]) -> Vec<u8> {
    let archive = dir.join(format!("out-{format}.tar"));
    let mut cmd = Command::new(gnu_program());
    cmd.arg("-cf").arg(&archive).arg(format!("--format={format}")).args(extra).arg("-C").arg(dir.join("src")).arg(".");
    let out = cmd.output().expect("run tar");
    assert!(out.status.success(), "tar failed: {}", String::from_utf8_lossy(&out.stderr));
    std::fs::read(&archive).expect("read archive")
}

fn sample(dir: &common::TempDir) {
    dir.write("src/a.txt", b"hello tar");
    dir.write("src/nested/b.bin", common::pseudo_random(9_000, 3));
    dir.write("src/empty.txt", b"");
}

fn names(data: &[u8]) -> Vec<(String, u64)> {
    let mut reader = TarReader::new(data);
    let mut found = Vec::new();
    while let Some(entry) = reader.next_entry().expect("next entry") {
        found.push((entry.name.clone(), entry.size));
        reader.skip_data(&entry).expect("skip");
    }
    found
}

#[test]
fn octal_and_base256_numeric_fields_round_trip() {
    for value in [0u64, 1, 7, 8, 511, 0o777, 1 << 20, (1 << 33) - 1] {
        let mut block = [0u8; header::BLOCK];
        header::put_octal(&mut block, header::SIZE, value);
        let back = header::parse_numeric(&block[header::SIZE.0..header::SIZE.0 + header::SIZE.1], "size").unwrap();
        assert_eq!(back, value, "octal round trip for {value}");
    }

    for value in [1u64 << 34, 1 << 40, u64::from(u32::MAX) * 4096, (1u64 << 62) - 1] {
        let mut block = [0u8; header::BLOCK];
        header::put_octal(&mut block, header::SIZE, value);
        assert!(block[header::SIZE.0] & 0x80 != 0, "{value} should use base-256");
        let back = header::parse_numeric(&block[header::SIZE.0..header::SIZE.0 + header::SIZE.1], "size").unwrap();
        assert_eq!(back, value, "base-256 round trip for {value}");
    }
}

#[test]
fn negative_mtime_uses_base256() {
    let mut block = [0u8; header::BLOCK];
    header::put_signed(&mut block, header::MTIME, -1234);
    let back = header::parse_signed(&block[header::MTIME.0..header::MTIME.0 + header::MTIME.1], "mtime").unwrap();
    assert_eq!(back, -1234);
}

#[test]
fn a_written_header_parses_back() {
    let head = header::Header {
        name: b"dir/file.txt".to_vec(),
        mode: 0o640,
        uid: 1000,
        gid: 1000,
        size: 12_345,
        mtime: 1_700_000_000,
        kind: Kind::Regular,
        linkname: Vec::new(),
        uname: b"tukan".to_vec(),
        gname: b"users".to_vec(),
        devmajor: 0,
        devminor: 0,
        format: header::Format::Ustar,
    };

    let block = header::write(&head);
    let back = header::parse(&block).expect("parse");

    assert_eq!(back.name, head.name);
    assert_eq!(back.mode, head.mode);
    assert_eq!(back.uid, head.uid);
    assert_eq!(back.size, head.size);
    assert_eq!(back.mtime, head.mtime);
    assert_eq!(back.uname, head.uname);
    assert_eq!(back.kind, Kind::Regular);
}

#[test]
fn a_corrupt_checksum_is_refused() {
    let head = header::Header { name: b"x".to_vec(), ..Default::default() };
    let mut block = header::write(&head);
    assert!(header::parse(&block).is_ok());

    block[0] ^= 0xff;
    assert!(header::parse(&block).is_err(), "a flipped name byte must fail the checksum");
}

#[test]
fn long_names_split_across_prefix_and_name() {
    let deep = format!("{}/{}", "d".repeat(120), "f".repeat(80));
    let split = header::split_ustar_name(deep.as_bytes()).expect("should fit ustar prefix+name");
    assert_eq!(split.0.len(), 120);
    assert_eq!(split.1.len(), 80);

    let unsplittable = "x".repeat(300);
    assert!(header::split_ustar_name(unsplittable.as_bytes()).is_none(), "a 300 byte single component cannot fit ustar");
}

#[test]
fn pax_records_round_trip() {
    let mut attributes = pax::Attributes::default();
    attributes.set("path", "a/very/long/path".as_bytes().to_vec());
    attributes.set("mtime", b"1700000000.123456789".to_vec());
    attributes.set("uid", b"1000".to_vec());

    let encoded = pax::encode(&attributes);
    let back = pax::parse(&encoded).expect("parse");

    assert_eq!(back.text("path").unwrap(), "a/very/long/path");
    assert_eq!(back.seconds("mtime").unwrap(), 1_700_000_000);
    assert_eq!(back.number("uid").unwrap(), 1000);
}

#[test]
fn pax_record_lengths_are_self_describing() {
    let mut attributes = pax::Attributes::default();
    attributes.set("k", vec![b'v'; 200]);
    let encoded = pax::encode(&attributes);

    let space = encoded.iter().position(|&b| b == b' ').unwrap();
    let declared: usize = std::str::from_utf8(&encoded[..space]).unwrap().parse().unwrap();
    assert_eq!(declared, encoded.len(), "the length field must count itself");
}

#[test]
fn reads_gnu_tar_ustar_gnu_and_pax_output() {
    if !common::gnu_tar().is_some() {
        eprintln!("skipping: GNU tar not installed");
        return;
    }

    let dir = common::TempDir::new("tar-formats");
    sample(&dir);

    for format in ["ustar", "gnu", "pax", "v7"] {
        let data = gnu_tar(&dir, format, &[]);
        let found = names(&data);

        let has = |want: &str| found.iter().any(|(name, _)| name.trim_start_matches("./") == want);
        assert!(has("a.txt"), "{format}: missing a.txt in {found:?}");
        assert!(has("nested/b.bin"), "{format}: missing nested/b.bin in {found:?}");

        let size = found.iter().find(|(n, _)| n.trim_start_matches("./") == "nested/b.bin").map(|(_, s)| *s);
        assert_eq!(size, Some(9_000), "{format}: wrong size");
    }
}

#[test]
fn reads_entry_contents_written_by_gnu_tar() {
    if !common::gnu_tar().is_some() {
        eprintln!("skipping: GNU tar not installed");
        return;
    }

    let dir = common::TempDir::new("tar-contents");
    sample(&dir);
    let expected = common::pseudo_random(9_000, 3);

    for format in ["ustar", "gnu", "pax"] {
        let data = gnu_tar(&dir, format, &[]);
        let mut reader = TarReader::new(data.as_slice());
        let mut seen = false;

        while let Some(entry) = reader.next_entry().unwrap() {
            if entry.name.trim_start_matches("./") == "nested/b.bin" {
                let body = reader.read_data(&entry).unwrap();
                assert!(body == expected, "{format}: contents differ");
                seen = true;
            } else {
                reader.skip_data(&entry).unwrap();
            }
        }
        assert!(seen, "{format}: never saw nested/b.bin");
    }
}

#[test]
fn reads_names_longer_than_a_ustar_header_allows() {
    if !common::gnu_tar().is_some() {
        eprintln!("skipping: GNU tar not installed");
        return;
    }

    let dir = common::TempDir::new("tar-longnames");
    let deep = format!("{}/{}/{}", "a".repeat(90), "b".repeat(90), "c".repeat(90));
    dir.write(format!("src/{deep}"), b"deep contents");

    for format in ["gnu", "pax"] {
        let data = gnu_tar(&dir, format, &[]);
        let found = names(&data);
        assert!(found.iter().any(|(name, _)| name.contains(&"c".repeat(90))), "{format}: long name lost in {found:?}");
    }
}

#[test]
fn reads_symlinks_and_hardlinks() {
    if !common::gnu_tar().is_some() || !cfg!(unix) {
        eprintln!("skipping: needs GNU tar on unix");
        return;
    }

    let dir = common::TempDir::new("tar-links");
    dir.write("src/target.txt", b"link target contents");

    #[cfg(unix)]
    {
        std::os::unix::fs::symlink("target.txt", dir.join("src/link.txt")).expect("symlink");
        std::fs::hard_link(dir.join("src/target.txt"), dir.join("src/hard.txt")).expect("hard link");
    }

    let data = gnu_tar(&dir, "gnu", &[]);
    let mut reader = TarReader::new(data.as_slice());

    let mut symlink = None;
    let mut hardlink = None;
    while let Some(entry) = reader.next_entry().unwrap() {
        match entry.kind {
            Kind::Symlink => symlink = Some(entry.clone()),
            Kind::HardLink => hardlink = Some(entry.clone()),
            _ => {}
        }
        reader.skip_data(&entry).unwrap();
    }

    let symlink = symlink.expect("no symlink entry");
    assert_eq!(symlink.linkname, "target.txt");
    assert_eq!(symlink.entry_kind(), ttarchive::platform::EntryKind::Symlink);

    let hardlink = hardlink.expect("no hardlink entry");
    let linked = hardlink.linkname.trim_start_matches("./");
    assert!(linked == "target.txt" || linked == "hard.txt", "hardlink should name the other member of the pair, got {linked:?}");
    assert_ne!(linked, hardlink.name.trim_start_matches("./"), "a hardlink must not point at itself");
    assert_eq!(hardlink.size, 0, "a hardlink entry carries no data");
}

#[test]
fn reads_a_sparse_file_written_by_gnu_tar() {
    if !common::gnu_tar().is_some() {
        eprintln!("skipping: GNU tar not installed");
        return;
    }

    let dir = common::TempDir::new("tar-sparse");
    std::fs::create_dir_all(dir.join("src")).unwrap();

    let mut data = vec![0u8; 1 << 20];
    data[0..16].copy_from_slice(b"start of the map");
    data[(1 << 20) - 16..].copy_from_slice(b"end of the file!");
    std::fs::write(dir.join("src/sparse.bin"), &data).unwrap();

    let archive = gnu_tar(&dir, "pax", &["--sparse"]);
    let mut reader = TarReader::new(archive.as_slice());

    while let Some(entry) = reader.next_entry().unwrap() {
        if entry.name.trim_start_matches("./") == "sparse.bin" {
            let body = reader.read_data(&entry).unwrap();
            assert_eq!(body.len(), data.len(), "sparse file expanded to the wrong length");
            assert!(body == data, "sparse expansion changed the bytes");
            return;
        }
        reader.skip_data(&entry).unwrap();
    }
    panic!("never saw sparse.bin");
}

#[test]
fn reads_bsdtar_output_too() {
    if !have("bsdtar") {
        eprintln!("skipping: bsdtar not installed");
        return;
    }

    let dir = common::TempDir::new("tar-bsdtar");
    sample(&dir);

    let archive = dir.join("bsd.tar");
    let out = Command::new("bsdtar").arg("-cf").arg(&archive).arg("-C").arg(dir.join("src")).arg(".").output().expect("run bsdtar");
    assert!(out.status.success(), "bsdtar failed: {}", String::from_utf8_lossy(&out.stderr));

    let found = names(&std::fs::read(&archive).unwrap());
    assert!(found.iter().any(|(n, _)| n.trim_start_matches("./") == "a.txt"), "{found:?}");
    assert!(found.iter().any(|(n, _)| n.trim_start_matches("./") == "nested/b.bin"), "{found:?}");
}

#[test]
fn a_truncated_archive_is_reported() {
    if !common::gnu_tar().is_some() {
        eprintln!("skipping: GNU tar not installed");
        return;
    }

    let dir = common::TempDir::new("tar-truncated");
    sample(&dir);
    let data = gnu_tar(&dir, "gnu", &[]);

    let cut = &data[..data.len().min(1024) - 100];
    let mut reader = TarReader::new(cut);

    let mut failed = false;
    loop {
        match reader.next_entry() {
            Ok(Some(entry)) => {
                if reader.read_data(&entry).is_err() {
                    failed = true;
                    break;
                }
            }
            Ok(None) => break,
            Err(_) => {
                failed = true;
                break;
            }
        }
    }
    assert!(failed, "a truncated archive should be reported, not silently accepted");
}

fn sparse_source(dir: &common::TempDir) -> Vec<u8> {
    use std::io::{Seek, SeekFrom, Write};

    std::fs::create_dir_all(dir.join("src")).unwrap();
    let path = dir.join("src/sparse.bin");
    let mut file = std::fs::File::create(&path).unwrap();

    file.write_all(b"start of the map").unwrap();
    file.seek(SeekFrom::Start(4 * 1024 * 1024)).unwrap();
    file.write_all(b"middle chunk here").unwrap();
    file.seek(SeekFrom::Start(8 * 1024 * 1024 - 16)).unwrap();
    file.write_all(b"end of the file!").unwrap();
    drop(file);

    std::fs::read(&path).unwrap()
}

#[test]
fn reads_every_sparse_layout_gnu_tar_can_write() {
    if !common::gnu_tar().is_some() {
        eprintln!("skipping: GNU tar not installed");
        return;
    }

    let dir = common::TempDir::new("tar-sparse-all");
    let expected = sparse_source(&dir);

    let layouts: [(&str, &[&str]); 5] = [
        ("oldgnu", &["--format=oldgnu"]),
        ("gnu", &["--format=gnu"]),
        ("pax-0.0", &["--format=posix", "--sparse-version=0.0"]),
        ("pax-0.1", &["--format=posix", "--sparse-version=0.1"]),
        ("pax-1.0", &["--format=posix", "--sparse-version=1.0"]),
    ];

    for (label, flags) in layouts {
        let archive = dir.join(format!("s-{label}.tar"));
        let mut cmd = Command::new(gnu_program());
        cmd.arg("--sparse").arg("-cf").arg(&archive).args(flags).arg("-C").arg(dir.join("src")).arg(".");
        let out = cmd.output().expect("run tar");
        assert!(out.status.success(), "{label}: tar failed: {}", String::from_utf8_lossy(&out.stderr));

        let stored = std::fs::metadata(&archive).unwrap().len();
        assert!(stored < expected.len() as u64 / 4, "{label}: the archive is not actually sparse ({stored} bytes)");

        let dest = dir.join(format!("out-{label}"));
        ttarchive::Archive::new(&archive).extract_to(&dest).unwrap_or_else(|e| panic!("{label}: {e}"));

        let back = std::fs::read(dest.join("sparse.bin")).unwrap_or_else(|e| panic!("{label}: missing entry: {e}"));
        assert_eq!(back.len(), expected.len(), "{label}: wrong length");
        assert!(back == expected, "{label}: the holes came back in the wrong places");
    }
}

#[test]
fn a_sparse_entry_reports_the_real_size_not_the_stored_one() {
    if !common::gnu_tar().is_some() {
        eprintln!("skipping: GNU tar not installed");
        return;
    }

    let dir = common::TempDir::new("tar-sparse-size");
    let expected = sparse_source(&dir);

    let archive = dir.join("s.tar");
    let out = Command::new(gnu_program())
        .arg("--sparse")
        .arg("-cf")
        .arg(&archive)
        .arg("--format=posix")
        .arg("--sparse-version=1.0")
        .arg("-C")
        .arg(dir.join("src"))
        .arg(".")
        .output()
        .expect("run tar");
    assert!(out.status.success());

    let entries = ttarchive::Archive::new(&archive).entries().expect("list");
    let entry = entries.iter().find(|e| e.name.ends_with("sparse.bin")).expect("no sparse entry");

    assert_eq!(entry.size, expected.len() as u64, "a sparse entry should report the size it expands to");
    assert!(entry.zip().is_none(), "a tar entry carries tar detail");
}

#[test]
fn we_write_sparse_entries_that_we_and_gnu_tar_both_read_back() {
    let dir = common::TempDir::new("tar-sparse-write");
    let expected = sparse_source(&dir);

    let archive = dir.join("ours.tar");
    ttarchive::Archive::new(&archive).set_sparse(true).create_from([dir.join("src")]).expect("create a sparse tar");

    let stored = std::fs::metadata(&archive).unwrap().len();
    assert!(stored < expected.len() as u64 / 4, "our sparse archive is not actually sparse ({stored} bytes)");

    let ours = dir.join("ours-out");
    ttarchive::Archive::new(&archive).extract_to(&ours).expect("extract our own sparse tar");
    let back = std::fs::read(ours.join("src/sparse.bin")).expect("missing entry");
    assert_eq!(back.len(), expected.len(), "wrong length after our own round trip");
    assert!(back == expected, "our own round trip put the holes in the wrong places");

    if !common::gnu_tar().is_some() {
        eprintln!("skipping the external half: GNU tar not installed");
        return;
    }

    let theirs = dir.join("gnu-out");
    std::fs::create_dir_all(&theirs).unwrap();
    let out = Command::new(gnu_program()).arg("-xf").arg(&archive).arg("-C").arg(&theirs).output().expect("run tar");
    assert!(out.status.success(), "GNU tar rejected our sparse tar: {}", String::from_utf8_lossy(&out.stderr));

    let back = std::fs::read(theirs.join("src/sparse.bin")).expect("GNU tar wrote no entry");
    assert!(back == expected, "GNU tar expanded our holes differently");

    if !have("bsdtar") {
        return;
    }

    let bsd = dir.join("bsd-out");
    std::fs::create_dir_all(&bsd).unwrap();
    let out = Command::new("bsdtar").arg("-xf").arg(&archive).arg("-C").arg(&bsd).output().expect("run bsdtar");
    assert!(out.status.success(), "bsdtar rejected our sparse tar: {}", String::from_utf8_lossy(&out.stderr));
    assert!(std::fs::read(bsd.join("src/sparse.bin")).unwrap() == expected, "bsdtar expanded our holes differently");
}

#[test]
fn the_segments_we_write_land_on_block_boundaries() {
    let dir = common::TempDir::new("tar-sparse-align");
    sparse_source(&dir);

    let archive = dir.join("aligned.tar");
    ttarchive::Archive::new(&archive).set_sparse(true).create_from([dir.join("src")]).expect("create");

    let raw = std::fs::read(&archive).unwrap();
    let at = raw.windows(13).position(|w| w == b"GNUSparseFile").expect("no sparse entry");
    let map_at = (at / 512) * 512 + 512;
    let text = String::from_utf8_lossy(&raw[map_at..map_at + 128]);

    let numbers: Vec<u64> = text.split('\n').skip(1).take_while(|line| !line.is_empty()).filter_map(|line| line.parse().ok()).collect();
    assert!(!numbers.is_empty(), "no map was written: {text:?}");
    for value in numbers {
        assert_eq!(value % 512, 0, "GNU tar only reads a PAX 1.0 map whose segments sit on block boundaries: {text:?}");
    }
}

#[test]
fn a_file_with_nothing_to_skip_is_stored_whole() {
    let dir = common::TempDir::new("tar-sparse-dense");
    std::fs::create_dir_all(dir.join("src")).unwrap();
    let solid = common::pseudo_random(2 * 1024 * 1024, 71);
    std::fs::write(dir.join("src/solid.bin"), &solid).unwrap();

    let archive = dir.join("dense.tar");
    ttarchive::Archive::new(&archive).set_sparse(true).create_from([dir.join("src")]).expect("create");

    let dest = dir.join("out");
    ttarchive::Archive::new(&archive).extract_to(&dest).expect("extract");
    assert!(std::fs::read(dest.join("src/solid.bin")).unwrap() == solid, "a dense file did not survive the sparse path");
}

#[test]
fn zip_refuses_to_pretend_it_can_store_holes() {
    let dir = common::TempDir::new("zip-sparse");
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(dir.join("src/a.txt"), b"hello").unwrap();

    let err = ttarchive::Archive::new(dir.join("a.zip")).set_sparse(true).create_from([dir.join("src")]).expect_err("zip should refuse");
    assert!(err.is_unsupported(), "expected Unsupported, got {err}");
}

fn v7_header(name: &str, size: usize, mode: &str) -> [u8; 512] {
    let mut block = [0u8; 512];
    block[..name.len()].copy_from_slice(name.as_bytes());
    block[100..108].copy_from_slice(format!("{mode:0>7}\0").as_bytes());
    block[108..116].copy_from_slice(b"0001750\0");
    block[116..124].copy_from_slice(b"0001750\0");
    block[124..136].copy_from_slice(format!("{size:011o}\0").as_bytes());
    block[136..148].copy_from_slice(b"14000000000\0");
    block[148..156].copy_from_slice(b"        ");

    let sum: u32 = block.iter().map(|&b| b as u32).sum();
    block[148..156].copy_from_slice(format!("{sum:06o}\0 ").as_bytes());
    block
}

fn v7_archive(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut out = Vec::new();
    for (name, body) in entries {
        let mode = if name.ends_with('/') { "755" } else { "644" };
        out.extend_from_slice(&v7_header(name, body.len(), mode));
        out.extend_from_slice(body);
        out.resize(out.len().div_ceil(512) * 512, 0);
    }
    out.extend_from_slice(&[0u8; 1024]);
    out
}

#[test]
fn a_v7_directory_is_recognised_by_its_trailing_slash() {
    let dir = common::TempDir::new("tar-v7-dirs");
    let archive = dir.join("v7.tar");
    std::fs::write(&archive, v7_archive(&[("./", b""), ("./sub/", b""), ("./sub/f.txt", b"hi\n"), ("./empty/", b"")])).unwrap();

    let listed: Vec<(String, bool)> = ttarchive::Archive::new(&archive).entries().unwrap().into_iter().map(|e| (e.name.clone(), e.is_dir())).collect();
    assert!(listed.contains(&("./sub/".to_string(), true)), "{listed:?}");
    assert!(listed.contains(&("./sub/f.txt".to_string(), false)), "{listed:?}");

    let out = dir.join("out");
    ttarchive::Archive::new(&archive).extract_to(&out).expect("a v7 archive with directories extracts");

    assert!(out.join("sub").is_dir());
    assert!(out.join("empty").is_dir(), "an empty v7 directory is still a directory");
    assert_eq!(std::fs::read(out.join("sub/f.txt")).unwrap(), b"hi\n");
}

#[test]
fn a_v7_file_whose_name_ends_in_a_slash_but_holds_data_is_not_mistaken_for_a_directory() {
    let data = v7_archive(&[("odd/", b"payload"), ("after.txt", b"still aligned")]);

    let mut reader = TarReader::new(&data[..]);
    let first = reader.next_entry().unwrap().expect("first entry");
    assert_eq!(first.kind, Kind::Regular);
    assert_eq!(first.size, 7);
    reader.skip_data(&first).unwrap();

    let second = reader.next_entry().unwrap().expect("the reader stays aligned past it");
    assert_eq!(second.name, "after.txt");
}
