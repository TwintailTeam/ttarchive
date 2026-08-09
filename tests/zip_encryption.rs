mod common;

use std::fs;

use common::{TempDir, compressible, pseudo_random};
use ttarchive::codecs::Level;
use ttarchive::utils::error::Error;
use ttarchive::{Archive, ArchiveType, EncryptionMethod};

const SCHEMES: [EncryptionMethod; 4] = [EncryptionMethod::Aes256, EncryptionMethod::Aes192, EncryptionMethod::Aes128, EncryptionMethod::ZipCrypto];

#[test]
fn round_trips_under_every_scheme() {
    for scheme in SCHEMES {
        let src = TempDir::new("enc-src");
        src.write("text.txt", compressible(50_000));
        src.write("noise.bin", pseudo_random(20_000, 9));
        src.write("empty.txt", "");

        let work = TempDir::new("enc-work");
        let archive = work.join("a.zip");

        Archive::new(&archive)
            .set_type(ArchiveType::Zip)
            .set_password("correct horse battery staple")
            .set_encryption(scheme)
            .create_from([src.path()])
            .unwrap_or_else(|e| panic!("{scheme:?}: create failed: {e}"));

        let dest = TempDir::new("enc-dest");
        Archive::new(&archive)
            .set_password("correct horse battery staple")
            .extract_to(dest.path())
            .unwrap_or_else(|e| panic!("{scheme:?}: extract failed: {e}"));

        let stem = src.path().file_name().unwrap().to_string_lossy().into_owned();
        let base = dest.join(&stem);

        assert_eq!(fs::read(base.join("text.txt")).unwrap(), compressible(50_000), "{scheme:?}");
        assert_eq!(fs::read(base.join("noise.bin")).unwrap(), pseudo_random(20_000, 9), "{scheme:?}");
        assert_eq!(fs::read(base.join("empty.txt")).unwrap(), Vec::<u8>::new(), "{scheme:?}");
    }
}

#[test]
fn entries_are_marked_encrypted() {
    for scheme in SCHEMES {
        let src = TempDir::new("mark-src");
        src.write("a.txt", "secret");

        let work = TempDir::new("mark-work");
        let archive = work.join("a.zip");
        Archive::new(&archive).set_type(ArchiveType::Zip).set_password("pw").set_encryption(scheme).create_from([src.join("a.txt")]).unwrap();

        let entries = Archive::new(&archive).entries().unwrap();
        let entry = &entries[0];

        let zip = entry.zip().unwrap();
        assert!(zip.is_encrypted(), "{scheme:?}: general purpose bit 0 must be set");

        match scheme {
            EncryptionMethod::ZipCrypto => {
                assert!(!zip.is_aes(), "ZipCrypto entries carry no 0x9901 field");
            }
            _ => {
                assert!(zip.is_aes(), "{scheme:?}: expected a 0x9901 AES extra field");
                assert_eq!(zip.method_code, 99, "{scheme:?}");
                assert_eq!(zip.crc32, 0, "{scheme:?}: AE-2 must store CRC 0");
            }
        }
    }
}

#[test]
fn archive_bytes_do_not_contain_the_plaintext() {
    for scheme in SCHEMES {
        let src = TempDir::new("leak-src");
        let secret = b"TOPSECRETMARKERVALUE".repeat(50);
        src.write("a.bin", &secret);

        let work = TempDir::new("leak-work");
        let archive = work.join("a.zip");
        Archive::new(&archive)
            .set_type(ArchiveType::Zip)
            .set_level(Level::None)
            .set_password("pw")
            .set_encryption(scheme)
            .create_from([src.join("a.bin")])
            .unwrap();

        let bytes = fs::read(&archive).unwrap();
        let needle = b"TOPSECRETMARKERVALUE";
        let found = bytes.windows(needle.len()).any(|w| w == needle);
        assert!(!found, "{scheme:?}: plaintext found in the archive");
    }
}

#[test]
fn wrong_password_is_rejected() {
    for scheme in SCHEMES {
        let src = TempDir::new("wrong-src");
        src.write("a.txt", compressible(5_000));

        let work = TempDir::new("wrong-work");
        let archive = work.join("a.zip");
        Archive::new(&archive).set_type(ArchiveType::Zip).set_password("the right one").set_encryption(scheme).create_from([src.join("a.txt")]).unwrap();

        let dest = TempDir::new("wrong-dest");
        let err = Archive::new(&archive).set_password("definitely not it").extract_to(dest.path()).expect_err("wrong password must fail");

        assert!(
            matches!(err, Error::WrongPassword | Error::ChecksumMismatch { .. } | Error::SizeMismatch { .. } | Error::Malformed { .. }),
            "{scheme:?}: unexpected error {err:?}"
        );
    }
}

