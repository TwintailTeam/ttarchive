pub mod aes;
pub mod aes_ni;
pub mod hmac;
pub mod random;
pub mod sevenz_aes;
pub mod sha1;
pub mod sha256;
pub mod stream;
pub mod winzip_aes;
pub mod zipcrypto;

use std::fmt;

use winzip_aes::Strength;

/// Which encryption scheme to use when writing with a password.
///
/// The AES ones mean WinZip AES in a ZIP and 7zAES in a 7z.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum Encryption {
    /// AES with a 256-bit key. The default, and the only one 7z has.
    #[default]
    Aes256,
    /// AES with a 192-bit key. ZIP only.
    Aes192,
    /// AES with a 128-bit key. ZIP only.
    Aes128,
    /// ZIP's original encryption. Broken, and only worth using for readers
    /// that accept nothing else.
    ZipCrypto,
}

impl Encryption {
    /// The AES key length, or `None` for ZipCrypto.
    pub fn strength(self) -> Option<Strength> {
        match self {
            Encryption::Aes256 => Some(Strength::Aes256),
            Encryption::Aes192 => Some(Strength::Aes192),
            Encryption::Aes128 => Some(Strength::Aes128),
            Encryption::ZipCrypto => None,
        }
    }

    /// How many extra bytes this scheme adds to each entry.
    pub fn overhead(self) -> u64 {
        match self.strength() {
            Some(s) => winzip_aes::overhead(s),
            None => zipcrypto::HEADER_LEN as u64,
        }
    }

    /// The ZIP version a reader needs to handle this scheme.
    pub fn version_needed(self) -> u16 {
        match self {
            Encryption::ZipCrypto => 20,
            _ => 51,
        }
    }
}

/// A password, kept out of logs and panic messages.
///
/// Its `Debug` prints a placeholder rather than the password itself.
#[derive(Clone, PartialEq, Eq)]
pub struct Password(Vec<u8>);

impl Password {
    /// Take a password from any bytes.
    pub fn new(bytes: impl Into<Vec<u8>>) -> Self {
        Password(bytes.into())
    }

    /// The password's bytes.
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    /// Whether the password is empty, which counts as having none.
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
