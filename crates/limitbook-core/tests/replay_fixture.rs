//! Full-pipeline ground truth: the entire fixture through frame + parse +
//! book via the same `Replay` engine the CLI uses.
//!
//! Every expected number below was computed by an INDEPENDENT simulation
//! (a from-scratch Python dict-based book working straight from the spec
//! offsets), not by this crate — so agreement here means two separate
//! implementations of the spec reconcile on real data. The fixture
//! guarantees per-symbol referential integrity, so anything nonzero in the
//! violation counters is a real bug.

use std::io::Read;

use limitbook_core::frame::Messages;
use limitbook_core::parse::{EventCode, Price4, Side};
use limitbook_core::replay::Replay;

const FIXTURE_GZ: &[u8] = include_bytes!("../../../tests/fixtures/itch50_20191230.itch.gz");

fn run_replay() -> Replay {
    let mut raw = Vec::new();
    flate2::read::GzDecoder::new(FIXTURE_GZ)
        .read_to_end(&mut raw)
        .expect("fixture must be valid gzip");
    let mut replay = Replay::new(1000);
    for payload in Messages::new(&raw) {
        replay.feed(payload.expect("no framing errors"));
    }
    replay.finish();
    replay
}

#[test]
fn fixture_replays_with_zero_violations() {
    let replay = run_replay();
    let stats = replay.stats();

    assert_eq!(stats.messages, 94_385);

    // The whole point: no dangling references, no invariant violations.
    assert_eq!(stats.parse_errors, 0);
    assert_eq!(stats.book_errors, 0, "{:?}", replay.violations());
    assert_eq!(stats.crossed_in_continuous, 0);
    assert_eq!(stats.consistency_failures, 0, "{:?}", replay.violations());
    assert_eq!(stats.symbol_mismatches, 0);
    assert!(replay.clean());

    // Final deep verify again, explicitly.
    replay
        .market()
        .verify()
        .expect("final book self-consistent");
}

#[test]
fn fixture_stats_match_independent_simulation() {
    let replay = run_replay();
    let stats = replay.stats();

    // Message mix (from fixture generation).
    assert_eq!(stats.by_type[b'A' as usize], 39_298);
    assert_eq!(stats.by_type[b'D' as usize], 38_613);
    assert_eq!(stats.by_type[b'E' as usize], 395);
    assert_eq!(stats.by_type[b'U' as usize], 3_946);
    assert_eq!(stats.by_type[b'X' as usize], 11_387);
    assert_eq!(stats.by_type[b'P' as usize], 108);
    assert_eq!(stats.by_type[b'F' as usize], 0);
    assert_eq!(stats.by_type[b'C' as usize], 0);

    // Book activity (independent Python simulation values).
    assert_eq!(stats.orders_added, 39_298);
    assert_eq!(stats.shares_added, 16_255_424);
    assert_eq!(stats.exec_events, 395);
    assert_eq!(stats.exec_shares, 52_120);
    assert_eq!(stats.exec_with_price_events, 0);
    assert_eq!(stats.orders_filled, 185);
    assert_eq!(stats.cancel_events, 11_387);
    assert_eq!(stats.cancelled_shares, 2_158_290);
    assert_eq!(stats.orders_cancelled_out, 0);
    assert_eq!(stats.deletes, 38_613);
    assert_eq!(stats.deleted_shares, 13_890_969);
    assert_eq!(stats.replaces, 3_946);
    assert_eq!(stats.trades, 108);
    assert_eq!(stats.trade_shares, 9_437);
    assert_eq!(stats.timestamp_regressions, 0);

    // Conservation: creations - removals = live orders at end.
    let created = stats.orders_added + stats.replaces;
    let removed = stats.deletes + stats.replaces + stats.orders_filled + stats.orders_cancelled_out;
    assert_eq!(created - removed, 500);
    assert_eq!(replay.market().live_orders(), 500);

    // Peaks (independent simulation).
    assert_eq!(stats.peak_live_orders, 523);
    assert_eq!(stats.peak_live_orders_at, 92_106);
    assert_eq!(stats.peak_book_orders, 145);
    assert_eq!(stats.peak_book_orders_locate, 7_992); // TSLA
    assert_eq!(stats.peak_side_levels, 50);
    assert_eq!(stats.peak_side_levels_locate, 7_992);
}

#[test]
fn fixture_market_phase_and_final_books() {
    let replay = run_replay();

    // The fixture is a pre-market slice: start-of-messages and
    // start-of-system-hours only. Market hours never begin, so the crossed
    // check was scoped out for the entire replay — by design, since
    // pre-open books legitimately cross.
    assert_eq!(
        replay.system_events(),
        &[
            (EventCode::StartOfMessages, 11_072_057_543_747),
            (EventCode::StartOfSystemHours, 14_400_000_198_145),
        ]
    );
    assert!(!replay.market().market_hours());

    // 13 symbols in the directory.
    assert_eq!(replay.symbols().len(), 13);
    assert_eq!(
        replay.symbols().get(&13).map(|s| &s[..]),
        Some(&b"AAPL    "[..])
    );
    assert_eq!(
        replay.symbols().get(&7992).map(|s| &s[..]),
        Some(&b"TSLA    "[..])
    );

    // Spot-check final books against the independent simulation:
    // AAPL 289.41 / 289.50, 54 live orders.
    let aapl = replay.market().book(13).unwrap();
    assert_eq!(aapl.order_count(), 54);
    assert_eq!(aapl.best_bid().map(|(p, _)| p), Some(Price4(2_894_100)));
    assert_eq!(aapl.best_ask().map(|(p, _)| p), Some(Price4(2_895_000)));

    // TSLA 431.00 / 431.50, 144 live orders.
    let tsla = replay.market().book(7992).unwrap();
    assert_eq!(tsla.order_count(), 144);
    assert_eq!(tsla.best_bid().map(|(p, _)| p), Some(Price4(4_310_000)));
    assert_eq!(tsla.best_ask().map(|(p, _)| p), Some(Price4(4_315_000)));

    // Every final book ends uncrossed (a data-health observation: even
    // though crossing was permitted pre-open, these books happen to end
    // clean) and internally ordered.
    for (locate, book) in replay.market().books() {
        if let (Some((bid, _)), Some((ask, _))) = (book.best_bid(), book.best_ask()) {
            assert!(bid < ask, "book {locate} ends crossed: {bid:?} >= {ask:?}");
        }
        let bid_prices: Vec<u32> = book.side_levels(Side::Buy).map(|(p, _, _)| p.0).collect();
        assert!(bid_prices.is_sorted_by(|a, b| a > b));
        let ask_prices: Vec<u32> = book.side_levels(Side::Sell).map(|(p, _, _)| p.0).collect();
        assert!(ask_prices.is_sorted());
    }

    // Per-symbol totals reconcile with the global counters.
    let per = replay.per_symbol();
    assert_eq!(per.len(), 13);
    let stats = replay.stats();
    assert_eq!(
        per.values().map(|s| s.adds).sum::<u64>(),
        stats.orders_added
    );
    assert_eq!(
        per.values().map(|s| s.exec_shares).sum::<u64>(),
        stats.exec_shares + stats.exec_with_price_shares
    );
    assert_eq!(per.values().map(|s| s.deletes).sum::<u64>(), stats.deletes);
    assert_eq!(
        per.values().map(|s| s.replaces).sum::<u64>(),
        stats.replaces
    );
}
