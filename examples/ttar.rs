use std::process::ExitCode;

use ttarchive::codecs::{Level, Method};
use ttarchive::utils::datetime::civil_from_unix;
use ttarchive::{Archive, ArchiveType, EncryptionMethod, Entry, Overwrite, Ownership, UnsafeEntries};

const USAGE: &str = "\
usage: ttar create  <archive> <inputs...> [options]
       ttar extract <archive> [dest]      [options]
       ttar list    <archive>             [options]

any command:
  --type <ext>              zip, tar, tar.gz, tar.bz2, tar.xz, tar.zst, tar.lzma,
                            tar.Z, tar.lz or 7z; otherwise guessed from the name
  --threads <n>             worker threads, every core by default
  --password <text>         password to encrypt with, or to decrypt

create:
  --level <level>           store, fast, default or best
  --method <method>         store, deflate or bzip2
  --encryption <scheme>     aes256, aes192, aes128 or zipcrypto
  --sparse                  store runs of zeros as holes (tar only)
  --volume-size <bytes>     split into volumes of at most this size

extract:
  --skip-unsafe             skip entries that would escape dest instead of stopping
  --overwrite <mode>        always, never or error
  --strip-components <n>    drop n leading directories from every name
  --strip-root              drop the leading directory if every entry shares it
  --select <a,b,...>        extract only these entries
  --no-symlinks             skip symbolic links instead of recreating them
  --no-permissions          leave permissions at the system default
  --no-timestamps           leave files with the time they were written
  --owner <mode>            ignore (default), ids or names

list:
  --long                    show permissions, owner and modification time
";

const VALUED: &[&str] =
    &["--type", "--threads", "--password", "--level", "--method", "--encryption", "--volume-size", "--overwrite", "--strip-components", "--select", "--owner"];

const SWITCHES: &[&str] = &["--sparse", "--skip-unsafe", "--strip-root", "--no-symlinks", "--no-permissions", "--no-timestamps", "--long", "--help"];

type Failure = Box<dyn std::error::Error>;

struct Args {
    positional: Vec<String>,
    valued: Vec<(String, String)>,
    switches: Vec<String>,
}

impl Args {
    fn parse(raw: impl IntoIterator<Item = String>) -> Result<Args, Failure> {
        let mut args = Args { positional: Vec::new(), valued: Vec::new(), switches: Vec::new() };
        let mut raw = raw.into_iter();
        let mut only_positional = false;

        while let Some(arg) = raw.next() {
            if only_positional || !arg.starts_with("--") {
                args.positional.push(arg);
            } else if arg == "--" {
                only_positional = true;
            } else if SWITCHES.contains(&arg.as_str()) {
                args.switches.push(arg);
            } else if VALUED.contains(&arg.as_str()) {
                let value = raw.next().ok_or_else(|| format!("{arg} needs a value"))?;
                args.valued.push((arg, value));
            } else {
                return Err(format!("unknown option {arg}").into());
            }
        }

        Ok(args)
    }

    fn value(&self, name: &str) -> Option<&str> {
        self.valued.iter().rev().find(|(flag, _)| flag == name).map(|(_, value)| value.as_str())
    }

    fn has(&self, name: &str) -> bool {
        self.switches.iter().any(|flag| flag == name)
    }

    fn number<T: std::str::FromStr>(&self, name: &str) -> Result<Option<T>, Failure> {
        self.value(name).map(|text| text.parse().map_err(|_| format!("{name} expects a number, not {text:?}").into())).transpose()
    }

    fn choice<T: Copy>(&self, name: &str, choices: &[(&str, T)]) -> Result<Option<T>, Failure> {
        let Some(text) = self.value(name) else { return Ok(None) };
        match choices.iter().find(|(label, _)| label.eq_ignore_ascii_case(text)) {
            Some((_, value)) => Ok(Some(*value)),
            None => {
                let known: Vec<&str> = choices.iter().map(|(label, _)| *label).collect();
                Err(format!("{name} expects one of {}, not {text:?}", known.join(", ")).into())
            }
        }
    }
}

