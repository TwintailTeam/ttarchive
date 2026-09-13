use ttarchive::sevenz::chain;
use ttarchive::sevenz::folder::Folder;
use ttarchive::sevenz::is_sevenz;
use ttarchive::sevenz::number::{bits, count, defined_bits, number, put_bits, put_defined_bits, put_number};
use ttarchive::sevenz::spec::{Branch, Codec, CoderId};
use ttarchive::utils::bytes::Cursor;

const VECTORS: [(&[u8], u64); 8] = [
    (&[0x00], 0),
    (&[0x7F], 0x7F),
    (&[0x80, 0x80], 0x80),
    (&[0xBF, 0xFF], 0x3FFF),
    (&[0xC0, 0x00, 0x40], 0x4000),
    (&[0xDF, 0xFF, 0xFF], 0x1F_FFFF),
    (&[0xF1, 0x89, 0x67, 0x45, 0x23], 0x01_2345_6789),
    (&[0xFF, 0x08, 0x07, 0x06, 0x05, 0x04, 0x03, 0x02, 0x01], 0x0102_0304_0506_0708),
];

#[test]
fn a_number_decodes_to_the_value_the_reference_encoding_gives_it() {
    for (encoded, expected) in VECTORS {
        let mut cursor = Cursor::new(encoded, 0);
        assert_eq!(number(&mut cursor, "test vector").unwrap(), expected, "decoding {encoded:02x?}");
        assert!(cursor.is_empty(), "decoding {encoded:02x?} left {} bytes", cursor.remaining());
    }
}

#[test]
fn a_number_encodes_to_the_exact_bytes_the_reference_encoding_gives_it() {
    for (expected, value) in VECTORS {
        let mut out = Vec::new();
        put_number(&mut out, value);
        assert_eq!(out, expected, "encoding {value:#x}");
    }
}

#[test]
fn the_high_bits_of_a_numbers_first_byte_are_the_most_significant_part() {
    let mut cursor = Cursor::new(&[0xBF, 0xFF], 0);
    assert_eq!(number(&mut cursor, "half big-endian").unwrap(), 0x3FFF);

    let mut cursor = Cursor::new(&[0xC1, 0x02, 0x03], 0);
    assert_eq!(number(&mut cursor, "half big-endian").unwrap(), 0x0001_0302);
}

#[test]
fn consecutive_numbers_each_consume_only_their_own_bytes() {
    let mut encoded = Vec::new();
    for (_, value) in VECTORS {
        put_number(&mut encoded, value);
    }

    let mut cursor = Cursor::new(&encoded, 0);
    for (_, expected) in VECTORS {
        assert_eq!(number(&mut cursor, "sequence").unwrap(), expected);
    }
    assert!(cursor.is_empty());
}

#[test]
fn a_number_past_the_count_ceiling_is_refused_rather_than_allocated() {
    let mut encoded = Vec::new();
    put_number(&mut encoded, u64::MAX);

    let mut cursor = Cursor::new(&encoded, 0);
    assert_eq!(number(&mut Cursor::new(&encoded, 0), "ceiling").unwrap(), u64::MAX);
    assert!(count(&mut cursor, "files").is_err());
}

#[test]
fn a_truncated_number_reports_a_malformed_archive_rather_than_a_value() {
    let mut cursor = Cursor::new(&[0xFF, 0x01, 0x02], 0);
    assert!(number(&mut cursor, "truncated").is_err());
}

#[test]
fn bit_vectors_run_most_significant_bit_first_within_each_byte() {
    let mut cursor = Cursor::new(&[0b1010_0000], 0);
    assert_eq!(bits(&mut cursor, 3, "empty streams").unwrap(), [true, false, true]);

    let mut cursor = Cursor::new(&[0b0100_0000, 0b1000_0000], 0);
    assert_eq!(bits(&mut cursor, 9, "empty streams").unwrap(), [false, true, false, false, false, false, false, false, true]);
}

#[test]
fn a_bit_vector_encodes_back_to_the_bytes_it_came_from() {
    for encoded in [&[0b1010_0000u8][..], &[0b0100_0000, 0b1000_0000][..], &[0xFF, 0xFF][..], &[0x00][..]] {
        let len = encoded.len() * 8;
        let decoded = bits(&mut Cursor::new(encoded, 0), len, "round trip").unwrap();
        let mut out = Vec::new();
        put_bits(&mut out, &decoded);
        assert_eq!(out, encoded);
    }
}

#[test]
fn an_all_are_defined_byte_replaces_the_vector_it_precedes() {
    let mut cursor = Cursor::new(&[0x01], 0);
    assert_eq!(defined_bits(&mut cursor, 5, "defined").unwrap(), [true; 5]);
    assert!(cursor.is_empty());

    let mut cursor = Cursor::new(&[0x00, 0b1000_1000], 0);
    assert_eq!(defined_bits(&mut cursor, 5, "defined").unwrap(), [true, false, false, false, true]);

    let mut out = Vec::new();
    put_defined_bits(&mut out, &[true; 5]);
    assert_eq!(out, [0x01]);

    let mut out = Vec::new();
    put_defined_bits(&mut out, &[true, false, false, false, true]);
    assert_eq!(out, [0x00, 0b1000_1000]);
}

