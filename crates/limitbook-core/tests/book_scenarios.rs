//! Order book scenario tests: the message semantics of spec §1.3–§1.4
//! exercised through the public API, including the error paths and the
//! atomicity guarantee (a rejected message leaves the market untouched).

use limitbook_core::book::{BookError, Effect, Market};
use limitbook_core::parse::{
    AddOrder, EventCode, Header, Message, OrderCancel, OrderDelete, OrderExecuted,
    OrderExecutedWithPrice, OrderReplace, Price4, Side, StockTradingAction, SystemEvent,
    TradingState,
};

fn hdr(locate: u16) -> Header {
    Header {
        stock_locate: locate,
        tracking_number: 0,
        timestamp: 0,
    }
}

fn add(locate: u16, oref: u64, side: Side, shares: u32, price: u32) -> Message<'static> {
    Message::AddOrder(AddOrder {
        header: hdr(locate),
        order_ref: oref,
        side,
        shares,
        stock: b"TEST    ",
        price: Price4(price),
        attribution: None,
    })
}

fn exec(locate: u16, oref: u64, shares: u32) -> Message<'static> {
    Message::OrderExecuted(OrderExecuted {
        header: hdr(locate),
        order_ref: oref,
        executed_shares: shares,
        match_number: 1,
    })
}

fn exec_with_price(locate: u16, oref: u64, shares: u32, price: u32) -> Message<'static> {
    Message::OrderExecutedWithPrice(OrderExecutedWithPrice {
        header: hdr(locate),
        order_ref: oref,
        executed_shares: shares,
        match_number: 2,
        printable: b'Y',
        execution_price: Price4(price),
    })
}

fn cancel(locate: u16, oref: u64, shares: u32) -> Message<'static> {
    Message::OrderCancel(OrderCancel {
        header: hdr(locate),
        order_ref: oref,
        cancelled_shares: shares,
    })
}

fn delete(locate: u16, oref: u64) -> Message<'static> {
    Message::OrderDelete(OrderDelete {
        header: hdr(locate),
        order_ref: oref,
    })
}

fn replace(locate: u16, orig: u64, new: u64, shares: u32, price: u32) -> Message<'static> {
    Message::OrderReplace(OrderReplace {
        header: hdr(locate),
        original_order_ref: orig,
        new_order_ref: new,
        shares,
        price: Price4(price),
    })
}

fn system_event(code: EventCode) -> Message<'static> {
    Message::SystemEvent(SystemEvent {
        header: hdr(0),
        event_code: code,
    })
}

fn trading_action(locate: u16, state: TradingState) -> Message<'static> {
    Message::StockTradingAction(StockTradingAction {
        header: hdr(locate),
        stock: b"TEST    ",
        trading_state: state,
        reserved: b' ',
        reason: b"    ",
    })
}

/// Applies a message that must succeed, verifying deep consistency after.
fn ok(market: &mut Market, msg: Message<'_>) -> Effect {
    let effect = market.apply(&msg).expect("apply should succeed");
    market.verify().expect("book must stay self-consistent");
    effect
}

/// Applies a message that must fail, and proves nothing changed.
fn fails(market: &mut Market, msg: Message<'_>, expected: BookError) {
    let before = market.clone();
    assert_eq!(market.apply(&msg), Err(expected));
    assert_eq!(*market, before, "failed apply must not mutate the market");
    market.verify().expect("book must stay self-consistent");
}

#[test]
fn adds_build_price_time_priority() {
    let mut m = Market::new();
    ok(&mut m, add(1, 10, Side::Buy, 100, 50_000));
    ok(&mut m, add(1, 11, Side::Buy, 200, 50_000)); // same level, later
    ok(&mut m, add(1, 12, Side::Buy, 300, 49_000)); // worse bid
    ok(&mut m, add(1, 20, Side::Sell, 150, 51_000));

    let book = m.book(1).unwrap();
    assert_eq!(book.best_bid(), Some((Price4(50_000), 300)));
    assert_eq!(book.best_ask(), Some((Price4(51_000), 150)));
    assert_eq!(book.order_count(), 4);
    assert_eq!(book.level_count(Side::Buy), 2);
    // Time priority within the top bid level: 10 arrived before 11.
    let queue: Vec<u64> = book.orders_at(Side::Buy, Price4(50_000)).collect();
    assert_eq!(queue, vec![10, 11]);
    assert!(!m.crossed(1));
}

