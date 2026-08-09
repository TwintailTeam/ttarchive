use std::io::{self, Read, Write};

use crate::crypto::winzip_aes::{AUTH_CODE_LEN, AesDecryptor, AesEncryptor, Strength, VERIFIER_LEN};
use crate::crypto::zipcrypto::{HEADER_LEN, ZipCrypto};
use crate::crypto::{Encryption, Password, random};
use crate::utils::error::{Error, Result};

enum ReadMode {
    ZipCrypto(ZipCrypto),
    Aes(Box<AesDecryptor>),
}

pub struct DecryptReader<R> {
    inner: R,
    mode: ReadMode,
    remaining: u64,
    verified: bool,
}

impl<R: Read> DecryptReader<R> {
    pub fn zipcrypto(mut inner: R, password: &Password, stored_size: u64, check_byte: u8) -> Result<Self> {
        if stored_size < HEADER_LEN as u64 {
            return Err(Error::malformed("encrypted entry is too short to contain its 12-byte encryption header"));
        }

        let mut cipher = ZipCrypto::new(password.as_bytes());
        let mut header = [0u8; HEADER_LEN];
        inner.read_exact(&mut header)?;

        let found = cipher.decrypt_header(&mut header);
        if found != check_byte {
            return Err(Error::WrongPassword);
        }

        Ok(DecryptReader { inner, mode: ReadMode::ZipCrypto(cipher), remaining: stored_size - HEADER_LEN as u64, verified: false })
    }

    pub fn winzip_aes(mut inner: R, password: &Password, stored_size: u64, strength: Strength) -> Result<Self> {
        let overhead = crate::crypto::winzip_aes::overhead(strength);
        if stored_size < overhead {
            return Err(Error::malformed(format!(
                "AES entry is {stored_size} bytes, too short for its {overhead} bytes of \
                 salt, verifier and authentication code"
            )));
        }

        let mut salt = vec![0u8; strength.salt_len()];
        inner.read_exact(&mut salt)?;

        let mut verifier = [0u8; VERIFIER_LEN];
        inner.read_exact(&mut verifier)?;

        let decryptor = AesDecryptor::new(password.as_bytes(), &salt, verifier, strength)?;

        Ok(DecryptReader { inner, mode: ReadMode::Aes(Box::new(decryptor)), remaining: stored_size - overhead, verified: false })
    }

    fn finish(&mut self) -> io::Result<()> {
        if self.verified {
            return Ok(());
        }
        self.verified = true;

        let mode = std::mem::replace(&mut self.mode, ReadMode::ZipCrypto(ZipCrypto::new(b"")));

        if let ReadMode::Aes(decryptor) = mode {
            let mut tag = [0u8; AUTH_CODE_LEN];
            self.inner.read_exact(&mut tag).map_err(|e| {
                if e.kind() == io::ErrorKind::UnexpectedEof { io::Error::from(Error::malformed("AES entry is missing its authentication code")) } else { e }
            })?;
            decryptor.verify(&tag)?;
        }

        Ok(())
    }
}

impl<R: Read> Read for DecryptReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.remaining == 0 {
            self.finish()?;
            return Ok(0);
        }

        let cap = buf.len().min(self.remaining as usize);
        let n = self.inner.read(&mut buf[..cap])?;
        if n == 0 {
            return Err(io::Error::from(Error::malformed("encrypted entry ended before its declared length")));
        }

        match &mut self.mode {
            ReadMode::ZipCrypto(cipher) => cipher.decrypt(&mut buf[..n]),
            ReadMode::Aes(decryptor) => decryptor.decrypt(&mut buf[..n]),
        }

        self.remaining -= n as u64;
        if self.remaining == 0 {
            self.finish()?;
        }

        Ok(n)
    }
}

enum WriteMode {
    ZipCrypto(ZipCrypto),
    Aes(Option<Box<AesEncryptor>>),
}

pub struct EncryptWriter<W> {
    inner: W,
    mode: WriteMode,
    written: u64,
}

impl<W: Write> EncryptWriter<W> {
    pub fn new(mut inner: W, password: &Password, encryption: Encryption, check_byte: u8) -> Result<Self> {
        match encryption.strength() {
            Some(strength) => {
                let encryptor = AesEncryptor::new(password.as_bytes(), strength)?;
                inner.write_all(encryptor.prefix())?;
                let written = encryptor.prefix().len() as u64;

                Ok(EncryptWriter { inner, mode: WriteMode::Aes(Some(Box::new(encryptor))), written })
            }
            None => {
                let mut cipher = ZipCrypto::new(password.as_bytes());

                let mut seed = [0u8; HEADER_LEN];
                random::fill(&mut seed);
                let header = cipher.encrypt_header(&seed, check_byte);
                inner.write_all(&header)?;

                Ok(EncryptWriter { inner, mode: WriteMode::ZipCrypto(cipher), written: HEADER_LEN as u64 })
            }
        }
    }

    pub fn written(&self) -> u64 {
        self.written
    }

    pub fn finish(mut self) -> Result<u64> {
        if let WriteMode::Aes(slot) = &mut self.mode
            && let Some(encryptor) = slot.take()
        {
            let tag = encryptor.finish();
            self.inner.write_all(&tag)?;
            self.written += tag.len() as u64;
        }
        self.inner.flush()?;
        Ok(self.written)
    }
}

impl<W: Write> Write for EncryptWriter<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let mut scratch = buf.to_vec();

        match &mut self.mode {
            WriteMode::ZipCrypto(cipher) => cipher.encrypt(&mut scratch),
            WriteMode::Aes(Some(encryptor)) => encryptor.encrypt(&mut scratch),
            WriteMode::Aes(None) => {
                return Err(io::Error::other("write after finish on an encrypted stream"));
            }
        }

        self.inner.write_all(&scratch)?;
        self.written += scratch.len() as u64;
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

pub fn encrypt_buffer(data: &[u8], password: &Password, encryption: Encryption, check_byte: u8) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(data.len() + encryption.overhead() as usize);
    let mut writer = EncryptWriter::new(&mut out, password, encryption, check_byte)?;
    writer.write_all(data)?;
    writer.finish()?;
    Ok(out)
}
