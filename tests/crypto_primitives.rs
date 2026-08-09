use ttarchive::crypto::aes::{Aes, AesCtr};
use ttarchive::crypto::hmac::{constant_time_eq, hmac_sha1, pbkdf2_sha1};
use ttarchive::crypto::sha1;
use ttarchive::crypto::zipcrypto::ZipCrypto;

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex(s: &str) -> Vec<u8> {
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
}

#[test]
fn sha1_known_vectors() {
    assert_eq!(hex(&sha1::digest(b"")), "da39a3ee5e6b4b0d3255bfef95601890afd80709");
    assert_eq!(hex(&sha1::digest(b"abc")), "a9993e364706816aba3e25717850c26c9cd0d89d");
    assert_eq!(hex(&sha1::digest(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq")), "84983e441c3bd26ebaae4aa1f95129e5e54670f1");
    let million = vec![b'a'; 1_000_000];
    assert_eq!(hex(&sha1::digest(&million)), "34aa973cd4c4daa4f61eeb2bdbad27316534016f");
}

#[test]
fn sha1_streaming_matches_one_shot() {
    let data: Vec<u8> = (0..1000).map(|i| (i % 251) as u8).collect();
    let expected = sha1::digest(&data);

    for chunk in [1usize, 7, 63, 64, 65, 128] {
        let mut h = sha1::Sha1::new();
        for piece in data.chunks(chunk) {
            h.update(piece);
        }
        assert_eq!(h.finish(), expected, "chunk size {chunk}");
    }
}

#[test]
fn hmac_sha1_known_vectors() {
    assert_eq!(hex(&hmac_sha1(&[0x0b; 20], b"Hi There")), "b617318655057264e28bc0b6fb378c8ef146be00");
    assert_eq!(hex(&hmac_sha1(b"Jefe", b"what do ya want for nothing?")), "effcdf6ae5eb2fa2d27416d5f184df9c259a7c79");
    assert_eq!(hex(&hmac_sha1(&[0xaa; 20], &[0xdd; 50])), "125d7342b9ac11cd91a39af48aa17b4f63f175d3");
    assert_eq!(hex(&hmac_sha1(&[0xaa; 80], b"Test Using Larger Than Block-Size Key - Hash Key First")), "aa4ae5e15272d00e95705637ce8a3b55ed402112");
}

#[test]
fn pbkdf2_known_vectors() {
    let mut out = [0u8; 20];

    pbkdf2_sha1(b"password", b"salt", 1, &mut out);
    assert_eq!(hex(&out), "0c60c80f961f0e71f3a9b524af6012062fe037a6");

    pbkdf2_sha1(b"password", b"salt", 2, &mut out);
    assert_eq!(hex(&out), "ea6c014dc72d6f8ccd1ed92ace1d41f0d8de8957");

    pbkdf2_sha1(b"password", b"salt", 4096, &mut out);
    assert_eq!(hex(&out), "4b007901b765489abead49d926f721d065a429c1");

    let mut long = [0u8; 25];
    pbkdf2_sha1(b"passwordPASSWORDpassword", b"saltSALTsaltSALTsaltSALTsaltSALTsalt", 4096, &mut long);
    assert_eq!(hex(&long), "3d2eec4fe41c849b80c8d83662c0e44a8b291a964cf2f07038");

    let mut short = [0u8; 16];
    pbkdf2_sha1(b"pass\0word", b"sa\0lt", 4096, &mut short);
    assert_eq!(hex(&short), "56fa6aa75548099dcc37d7f03425e0c3");
}

#[test]
fn aes_known_vectors() {
    let plaintext = unhex("00112233445566778899aabbccddeeff");

    for (key_hex, expected) in [
        ("000102030405060708090a0b0c0d0e0f", "69c4e0d86a7b0430d8cdb78070b4c55a"),
        ("000102030405060708090a0b0c0d0e0f1011121314151617", "dda97ca4864cdfe06eaf70a0ec0d7191"),
        ("000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f", "8ea2b7ca516745bfeafc49904b496089"),
    ] {
        let cipher = Aes::new(&unhex(key_hex)).expect("valid key length");
        let mut block = [0u8; 16];
        block.copy_from_slice(&plaintext);
        cipher.encrypt_block(&mut block);
        assert_eq!(hex(&block), expected, "key {key_hex}");
    }
}

#[test]
fn aes_rejects_bad_key_lengths() {
    assert!(Aes::new(&[0u8; 15]).is_none());
    assert!(Aes::new(&[0u8; 17]).is_none());
    assert!(Aes::new(&[]).is_none());
}

#[test]
fn aes_ctr_is_self_inverse() {
    let key = [0x42u8; 32];
    let original: Vec<u8> = (0..1000).map(|i| (i % 256) as u8).collect();

    let mut data = original.clone();
    AesCtr::new(&key).unwrap().apply(&mut data);
    assert_ne!(data, original, "ciphertext must differ from plaintext");

    AesCtr::new(&key).unwrap().apply(&mut data);
    assert_eq!(data, original);
}

#[test]
fn aes_ctr_is_chunk_independent() {
    let key = [0x7fu8; 16];
    let original: Vec<u8> = (0..500).map(|i| (i * 7 % 256) as u8).collect();

    let mut whole = original.clone();
    AesCtr::new(&key).unwrap().apply(&mut whole);

    for chunk in [1usize, 3, 15, 16, 17, 64] {
        let mut piecewise = original.clone();
        let mut ctr = AesCtr::new(&key).unwrap();
        for piece in piecewise.chunks_mut(chunk) {
            ctr.apply(piece);
        }
        assert_eq!(piecewise, whole, "chunk size {chunk}");
    }
}

#[test]
fn zipcrypto_round_trips() {
    let password = b"correct horse battery staple";
    let original = b"the compressed data stream goes here".to_vec();

    let mut data = original.clone();
    ZipCrypto::new(password).encrypt(&mut data);
    assert_ne!(data, original);

    ZipCrypto::new(password).decrypt(&mut data);
    assert_eq!(data, original);
}

#[test]
fn zipcrypto_wrong_password_produces_different_output() {
    let original = b"secret contents".to_vec();
    let mut data = original.clone();
    ZipCrypto::new(b"right").encrypt(&mut data);

    let mut wrong = data.clone();
    ZipCrypto::new(b"wrong").decrypt(&mut wrong);
    assert_ne!(wrong, original);
}

#[test]
fn zipcrypto_state_carries_across_calls() {
    let password = b"pw";
    let original: Vec<u8> = (0..300).map(|i| (i % 256) as u8).collect();

    let mut whole = original.clone();
    ZipCrypto::new(password).encrypt(&mut whole);

    let mut piecewise = original.clone();
    let mut cipher = ZipCrypto::new(password);
    for piece in piecewise.chunks_mut(17) {
        cipher.encrypt(piece);
    }
    assert_eq!(piecewise, whole);
}

#[test]
fn constant_time_eq_behaves_like_eq() {
    assert!(constant_time_eq(b"abc", b"abc"));
    assert!(!constant_time_eq(b"abc", b"abd"));
    assert!(!constant_time_eq(b"abc", b"ab"));
    assert!(constant_time_eq(b"", b""));
}

#[test]
fn hardware_and_software_ctr_agree() {
    use ttarchive::crypto::aes::Aes;

    let mut ctr = AesCtr::new(&[0x42u8; 32]).unwrap();
    println!("hardware accelerated: {}", ctr.is_hardware_accelerated());

    for key_len in [16usize, 24, 32] {
        let key: Vec<u8> = (0..key_len).map(|i| (i as u8).wrapping_mul(37)).collect();

        let cipher = Aes::new(&key).unwrap();
        let mut expected = Vec::new();
        for counter in 1u128..=40 {
            let mut block = counter.to_le_bytes();
            cipher.encrypt_block(&mut block);
            expected.extend_from_slice(&block);
        }

        let mut got = vec![0u8; expected.len()];
        ctr = AesCtr::new(&key).unwrap();
        ctr.apply(&mut got);

        assert_eq!(got, expected, "key length {key_len}");
    }
}

#[test]
fn ctr_bulk_and_tail_paths_agree() {
    let key = [0x9au8; 32];
    let original: Vec<u8> = (0..5000).map(|i| (i % 256) as u8).collect();

    let mut whole = original.clone();
    AesCtr::new(&key).unwrap().apply(&mut whole);

    for chunk in [1usize, 15, 16, 17, 127, 128, 129, 200, 1000] {
        let mut piecewise = original.clone();
        let mut ctr = AesCtr::new(&key).unwrap();
        for piece in piecewise.chunks_mut(chunk) {
            ctr.apply(piece);
        }
        assert_eq!(piecewise, whole, "chunk size {chunk}");
    }
}
