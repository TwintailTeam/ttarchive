const POLY: u32 = 0xEDB8_8320;

static TABLES: [[u32; 256]; 8] = build_tables();

const fn build_tables() -> [[u32; 256]; 8] {
    let mut t = [[0u32; 256]; 8];

    let mut i = 0;
    while i < 256 {
        let mut crc = i as u32;
        let mut bit = 0;
        while bit < 8 {
            crc = if crc & 1 != 0 { (crc >> 1) ^ POLY } else { crc >> 1 };
            bit += 1;
        }
        t[0][i] = crc;
        i += 1;
    }

    let mut n = 1;
    while n < 8 {
        let mut i = 0;
        while i < 256 {
            let prev = t[n - 1][i];
            t[n][i] = (prev >> 8) ^ t[0][(prev & 0xff) as usize];
            i += 1;
        }
        n += 1;
    }

    t
}

#[derive(Debug, Clone, Copy)]
pub struct Crc32 {
    state: u32,
}

impl Crc32 {
    pub const fn new() -> Self {
        Crc32 { state: 0xFFFF_FFFF }
    }

    pub const fn resume(value: u32) -> Self {
        Crc32 { state: !value }
    }

    pub fn update(&mut self, data: &[u8]) {
        let mut crc = self.state;
        let mut chunks = data.chunks_exact(8);

        for c in &mut chunks {
            let head = crc ^ u32::from_le_bytes([c[0], c[1], c[2], c[3]]);
            crc = TABLES[7][(head & 0xff) as usize]
                ^ TABLES[6][((head >> 8) & 0xff) as usize]
                ^ TABLES[5][((head >> 16) & 0xff) as usize]
                ^ TABLES[4][((head >> 24) & 0xff) as usize]
                ^ TABLES[3][c[4] as usize]
                ^ TABLES[2][c[5] as usize]
                ^ TABLES[1][c[6] as usize]
                ^ TABLES[0][c[7] as usize];
        }

        for &b in chunks.remainder() {
            crc = (crc >> 8) ^ TABLES[0][((crc ^ b as u32) & 0xff) as usize];
        }

        self.state = crc;
    }

    pub const fn finish(&self) -> u32 {
        !self.state
    }
}

impl Default for Crc32 {
    fn default() -> Self {
        Self::new()
    }
}

pub fn checksum(data: &[u8]) -> u32 {
    let mut c = Crc32::new();
    c.update(data);
    c.finish()
}

pub fn combine(crc_a: u32, crc_b: u32, len_b: u64) -> u32 {
    if len_b == 0 {
        return crc_a;
    }

    let mut odd = [0u32; 32];
    let mut even = [0u32; 32];

    odd[0] = POLY;
    let mut row = 1u32;
    for slot in odd.iter_mut().skip(1) {
        *slot = row;
        row <<= 1;
    }

    matrix_square(&mut even, &odd);
    matrix_square(&mut odd, &even);

    let mut crc = crc_a;
    let mut remaining = len_b;

    loop {
        matrix_square(&mut even, &odd);
        if remaining & 1 != 0 {
            crc = matrix_apply(&even, crc);
        }
        remaining >>= 1;
        if remaining == 0 {
            break;
        }

        matrix_square(&mut odd, &even);
        if remaining & 1 != 0 {
            crc = matrix_apply(&odd, crc);
        }
        remaining >>= 1;
        if remaining == 0 {
            break;
        }
    }

    crc ^ crc_b
}

#[inline]
fn matrix_apply(matrix: &[u32; 32], mut vector: u32) -> u32 {
    let mut sum = 0u32;
    let mut i = 0;
    while vector != 0 {
        if vector & 1 != 0 {
            sum ^= matrix[i];
        }
        vector >>= 1;
        i += 1;
    }
    sum
}

#[inline]
fn matrix_square(square: &mut [u32; 32], source: &[u32; 32]) {
    for i in 0..32 {
        square[i] = matrix_apply(source, source[i]);
    }
}
