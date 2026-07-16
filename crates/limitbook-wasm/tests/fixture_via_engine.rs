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

/// The queue accessors are read-only presentation over the same book the
/// parity checks pin down: every exposed level queue must reconcile with
/// the aggregated snapshot (same order count, same share total), the
/// front-of-line order must report rank 0, and — the non-negotiable part —
/// an engine that watched an order all the way to the end must land on a
/// final state identical to one that never watched anything.
#[test]
fn midday_queue_accessors_are_read_only_and_reconcile_with_snapshot() {
    let bytes = fixture_bytes("itch50_20191230_midday.itch.gz");

    // Engine A: plain replay, no queue reads.
    let mut plain = Engine::new(bytes.clone(), 1000);
    replay_to_end(&mut plain);

    // Engine B: replay half way, watch the order at the front of AAPL's
    // best bid queue, read queues along the way, then run to the end.
    let mut watched = Engine::new(bytes, 1000);
    while watched.messages() < 80_000.0 {
        watched.step(4096);
    }
    let aapl = watched.locate("AAPL") as u16;
    let snap = watched.snapshot(aapl, 1);
    assert!(snap[0] >= 1.0, "AAPL has a bid mid-replay");
    let best_bid = snap[2] as u32;
    let queue = watched.level_queue(aapl, true, best_bid);
    assert!(!queue.is_empty(), "best-bid level has a queue");
    let front_ref = queue[0];
    let pos = watched.order_position(front_ref);
    assert_eq!(pos[0], f64::from(best_bid));
    assert_eq!(pos[1], 1.0, "bid side");
    assert_eq!(pos[2], queue[1] as f64, "shares match the queue record");
    assert_eq!(pos[3], 0.0, "front of the line");
    assert_eq!(pos[4], 0.0, "nothing ahead of the front");
    assert_eq!(pos[5], (queue.len() / 2) as f64, "queue length");
    watched.watch(front_ref);
    replay_to_end(&mut watched);

    // Watching and queue reads changed nothing: identical final state.
    assert_eq!(watched.messages(), plain.messages());
    assert_eq!(watched.violations(), plain.violations());
    assert_eq!(watched.live_orders(), plain.live_orders());
    for sym in ["AAPL", "SPY", "TSLA"] {
        let locate = plain.locate(sym) as u16;
        assert_eq!(
            watched.snapshot(locate, 4096),
            plain.snapshot(locate, 4096),
            "{sym} final book must be identical with and without watching"
        );
    }

    // The watched order was resting mid-day; whatever happened to it, the
    // event stream must account for it: either it is still live at the end
    // or its death was witnessed with a valid fate kind.
    let events = watched.take_watch_events();
    assert_eq!(events.len() % 5, 0, "5-slot event records");
    for record in events.chunks(5) {
        assert!((1.0..=4.0).contains(&record[0]), "valid fate kind");
    }
    if watched.order_position(front_ref).is_empty() {
        assert!(
            !events.is_empty(),
            "order died but no fate event was recorded"
        );
    }

    // Every exposed queue reconciles with the aggregated snapshot.
    for sym in ["AAPL", "SPY", "TSLA"] {
        let locate = plain.locate(sym) as u16;
        let snap = plain.snapshot(locate, 4096);
        let (n_bid, n_ask) = (snap[0] as usize, snap[1] as usize);
        for (base, count, bid) in [(2, n_bid, true), (2 + 3 * n_bid, n_ask, false)] {
            for level in 0..count {
                let price = snap[base + 3 * level] as u32;
                let shares = snap[base + 3 * level + 1];
                let orders = snap[base + 3 * level + 2] as usize;
                let queue = plain.level_queue(locate, bid, price);
                assert_eq!(queue.len(), 2 * orders, "{sym} queue length at {price}");
                let total: u64 = queue.chunks(2).map(|pair| pair[1]).sum();
                assert_eq!(total as f64, shares, "{sym} queue shares at {price}");
            }
        }
    }
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
