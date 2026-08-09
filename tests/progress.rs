mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use ttarchive::{Archive, ArchiveType, Operation, ProgressUpdate};

#[derive(Default)]
struct Seen {
    calls: AtomicU64,
    max_bytes: AtomicU64,
    total_bytes: AtomicU64,
    max_entries: AtomicU64,
    total_entries: AtomicU64,
    went_backwards: AtomicBool,
    over_total: AtomicBool,
    wrong_operation: AtomicBool,
}

impl Seen {
    fn watch(self: &Arc<Self>, want: Operation) -> impl Fn(&ProgressUpdate<'_>) + Send + Sync + 'static {
        let seen = Arc::clone(self);
        move |update: &ProgressUpdate<'_>| {
            seen.calls.fetch_add(1, Ordering::Relaxed);

            if update.operation != want {
                seen.wrong_operation.store(true, Ordering::Relaxed);
            }

            let previous = seen.max_bytes.fetch_max(update.processed_bytes, Ordering::Relaxed);
            if update.processed_bytes < previous {
                seen.went_backwards.store(true, Ordering::Relaxed);
            }

            if update.total_bytes > 0 && update.processed_bytes > update.total_bytes {
                seen.over_total.store(true, Ordering::Relaxed);
            }
            if update.total_entries > 0 && update.processed_entries > update.total_entries {
                seen.over_total.store(true, Ordering::Relaxed);
            }

            seen.max_entries.fetch_max(update.processed_entries, Ordering::Relaxed);
            seen.total_bytes.fetch_max(update.total_bytes, Ordering::Relaxed);
            seen.total_entries.fetch_max(update.total_entries, Ordering::Relaxed);
        }
    }

    fn check(&self, label: &str) {
        assert!(self.calls.load(Ordering::Relaxed) > 0, "{label}: the callback never fired");
        assert!(!self.wrong_operation.load(Ordering::Relaxed), "{label}: reported the wrong operation");
        assert!(!self.went_backwards.load(Ordering::Relaxed), "{label}: processed_bytes went backwards");
        assert!(!self.over_total.load(Ordering::Relaxed), "{label}: reported more progress than the declared total");
    }

    fn assert_entries_complete(&self, label: &str) {
        self.check(label);

        let (done, total) = (self.max_entries.load(Ordering::Relaxed), self.total_entries.load(Ordering::Relaxed));
        assert!(total > 0, "{label}: never declared an entry total");
        assert_eq!(done, total, "{label}: finished {done} of {total} entries, so a bar tracking entries never completes");
    }

    fn assert_complete(&self, label: &str) {
        self.assert_entries_complete(label);

        let (bytes, want) = (self.max_bytes.load(Ordering::Relaxed), self.total_bytes.load(Ordering::Relaxed));
        assert_eq!(bytes, want, "{label}: finished {bytes} of {want} bytes");
    }
}

fn sample(dir: &common::TempDir) -> std::path::PathBuf {
    dir.write("src/a.txt", common::compressible(40_000));
    dir.write("src/nested/b.bin", common::pseudo_random(30_000, 7));
    dir.write("src/nested/deep/c.txt", common::compressible(15_000));
    dir.write("src/empty.txt", b"");
    dir.join("src")
}

const WRITABLE: [(&str, ArchiveType); 7] = [
    ("p.zip", ArchiveType::Zip),
    ("p.tar", ArchiveType::Tar),
    ("p.tar.gz", ArchiveType::TarGz),
    ("p.tar.bz2", ArchiveType::TarBz2),
    ("p.tar.lzma", ArchiveType::TarLzma),
    ("p.tar.xz", ArchiveType::TarXz),
    ("p.tar.zst", ArchiveType::TarZst),
];

#[test]
fn create_reports_progress_to_completion_for_every_writable_format() {
    for (name, kind) in WRITABLE {
        let dir = common::TempDir::new("progress-create");
        let source = sample(&dir);

        let seen = Arc::new(Seen::default());
        Archive::new(dir.join(name))
            .set_type(kind)
            .on_progress(seen.watch(Operation::Create))
            .create_from([&source])
            .unwrap_or_else(|e| panic!("{name}: create failed: {e}"));

        seen.assert_complete(&format!("{name} create"));
    }
}

