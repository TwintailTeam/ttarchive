use ttarchive::utils::bytes::{Cursor, rfind};
use ttarchive::utils::error::Error;

#[test]
fn reads_little_endian_fields() {
    let buf = [0x50, 0x4b, 0x03, 0x04, 0x0a, 0x00];
    let mut c = Cursor::new(&buf, 100);

    assert_eq!(c.u32("sig").unwrap(), 0x0403_4b50);
    assert_eq!(c.offset(), 104, "offset is absolute within the archive");
    assert_eq!(c.u16("version").unwrap(), 10);
    assert!(c.is_empty());
}

#[test]
fn truncation_is_an_error_not_a_panic() {
    let buf = [0x01, 0x02];
    let mut c = Cursor::new(&buf, 0);

    let err = c.u32("sig").unwrap_err();
    assert!(matches!(err, Error::Malformed { .. }), "got {err:?}");
}

#[test]
fn rfind_takes_the_last_match() {
    let sig = [0x50, 0x4b, 0x05, 0x06];
    let mut buf = Vec::new();
    buf.extend_from_slice(&sig);
    buf.extend_from_slice(b"junk");
    buf.extend_from_slice(&sig);
    buf.push(0xff);

    assert_eq!(rfind(&buf, &sig), Some(8));
}

#[test]
fn rfind_handles_input_shorter_than_the_needle() {
    assert_eq!(rfind(b"ab", &[0x50, 0x4b, 0x05, 0x06]), None);
    assert_eq!(rfind(b"", &[0x50, 0x4b, 0x05, 0x06]), None);
}
