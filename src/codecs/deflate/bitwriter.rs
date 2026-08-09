#[derive(Debug, Default)]
pub struct BitWriter {
    out: Vec<u8>,
    acc: u64,
    count: u32,
}

impl BitWriter {
    pub fn new() -> Self {
        BitWriter::default()
    }

    pub fn with_capacity(capacity: usize) -> Self {
        BitWriter { out: Vec::with_capacity(capacity), acc: 0, count: 0 }
    }

    pub fn len(&self) -> usize {
        self.out.len()
    }

    pub fn is_empty(&self) -> bool {
        self.out.is_empty() && self.count == 0
    }

    #[inline]
    pub fn write_bits(&mut self, value: u32, n: u32) {
        debug_assert!(n <= 32);
        let masked = (value as u64) & ((1u64 << n) - 1);
        self.acc |= masked << self.count;
        self.count += n;
        self.flush_whole_bytes();
    }

    #[inline]
    fn flush_whole_bytes(&mut self) {
        while self.count >= 8 {
            self.out.push((self.acc & 0xff) as u8);
            self.acc >>= 8;
            self.count -= 8;
        }
    }

    pub fn align_to_byte(&mut self) {
        if self.count > 0 {
            self.out.push((self.acc & 0xff) as u8);
            self.acc = 0;
            self.count = 0;
        }
    }

    pub fn write_bytes(&mut self, bytes: &[u8]) {
        debug_assert_eq!(self.count, 0, "write_bytes requires byte alignment");
        self.out.extend_from_slice(bytes);
    }

    pub fn pending_bits(&self) -> u32 {
        self.count
    }

    pub fn as_slice(&self) -> &[u8] {
        &self.out
    }

    pub fn finish(mut self) -> Vec<u8> {
        self.align_to_byte();
        self.out
    }

    pub fn take(&mut self) -> Vec<u8> {
        self.align_to_byte();
        std::mem::take(&mut self.out)
    }

    pub fn drain_complete_bytes(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.out)
    }
}
