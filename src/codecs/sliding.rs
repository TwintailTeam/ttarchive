pub struct Feed<'a> {
    bytes: &'a [u8],
    base: usize,
}

impl<'a> Feed<'a> {
    pub fn whole(bytes: &'a [u8]) -> Self {
        Feed { bytes, base: 0 }
    }

    pub fn from(bytes: &'a [u8], base: usize) -> Self {
        Feed { bytes, base }
    }

    #[inline]
    pub fn base(&self) -> usize {
        self.base
    }

    #[inline]
    pub fn get(&self, at: usize) -> u8 {
        self.bytes[at - self.base]
    }

    #[inline]
    pub fn end(&self) -> usize {
        self.base + self.bytes.len()
    }

    #[inline]
    pub fn slice(&self, from: usize, to: usize) -> &[u8] {
        &self.bytes[from - self.base..to - self.base]
    }
}

pub struct Sliding {
    bytes: Vec<u8>,
    base: usize,
}

impl Sliding {
    pub fn new(keep: usize) -> Self {
        Sliding { bytes: Vec::with_capacity(slack(keep) + (1 << 16)), base: 0 }
    }

    pub fn push(&mut self, more: &[u8]) {
        self.bytes.extend_from_slice(more);
    }

    pub fn feed(&self) -> Feed<'_> {
        Feed::from(&self.bytes, self.base)
    }

    pub fn end(&self) -> usize {
        self.base + self.bytes.len()
    }

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
