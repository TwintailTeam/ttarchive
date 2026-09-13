use std::time::Instant;

use ttarchive::crypto::aes::{Aes, AesCtr, AesDecrypt, BLOCK_SIZE};
use ttarchive::crypto::hmac::pbkdf2_sha1;
use ttarchive::crypto::sevenz_aes;
use ttarchive::crypto::{Password, sha1, sha256};

const BYTES: usize = 64 * 1024 * 1024;

fn rate(label: &str, bytes: usize, elapsed: f64) {
    let mbps = (bytes as f64 / (1024.0 * 1024.0)) / elapsed;
    println!("{label:<34} {mbps:>8.0} MiB/s   {elapsed:>6.2}s");
}

fn timed(label: &str, bytes: usize, mut body: impl FnMut()) {
    let start = Instant::now();
    body();
    rate(label, bytes, start.elapsed().as_secs_f64());
}

fn main() {
    let data: Vec<u8> = (0..BYTES).map(|i| (i % 251) as u8).collect();
    let key = [0x42u8; 32];
    let key128 = [0x42u8; 16];

    let probe = AesCtr::new(&[0u8; 32]).expect("256 bit key");
    println!("hardware AES: {}", if probe.is_hardware_accelerated() { "yes" } else { "no" });
    println!("{} MiB per measurement", BYTES / (1024 * 1024));
    println!();

    println!("HASHES");
    timed("sha256", BYTES, || {
        let _ = sha256::digest(&data);
    });
    timed("sha1", BYTES, || {
        let _ = sha1::digest(&data);
    });

    println!();
    println!("BLOCK CIPHER");
    let cipher = Aes::new(&key).expect("256 bit key");
    timed("aes-256 encrypt_block", BYTES, || {
        let mut block = [0u8; BLOCK_SIZE];
        for chunk in data.chunks_exact(BLOCK_SIZE) {
            block.copy_from_slice(chunk);
            cipher.encrypt_block(&mut block);
        }
    });

    let inverse = AesDecrypt::new(&key).expect("256 bit key");
    timed("aes-256 decrypt_block", BYTES, || {
        let mut block = [0u8; BLOCK_SIZE];
        for chunk in data.chunks_exact(BLOCK_SIZE) {
            block.copy_from_slice(chunk);
            inverse.decrypt_block(&mut block);
        }
    });

    println!();
    println!("STREAM MODES");
    let mut buffer = data.clone();
    let mut ctr = AesCtr::new(&key).expect("256 bit key");
    println!("  ctr accelerated: {}", ctr.is_hardware_accelerated());
    timed("aes-256 ctr (winzip aes)", BYTES, || ctr.apply(&mut buffer));

    let mut ctr128 = AesCtr::new(&key128).expect("128 bit key");
    let mut buffer128 = data.clone();
    timed("aes-128 ctr", BYTES, || ctr128.apply(&mut buffer128));

    let properties = sevenz_aes::Properties::generate(1);
    let derived = properties.key(&Password::from("benchmark"));
    let encrypted = sevenz_aes::cbc_encrypt(&data, &derived, properties.iv).expect("encrypt");

    timed("aes-256 cbc encrypt", BYTES, || {
        let _ = sevenz_aes::cbc_encrypt(&data, &derived, properties.iv).expect("encrypt");
    });

    {
        use std::io::Read;
        let reader = sevenz_aes::CbcDecryptReader::new(&encrypted[..], &derived, properties.iv).expect("reader");
        println!("  cbc accelerated: {}", reader.is_hardware_accelerated());
        drop(reader);

        timed("aes-256 cbc decrypt (7zAES)", BYTES, || {
            let mut out = Vec::with_capacity(encrypted.len());
            sevenz_aes::CbcDecryptReader::new(&encrypted[..], &derived, properties.iv).expect("reader").read_to_end(&mut out).expect("decrypt");
        });
    }

    println!();
    println!("KEY DERIVATION");
    for cycles in [16u8, 19, 22] {
        let properties = sevenz_aes::Properties::generate(cycles);
        let password = Password::from("benchmark");
        let start = Instant::now();
        let _ = properties.key(&password);
        let elapsed = start.elapsed().as_secs_f64();
        println!("{:<34} {:>8.3}s   ({} rounds)", format!("7zAES, 2^{cycles} rounds"), elapsed, 1u64 << cycles);
    }

    let start = Instant::now();
    let mut out = [0u8; 32];
    pbkdf2_sha1(b"benchmark", b"salt", 1_000, &mut out);
    println!("{:<34} {:>8.3}s   (1000 rounds)", "winzip aes, pbkdf2-sha1", start.elapsed().as_secs_f64());
}
