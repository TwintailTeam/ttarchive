use std::process::ExitCode;

use ttarchive::codecs::{Level, Method};
use ttarchive::{Archive, ArchiveType, EncryptionMethod, UnsafeEntries};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        eprintln!("usage: ttar <create|extract|list> <archive> [args...]");
        return ExitCode::FAILURE;
    }

    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("ttar: {e}");
            ExitCode::FAILURE
        }
    }
}

fn option<'a>(args: &'a [String], name: &str) -> Option<&'a str> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).map(|s| s.as_str())
}

fn positional(args: &[String]) -> Vec<&str> {
    let mut out = Vec::new();
    let mut skip = false;
    for arg in args {
        if skip {
            skip = false;
            continue;
        }
        if arg.starts_with("--") {
            skip = true;
            continue;
        }
        out.push(arg.as_str());
    }
    out
}

fn run(args: &[String]) -> ttarchive::Result<()> {
    let positional = positional(args);
    let command = positional[0];

    let mut archive = Archive::new(positional[1]);
    if let Some(name) = option(args, "--type") {
        let kind = ArchiveType::ALL.into_iter().find(|k| k.extension().trim_start_matches('.') == name.trim_start_matches('.'));
        archive = archive.set_type(kind.unwrap_or(ArchiveType::Zip));
    }

    if let Some(threads) = option(args, "--threads") {
        archive = archive.set_threads(threads.parse().ok());
    }
    if let Some(password) = option(args, "--password") {
        archive = archive.set_password(password);
    }
    if args.iter().any(|a| a == "--skip-unsafe") {
        archive = archive.set_unsafe_entries(UnsafeEntries::Skip);
    }

    match command {
        "create" => {
            if let Some(level) = option(args, "--level") {
                archive = archive.set_level(match level {
                    "store" | "none" => Level::None,
                    "fast" => Level::Fast,
                    "best" => Level::Best,
                    _ => Level::Default,
                });
            }
            if let Some(method) = option(args, "--method") {
                archive = archive.set_method(match method {
                    "store" => Method::Store,
                    "bzip2" | "bz2" => Method::Bzip2,
                    _ => Method::Deflate,
                });
            }
            if let Some(scheme) = option(args, "--encryption") {
                archive = archive.set_encryption(match scheme {
                    "zipcrypto" => EncryptionMethod::ZipCrypto,
                    "aes128" => EncryptionMethod::Aes128,
                    "aes192" => EncryptionMethod::Aes192,
                    _ => EncryptionMethod::Aes256,
                });
            }
            if args.iter().any(|a| a == "--sparse") {
                archive = archive.set_sparse(true);
            }
            if let Some(size) = option(args, "--volume-size")
                && let Ok(size) = size.parse()
            {
                archive = archive.set_volume_size(size);
            }

            let summary = archive.create_from(&positional[2..])?;
            eprintln!("created {} files, {} bytes in, {} bytes out, {} volume(s)", summary.files, summary.bytes, summary.archive_size, summary.volumes);
        }

        "extract" => {
            let dest = positional.get(2).copied().unwrap_or(".");
            if let Some(count) = option(args, "--strip-components")
                && let Ok(count) = count.parse()
            {
                archive = archive.set_strip_components(count);
            }
            if args.iter().any(|a| a == "--strip-root") {
                archive = archive.set_strip_root(true);
            }
            let summary = archive.extract_to(dest)?;
            eprintln!(
                "extracted {} files, {} dirs, {} symlinks, {} bytes ({} refused)",
                summary.files, summary.directories, summary.symlinks, summary.bytes, summary.refused
            );
        }

        "list" => {
            for entry in archive.entries()? {
                let zip = entry.zip();
                let packed = zip.map_or(entry.size, |z| z.compressed_size);
                let locked = zip.is_some_and(|z| z.is_encrypted());
                println!("{:>12} {:>12} {} {}", entry.size, packed, if locked { "enc" } else { "   " }, entry.name);
            }
        }

        other => {
            eprintln!("unknown command {other:?}");
        }
    }

    Ok(())
}
