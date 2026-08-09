use crate::utils::crc32;

pub const HEADER_LEN: usize = 12;

#[derive(Clone)]
pub struct ZipCrypto {
    keys: [u32; 3],
}

impl ZipCrypto {
    pub fn new(password: &[u8]) -> Self {
        let mut state = ZipCrypto { keys: [305_419_896, 591_751_049, 878_082_192] };
        for &byte in password {
            state.update_keys(byte);
        }
        state
    }

    #[inline]
    fn update_keys(&mut self, byte: u8) {
        self.keys[0] = crc32_byte(self.keys[0], byte);
        self.keys[1] = self.keys[1].wrapping_add(self.keys[0] & 0xff).wrapping_mul(134_775_813).wrapping_add(1);
        self.keys[2] = crc32_byte(self.keys[2], (self.keys[1] >> 24) as u8);
    }

    #[inline]
    fn keystream_byte(&self) -> u8 {
        let temp = (self.keys[2] | 2) as u16;
        (temp.wrapping_mul(temp ^ 1) >> 8) as u8
    }

    #[inline]
    pub fn decrypt(&mut self, data: &mut [u8]) {
        for byte in data.iter_mut() {
            *byte ^= self.keystream_byte();
            self.update_keys(*byte);
        }
    }

    #[inline]
    pub fn encrypt(&mut self, data: &mut [u8]) {
        for byte in data.iter_mut() {
            let plain = *byte;
            *byte = plain ^ self.keystream_byte();
            self.update_keys(plain);
        }
    }

    pub fn decrypt_header(&mut self, header: &mut [u8; HEADER_LEN]) -> u8 {
        self.decrypt(header);
        header[HEADER_LEN - 1]
    }

    pub fn encrypt_header(&mut self, random: &[u8; HEADER_LEN], check_byte: u8) -> [u8; HEADER_LEN] {
        let mut header = *random;
        header[HEADER_LEN - 1] = check_byte;
        self.encrypt(&mut header);
        header
    }
}

#[inline]
fn crc32_byte(crc: u32, byte: u8) -> u32 {
    let mut c = crc32::Crc32::resume(!crc);
    c.update(&[byte]);
    !c.finish()
}
