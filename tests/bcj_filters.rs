mod common;

use ttarchive::codecs::bcj;

const FILTERS: [(u64, &[u8]); 9] = [
    (bcj::X86, &[]),
    (bcj::POWERPC, &[]),
    (bcj::IA64, &[]),
    (bcj::ARM, &[]),
    (bcj::ARM_THUMB, &[]),
    (bcj::SPARC, &[]),
    (bcj::ARM64, &[]),
    (bcj::RISCV, &[]),
    (bcj::DELTA, &[3]),
];

fn name(id: u64) -> &'static str {
    match id {
        bcj::X86 => "x86",
        bcj::POWERPC => "powerpc",
        bcj::IA64 => "ia64",
        bcj::ARM => "arm",
        bcj::ARM_THUMB => "armthumb",
        bcj::SPARC => "sparc",
        bcj::ARM64 => "arm64",
        bcj::RISCV => "riscv",
        bcj::DELTA => "delta",
        _ => "unknown",
    }
}

#[test]
fn every_filter_undoes_its_own_conversion() {
    let original = common::machine_code();

    for (id, props) in FILTERS {
        let mut data = original.clone();
        bcj::encode(id, props, &mut data).unwrap_or_else(|e| panic!("{}: {e}", name(id)));
        assert_ne!(data, original, "{} did nothing at all to real machine code", name(id));

        bcj::decode(id, props, &mut data).unwrap_or_else(|e| panic!("{}: {e}", name(id)));
        assert_eq!(data, original, "{} did not come back", name(id));
    }
}

#[test]
fn every_filter_is_reversible_on_data_that_is_not_code() {
    for seed in [1u32, 2, 3] {
        let original = common::pseudo_random(50_000, seed);

        for (id, props) in FILTERS {
            let mut data = original.clone();
            bcj::encode(id, props, &mut data).unwrap();
            bcj::decode(id, props, &mut data).unwrap();
            assert_eq!(data, original, "{} on random data, seed {seed}", name(id));
        }
    }
}

#[test]
fn a_converted_stream_decodes_to_what_was_converted_at_any_start_offset() {
    let original = common::machine_code();

    for start in [0u32, 16, 0x1000, 0xFFFF_FFF0] {
        let props = start.to_le_bytes();
        for (id, _) in FILTERS {
            if id == bcj::DELTA {
                continue;
            }

            let mut data = original.clone();
            bcj::encode(id, &props, &mut data).unwrap();
            bcj::decode(id, &props, &mut data).unwrap();
            assert_eq!(data, original, "{} at start {start:#x}", name(id));
        }
    }
}

#[test]
fn a_start_offset_that_splits_an_instruction_is_refused() {
    let mut data = common::pseudo_random(4_096, 5);

    for (id, alignment) in [(bcj::IA64, 16u32), (bcj::ARM, 4), (bcj::ARM_THUMB, 2), (bcj::RISCV, 2), (bcj::ARM64, 4), (bcj::SPARC, 4), (bcj::POWERPC, 4)] {
        let bad = (alignment / 2).max(1).to_le_bytes();
        assert!(bcj::decode(id, &bad, &mut data).is_err(), "{} accepted a start offset inside an instruction", name(id));
        assert!(bcj::encode(id, &bad, &mut data).is_err(), "{} accepted a start offset inside an instruction", name(id));
        assert!(bcj::decode(id, &alignment.to_le_bytes(), &mut data).is_ok(), "{} refused an aligned start offset", name(id));
    }

    assert!(bcj::decode(bcj::X86, &1u32.to_le_bytes(), &mut data).is_ok(), "x86 instructions are not aligned at all");
}

#[test]
fn short_input_is_left_alone_rather_than_panicking() {
    for len in 0usize..16 {
        for (id, props) in FILTERS {
            let original = common::pseudo_random(len, 7);
            let mut data = original.clone();
            bcj::encode(id, props, &mut data).unwrap();
            bcj::decode(id, props, &mut data).unwrap();
            assert_eq!(data, original, "{} on {len} bytes", name(id));
        }
    }
}

