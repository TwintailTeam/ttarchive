use ttarchive::utils::cp437;
use ttarchive::utils::datetime::{self, Civil, DosDateTime, civil_from_unix, from_dos, to_dos, unix_from_civil};

#[test]
fn cp437_ascii_is_identity() {
    assert_eq!(cp437::decode(b"hello.txt"), "hello.txt");
    assert_eq!(cp437::encode("hello.txt").unwrap(), b"hello.txt");
}

#[test]
fn cp437_high_half_decodes() {
    assert_eq!(cp437::decode(&[0x81, 0xE1, 0x9C]), "üß£");
}

#[test]
fn cp437_round_trips() {
    let s = "Grüße£.txt";
    let bytes = cp437::encode(s).expect("all chars are in cp437");
    assert_eq!(cp437::decode(&bytes), s);
}

#[test]
fn cp437_encode_rejects_unmappable() {
    assert!(cp437::encode("日本語.txt").is_none());
    assert!(cp437::encode("emoji😀").is_none());
}

#[test]
fn decode_name_honours_utf8_flag() {
    let utf8 = "日本.txt".as_bytes();
    assert_eq!(cp437::decode_name(utf8, true), "日本.txt");

    assert_ne!(cp437::decode_name(utf8, false), "日本.txt");

    let invalid = &[0xFF, 0xFE, b'a'];
    assert_eq!(cp437::decode_name(invalid, true), cp437::decode(invalid));
}

#[test]
fn dos_epoch_is_1980() {
    let dt = to_dos(datetime::DOS_EPOCH);
    assert_eq!(from_dos(dt), datetime::DOS_EPOCH);

    let c = civil_from_unix(datetime::DOS_EPOCH);
    assert_eq!((c.year, c.month, c.day), (1980, 1, 1));
}

#[test]
fn dos_clamps_out_of_range_dates() {
    assert_eq!(to_dos(0), DosDateTime::MIN);
    assert_eq!(to_dos(-1_000_000), DosDateTime::MIN);
}

#[test]
fn dos_has_two_second_resolution() {
    let odd = unix_from_civil(Civil { year: 2026, month: 8, day: 4, hour: 12, minute: 30, second: 45 });
    assert_eq!(from_dos(to_dos(odd)), odd - 1);
}

#[test]
fn dos_round_trips_even_seconds() {
    for (y, mo, d, h, mi, s) in [(1980, 1, 1, 0, 0, 0), (2000, 2, 29, 23, 59, 58), (2026, 8, 4, 12, 30, 44), (2107, 12, 31, 23, 59, 58)] {
        let secs = unix_from_civil(Civil { year: y, month: mo, day: d, hour: h, minute: mi, second: s });
        assert_eq!(from_dos(to_dos(secs)), secs, "{y}-{mo}-{d} {h}:{mi}:{s}");
    }
}

#[test]
fn civil_conversion_round_trips() {
    for secs in [0i64, 1, 951_782_400, 1_754_265_600, 4_102_444_800] {
        assert_eq!(unix_from_civil(civil_from_unix(secs)), secs);
    }
}

#[test]
fn from_dos_tolerates_garbage() {
    let garbage = DosDateTime { time: 0xFFFF, date: 0x0000 };
    let secs = from_dos(garbage);
    let c = civil_from_unix(secs);
    assert!((1..=12).contains(&c.month));
    assert!((1..=31).contains(&c.day));
    assert!(c.hour <= 23);
}

#[test]
fn filetime_round_trips() {
    let secs = 1_754_265_600i64;
    assert_eq!(datetime::unix_from_filetime(datetime::filetime_from_unix(secs)), secs);

    assert_eq!(datetime::unix_from_filetime(0), -11_644_473_600);
}
