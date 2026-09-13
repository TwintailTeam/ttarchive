use std::io::Read;

use crate::crypto::Password;
use crate::crypto::aes::{BLOCK_SIZE, CbcDecrypt};
use crate::crypto::sha256::Sha256;
use crate::utils::error::{Error, Result};

pub const MAX_FIELD: usize = 16;

const CYCLES_LITERAL: u8 = 0x3F;

const CHUNK: usize = 64 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Properties {
    pub cycles: u8,
    pub salt: Vec<u8>,
    pub iv: [u8; BLOCK_SIZE],
}

impl Properties {
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        let short = || Error::malformed("7zAES coder properties end before the salt and IV they declare");

        let (&flags, rest) = bytes.split_first().ok_or_else(short)?;
        let cycles = flags & 0x3F;

        if flags & 0xC0 == 0 {
            return Ok(Properties { cycles, salt: Vec::new(), iv: [0; BLOCK_SIZE] });
        }

        let (&sizes, rest) = rest.split_first().ok_or_else(short)?;
        let salt_len = ((flags >> 7) & 1) as usize + (sizes >> 4) as usize;
        let iv_len = ((flags >> 6) & 1) as usize + (sizes & 0x0F) as usize;

        if salt_len > MAX_FIELD || iv_len > MAX_FIELD {
            return Err(Error::malformed(format!("7zAES coder declares a {salt_len} byte salt and a {iv_len} byte IV; neither may pass {MAX_FIELD}")));
        }
        if rest.len() < salt_len + iv_len {
            return Err(short());
        }

        let mut iv = [0u8; BLOCK_SIZE];
        iv[..iv_len].copy_from_slice(&rest[salt_len..salt_len + iv_len]);

        Ok(Properties { cycles, salt: rest[..salt_len].to_vec(), iv })
    }

    pub fn key(&self, password: &Password) -> [u8; 32] {
        derive_key(password, &self.salt, self.cycles)
    }
}

pub fn derive_key(password: &Password, salt: &[u8], cycles: u8) -> [u8; 32] {
    let password = utf16le(password);
    let mut key = [0u8; 32];

    if cycles == CYCLES_LITERAL {
        let mut at = 0usize;
        for &byte in salt.iter().chain(password.iter()) {
            if at == key.len() {
                break;
            }
            key[at] = byte;
            at += 1;
        }
        return key;
    }

    let mut sha = Sha256::new();
    let mut counter = 0u64;
    for _ in 0..1u64 << cycles {
        sha.update(salt);
        sha.update(&password);
        sha.update(&counter.to_le_bytes());
        counter += 1;
    }

    key.copy_from_slice(&sha.finish());
    key
}

fn utf16le(password: &Password) -> Vec<u8> {
    String::from_utf8_lossy(password.as_bytes()).encode_utf16().flat_map(u16::to_le_bytes).collect()
}

impl Properties {
    pub fn generate(cycles: u8) -> Self {
        let mut salt = vec![0u8; 8];
        let mut iv = [0u8; BLOCK_SIZE];
        crate::crypto::random::fill(&mut salt);
        crate::crypto::random::fill(&mut iv[..8]);
        Properties { cycles: cycles & 0x3F, salt, iv }
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        let salt_len = self.salt.len();
        let iv_len = BLOCK_SIZE;

        let salt_high = (salt_len > 0x0F) as u8;
        let iv_high = (iv_len > 0x0F) as u8;

        let mut out = Vec::with_capacity(2 + salt_len + iv_len);
        out.push(self.cycles | (salt_high << 7) | (iv_high << 6));
        out.push(((salt_len as u8 - salt_high) << 4) | (iv_len as u8 - iv_high));
        out.extend_from_slice(&self.salt);
        out.extend_from_slice(&self.iv);
        out
    }
}

pub fn cbc_encrypt(plain: &[u8], key: &[u8], iv: [u8; BLOCK_SIZE]) -> Result<Vec<u8>> {
    let cipher = crate::crypto::aes::Aes::new(key).ok_or_else(|| Error::malformed(format!("7zAES was handed a {} byte key", key.len())))?;

    let blocks = plain.len().div_ceil(BLOCK_SIZE).max(1);
    let mut out = Vec::with_capacity(blocks * BLOCK_SIZE);
    let mut chain = iv;

    for index in 0..blocks {
        let mut block = [0u8; BLOCK_SIZE];
        let at = index * BLOCK_SIZE;
        let take = plain.len().saturating_sub(at).min(BLOCK_SIZE);
        block[..take].copy_from_slice(&plain[at..at + take]);

        for (byte, previous) in block.iter_mut().zip(chain.iter()) {
            *byte ^= previous;
        }
        cipher.encrypt_block(&mut block);
        chain = block;
        out.extend_from_slice(&block);
    }

    Ok(out)
}

pub struct CbcDecryptReader<R> {
    inner: R,
    cipher: CbcDecrypt,
    buffer: Vec<u8>,
    at: usize,
    filled: usize,
    done: bool,
}

impl<R: Read> CbcDecryptReader<R> {
    pub fn new(inner: R, key: &[u8], iv: [u8; BLOCK_SIZE]) -> Result<Self> {
        let cipher = CbcDecrypt::new(key, iv).ok_or_else(|| Error::malformed(format!("7zAES was handed a {} byte key", key.len())))?;
        Ok(CbcDecryptReader { inner, cipher, buffer: vec![0u8; CHUNK], at: 0, filled: 0, done: false })
    }

    pub fn is_hardware_accelerated(&self) -> bool {
        self.cipher.is_hardware_accelerated()
    }

    fn next_chunk(&mut self) -> std::io::Result<()> {
        let mut got = 0usize;
        while got < self.buffer.len() {
            match self.inner.read(&mut self.buffer[got..])? {
                0 => break,
                n => got += n,
            }
        }

        if got == 0 {
            self.done = true;
            self.filled = 0;
            return Ok(());
        }
        if got % BLOCK_SIZE != 0 {
            return Err(Error::malformed(format!("7zAES stream ends {} bytes into a {BLOCK_SIZE} byte block", got % BLOCK_SIZE)).into());
        }

        self.cipher.apply(&mut self.buffer[..got]);
        self.at = 0;
        self.filled = got;
        Ok(())
    }
}

impl<R: Read> Read for CbcDecryptReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }

        if self.at == self.filled {
            if self.done {
                return Ok(0);
            }
            self.next_chunk()?;
            if self.filled == 0 {
                return Ok(0);
            }
        }

        let take = (self.filled - self.at).min(buf.len());
        buf[..take].copy_from_slice(&self.buffer[self.at..self.at + take]);
        self.at += take;
        Ok(take)
    }
}