fn main() -> ExitCode {
    let args = match Args::parse(std::env::args().skip(1)) {
        Ok(args) => args,
        Err(e) => {
            eprintln!("ttar: {e} (see ttar --help)");
            return ExitCode::FAILURE;
        }
    };

    if args.has("--help") {
        print!("{USAGE}");
        return ExitCode::SUCCESS;
    }

    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("ttar: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run(args: &Args) -> Result<(), Failure> {
    let [command, path, rest @ ..] = args.positional.as_slice() else { return Err("expected a command and an archive (see ttar --help)".into()) };

    let mut archive = Archive::new(path);

    if let Some(name) = args.value("--type") {
        let wanted = name.trim_start_matches('.');
        let kind = ArchiveType::ALL
            .into_iter()
            .find(|k| k.extension().trim_start_matches('.').eq_ignore_ascii_case(wanted))
            .ok_or_else(|| format!("unknown archive type {name:?}"))?;
        archive = archive.set_type(kind);
    }
    if let Some(threads) = args.number::<usize>("--threads")? {
        archive = archive.set_threads(Some(threads));
    }
    if let Some(password) = args.value("--password") {
        archive = archive.set_password(password);
    }

    match command.as_str() {
        "create" => create(archive, args, rest),
        "extract" => extract(archive, args, rest),
        "list" => list(archive, args),
        other => Err(format!("unknown command {other:?} (see ttar --help)").into()),
    }
}

fn create(mut archive: Archive, args: &Args, inputs: &[String]) -> Result<(), Failure> {
    if inputs.is_empty() {
        return Err("create needs at least one input".into());
    }

    if let Some(level) =
        args.choice("--level", &[("store", Level::None), ("none", Level::None), ("fast", Level::Fast), ("default", Level::Default), ("best", Level::Best)])?
    {
        archive = archive.set_level(level);
    }
    if let Some(method) =
        args.choice("--method", &[("store", Method::Store), ("deflate", Method::Deflate), ("bzip2", Method::Bzip2), ("bz2", Method::Bzip2)])?
    {
        archive = archive.set_method(method);
    }
    if let Some(scheme) = args.choice(
        "--encryption",
        &[
            ("aes256", EncryptionMethod::Aes256),
            ("aes192", EncryptionMethod::Aes192),
            ("aes128", EncryptionMethod::Aes128),
            ("zipcrypto", EncryptionMethod::ZipCrypto),
        ],
    )? {
        archive = archive.set_encryption(scheme);
    }
    if args.has("--sparse") {
        archive = archive.set_sparse(true);
    }
    if let Some(size) = args.number::<u64>("--volume-size")? {
        archive = archive.set_volume_size(size);
    }

    let summary = archive.create_from(inputs)?;
    eprintln!(
        "created {} files, {} dirs, {} symlinks, {} hard links; {} bytes in, {} bytes out, {} volume(s)",
        summary.files, summary.directories, summary.symlinks, summary.hardlinks, summary.bytes, summary.archive_size, summary.volumes
    );
    if summary.specials > 0 {
        eprintln!("left out {} fifos, devices or sockets", summary.specials);
    }
    Ok(())
}

fn extract(mut archive: Archive, args: &Args, rest: &[String]) -> Result<(), Failure> {
    let dest = match rest {
        [] => ".",
        [dest] => dest.as_str(),
        _ => return Err("extract takes one destination".into()),
    };

    if args.has("--skip-unsafe") {
        archive = archive.set_unsafe_entries(UnsafeEntries::Skip);
    }
    if let Some(overwrite) = args.choice("--overwrite", &[("always", Overwrite::Always), ("never", Overwrite::Never), ("error", Overwrite::Error)])? {
        archive = archive.set_overwrite(overwrite);
    }
    if let Some(count) = args.number::<usize>("--strip-components")? {
        archive = archive.set_strip_components(count);
    }
    if args.has("--strip-root") {
        archive = archive.set_strip_root(true);
    }
    if let Some(names) = args.value("--select") {
        archive = archive.set_selection(names.split(',').filter(|name| !name.is_empty()));
    }
    if args.has("--no-symlinks") {
        archive = archive.set_restore_symlinks(false);
    }
    if args.has("--no-permissions") {
        archive = archive.set_preserve_permissions(false);
    }
    if args.has("--no-timestamps") {
        archive = archive.set_preserve_timestamps(false);
    }
    if let Some(ownership) = args.choice("--owner", &[("ignore", Ownership::Ignore), ("ids", Ownership::Ids), ("names", Ownership::Names)])? {
        archive = archive.set_ownership(ownership);
    }

    let summary = archive.extract_to(dest)?;
    eprintln!(
        "extracted {} files, {} dirs, {} symlinks, {} hard links, {} bytes",
        summary.files, summary.directories, summary.symlinks, summary.hardlinks, summary.bytes
    );

    let notes = [
        (summary.skipped, "skipped"),
        (summary.refused, "refused as unsafe"),
        (summary.specials, "fifos, devices or sockets passed over"),
        (summary.deleted, "deleted by the archive"),
        (summary.owners_not_restored, "left without their recorded owner"),
    ];
    for (count, what) in notes.into_iter().filter(|(count, _)| *count > 0) {
        eprintln!("{count} {what}");
    }
    Ok(())
}

fn list(archive: Archive, args: &Args) -> Result<(), Failure> {
    let long = args.has("--long");

    for entry in archive.entries()? {
        let zip = entry.zip();
        let packed = zip.map_or(entry.size, |z| z.compressed_size);
        let locked = if zip.is_some_and(|z| z.is_encrypted()) { "enc" } else { "   " };

        if long {
            println!("{} {:<17} {:>12} {} {} {}", permissions(&entry), owner(&entry), entry.size, timestamp(entry.mtime()), locked, entry.name);
        } else {
            println!("{:>12} {:>12} {} {}", entry.size, packed, locked, entry.name);
        }
    }
    Ok(())
}

fn permissions(entry: &Entry) -> String {
    let kind = if entry.is_dir() {
        'd'
    } else if entry.is_symlink() {
        'l'
    } else {
        '-'
    };
    let Some(mode) = entry.mode() else { return format!("{kind}?????????") };

    let mut text = String::from(kind);
    for (bit, letter) in [(0o400, 'r'), (0o200, 'w'), (0o100, 'x'), (0o040, 'r'), (0o020, 'w'), (0o010, 'x'), (0o004, 'r'), (0o002, 'w'), (0o001, 'x')] {
        text.push(if mode & bit != 0 { letter } else { '-' });
    }

    let special = |text: &mut String, at: usize, set: bool, lower: char, upper: char| {
        if set {
            let executable = text.as_bytes()[at] == b'x';
            text.replace_range(at..at + 1, &String::from(if executable { lower } else { upper }));
        }
    };
    special(&mut text, 3, mode & 0o4000 != 0, 's', 'S');
    special(&mut text, 6, mode & 0o2000 != 0, 's', 'S');
    special(&mut text, 9, mode & 0o1000 != 0, 't', 'T');
    text
}

fn owner(entry: &Entry) -> String {
    let user = entry.user().map(str::to_owned).or_else(|| entry.uid().map(|id| id.to_string()));
    let group = entry.group().map(str::to_owned).or_else(|| entry.gid().map(|id| id.to_string()));
    match (user, group) {
        (None, None) => "-".to_owned(),
        (user, group) => format!("{}/{}", user.as_deref().unwrap_or("-"), group.as_deref().unwrap_or("-")),
    }
}

fn timestamp(seconds: i64) -> String {
    let at = civil_from_unix(seconds);
    format!("{:04}-{:02}-{:02} {:02}:{:02}", at.year, at.month, at.day, at.hour, at.minute)
}