#[test]
fn the_signature_identifies_a_seven_zip_archive_and_nothing_else() {
    assert!(is_sevenz(&[0x37, 0x7A, 0xBC, 0xAF, 0x27, 0x1C, 0x00, 0x04]));
    assert!(!is_sevenz(&[0x37, 0x7A, 0xBC, 0xAF, 0x27]));
    assert!(!is_sevenz(&[0xFD, 0x37, 0x7A, 0x58, 0x5A, 0x00]));
    assert!(!is_sevenz(&[]));
}

#[test]
fn both_the_short_and_the_long_spelling_of_a_branch_coder_resolve_alike() {
    let pairs: [(&[u8], &[u8], Branch); 6] = [
        (&[0x04], &[0x03, 0x03, 0x01, 0x03], Branch::X86),
        (&[0x05], &[0x03, 0x03, 0x02, 0x05], Branch::Ppc),
        (&[0x06], &[0x03, 0x03, 0x04, 0x01], Branch::Ia64),
        (&[0x07], &[0x03, 0x03, 0x05, 0x01], Branch::Arm),
        (&[0x08], &[0x03, 0x03, 0x07, 0x01], Branch::ArmThumb),
        (&[0x09], &[0x03, 0x03, 0x08, 0x05], Branch::Sparc),
    ];

    for (short, long, branch) in pairs {
        assert_eq!(Codec::from_id(CoderId::new(short).unwrap()).unwrap(), Codec::Branch(branch));
        assert_eq!(Codec::from_id(CoderId::new(long).unwrap()).unwrap(), Codec::Branch(branch));
    }
}

#[test]
fn the_coders_the_cli_actually_writes_all_resolve() {
    let known: [(&[u8], Codec); 9] = [
        (&[0x00], Codec::Copy),
        (&[0x03], Codec::Delta),
        (&[0x21], Codec::Lzma2),
        (&[0x03, 0x01, 0x01], Codec::Lzma),
        (&[0x03, 0x03, 0x01, 0x1B], Codec::Bcj2),
        (&[0x03, 0x04, 0x01], Codec::Ppmd),
        (&[0x04, 0x01, 0x08], Codec::Deflate),
        (&[0x04, 0x02, 0x02], Codec::Bzip2),
        (&[0x06, 0xF1, 0x07, 0x01], Codec::Aes256Sha256),
    ];

    for (id, codec) in known {
        assert_eq!(Codec::from_id(CoderId::new(id).unwrap()).unwrap(), codec);
    }
}

#[test]
fn a_coder_from_a_fork_is_named_in_the_error_rather_than_reported_as_corruption() {
    let error = Codec::from_id(CoderId::new(&[0x04, 0xF7, 0x11, 0x01]).unwrap()).unwrap_err();
    assert!(error.is_unsupported());
    assert!(error.to_string().contains("Zstandard"), "{error}");

    let error = Codec::from_id(CoderId::new(&[0x7F, 0x7F, 0x7F]).unwrap()).unwrap_err();
    assert!(error.is_unsupported());
}

fn folder(bytes: &[u8], unpack_sizes: Vec<u64>) -> Folder {
    let mut folder = Folder::read(&mut Cursor::new(bytes, 0)).expect("a well formed folder");
    folder.unpack_sizes = unpack_sizes;
    folder
}

#[test]
fn a_linear_folder_resolves_its_coders_into_the_order_they_run_in() {
    let parsed = folder(&[0x02, 0x01, 0x00, 0x01, 0x00, 0x00, 0x01], vec![40, 40]);

    assert_eq!(parsed.coders.len(), 2);
    assert_eq!(parsed.total_in_streams(), 2);
    assert_eq!(parsed.total_out_streams(), 2);
    assert_eq!(parsed.packed_indices, [1]);
    assert_eq!(parsed.main_out_stream().unwrap(), 0);
    assert_eq!(parsed.unpack_size().unwrap(), 40);
    assert!(parsed.is_linear());
}

#[test]
fn a_folder_that_binds_its_coders_into_a_cycle_is_refused_rather_than_recursed() {
    let parsed = folder(&[0x03, 0x01, 0x00, 0x01, 0x00, 0x01, 0x00, 0x00, 0x01, 0x01, 0x01], vec![8, 8, 8]);

    assert_eq!(parsed.main_out_stream().unwrap(), 0);
    assert_eq!(parsed.packed_indices, [2]);

    let packed: Vec<Box<dyn std::io::Read + Send>> = vec![Box::new(&b"12345678"[..])];
    match chain::decoder(&parsed, packed, None) {
        Ok(_) => panic!("a cyclic folder was accepted"),
        Err(error) => assert!(error.to_string().contains("cycle"), "{error}"),
    }
}

#[test]
fn a_folder_whose_bind_pair_points_past_its_coders_is_refused() {
    assert!(Folder::read(&mut Cursor::new(&[0x02, 0x01, 0x00, 0x01, 0x00, 0x09, 0x01], 0)).is_err());
    assert!(Folder::read(&mut Cursor::new(&[0x02, 0x01, 0x00, 0x01, 0x00, 0x00, 0x09], 0)).is_err());
}

#[test]
fn a_coder_id_outside_one_to_fifteen_bytes_is_refused() {
    assert!(CoderId::new(&[]).is_err());
    assert!(CoderId::new(&[0; 16]).is_err());
    assert_eq!(CoderId::new(&[0x03, 0x01, 0x01]).unwrap().as_slice(), [0x03, 0x01, 0x01]);
    assert_eq!(format!("{:?}", CoderId::new(&[0x06, 0xF1, 0x07, 0x01]).unwrap()), "06F10701");
}
