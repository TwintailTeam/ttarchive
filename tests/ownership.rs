mod common;

use std::fs::File;
use std::io::BufWriter;
use std::path::{Path, PathBuf};

use common::TempDir;
use ttarchive::platform::EntryMeta;
use ttarchive::platform::accounts::Accounts;
use ttarchive::tar::{TarReader, TarWriter};
use ttarchive::{Archive, ExtractOptions, Ownership};

fn owned(mut meta: EntryMeta, uid: u32, gid: u32, user: Option<&str>, group: Option<&str>) -> EntryMeta {
    meta.uid = Some(uid);
    meta.gid = Some(gid);
    meta.user = user.map(str::to_owned);
    meta.group = group.map(str::to_owned);
    meta
}

fn write_tar(path: &Path, entries: &[(&str, EntryMeta, &[u8], &str)]) {
    let mut tar = TarWriter::new(BufWriter::new(File::create(path).unwrap()));
    for (name, meta, body, link) in entries {
        tar.add_entry(name, meta, body, link).unwrap();
    }
    tar.finish().unwrap();
}

fn foreign_tar(dir: &TempDir) -> PathBuf {
    let archive = dir.join("foreign.tar");
    write_tar(
        &archive,
        &[
            ("d/", owned(EntryMeta::directory(), 0, 0, Some("root"), Some("root")), b"", ""),
            ("d/a.txt", owned(EntryMeta::file(), 0, 0, Some("root"), Some("root")), b"alpha", ""),
            ("b.txt", owned(EntryMeta::file(), 0, 0, Some("root"), Some("root")), b"beta", ""),
        ],
    );
    archive
}

#[test]
fn ignore_is_the_default_and_touches_nobody() {
    assert_eq!(ExtractOptions::default().ownership, Ownership::Ignore);

    let dir = TempDir::new("owner-default");
    let archive = foreign_tar(&dir);
    let summary = Archive::new(&archive).extract_to(dir.join("out")).unwrap();

    assert_eq!(summary.owners_not_restored, 0);
    assert_eq!(std::fs::read(dir.join("out/d/a.txt")).unwrap(), b"alpha");
}

#[test]
fn an_owner_the_system_refuses_is_counted_and_the_files_still_arrive() {
    if running_as_root() {
        eprintln!("skipping: root may give files to anyone");
        return;
    }

    for ownership in [Ownership::Ids, Ownership::Names] {
        let dir = TempDir::new(&format!("owner-refused-{ownership:?}"));
        let archive = foreign_tar(&dir);
        let out = dir.join("out");

        let summary = Archive::new(&archive).set_ownership(ownership).extract_to(&out).unwrap_or_else(|e| panic!("{ownership:?}: {e}"));

        assert_eq!(summary.owners_not_restored, 3, "{ownership:?}");
        assert_eq!(summary.files, 2, "{ownership:?}");
        assert_eq!(std::fs::read(out.join("d/a.txt")).unwrap(), b"alpha");
        assert_eq!(std::fs::read(out.join("b.txt")).unwrap(), b"beta");
    }
}

#[test]
fn an_archive_that_records_no_owner_counts_nothing() {
    let dir = TempDir::new("owner-unrecorded");
    dir.write("src/a.txt", b"alpha");
    let archive = dir.join("plain.7z");
    Archive::new(&archive).create_from([dir.join("src")]).unwrap();

    let summary = Archive::new(&archive).set_ownership(Ownership::Ids).extract_to(dir.join("out")).unwrap();

    assert_eq!(summary.owners_not_restored, 0);
    assert_eq!(std::fs::read(dir.join("out/src/a.txt")).unwrap(), b"alpha");
}

#[test]
fn the_account_tables_are_read_like_passwd_and_group() {
    let passwd = "# comment:x:5:5\nroot:x:0:0:root:/root:/bin/sh\n+nis:x:7:7::/:\nbroken line\nnobody:x:notanumber:1\ntukan:x:1000:1000::/home/tukan:/bin/zsh\nduplicate:x:1000:1000::/:\ntukan:x:2000:2000::/:\n";
    let group = "wheel:x:10:tukan,root\nusers:x:100:\n:x:9:\n";
    let accounts = Accounts::parse(passwd, group);

    assert_eq!(accounts.user_id("root"), Some(0));
    assert_eq!(accounts.user_id("tukan"), Some(1000));
    assert_eq!(accounts.user_name(1000), Some("tukan"));
    assert_eq!(accounts.user_id("duplicate"), Some(1000));
    assert_eq!(accounts.user_id("+nis"), None);
    assert_eq!(accounts.user_id("# comment"), None);
    assert_eq!(accounts.user_id("nobody"), None);
    assert_eq!(accounts.group_id("wheel"), Some(10));
    assert_eq!(accounts.group_name(100), Some("users"));
    assert_eq!(accounts.group_name(9), None);
    assert_eq!(accounts.user_name(4242), None);
}

