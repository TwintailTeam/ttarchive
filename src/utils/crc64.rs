const POLYNOMIAL: u64 = 0xC96C_5795_D787_0F42;

const TABLE: [u64; 256] = build_table();

const fn build_table() -> [u64; 256] {
    let mut table = [0u64; 256];
    let mut i = 0;
    while i < 256 {
        let mut crc = i as u64;
        let mut bit = 0;
        while bit < 8 {
            crc = if crc & 1 != 0 { (crc >> 1) ^ POLYNOMIAL } else { crc >> 1 };
            bit += 1;
        }
        table[i] = crc;
        i += 1;
    }
    table
}

#[derive(Debug, Clone)]
pub struct Crc64 {
    state: u64,
}

impl Default for Crc64 {
    fn default() -> Self {
        Self::new()
    }
}

impl Crc64 {
    pub fn new() -> Self {
        Crc64 { state: u64::MAX }
    }

    pub fn update(&mut self, data: &[u8]) {
        for &byte in data {
            let index = ((self.state ^ byte as u64) & 0xff) as usize;
            self.state = (self.state >> 8) ^ TABLE[index];
        }
    }

    pub fn finish(&self) -> u64 {
        !self.state
    }
}

pub fn checksum(data: &[u8]) -> u64 {
    let mut crc = Crc64::new();
    crc.update(data);
    crc.finish()
}
