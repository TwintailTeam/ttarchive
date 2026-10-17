use std::process::ExitCode;
use std::sync::Arc;

use ttarchive::codecs::{Level, Method};
use ttarchive::utils::datetime::civil_from_unix;
use ttarchive::{
    Archive, ArchiveType, CreateOptions, EncryptionMethod, Entry, ExtractOptions, NamePolicy, Operation, Overwrite, Ownership, ProgressUpdate, UnsafeEntries,
};

const USAGE: &str = "\
usage: ttar create  <archive> [inputs...] [options]
       ttar extract <archive> [dest]      [options]
       ttar list    <archive>             [options]
       ttar formats

  create with no inputs stores the current directory.
  extract with no dest unpacks into a directory named after the archive.

any command:
  --type <ext>              zip, tar, tar.gz, tar.bz2, tar.xz, tar.zst, tar.lzma,
                            tar.Z, tar.lz or 7z; otherwise guessed from the name
  --threads <n>             worker threads, every core by default
  --password <text>         password to encrypt with, or to decrypt
  --progress                report progress on stderr while working
  --quiet                   do not print the closing summary
  --help, --version

create:
  --level <level>            store, fast, default or best
  --method <method>          store, deflate or bzip2 (per entry; zip and 7z)
  --encryption <scheme>      aes256, aes192, aes128 or zipcrypto
  --sparse                   store runs of zeros as holes (tar only)
  --volume-size <bytes>      split into volumes of at most this size
  --comment <text>           archive comment (zip only)
  --no-recursive             store only what is named, not what is inside it
  --follow-symlinks          store what each link points at instead of the link

extract:
  --skip-unsafe              skip entries that would escape dest instead of stopping
  --overwrite <mode>         always, never or error
  --strip-components <n>     drop n leading directories from every name
  --strip-root               drop the leading directory if every entry shares it
  --select <a,b,...>         extract only these entries; may be repeated
  --name-policy <policy>     native (default) or strict, which refuses any name
                             Windows would refuse, on every platform
  --no-symlinks              skip symbolic links instead of recreating them
  --no-permissions           leave permissions at the system default
  --no-timestamps            leave files with the time they were written
  --owner <mode>             ignore (default), ids or names

list:
  --long                     show permissions, owner, time and method
";

const VALUED: &[&str] = &[
    "--type",
    "--threads",
    "--password",
    "--level",
    "--method",
    "--encryption",
    "--volume-size",
    "--comment",
    "--overwrite",
    "--strip-components",
    "--select",
    "--name-policy",
    "--owner",
];

const SWITCHES: &[&str] = &[
    "--sparse",
    "--no-recursive",
    "--follow-symlinks",
    "--skip-unsafe",
    "--strip-root",
    "--no-symlinks",
    "--no-permissions",
    "--no-timestamps",
    "--long",
    "--progress",
    "--quiet",
    "--help",
    "--version",
];

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

    fn values(&self, name: &str) -> Vec<&str> {
        self.valued.iter().filter(|(flag, _)| flag == name).map(|(_, value)| value.as_str()).collect()
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
    if args.has("--version") {
        println!("ttar {} (ttarchive {})", env!("CARGO_PKG_VERSION"), env!("CARGO_PKG_VERSION"));
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
    if args.positional.first().is_some_and(|command| command == "formats") {
        return formats();
    }

    let [command, path, rest @ ..] = args.positional.as_slice() else { return Err("expected a command and an archive (see ttar --help)".into()) };

    let mut archive = Archive::new(path);
    if let Some(kind) = archive_type(args)? {
        archive = archive.set_type(kind);
    }
    if args.has("--progress") {
        archive = archive.with_progress(progress());
    }

    match command.as_str() {
        "create" => create(archive, args, rest),
        "extract" => extract(archive, args, rest),
        "list" => list(archive, args),
        other => Err(format!("unknown command {other:?} (see ttar --help)").into()),
    }
}

fn archive_type(args: &Args) -> Result<Option<ArchiveType>, Failure> {
    let Some(name) = args.value("--type") else { return Ok(None) };
    let wanted = name.trim_start_matches('.');

    for kind in ArchiveType::ALL {
        if kind.extensions().iter().any(|e| e.trim_start_matches('.').eq_ignore_ascii_case(wanted)) {
            return Ok(Some(kind));
        }
    }
    Err(format!("unknown archive type {name:?}; ttar formats lists them").into())
}

fn progress() -> Arc<dyn ttarchive::ProgressCallback> {
    Arc::new(|update: &ProgressUpdate<'_>| {
        let verb = match update.operation {
            Operation::Create => "storing",
            Operation::Extract => "extracting",
        };
        let entry = update.current_entry.unwrap_or("");
        match update.percent() {
            Some(done) => eprint!("\r{verb} {done:>5.1}%  {}/{} entries  {entry:<40.40}", update.processed_entries, update.total_entries),
            None => eprint!("\r{verb} {} entries  {entry:<40.40}", update.processed_entries),
        }
        if update.total_entries > 0 && update.processed_entries == update.total_entries {
            eprintln!();
        }
    })
}

