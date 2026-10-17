mod common;

use std::time::{Duration, Instant};

use ttarchive::codecs::{Level, bzip2, compress, deflate, gzip, lzip, lzma, xz, zstd};

const PATIENCE: Duration = Duration::from_secs(20);

struct Watchdog {
    done: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl Watchdog {
    fn watching(what: &'static str, limit: Duration) -> Self {
        let done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = done.clone();
        std::thread::spawn(move || {
            let deadline = Instant::now() + limit;
            while Instant::now() < deadline {
                if flag.load(std::sync::atomic::Ordering::Relaxed) {
                    return;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            eprintln!("{what} never finished: it is stuck on a malformed input");
            std::process::abort();
        });
        Watchdog { done }
    }
}

impl Drop for Watchdog {
    fn drop(&mut self) {
        self.done.store(true, std::sync::atomic::Ordering::Relaxed);
    }
}

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, limit: usize) -> usize {
        (self.next() % limit.max(1) as u64) as usize
    }
}

fn mutants(data: &[u8], seed: u64, multi: usize) -> Vec<(String, Vec<u8>)> {
    let mut cases = Vec::new();

    for len in (0..data.len()).step_by(13) {
        cases.push((format!("truncate to {len}"), data[..len].to_vec()));
    }
    for at in (0..data.len()).step_by(7) {
        for (tag, value) in [("flip bit 0", data[at] ^ 0x01), ("flip bit 7", data[at] ^ 0x80), ("ones", 0xff)] {
            let mut bytes = data.to_vec();
            bytes[at] = value;
            cases.push((format!("{tag} at {at}"), bytes));
        }
    }

    let mut rng = Rng(seed | 1);
    for case in 0..multi {
        let mut bytes = data.to_vec();
        for _ in 0..2 + rng.below(6) {
            let at = rng.below(bytes.len().max(1));
            if at < bytes.len() {
                bytes[at] = (rng.next() & 0xff) as u8;
            }
        }
        if rng.below(4) == 0 && !bytes.is_empty() {
            bytes.truncate(rng.below(bytes.len()));
        }
        cases.push((format!("multi {case}"), bytes));
    }

    cases
}

fn sweep(label: &str, data: &[u8], multi: usize, run: impl Fn(&[u8])) {
    let started = Instant::now();
    for (what, bytes) in mutants(data, 0x5eed_1234 ^ data.len() as u64, multi) {
        let each = Instant::now();
        run(&bytes);
        assert!(each.elapsed() < PATIENCE, "{label}: {what} took {:?}", each.elapsed());
    }
    assert!(started.elapsed() < PATIENCE * 6, "{label}: the whole sweep took {:?}", started.elapsed());
}

fn payload() -> Vec<u8> {
    let mut out = common::compressible(1_500);
    out.extend(common::pseudo_random(400, 11));
    out.extend(vec![0u8; 200]);
    out
}

#[test]
fn no_mutation_of_a_compressed_stream_panics_or_runs_away() {
    let _watchdog = Watchdog::watching("the codec sweep", PATIENCE * 9);
    let data = payload();

    let deflated = deflate::compress(&data, Level::Default);
    sweep("deflate", &deflated, 400, |b| {
        let _ = deflate::decompress(b, 0);
    });
    sweep("deflate64", &deflated, 400, |b| {
        let _ = deflate::inflate::decompress_variant(b, 0, deflate::inflate::Variant::Deflate64);
    });

    let gz = gzip::compress(&data, Level::Default, &gzip::Member::default()).unwrap();
    sweep("gzip", &gz, 400, |b| {
        let _ = gzip::decompress(b, 0);
    });

    let bz = bzip2::compress(&data, 1).unwrap();
    sweep("bzip2", &bz, 400, |b| {
        let _ = bzip2::decompress(b, 0);
    });

    let alone = lzma::alone::compress(&data, 16).unwrap();
    sweep("lzma alone", &alone, 400, |b| {
        let _ = lzma::alone::decompress(b, 0);
    });

    let xzed = xz::encode::compress_default(&data, 16).unwrap();
    sweep("xz", &xzed, 400, |b| {
        let _ = xz::decompress(b, 0);
    });

    let zst = zstd::encode::compress(&data, true).unwrap();
    sweep("zstd", &zst, 400, |b| {
        let _ = zstd::decompress(b, 0);
    });

    let lz = lzip::compress_at(&data, 16, Level::Default).unwrap();
    sweep("lzip", &lz, 400, |b| {
        let _ = lzip::decompress(b, 0);
    });
    sweep("compress", &lz, 400, |b| {
        let _ = compress::decompress(b, 0);
    });
}

#[test]
fn no_mutation_of_an_archive_panics_or_runs_away() {
    let _watchdog = Watchdog::watching("the archive sweep", PATIENCE * 9);
    let dir = common::TempDir::new("malformed-archives");
    dir.write("src/a.txt", common::compressible(600));
    dir.write("src/nested/b.bin", common::pseudo_random(300, 3));
    dir.write("src/nested/deep/an-entry-name-long-enough-to-need-an-extended-header-of-its-own.txt", b"three");

    for (name, kind) in [
        ("plain.tar", ttarchive::ArchiveType::Tar),
        ("plain.zip", ttarchive::ArchiveType::Zip),
        ("plain.7z", ttarchive::ArchiveType::SevenZ),
        ("wrapped.tar.gz", ttarchive::ArchiveType::TarGz),
    ] {
        let archive = dir.join(name);
        ttarchive::Archive::new(&archive).set_type(kind).create_from([dir.join("src")]).unwrap();
        let bytes = std::fs::read(&archive).unwrap();

        let out = common::TempDir::new(&format!("malformed-{}", name.replace('.', "-")));
        let mutant = out.join("mutant");
        let into = out.join("into");

        sweep(&format!("{name} list"), &bytes, 300, |b| {
            std::fs::write(&mutant, b).unwrap();
            let _ = ttarchive::Archive::new(&mutant).set_type(kind).entries();
        });
        sweep(&format!("{name} extract"), &bytes, 100, |b| {
            std::fs::write(&mutant, b).unwrap();
            let _ = std::fs::remove_dir_all(&into);
            let _ = ttarchive::Archive::new(&mutant).set_type(kind).extract_to(&into);
        });
    }
}

#[test]
fn a_zstd_frame_cut_to_its_header_is_refused_rather_than_looped_over() {
    let _watchdog = Watchdog::watching("the zstd header test", PATIENCE);
    let packed = zstd::encode::compress(&payload(), true).unwrap();

    for len in 0..=16.min(packed.len()) {
        let started = Instant::now();
        let result = zstd::decompress(&packed[..len], 0);
        assert!(started.elapsed() < PATIENCE, "zstd cut to {len} bytes took {:?}", started.elapsed());
        match len {
            0 => assert_eq!(result.unwrap_or_default(), Vec::new(), "an empty stream is an empty result"),
            _ => assert!(result.is_err(), "zstd cut to {len} bytes decoded anyway"),
        }
    }
}

#[test]
fn an_lzma_stream_that_lies_about_its_size_stops_when_its_input_runs_out() {
    let _watchdog = Watchdog::watching("the lzma size test", PATIENCE);
    let data = payload();
    let packed = lzma::alone::compress(&data, 16).unwrap();

    for at in 5..13 {
        let mut bytes = packed.clone();
        bytes[at] ^= 0x40;

        let started = Instant::now();
        let result = lzma::alone::decompress(&bytes, 0);
        assert!(started.elapsed() < PATIENCE, "a size field flipped at {at} ran for {:?}", started.elapsed());

        if let Ok(out) = result {
            assert!(out.len() <= data.len() * 4, "a size field flipped at {at} produced {} bytes from {} of input", out.len(), bytes.len());
        }
    }
}

#[test]
fn a_truncated_lzip_member_is_refused_promptly() {
    let _watchdog = Watchdog::watching("the lzip truncation test", PATIENCE);
    let packed = lzip::compress_at(&payload(), 16, Level::Default).unwrap();

    for cut in [packed.len() / 4, packed.len() / 2, packed.len() - 20, packed.len() - 1] {
        let started = Instant::now();
        assert!(lzip::decompress(&packed[..cut], 0).is_err(), "lzip cut to {cut} decoded anyway");
        assert!(started.elapsed() < PATIENCE, "lzip cut to {cut} took {:?}", started.elapsed());
    }
}

fn with_sevenz_crcs(bytes: &[u8]) -> Option<Vec<u8>> {
    use ttarchive::utils::crc32;

    let mut fixed = bytes.to_vec();
    if fixed.len() < 32 {
        return None;
    }
    let offset = u64::from_le_bytes(fixed[12..20].try_into().ok()?) as usize;
    let size = u64::from_le_bytes(fixed[20..28].try_into().ok()?) as usize;
    let start = 32usize.checked_add(offset)?;
    let end = start.checked_add(size)?;
    if size == 0 || end > fixed.len() {
        return None;
    }

    let header_crc = crc32::checksum(&fixed[start..end]);
    fixed[28..32].copy_from_slice(&header_crc.to_le_bytes());
    let start_crc = crc32::checksum(&fixed[12..32]);
    fixed[8..12].copy_from_slice(&start_crc.to_le_bytes());
    Some(fixed)
}

fn tar_checksums(bytes: &[u8]) -> Vec<u8> {
    let mut fixed = bytes.to_vec();
    let mut at = 0;
    while at + 512 <= fixed.len() {
        let block = &mut fixed[at..at + 512];
        if block.iter().any(|&b| b != 0) {
            block[148..156].fill(b' ');
            let sum: u32 = block.iter().map(|&b| b as u32).sum();
            block[148..156].copy_from_slice(format!("{sum:06o}\0 ").as_bytes());
        }
        at += 512;
    }
    fixed
}

#[test]
fn a_mutated_seven_zip_header_whose_checksums_still_match_is_parsed_without_panicking() {
    let _watchdog = Watchdog::watching("the 7z header sweep", PATIENCE * 9);

    let dir = common::TempDir::new("malformed-7z-header");
    dir.write("src/a.txt", common::compressible(600));
    dir.write("src/nested/b.bin", common::pseudo_random(300, 3));
    let archive = dir.join("header.7z");
    ttarchive::Archive::new(&archive).create_from([dir.join("src")]).unwrap();
    let bytes = std::fs::read(&archive).unwrap();

    let out = common::TempDir::new("malformed-7z-header-out");
    let mutant = out.join("mutant");
    let into = out.join("into");

    let mut repaired = 0usize;
    for (what, candidate) in mutants(&bytes, 7, 2_000) {
        let Some(fixed) = with_sevenz_crcs(&candidate) else { continue };
        repaired += 1;

        let each = Instant::now();
        std::fs::write(&mutant, &fixed).unwrap();
        let _ = ttarchive::Archive::new(&mutant).entries();
        let _ = std::fs::remove_dir_all(&into);
        let _ = ttarchive::Archive::new(&mutant).extract_to(&into);
        assert!(each.elapsed() < PATIENCE, "7z mutant {what} took {:?}", each.elapsed());
    }

    assert!(repaired > 500, "only {repaired} mutants kept a parseable signature header");
}

#[test]
fn a_mutated_tar_whose_checksums_still_match_is_parsed_without_panicking() {
    let _watchdog = Watchdog::watching("the tar header sweep", PATIENCE * 9);

    let dir = common::TempDir::new("malformed-tar-header");
    dir.write("src/a.txt", common::compressible(600));
    dir.write("src/nested/deep/an-entry-name-long-enough-to-need-an-extended-header-of-its-own.txt", b"three");
    let archive = dir.join("header.tar");
    ttarchive::Archive::new(&archive).create_from([dir.join("src")]).unwrap();
    let bytes = std::fs::read(&archive).unwrap();

    let out = common::TempDir::new("malformed-tar-header-out");
    let mutant = out.join("mutant");
    let into = out.join("into");

    for (what, candidate) in mutants(&bytes, 11, 1_500) {
        let each = Instant::now();
        std::fs::write(&mutant, tar_checksums(&candidate)).unwrap();
        let _ = ttarchive::Archive::new(&mutant).entries();
        let _ = std::fs::remove_dir_all(&into);
        let _ = ttarchive::Archive::new(&mutant).extract_to(&into);
        assert!(each.elapsed() < PATIENCE, "tar mutant {what} took {:?}", each.elapsed());
    }
}