#[test]
fn a_branch_filter_makes_repeated_calls_look_alike() {
    let mut data = Vec::new();
    for index in 0..64u32 {
        let target = 0x1000u32.wrapping_sub(index * 4 + 4) >> 2;
        data.extend_from_slice(&[target as u8, (target >> 8) as u8, (target >> 16) as u8, 0xEB]);
    }

    let plain = data.clone();
    bcj::encode(bcj::ARM, &[], &mut data).unwrap();

    let distinct = |bytes: &[u8]| -> usize {
        let mut words: Vec<[u8; 4]> = bytes.chunks_exact(4).map(|c| [c[0], c[1], c[2], c[3]]).collect();
        words.sort();
        words.dedup();
        words.len()
    };

    assert_eq!(distinct(&plain), 64, "every call in the input should differ");
    assert_eq!(distinct(&data), 1, "calls to one address should become one repeated word");

    bcj::decode(bcj::ARM, &[], &mut data).unwrap();
    assert_eq!(data, plain);
}

#[test]
fn the_riscv_filter_survives_a_stream_that_already_looks_converted() {
    let mut data = Vec::new();
    for index in 0..32u32 {
        data.extend_from_slice(&(0x17 | (2 << 7) | (index << 12)).to_le_bytes());
        data.extend_from_slice(&(0x1234_5678u32.wrapping_add(index)).to_be_bytes());
    }

    let original = data.clone();
    bcj::encode(bcj::RISCV, &[], &mut data).unwrap();
    bcj::decode(bcj::RISCV, &[], &mut data).unwrap();
    assert_eq!(data, original, "a stream shaped like the converted form must still round trip");
}

#[test]
fn a_call_instruction_at_the_very_start_is_converted_rather_than_crashing() {
    for lead in [0xE8u8, 0xE9] {
        let mut data = vec![lead, 0x00, 0x00, 0x00, 0x00, 0x90, 0x90, 0x90, lead, 0x10, 0x00, 0x00, 0x00, 0x90];
        let original = data.clone();

        bcj::decode(bcj::X86, &[], &mut data).unwrap();
        assert_ne!(data, original, "a call at offset 0 should be converted");

        bcj::encode(bcj::X86, &[], &mut data).unwrap();
        assert_eq!(data, original);
    }
}

#[test]
fn a_run_of_call_instructions_round_trips() {
    let mut data = Vec::new();
    for index in 0..200u32 {
        data.push(if index % 2 == 0 { 0xE8 } else { 0xE9 });
        data.extend_from_slice(&index.to_le_bytes());
    }

    let original = data.clone();
    bcj::encode(bcj::X86, &[], &mut data).unwrap();
    assert_ne!(data, original);
    bcj::decode(bcj::X86, &[], &mut data).unwrap();
    assert_eq!(data, original);
}

#[test]
fn a_streaming_converter_agrees_with_one_that_sees_everything() {
    let original = common::machine_code();

    for (id, props) in FILTERS {
        let mut whole = original.clone();
        bcj::decode(id, props, &mut whole).unwrap();

        for chunk in [1usize, 3, 16, 17, 64, 1024, 65_536] {
            let mut converter = bcj::Converter::decode(id, props).unwrap();
            let mut pending: Vec<u8> = Vec::new();
            let mut out: Vec<u8> = Vec::new();
            let mut at = 0usize;

            while at < original.len() {
                let take = chunk.min(original.len() - at);
                pending.extend_from_slice(&original[at..at + take]);
                at += take;

                let done = converter.convert(&mut pending, false).unwrap();
                out.extend_from_slice(&pending[..done]);
                pending.drain(..done);
            }

            let done = converter.convert(&mut pending, true).unwrap();
            out.extend_from_slice(&pending[..done]);

            assert_eq!(out.len(), whole.len(), "{} at chunk {chunk}", name(id));
            assert!(out == whole, "{} at chunk {chunk} converted differently in pieces", name(id));
        }
    }
}

#[test]
fn a_streaming_converter_holds_back_no_more_than_its_reserve() {
    let original = common::machine_code();

    for (id, props) in FILTERS {
        let mut converter = bcj::Converter::decode(id, props).unwrap();
        let mut pending: Vec<u8> = Vec::new();
        let mut at = 0usize;
        let mut most = 0usize;

        while at < original.len() {
            let take = 4_096.min(original.len() - at);
            pending.extend_from_slice(&original[at..at + take]);
            at += take;

            let done = converter.convert(&mut pending, false).unwrap();
            pending.drain(..done);
            most = most.max(pending.len());
        }

        assert!(most <= bcj::reserve(id).max(1), "{} held back {most} bytes, past its {} byte reserve", name(id), bcj::reserve(id));
    }
}
