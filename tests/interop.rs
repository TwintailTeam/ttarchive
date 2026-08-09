mod common;

use std::collections::BTreeMap;
use std::path::Path;
use std::process::{Command, Stdio};

use common::{TempDir, compressible, pseudo_random};
use ttarchive::codecs::Level;
use ttarchive::{Archive, ArchiveType, EncryptionMethod};

fn have(tool: &str) -> bool {
    Command::new("sh").arg("-c").arg(format!("command -v {tool}")).stdout(Stdio::null()).stderr(Stdio::null()).status().map(|s| s.success()).unwrap_or(false)
}

fn skip(tool: &str) {
    println!("skipping: {tool} is not installed");
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct FileDigest {
    len: usize,
    crc: u32,
}

impl FileDigest {
    fn of(bytes: &[u8]) -> Self {
        FileDigest { len: bytes.len(), crc: ttarchive::utils::crc32::checksum(bytes) }
    }
}

fn snapshot(root: &Path) -> BTreeMap<String, FileDigest> {
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
                out.insert(rel, FileDigest::of(&std::fs::read(&path).unwrap_or_default()));
            }
        }
    }
    out
}

fn build_source(tag: &str) -> TempDir {
    let src = TempDir::new(tag);
    src.write("data/text.txt", compressible(120_000));
    src.write("data/noise.bin", pseudo_random(80_000, 17));
    src.write("data/nested/deep/leaf.txt", b"leaf contents");
    src.write("empty.txt", "");
    src.write("unicode-\u{00e9}\u{00fc}.txt", "non-ascii name");
    src
}

fn contents_under(root: &Path, stem: &str) -> BTreeMap<String, FileDigest> {
    let nested = root.join(stem);
    if nested.is_dir() { snapshot(&nested) } else { snapshot(root) }
}

#[test]
fn unzip_verifies_our_archives() {
    if !have("unzip") {
        return skip("unzip");
    }

    for level in [Level::None, Level::Fast, Level::Default, Level::Best] {
        let src = build_source("iz-src");
        let work = TempDir::new("iz-work");
        let archive = work.join("a.zip");

        Archive::new(&archive).set_type(ArchiveType::Zip).set_level(level).create_from([src.path()]).unwrap();

        let out = must_run(work.path(), "unzip", &["-t", "a.zip"]);
        assert!(out.contains("No errors detected"), "{level:?}: unzip -t said:\n{out}");
    }
}

#[test]
fn unzip_extracts_our_archives_byte_for_byte() {
    if !have("unzip") {
        return skip("unzip");
    }

    let src = build_source("ize-src");
    let work = TempDir::new("ize-work");
    let archive = work.join("a.zip");

    Archive::new(&archive).set_type(ArchiveType::Zip).create_from([src.path()]).unwrap();

    let dest = TempDir::new("ize-dest");
    must_run(work.path(), "unzip", &["-q", "a.zip", "-d", dest.path().to_str().unwrap()]);

    let stem = src.path().file_name().unwrap().to_string_lossy().into_owned();
    assert_eq!(snapshot(src.path()), contents_under(dest.path(), &stem));
}

#[test]
fn p7zip_extracts_our_archives() {
    if !have("7z") {
        return skip("7z");
    }

    let src = build_source("7ze-src");
    let work = TempDir::new("7ze-work");
    let archive = work.join("a.zip");

    Archive::new(&archive).set_type(ArchiveType::Zip).create_from([src.path()]).unwrap();

    let dest = TempDir::new("7ze-dest");
    must_run(work.path(), "7z", &["x", "-y", "a.zip", &format!("-o{}", dest.path().display())]);

    let stem = src.path().file_name().unwrap().to_string_lossy().into_owned();
    assert_eq!(snapshot(src.path()), contents_under(dest.path(), &stem));
}

#[test]
fn python_zipfile_reads_our_archives() {
    if !have("python3") {
        return skip("python3");
    }

    let src = build_source("py-src");
    let work = TempDir::new("py-work");
    let archive = work.join("a.zip");

    Archive::new(&archive).set_type(ArchiveType::Zip).create_from([src.path()]).unwrap();

    let script = "
import zipfile, sys
z = zipfile.ZipFile('a.zip')
bad = z.testzip()
assert bad is None, f'corrupt entry: {bad}'
names = z.namelist()
assert len(names) > 0
print('OK', len(names))
";
    let out = must_run(work.path(), "python3", &["-c", script]);
    assert!(out.contains("OK"), "python zipfile said:\n{out}");
}