#[test]
fn duplicate_add_rejected() {
    let mut m = Market::new();
    ok(&mut m, add(1, 10, Side::Buy, 100, 50_000));
    fails(
        &mut m,
        add(1, 10, Side::Sell, 999, 60_000),
        BookError::DuplicateOrder { order_ref: 10 },
    );
}

#[test]
fn execute_partial_then_full() {
    let mut m = Market::new();
    ok(&mut m, add(1, 10, Side::Sell, 100, 51_000));
    ok(&mut m, add(1, 11, Side::Sell, 50, 51_000));

    // Partial execution: shares shrink, queue position kept.
    assert_eq!(
        ok(&mut m, exec(1, 10, 30)),
        Effect::Executed {
            order_ref: 10,
            executed: 30,
            remaining: 70,
        }
    );
    assert_eq!(m.order(10).unwrap().shares, 70);
    assert_eq!(
        m.book(1).unwrap().shares_at(Side::Sell, Price4(51_000)),
        Some(120)
    );
    let queue: Vec<u64> = m
        .book(1)
        .unwrap()
        .orders_at(Side::Sell, Price4(51_000))
        .collect();
    assert_eq!(
        queue,
        vec![10, 11],
        "partial execution keeps queue position"
    );

    // Executing the exact remainder kills the order (spec §1.4: at zero
    // display shares the order is dead).
    assert_eq!(
        ok(&mut m, exec(1, 10, 70)),
        Effect::Executed {
            order_ref: 10,
            executed: 70,
            remaining: 0,
        }
    );
    assert!(m.order(10).is_none());
    let queue: Vec<u64> = m
        .book(1)
        .unwrap()
        .orders_at(Side::Sell, Price4(51_000))
        .collect();
    assert_eq!(queue, vec![11]);

    // The dead reference now dangles.
    fails(
        &mut m,
        exec(1, 10, 1),
        BookError::UnknownOrder { order_ref: 10 },
    );
}

#[test]
fn execute_with_price_has_same_book_effect() {
    let mut m = Market::new();
    ok(&mut m, add(1, 10, Side::Buy, 100, 50_000));
    // "C" deducts shares exactly like "E"; the price is tape-only.
    assert_eq!(
        ok(&mut m, exec_with_price(1, 10, 40, 50_500)),
        Effect::Executed {
            order_ref: 10,
            executed: 40,
            remaining: 60,
        }
    );
    assert_eq!(m.order(10).unwrap().shares, 60);
    // Display price unchanged by a "C" execution.
    assert_eq!(m.order(10).unwrap().price, Price4(50_000));
}

#[test]
fn overfill_rejected_and_state_untouched() {
    let mut m = Market::new();
    ok(&mut m, add(1, 10, Side::Buy, 100, 50_000));
    fails(
        &mut m,
        exec(1, 10, 101),
        BookError::Overfill {
            order_ref: 10,
            resting: 100,
            requested: 101,
        },
    );
    // Boundary: exactly the resting size is legal.
    ok(&mut m, exec(1, 10, 100));
    assert_eq!(m.live_orders(), 0);
}

#[test]
fn cancel_partial_to_zero_and_over() {
    let mut m = Market::new();
    ok(&mut m, add(1, 10, Side::Buy, 100, 50_000));
    ok(&mut m, add(1, 11, Side::Buy, 100, 50_000));

    assert_eq!(
        ok(&mut m, cancel(1, 10, 60)),
        Effect::Cancelled {
            order_ref: 10,
            cancelled: 60,
            remaining: 40,
        }
    );
    let queue: Vec<u64> = m
        .book(1)
        .unwrap()
        .orders_at(Side::Buy, Price4(50_000))
        .collect();
    assert_eq!(queue, vec![10, 11], "partial cancel keeps queue position");

    fails(
        &mut m,
        cancel(1, 10, 41),
        BookError::Overcancel {
            order_ref: 10,
            resting: 40,
            requested: 41,
        },
    );

    // Cancelling the exact remainder removes the order.
    assert_eq!(
        ok(&mut m, cancel(1, 10, 40)),
        Effect::Cancelled {
            order_ref: 10,
            cancelled: 40,
            remaining: 0,
        }
    );
    assert!(m.order(10).is_none());
}

