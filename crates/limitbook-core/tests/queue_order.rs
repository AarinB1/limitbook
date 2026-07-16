//! The exposed FIFO queue accessors must report the book's true
//! arrival-order state — a queue view that shows the wrong order is a lie
//! about the one thing price-time priority proves.
//!
//! Two layers of evidence:
//! - scenario tests walk `queue_at`/`queue_position` through every
//!   queue-affecting message semantic (partial cancels keep the place,
//!   replaces reset it, fills pop the front);
//! - the mid-day fixture (the slice the browser demo replays) is fed in
//!   parallel to an INDEPENDENT shadow FIFO model — plain per-level
//!   `Vec<u64>` arrival lists built straight from the decoded messages —
//!   and every level queue of every book must match it exactly, at
//!   checkpoints along the replay and at the final state.

use std::collections::BTreeMap;
use std::io::Read;

use limitbook_core::book::Market;
use limitbook_core::frame::Messages;
use limitbook_core::parse::{
    self, AddOrder, Header, Message, OrderCancel, OrderDelete, OrderExecuted, OrderReplace, Price4,
    Side,
};

// --- scenario layer ---------------------------------------------------------

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

fn queue(m: &Market, locate: u16, side: Side, price: u32) -> Vec<(u64, u32)> {
    m.queue_at(locate, side, Price4(price)).collect()
}

#[test]
fn queue_at_tracks_time_priority_through_every_mutation() {
    let mut m = Market::new();
    let (l, px) = (1u16, 50_000u32);
    for msg in [
        add(l, 1, Side::Buy, 100, px),
        add(l, 2, Side::Buy, 200, px),
        add(l, 3, Side::Buy, 300, px),
    ] {
        m.apply(&msg).unwrap();
    }
    assert_eq!(queue(&m, l, Side::Buy, px), [(1, 100), (2, 200), (3, 300)]);
    assert_eq!(m.queue_position(1), Some((0, 0)));
    assert_eq!(m.queue_position(3), Some((2, 300)));

    // Partial cancel keeps the queue place; only shares change.
    m.apply(&cancel(l, 2, 50)).unwrap();
    assert_eq!(queue(&m, l, Side::Buy, px), [(1, 100), (2, 150), (3, 300)]);
    assert_eq!(m.queue_position(3), Some((2, 250)));

    // Partial execution at the front keeps the place too.
    m.apply(&exec(l, 1, 40)).unwrap();
    assert_eq!(queue(&m, l, Side::Buy, px), [(1, 60), (2, 150), (3, 300)]);

    // Replace retires the ref and re-enters at the BACK — priority reset.
    m.apply(&replace(l, 1, 4, 500, px)).unwrap();
    assert_eq!(queue(&m, l, Side::Buy, px), [(2, 150), (3, 300), (4, 500)]);
    assert_eq!(m.queue_position(4), Some((2, 450)));
    assert_eq!(m.queue_position(1), None, "replaced ref is dead");

    // A fill to zero pops the order out of the line entirely.
    m.apply(&exec(l, 2, 150)).unwrap();
    assert_eq!(queue(&m, l, Side::Buy, px), [(3, 300), (4, 500)]);
    assert_eq!(m.queue_position(4), Some((1, 300)));

    m.apply(&delete(l, 3)).unwrap();
    assert_eq!(queue(&m, l, Side::Buy, px), [(4, 500)]);
    assert_eq!(m.queue_position(4), Some((0, 0)));

    // Sides and levels are independent queues; unknown levels are empty.
    m.apply(&add(l, 5, Side::Sell, 70, px)).unwrap();
    assert_eq!(queue(&m, l, Side::Sell, px), [(5, 70)]);
    assert_eq!(queue(&m, l, Side::Buy, px), [(4, 500)]);
    assert_eq!(queue(&m, l, Side::Buy, px + 1), []);
    assert_eq!(queue(&m, 2, Side::Buy, px), []);
    assert_eq!(m.queue_position(99), None);
}

#[test]
fn replace_to_a_new_level_joins_that_queue_at_the_back() {
    let mut m = Market::new();
    m.apply(&add(1, 1, Side::Sell, 100, 60_000)).unwrap();
    m.apply(&add(1, 2, Side::Sell, 200, 61_000)).unwrap();
    m.apply(&add(1, 3, Side::Sell, 300, 61_000)).unwrap();

    // 1 reprices onto the 61_000 level: it arrives last in that line.
    m.apply(&replace(1, 1, 9, 100, 61_000)).unwrap();
    assert_eq!(queue(&m, 1, Side::Sell, 60_000), []);
    assert_eq!(
        queue(&m, 1, Side::Sell, 61_000),
        [(2, 200), (3, 300), (9, 100)]
    );
    assert_eq!(m.queue_position(9), Some((2, 500)));
}

// --- fixture layer: independent shadow FIFO model ----------------------------

const MIDDAY_GZ: &[u8] = include_bytes!("../../../tests/fixtures/itch50_20191230_midday.itch.gz");

