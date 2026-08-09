/// The input addressed by absolute position, so an encoder whose caller has
/// dropped an early part of the stream can still work on what was kept.
///
/// `base` is the position `bytes[0]` occupies in the whole input. Every match
/// an encoder can still emit reaches back at most one window, so a caller only
/// has to keep that much.
pub struct Feed<'a> {
    bytes: &'a [u8],
    base: usize,
}

impl<'a> Feed<'a> {
    /// View `bytes` as the whole input, starting at position zero.
    pub fn whole(bytes: &'a [u8]) -> Self {
        Feed { bytes, base: 0 }
    }

    /// View `bytes` as the part of the input starting at `base`.
    pub fn from(bytes: &'a [u8], base: usize) -> Self {
        Feed { bytes, base }
    }

    /// The position of the first byte held.
    #[inline]
    pub fn base(&self) -> usize {
        self.base
    }

    /// The byte at absolute position `at`.
    #[inline]
    pub fn get(&self, at: usize) -> u8 {
        self.bytes[at - self.base]
    }

    /// The position one past the last byte held.
    #[inline]
    pub fn end(&self) -> usize {
        self.base + self.bytes.len()
    }

    /// The bytes between two absolute positions.
    #[inline]
    pub fn slice(&self, from: usize, to: usize) -> &[u8] {
        &self.bytes[from - self.base..to - self.base]
    }
}

/// The window a streaming encoder keeps so it can look back into it.
///
/// Bytes are appended as they arrive and dropped once no match can reach them.
/// Trimming happens in batches, because moving the survivors down costs as much
/// as the bytes it keeps.
pub struct Sliding {
    bytes: Vec<u8>,
    base: usize,
}

impl Sliding {
    /// An empty window that will retain at least `keep` bytes of history.
    pub fn new(keep: usize) -> Self {
        Sliding { bytes: Vec::with_capacity(slack(keep) + (1 << 16)), base: 0 }
    }

    /// Append newly arrived input.
    pub fn push(&mut self, more: &[u8]) {
        self.bytes.extend_from_slice(more);
    }

    /// Address what is held, by absolute position.
    pub fn feed(&self) -> Feed<'_> {
        Feed::from(&self.bytes, self.base)
    }

    /// The position one past the last byte held.
    pub fn end(&self) -> usize {
        self.base + self.bytes.len()
    }

    /// Drop history that no match reaching back from `at` can name.
    ///
    /// Waits until the excess is worth the move: dropping the oldest bytes
    /// shifts every survivor down, so trimming on every call would copy the
    /// whole window per chunk encoded.
    pub fn retain(&mut self, at: usize, keep: usize) {
        let oldest = at.saturating_sub(keep);
        if oldest <= self.base || self.bytes.len() < keep + slack(keep) {
            return;
        }
        self.bytes.drain(..oldest - self.base);
        self.base = oldest;
    }
}

fn slack(keep: usize) -> usize {
    (keep / 4).max(1 << 16)
}
