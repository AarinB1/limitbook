//! Replays the checked-in fixtures through the wasm-boundary `Engine` (as an
//! rlib on the host) and asserts each lands on the same final state the CLI
//! replay reports. Any drift here is a bug in the glue layer, not the core.
//!
//! Two fixtures, two regimes: the pre-market slice (crossed-book invariant
//! dormant) and the mid-day continuous-trading slice the browser demo
//! replays (invariant armed). The browser's end-of-replay verdict asserts
//! the same numbers as `midday_engine_matches_cli_replay`.

use std::io::Read;

use limitbook_wasm::Engine;

fn fixture_bytes(name: &str) -> Vec<u8> {
    let path = format!("{}/../../tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
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

/// Steps in uneven batches to exercise resuming mid-stream; returns the
/// total number of messages fed.
fn replay_to_end(engine: &mut Engine) -> u64 {
    let mut total = 0u64;
    for batch in [1, 7, 1000, 4096].iter().cycle() {
        let fed = engine.step(*batch);
        total += u64::from(fed);
        if engine.done() {
            break;
        }
        assert!(fed > 0, "step returned 0 before done()");
    }
    total
}

#[test]
fn engine_matches_cli_replay() {
    let mut engine = Engine::new(fixture_bytes("itch50_20191230.itch.gz"), 1000);
    let total = replay_to_end(&mut engine);

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

#[test]
fn midday_engine_matches_cli_replay() {
    let mut engine = Engine::new(fixture_bytes("itch50_20191230_midday.itch.gz"), 1000);
    let total = replay_to_end(&mut engine);

    // Totals the CLI reports for this fixture (tools/regen_midday_fixture.sh,
    // tests/fixtures/README.md). The slice sits inside market hours, so a
    // clean run also proves the armed crossed-book invariant held.
    assert_eq!(total, 157_824);
    assert_eq!(engine.messages(), 157_824.0);
    assert_eq!(engine.violations(), 0.0);
    assert_eq!(engine.live_orders(), 1_411);

    // End-of-slice quotes, cross-checked against `limitbook replay`.
    assert_eq!(quote(&engine, "AAPL"), (2_910_300.0, 2_910_500.0));
    assert_eq!(quote(&engine, "SPY"), (3_217_000.0, 3_217_100.0));
    assert_eq!(quote(&engine, "TSLA"), (4_189_400.0, 4_190_500.0));
}
