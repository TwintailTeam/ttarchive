mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use common::{TempDir, compressible, have, pseudo_random, skip};
use ttarchive::codecs::ppmd;
use ttarchive::sevenz::SevenZReader;
use ttarchive::utils::crc32;
use ttarchive::utils::limits::{MAX_CODEC_MEMORY, PREALLOC_MAX, codec_memory, prealloc, zeroed};
use ttarchive::{Archive, Error};

fn legacy(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/methods/legacy").join(name)
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

fn plain_header_ppmd(dir: &TempDir) -> PathBuf {
    dir.write("src/prose.txt", compressible(20_000));
    dir.write("src/noise.bin", pseudo_random(6_000, 3));

    let output = Command::new(common::resolve("7z")).args(["a", "-mhc=off", "-m0=PPMd", "ppmd.7z", "src"]).current_dir(dir.path()).output().expect("7z");
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    dir.join("ppmd.7z")
}

fn claim_memory(archive: &Path, mem_size: u32) -> Vec<u8> {
    let reader = SevenZReader::open(archive).unwrap();
    let properties = reader.header().streams.folders[0].coders[0].properties.clone();
    assert_eq!(properties.len(), 5, "a PPMd coder carries five property bytes");

    let mut bytes = fs::read(archive).unwrap();
    let header_at = 32 + u64::from_le_bytes(bytes[12..20].try_into().unwrap()) as usize;
    let header_len = u64::from_le_bytes(bytes[20..28].try_into().unwrap()) as usize;

    let mut coder = vec![0x03, 0x04, 0x01, 0x05];
    coder.extend_from_slice(&properties);
    let at = header_at + find(&bytes[header_at..header_at + header_len], &coder).expect("the PPMd coder in a plain header");

    bytes[at + 5..at + 9].copy_from_slice(&mem_size.to_le_bytes());

    let digest = crc32::checksum(&bytes[header_at..header_at + header_len]);
    bytes[28..32].copy_from_slice(&digest.to_le_bytes());
    let start = crc32::checksum(&bytes[12..32]);
    bytes[8..12].copy_from_slice(&start.to_le_bytes());
    bytes
}

#[test]
fn a_declared_size_never_buys_more_than_the_preallocation_ceiling() {
    assert_eq!(prealloc(0), 0);
    assert_eq!(prealloc(4_096), 4_096);
    assert_eq!(prealloc(PREALLOC_MAX as u64), PREALLOC_MAX);
    assert_eq!(prealloc(u32::MAX as u64), PREALLOC_MAX);
    assert_eq!(prealloc(u64::MAX), PREALLOC_MAX);
}

#[test]
fn codec_memory_past_the_ceiling_is_refused_as_unsupported() {
    assert_eq!(codec_memory(MAX_CODEC_MEMORY, "test").unwrap(), MAX_CODEC_MEMORY as usize);

    for declared in [MAX_CODEC_MEMORY + 1, u32::MAX - 3, u32::MAX] {
        let error = codec_memory(declared, "test").unwrap_err();
        assert!(error.is_unsupported(), "{declared}: {error}");
    }
}

#[test]
fn an_allocation_the_machine_cannot_make_is_an_error_rather_than_an_abort() {
    let error = zeroed(usize::MAX / 2).unwrap_err();
    assert!(matches!(error, Error::Malformed { .. }), "{error}");

    assert_eq!(zeroed(64).unwrap(), vec![0u8; 64]);
}

#[test]
fn a_ppmd_coder_asking_for_four_gigabytes_is_refused_before_anything_is_allocated() {
    let stream = [0u8; 64];

    for mem_size in [MAX_CODEC_MEMORY + 1, u32::MAX] {
        let started = Instant::now();
        let error = ppmd::h::Reader::new(&stream[..], 1_000, 6, mem_size).err().expect("refused");
        assert!(error.is_unsupported(), "{mem_size}: {error}");
        assert!(started.elapsed() < Duration::from_millis(50), "refusing took {:?}, as if it had allocated first", started.elapsed());

        let mut properties = vec![6u8];
        properties.extend_from_slice(&mem_size.to_le_bytes());
        let error = ppmd::h::from_properties(&stream[..], &properties, 1_000).err().expect("refused");
        assert!(error.is_unsupported(), "{mem_size}: {error}");
    }
}

#[test]
fn a_real_archive_rewritten_to_claim_four_gigabytes_fails_cleanly() {
    if !have("7z") {
        return skip("7z");
    }

    let dir = TempDir::new("mal-ppmd-archive");
    let archive = plain_header_ppmd(&dir);
    Archive::new(&archive).extract_to(dir.join("honest")).expect("the untouched archive extracts");

    let bomb = dir.join("bomb.7z");
    fs::write(&bomb, claim_memory(&archive, u32::MAX)).unwrap();

    let reader = SevenZReader::open(&bomb).expect("the rewritten header still parses");
    assert_eq!(&reader.header().streams.folders[0].coders[0].properties[1..5], &u32::MAX.to_le_bytes());

    let started = Instant::now();
    let error = Archive::new(&bomb).extract_to(dir.join("out")).err().expect("four gigabytes is refused");
    assert!(error.is_unsupported(), "{error}");
    assert!(started.elapsed() < Duration::from_secs(1), "refusing took {:?}", started.elapsed());
}

#[test]
fn a_zip_entry_that_lies_about_its_size_fails_instead_of_reserving_what_it_claims() {
    let dir = TempDir::new("mal-zip-size");

    for name in ["shrink.zip", "reduce.zip", "implode.zip"] {
        let mut bytes = fs::read(legacy(name)).unwrap();

        let central = find(&bytes, &[0x50, 0x4b, 0x01, 0x02]).expect("a central directory entry");
        bytes[central + 24..central + 28].copy_from_slice(&0xFFFF_FFF0u32.to_le_bytes());
        let local = find(&bytes, &[0x50, 0x4b, 0x03, 0x04]).expect("a local header");
        bytes[local + 22..local + 26].copy_from_slice(&0xFFFF_FFF0u32.to_le_bytes());

        let path = dir.join(format!("lying-{name}"));
        fs::write(&path, &bytes).unwrap();

        let started = Instant::now();
        let result = Archive::new(&path).extract_to(dir.join(format!("out-{name}")));
        assert!(result.is_err(), "{name}: an entry four gigabytes short of its own claim must not extract");
        assert!(started.elapsed() < Duration::from_secs(5), "{name}: took {:?}", started.elapsed());
    }
}

#[cfg(target_os = "linux")]
#[test]
fn none_of_the_malicious_inputs_raise_the_resident_set() {
    fn peak_kib() -> u64 {
        let status = fs::read_to_string("/proc/self/status").unwrap();
        status.lines().find_map(|l| l.strip_prefix("VmHWM:")).and_then(|v| v.trim().trim_end_matches("kB").trim().parse().ok()).unwrap_or(0)
    }

    let before = peak_kib();
    let stream = [0u8; 64];

    for _ in 0..16 {
        let _ = ppmd::h::Reader::new(&stream[..], 1_000, 6, u32::MAX);
        let _ = zeroed(usize::MAX / 2);
    }

    let dir = TempDir::new("mal-rss");
    for name in ["shrink.zip", "implode.zip"] {
        let mut bytes = fs::read(legacy(name)).unwrap();
        let central = find(&bytes, &[0x50, 0x4b, 0x01, 0x02]).unwrap();
        bytes[central + 24..central + 28].copy_from_slice(&0xFFFF_FFF0u32.to_le_bytes());
        let path = dir.join(name);
        fs::write(&path, &bytes).unwrap();
        let _ = Archive::new(&path).extract_to(dir.join("out"));
    }

    let grown = peak_kib().saturating_sub(before);
    assert!(grown < 64 * 1024, "malicious sizes raised the peak resident set by {} MiB", grown / 1024);
}
