use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use crate::utils::error::{Error, Result};

pub fn for_each<S: Send, I: Fn() -> Result<S> + Sync, F: Fn(&mut S, usize) -> Result<()> + Sync>(count: usize, threads: usize, init: I, task: F) -> Result<()> {
    if count == 0 {
        return Ok(());
    }

    if threads <= 1 {
        let mut state = init()?;
        for index in 0..count {
            task(&mut state, index)?;
        }
        return Ok(());
    }

    let next = AtomicUsize::new(0);
    let failed = AtomicBool::new(false);
    let error: Mutex<Option<Error>> = Mutex::new(None);

    let record = |e: Error| {
        let mut slot = error.lock().unwrap_or_else(|p| p.into_inner());
        if slot.is_none() {
            *slot = Some(e);
        }
        failed.store(true, Ordering::Release);
    };

    std::thread::scope(|scope| {
        for _ in 0..threads {
            scope.spawn(|| {
                let mut state = match init() {
                    Ok(s) => s,
                    Err(e) => {
                        record(e);
                        return;
                    }
                };

                loop {
                    if failed.load(Ordering::Acquire) {
                        return;
                    }
                    let index = next.fetch_add(1, Ordering::Relaxed);
                    if index >= count {
                        return;
                    }
                    if let Err(e) = task(&mut state, index) {
                        record(e);
                        return;
                    }
                }
            });
        }
    });

    match error.into_inner().unwrap_or_else(|p| p.into_inner()) {
        Some(e) => Err(e),
        None => Ok(()),
    }
}