#[test]
fn libarchive_reads_our_archives() {
    if !have("bsdtar") {
        return skip("bsdtar");
    }

    let src = build_source("la-src");
    let work = TempDir::new("la-work");
    let archive = work.join("a.zip");

    Archive::new(&archive).set_type(ArchiveType::Zip).create_from([src.path()]).unwrap();

    let dest = TempDir::new("la-dest");
    must_run(work.path(), "bsdtar", &["-x", "-f", "a.zip", "-C", dest.path().to_str().unwrap()]);

    let stem = src.path().file_name().unwrap().to_string_lossy().into_owned();
    assert_eq!(snapshot(src.path()), contents_under(dest.path(), &stem));
}

#[test]
fn infozip_zip_created_archives_extract() {
    if !have("zip") {
        return skip("zip (install it for extra coverage)");
    }

    let src = build_source("izc-src");
    let work = TempDir::new("izc-work");

    must_run(work.path(), "zip", &["-q", "-r", "a.zip", src.path().to_str().unwrap()]);

    let dest = TempDir::new("izc-dest");
    Archive::new(work.join("a.zip")).extract_to(dest.path()).expect("extract Info-ZIP archive");

    let ours = snapshot(dest.path());
    assert!(!ours.is_empty(), "nothing was extracted");
}

#[test]
fn we_extract_infozip_zipcrypto_archives() {
    if !have("zip") {
        return skip("zip (install it for extra coverage)");
    }

    let src = build_source("ize-crypt-src");
    let work = TempDir::new("ize-crypt-work");

    must_run(work.path(), "zip", &["-q", "-r", "-e", "-P", PASSWORD, "a.zip", src.path().to_str().unwrap()]);

    let dest = TempDir::new("ize-crypt-dest");
    Archive::new(work.join("a.zip")).set_password(PASSWORD).extract_to(dest.path()).expect("extract an Info-ZIP encrypted archive");

    let stem = src.path().strip_prefix("/").unwrap_or(src.path()).to_string_lossy().into_owned();
    assert_eq!(snapshot(src.path()), contents_under(dest.path(), &stem), "contents differ");
}

#[test]
fn we_extract_p7zip_archives() {
    if !have("7z") {
        return skip("7z");
    }

    for (method, tag) in [("Deflate", "defl"), ("Copy", "store")] {
        let src = build_source(&format!("r7z-src-{tag}"));
        let work = TempDir::new(&format!("r7z-work-{tag}"));

        must_run(work.path(), "7z", &["a", "-tzip", &format!("-mm={method}"), "a.zip", src.path().to_str().unwrap()]);

        let dest = TempDir::new(&format!("r7z-dest-{tag}"));
        Archive::new(work.join("a.zip")).extract_to(dest.path()).unwrap_or_else(|e| panic!("{method}: {e}"));

        let stem = src.path().file_name().unwrap().to_string_lossy().into_owned();
        assert_eq!(snapshot(src.path()), contents_under(dest.path(), &stem), "{method}");
    }
}

#[test]
fn we_extract_python_zipfile_archives() {
    if !have("python3") {
        return skip("python3");
    }

    let work = TempDir::new("rpy-work");

    let script = r#"
import zipfile, os
data_text = (b"the quick brown fox " * 6000)
data_noise = bytes((i*37) % 256 for i in range(50000))
with zipfile.ZipFile('a.zip', 'w') as z:
    z.writestr('stored.bin', data_noise, zipfile.ZIP_STORED)
    z.writestr('deflated.txt', data_text, zipfile.ZIP_DEFLATED)
    z.writestr('empty.txt', b'')
    z.writestr('dir/', b'')
    z.writestr('dir/nested.txt', b'nested')
    z.writestr('unicode-éü.txt', b'non-ascii')
open('expect_stored.bin','wb').write(data_noise)
open('expect_deflated.txt','wb').write(data_text)
print('OK')
"#;
    must_run(work.path(), "python3", &["-c", script]);

    let dest = TempDir::new("rpy-dest");
    Archive::new(work.join("a.zip")).extract_to(dest.path()).expect("extract python archive");

    assert_eq!(std::fs::read(dest.join("stored.bin")).unwrap(), std::fs::read(work.join("expect_stored.bin")).unwrap());
    assert_eq!(std::fs::read(dest.join("deflated.txt")).unwrap(), std::fs::read(work.join("expect_deflated.txt")).unwrap());
    assert_eq!(std::fs::read(dest.join("empty.txt")).unwrap(), Vec::<u8>::new());
    assert_eq!(std::fs::read(dest.join("dir/nested.txt")).unwrap(), b"nested");
    assert!(dest.join("unicode-\u{00e9}\u{00fc}.txt").exists(), "UTF-8 name missing");
}