fn create_options(args: &Args) -> Result<CreateOptions, Failure> {
    let mut options = CreateOptions::default();

    if let Some(level) =
        args.choice("--level", &[("store", Level::None), ("none", Level::None), ("fast", Level::Fast), ("default", Level::Default), ("best", Level::Best)])?
    {
        options.level = level;
    }
    options.method = args.choice("--method", &[("store", Method::Store), ("deflate", Method::Deflate), ("bzip2", Method::Bzip2), ("bz2", Method::Bzip2)])?;
    if let Some(scheme) = args.choice(
        "--encryption",
        &[
            ("aes256", EncryptionMethod::Aes256),
            ("aes192", EncryptionMethod::Aes192),
            ("aes128", EncryptionMethod::Aes128),
            ("zipcrypto", EncryptionMethod::ZipCrypto),
        ],
    )? {
        options.encryption = scheme;
    }
    options.sparse = args.has("--sparse");
    options.recursive = !args.has("--no-recursive");
    options.store_symlinks = !args.has("--follow-symlinks");
    options.volume_size = args.number::<u64>("--volume-size")?;
    options.threads = args.number::<usize>("--threads")?;
    options.password = args.value("--password").map(Into::into);
    if let Some(comment) = args.value("--comment") {
        options.comment = comment.as_bytes().to_vec();
    }

    Ok(options)
}

fn extract_options(args: &Args) -> Result<ExtractOptions, Failure> {
    let mut options = ExtractOptions::default();

    if args.has("--skip-unsafe") {
        options.unsafe_entries = UnsafeEntries::Skip;
    }
    if let Some(overwrite) = args.choice("--overwrite", &[("always", Overwrite::Always), ("never", Overwrite::Never), ("error", Overwrite::Error)])? {
        options.overwrite = overwrite;
    }
    if let Some(policy) = args.choice("--name-policy", &[("native", NamePolicy::Native), ("strict", NamePolicy::Strict)])? {
        options.name_policy = policy;
    }
    if let Some(ownership) = args.choice("--owner", &[("ignore", Ownership::Ignore), ("ids", Ownership::Ids), ("names", Ownership::Names)])? {
        options.ownership = ownership;
    }
    if let Some(count) = args.number::<usize>("--strip-components")? {
        options.strip_components = count;
    }
    options.strip_root = args.has("--strip-root");
    options.restore_symlinks = !args.has("--no-symlinks");
    options.preserve_permissions = !args.has("--no-permissions");
    options.preserve_timestamps = !args.has("--no-timestamps");
    options.threads = args.number::<usize>("--threads")?;
    options.password = args.value("--password").map(Into::into);
    options.selection = args.values("--select").iter().flat_map(|list| list.split(',')).filter(|name| !name.is_empty()).map(str::to_owned).collect();

    Ok(options)
}

