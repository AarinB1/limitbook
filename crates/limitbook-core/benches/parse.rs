//! Criterion micro-benchmarks for [`limitbook_core::parse::decode`], one per
//! ITCH 5.0 message type.
//!
//! Payloads are synthesized at the spec's exact fixed length for each type
//! ([`message_len`]), with valid values in the strictly-validated coded
//! fields (Side, Event Code, Trading State) so every decode takes the
//! success path — the same path a clean feed replay takes. Decode cost
//! depends on the layout, not the field values, so synthetic payloads
//! measure the same work as real ones; `benches/fixture.rs` cross-checks
//! against real captured data.

use criterion::{Criterion, criterion_group, criterion_main};
use std::hint::black_box;

use limitbook_core::parse::{decode, message_len};

/// Every message type defined by ITCH 5.0 (spec/itch50_spec.txt, section 1).
const ALL_TYPES: &[u8] = b"SRHYLVWKJhAFECXDUPQBINO";

/// Builds a decodable payload of the spec's exact length for `ty`: header
/// fields nonzero, strictly-validated coded fields set to defined values,
/// everything else a filler byte (informational fields pass through raw).
fn payload_for(ty: u8) -> Vec<u8> {
    let len = message_len(ty).expect("all benched types are spec-defined");
    let mut p = vec![0x20u8; len];
    p[0] = ty;
    p[1..3].copy_from_slice(&7u16.to_be_bytes()); // stock locate
    p[3..5].copy_from_slice(&1u16.to_be_bytes()); // tracking number
    p[5..11].copy_from_slice(&[0x00, 0x2A, 0x1D, 0x5C, 0xD1, 0x00]); // timestamp
    match ty {
        b'S' => p[11] = b'O',               // Event Code (spec section 1.1)
        b'H' => p[19] = b'T',               // Trading State (spec section 1.2.2)
        b'A' | b'F' | b'P' => p[19] = b'B', // Buy/Sell Indicator (spec section 1.3.1)
        _ => {}
    }
    p
}

fn bench_decode(c: &mut Criterion) {
    let mut group = c.benchmark_group("decode");
    for &ty in ALL_TYPES {
        let payload = payload_for(ty);
        decode(&payload).expect("synthesized payload must decode");
        group.bench_function(String::from(ty as char), |b| {
            b.iter(|| decode(black_box(payload.as_slice())).unwrap())
        });
    }
    group.finish();
}

criterion_group!(benches, bench_decode);
criterion_main!(benches);