#[test]
fn we_extract_streamed_entries_with_data_descriptors() {
    if !have("python3") {
        return skip("python3");
    }

    let work = TempDir::new("dd-work");

    let script = r#"
import zipfile
payload = (b"streamed payload " * 5000)
class Unseekable:
    def __init__(self, f): self.f = f
    def write(self, b): return self.f.write(b)
    def flush(self): return self.f.flush()
    def tell(self): return self.f.tell()
    def seekable(self): return False
with open('a.zip','wb') as raw:
    with zipfile.ZipFile(Unseekable(raw), 'w', zipfile.ZIP_DEFLATED) as z:
        z.writestr('streamed.txt', payload)
open('expect.txt','wb').write(payload)
print('OK')
"#;
    must_run(work.path(), "python3", &["-c", script]);

    let entries = Archive::new(work.join("a.zip")).entries().unwrap();
    assert!(entries[0].zip().unwrap().has_data_descriptor(), "expected general purpose bit 3");

    let dest = TempDir::new("dd-dest");
    Archive::new(work.join("a.zip")).extract_to(dest.path()).expect("extract streamed archive");
    assert_eq!(std::fs::read(dest.join("streamed.txt")).unwrap(), std::fs::read(work.join("expect.txt")).unwrap());
}

#[test]
fn we_extract_libarchive_archives() {
    if !have("bsdtar") {
        return skip("bsdtar");
    }

    let src = build_source("rla-src");
    let work = TempDir::new("rla-work");

    must_run(
        work.path(),
        "bsdtar",
        &["-a", "-c", "-f", "a.zip", "-C", src.path().parent().unwrap().to_str().unwrap(), src.path().file_name().unwrap().to_str().unwrap()],
    );

    let dest = TempDir::new("rla-dest");
    Archive::new(work.join("a.zip")).extract_to(dest.path()).expect("extract libarchive archive");

    let stem = src.path().file_name().unwrap().to_string_lossy().into_owned();
    assert_eq!(snapshot(src.path()), contents_under(dest.path(), &stem));
}

#[test]
fn we_extract_jar_archives() {
    if !have("jar") {
        return skip("jar");
    }

    let src = build_source("jar-src");
    let work = TempDir::new("jar-work");

    must_run(work.path(), "jar", &["--create", "--file", "a.jar", "-C", src.path().to_str().unwrap(), "."]);

    let dest = TempDir::new("jar-dest");
    Archive::new(work.join("a.jar")).set_type(ArchiveType::Zip).extract_to(dest.path()).expect("extract jar");

    let extracted = snapshot(dest.path());
    assert!(extracted.contains_key("data/text.txt"), "got {:?}", extracted.keys());
    assert_eq!(extracted["data/text.txt"], FileDigest::of(&compressible(120_000)));
}

const PASSWORD: &str = "S3cr3t-p@ssw0rd";

#[test]
fn we_extract_p7zip_aes_archives() {
    if !have("7z") {
        return skip("7z");
    }

    for strength in ["AES256", "AES192", "AES128"] {
        let src = build_source(&format!("aes-src-{strength}"));
        let work = TempDir::new(&format!("aes-work-{strength}"));

        must_run(work.path(), "7z", &["a", "-tzip", &format!("-p{PASSWORD}"), &format!("-mem={strength}"), "a.zip", src.path().to_str().unwrap()]);

        let dest = TempDir::new(&format!("aes-dest-{strength}"));
        Archive::new(work.join("a.zip")).set_password(PASSWORD).extract_to(dest.path()).unwrap_or_else(|e| panic!("{strength}: {e}"));

        let stem = src.path().file_name().unwrap().to_string_lossy().into_owned();
        assert_eq!(snapshot(src.path()), contents_under(dest.path(), &stem), "{strength}");
    }
}

#[test]
fn we_extract_p7zip_zipcrypto_archives() {
    if !have("7z") {
        return skip("7z");
    }

    let src = build_source("zc-src");
    let work = TempDir::new("zc-work");

    must_run(work.path(), "7z", &["a", "-tzip", &format!("-p{PASSWORD}"), "-mem=ZipCrypto", "a.zip", src.path().to_str().unwrap()]);

    let dest = TempDir::new("zc-dest");
    Archive::new(work.join("a.zip")).set_password(PASSWORD).extract_to(dest.path()).expect("extract ZipCrypto archive");

    let stem = src.path().file_name().unwrap().to_string_lossy().into_owned();
    assert_eq!(snapshot(src.path()), contents_under(dest.path(), &stem));
}

