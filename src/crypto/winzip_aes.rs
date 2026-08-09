use crate::crypto::aes::AesCtr;
use crate::crypto::hmac::{HmacSha1, constant_time_eq, pbkdf2_sha1};
use crate::crypto::random;
use crate::utils::error::{Error, Result};

pub const ITERATIONS: u32 = 1000;
pub const VERIFIER_LEN: usize = 2;
pub const AUTH_CODE_LEN: usize = 10;
pub const VENDOR_ID: [u8; 2] = *b"AE";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Strength {
    Aes128,
    Aes192,
    Aes256,
}

impl Strength {
    pub fn from_code(code: u8) -> Result<Self> {
        match code {
            1 => Ok(Strength::Aes128),
            2 => Ok(Strength::Aes192),
            3 => Ok(Strength::Aes256),
            other => Err(Error::malformed(format!("unknown AES strength code {other}"))),
        }
    }

    pub fn code(self) -> u8 {
        match self {
            Strength::Aes128 => 1,
            Strength::Aes192 => 2,
            Strength::Aes256 => 3,
        }
    }

    pub fn key_len(self) -> usize {
        match self {
            Strength::Aes128 => 16,
            Strength::Aes192 => 24,
            Strength::Aes256 => 32,
        }
    }

    pub fn salt_len(self) -> usize {
        self.key_len() / 2
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AesExtra {
    pub version: u16,
    pub strength: Strength,
    pub actual_method: u16,
}

impl AesExtra {
    pub fn parse(data: &[u8]) -> Result<Self> {
        if data.len() < 7 {
            return Err(Error::malformed(format!("AES extra field is {} bytes, expected 7", data.len())));
        }

        let version = u16::from_le_bytes([data[0], data[1]]);
        if data[2..4] != VENDOR_ID {
            return Err(Error::malformed("AES extra field has an unknown vendor id"));
        }

        Ok(AesExtra { version, strength: Strength::from_code(data[4])?, actual_method: u16::from_le_bytes([data[5], data[6]]) })
    }

    pub fn encode(&self) -> [u8; 7] {
        let mut out = [0u8; 7];
        out[..2].copy_from_slice(&self.version.to_le_bytes());
        out[2..4].copy_from_slice(&VENDOR_ID);
        out[4] = self.strength.code();
        out[5..7].copy_from_slice(&self.actual_method.to_le_bytes());
        out
    }

    pub fn crc_is_meaningful(&self) -> bool {
        self.version == 1
    }
}

pub fn overhead(strength: Strength) -> u64 {
    (strength.salt_len() + VERIFIER_LEN + AUTH_CODE_LEN) as u64
}

struct DerivedKeys {
    encryption: Vec<u8>,
    authentication: Vec<u8>,
    verifier: [u8; VERIFIER_LEN],
}

fn derive(password: &[u8], salt: &[u8], strength: Strength) -> DerivedKeys {
    let key_len = strength.key_len();
    let mut material = vec![0u8; key_len * 2 + VERIFIER_LEN];
    pbkdf2_sha1(password, salt, ITERATIONS, &mut material);

    let mut verifier = [0u8; VERIFIER_LEN];
    verifier.copy_from_slice(&material[key_len * 2..]);

    DerivedKeys { encryption: material[..key_len].to_vec(), authentication: material[key_len..key_len * 2].to_vec(), verifier }
}

pub struct AesDecryptor {
    ctr: AesCtr,
    mac: HmacSha1,
}

impl AesDecryptor {
    pub fn new(password: &[u8], salt: &[u8], verifier: [u8; VERIFIER_LEN], strength: Strength) -> Result<Self> {
        let keys = derive(password, salt, strength);

        if !constant_time_eq(&keys.verifier, &verifier) {
            return Err(Error::WrongPassword);
        }

        Ok(AesDecryptor {
            ctr: AesCtr::new(&keys.encryption).ok_or_else(|| Error::malformed("invalid AES key length"))?,
            mac: HmacSha1::new(&keys.authentication),
        })
    }

    pub fn decrypt(&mut self, data: &mut [u8]) {
        self.mac.update(data);
        self.ctr.apply(data);
    }

    pub fn verify(self, expected: &[u8]) -> Result<()> {
        let tag = self.mac.finish();
        if !constant_time_eq(&tag[..AUTH_CODE_LEN], expected) {
            return Err(Error::AuthenticationFailed);
        }
        Ok(())
    }
}

pub struct AesEncryptor {
    ctr: AesCtr,
    mac: HmacSha1,
    prefix: Vec<u8>,
}

impl AesEncryptor {
    pub fn new(password: &[u8], strength: Strength) -> Result<Self> {
        let mut salt = vec![0u8; strength.salt_len()];
        random::fill(&mut salt);
        Self::with_salt(password, strength, &salt)
    }

    pub fn with_salt(password: &[u8], strength: Strength, salt: &[u8]) -> Result<Self> {
        if salt.len() != strength.salt_len() {
            return Err(Error::malformed(format!("salt is {} bytes, expected {} for {strength:?}", salt.len(), strength.salt_len())));
        }

        let keys = derive(password, salt, strength);

        let mut prefix = salt.to_vec();
        prefix.extend_from_slice(&keys.verifier);

        Ok(AesEncryptor {
            ctr: AesCtr::new(&keys.encryption).ok_or_else(|| Error::malformed("invalid AES key length"))?,
            mac: HmacSha1::new(&keys.authentication),
            prefix,
        })
    }

    pub fn prefix(&self) -> &[u8] {
        &self.prefix
    }

    pub fn encrypt(&mut self, data: &mut [u8]) {
        self.ctr.apply(data);
        self.mac.update(data);
    }

    pub fn finish(self) -> [u8; AUTH_CODE_LEN] {
        let tag = self.mac.finish();
        let mut out = [0u8; AUTH_CODE_LEN];
        out.copy_from_slice(&tag[..AUTH_CODE_LEN]);
        out
    }
}
