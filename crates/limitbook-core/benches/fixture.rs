//! Criterion benchmarks over the checked-in ground-truth fixture (94,385
//! real messages, 13 symbols, 2019-12-30 pre-market): whole-stream
//! parse-only and parse + book reconstruction from in-memory bytes.
//!
//! These are the criterion-grade counterparts of the whole-day numbers in
//! BENCHMARKS.md (`limitbook bench`): same code paths, statistically
//! sampled, but over the small fixture so they run in CI-scale time.
//! Throughput is reported in messages/second (`Throughput::Elements`).

use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use std::hint::black_box;
use std::io::Read;

use limitbook_core::book::Market;
use limitbook_core::frame::Messages;
use limitbook_core::parse;

const FIXTURE_GZ: &[u8] = include_bytes!("../../../tests/fixtures/itch50_20191230.itch.gz");

fn decompressed_fixture() -> Vec<u8> {
    let mut raw = Vec::new();
    flate2::read::GzDecoder::new(FIXTURE_GZ)
        .read_to_end(&mut raw)
        .expect("fixture must be valid gzip");
    raw
}

fn message_count(raw: &[u8]) -> u64 {
    let mut n = 0u64;
    for m in Messages::new(raw) {
        m.expect("clean framing");
        n += 1;
    }
    n
}

fn bench_fixture(c: &mut Criterion) {
    let raw = decompressed_fixture();
    let n = message_count(&raw);

    let mut group = c.benchmark_group("fixture");
    group.throughput(Throughput::Elements(n));

    // Framing + decode of every message; the timestamp sum keeps the
    // decoded values observably live.
    group.bench_function("parse_only", |b| {
        b.iter(|| {
            let mut ts_sum = 0u64;
            for payload in Messages::new(black_box(raw.as_slice())) {
                let msg = parse::decode(payload.unwrap()).unwrap();
                ts_sum = ts_sum.wrapping_add(msg.header().timestamp);
            }
            ts_sum
        })
    });

    // Framing + decode + book reconstruction from an empty market. The
    // market is rebuilt inside the timed routine because reconstruction
    // from message 0 is the operation being measured; it is returned so
    // its drop is untimed.
    group.bench_function("parse_and_book", |b| {
        b.iter(|| {
            let mut market = Market::new();
            let mut book_errors = 0u64;
            for payload in Messages::new(black_box(raw.as_slice())) {
                let msg = parse::decode(payload.unwrap()).unwrap();
                if market.apply(&msg).is_err() {
                    book_errors += 1;
                }
            }
            assert_eq!(book_errors, 0);
            market
        })
    });

    group.finish();
}

criterion_group!(benches, bench_fixture);
criterion_main!(benches);
