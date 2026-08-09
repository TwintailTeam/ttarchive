use std::path::{Path, PathBuf};

use ttarchive::platform::policy::{NamePolicy, has_directory_suffix, symlink_target_escapes, to_entry_name, to_relative_path, validate};
use ttarchive::utils::error::PathRejection;

#[test]
fn accepts_ordinary_relative_names() {
    for name in ["a.txt", "dir/a.txt", "a/b/c/d.bin", "./a.txt", "dir//a.txt", "sp ace/x"] {
        assert!(validate(name, NamePolicy::Strict).is_ok(), "rejected {name:?}");
    }
}

#[test]
fn rejects_parent_traversal() {
    for name in ["../evil", "a/../../evil", "../../etc/passwd", "a/b/../../../evil", "..", "dir/.."] {
        assert_eq!(validate(name, NamePolicy::Native), Err(PathRejection::ParentTraversal), "should reject {name:?}");
    }
}

#[test]
fn rejects_backslash_traversal_on_every_platform() {
    for name in ["..\\evil", "a\\..\\..\\evil", "..\\..\\Windows\\System32\\x"] {
        assert_eq!(validate(name, NamePolicy::Native), Err(PathRejection::ParentTraversal), "should reject {name:?}");
    }
}

#[test]
fn rejects_rooted_names() {
    for name in ["/etc/passwd", "\\windows\\x", "\\\\host\\share", "/a/b"] {
        assert_eq!(validate(name, NamePolicy::Native), Err(PathRejection::Absolute), "should reject {name:?}");
    }
}

#[test]
fn rejects_control_characters() {
    assert_eq!(validate("a\0b", NamePolicy::Native), Err(PathRejection::IllegalCharacter));
    assert_eq!(validate("a\nb", NamePolicy::Native), Err(PathRejection::IllegalCharacter));
    assert_eq!(validate("a\u{7f}b", NamePolicy::Native), Err(PathRejection::IllegalCharacter));
}

#[test]
fn rejects_empty_and_dot_only_names() {
    assert_eq!(validate("", NamePolicy::Native), Err(PathRejection::Empty));
    assert_eq!(validate(".", NamePolicy::Native), Err(PathRejection::Empty));
    assert_eq!(validate("./", NamePolicy::Native), Err(PathRejection::Empty));
    assert_eq!(validate("././.", NamePolicy::Native), Err(PathRejection::Empty));

    assert_eq!(validate("///", NamePolicy::Native), Err(PathRejection::Absolute));
}

#[test]
fn drive_prefixes_are_policy_dependent() {
    for name in ["C:\\Windows\\x", "c:relative", "a:b"] {
        assert_eq!(validate(name, NamePolicy::Strict), Err(PathRejection::Absolute), "strict should reject {name:?}");
    }

    assert!(validate("data:stream", NamePolicy::Native).is_ok() || cfg!(windows));
}

#[test]
fn strict_rejects_windows_reserved_names() {
    for name in ["CON", "con", "CON.txt", "aux.tar.gz", "NUL", "COM1", "lpt9.dat", "dir/PRN"] {
        assert_eq!(validate(name, NamePolicy::Strict), Err(PathRejection::IllegalCharacter), "should reject {name:?}");
    }
}

#[test]
fn strict_rejects_windows_illegal_characters() {
    for name in ["a<b", "a>b", "a\"b", "a|b", "a?b", "a*b", "data:stream"] {
        assert_eq!(validate(name, NamePolicy::Strict), Err(PathRejection::IllegalCharacter), "should reject {name:?}");
    }
}

#[test]
fn strict_rejects_trailing_dot_or_space() {
    assert_eq!(validate("foo.", NamePolicy::Strict), Err(PathRejection::IllegalCharacter));
    assert_eq!(validate("foo ", NamePolicy::Strict), Err(PathRejection::IllegalCharacter));
    assert_eq!(validate("dir./x", NamePolicy::Strict), Err(PathRejection::IllegalCharacter));
}

#[test]
#[cfg(unix)]
fn native_allows_unix_legal_names() {
    for name in ["a:b", "CON", "foo.", "a?b", "x<y", "data:stream"] {
        assert!(validate(name, NamePolicy::Native).is_ok(), "rejected {name:?}");
    }
}

#[test]
fn to_relative_path_normalises_separators() {
    let p = to_relative_path("a\\b/c.txt", NamePolicy::Native).unwrap();
    assert_eq!(p, PathBuf::from("a").join("b").join("c.txt"));
    assert!(p.is_relative());

    let p = to_relative_path("./a//b.txt", NamePolicy::Native).unwrap();
    assert_eq!(p, PathBuf::from("a").join("b.txt"));
}

#[test]
fn directory_suffix_detection() {
    assert!(has_directory_suffix("dir/"));
    assert!(has_directory_suffix("dir\\"));
    assert!(!has_directory_suffix("file.txt"));
}

#[test]
fn to_entry_name_produces_forward_slashes() {
    let p: PathBuf = ["a", "b", "c.txt"].iter().collect();
    assert_eq!(to_entry_name(&p, false).as_deref(), Some("a/b/c.txt"));
    assert_eq!(to_entry_name(&p, true).as_deref(), Some("a/b/c.txt/"));

    assert_eq!(to_entry_name(Path::new("../x"), false), None);
}

#[test]
fn detects_escaping_symlink_targets() {
    let root = Path::new("/dest");

    assert!(symlink_target_escapes(root, &root.join("link"), "/etc"));
    assert!(symlink_target_escapes(root, &root.join("link"), "C:\\Windows"));

    assert!(symlink_target_escapes(root, &root.join("link"), "../outside"));
    assert!(symlink_target_escapes(root, &root.join("a").join("link"), "../../../outside"));

    assert!(!symlink_target_escapes(root, &root.join("link"), "sibling"));
    assert!(!symlink_target_escapes(root, &root.join("a").join("link"), "../b"));
    assert!(!symlink_target_escapes(root, &root.join("a").join("link"), "./x/../y"));
}
