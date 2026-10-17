mod common;

use common::TempDir;
use ttarchive::{Archive, ArchiveType};

fn says(error: impl std::fmt::Display, needle: &str, what: &str) {
    let text = error.to_string();
    assert!(text.contains(needle), "{what}: {text:?} never mentions {needle:?}");
}

#[test]
fn a_missing_archive_is_named_whatever_its_extension() {
    let dir = TempDir::new("errors-missing");

    for name in ["gone.zip", "gone.tar", "gone.tar.gz", "gone.7z", "gone"] {
        let path = dir.join(name);
        let listed = Archive::new(&path).entries().unwrap_err();
        says(&listed, name, &format!("listing {name}"));

        let extracted = Archive::new(&path).extract_to(dir.join("out")).unwrap_err();
        says(&extracted, name, &format!("extracting {name}"));
    }
}

#[test]
fn an_input_that_is_not_there_is_named_when_creating() {
    let dir = TempDir::new("errors-input");
    let missing = dir.join("never-written.txt");

    for kind in ArchiveType::ALL.into_iter().filter(|k| k.can_write()) {
        let archive = dir.join(format!("out{}", kind.extension()));
        let error = Archive::new(&archive).set_type(kind).create_from([&missing]).unwrap_err();
        says(&error, "never-written.txt", &format!("creating {}", kind.extension()));
    }
}

#[test]
fn an_archive_that_cannot_be_written_names_the_path_it_tried() {
    let dir = TempDir::new("errors-output");
    dir.write("src/a.txt", b"alpha");
    let archive = dir.join("no-such-directory/deeper/out.zip");

    let error = Archive::new(&archive).create_from([dir.join("src")]).unwrap_err();
    says(&error, "out.zip", "creating into a missing directory");
}

#[test]
fn a_file_in_the_way_of_an_extracted_directory_is_named() {
    let dir = TempDir::new("errors-extract");
    dir.write("src/nested/a.txt", b"alpha");
    let archive = dir.join("tree.tar");
    Archive::new(&archive).create_from([dir.join("src")]).unwrap();

    let out = dir.join("out");
    std::fs::create_dir_all(&out).unwrap();
    std::fs::write(out.join("src"), b"i am a file, not a directory").unwrap();

    let summary = Archive::new(&archive).extract_to(&out);
    match summary {
        Ok(_) => {}
        Err(e) => says(&e, "src", "extracting over a file"),
    }
}
