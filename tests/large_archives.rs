mod common;

use std::fs::{self, File};
use std::io::{BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::time::Instant;

use common::TempDir;
use ttarchive::codecs::Level;
use ttarchive::utils::crc32::Crc32;
use ttarchive::{Archive, ArchiveType, EncryptionMethod};

fn payload_mb() -> u64 {
    std::env::var("TTARCHIVE_LARGE_MB").ok().and_then(|v| v.parse().ok()).unwrap_or(1024)
}

fn scratch_base() -> PathBuf {
    let dir = std::env::var("TTARCHIVE_LARGE_DIR").map(PathBuf::from).unwrap_or_else(|_| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/large-tests"));
    fs::create_dir_all(&dir).expect("create scratch dir");
    dir
}

fn scratch(tag: &str) -> TempDir {
    TempDir::new_in(scratch_base(), tag)
}

fn huge_enabled() -> bool {
    std::env::var("TTARCHIVE_HUGE").is_ok_and(|v| v == "1")
}

fn write_noise(path: &Path, bytes: u64, seed: u32) -> u32 {
    let mut out = BufWriter::with_capacity(1 << 20, File::create(path).expect("create"));
    let mut crc = Crc32::new();
    let mut state = seed | 1;

    let mut block = vec![0u8; 1 << 20];
    let mut written = 0u64;
    while written < bytes {
        for chunk in block.chunks_mut(4) {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            chunk.copy_from_slice(&state.to_le_bytes()[..chunk.len()]);
        }
        let take = block.len().min((bytes - written) as usize);
        out.write_all(&block[..take]).expect("write");
        crc.update(&block[..take]);
        written += take as u64;
    }

    out.flush().expect("flush");
    crc.finish()
}

fn write_compressible(path: &Path, bytes: u64) -> u32 {
    let mut out = BufWriter::with_capacity(1 << 20, File::create(path).expect("create"));
    let mut crc = Crc32::new();

    let phrase = b"the quick brown fox jumps over the lazy dog. ";
    let block: Vec<u8> = phrase.iter().copied().cycle().take(1 << 20).collect();

    let mut written = 0u64;
    while written < bytes {
        let take = block.len().min((bytes - written) as usize);
        out.write_all(&block[..take]).expect("write");
        crc.update(&block[..take]);
        written += take as u64;
    }

    out.flush().expect("flush");
    crc.finish()
}

fn crc_of(path: &Path) -> u32 {
    let mut file = File::open(path).expect("open");
    let mut crc = Crc32::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = file.read(&mut buf).expect("read");
        if n == 0 {
            break;
        }
        crc.update(&buf[..n]);
    }
    crc.finish()
}

fn report(label: &str, bytes: u64, elapsed: std::time::Duration) {
    let mb = bytes as f64 / (1024.0 * 1024.0);
    let secs = elapsed.as_secs_f64().max(1e-9);
    println!("  {label}: {mb:.0} MiB in {secs:.2}s  ({:.0} MiB/s)", mb / secs);
}

#[test]
#[ignore = "large: writes ~1 GB"]
fn single_large_incompressible_file_round_trips() {
    let mb = payload_mb();
    let bytes = mb * 1024 * 1024;
    println!("single_large_incompressible_file_round_trips: {mb} MiB");

    let src = scratch("big1-src");
    let payload = src.join("payload.bin");
    let expected = write_noise(&payload, bytes, 12345);

    let work = scratch("big1-work");
    let archive = work.join("a.zip");

    let start = Instant::now();
    let summary = Archive::new(&archive).set_type(ArchiveType::Zip).create_from([&payload]).expect("create");
    report("compress", bytes, start.elapsed());

    assert!(summary.archive_size < bytes + bytes / 100, "archive {} vs payload {bytes}", summary.archive_size);

    let dest = scratch("big1-dest");
    let start = Instant::now();
    Archive::new(&archive).extract_to(dest.path()).expect("extract");
    report("extract", bytes, start.elapsed());

    let out = dest.join("payload.bin");
    assert_eq!(fs::metadata(&out).unwrap().len(), bytes, "size");
    assert_eq!(crc_of(&out), expected, "contents");
}

#[test]
#[ignore = "large: writes ~1 GB"]
fn single_large_compressible_file_round_trips() {
    let mb = payload_mb();
    let bytes = mb * 1024 * 1024;
    println!("single_large_compressible_file_round_trips: {mb} MiB");

    let src = scratch("big2-src");
    let payload = src.join("payload.txt");
    let expected = write_compressible(&payload, bytes);

    let work = scratch("big2-work");
    let archive = work.join("a.zip");

    let start = Instant::now();
    let summary = Archive::new(&archive).set_type(ArchiveType::Zip).create_from([&payload]).expect("create");
    report("compress", bytes, start.elapsed());

    assert!(summary.archive_size < bytes / 50, "repetitive text should compress >50x, got {} from {bytes}", summary.archive_size);

    let dest = scratch("big2-dest");
    let start = Instant::now();
    Archive::new(&archive).extract_to(dest.path()).expect("extract");
    report("extract", bytes, start.elapsed());

    assert_eq!(crc_of(&dest.join("payload.txt")), expected);
}

#[test]
#[ignore = "large: writes ~1 GB"]
fn many_files_totalling_a_gigabyte() {
    let mb = payload_mb();
    let count = 512u64;
    let each = (mb * 1024 * 1024) / count;
    println!("many_files_totalling_a_gigabyte: {count} files of {} KiB", each / 1024);

    let src = scratch("many-src");
    let mut expected = Vec::with_capacity(count as usize);
    for i in 0..count {
        let path = src.join(format!("f{i:04}.bin"));
        let crc = if i % 2 == 0 { write_noise(&path, each, i as u32 + 1) } else { write_compressible(&path, each) };
        expected.push((format!("f{i:04}.bin"), crc));
    }

    let work = scratch("many-work");
    let archive = work.join("a.zip");

    let start = Instant::now();
    let summary = Archive::new(&archive).set_type(ArchiveType::Zip).create_from([src.path()]).expect("create");
    report("compress", mb * 1024 * 1024, start.elapsed());
    assert_eq!(summary.files, count);

    let dest = scratch("many-dest");
    let start = Instant::now();
    Archive::new(&archive).extract_to(dest.path()).expect("extract");
    report("extract", mb * 1024 * 1024, start.elapsed());

    let stem = src.path().file_name().unwrap().to_string_lossy().into_owned();
    for (name, crc) in &expected {
        assert_eq!(crc_of(&dest.join(&stem).join(name)), *crc, "{name}");
    }
}

#[test]
#[ignore = "large: writes ~1 GB twice"]
fn parallel_matches_sequential_at_scale() {
    let mb = payload_mb() / 4;
    let count = 128u64;
    let each = (mb * 1024 * 1024) / count;

    let src = scratch("par-src");
    for i in 0..count {
        write_compressible(&src.join(format!("f{i:04}.txt")), each);
    }

    let work = scratch("par-work");

    let start = Instant::now();
    Archive::new(work.join("seq.zip")).set_type(ArchiveType::Zip).set_threads(Some(1)).create_from([src.path()]).unwrap();
    report("sequential", mb * 1024 * 1024, start.elapsed());

    let start = Instant::now();
    Archive::new(work.join("par.zip")).set_type(ArchiveType::Zip).set_threads(None).create_from([src.path()]).unwrap();
    report("parallel", mb * 1024 * 1024, start.elapsed());

    assert_eq!(crc_of(&work.join("seq.zip")), crc_of(&work.join("par.zip")), "thread count must not change the output");
}

#[test]
#[ignore = "large: writes ~1 GB"]
fn large_multivolume_round_trips() {
    let mb = payload_mb() / 2;
    let bytes = mb * 1024 * 1024;
    let volume = 100 * 1024 * 1024;

    let src = scratch("mv-src");
    let payload = src.join("payload.bin");
    let expected = write_noise(&payload, bytes, 777);

    let work = scratch("mv-work");
    let archive = work.join("a.zip");

    let summary = Archive::new(&archive).set_type(ArchiveType::Zip).set_volume_size(volume).create_from([&payload]).expect("create split");

    println!("  {} volumes of {} MiB", summary.volumes, volume / (1024 * 1024));
    assert!(summary.volumes > 1);

    let dest = scratch("mv-dest");
    Archive::new(work.join("a.z01")).set_type(ArchiveType::Zip).extract_to(dest.path()).expect("extract split");

    assert_eq!(crc_of(&dest.join("payload.bin")), expected);
}

#[test]
#[ignore = "large: writes ~1 GB"]
fn large_encrypted_archive_round_trips() {
    let mb = payload_mb() / 2;
    let bytes = mb * 1024 * 1024;

    let src = scratch("enc-src");
    let payload = src.join("payload.bin");
    let expected = write_noise(&payload, bytes, 4242);

    let work = scratch("enc-work");
    let archive = work.join("a.zip");

    let start = Instant::now();
    Archive::new(&archive)
        .set_type(ArchiveType::Zip)
        .set_password("a long enough passphrase for a large archive")
        .set_encryption(EncryptionMethod::Aes256)
        .create_from([&payload])
        .expect("create encrypted");
    report("encrypt", bytes, start.elapsed());

    let dest = scratch("enc-dest");
    let start = Instant::now();
    Archive::new(&archive).set_password("a long enough passphrase for a large archive").extract_to(dest.path()).expect("extract encrypted");
    report("decrypt", bytes, start.elapsed());

    assert_eq!(crc_of(&dest.join("payload.bin")), expected);
}

#[test]
#[ignore = "large: writes ~1 GB"]
fn external_tools_verify_large_archives() {
    let mb = payload_mb() / 2;
    let bytes = mb * 1024 * 1024;

    let src = scratch("ext-src");
    write_noise(&src.join("noise.bin"), bytes / 2, 5);
    write_compressible(&src.join("text.txt"), bytes / 2);

    let work = scratch("ext-work");
    let archive = work.join("a.zip");
    Archive::new(&archive).set_type(ArchiveType::Zip).create_from([src.path()]).unwrap();

    for (tool, args) in [("unzip", vec!["-t", "a.zip"]), ("7z", vec!["t", "a.zip"])] {
        let available = std::process::Command::new("sh").arg("-c").arg(format!("command -v {tool}")).output().map(|o| o.status.success()).unwrap_or(false);
        if !available {
            println!("  skipping {tool}: not installed");
            continue;
        }

        let out = std::process::Command::new(tool).args(&args).current_dir(work.path()).output().expect("run tool");
        let text = String::from_utf8_lossy(&out.stdout);
        assert!(out.status.success(), "{tool} failed:\n{text}");
        println!("  {tool}: ok");
    }
}

#[test]
#[ignore = "huge: needs TTARCHIVE_HUGE=1 and >4 GiB of scratch space"]
fn zip64_entry_larger_than_four_gigabytes() {
    if !huge_enabled() {
        println!("skipping: set TTARCHIVE_HUGE=1 to run");
        return;
    }

    let bytes = 5u64 * 1024 * 1024 * 1024;

    let src = scratch("z64-src");
    let payload = src.join("huge.txt");
    println!("  generating {} GiB", bytes / (1024 * 1024 * 1024));
    let expected = write_compressible(&payload, bytes);

    let work = scratch("z64-work");
    let archive = work.join("a.zip");

    let start = Instant::now();
    Archive::new(&archive).set_type(ArchiveType::Zip).set_level(Level::Fast).create_from([&payload]).expect("create zip64");
    report("compress", bytes, start.elapsed());

    let entries = Archive::new(&archive).entries().unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].size, bytes, "Zip64 size must round-trip");
    let zip = entries[0].zip().unwrap();
    assert!(zip.version_needed >= 45, "version needed should be 4.5 for Zip64, got {}", zip.version_needed);

    let dest = scratch("z64-dest");
    let start = Instant::now();
    Archive::new(&archive).extract_to(dest.path()).expect("extract zip64");
    report("extract", bytes, start.elapsed());

    let out = dest.join("huge.txt");
    assert_eq!(fs::metadata(&out).unwrap().len(), bytes);
    assert_eq!(crc_of(&out), expected);
}

#[test]
#[ignore = "huge: needs TTARCHIVE_HUGE=1"]
fn external_tools_verify_zip64_archives() {
    if !huge_enabled() {
        println!("skipping: set TTARCHIVE_HUGE=1 to run");
        return;
    }

    let bytes = 5u64 * 1024 * 1024 * 1024;
    let src = scratch("z64x-src");
    write_compressible(&src.join("huge.txt"), bytes);

    let work = scratch("z64x-work");
    let archive = work.join("a.zip");
    Archive::new(&archive).set_type(ArchiveType::Zip).set_level(Level::Fast).create_from([&src.join("huge.txt")]).unwrap();

    let out = std::process::Command::new("7z").args(["t", "a.zip"]).current_dir(work.path()).output().expect("run 7z");
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "7z rejected our Zip64 archive:\n{text}");
}
