pub mod aes;
pub mod aes_ni;
pub mod hmac;
pub mod random;
pub mod sha1;
pub mod sha256;
pub mod stream;
pub mod winzip_aes;
pub mod zipcrypto;

use std::fmt;

use winzip_aes::Strength;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum Encryption {
    #[default]
    Aes256,
    Aes192,
    Aes128,
    ZipCrypto,
}

impl Encryption {
    pub fn strength(self) -> Option<Strength> {
        match self {
            Encryption::Aes256 => Some(Strength::Aes256),
            Encryption::Aes192 => Some(Strength::Aes192),
            Encryption::Aes128 => Some(Strength::Aes128),
            Encryption::ZipCrypto => None,
        }
    }

    pub fn overhead(self) -> u64 {
        match self.strength() {
            Some(s) => winzip_aes::overhead(s),
            None => zipcrypto::HEADER_LEN as u64,
        }
    }

    pub fn version_needed(self) -> u16 {
        match self {
            Encryption::ZipCrypto => 20,
            _ => 51,
        }
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct Password(Vec<u8>);

impl Password {
    pub fn new(bytes: impl Into<Vec<u8>>) -> Self {
        Password(bytes.into())
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl From<&str> for Password {
    fn from(s: &str) -> Self {
        Password(s.as_bytes().to_vec())
    }
}

impl From<String> for Password {
    fn from(s: String) -> Self {
        Password(s.into_bytes())
    }
}

impl Drop for Password {
    fn drop(&mut self) {
        for byte in self.0.iter_mut() {
            unsafe { std::ptr::write_volatile(byte, 0) };
        }
        std::sync::atomic::fence(std::sync::atomic::Ordering::SeqCst);
    }
}

impl fmt::Debug for Password {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Password({} bytes, redacted)", self.0.len())
    }
}
