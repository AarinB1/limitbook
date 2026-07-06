//! Validates the checked-in ground-truth fixture (see tests/fixtures/README.md
//! at the workspace root). If these assertions fail after regenerating the
//! fixture, update the expected counts from `make-fixture`'s summary output.

use std::io::Read;

use limitbook_core::frame::{MIN_MESSAGE_LEN, Messages};

const FIXTURE_GZ: &[u8] = include_bytes!("../../../tests/fixtures/itch50_20191230.itch.gz");

/// Every message type defined by ITCH 5.0 (spec/itch50_spec.txt, section 1).
const VALID_TYPES: &[u8] = b"SRHYLVWKJhAFECXDUPQBINO";

/// Message types actually present in this fixture, with exact counts from
/// generation (tools/regen_fixture.sh).
const EXPECTED_COUNTS: &[(u8, usize)] = &[
    (b'A', 39298),
    (b'D', 38613),
    (b'E', 395),
    (b'H', 13),
    (b'L', 597),
    (b'P', 108),
    (b'R', 13),
    (b'S', 2),
    (b'U', 3946),
    (b'X', 11387),
    (b'Y', 13),
];

fn decompressed_fixture() -> Vec<u8> {
    let mut raw = Vec::new();
    flate2::read::GzDecoder::new(FIXTURE_GZ)
        .read_to_end(&mut raw)
        .expect("fixture must be valid gzip");
    raw
}

#[test]
fn fixture_frames_cleanly_with_expected_contents() {
    let raw = decompressed_fixture();
    let mut counts = std::collections::BTreeMap::new();
    let mut total = 0usize;
    for msg in Messages::new(&raw) {
        let msg = msg.expect("fixture must contain no framing errors");
        assert!(msg.len() >= MIN_MESSAGE_LEN);
        assert!(
            VALID_TYPES.contains(&msg[0]),
            "unknown message type {:?}",
            msg[0] as char
        );
        *counts.entry(msg[0]).or_insert(0usize) += 1;
        total += 1;
    }
    assert_eq!(total, 94385);
    let expected: std::collections::BTreeMap<u8, usize> = EXPECTED_COUNTS.iter().copied().collect();
    assert_eq!(counts, expected);
}

#[test]
fn fixture_starts_with_system_event() {
    let raw = decompressed_fixture();
    let first = Messages::new(&raw)
        .next()
        .expect("fixture is not empty")
        .expect("first frame is valid");
    // Spec 1.1: System Event Message, type "S", 12 bytes.
    assert_eq!(first[0], b'S');
    assert_eq!(first.len(), 12);
}