fn create(archive: Archive, args: &Args, inputs: &[String]) -> Result<(), Failure> {
    let archive = archive.with_create_options(create_options(args)?);
    let summary = if inputs.is_empty() { archive.create()? } else { archive.create_from(inputs)? };

    if args.has("--quiet") {
        return Ok(());
    }
    eprintln!(
        "created {} files, {} dirs, {} symlinks, {} hard links; {} bytes in, {} bytes out, {} volume(s)",
        summary.files, summary.directories, summary.symlinks, summary.hardlinks, summary.bytes, summary.archive_size, summary.volumes
    );
    if summary.specials > 0 {
        eprintln!("left out {} fifos, devices or sockets", summary.specials);
    }
    Ok(())
}

fn extract(archive: Archive, args: &Args, rest: &[String]) -> Result<(), Failure> {
    let archive = archive.with_extract_options(extract_options(args)?);

    let summary = match rest {
        [] => archive.extract()?,
        [dest] => archive.extract_to(dest)?,
        _ => return Err("extract takes one destination".into()),
    };

    if args.has("--quiet") {
        return Ok(());
    }
    eprintln!(
        "extracted {} files, {} dirs, {} symlinks, {} hard links, {} bytes",
        summary.files, summary.directories, summary.symlinks, summary.hardlinks, summary.bytes
    );

    let notes = [
        (summary.skipped, "skipped"),
        (summary.refused, "refused as unsafe"),
        (summary.specials, "fifos, devices or sockets passed over"),
        (summary.deleted, "deleted because the archive said to"),
        (summary.owners_not_restored, "left without their recorded owner"),
    ];
    for (count, what) in notes.into_iter().filter(|(count, _)| *count > 0) {
        eprintln!("{count} {what}");
    }
    Ok(())
}

fn list(archive: Archive, args: &Args) -> Result<(), Failure> {
    let long = args.has("--long");
    let entries = archive.entries()?;

    let mut files = 0u64;
    let mut bytes = 0u64;
    for entry in &entries {
        if entry.is_file() {
            files += 1;
            bytes += entry.size;
        }

        if long {
            println!(
                "{} {:<17} {:>11} {} {:<8} {}{}",
                permissions(entry),
                owner(entry),
                entry.size,
                timestamp(entry.mtime()),
                method(entry),
                entry.name,
                link(entry)
            );
        } else {
            let packed = entry.zip().map_or(entry.size, |z| z.compressed_size);
            let locked = if encrypted(entry) { "enc" } else { "   " };
            println!("{:>12} {:>12} {} {}", entry.size, packed, locked, entry.name);
        }
    }

    if !args.has("--quiet") {
        eprintln!("{} entries, {files} files, {bytes} bytes uncompressed", entries.len());
    }
    Ok(())
}

fn formats() -> Result<(), Failure> {
    println!("{:<10} {:<9} {}", "format", "write", "names");
    for kind in ArchiveType::ALL {
        let writes = if kind.can_write() { "yes" } else { "read only" };
        println!("{:<10} {:<9} {}", kind.extension(), writes, kind.extensions().join(" "));
    }
    Ok(())
}

fn encrypted(entry: &Entry) -> bool {
    entry.zip().is_some_and(|z| z.is_encrypted())
}

fn method(entry: &Entry) -> String {
    if let Some(zip) = entry.zip() {
        let name = zip.method().map(|m| format!("{m:?}")).unwrap_or_else(|_| format!("code {}", zip.effective_method_code()));
        return if zip.is_encrypted() { format!("{name}+enc") } else { name };
    }
    if entry.sevenz().is_some() {
        return "7z".to_owned();
    }
    match entry.tar() {
        Some(tar) if tar.sparse => "sparse".to_owned(),
        _ => "-".to_owned(),
    }
}

fn link(entry: &Entry) -> String {
    match entry.tar() {
        Some(tar) if !tar.linkname.is_empty() => format!(" -> {}", tar.linkname),
        _ => String::new(),
    }
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