/// A from-scratch FIFO book: per-level arrival lists plus an order map,
/// built directly from decoded messages with none of limitbook's book code.
#[derive(Default)]
struct Shadow {
    /// order ref -> (locate, side key, price ticks, remaining shares)
    orders: BTreeMap<u64, (u16, u8, u32, u32)>,
    /// (locate, side key, price ticks) -> refs in arrival order
    levels: BTreeMap<(u16, u8, u32), Vec<u64>>,
}

fn side_key(side: Side) -> u8 {
    match side {
        Side::Buy => 0,
        Side::Sell => 1,
    }
}

impl Shadow {
    fn insert(&mut self, oref: u64, locate: u16, side: u8, price: u32, shares: u32) {
        assert!(
            self.orders
                .insert(oref, (locate, side, price, shares))
                .is_none(),
            "duplicate ref {oref}"
        );
        self.levels
            .entry((locate, side, price))
            .or_default()
            .push(oref);
    }

    fn remove(&mut self, oref: u64) -> (u16, u8, u32, u32) {
        let meta = self.orders.remove(&oref).expect("remove of live order");
        let (locate, side, price, _) = meta;
        let level = self.levels.get_mut(&(locate, side, price)).unwrap();
        level.retain(|&r| r != oref);
        if level.is_empty() {
            self.levels.remove(&(locate, side, price));
        }
        meta
    }

    fn reduce(&mut self, oref: u64, qty: u32) {
        let remaining = {
            let entry = self.orders.get_mut(&oref).expect("reduce of live order");
            entry.3 = entry.3.checked_sub(qty).expect("overfill");
            entry.3
        };
        if remaining == 0 {
            self.remove(oref);
        }
    }

    fn apply(&mut self, msg: &Message<'_>) {
        match msg {
            Message::AddOrder(m) => self.insert(
                m.order_ref,
                m.header.stock_locate,
                side_key(m.side),
                m.price.0,
                m.shares,
            ),
            Message::OrderExecuted(m) => self.reduce(m.order_ref, m.executed_shares),
            Message::OrderExecutedWithPrice(m) => self.reduce(m.order_ref, m.executed_shares),
            Message::OrderCancel(m) => self.reduce(m.order_ref, m.cancelled_shares),
            Message::OrderDelete(m) => {
                self.remove(m.order_ref);
            }
            Message::OrderReplace(m) => {
                // Side and locate carry over; the new ref arrives at the
                // back of its (possibly new) level.
                let (locate, side, _, _) = self.remove(m.original_order_ref);
                self.insert(m.new_order_ref, locate, side, m.price.0, m.shares);
            }
            _ => {}
        }
    }

    /// Every level queue as (level key) -> [(ref, shares)...] in arrival
    /// order, for whole-book comparison.
    fn full_state(&self) -> BTreeMap<(u16, u8, u32), Vec<(u64, u32)>> {
        self.levels
            .iter()
            .map(|(&key, refs)| {
                let queue = refs.iter().map(|&r| (r, self.orders[&r].3)).collect();
                (key, queue)
            })
            .collect()
    }
}

/// Every level queue the Market exposes, keyed like the shadow's.
fn market_state(market: &Market) -> BTreeMap<(u16, u8, u32), Vec<(u64, u32)>> {
    let mut out = BTreeMap::new();
    for (locate, book) in market.books() {
        for side in [Side::Buy, Side::Sell] {
            for (price, _, _) in book.side_levels(side) {
                let queue: Vec<(u64, u32)> = market.queue_at(locate, side, price).collect();
                out.insert((locate, side_key(side), price.0), queue);
            }
        }
    }
    out
}

/// The exposed queues must equal the shadow's arrival lists exactly, and
/// `queue_position` must agree with each order's index and the shares
/// ahead of it.
fn assert_matches_shadow(market: &Market, shadow: &Shadow, at: u64) {
    let expected = shadow.full_state();
    let actual = market_state(market);
    assert_eq!(
        actual, expected,
        "level queues diverged from the independent FIFO model at message {at}"
    );
    for queue in expected.values() {
        let mut shares_ahead = 0u64;
        for (rank, &(oref, shares)) in queue.iter().enumerate() {
            assert_eq!(
                market.queue_position(oref),
                Some((rank, shares_ahead)),
                "queue_position({oref}) wrong at message {at}"
            );
            shares_ahead += u64::from(shares);
        }
    }
}

#[test]
fn midday_fixture_queues_match_independent_fifo_model() {
    let mut raw = Vec::new();
    flate2::read::GzDecoder::new(MIDDAY_GZ)
        .read_to_end(&mut raw)
        .expect("fixture must be valid gzip");

    let mut market = Market::new();
    let mut shadow = Shadow::default();
    let mut fed = 0u64;
    for payload in Messages::new(&raw) {
        let payload = payload.expect("no framing errors");
        let msg = parse::decode(payload).expect("no parse errors");
        market.apply(&msg).expect("no book errors");
        shadow.apply(&msg);
        fed += 1;
        if fed.is_multiple_of(20_000) {
            assert_matches_shadow(&market, &shadow, fed);
        }
    }
    assert_eq!(fed, 157_824, "fixture message count");
    assert_matches_shadow(&market, &shadow, fed);
    assert!(!shadow.levels.is_empty(), "final state must be non-trivial");
}