#[test]
fn missing_password_reports_that_one_is_needed() {
    let src = TempDir::new("nopw-src");
    src.write("a.txt", "secret");

    let work = TempDir::new("nopw-work");
    let archive = work.join("a.zip");
    Archive::new(&archive).set_type(ArchiveType::Zip).set_password("pw").create_from([src.join("a.txt")]).unwrap();

    let dest = TempDir::new("nopw-dest");
    let err = Archive::new(&archive).extract_to(dest.path()).expect_err("must require a password");

    assert!(matches!(err, Error::PasswordRequired { .. }), "got {err:?}");
    assert!(err.needs_password());
}

#[test]
fn listing_works_without_a_password() {
    let src = TempDir::new("list-src");
    src.write("visible-name.txt", "secret contents");

    let work = TempDir::new("list-work");
    let archive = work.join("a.zip");
    Archive::new(&archive).set_type(ArchiveType::Zip).set_password("pw").create_from([src.join("visible-name.txt")]).unwrap();

    let entries = Archive::new(&archive).entries().expect("listing needs no password");
    assert_eq!(entries.len(), 1);
    assert!(entries[0].name.ends_with("visible-name.txt"));
}

#[test]
fn modified_ciphertext_is_detected() {
    let src = TempDir::new("tamper-src");
    src.write("a.txt", compressible(20_000));

    let work = TempDir::new("tamper-work");
    let archive = work.join("a.zip");
    Archive::new(&archive).set_type(ArchiveType::Zip).set_password("pw").set_encryption(EncryptionMethod::Aes256).create_from([src.join("a.txt")]).unwrap();

    let mut bytes = fs::read(&archive).unwrap();
    let middle = bytes.len() / 2;
    bytes[middle] ^= 0x01;
    fs::write(&archive, &bytes).unwrap();

    let dest = TempDir::new("tamper-dest");
    let err = Archive::new(&archive).set_password("pw").extract_to(dest.path()).expect_err("tampering must be detected");

    assert!(matches!(err, Error::AuthenticationFailed | Error::Malformed { .. }), "expected an authentication failure, got {err:?}");
}

#[test]
fn each_entry_uses_a_distinct_salt() {
    let src = TempDir::new("salt-src");
    for i in 0..8 {
        src.write(format!("f{i}.bin"), b"the very same bytes every time".repeat(20));
    }

    let work = TempDir::new("salt-work");
    let archive = work.join("a.zip");
    Archive::new(&archive)
        .set_type(ArchiveType::Zip)
        .set_level(Level::None)
        .set_password("pw")
        .set_encryption(EncryptionMethod::Aes256)
        .create_from([src.path()])
        .unwrap();

    let bytes = fs::read(&archive).unwrap();
    let plain = b"the very same bytes every time".repeat(20);
    assert!(!bytes.windows(16).any(|w| w == &plain[..16]), "plaintext is readable in the archive");

    let entries = Archive::new(&archive).set_password("pw").entries().unwrap();
    let files: Vec<_> = entries.iter().filter(|e| e.is_file()).collect();
    assert_eq!(files.len(), 8, "expected eight file entries");

    let mut payloads: Vec<Vec<u8>> = Vec::new();
    for entry in &files {
        let zip = entry.zip().expect("a zip entry");
        let at = zip.local_header_offset as usize;
        let name_len = u16::from_le_bytes([bytes[at + 26], bytes[at + 27]]) as usize;
        let extra_len = u16::from_le_bytes([bytes[at + 28], bytes[at + 29]]) as usize;
        let start = at + 30 + name_len + extra_len;
        payloads.push(bytes[start..start + zip.compressed_size as usize].to_vec());
    }

    for (i, one) in payloads.iter().enumerate() {
        assert!(one.len() > 16, "entry {i} stored almost nothing");
        for (j, other) in payloads.iter().enumerate().skip(i + 1) {
            assert_ne!(one, other, "entries {i} and {j} encrypted identical plaintext to identical bytes");
            assert_ne!(one[..16], other[..16], "entries {i} and {j} share a salt");
        }
    }

    let dest = TempDir::new("salt-dest");
    Archive::new(&archive).set_password("pw").extract_to(dest.path()).unwrap();
}

