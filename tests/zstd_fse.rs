mod common;

use ttarchive::codecs::zstd::bits::{BackwardBits, BitWriter};
use ttarchive::codecs::zstd::fse::{EncTable, State, Table};

fn round_trip(counts: &[i32], log: u32, symbols: &[u8]) -> Vec<u8> {
    let table = Table::from_counts(counts, log).expect("the distribution should fill the table");
    let encoder = EncTable::new(table.clone()).expect("the decode table should invert");

    let mut writer = BitWriter::new();

    let (last, rest) = symbols.split_last().expect("a run needs at least one symbol");
    let mut state = encoder.start(*last).expect("the table should carry the symbol");
    for &symbol in rest.iter().rev() {
        encoder.encode(&mut state, symbol, &mut writer).expect("the table should carry the symbol");
    }
    encoder.flush(state, &mut writer);

    let stream = writer.finish();

    let mut bits = BackwardBits::new(&stream).expect("the writer should mark the end of the stream");
    let mut state = State::new(&table, &mut bits);

    let mut out = Vec::with_capacity(symbols.len());
    for index in 0..symbols.len() {
        out.push(state.symbol(&table));
        if index + 1 < symbols.len() {
            state.advance(&table, &mut bits);
        }
    }

    out
}

#[test]
fn a_flat_distribution_round_trips() {
    let counts = [8, 8, 8, 8];
    let symbols: Vec<u8> = (0..64u8).map(|i| i % 4).collect();

    assert_eq!(round_trip(&counts, 5, &symbols), symbols);
}

#[test]
fn a_skewed_distribution_round_trips() {
    let counts = [26, 2, 2, 2];
    let symbols: Vec<u8> = (0..200u32).map(|i| if i % 7 == 0 { (i % 4) as u8 } else { 0 }).collect();

    assert_eq!(round_trip(&counts, 5, &symbols), symbols);
}

#[test]
fn low_probability_symbols_round_trip() {
    let counts = [21, 6, 3, -1, -1];
    let symbols: Vec<u8> = vec![0, 1, 3, 0, 4, 2, 0, 0, 3, 1, 4, 0, 2, 0, 1, 0];

    assert_eq!(round_trip(&counts, 5, &symbols), symbols);
}

#[test]
fn a_single_symbol_run_round_trips() {
    let counts = [8, 8, 8, 8];
    assert_eq!(round_trip(&counts, 5, &[2]), vec![2]);
}

#[test]
fn the_predefined_offset_distribution_round_trips() {
    let counts = [1, 1, 1, 1, 1, 1, 2, 2, 2, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, -1, -1, -1, -1, -1];
    let symbols: Vec<u8> = (0..300u32).map(|i| (i * 7 % 29) as u8).collect();

    assert_eq!(round_trip(&counts, 5, &symbols), symbols);
}

#[test]
fn the_predefined_literal_length_distribution_round_trips() {
    let counts = [4, 3, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 1, 1, 1, 2, 2, 2, 2, 2, 2, 2, 2, 2, 3, 2, 1, 1, 1, 1, 1, -1, -1, -1, -1];
    let symbols: Vec<u8> = (0..500u32).map(|i| (i * 13 % 36) as u8).collect();

    assert_eq!(round_trip(&counts, 6, &symbols), symbols);
}

#[test]
fn the_predefined_match_length_distribution_round_trips() {
    let counts = [
        1, 4, 3, 2, 2, 2, 2, 2, 2, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, -1, -1, -1,
        -1, -1, -1, -1,
    ];
    let symbols: Vec<u8> = (0..600u32).map(|i| (i * 11 % 53) as u8).collect();

    assert_eq!(round_trip(&counts, 6, &symbols), symbols);
}

#[test]
fn a_long_pseudo_random_run_round_trips() {
    let counts = [4, 3, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 1, 1, 1, 2, 2, 2, 2, 2, 2, 2, 2, 2, 3, 2, 1, 1, 1, 1, 1, -1, -1, -1, -1];
    let symbols: Vec<u8> = common::pseudo_random(4096, 91).iter().map(|&b| b % 36).collect();

    assert_eq!(round_trip(&counts, 6, &symbols), symbols);
}

#[test]
fn a_table_log_the_format_forbids_is_rejected() {
    for log in [0, 1, 4, 10, 16] {
        assert!(Table::from_counts(&[8, 8, 8, 8], log).is_err(), "table log {log} should have been rejected");
    }
}

#[test]
fn the_bit_writer_never_ends_in_a_zero_byte() {
    for count in 0..40u32 {
        let mut writer = BitWriter::new();
        for _ in 0..count {
            writer.add(0, 1);
        }
        let stream = writer.finish();
        assert_ne!(stream.last().copied(), Some(0), "{count} zero bits produced a stream ending in a zero byte");
        assert!(BackwardBits::new(&stream).is_ok(), "{count} zero bits produced an unreadable stream");
    }
}

#[test]
fn the_bit_writer_returns_values_in_the_reverse_of_the_order_written() {
    let written: [(u64, u32); 6] = [(5, 3), (300, 9), (1, 1), (0, 4), (65_535, 16), (7, 3)];

    let mut writer = BitWriter::new();
    for &(value, bits) in &written {
        writer.add(value, bits);
    }
    let stream = writer.finish();

    let mut reader = BackwardBits::new(&stream).unwrap();
    for &(value, bits) in written.iter().rev() {
        assert_eq!(reader.bits(bits), value, "reading {bits} bits should give back {value}");
    }
}