#[test]
fn delete_removes_and_prunes_level() {
    let mut m = Market::new();
    ok(&mut m, add(1, 10, Side::Buy, 100, 50_000));
    ok(&mut m, add(1, 11, Side::Buy, 100, 49_000));
    assert_eq!(
        ok(&mut m, delete(1, 10)),
        Effect::Deleted {
            order_ref: 10,
            remaining: 100,
        }
    );
    let book = m.book(1).unwrap();
    assert_eq!(book.level_count(Side::Buy), 1, "emptied level is pruned");
    assert_eq!(book.best_bid(), Some((Price4(49_000), 100)));

    fails(
        &mut m,
        delete(1, 10),
        BookError::UnknownOrder { order_ref: 10 },
    );
}

#[test]
fn locate_mismatch_rejected_for_all_mutations() {
    let mut m = Market::new();
    ok(&mut m, add(1, 10, Side::Buy, 100, 50_000));
    let err = BookError::LocateMismatch {
        order_ref: 10,
        order_locate: 1,
        message_locate: 2,
    };
    fails(&mut m, exec(2, 10, 10), err);
    fails(&mut m, cancel(2, 10, 10), err);
    fails(&mut m, delete(2, 10), err);
    fails(&mut m, replace(2, 10, 11, 100, 50_000), err);
}

#[test]
fn replace_retires_original_and_resets_priority() {
    let mut m = Market::new();
    ok(&mut m, add(1, 10, Side::Buy, 100, 50_000));
    ok(&mut m, add(1, 11, Side::Buy, 100, 50_000));

    // Replace order 10 at the SAME price: it must go to the back — losing
    // time priority is the point of §1.4.5.
    assert_eq!(
        ok(&mut m, replace(1, 10, 12, 80, 50_000)),
        Effect::Replaced {
            original: 10,
            new: 12,
            prior_remaining: 100,
        }
    );
    let queue: Vec<u64> = m
        .book(1)
        .unwrap()
        .orders_at(Side::Buy, Price4(50_000))
        .collect();
    assert_eq!(queue, vec![11, 12], "replace re-enters at the back");

    // Side is inherited (the message carries none); shares/price are new.
    let new_order = m.order(12).unwrap();
    assert_eq!(new_order.side, Side::Buy);
    assert_eq!(new_order.shares, 80);
    assert!(m.order(10).is_none(), "original reference retired");
    fails(
        &mut m,
        exec(1, 10, 1),
        BookError::UnknownOrder { order_ref: 10 },
    );

    // Replace to a different price moves levels; the old level survives
    // only because order 11 is still there.
    ok(&mut m, replace(1, 12, 13, 80, 49_500));
    let book = m.book(1).unwrap();
    assert_eq!(book.best_bid(), Some((Price4(50_000), 100)));
    assert_eq!(book.shares_at(Side::Buy, Price4(49_500)), Some(80));

    // Chained replaces work: 13 is live, 12 retired.
    fails(
        &mut m,
        replace(1, 12, 14, 10, 49_000),
        BookError::UnknownOrder { order_ref: 12 },
    );
}

#[test]
fn replace_atomicity_on_new_ref_collision() {
    let mut m = Market::new();
    ok(&mut m, add(1, 10, Side::Buy, 100, 50_000));
    ok(&mut m, add(1, 11, Side::Buy, 100, 49_000));
    // New reference collides with a live order: nothing must change —
    // in particular the original must NOT be removed.
    fails(
        &mut m,
        replace(1, 10, 11, 80, 50_000),
        BookError::DuplicateOrder { order_ref: 11 },
    );
    assert!(m.order(10).is_some());
    // Replacing an order with itself violates day-uniqueness too.
    fails(
        &mut m,
        replace(1, 10, 10, 80, 50_000),
        BookError::DuplicateOrder { order_ref: 10 },
    );
}