#[test]
fn encrypted_multivolume_round_trips() {
    use ttarchive::zip::volumes::MIN_VOLUME_SIZE;

    let src = TempDir::new("encmv-src");
    for i in 0..10 {
        src.write(format!("f{i:02}.bin"), pseudo_random(40_000 + i * 13, i as u32 + 1));
    }

    let work = TempDir::new("encmv-work");
    let archive = work.join("a.zip");

    let summary = Archive::new(&archive)
        .set_type(ArchiveType::Zip)
        .set_password("pw")
        .set_encryption(EncryptionMethod::Aes256)
        .set_volume_size(MIN_VOLUME_SIZE)
        .create_from([src.path()])
        .unwrap();
    assert!(summary.volumes > 1, "expected several volumes");

    let dest = TempDir::new("encmv-dest");
    Archive::new(&archive).set_password("pw").extract_to(dest.path()).unwrap();

    let stem = src.path().file_name().unwrap().to_string_lossy().into_owned();
    for i in 0..10 {
        let got = fs::read(dest.join(&stem).join(format!("f{i:02}.bin"))).unwrap();
        assert_eq!(got, pseudo_random(40_000 + i * 13, i as u32 + 1), "file {i}");
    }
}

#[test]
fn large_streamed_entry_round_trips_encrypted() {
    let src = TempDir::new("big-src");
    let big = compressible(20 * 1024 * 1024);
    src.write("big.txt", &big);

    let work = TempDir::new("big-work");
    let archive = work.join("a.zip");

    Archive::new(&archive).set_type(ArchiveType::Zip).set_password("pw").set_encryption(EncryptionMethod::Aes256).create_from([src.join("big.txt")]).unwrap();

    let entries = Archive::new(&archive).entries().unwrap();
    assert!(entries[0].zip().unwrap().has_data_descriptor(), "streamed encrypted entries use bit 3");

    let dest = TempDir::new("big-dest");
    Archive::new(&archive).set_password("pw").extract_to(dest.path()).unwrap();
    assert_eq!(fs::read(dest.join("big.txt")).unwrap(), big);
}

#[test]
fn unencrypted_archive_ignores_a_supplied_password() {
    let src = TempDir::new("plain-src");
    src.write("a.txt", "plain contents");

    let work = TempDir::new("plain-work");
    let archive = work.join("a.zip");
    Archive::new(&archive).set_type(ArchiveType::Zip).create_from([src.join("a.txt")]).unwrap();

    let dest = TempDir::new("plain-dest");
    Archive::new(&archive).set_password("irrelevant").extract_to(dest.path()).unwrap();
    assert_eq!(fs::read(dest.join("a.txt")).unwrap(), b"plain contents");
}

#[test]
fn large_entry_is_actually_encrypted() {
    let src = TempDir::new("bigenc-src");
    let marker = b"UNIQUE-PLAINTEXT-MARKER-abcdef";
    let mut payload = Vec::with_capacity(24 * 1024 * 1024);
    while payload.len() < 24 * 1024 * 1024 {
        payload.extend_from_slice(marker);
    }
    src.write("big.bin", &payload);

    for scheme in SCHEMES {
        let work = TempDir::new("bigenc-work");
        let archive = work.join("a.zip");

        Archive::new(&archive)
            .set_type(ArchiveType::Zip)
            .set_level(Level::None)
            .set_password("pw")
            .set_encryption(scheme)
            .create_from([src.join("big.bin")])
            .unwrap_or_else(|e| panic!("{scheme:?}: {e}"));

        let bytes = fs::read(&archive).unwrap();
        let leaked = bytes.windows(marker.len()).any(|w| w == marker);
        assert!(!leaked, "{scheme:?}: large entry was written in plaintext");

        let entries = Archive::new(&archive).entries().unwrap();
        assert!(entries[0].zip().unwrap().is_encrypted(), "{scheme:?}: bit 0 must be set");

        let dest = TempDir::new("bigenc-dest");
        Archive::new(&archive).set_password("pw").extract_to(dest.path()).unwrap_or_else(|e| panic!("{scheme:?}: extract: {e}"));
        assert_eq!(fs::read(dest.join("big.bin")).unwrap(), payload, "{scheme:?}");
    }
}
