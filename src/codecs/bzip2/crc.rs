const TABLE: [u32; 256] = build_table();

const fn build_table() -> [u32; 256] {
    let mut table = [0u32; 256];
    let mut i = 0;
    while i < 256 {
        let mut crc = (i as u32) << 24;
        let mut bit = 0;
        while bit < 8 {
            crc = if crc & 0x8000_0000 != 0 { (crc << 1) ^ 0x04C1_1DB7 } else { crc << 1 };
            bit += 1;
        }
        table[i] = crc;
        i += 1;
    }
    table
}

#[derive(Debug, Clone)]
pub struct Crc {
    state: u32,
}

impl Default for Crc {
    fn default() -> Self {
        Self::new()
    }
}

impl Crc {
    pub fn new() -> Self {
        Crc { state: 0xFFFF_FFFF }
    }

    pub fn update(&mut self, data: &[u8]) {
        for &b in data {
            let index = ((self.state >> 24) ^ b as u32) as u8;
            self.state = (self.state << 8) ^ TABLE[index as usize];
        }
    }

    pub fn update_run(&mut self, byte: u8, count: usize) {
        for _ in 0..count {
            let index = ((self.state >> 24) ^ byte as u32) as u8;
            self.state = (self.state << 8) ^ TABLE[index as usize];
        }
    }

    pub fn finish(&self) -> u32 {
        !self.state
    }
}

pub fn combine(running: u32, block: u32) -> u32 {
    running.rotate_left(1) ^ block
}
