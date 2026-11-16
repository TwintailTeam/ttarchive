use ttarchive::crypto::{md5, sha512};

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn pattern(len: usize, seed: u64) -> Vec<u8> {
    let mut out = Vec::with_capacity(len + 8);
    let mut state = seed | 1;
    while out.len() < len {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        out.extend_from_slice(&state.to_le_bytes());
    }
    out.truncate(len);
    out
}

#[test]
fn md5_known_vectors() {
    assert_eq!(hex(&md5::digest(b"")), "d41d8cd98f00b204e9800998ecf8427e");
    assert_eq!(hex(&md5::digest(b"a")), "0cc175b9c0f1b6a831c399e269772661");
    assert_eq!(hex(&md5::digest(b"abc")), "900150983cd24fb0d6963f7d28e17f72");
    assert_eq!(hex(&md5::digest(b"message digest")), "f96b697d7cb7938d525a2f31aaf161d0");
    assert_eq!(hex(&md5::digest(b"abcdefghijklmnopqrstuvwxyz")), "c3fcd3d76192e4007dfb496cca67e13b");
    assert_eq!(hex(&md5::digest(b"12345678901234567890123456789012345678901234567890123456789012345678901234567890")), "57edf4a22be3c955ac49da2e2107b67a");
}

#[test]
fn sha512_known_vectors() {
    assert_eq!(
        hex(&sha512::digest(b"")),
        "cf83e1357eefb8bdf1542850d66d8007d620e4050b5715dc83f4a921d36ce9ce47d0d13c5d85f2b0ff8318d2877eec2f63b931bd47417a81a538327af927da3e"
    );
    assert_eq!(
        hex(&sha512::digest(b"abc")),
        "ddaf35a193617abacc417349ae20413112e6fa4e89a97ea20a9eeee64b55d39a2192992a274fc1a836ba3c23a3feebbd454d4423643ce80e2a9ac94fa54ca49f"
    );
    assert_eq!(
        hex(&sha512::digest(b"abcdefghbcdefghicdefghijdefghijkefghijklfghijklmghijklmnhijklmnoijklmnopjklmnopqklmnopqrlmnopqrsmnopqrstnopqrstu")),
        "8e959b75dae313da8cf4f72814fc143f8f7779c6eb9f7fa17299aeadb6889018501d289e4900f7e4331b99dec4b5433ac7d329eeb6dd26545e96e55b874be909"
    );
}

#[test]
fn streaming_matches_one_shot_at_every_split() {
    let data = pattern(600, 9);

    for split in 0..data.len() {
        let mut a = md5::Md5::new();
        a.update(&data[..split]);
        a.update(&data[split..]);
        assert_eq!(a.finish(), md5::digest(&data), "md5 split at {split}");

        let mut b = sha512::Sha512::new();
        b.update(&data[..split]);
        b.update(&data[split..]);
        assert_eq!(b.finish(), sha512::digest(&data), "sha512 split at {split}");
    }
}

#[test]
fn lengths_around_every_block_boundary_are_padded_correctly() {
    let data = pattern(600, 3);

    for len in 0..=520 {
        let slice = &data[..len];
        let mut md5_stream = md5::Md5::new();
        for chunk in slice.chunks(7) {
            md5_stream.update(chunk);
        }
        assert_eq!(md5_stream.finish(), md5::digest(slice), "md5 in sevens at {len} bytes");

        let mut sha_stream = sha512::Sha512::new();
        for chunk in slice.chunks(7) {
            sha_stream.update(chunk);
        }
        assert_eq!(sha_stream.finish(), sha512::digest(slice), "sha512 in sevens at {len} bytes");
    }
}
