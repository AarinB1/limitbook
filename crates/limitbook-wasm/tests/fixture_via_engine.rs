//! Replays the checked-in fixture through the wasm-boundary `Engine` (as an
//! rlib on the host) and asserts it lands on the same final state the CLI
//! replay reports. Any drift here is a bug in the glue layer, not the core.

use std::io::Read;

use limitbook_wasm::Engine;

fn fixture_bytes() -> Vec<u8> {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/fixtures/itch50_20191230.itch.gz"
    );
    let gz = std::fs::read(path).expect("read fixture");
    let mut raw = Vec::new();
    flate2::read::GzDecoder::new(&gz[..])
        .read_to_end(&mut raw)
        .expect("gunzip fixture");
    raw
}

/// Best bid/ask in Price(4) ticks for a symbol's book.
fn quote(engine: &Engine, symbol: &str) -> (f64, f64) {
    let locate = engine.locate(symbol);
    assert!(locate >= 0, "{symbol} missing from directory");
    let snap = engine.snapshot(locate as u16, 1);
    let n_bid = snap[0] as usize;
    let n_ask = snap[1] as usize;
    assert!(n_bid > 0 && n_ask > 0, "{symbol} book is empty on a side");
    (snap[2], snap[2 + 3 * n_bid])
}

#[test]
fn engine_matches_cli_replay() {
    let mut engine = Engine::new(fixture_bytes(), 1000);

    // Step in uneven batches to exercise resuming mid-stream.
    let mut total = 0u64;
    for batch in [1, 7, 1000, 4096].iter().cycle() {
        let fed = engine.step(*batch);
        total += u64::from(fed);
        if engine.done() {
            break;
        }
        assert!(fed > 0, "step returned 0 before done()");
    }

    // Totals the CLI reports for this fixture.
    assert_eq!(total, 94_385);
    assert_eq!(engine.messages(), 94_385.0);
    assert_eq!(engine.violations(), 0.0);
    assert_eq!(engine.live_orders(), 500);

    // End-of-slice quotes, cross-checked against `limitbook replay`.
    assert_eq!(quote(&engine, "AAPL"), (2_894_100.0, 2_895_000.0));
    assert_eq!(quote(&engine, "TSLA"), (4_310_000.0, 4_315_000.0));

    // Once exhausted, further steps are no-ops.
    assert_eq!(engine.step(100), 0);
    assert_eq!(engine.messages(), 94_385.0);
}
