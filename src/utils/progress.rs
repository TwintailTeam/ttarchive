use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// Which direction of work is being reported.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Operation {
    /// Reading an archive and writing files out.
    Extract,
    /// Reading files and writing an archive.
    Create,
}

/// A snapshot handed to the progress callback.
#[derive(Debug, Clone, Copy)]
pub struct ProgressUpdate<'a> {
    /// Whether we are extracting or creating.
    pub operation: Operation,

    /// Uncompressed bytes processed so far across all entries.
    pub processed_bytes: u64,

    /// Total uncompressed bytes expected.
    ///
    /// Zero when the total is not known ahead of time, as for a streamed archive
    /// whose sizes follow the data. Use [`ProgressUpdate::percent`], which
    /// accounts for that.
    pub total_bytes: u64,

    /// Entries fully finished so far.
    pub processed_entries: u64,

    /// Total number of entries, or zero when not yet known.
    pub total_entries: u64,

    /// Name of the entry currently being worked on, if one is in flight.
    ///
    /// Several entries run at once with parallel workers; this reports the most
    /// recently started one, for display only.
    pub current_entry: Option<&'a str>,
}

impl ProgressUpdate<'_> {
    /// Completion in `0.0..=100.0`, or `None` when the total is unknown.
    pub fn percent(&self) -> Option<f64> {
        if self.total_bytes == 0 {
            return None;
        }
        let pct = self.processed_bytes as f64 / self.total_bytes as f64 * 100.0;
        Some(pct.min(100.0))
    }
}

/// Receives progress updates.
///
/// Blanket-implemented for any `Fn(&ProgressUpdate<'_>) + Send + Sync`.
pub trait ProgressCallback: Send + Sync {
    fn on_progress(&self, update: &ProgressUpdate<'_>);
}

impl<F> ProgressCallback for F
where
    F: Fn(&ProgressUpdate<'_>) + Send + Sync,
{
    fn on_progress(&self, update: &ProgressUpdate<'_>) {
        self(update)
    }
}

const BYTE_THRESHOLD: u64 = 256 * 1024;
const TIME_THRESHOLD: Duration = Duration::from_millis(16);

#[derive(Clone)]
pub struct Reporter {
    inner: Option<Arc<Inner>>,
}

struct Inner {
    callback: Arc<dyn ProgressCallback>,
    operation: Operation,
    processed_bytes: AtomicU64,
    total_bytes: AtomicU64,
    processed_entries: AtomicU64,
    total_entries: AtomicU64,
    last_emit_bytes: AtomicU64,
    last_emit_micros: AtomicU64,
    start: Instant,
}

impl Reporter {
    pub fn disabled() -> Self {
        Reporter { inner: None }
    }

    pub fn new(callback: Arc<dyn ProgressCallback>, operation: Operation) -> Self {
        Reporter {
            inner: Some(Arc::new(Inner {
                callback,
                operation,
                processed_bytes: AtomicU64::new(0),
                total_bytes: AtomicU64::new(0),
                processed_entries: AtomicU64::new(0),
                total_entries: AtomicU64::new(0),
                last_emit_bytes: AtomicU64::new(0),
                last_emit_micros: AtomicU64::new(0),
                start: Instant::now(),
            })),
        }
    }

    #[inline]
    pub fn is_disabled(&self) -> bool {
        self.inner.is_none()
    }

    pub fn set_totals(&self, total_bytes: u64, total_entries: u64) {
        let Some(inner) = &self.inner else { return };
        inner.total_bytes.store(total_bytes, Ordering::Relaxed);
        inner.total_entries.store(total_entries, Ordering::Relaxed);
        inner.emit(None);
    }

    #[inline]
    pub fn add_bytes(&self, delta: u64) {
        let Some(inner) = &self.inner else { return };
        let processed = inner.processed_bytes.fetch_add(delta, Ordering::Relaxed) + delta;

        if inner.should_emit(processed) {
            inner.emit(None);
        }
    }

    pub fn start_entry(&self, name: &str) {
        let Some(inner) = &self.inner else { return };
        inner.emit(Some(name));
    }

    pub fn finish_entry(&self) {
        let Some(inner) = &self.inner else { return };
        inner.processed_entries.fetch_add(1, Ordering::Relaxed);
    }

    pub fn finish(&self) {
        let Some(inner) = &self.inner else { return };
        inner.emit(None);
    }
}

impl Inner {
    fn should_emit(&self, processed: u64) -> bool {
        let last = self.last_emit_bytes.load(Ordering::Relaxed);
        if processed.saturating_sub(last) >= BYTE_THRESHOLD
            && self.last_emit_bytes.compare_exchange(last, processed, Ordering::Relaxed, Ordering::Relaxed).is_ok()
        {
            return true;
        }

        let now = self.start.elapsed().as_micros() as u64;
        let last_time = self.last_emit_micros.load(Ordering::Relaxed);
        if now.saturating_sub(last_time) >= TIME_THRESHOLD.as_micros() as u64
            && self.last_emit_micros.compare_exchange(last_time, now, Ordering::Relaxed, Ordering::Relaxed).is_ok()
        {
            return true;
        }

        false
    }

    fn emit(&self, current_entry: Option<&str>) {
        let update = ProgressUpdate {
            operation: self.operation,
            processed_bytes: self.processed_bytes.load(Ordering::Relaxed),
            total_bytes: self.total_bytes.load(Ordering::Relaxed),
            processed_entries: self.processed_entries.load(Ordering::Relaxed),
            total_entries: self.total_entries.load(Ordering::Relaxed),
            current_entry,
        };
        self.callback.on_progress(&update);
    }
}

impl std::fmt::Debug for Reporter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Reporter").field("enabled", &self.inner.is_some()).finish()
    }
}