#[test]
fn extract_reports_progress_to_completion_for_every_writable_format() {
    for (name, kind) in WRITABLE {
        let dir = common::TempDir::new("progress-extract");
        let source = sample(&dir);

        let archive = dir.join(name);
        Archive::new(&archive).set_type(kind).create_from([&source]).unwrap();

        let seen = Arc::new(Seen::default());
        Archive::new(&archive)
            .set_type(kind)
            .on_progress(seen.watch(Operation::Extract))
            .extract_to(dir.join("out"))
            .unwrap_or_else(|e| panic!("{name}: extract failed: {e}"));

        seen.assert_complete(&format!("{name} extract"));
    }
}

#[test]
fn an_archive_holding_only_directories_still_completes() {
    let dir = common::TempDir::new("progress-dirs");
    std::fs::create_dir_all(dir.join("src/one/two/three")).unwrap();

    for (name, kind) in WRITABLE {
        let archive = dir.join(name);
        Archive::new(&archive).set_type(kind).create_from([dir.join("src")]).unwrap_or_else(|e| panic!("{name}: {e}"));

        let seen = Arc::new(Seen::default());
        Archive::new(&archive)
            .set_type(kind)
            .on_progress(seen.watch(Operation::Extract))
            .extract_to(dir.join(format!("{name}-out")))
            .unwrap_or_else(|e| panic!("{name}: {e}"));

        seen.assert_complete(&format!("{name} directories only"));
    }
}

#[test]
fn stripped_entries_do_not_leave_the_entry_count_short() {
    let dir = common::TempDir::new("progress-strip");
    let source = sample(&dir);

    for (name, kind) in WRITABLE {
        let archive = dir.join(name);
        Archive::new(&archive).set_type(kind).create_from([&source]).unwrap();

        let seen = Arc::new(Seen::default());
        Archive::new(&archive)
            .set_type(kind)
            .set_strip_root(true)
            .on_progress(seen.watch(Operation::Extract))
            .extract_to(dir.join(format!("{name}-strip")))
            .unwrap_or_else(|e| panic!("{name}: {e}"));

        seen.assert_complete(&format!("{name} with strip_root"));
    }
}

#[test]
fn skipped_entries_still_count_towards_completion() {
    let dir = common::TempDir::new("progress-skip");
    let source = sample(&dir);

    for (name, kind) in WRITABLE {
        let archive = dir.join(name);
        Archive::new(&archive).set_type(kind).create_from([&source]).unwrap();

        let out = dir.join(format!("{name}-twice"));
        Archive::new(&archive).set_type(kind).extract_to(&out).unwrap();

        let seen = Arc::new(Seen::default());
        let summary = Archive::new(&archive)
            .set_type(kind)
            .set_overwrite(ttarchive::Overwrite::Never)
            .on_progress(seen.watch(Operation::Extract))
            .extract_to(&out)
            .unwrap_or_else(|e| panic!("{name}: {e}"));

        assert!(summary.skipped > 0, "{name}: the second pass should have skipped something");

        seen.assert_entries_complete(&format!("{name} second pass"));
    }
}

#[test]
fn the_totals_are_declared_before_any_work_is_reported() {
    let dir = common::TempDir::new("progress-order");
    let source = sample(&dir);

    for (name, kind) in WRITABLE {
        let archive = dir.join(name);
        Archive::new(&archive).set_type(kind).create_from([&source]).unwrap();

        let bad = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&bad);

        Archive::new(&archive)
            .set_type(kind)
            .on_progress(move |update: &ProgressUpdate<'_>| {
                if update.processed_bytes > 0 && update.total_bytes == 0 {
                    flag.store(true, Ordering::Relaxed);
                }
            })
            .extract_to(dir.join(format!("{name}-order")))
            .unwrap();

        assert!(!bad.load(Ordering::Relaxed), "{name}: reported bytes before declaring a total");
    }
}