#[test]
fn a_long_owner_name_survives_in_an_extended_header() {
    let dir = TempDir::new("owner-long-name");
    let archive = dir.join("long.tar");
    let user = "a-user-name-far-longer-than-thirty-two-bytes";
    let group = "short";
    write_tar(&archive, &[("a.txt", owned(EntryMeta::file(), 1234, 5678, Some(user), Some(group)), b"alpha", "")]);

    let mut reader = TarReader::new(File::open(&archive).unwrap());
    let entry = reader.next_entry().unwrap().unwrap();
    assert_eq!(entry.meta.user.as_deref(), Some(user));
    assert_eq!(entry.meta.group.as_deref(), Some(group));

    let listed = Archive::new(&archive).entries().unwrap();
    assert_eq!(listed[0].user(), Some(user));
    assert_eq!(listed[0].group(), Some(group));
    assert_eq!(listed[0].uid(), Some(1234));
}

#[test]
fn permissions_and_timestamps_are_switched_separately() {
    let dir = TempDir::new("owner-switches");
    let file = dir.write("src/a.txt", b"alpha");
    File::options()
        .write(true)
        .open(&file)
        .unwrap()
        .set_times(std::fs::FileTimes::new().set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000_000)))
        .unwrap();
    let mut permissions = std::fs::metadata(&file).unwrap().permissions();
    permissions.set_readonly(true);
    std::fs::set_permissions(&file, permissions).unwrap();

    let archive = dir.join("switches.tar");
    Archive::new(&archive).create_from([dir.join("src")]).unwrap();

    let times_only = dir.join("times-only");
    Archive::new(&archive).set_preserve_permissions(false).extract_to(&times_only).unwrap();
    let restored = times_only.join("src/a.txt");
    assert!(!std::fs::metadata(&restored).unwrap().permissions().readonly(), "permissions were restored with preserve_permissions off");
    assert_eq!(modified(&restored), 1_000_000_000, "times were not restored with only permissions off");

    let permissions_only = dir.join("permissions-only");
    Archive::new(&archive).set_preserve_timestamps(false).extract_to(&permissions_only).unwrap();
    let restored = permissions_only.join("src/a.txt");
    assert!(std::fs::metadata(&restored).unwrap().permissions().readonly(), "permissions were not restored with only timestamps off");
    assert!(modified(&restored) > 1_000_000_000 + 86_400, "times were restored with preserve_timestamps off");

    for path in [&file, &restored] {
        let mut permissions = std::fs::metadata(path).unwrap().permissions();
        permissions.set_readonly(false);
        std::fs::set_permissions(path, permissions).unwrap();
    }
}

fn modified(path: &Path) -> u64 {
    std::fs::metadata(path).unwrap().modified().unwrap().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs()
}

#[cfg(unix)]
fn running_as_root() -> bool {
    use std::os::unix::fs::MetadataExt;
    let dir = TempDir::new("owner-whoami");
    std::fs::metadata(dir.write("probe", b"")).unwrap().uid() == 0
}

#[cfg(not(unix))]
fn running_as_root() -> bool {
    false
}

#[cfg(unix)]
mod unix {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    use super::*;

    pub struct Me {
        pub uid: u32,
        pub gid: u32,
        pub name: Option<String>,
    }

    pub fn me() -> Me {
        let dir = TempDir::new("owner-me");
        let probe = std::fs::metadata(dir.write("probe", b"")).unwrap();
        let name = Accounts::load().user_name(probe.uid()).map(str::to_owned);
        Me { uid: probe.uid(), gid: probe.gid(), name }
    }

    pub fn another_group(me: &Me) -> Option<(String, u32)> {
        let text = std::fs::read_to_string("/etc/group").ok()?;
        text.lines().find_map(|line| {
            let fields: Vec<&str> = line.split(':').collect();
            let [name, _, gid, members] = fields[..] else { return None };
            let gid: u32 = gid.parse().ok()?;
            let member = me.uid == 0 || me.name.as_deref().is_some_and(|mine| members.split(',').any(|m| m == mine));
            (gid != me.gid && member && !name.is_empty()).then(|| (name.to_owned(), gid))
        })
    }

    pub fn owner_of(path: &Path) -> (u32, u32) {
        let md = std::fs::symlink_metadata(path).unwrap();
        (md.uid(), md.gid())
    }

    pub fn mode_of(path: &Path) -> u32 {
        std::fs::metadata(path).unwrap().permissions().mode() & 0o7777
    }

    pub fn with_mode(mut meta: EntryMeta, mode: u32) -> EntryMeta {
        meta.unix_mode = Some(mode);
        meta
    }
}

