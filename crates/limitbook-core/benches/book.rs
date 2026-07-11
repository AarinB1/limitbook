//! Criterion micro-benchmarks for [`Market::apply`]: each book operation
//! (add, execute, cancel, delete, replace) applied to a *warm* book.
//!
//! The warm book models one active symbol: 100 price levels per side, 10
//! orders per level (2,000 live orders), one-cent ticks around a $150.00
//! mid. Each measurement clones the warm market in setup (untimed) and
//! applies exactly one message, so every sample sees identical state; the
//! clone is returned from the routine so its drop is untimed too.
//!
//! Operations are placed where real flow lands them: executions hit the
//! front of the best level's queue, deletes target the back of a mid-book
//! queue (worst case for the level's linear dequeue scan), adds either
//! join an existing level or open a new one (BTreeMap insert).

use criterion::{BatchSize, Criterion, criterion_group, criterion_main};

use limitbook_core::book::Market;
use limitbook_core::parse::{
    AddOrder, Header, Message, OrderCancel, OrderDelete, OrderExecuted, OrderReplace, Price4, Side,
    Stock,
};

const LOCATE: u16 = 7;
const STOCK: Stock = *b"BENCH   ";
/// Levels per side and orders per level of the warm book.
const LEVELS: u64 = 100;
const ORDERS_PER_LEVEL: u64 = 10;
/// Every resting order carries this many shares.
const SHARES: u32 = 100;
/// One cent in Price4 units (4 implied decimals).
const TICK: u32 = 100;
/// Best bid $149.99; bid levels descend one tick each.
const BEST_BID: u32 = 1_499_900;
/// Best ask $150.01; ask levels ascend one tick each.
const BEST_ASK: u32 = 1_500_100;
const BID_REF_BASE: u64 = 1_000;
const ASK_REF_BASE: u64 = 100_000;

fn header() -> Header {
    Header {
        stock_locate: LOCATE,
        tracking_number: 1,
        timestamp: 34_200_000_000_000,
    }
}

fn add(order_ref: u64, side: Side, price: u32) -> Message<'static> {
    Message::AddOrder(AddOrder {
        header: header(),
        order_ref,
        side,
        shares: SHARES,
        stock: &STOCK,
        price: Price4(price),
        attribution: None,
    })
}

/// Builds the warm market through the public [`Market::apply`] path.
fn warm_market() -> Market {
    let mut market = Market::new();
    for level in 0..LEVELS {
        for slot in 0..ORDERS_PER_LEVEL {
            let n = level * ORDERS_PER_LEVEL + slot;
            let bid_price = BEST_BID - TICK * level as u32;
            let ask_price = BEST_ASK + TICK * level as u32;
            market
                .apply(&add(BID_REF_BASE + n, Side::Buy, bid_price))
                .expect("warm bid add");
            market
                .apply(&add(ASK_REF_BASE + n, Side::Sell, ask_price))
                .expect("warm ask add");
        }
    }
    assert_eq!(
        market.live_orders(),
        (2 * LEVELS * ORDERS_PER_LEVEL) as usize
    );
    market
}

/// Order ref for (level, slot) on the bid side; slot 0 is the queue front.
fn bid_ref(level: u64, slot: u64) -> u64 {
    BID_REF_BASE + level * ORDERS_PER_LEVEL + slot
}

fn bench_ops(c: &mut Criterion) {
    let warm = warm_market();
    // (name, one message to apply against the warm book)
    let unused_ref = 9_000_000u64;
    let ops: Vec<(&str, Message<'static>)> = vec![
        (
            "add_new_level",
            add(
                unused_ref,
                Side::Buy,
                BEST_BID - TICK * (LEVELS as u32 + 50),
            ),
        ),
        ("add_join_level", add(unused_ref, Side::Buy, BEST_BID)),
        (
            "execute_partial",
            Message::OrderExecuted(OrderExecuted {
                header: header(),
                order_ref: ASK_REF_BASE, // front of the best ask queue
                executed_shares: SHARES / 2,
                match_number: 42,
            }),
        ),
        (
            "execute_full_removes_order",
            Message::OrderExecuted(OrderExecuted {
                header: header(),
                order_ref: ASK_REF_BASE,
                executed_shares: SHARES,
                match_number: 42,
            }),
        ),
        (
            "cancel_partial",
            Message::OrderCancel(OrderCancel {
                header: header(),
                order_ref: bid_ref(0, 0),
                cancelled_shares: SHARES / 2,
            }),
        ),
        (
            "delete_back_of_mid_level",
            Message::OrderDelete(OrderDelete {
                header: header(),
                order_ref: bid_ref(LEVELS / 2, ORDERS_PER_LEVEL - 1),
            }),
        ),
        (
            "replace_to_better_price",
            Message::OrderReplace(OrderReplace {
                header: header(),
                original_order_ref: bid_ref(LEVELS / 2, 0),
                new_order_ref: unused_ref,
                shares: SHARES,
                price: Price4(BEST_BID - TICK * (LEVELS as u32 / 2) + TICK / 2),
            }),
        ),
    ];

    let mut group = c.benchmark_group("book_apply");
    for (name, msg) in &ops {
        // Prove the op succeeds against the warm book before timing it.
        warm.clone()
            .apply(msg)
            .expect("bench op must apply cleanly");
        group.bench_function(*name, |b| {
            b.iter_batched(
                || warm.clone(),
                |mut market| {
                    let effect = market.apply(msg);
                    debug_assert!(effect.is_ok());
                    (market, effect)
                },
                BatchSize::SmallInput,
            )
        });
    }
    group.finish();
}

criterion_group!(benches, bench_ops);
criterion_main!(benches);