#[test]
fn zero_values_rejected() {
    let mut m = Market::new();
    fails(
        &mut m,
        add(1, 10, Side::Buy, 0, 50_000),
        BookError::ZeroShares { order_ref: 10 },
    );
    fails(
        &mut m,
        add(1, 10, Side::Buy, 100, 0),
        BookError::ZeroPrice { order_ref: 10 },
    );
    ok(&mut m, add(1, 10, Side::Buy, 100, 50_000));
    fails(
        &mut m,
        exec(1, 10, 0),
        BookError::ZeroQuantity { order_ref: 10 },
    );
    fails(
        &mut m,
        cancel(1, 10, 0),
        BookError::ZeroQuantity { order_ref: 10 },
    );
    fails(
        &mut m,
        replace(1, 10, 11, 0, 50_000),
        BookError::ZeroShares { order_ref: 11 },
    );
    fails(
        &mut m,
        replace(1, 10, 11, 100, 0),
        BookError::ZeroPrice { order_ref: 11 },
    );
}

#[test]
fn crossed_book_scoped_to_continuous_trading() {
    let mut m = Market::new();

    // Pre-open: a crossed book (bid 51 > ask 50) is legitimate.
    ok(&mut m, add(1, 10, Side::Buy, 100, 51_000));
    ok(&mut m, add(1, 20, Side::Sell, 100, 50_000));
    assert!(m.crossed(1));
    assert!(!m.market_hours());
    assert!(
        !m.in_continuous_trading(1),
        "pre-open: cross invariant out of scope"
    );

    // System hours alone don't start continuous trading...
    ok(&mut m, system_event(EventCode::StartOfSystemHours));
    assert!(!m.in_continuous_trading(1));

    // ...market hours do, but only for stocks in the Trading state; a
    // stock absent from the Trading Action spin counts as halted.
    ok(&mut m, system_event(EventCode::StartOfMarketHours));
    assert!(m.market_hours());
    assert_eq!(m.trading_state(1), TradingState::Halted);
    assert!(!m.in_continuous_trading(1));

    ok(&mut m, trading_action(1, TradingState::Trading));
    assert!(m.in_continuous_trading(1));
    assert!(m.crossed(1), "still crossed — now it IS a violation");

    // Uncross: delete the aggressive bid; a locked book (bid == ask) still
    // counts as crossed.
    ok(&mut m, delete(1, 10));
    assert!(!m.crossed(1));
    ok(&mut m, add(1, 11, Side::Buy, 100, 50_000));
    assert!(m.crossed(1), "locked book (bid == ask) counts");

    // A halt takes the stock back out of scope; end of market hours too.
    ok(&mut m, trading_action(1, TradingState::Halted));
    assert!(!m.in_continuous_trading(1));
    ok(&mut m, trading_action(1, TradingState::Trading));
    assert!(m.in_continuous_trading(1));
    ok(&mut m, system_event(EventCode::EndOfMarketHours));
    assert!(!m.in_continuous_trading(1));
}

#[test]
fn books_are_isolated_by_locate() {
    let mut m = Market::new();
    ok(&mut m, add(1, 10, Side::Buy, 100, 50_000));
    ok(&mut m, add(2, 20, Side::Sell, 200, 40_000));
    assert_eq!(m.book(1).unwrap().best_ask(), None);
    assert_eq!(m.book(2).unwrap().best_bid(), None);
    assert_eq!(m.live_orders(), 2);
    // Same price in two books never interacts.
    assert!(!m.crossed(1));
    assert!(!m.crossed(2));
}

#[test]
fn side_levels_iterate_best_to_worst() {
    let mut m = Market::new();
    ok(&mut m, add(1, 1, Side::Buy, 10, 48_000));
    ok(&mut m, add(1, 2, Side::Buy, 20, 50_000));
    ok(&mut m, add(1, 3, Side::Buy, 30, 49_000));
    ok(&mut m, add(1, 4, Side::Sell, 40, 52_000));
    ok(&mut m, add(1, 5, Side::Sell, 50, 51_000));
    let book = m.book(1).unwrap();
    let bids: Vec<u32> = book.side_levels(Side::Buy).map(|(p, _, _)| p.0).collect();
    assert_eq!(bids, vec![50_000, 49_000, 48_000], "bids descend from best");
    let asks: Vec<u32> = book.side_levels(Side::Sell).map(|(p, _, _)| p.0).collect();
    assert_eq!(asks, vec![51_000, 52_000], "asks ascend from best");
}