#[test]
fn percent_never_exceeds_one_hundred() {
    let dir = common::TempDir::new("progress-percent");
    let source = sample(&dir);

    for (name, kind) in WRITABLE {
        let archive = dir.join(name);
        let worst = Arc::new(AtomicU64::new(0));
        let track = Arc::clone(&worst);

        Archive::new(&archive)
            .set_type(kind)
            .on_progress(move |update: &ProgressUpdate<'_>| {
                if let Some(pct) = update.percent() {
                    track.fetch_max((pct * 100.0) as u64, Ordering::Relaxed);
                }
            })
            .create_from([&source])
            .unwrap();

        assert!(worst.load(Ordering::Relaxed) <= 10_000, "{name}: percent went above 100");
    }
}

#[test]
fn a_shared_callback_sees_both_directions() {
    let dir = common::TempDir::new("progress-shared");
    let source = sample(&dir);

    let creates = Arc::new(AtomicU64::new(0));
    let extracts = Arc::new(AtomicU64::new(0));

    let c = Arc::clone(&creates);
    let e = Arc::clone(&extracts);
    let callback: Arc<dyn ttarchive::ProgressCallback> = Arc::new(move |update: &ProgressUpdate<'_>| match update.operation {
        Operation::Create => {
            c.fetch_add(1, Ordering::Relaxed);
        }
        Operation::Extract => {
            e.fetch_add(1, Ordering::Relaxed);
        }
    });

    let archive = dir.join("shared.tar.gz");
    Archive::new(&archive).with_progress(Arc::clone(&callback)).create_from([&source]).unwrap();
    Archive::new(&archive).with_progress(Arc::clone(&callback)).extract_to(dir.join("out")).unwrap();

    assert!(creates.load(Ordering::Relaxed) > 0, "no create updates");
    assert!(extracts.load(Ordering::Relaxed) > 0, "no extract updates");
}

#[test]
fn the_named_entry_is_one_the_archive_actually_holds() {
    let dir = common::TempDir::new("progress-names");
    let source = sample(&dir);

    for (name, kind) in WRITABLE {
        let archive = dir.join(name);
        Archive::new(&archive).set_type(kind).create_from([&source]).unwrap();

        let bad = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let sink = Arc::clone(&bad);

        Archive::new(&archive)
            .set_type(kind)
            .on_progress(move |update: &ProgressUpdate<'_>| {
                if let Some(entry) = update.current_entry
                    && !entry.contains("a.txt")
                    && !entry.contains("b.bin")
                    && !entry.contains("c.txt")
                    && !entry.contains("empty.txt")
                    && !entry.contains("src")
                {
                    sink.lock().unwrap().push(entry.to_owned());
                }
            })
            .extract_to(dir.join(format!("{name}-named")))
            .unwrap();

        let unexpected = bad.lock().unwrap();
        assert!(unexpected.is_empty(), "{name}: reported entries not in the archive: {unexpected:?}");
    }
}

#[test]
fn progress_is_optional_and_costs_nothing_when_unused() {
    let dir = common::TempDir::new("progress-none");
    let source = sample(&dir);

    for (name, kind) in WRITABLE {
        let archive = dir.join(name);
        Archive::new(&archive).set_type(kind).create_from([&source]).unwrap_or_else(|e| panic!("{name}: {e}"));
        Archive::new(&archive).set_type(kind).extract_to(dir.join(format!("{name}-quiet"))).unwrap_or_else(|e| panic!("{name}: {e}"));
    }
}

