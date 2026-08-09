#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

pub struct TempDir {
    path: PathBuf,
}

impl TempDir {
    pub fn new(tag: &str) -> Self {
        Self::new_in(std::env::temp_dir(), tag)
    }

    pub fn new_in(base: impl AsRef<Path>, tag: &str) -> Self {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let unique = format!("ttarchive-{tag}-{}-{}", std::process::id(), COUNTER.fetch_add(1, Ordering::Relaxed));
        let path = base.as_ref().join(unique);
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("create temp dir");
        TempDir { path }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn join(&self, rel: impl AsRef<Path>) -> PathBuf {
        self.path.join(rel)
    }

    pub fn write(&self, rel: impl AsRef<Path>, contents: impl AsRef<[u8]>) -> PathBuf {
        let target = self.join(rel);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).expect("create parent");
        }
        std::fs::write(&target, contents).expect("write file");
        target
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

pub fn pseudo_random(len: usize, seed: u32) -> Vec<u8> {
    let mut x = seed | 1;
    (0..len)
        .map(|_| {
            x = x.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (x >> 16) as u8
        })
        .collect()
}

pub fn compressible(len: usize) -> Vec<u8> {
    let phrase = b"the quick brown fox jumps over the lazy dog ";
    phrase.iter().copied().cycle().take(len).collect()
}
