const POLYNOMIAL: u64 = 0xC96C_5795_D787_0F42;

static TABLES: [[u64; 256]; 8] = build_tables();

const fn build_tables() -> [[u64; 256]; 8] {
    let mut t = [[0u64; 256]; 8];

    let mut i = 0;
    while i < 256 {
        let mut crc = i as u64;
        let mut bit = 0;
        while bit < 8 {
            crc = if crc & 1 != 0 { (crc >> 1) ^ POLYNOMIAL } else { crc >> 1 };
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

pub const STREAMS: usize = 8;

const STRIPE_MIN: usize = 64;

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
    pub const fn new() -> Self {
        Crc64 { state: u64::MAX }
    }

    pub fn update(&mut self, data: &[u8]) {
        self.state = update(self.state, data);
    }

    pub const fn finish(&self) -> u64 {
        !self.state
    }
}

pub fn checksum(data: &[u8]) -> u64 {
    let mut crc = Crc64::new();
    crc.update(data);
    crc.finish()
}

fn update(crc: u64, data: &[u8]) -> u64 {
    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    if data.len() >= super::crc64_clmul::MIN_BYTES && super::crc64_clmul::available() {
        return super::crc64_clmul::update(crc, data);
    }

    striped(crc, data)
}

pub fn striped(crc: u64, data: &[u8]) -> u64 {
    let stripe = (data.len() / STREAMS) & !7;
    if stripe < STRIPE_MIN {
        return sliced(crc, data);
    }

    let mut states = [0u64; STREAMS];
    states[0] = crc;

    for offset in (0..stripe).step_by(8) {
        for (stream, state) in states.iter_mut().enumerate() {
            let at = stream * stripe + offset;
            let word = u64::from_le_bytes(data[at..at + 8].try_into().unwrap_or([0; 8]));
            *state = eight(*state, word);
        }
    }

    sliced(join(&states, stripe), &data[stripe * STREAMS..])
}

pub fn join(states: &[u64; STREAMS], stripe: usize) -> u64 {
    let shift = power_of_x(8 * stripe as u64);

    let mut crc = states[0];
    for state in &states[1..] {
        crc = multiply(crc, shift) ^ state;
    }
    crc
}

#[inline(always)]
fn eight(state: u64, word: u64) -> u64 {
    let head = state ^ word;
    TABLES[7][(head & 0xff) as usize]
        ^ TABLES[6][((head >> 8) & 0xff) as usize]
        ^ TABLES[5][((head >> 16) & 0xff) as usize]
        ^ TABLES[4][((head >> 24) & 0xff) as usize]
        ^ TABLES[3][((head >> 32) & 0xff) as usize]
        ^ TABLES[2][((head >> 40) & 0xff) as usize]
        ^ TABLES[1][((head >> 48) & 0xff) as usize]
        ^ TABLES[0][((head >> 56) & 0xff) as usize]
}

pub fn sliced(mut crc: u64, data: &[u8]) -> u64 {
    let mut chunks = data.chunks_exact(8);

    for c in &mut chunks {
        crc = eight(crc, u64::from_le_bytes(c.try_into().unwrap_or([0; 8])));
    }

    for &b in chunks.remainder() {
        crc = (crc >> 8) ^ TABLES[0][((crc ^ b as u64) & 0xff) as usize];
    }

    crc
}

pub const ONE: u64 = 1 << 63;

#[inline]
pub const fn times_x(value: u64) -> u64 {
    (value >> 1) ^ (POLYNOMIAL & 0u64.wrapping_sub(value & 1))
}

pub const fn over_x(value: u64) -> u64 {
    let carried = value >> 63;
    let lifted = value ^ (POLYNOMIAL & 0u64.wrapping_sub(carried));
    (lifted << 1) | carried
}

pub const fn multiply(a: u64, b: u64) -> u64 {
    let mut product = 0u64;
    let mut term = a;
    let mut power = 0;

    while power < 64 {
        if b >> (63 - power) & 1 == 1 {
            product ^= term;
        }
        term = times_x(term);
        power += 1;
    }

    product
}

pub const fn power_of_x(exponent: u64) -> u64 {
    let mut result = ONE;
    let mut base = times_x(ONE);
    let mut left = exponent;

    while left > 0 {
        if left & 1 == 1 {
            result = multiply(result, base);
        }
        base = multiply(base, base);
        left >>= 1;
    }

    result
}

pub const fn inverse_power_of_x(exponent: u64) -> u64 {
    let mut result = ONE;
    let mut left = exponent;

    while left > 0 {
        result = over_x(result);
        left -= 1;
    }

    result
}