#[test]
fn a_large_entry_reports_progress_as_it_is_written_not_all_at_once() {
    const BODY: usize = 8 * 1024 * 1024;

    for (name, kind) in WRITABLE {
        let dir = common::TempDir::new("progress-granular");
        dir.write("src/big.bin", common::compressible(BODY));

        let archive = dir.join(name);
        Archive::new(&archive).set_type(kind).create_from([dir.join("src")]).unwrap_or_else(|e| panic!("{name}: {e}"));

        let steps = Arc::new(AtomicU64::new(0));
        let counted = Arc::clone(&steps);
        let last = Arc::new(AtomicU64::new(0));
        let previous = Arc::clone(&last);

        Archive::new(&archive)
            .set_type(kind)
            .on_progress(move |update: &ProgressUpdate<'_>| {
                let before = previous.swap(update.processed_bytes, Ordering::Relaxed);
                if update.processed_bytes > before {
                    counted.fetch_add(1, Ordering::Relaxed);
                }
            })
            .extract_to(dir.join("out"))
            .unwrap_or_else(|e| panic!("{name}: {e}"));

        let seen = steps.load(Ordering::Relaxed);
        assert!(seen > 4, "{name}: an 8 MiB entry advanced the byte count only {seen} times, so it was buffered whole");
    }
}

#[test]
fn zip_and_tar_report_progress_the_same_way() {
    const BODY: usize = 4 * 1024 * 1024;

    let mut shapes = Vec::new();
    for (name, kind) in [("same.zip", ArchiveType::Zip), ("same.tar", ArchiveType::Tar), ("same.tar.gz", ArchiveType::TarGz)] {
        let dir = common::TempDir::new("progress-parity");
        dir.write("src/one.bin", common::compressible(BODY));
        dir.write("src/two.txt", b"small");

        let archive = dir.join(name);
        Archive::new(&archive).set_type(kind).create_from([dir.join("src")]).unwrap();

        let seen = Arc::new(Seen::default());
        Archive::new(&archive).set_type(kind).on_progress(seen.watch(Operation::Extract)).extract_to(dir.join("out")).unwrap();

        seen.assert_complete(&format!("{name} extract"));
        shapes.push((name, seen.calls.load(Ordering::Relaxed) > 4));
    }

    for (name, granular) in shapes {
        assert!(granular, "{name}: reported too coarsely to match the others");
    }
}

#[test]
fn a_large_entry_reports_creation_progress_as_it_is_read() {
    const BODY: usize = 8 * 1024 * 1024;

    for (name, kind) in WRITABLE {
        let dir = common::TempDir::new("progress-create-granular");
        dir.write("src/big.bin", common::compressible(BODY));

        let steps = Arc::new(AtomicU64::new(0));
        let counted = Arc::clone(&steps);
        let last = Arc::new(AtomicU64::new(0));
        let previous = Arc::clone(&last);

        Archive::new(dir.join(name))
            .set_type(kind)
            .on_progress(move |update: &ProgressUpdate<'_>| {
                let before = previous.swap(update.processed_bytes, Ordering::Relaxed);
                if update.processed_bytes > before {
                    counted.fetch_add(1, Ordering::Relaxed);
                }
            })
            .create_from([dir.join("src")])
            .unwrap_or_else(|e| panic!("{name}: {e}"));

        let seen = steps.load(Ordering::Relaxed);
        assert!(seen > 4, "{name}: an 8 MiB entry advanced the byte count only {seen} times while being read");
    }
}

#[test]
fn zip_and_tar_report_creation_progress_the_same_way() {
    const BODY: usize = 4 * 1024 * 1024;

    for (name, kind) in [("c.zip", ArchiveType::Zip), ("c.tar", ArchiveType::Tar), ("c.tar.gz", ArchiveType::TarGz)] {
        let dir = common::TempDir::new("progress-create-parity");
        dir.write("src/one.bin", common::compressible(BODY));
        dir.write("src/two.txt", b"small");

        let seen = Arc::new(Seen::default());
        Archive::new(dir.join(name)).set_type(kind).on_progress(seen.watch(Operation::Create)).create_from([dir.join("src")]).unwrap();

        seen.assert_complete(&format!("{name} create"));
        assert!(seen.calls.load(Ordering::Relaxed) > 4, "{name}: reported too coarsely to match the others");
    }
}
