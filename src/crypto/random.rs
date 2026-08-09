use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, Hasher};
use std::sync::atomic::{AtomicU64, Ordering};

static SEQUENCE: AtomicU64 = AtomicU64::new(0);

pub fn fill(out: &mut [u8]) {
    #[cfg(unix)]
    {
        if fill_from_urandom(out) {
            return;
        }
    }
    fill_from_fallback(out);
}

#[cfg(unix)]
fn fill_from_urandom(out: &mut [u8]) -> bool {
    use std::fs::File;
    use std::io::Read;

    let Ok(mut file) = File::open("/dev/urandom") else {
        return false;
    };
    file.read_exact(out).is_ok()
}

fn fill_from_fallback(out: &mut [u8]) {
    for chunk in out.chunks_mut(8) {
        let sequence = SEQUENCE.fetch_add(1, Ordering::Relaxed);

        let mut hasher = RandomState::new().build_hasher();
        hasher.write_u64(sequence);

        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos() as u64).unwrap_or(sequence);
        hasher.write_u64(now);

        let local = 0u8;
        hasher.write_usize(&local as *const u8 as usize);

        let value = hasher.finish();
        let bytes = value.to_le_bytes();
        let take = chunk.len();
        chunk.copy_from_slice(&bytes[..take]);
    }
}