#[cfg(unix)]
#[test]
fn ids_hand_every_kind_of_entry_to_the_recorded_group() {
    use unix::*;

    let me = me();
    let Some((_, other)) = another_group(&me) else {
        eprintln!("skipping: this user belongs to no second group to hand files to");
        return;
    };

    let dir = TempDir::new("owner-ids");
    let archive = dir.join("ids.tar");
    write_tar(
        &archive,
        &[
            ("d/", owned(EntryMeta::directory(), me.uid, other, None, None), b"", ""),
            ("d/a.txt", owned(EntryMeta::file(), me.uid, other, None, None), b"alpha", ""),
            ("d/link", owned(EntryMeta::symlink(), me.uid, other, None, None), b"", "a.txt"),
        ],
    );

    let out = dir.join("out");
    let summary = Archive::new(&archive).set_ownership(Ownership::Ids).extract_to(&out).unwrap();

    assert_eq!(summary.owners_not_restored, 0);
    assert_eq!(owner_of(&out.join("d")), (me.uid, other));
    assert_eq!(owner_of(&out.join("d/a.txt")), (me.uid, other));
    assert_eq!(owner_of(&out.join("d/link")), (me.uid, other), "the link itself should change hands");

    let untouched = dir.join("untouched");
    Archive::new(&archive).extract_to(&untouched).unwrap();
    assert_eq!(owner_of(&untouched.join("d/a.txt")), (me.uid, me.gid));
}

#[cfg(unix)]
#[test]
fn names_win_over_the_recorded_numbers_and_fall_back_to_them() {
    use unix::*;

    let me = me();
    let (Some(user), Some((group, other))) = (me.name.clone(), another_group(&me)) else {
        eprintln!("skipping: this user has no passwd entry or no second group");
        return;
    };

    let dir = TempDir::new("owner-names");
    let archive = dir.join("names.tar");
    let bogus = 3_999_999;
    write_tar(
        &archive,
        &[
            ("named.txt", owned(EntryMeta::file(), bogus, bogus, Some(&user), Some(&group)), b"named", ""),
            ("unknown.txt", owned(EntryMeta::file(), me.uid, other, Some("no-such-user-here"), Some("no-such-group-here")), b"unknown", ""),
        ],
    );

    let out = dir.join("out");
    let summary = Archive::new(&archive).set_ownership(Ownership::Names).extract_to(&out).unwrap();

    assert_eq!(summary.owners_not_restored, 0);
    assert_eq!(owner_of(&out.join("named.txt")), (me.uid, other));
    assert_eq!(owner_of(&out.join("unknown.txt")), (me.uid, other));

    if !running_as_root() {
        let by_number = Archive::new(&archive).set_ownership(Ownership::Ids).extract_to(dir.join("by-number")).unwrap();
        assert_eq!(by_number.owners_not_restored, 1, "the bogus numbers should have been refused");
    }
}

#[cfg(unix)]
#[test]
fn restoring_the_owner_does_not_strip_setuid_and_setgid() {
    use unix::*;

    let me = me();
    let group = another_group(&me).map_or(me.gid, |(_, gid)| gid);

    let dir = TempDir::new("owner-setuid");
    let archive = dir.join("setuid.tar");
    write_tar(&archive, &[("tool", with_mode(owned(EntryMeta::file(), me.uid, group, None, None), 0o6755), b"#!/bin/sh\n", "")]);

    let out = dir.join("out");
    let summary = Archive::new(&archive).set_ownership(Ownership::Ids).extract_to(&out).unwrap();

    assert_eq!(summary.owners_not_restored, 0);
    assert_eq!(owner_of(&out.join("tool")), (me.uid, group));
    assert_eq!(mode_of(&out.join("tool")), 0o6755);
}

#[cfg(unix)]
#[test]
fn every_format_that_records_an_owner_carries_it_back() {
    use ttarchive::ArchiveType;
    use unix::*;

    let me = me();
    let Some((group, other)) = another_group(&me) else {
        eprintln!("skipping: this user belongs to no second group to hand files to");
        return;
    };

    let dir = TempDir::new("owner-formats");
    let file = dir.write("src/a.txt", b"alpha");
    std::os::unix::fs::chown(&file, None, Some(other)).unwrap();

    for kind in ArchiveType::ALL.into_iter().filter(|k| k.can_write()) {
        let label = kind.extension();
        let archive = dir.join(format!("archive{label}"));
        Archive::new(&archive).set_type(kind).create_from([dir.join("src")]).unwrap_or_else(|e| panic!("{label}: {e}"));

        let listed = Archive::new(&archive).set_type(kind).entries().unwrap();
        let entry = listed.iter().find(|e| e.name.ends_with("a.txt")).unwrap();

        let out = dir.join(format!("out{label}"));
        let summary = Archive::new(&archive).set_type(kind).set_ownership(Ownership::Ids).extract_to(&out).unwrap_or_else(|e| panic!("{label}: {e}"));
        assert_eq!(summary.owners_not_restored, 0, "{label}");

        let restored = owner_of(&out.join("src/a.txt"));
        match kind {
            ArchiveType::SevenZ => {
                assert_eq!(entry.gid(), None, "{label}");
                assert_eq!(restored, (me.uid, me.gid), "{label}");
            }
            ArchiveType::Zip => {
                assert_eq!(entry.gid(), Some(other), "{label}");
                assert_eq!(restored, (me.uid, other), "{label}");
            }
            _ => {
                assert_eq!(entry.gid(), Some(other), "{label}");
                assert_eq!(entry.group(), Some(group.as_str()), "{label}");
                assert_eq!(entry.user(), me.name.as_deref(), "{label}");
                assert_eq!(restored, (me.uid, other), "{label}");
            }
        }
    }
}