#[test]
fn p7zip_extracts_our_aes_archives() {
    if !have("7z") {
        return skip("7z");
    }

    for scheme in [EncryptionMethod::Aes256, EncryptionMethod::Aes192, EncryptionMethod::Aes128] {
        let src = build_source("oaes-src");
        let work = TempDir::new("oaes-work");
        let archive = work.join("a.zip");

        Archive::new(&archive).set_type(ArchiveType::Zip).set_password(PASSWORD).set_encryption(scheme).create_from([src.path()]).unwrap();

        let dest = TempDir::new("oaes-dest");
        let (ok, out) = run(work.path(), "7z", &["x", "-y", &format!("-p{PASSWORD}"), "a.zip", &format!("-o{}", dest.path().display())]);
        assert!(ok, "{scheme:?}: 7z could not extract our archive:\n{out}");

        let stem = src.path().file_name().unwrap().to_string_lossy().into_owned();
        assert_eq!(snapshot(src.path()), contents_under(dest.path(), &stem), "{scheme:?}");
    }
}

#[test]
fn p7zip_extracts_our_zipcrypto_archives() {
    if !have("7z") {
        return skip("7z");
    }

    let src = build_source("ozc-src");
    let work = TempDir::new("ozc-work");
    let archive = work.join("a.zip");

    Archive::new(&archive).set_type(ArchiveType::Zip).set_password(PASSWORD).set_encryption(EncryptionMethod::ZipCrypto).create_from([src.path()]).unwrap();

    let dest = TempDir::new("ozc-dest");
    let (ok, out) = run(work.path(), "7z", &["x", "-y", &format!("-p{PASSWORD}"), "a.zip", &format!("-o{}", dest.path().display())]);
    assert!(ok, "7z could not extract our ZipCrypto archive:\n{out}");

    let stem = src.path().file_name().unwrap().to_string_lossy().into_owned();
    assert_eq!(snapshot(src.path()), contents_under(dest.path(), &stem));
}

#[test]
fn unzip_extracts_our_zipcrypto_archives() {
    if !have("unzip") {
        return skip("unzip");
    }

    let src = build_source("uzc-src");
    let work = TempDir::new("uzc-work");
    let archive = work.join("a.zip");

    Archive::new(&archive).set_type(ArchiveType::Zip).set_password(PASSWORD).set_encryption(EncryptionMethod::ZipCrypto).create_from([src.path()]).unwrap();

    let out = must_run(work.path(), "unzip", &["-t", "-P", PASSWORD, "a.zip"]);
    assert!(out.contains("No errors detected"), "unzip -t said:\n{out}");
}

#[test]
fn p7zip_rejects_a_wrong_password_on_our_archives() {
    if !have("7z") {
        return skip("7z");
    }

    let src = TempDir::new("bad-src");
    src.write("a.txt", compressible(10_000));

    let work = TempDir::new("bad-work");
    let archive = work.join("a.zip");
    Archive::new(&archive).set_type(ArchiveType::Zip).set_password(PASSWORD).create_from([src.join("a.txt")]).unwrap();

    let dest = TempDir::new("bad-dest");
    let (ok, _) = run(work.path(), "7z", &["x", "-y", "-pWRONG", "a.zip", &format!("-o{}", dest.path().display())]);
    assert!(!ok, "7z should have refused the wrong password");
}

#[test]
fn zipinfo_reports_expected_attributes() {
    if !have("zipinfo") {
        return skip("zipinfo");
    }

    let src = TempDir::new("zi-src");
    src.write("plain.txt", "contents");
    src.write("script.sh", "#!/bin/sh\necho hi\n");

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(src.join("script.sh"), std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    let work = TempDir::new("zi-work");
    let archive = work.join("a.zip");
    Archive::new(&archive).set_type(ArchiveType::Zip).create_from([src.path()]).unwrap();

    let out = must_run(work.path(), "zipinfo", &["-l", "a.zip"]);

    #[cfg(unix)]
    assert!(out.lines().any(|l| l.contains("script.sh") && l.contains("rwxr-xr-x")), "expected 0755 on script.sh:\n{out}");
    assert!(out.lines().any(|l| l.contains("plain.txt") && l.contains("rw-r--r--")), "expected 0644 on plain.txt:\n{out}");
}

#[test]
fn tools_recognise_our_zip64_archives() {
    if !have("7z") {
        return skip("7z");
    }

    let src = TempDir::new("z64-src");
    for i in 0..70_000u32 {
        src.write(format!("d{}/f{i}.txt", i % 100), i.to_string());
    }

    let work = TempDir::new("z64-work");
    let archive = work.join("a.zip");
    Archive::new(&archive).set_type(ArchiveType::Zip).create_from([src.path()]).unwrap();

    let entries = Archive::new(&archive).entries().unwrap();
    assert!(entries.len() >= 70_000, "got {} entries", entries.len());

    let out = must_run(work.path(), "7z", &["t", "a.zip"]);
    assert!(out.contains("Everything is Ok"), "7z said:\n{out}");
}
