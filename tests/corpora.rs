mod common;

use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;

use common::{Shape, TempDir, have, shapes, snapshot, write_shape};
use ttarchive::{Archive, ArchiveType, Method};

fn shape(name: &str) -> Shape {
    shapes().into_iter().find(|s| s.name == name).unwrap_or_else(|| panic!("no shape {name}"))
}

fn expected(shape: &Shape) -> BTreeMap<String, Vec<u8>> {
    shape.files.iter().cloned().collect()
}

fn compare(label: &str, found: &BTreeMap<String, Vec<u8>>, wanted: &BTreeMap<String, Vec<u8>>) {
    let missing: Vec<&String> = wanted.keys().filter(|k| !found.contains_key(*k)).collect();
    let extra: Vec<&String> = found.keys().filter(|k| !wanted.contains_key(*k)).collect();
    assert!(missing.is_empty() && extra.is_empty(), "{label}: missing {missing:?}, unexpected {extra:?}");

    for (name, bytes) in wanted {
        let got = &found[name];
        assert_eq!(got.len(), bytes.len(), "{label}: {name} has the wrong length");
        assert!(got == bytes, "{label}: {name} came back with different bytes");
    }
}

fn run(dir: &Path, tool: &str, args: &[&str]) {
    let output = Command::new(common::resolve(tool)).args(args).current_dir(dir).output().unwrap_or_else(|e| panic!("failed to run {tool}: {e}"));
    assert!(output.status.success(), "{tool} {args:?} failed:\n{}{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
}

fn through_every_writable_format(name: &str) {
    let shape = shape(name);
    let wanted = expected(&shape);
    let dir = TempDir::new(&format!("corpus-{name}"));
    let root = write_shape(&dir, "input", &shape);

    for kind in ArchiveType::ALL.into_iter().filter(|k| k.can_write()) {
        let archive = dir.join(format!("archive{}", kind.extension()));
        let label = format!("{name} as {}", kind.extension());

        Archive::new(&archive).set_type(kind).create_from([&root]).unwrap_or_else(|e| panic!("{label}: create: {e}"));

        let out = dir.join(format!("out{}", kind.extension()));
        Archive::new(&archive).set_type(kind).extract_to(&out).unwrap_or_else(|e| panic!("{label}: extract: {e}"));

        compare(&label, &snapshot(&out.join("input")), &wanted);
        let _ = std::fs::remove_dir_all(&out);
        let _ = std::fs::remove_file(&archive);
    }
}

#[test]
fn stored_chunks_survive_every_writable_format() {
    through_every_writable_format("stored-chunks");
}

#[test]
fn many_small_files_survive_every_writable_format() {
    through_every_writable_format("many-small");
}

#[test]
fn sizes_on_every_chunk_boundary_survive_every_writable_format() {
    through_every_writable_format("edge-sizes");
}

#[test]
fn pathological_runs_survive_every_writable_format() {
    through_every_writable_format("pathological");
}

#[test]
fn data_shaped_like_branch_instructions_survives_every_writable_format() {
    through_every_writable_format("filter-bait");
}

#[test]
fn already_compressed_data_survives_every_writable_format() {
    through_every_writable_format("already-compressed");
}

#[test]
fn every_shape_survives_every_7z_coder_encryption_and_volumes() {
    for shape in shapes() {
        let wanted = expected(&shape);
        let dir = TempDir::new(&format!("corpus-7z-{}", shape.name));
        let root = write_shape(&dir, "input", &shape);

        let variants: [(&str, Option<Method>, bool, bool); 6] = [
            ("lzma2", None, false, false),
            ("copy", Some(Method::Store), false, false),
            ("deflate", Some(Method::Deflate), false, false),
            ("bzip2", Some(Method::Bzip2), false, false),
            ("encrypted", None, true, false),
            ("volumes", None, false, true),
        ];

        for (variant, method, encrypted, volumes) in variants {
            let label = format!("{} as 7z {variant}", shape.name);
            let archive = dir.join(format!("{variant}.7z"));

            let mut writer = Archive::new(&archive);
            if let Some(method) = method {
                writer = writer.set_method(method);
            }
            if encrypted {
                writer = writer.set_password("corpora");
            }
            if volumes {
                writer = writer.set_volume_size(1 << 20);
            }
            writer.create_from([&root]).unwrap_or_else(|e| panic!("{label}: create: {e}"));

            let out = dir.join(format!("out-{variant}"));
            let mut reader = Archive::new(&archive);
            if encrypted {
                reader = reader.set_password("corpora");
            }
            reader.extract_to(&out).unwrap_or_else(|e| panic!("{label}: extract: {e}"));

            compare(&label, &snapshot(&out.join("input")), &wanted);
            let _ = std::fs::remove_dir_all(&out);
        }
    }
}

#[test]
fn seven_zip_reads_every_shape_we_write() {
    if !have("7z") {
        eprintln!("skipping: 7z is not installed");
        return;
    }

    for shape in shapes() {
        let wanted = expected(&shape);
        let dir = TempDir::new(&format!("corpus-to-7z-{}", shape.name));
        let root = write_shape(&dir, "input", &shape);

        for (variant, method) in [("lzma2", None), ("bzip2", Some(Method::Bzip2)), ("copy", Some(Method::Store))] {
            let name = format!("{variant}.7z");
            let mut writer = Archive::new(dir.join(&name));
            if let Some(method) = method {
                writer = writer.set_method(method);
            }
            writer.create_from([&root]).unwrap();

            let out = format!("out-{variant}");
            run(dir.path(), "7z", &["x", "-y", "-bso0", "-bsp0", &format!("-o{out}"), &name]);
            compare(&format!("7-Zip reading {} {variant}", shape.name), &snapshot(&dir.join(&out).join("input")), &wanted);
        }
    }
}

#[test]
fn we_read_every_shape_seven_zip_writes() {
    if !have("7z") {
        eprintln!("skipping: 7z is not installed");
        return;
    }

    for shape in shapes() {
        let wanted = expected(&shape);
        let dir = TempDir::new(&format!("corpus-from-7z-{}", shape.name));
        write_shape(&dir, "input", &shape);

        for (variant, switches) in [
            ("lzma2", &["-mhc=on"][..]),
            ("small-dictionary", &["-md=1m"][..]),
            ("ppmd", &["-m0=PPMd"][..]),
            ("bzip2", &["-m0=BZip2"][..]),
            ("bcj2", &["-m0=BCJ2"][..]),
            ("non-solid", &["-ms=off"][..]),
        ] {
            let name = format!("{variant}.7z");
            let mut args = vec!["a", "-bso0", "-bsp0"];
            args.extend_from_slice(switches);
            args.push(&name);
            args.push("input");
            run(dir.path(), "7z", &args);

            let out = dir.join(format!("out-{variant}"));
            Archive::new(dir.join(&name)).extract_to(&out).unwrap_or_else(|e| panic!("{} from 7-Zip {variant}: {e}", shape.name));
            compare(&format!("{} from 7-Zip {variant}", shape.name), &snapshot(&out.join("input")), &wanted);
            let _ = std::fs::remove_dir_all(&out);
        }
    }
}

#[test]
fn system_tar_and_zip_agree_with_us_on_every_shape() {
    let tools = have("tar") && have("zip") && have("unzip");
    if !tools {
        eprintln!("skipping: tar, zip or unzip is not installed");
        return;
    }

    for shape in shapes() {
        let wanted = expected(&shape);
        let dir = TempDir::new(&format!("corpus-tools-{}", shape.name));
        let root = write_shape(&dir, "input", &shape);

        Archive::new(dir.join("ours.tar.gz")).create_from([&root]).unwrap();
        std::fs::create_dir_all(dir.join("tar-out")).unwrap();
        run(dir.path(), "tar", &["-xzf", "ours.tar.gz", "-C", "tar-out"]);
        compare(&format!("tar reading our {}", shape.name), &snapshot(&dir.join("tar-out/input")), &wanted);

        Archive::new(dir.join("ours.zip")).create_from([&root]).unwrap();
        run(dir.path(), "unzip", &["-qq", "-o", "ours.zip", "-d", "unzip-out"]);
        compare(&format!("unzip reading our {}", shape.name), &snapshot(&dir.join("unzip-out/input")), &wanted);

        run(dir.path(), "tar", &["-czf", "theirs.tar.gz", "input"]);
        Archive::new(dir.join("theirs.tar.gz")).extract_to(dir.join("from-tar")).unwrap_or_else(|e| panic!("{}: {e}", shape.name));
        compare(&format!("our reading of tar's {}", shape.name), &snapshot(&dir.join("from-tar/input")), &wanted);

        run(dir.path(), "zip", &["-q", "-r", "theirs.zip", "input"]);
        Archive::new(dir.join("theirs.zip")).extract_to(dir.join("from-zip")).unwrap_or_else(|e| panic!("{}: {e}", shape.name));
        compare(&format!("our reading of zip's {}", shape.name), &snapshot(&dir.join("from-zip/input")), &wanted);
    }
}
